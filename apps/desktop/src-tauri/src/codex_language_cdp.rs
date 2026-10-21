use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};
use tungstenite::{client, Message, WebSocket};

const MAX_RESPONSE: usize = 512 * 1024;

#[derive(Clone, Deserialize)]
pub(super) struct Target {
    pub(super) id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    socket: Option<String>,
}

pub(super) fn app_origin(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        url.scheme() == "app"
            && url.host_str() == Some("-")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "/" | "/index.html")
    })
}

fn socket_url(value: &str, port: u16) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| "调试目标地址无效")?;
    if url.scheme() != "ws"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(port)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with("/devtools/page/")
        || url.path().trim_start_matches("/devtools/page/").is_empty()
    {
        return Err("调试目标超出指定本机端口，未执行语言脚本。".into());
    }
    Ok(url)
}

pub(super) fn targets(port: u16) -> Result<Vec<Target>, String> {
    crate::remote::ensure_crypto_provider();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| "无法初始化本机语言兼容连接")?;
    let response = client
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .map_err(|_| "客户端未开放本机调试通道；此版本可能不支持兼容模式。")?;
    if !response.status().is_success() {
        return Err("客户端调试通道尚未就绪".into());
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取调试目标")?;
    if bytes.len() > MAX_RESPONSE {
        return Err("客户端返回的调试目标超过安全限制".into());
    }
    let targets: Vec<Target> =
        serde_json::from_slice(&bytes).map_err(|_| "客户端未返回有效的调试目标")?;
    if targets.len() > 128 {
        return Err("客户端调试目标过多，未继续连接".into());
    }
    Ok(targets
        .into_iter()
        .filter(|target| {
            target.kind == "page" && app_origin(&target.url) && target.socket.is_some()
        })
        .collect())
}

#[derive(Debug)]
pub(super) enum BootstrapResult {
    /// The current document already owns a compatible, self-contained hook.
    /// Preserve its readiness state and avoid registering or reloading it.
    Existing(Value),
    /// A single bootstrap reload was acknowledged. Readiness still needs to
    /// be verified by polling the caller's probe; this is not application success.
    ReloadRequested,
}

