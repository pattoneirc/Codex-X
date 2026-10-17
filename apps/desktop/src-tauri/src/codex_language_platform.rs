//! The locale compatibility launcher never modifies or re-signs the app bundle.
use std::path::{Path, PathBuf};
#[cfg(test)]
static REAPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn process_reaper() -> Result<&'static std::sync::mpsc::Sender<std::process::Child>, String> {
    static REAPER: std::sync::OnceLock<
        Result<std::sync::mpsc::Sender<std::process::Child>, String>,
    > = std::sync::OnceLock::new();
    REAPER
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel::<std::process::Child>();
            std::thread::Builder::new()
                .name("codex-language-process-reaper".into())
                .spawn(move || {
                    let mut children: Vec<std::process::Child> = Vec::new();
                    loop {
                        match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                            Ok(child) => children.push(child),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(_) => break,
                        }
                        children.retain_mut(|child| match child.try_wait() {
                            Ok(None) => true,
                            Ok(Some(_)) => {
                                #[cfg(test)]
                                REAPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                false
                            }
                            Err(_) => false,
                        });
                    }
                })
                .map_err(|_| "无法初始化客户端进程回收线程，尚未退出客户端。".to_owned())?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub(crate) fn prepare_launch() -> Result<(), String> {
    process_reaper().map(|_| ())
}

#[derive(Clone)]
pub(crate) struct DesktopApp {
    pub(crate) path: PathBuf,
    pub(crate) executable: PathBuf,
    pub(crate) bundle_id: String,
    pub(crate) name: String,
    pub(crate) version: String,
}

#[cfg(target_os = "windows")]
fn windows_app(app: &DesktopApp) -> crate::platform::desktop_windows::WindowsDesktopApp {
    crate::platform::desktop_windows::WindowsDesktopApp {
        path: app.path.clone(),
        executable: app.executable.clone(),
        bundle_id: app.bundle_id.clone(),
        name: app.name.clone(),
        version: app.version.clone(),
    }
}

#[cfg(target_os = "macos")]
fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = crate::platform::locale_platform_command(Path::new(program), args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn detect() -> Result<DesktopApp, String> {
    #[cfg(target_os = "macos")]
    {
        let app = crate::platform::locale_desktop_app()?;
        Ok(DesktopApp {
            path: app.0,
            executable: app.1,
            bundle_id: app.2,
            name: app.3,
            version: app.4,
        })
    }
    #[cfg(target_os = "windows")]
    {
        let app = crate::platform::desktop_windows::detect()?;
        Ok(DesktopApp {
            path: app.path,
            executable: app.executable,
            bundle_id: app.bundle_id,
            name: app.name,
            version: app.version,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台尚未实现可验证官方进程的界面语言启动器。".into())
    }
}

pub(crate) fn detect_for_operation() -> Result<DesktopApp, String> {
    #[cfg(target_os = "windows")]
    {
        let app = crate::platform::desktop_windows::detect_fresh()?;
        Ok(DesktopApp {
            path: app.path,
            executable: app.executable,
            bundle_id: app.bundle_id,
            name: app.name,
            version: app.version,
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        detect()
    }
}

pub(crate) fn validate(app: &DesktopApp) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if !matches!(
            app.bundle_id.as_str(),
            "com.openai.codex" | "com.openai.codex.beta"
        ) {
            return Err("不是受支持的官方 Codex 客户端".into());
        }
        let path = app.path.to_str().ok_or("客户端路径无效")?;
        crate::platform::locale_platform_command(
            Path::new("/usr/bin/codesign"),
            &["--verify", "--strict", path],
        )?;
        let details = crate::platform::locale_platform_command(
            Path::new("/usr/bin/codesign"),
            &["--display", "--verbose=4", path],
        )?;
        let info = String::from_utf8_lossy(&details.stderr);
        if !info.lines().any(|line| line == "TeamIdentifier=2DC432GLL2") {
            return Err("无法确认客户端为 OpenAI 官方签名，未重启或连接客户端。".into());
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::desktop_windows::validate(&windows_app(app))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("当前平台不支持安全的语言兼容启动".into())
    }
}

pub(crate) fn validate_launch(app: &DesktopApp, home: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::platform::desktop_windows::validate_launch(&windows_app(app), home)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, home);
        Ok(())
    }
}

pub(crate) fn quit(app: &DesktopApp) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        crate::platform::locale_quit_desktop(&app.executable, &app.bundle_id)
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::desktop_windows::quit(&windows_app(app))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("当前平台不支持安全的语言兼容启动".into())
    }
}