pub(super) struct Session {
    socket: WebSocket<TcpStream>,
    next_id: u64,
    script: String,
    bootstrap_started: bool,
}
impl Session {
    pub(super) fn connect(target: &Target, port: u16) -> Result<Self, String> {
        if target.kind != "page" || !app_origin(&target.url) {
            return Err("不是官方客户端的本地界面目标".into());
        }
        let url = socket_url(target.socket.as_deref().ok_or("目标没有调试连接")?, port)?;
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            Duration::from_secs(2),
        )
        .map_err(|_| "本机调试连接未建立")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "无法设置调试读取超时")?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "无法设置调试写入超时")?;
        let (mut socket, _) = client(url.as_str(), stream)
            .map_err(|_| "客户端拒绝了安全调试连接；未降低客户端安全设置。")?;
        socket.set_config(|config| {
            config.max_message_size = Some(MAX_RESPONSE);
            config.max_frame_size = Some(MAX_RESPONSE);
        });
        Ok(Self {
            socket,
            next_id: 0,
            script: String::new(),
            bootstrap_started: false,
        })
    }
    pub(super) fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .map_err(|_| "语言兼容命令发送失败")?;
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            match self.socket.read().map_err(|_| "语言兼容连接中断或超时")? {
                Message::Text(text) => {
                    if text.len() > MAX_RESPONSE {
                        return Err("调试返回内容超过限制".into());
                    }
                    let event: Value =
                        serde_json::from_str(&text).map_err(|_| "调试返回格式无效")?;
                    if event.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if event.get("error").is_some() {
                        return Err("当前客户端不支持所需的界面兼容命令".into());
                    }
                    return Ok(event.get("result").cloned().unwrap_or(Value::Null));
                }
                Message::Ping(bytes) => {
                    self.socket
                        .send(Message::Pong(bytes))
                        .map_err(|_| "调试连接中断")?;
                }
                Message::Close(_) => return Err("客户端关闭了语言兼容连接".into()),
                _ => {}
            }
        }
        Err("等待界面兼容结果超时".into())
    }
    /// Begin once per explicit bootstrap attempt. Every error is terminal for
    /// that attempt, even if a registration/reload acknowledgement was lost.
    /// Callers must not reconnect and repeat installation after such an error.
    pub(super) fn begin_bootstrap(
        &mut self,
        source: &str,
        check: &str,
        locale: &str,
    ) -> Result<BootstrapResult, String> {
        if self.bootstrap_started {
            return Err("本轮界面引导已经开始，未重复注册脚本或重载页面。".into());
        }
        // Consume the attempt before any fallible request: a write or reply
        // failure cannot prove that the browser did not execute the command.
        self.bootstrap_started = true;
        if !matches!(locale, "zh-CN" | "en-US") {
            return Err("目标界面语言不受支持，未注册脚本或重载页面。".into());
        }
        let origin=self.evaluate("(()=>{try{const u=new URL(location.href);return {allowed:window===window.top&&u.protocol==='app:'&&u.host==='-'&&!u.username&&!u.password&&['/','/index.html'].includes(u.pathname)};}catch{return {allowed:false};}})()")?;
        if origin.get("allowed").and_then(Value::as_bool) != Some(true) {
            return Err("目标页面来源在连接后已改变，未注册脚本或重载页面。".into());
        }
        let existing = self.evaluate(check)?;
        if existing.get("installed").and_then(Value::as_bool) == Some(true)
            && existing.get("locale").and_then(Value::as_str) == Some(locale)
        {
            return Ok(BootstrapResult::Existing(existing));
        }
        self.call("Page.enable", json!({}))?;
        let result = self
            .call(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source":source}),
            )
            .map_err(|error| {
                format!("未能确认兼容脚本注册，已停止本轮引导，不会自动重试或重载：{error}")
            })?;
        self.script = result
            .get("identifier")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or("客户端未确认兼容脚本注册，已停止本轮引导，未继续重载。")?
            .to_owned();
        self.call("Page.reload", json!({"ignoreCache":false}))
            .map_err(|error| {
                format!("页面重载请求可能已经发出，已停止本轮引导，不会再次发送：{error}")
            })?;
        Ok(BootstrapResult::ReloadRequested)
    }

    /// Explicitly release only this session's future-document registration
    /// after bootstrap verification. Never restore the live hook or reload it.
    pub(super) fn cleanup_lease(&mut self) -> Result<(), String> {
        if self.script.is_empty() {
            return Ok(());
        }
        let script = self.script.clone();
        self.call(
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({"identifier":script}),
        )?;
        self.script.clear();
        Ok(())
    }
    pub(super) fn evaluate(&mut self, source: &str) -> Result<Value, String> {
        let result=self.call("Runtime.evaluate",json!({"expression":source,"returnByValue":true,"awaitPromise":true,"userGesture":false}))?;
        if result.get("exceptionDetails").is_some() {
            return Err("客户端界面不支持此语言兼容模式，未报告应用成功。".into());
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Dropping a connection is not permission to change the page or its
        // registered scripts. A successful bootstrap releases its own lease
        // explicitly; disconnecting must never restore or reload the client.
        let _ = self.socket.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_browser_targets_remote_websockets_and_credentials() {
        assert!(app_origin("app://-/index.html?initialRoute=%2Fsettings"));
        for origin in [
            "https://chatgpt.com/",
            "http://127.0.0.1/index.html",
            "app://other/index.html",
            "app://user@-/index.html",
            "app://-/browser.html",
        ] {
            assert!(!app_origin(origin));
        }
        assert!(socket_url("ws://127.0.0.1:4567/devtools/page/fixture", 4567).is_ok());
        for url in [
            "ws://localhost:4567/devtools/page/x",
            "ws://127.0.0.1:4568/devtools/page/x",
            "ws://127.0.0.1:4567/devtools/browser/x",
            "ws://user@127.0.0.1:4567/devtools/page/x",
            "wss://127.0.0.1:4567/devtools/page/x",
        ] {
            assert!(socket_url(url, 4567).is_err());
        }
    }
    #[test]
    fn simulated_cdp_registers_main_world_script_and_returns_only_fixture_status() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for (index, expected) in [
                "Runtime.evaluate",
                "Runtime.evaluate",
                "Page.enable",
                "Page.addScriptToEvaluateOnNewDocument",
                "Page.reload",
                "Runtime.evaluate",
                "Page.removeScriptToEvaluateOnNewDocument",
            ]
            .into_iter()
            .enumerate()
            {
                let Message::Text(text) = socket.read().unwrap() else {
                    panic!("expected command")
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(request["method"], expected);
                let result = match expected {
                    "Page.addScriptToEvaluateOnNewDocument" => {
                        assert!(request["params"].get("worldName").is_none());
                        json!({"identifier":"fixture-script"})
                    }
                    "Runtime.evaluate" if index == 0 => {
                        json!({"result":{"value":{"allowed":true}}})
                    }
                    "Runtime.evaluate" if index == 1 => {
                        json!({"result":{"value":{"installed":false,"ready":false}}})
                    }
                    "Runtime.evaluate" => {
                        json!({"result":{"value":{"installed":true,"locale":"zh-CN","ready":true}}})
                    }
                    _ => json!({}),
                };
                socket
                    .send(Message::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
        });
        let target = Target {
            id: "fixture".into(),
            kind: "page".into(),
            url: "app://-/index.html".into(),
            socket: Some(format!("ws://127.0.0.1:{port}/devtools/page/fixture")),
        };
        {
            let mut session = Session::connect(&target, port).unwrap();
            assert!(matches!(
                session
                    .begin_bootstrap("/* fixture i18n script */", "fixtureStatus()", "zh-CN")
                    .unwrap(),
                BootstrapResult::ReloadRequested
            ));
            assert!(session
                .begin_bootstrap("/* fixture i18n script */", "fixtureStatus()", "zh-CN")
                .is_err());
            assert_eq!(session.evaluate("fixtureStatus()").unwrap()["ready"], true);
            session.cleanup_lease().unwrap();
            session.cleanup_lease().unwrap();
        }
        server.join().unwrap();
    }
}