pub(crate) fn ensure_stopped(app: &DesktopApp) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if !crate::platform::locale_desktop_pids(&app.executable)?.is_empty() {
            return Err("客户端在保存语言前重新启动，已取消写入；请完全退出后重试。".into());
        }
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        if !crate::platform::desktop_windows::pids(&windows_app(app))?.is_empty() {
            return Err("客户端在保存语言前重新启动，已取消写入；请完全退出后重试。".into());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = app;
        Err("当前平台不支持安全的语言兼容启动".into())
    }
}

pub(crate) fn quit_spawned(app: &DesktopApp, pid: u32) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if !crate::platform::locale_desktop_pids(&app.executable)?.contains(&pid) {
            return Err("刚启动的官方进程身份已改变，未自动退出其它实例；请手动恢复。".into());
        }
        quit(app)
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::desktop_windows::verify_process(&windows_app(app), pid)?;
        quit(app)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, pid);
        Err("当前平台不支持安全的客户端恢复".into())
    }
}

pub(crate) fn launch(app: &DesktopApp, home: &Path, port: Option<u16>) -> Result<u32, String> {
    #[cfg(target_os = "windows")]
    {
        return crate::platform::desktop_windows::launch(&windows_app(app), Some(home), port);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = std::process::Command::new(&app.executable);
        command
            .env("CODEX_HOME", home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(port) = port {
            command.args([
                format!("--remote-debugging-port={port}"),
                "--remote-debugging-address=127.0.0.1".into(),
            ]);
        }
        let reaper = process_reaper()?;
        let child = command
            .spawn()
            .map_err(|_| "无法启动官方客户端，请手动打开客户端后重试。")?;
        let pid = child.id();
        reaper
            .send(child)
            .map_err(|_| "客户端已启动，但进程监督不可用；请手动恢复并重试。")?;
        Ok(pid)
    }
}

pub(crate) fn verify_listener(app: &DesktopApp, pid: u32, port: u16) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let ids = crate::platform::locale_desktop_pids(&app.executable)?;
        if !ids.contains(&pid) {
            return Err("官方客户端进程已经改变，已停止语言兼容连接。".into());
        }
        let output = command(
            "/usr/sbin/lsof",
            &["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"],
        )?;
        verify_listener_output(&output, pid, port)
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::desktop_windows::verify_listener(&windows_app(app), pid, port)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, pid, port);
        Err("当前平台无法验证调试端口归属".into())
    }
}

fn verify_listener_output(output: &str, pid: u32, port: u16) -> Result<(), String> {
    let mut current = None;
    let mut verified = false;
    for line in output.lines() {
        if let Some(p) = line.strip_prefix('p') {
            current = p.parse::<u32>().ok();
        }
        if let Some(address) = line.strip_prefix('n') {
            if current != Some(pid) || address != format!("127.0.0.1:{port}") {
                return Err("调试端口不属于指定官方进程或未限制在本机，未执行任何注入。".into());
            }
            verified = true;
        }
    }
    if !verified {
        return Err("官方客户端尚未开放可验证的本机调试端口；此构建可能不支持运行时兼容。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn listener_must_match_pid_and_ipv4_loopback() {
        assert!(verify_listener_output("p123\nn127.0.0.1:4567\n", 123, 4567).is_ok());
        for input in [
            "p456\nn127.0.0.1:4567\n",
            "p123\nn*:4567\n",
            "p123\nn192.168.1.2:4567\n",
            "p123\nn127.0.0.1:8888\n",
            "",
        ] {
            assert!(verify_listener_output(input, 123, 4567).is_err());
        }
    }
    #[test]
    fn spawned_fixture_children_are_waited_by_the_reaper() {
        let before = REAPED.load(std::sync::atomic::Ordering::Relaxed);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        process_reaper().unwrap().send(child).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while REAPED.load(std::sync::atomic::Ordering::Relaxed) <= before
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(REAPED.load(std::sync::atomic::Ordering::Relaxed) > before);
    }
}
