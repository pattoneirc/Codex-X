//! User-triggered, reversible locale compatibility. Reading status never
//! attaches to the official app; only explicit apply/restore can restart it.
#[path = "codex_language_cdp.rs"]
mod cdp;
#[path = "codex_language_platform.rs"]
mod desktop;

use crate::error::{CodexxError, Result};
use crate::live_config::{
    acquire_live_config_lock, atomic_write_if_unchanged, read_file_snapshot,
    restore_file_snapshot_if_unchanged,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};
use toml_edit::{value, DocumentMut, Item};

const SCRIPT: &str = include_str!("codex_locale_runtime.js");
const PREF_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Locale {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "zh-CN")]
    Chinese,
    #[serde(rename = "en-US")]
    English,
}
impl Default for Locale {
    fn default() -> Self {
        Self::Auto
    }
}
impl Locale {
    fn tag(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Chinese => Some("zh-CN"),
            Self::English => Some("en-US"),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Preferences {
    locale: Locale,
    enabled: bool,
    previous: Option<String>,
    previous_captured: bool,
    applied: Option<String>,
    // Only a new explicit switch selection creates an intent. Legacy locale /
    // enabled preferences never become a pending configuration change.
    pending: Option<PendingLanguage>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingLanguage {
    enabled: bool,
    captured_locale: Option<String>,
    // A successful config write can precede a failed launch. Keep that state
    // retryable without confusing it with a later external language change.
    #[serde(default)]
    written_locale: Option<LocaleSnapshot>,
}

#[derive(Clone, Serialize, Deserialize)]
struct LocaleSnapshot {
    locale: Option<String>,
}

fn chinese_enabled(locale: Option<&str>) -> bool {
    locale == Some("zh-CN")
}

fn pending_enabled(prefs: &Preferences, current: &Option<String>) -> Option<bool> {
    prefs.pending.as_ref().and_then(|pending| {
        let expected = pending
            .written_locale
            .as_ref()
            .map(|written| &written.locale)
            .unwrap_or(&pending.captured_locale);
        (expected == current).then_some(pending.enabled)
    })
}

fn stage_switch(prefs: &mut Preferences, enabled: bool, current: Option<String>) {
    prefs.locale = if enabled {
        Locale::Chinese
    } else {
        Locale::Auto
    };
    prefs.enabled = enabled;
    prefs.pending = (enabled != chinese_enabled(current.as_deref())).then_some(PendingLanguage {
        enabled,
        captured_locale: current,
        written_locale: None,
    });
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    phase: String,
    message: String,
    ready_pages: usize,
}
struct Runtime {
    scope: String,
    locale: Locale,
    cancel: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    progress: Arc<Mutex<Progress>>,
}
impl Runtime {
    fn stop(mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn runtime() -> &'static Mutex<Option<Runtime>> {
    static RUNTIME: OnceLock<Mutex<Option<Runtime>>> = OnceLock::new();
    RUNTIME.get_or_init(|| Mutex::new(None))
}
fn operation() -> &'static Mutex<()> {
    static OP: OnceLock<Mutex<()>> = OnceLock::new();
    OP.get_or_init(|| Mutex::new(()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LanguageStatus {
    locale: String,
    enabled: bool,
    pending_enabled: Option<bool>,
    supported: bool,
    app_name: Option<String>,
    app_version: Option<String>,
    phase: String,
    message: String,
    ready_pages: usize,
    restore_available: bool,
}

fn config_home(config_dir: Option<String>) -> Result<PathBuf> {
    crate::resolve_codex_dir(config_dir)
}
fn prefs_path(home: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(crate::paths::normalized_path_scope(home).as_bytes());
    let name = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(crate::paths::app_home()?
        .join("codex-language")
        .join(format!("{name}.json")))
}
fn read_preferences(home: &Path) -> Result<(Option<Vec<u8>>, Preferences)> {
    let bytes = read_file_snapshot(&prefs_path(home)?)?;
    if bytes.as_ref().is_some_and(|v| v.len() > PREF_LIMIT) {
        return Err(CodexxError::Config(
            "语言兼容偏好文件过大，未更改配置。".into(),
        ));
    }
    let prefs = bytes
        .as_deref()
        .map(|bytes| {
            serde_json::from_slice(bytes)
                .map_err(|_| CodexxError::Config("语言兼容偏好无法读取，未覆盖原文件。".into()))
        })
        .transpose()?
        .unwrap_or_default();
    Ok((bytes, prefs))
}
fn write_preferences(home: &Path, expected: Option<&[u8]>, prefs: &Preferences) -> Result<()> {
    let path = prefs_path(home)?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| {
        CodexxError::Config("无法创建语言偏好目录，请检查应用数据目录权限。".into())
    })?;
    let bytes = serde_json::to_vec_pretty(prefs)
        .map_err(|_| CodexxError::Config("无法保存语言偏好".into()))?;
    atomic_write_if_unchanged(&path, expected, &bytes)
}
fn document(home: &Path) -> Result<(Option<Vec<u8>>, DocumentMut)> {
    let bytes = read_file_snapshot(&crate::config_path(home))?;
    if bytes.as_ref().is_some_and(|v| v.len() > 8 * 1024 * 1024) {
        return Err(CodexxError::Config(
            "Codex 配置文件过大，未更改语言设置".into(),
        ));
    }
    let doc = bytes
        .as_deref()
        .map(|bytes| {
            std::str::from_utf8(bytes)
                .map_err(|_| CodexxError::Config("Codex 配置不是 UTF-8".into()))
                .and_then(|text| {
                    text.parse::<DocumentMut>().map_err(|_| {
                        CodexxError::Config("Codex 配置 TOML 无效，未覆盖原文件。".into())
                    })
                })
        })
        .transpose()?
        .unwrap_or_default();
    Ok((bytes, doc))
}
fn locale_value(doc: &DocumentMut) -> Result<Option<String>> {
    let Some(desktop) = doc.get("desktop") else {
        return Ok(None);
    };
    let table = desktop
        .as_table_like()
        .ok_or_else(|| CodexxError::Config("desktop 必须是 TOML 表，未更改配置。".into()))?;
    table
        .get("localeOverride")
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                CodexxError::Config("desktop.localeOverride 必须是文字，未更改配置。".into())
            })
        })
        .transpose()
}
fn change_locale(doc: &mut DocumentMut, locale: Option<&str>) -> Result<()> {
    if doc.get("desktop").is_none() {
        if locale.is_none() {
            return Ok(());
        }
        doc["desktop"] = Item::Table(toml_edit::Table::new());
    }
    let table = doc
        .get_mut("desktop")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| CodexxError::Config("desktop 必须是 TOML 表，未更改配置。".into()))?;
    if let Some(locale) = locale {
        table.insert("localeOverride", value(locale));
    } else {
        table.remove("localeOverride");
    }
    Ok(())
}

pub(crate) fn get_status(config_dir: Option<String>) -> Result<LanguageStatus> {
    let home = config_home(config_dir)?;
    let (_, prefs) = read_preferences(&home)?;
    let (_, doc) = document(&home)?;
    let actual = locale_value(&doc)?;
    let enabled = chinese_enabled(actual.as_deref());
    let pending = pending_enabled(&prefs, &actual);
    let scope = crate::paths::normalized_path_scope(&home);
    let current = runtime()
        .lock()
        .map_err(|_| CodexxError::Config("语言兼容状态忙，请重试".into()))?
        .as_ref()
        .filter(|r| r.scope == scope)
        .map(|r| {
            let mut progress = r.progress.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if r.locale.tag() != actual.as_deref() {
                progress.phase = "inactive".into();
                progress.message = "Codex 语言配置已改变；重启后使用当前配置。".into();
                progress.ready_pages = 0;
            }
            progress
        });
    let app = desktop::detect();
    let supported = app.is_ok();
    let app_name = app.as_ref().ok().map(|a| a.name.clone());
    let app_version = app.as_ref().ok().map(|a| a.version.clone());
    let mut progress = current.unwrap_or_else(|| Progress {
        phase: if supported { "inactive" } else { "unsupported" }.into(),
        message: app
            .err()
            .unwrap_or_else(|| "语言选择来自 Codex 配置。".into()),
        ready_pages: 0,
    });
    if pending.is_some() {
        progress.phase = "pending".into();
        progress.message = "重启 Codex 后应用所选语言。".into();
    }
    Ok(LanguageStatus {
        locale: actual.unwrap_or_else(|| "auto".into()),
        enabled,
        pending_enabled: pending,
        supported,
        app_name,
        app_version,
        phase: progress.phase,
        message: progress.message,
        ready_pages: progress.ready_pages,
        restore_available: prefs.previous_captured,
    })
}

pub(crate) fn save_preferences(
    config_dir: Option<String>,
    locale: Locale,
    enabled: bool,
) -> Result<LanguageStatus> {
    let _operation = operation()
        .lock()
        .map_err(|_| CodexxError::Config("语言操作忙".into()))?;
    let home = config_home(config_dir.clone())?;
    let (bytes, mut prefs) = read_preferences(&home)?;
    if !matches!(
        (locale, enabled),
        (Locale::Chinese, true) | (Locale::Auto, false)
    ) {
        return Err(CodexxError::Config(
            "请选择开启或关闭 Codex 简体中文。".into(),
        ));
    }
    let (_, doc) = document(&home)?;
    stage_switch(&mut prefs, enabled, locale_value(&doc)?);
    write_preferences(&home, bytes.as_deref(), &prefs)?;
    get_status(config_dir)
}

fn code(locale: Locale, owner: &str) -> String {
    SCRIPT.replace(
        "__CODEXX_LANGUAGE_CONFIG__",
        &json!({"locale":locale.tag(),"owner":owner}).to_string(),
    )
}
fn status_code(owner: &str) -> String {
    format!("(()=>{{const s=window.__CODEXX_LOCALE_RUNTIME_V1__;return s&&s.owner==={} ? s.status() : {{installed:false,ready:false}};}})()",json!(owner))
}
fn progress(target: &Arc<Mutex<Progress>>, phase: &str, message: &str, pages: usize) {
    *target.lock().unwrap_or_else(|p| p.into_inner()) = Progress {
        phase: phase.into(),
        message: message.into(),
        ready_pages: pages,
    };
}

fn supervise(
    app: desktop::DesktopApp,
    pid: u32,
    port: u16,
    locale: Locale,
    owner: String,
    cancel: Arc<AtomicBool>,
    state: Arc<Mutex<Progress>>,
) {
    let started = Instant::now();
    let source = code(locale, &owner);
    let check = status_code(&owner);
    let mut sessions: HashMap<String, cdp::Session> = HashMap::new();
    let mut ever_ready = false;
    let mut waiting_since: Option<Instant> = None;
    let mut last_error = "客户端正在启动，等待可验证的本机调试通道。".to_owned();
    while !cancel.load(Ordering::Acquire) {
        let attempt = (|| -> std::result::Result<usize, String> {
            desktop::verify_listener(&app, pid, port)?;
            let targets = cdp::targets(port)?;
            if targets.len() > 16 {
                return Err("官方界面窗口超过兼容模式限制，请减少窗口后重新应用。".into());
            }
            let present: Vec<_> = targets.iter().map(|target| target.id.clone()).collect();
            sessions.retain(|id, _| present.contains(id));
            if targets.is_empty() {
                return Err("没有受支持的 app://-/index.html 官方界面，未执行脚本。".into());
            }
            let mut ready = 0;
            for target in targets {
                if !sessions.contains_key(&target.id) {
                    let mut session = cdp::Session::connect(&target, port)?;
                    session.install(&source)?;
                    sessions.insert(target.id.clone(), session);
                }
                let session = sessions.get_mut(&target.id).unwrap();
                match session.evaluate(&check) {
                    Ok(status) => {
                        if status.get("ready").and_then(Value::as_bool) == Some(true) {
                            ready += 1;
                        } else {
                            last_error =
                                "已连接官方界面，但尚未确认翻译开关、目标语言和官方语言包。".into();
                        }
                    }
                    Err(error) => {
                        // Page.reload creates a new execution context. Keep its
                        // CDP session and registered script while that context
                        // starts; reconnecting/reloading here would loop forever.
                        last_error = error;
                    }
                }
            }
            Ok(ready)
        })();
        match attempt {
            Ok(ready) if ready > 0 => {
                ever_ready = true;
                waiting_since = None;
                progress(
                    &state,
                    "active",
                    "当前 Codex 窗口已验证简体中文兼容。",
                    ready,
                )
            }
            Ok(_) => {
                waiting_since.get_or_insert_with(Instant::now);
                progress(
                    &state,
                    "starting",
                    "已连接官方界面，等待翻译开关和目标语言包就绪；尚未报告应用成功。",
                    0,
                );
            }
            Err(error) => {
                last_error = error;
                if ever_ready {
                    progress(&state, "failed", &last_error, 0);
                    break;
                }
            }
        }
        if !ever_ready && started.elapsed() > Duration::from_secs(35) {
            progress(&state,"failed",&format!("未能验证语言兼容：{last_error} 请使用“恢复并重启”恢复正常启动；此构建可能不支持该模式。"),0);
            break;
        }
        if ever_ready
            && waiting_since.is_some_and(|start| start.elapsed() > Duration::from_secs(20))
        {
            progress(&state,"failed","官方界面长时间未重新验证目标语言，兼容状态已失效；请检查是否在官方设置中改过语言，或恢复后重新应用。",0);
            break;
        }
        for _ in 0..10 {
            if cancel.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EditMode {
    Enable,
    Disable,
    Restore,
    Restart(Option<String>),
}

fn plan_apply(prefs: &Preferences, current: &Option<String>, restore: bool) -> Result<EditMode> {
    if restore {
        return Ok(EditMode::Restore);
    }
    match pending_enabled(prefs, current) {
        Some(true) => Ok(EditMode::Enable),
        Some(false) => Ok(EditMode::Disable),
        None => Ok(EditMode::Restart(current.clone())),
    }
}

fn edit_stopped_locale(
    home: &Path,
    prefs: &mut Preferences,
    expected_preferences: Option<&[u8]>,
    mode: &EditMode,
    ensure_stopped: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<()> {
    let _guard = acquire_live_config_lock(&home)?;
    ensure_stopped().map_err(CodexxError::Config)?;
    let (before, mut doc) = document(&home)?;
    let current = locale_value(&doc)?;
    if matches!(mode, EditMode::Enable | EditMode::Disable)
        && prefs.pending.is_some()
        && pending_enabled(prefs, &current).is_none()
    {
        return Err(CodexxError::Config(
            "Codex 的语言配置在重启前已改变，未覆盖该修改；请重新选择开关。".into(),
        ));
    }
    match mode {
        EditMode::Enable => {
            let retrying_written_intent = prefs
                .pending
                .as_ref()
                .is_some_and(|pending| pending.written_locale.is_some());
            if !retrying_written_intent || !prefs.previous_captured || current != prefs.applied {
                prefs.previous = current.clone();
                prefs.previous_captured = true;
            }
            change_locale(&mut doc, Some("zh-CN"))?;
            prefs.locale = Locale::Chinese;
            prefs.enabled = true;
            prefs.applied = Some("zh-CN".into());
        }
        EditMode::Disable => {
            if chinese_enabled(current.as_deref()) {
                let previous = (prefs.previous_captured && current == prefs.applied)
                    .then_some(prefs.previous.as_deref())
                    .flatten()
                    .filter(|previous| !chinese_enabled(Some(previous)));
                change_locale(&mut doc, previous)?;
            }
            prefs.locale = Locale::Auto;
            prefs.enabled = false;
            // Keep recovery ownership until the new process starts. A failed
            // launch must still be able to finish this same disable request.
        }
        EditMode::Restore => {
            if prefs.previous_captured && current == prefs.applied {
                change_locale(&mut doc, prefs.previous.as_deref())?;
            }
            prefs.previous_captured = false;
            prefs.previous = None;
            prefs.applied = None;
            prefs.enabled = false;
            prefs.locale = Locale::Auto;
            prefs.pending = None;
        }
        EditMode::Restart(expected) => {
            if &current != expected {
                return Err(CodexxError::Config(
                    "Codex 的语言配置在重启前已改变，未覆盖该修改。".into(),
                ));
            }
            if prefs.pending.is_some() {
                let mut refreshed = prefs.clone();
                refreshed.pending = None;
                write_preferences(home, expected_preferences, &refreshed)?;
                *prefs = refreshed;
            }
            return Ok(());
        }
    }
    if let Some(pending) = &mut prefs.pending {
        pending.written_locale = Some(LocaleSnapshot {
            locale: locale_value(&doc)?,
        });
    }
    let replacement = doc.to_string();
    // Capture the recovery intent before changing the official setting.
    if matches!(mode, EditMode::Restore) {
        atomic_write_if_unchanged(
            &crate::config_path(&home),
            before.as_deref(),
            replacement.as_bytes(),
        )?;
        write_preferences(home, expected_preferences, prefs)
    } else {
        commit_language_intent(home, expected_preferences, prefs, || {
            atomic_write_if_unchanged(
                &crate::config_path(&home),
                before.as_deref(),
                replacement.as_bytes(),
            )
        })
    }
}

fn finish_language_apply(home: &Path, prefs: &mut Preferences, mode: &EditMode) -> Result<()> {
    if prefs.pending.is_none() {
        return Ok(());
    }
    let expected = serde_json::to_vec_pretty(prefs)
        .map_err(|_| CodexxError::Config("无法编码语言恢复记录".into()))?;
    let mut finished = prefs.clone();
    finished.pending = None;
    if matches!(mode, EditMode::Disable | EditMode::Restore) {
        finished.previous_captured = false;
        finished.previous = None;
        finished.applied = None;
    }
    write_preferences(home, Some(&expected), &finished)?;
    *prefs = finished;
    Ok(())
}

fn commit_language_intent(
    home: &Path,
    expected: Option<&[u8]>,
    prefs: &Preferences,
    commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    write_preferences(home, expected, prefs)?;
    let staged = serde_json::to_vec_pretty(prefs)
        .map_err(|_| CodexxError::Config("无法编码语言恢复记录".into()))?;
    match commit() {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(rollback) =
                restore_file_snapshot_if_unchanged(&prefs_path(home)?, Some(&staged), expected)
            {
                return Err(CodexxError::Config(format!(
                    "{error}；偏好恢复记录已被另外修改，未覆盖它：{rollback}"
                )));
            }
            Err(error)
        }
    }
}

fn scope_available(existing: Option<&str>, requested: &str) -> Result<()> {
    if existing.is_some_and(|existing| existing != requested) {
        return Err(CodexxError::Config(
            "另一个配置目录正在使用语言兼容；请切回该目录恢复后再应用此目录，未退出任何客户端。"
                .into(),
        ));
    }
    Ok(())
}

fn complete_after_quit<T>(
    action: impl FnOnce() -> Result<T>,
    recover: impl FnOnce() -> std::result::Result<(), String>,
) -> Result<T> {
    match action() {
        Ok(result) => Ok(result),
        Err(error) => match recover() {
            Ok(()) => Err(CodexxError::Config(format!(
                "{error}；已恢复正常启动，兼容尚未应用。"
            ))),
            Err(recovery) => Err(CodexxError::Config(format!(
                "{error}；正常启动也未完成：{recovery} 请手动打开官方客户端。"
            ))),
        },
    }
}

struct PreparedRuntime {
    reservation: Option<std::net::TcpListener>,
    port: u16,
    start: Option<std::sync::mpsc::Sender<u32>>,
    runtime: Option<Runtime>,
}
impl PreparedRuntime {
    fn prepare(home: &Path, app: desktop::DesktopApp, locale: Locale) -> Result<Self> {
        let reservation =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|_| {
                CodexxError::Config("无法分配随机本机兼容端口，客户端尚未退出。".into())
            })?;
        let port = reservation
            .local_addr()
            .map_err(|_| CodexxError::Config("无法读取兼容端口，客户端尚未退出。".into()))?
            .port();
        let mut nonce = [0u8; 32];
        getrandom::getrandom(&mut nonce)
            .map_err(|_| CodexxError::Config("无法生成兼容会话标识，客户端尚未退出。".into()))?;
        let owner = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(Progress {
            phase: "starting".into(),
            message: "正在验证官方进程、界面来源和语言包。".into(),
            ready_pages: 0,
        }));
        let cancelled = cancel.clone();
        let watched = state.clone();
        let (sender, receiver) = std::sync::mpsc::channel::<u32>();
        let thread = std::thread::Builder::new()
            .name("codex-language-compatibility".into())
            .spawn(move || {
                if let Ok(pid) = receiver.recv() {
                    supervise(app, pid, port, locale, owner, cancelled, watched);
                }
            })
            .map_err(|_| CodexxError::Config("无法创建兼容监督线程，客户端尚未退出。".into()))?;
        Ok(Self {
            reservation: Some(reservation),
            port,
            start: Some(sender),
            runtime: Some(Runtime {
                scope: crate::paths::normalized_path_scope(home),
                locale,
                cancel,
                thread: Some(thread),
                progress: state,
            }),
        })
    }
    fn release_port(&mut self) {
        self.reservation.take();
    }
    fn activate(mut self, pid: u32) -> Result<Runtime> {
        self.start
            .take()
            .unwrap()
            .send(pid)
            .map_err(|_| CodexxError::Config("兼容监督未接收启动结果，请恢复后重试。".into()))?;
        Ok(self.runtime.take().unwrap())
    }
}
impl Drop for PreparedRuntime {
    fn drop(&mut self) {
        self.start.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.stop();
        }
    }
}

/// The settings restart button and toast share the language-aware launch path.
/// Inspection does not change preferences, settings, or the running client.
pub(crate) fn restart_client(config_dir: Option<String>) -> Result<(String, bool)> {
    let _operation = operation()
        .lock()
        .map_err(|_| CodexxError::Config("已有语言或重启操作正在进行".into()))?;
    let scope = crate::paths::normalized_path_scope(&config_home(config_dir.clone())?);
    let has_runtime = {
        let active = runtime()
            .lock()
            .map_err(|_| CodexxError::Config("语言兼容状态忙，请重试".into()))?;
        scope_available(active.as_ref().map(|item| item.scope.as_str()), &scope)?;
        active.is_some()
    };
    let status = get_status(config_dir.clone())?;
    if (status.supported || cfg!(target_os = "windows"))
        && (status.enabled || status.pending_enabled.is_some() || has_runtime)
    {
        let applied = apply_locked(config_dir, false)?;
        return Ok((applied.app_name.unwrap_or_else(|| "Codex".into()), true));
    }
    crate::platform::restart_codex_desktop().map_err(CodexxError::Config)
}

/// Explicit user action only. Never called during status reads or app startup.
pub(crate) fn apply(config_dir: Option<String>, restore: bool) -> Result<LanguageStatus> {
    let _operation = operation()
        .lock()
        .map_err(|_| CodexxError::Config("已有语言操作正在进行".into()))?;
    apply_locked(config_dir, restore)
}

fn apply_locked(config_dir: Option<String>, restore: bool) -> Result<LanguageStatus> {
    let home = config_home(config_dir.clone())?;
    let scope = crate::paths::normalized_path_scope(&home);
    {
        let current = runtime()
            .lock()
            .map_err(|_| CodexxError::Config("语言兼容状态忙，客户端尚未退出。".into()))?;
        scope_available(
            current.as_ref().map(|runtime| runtime.scope.as_str()),
            &scope,
        )?;
    }
    let (bytes, mut prefs) = read_preferences(&home)?;
    let (_, initial) = document(&home)?;
    let actual = locale_value(&initial)?;
    let mode = plan_apply(&prefs, &actual, restore)?;
    let compatibility = matches!(&mode, EditMode::Enable)
        || matches!(&mode, EditMode::Restart(locale) if chinese_enabled(locale.as_deref()));
    let app = desktop::detect_for_operation().map_err(CodexxError::Config)?;
    desktop::validate(&app).map_err(CodexxError::Config)?;
    desktop::validate_launch(&app, &home).map_err(CodexxError::Config)?;
    // Allocate every fallible supervision resource before quitting the app.
    desktop::prepare_launch().map_err(CodexxError::Config)?;
    let mut prepared = if compatibility {
        Some(PreparedRuntime::prepare(
            &home,
            app.clone(),
            Locale::Chinese,
        )?)
    } else {
        None
    };
    desktop::quit(&app).map_err(CodexxError::Config)?;
    let previous = runtime()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(previous) = previous {
        previous.stop();
    }
    let started_pid = std::cell::Cell::new(None);
    complete_after_quit(
        || {
            edit_stopped_locale(&home, &mut prefs, bytes.as_deref(), &mode, || {
                desktop::ensure_stopped(&app)
            })?;
            if let Some(mut pending) = prepared.take() {
                pending.release_port();
                let pid = desktop::launch(&app, &home, Some(pending.port))
                    .map_err(CodexxError::Config)?;
                started_pid.set(Some(pid));
                let active = pending.activate(pid)?;
                if let Err(error) = finish_language_apply(&home, &mut prefs, &mode) {
                    active.stop();
                    return Err(error);
                }
                *runtime()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(active);
            } else {
                let pid = desktop::launch(&app, &home, None).map_err(CodexxError::Config)?;
                started_pid.set(Some(pid));
                finish_language_apply(&home, &mut prefs, &mode)?;
            }
            Ok(())
        },
        || {
            if let Some(pid) = started_pid.get() {
                desktop::quit_spawned(&app, pid)?;
            }
            desktop::launch(&app, &home, None).map(|_| ())
        },
    )?;
    get_status(config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_dir(home: &Path) -> Option<String> {
        Some(home.to_string_lossy().into_owned())
    }

    fn fixture_locale(home: &Path, locale: &str) {
        std::fs::write(
            crate::config_path(home),
            format!("# private language fixture\n[desktop]\nlocaleOverride='{locale}'\nunknown='keep'\n"),
        )
        .unwrap();
    }

    fn edit_pending(home: &Path, mode: &EditMode) -> Preferences {
        let (bytes, mut prefs) = read_preferences(home).unwrap();
        edit_stopped_locale(home, &mut prefs, bytes.as_deref(), mode, || Ok(())).unwrap();
        prefs
    }

    #[test]
    fn status_reads_real_locale_and_old_preferences_do_not_create_pending_changes() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "zh-CN");
        let old = br#"{"locale":"en-US","enabled":false,"unknownPreference":"keep"}"#;
        let path = prefs_path(home.path()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, old).unwrap();
        let before = std::fs::read(crate::config_path(home.path())).unwrap();
        let status = get_status(config_dir(home.path())).unwrap();
        assert_eq!(status.locale, "zh-CN");
        assert!(status.enabled);
        assert_eq!(status.pending_enabled, None);
        assert!(serde_json::to_value(&status)
            .unwrap()
            .get("requiresManager")
            .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), old);
        assert_eq!(
            std::fs::read(crate::config_path(home.path())).unwrap(),
            before
        );

        fixture_locale(home.path(), "ja-JP");
        let status = get_status(config_dir(home.path())).unwrap();
        assert_eq!(status.locale, "ja-JP");
        assert!(!status.enabled);
        assert_eq!(status.pending_enabled, None);
        assert_eq!(std::fs::read(&path).unwrap(), old);
    }

    #[test]
    fn saving_switch_only_stages_an_explicit_target_and_can_cancel_it() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        let before = std::fs::read(crate::config_path(home.path())).unwrap();
        let status = save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        assert_eq!(status.locale, "fr-FR");
        assert!(!status.enabled);
        assert_eq!(status.pending_enabled, Some(true));
        assert_eq!(
            std::fs::read(crate::config_path(home.path())).unwrap(),
            before
        );
        let (_, prefs) = read_preferences(home.path()).unwrap();
        assert_eq!(
            prefs.pending.as_ref().unwrap().captured_locale.as_deref(),
            Some("fr-FR")
        );

        let status = save_preferences(config_dir(home.path()), Locale::Auto, false).unwrap();
        assert_eq!(status.pending_enabled, None);
        assert!(!read_preferences(home.path()).unwrap().1.pending.is_some());
        assert_eq!(
            std::fs::read(crate::config_path(home.path())).unwrap(),
            before
        );
        assert!(save_preferences(config_dir(home.path()), Locale::English, true).is_err());
    }

    #[test]
    fn external_language_changes_invalidate_pending_without_status_writes() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        let bytes = std::fs::read(prefs_path(home.path()).unwrap()).unwrap();
        fixture_locale(home.path(), "ja-JP");
        let status = get_status(config_dir(home.path())).unwrap();
        assert_eq!(status.locale, "ja-JP");
        assert_eq!(status.pending_enabled, None);
        assert_eq!(
            std::fs::read(prefs_path(home.path()).unwrap()).unwrap(),
            bytes
        );
        let (_, mut prefs) = read_preferences(home.path()).unwrap();
        let (_, doc) = document(home.path()).unwrap();
        let mode = plan_apply(&prefs, &locale_value(&doc).unwrap(), false).unwrap();
        assert_eq!(mode, EditMode::Restart(Some("ja-JP".into())));
        let before = std::fs::read(crate::config_path(home.path())).unwrap();
        assert!(edit_stopped_locale(
            home.path(),
            &mut prefs,
            Some(&bytes),
            &EditMode::Enable,
            || Ok(())
        )
        .is_err());
        assert_eq!(
            std::fs::read(crate::config_path(home.path())).unwrap(),
            before
        );
        edit_stopped_locale(home.path(), &mut prefs, Some(&bytes), &mode, || Ok(())).unwrap();
        assert!(read_preferences(home.path()).unwrap().1.pending.is_none());
        assert_eq!(
            std::fs::read(crate::config_path(home.path())).unwrap(),
            before
        );
    }

    #[test]
    fn external_change_back_to_original_locale_wins_after_a_completed_config_write() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        edit_pending(home.path(), &EditMode::Enable);
        fixture_locale(home.path(), "fr-FR");
        let (_, prefs) = read_preferences(home.path()).unwrap();
        let current = Some("fr-FR".into());
        assert_eq!(pending_enabled(&prefs, &current), None);
        assert_eq!(
            plan_apply(&prefs, &current, false).unwrap(),
            EditMode::Restart(current)
        );
        assert_eq!(
            get_status(config_dir(home.path())).unwrap().pending_enabled,
            None
        );
    }

    #[test]
    fn switch_enable_and_disable_restore_previous_locale_and_finish_pending() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        let mut prefs = edit_pending(home.path(), &EditMode::Enable);
        assert_eq!(prefs.previous.as_deref(), Some("fr-FR"));
        let status = get_status(config_dir(home.path())).unwrap();
        assert!(status.enabled);
        assert_eq!(status.pending_enabled, Some(true));
        finish_language_apply(home.path(), &mut prefs, &EditMode::Enable).unwrap();
        assert_eq!(
            get_status(config_dir(home.path())).unwrap().pending_enabled,
            None
        );

        save_preferences(config_dir(home.path()), Locale::Auto, false).unwrap();
        let mut prefs = edit_pending(home.path(), &EditMode::Disable);
        let status = get_status(config_dir(home.path())).unwrap();
        assert_eq!(status.locale, "fr-FR");
        assert!(!status.enabled);
        assert_eq!(status.pending_enabled, Some(false));
        assert!(
            prefs.previous_captured,
            "ownership remains until a successful launch"
        );
        finish_language_apply(home.path(), &mut prefs, &EditMode::Disable).unwrap();
        assert!(prefs.pending.is_none());
        assert!(!prefs.previous_captured);
        assert_eq!(
            get_status(config_dir(home.path())).unwrap().pending_enabled,
            None
        );
    }

    #[test]
    fn a_new_enable_captures_current_locale_instead_of_an_old_managed_baseline() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "en-US");
        let prefs = Preferences {
            previous: Some("fr-FR".into()),
            previous_captured: true,
            applied: Some("en-US".into()),
            ..Preferences::default()
        };
        write_preferences(home.path(), None, &prefs).unwrap();
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        let prefs = edit_pending(home.path(), &EditMode::Enable);
        assert_eq!(prefs.previous.as_deref(), Some("en-US"));
    }

    #[test]
    fn disabling_preexisting_chinese_clears_it_even_without_recovery_or_with_chinese_previous() {
        for previous_chinese in [false, true] {
            let home = tempfile::tempdir().unwrap();
            fixture_locale(home.path(), "zh-CN");
            if previous_chinese {
                let prefs = Preferences {
                    previous_captured: true,
                    previous: Some("zh-CN".into()),
                    applied: Some("zh-CN".into()),
                    ..Preferences::default()
                };
                write_preferences(home.path(), None, &prefs).unwrap();
            }
            save_preferences(config_dir(home.path()), Locale::Auto, false).unwrap();
            let mut prefs = edit_pending(home.path(), &EditMode::Disable);
            let (_, doc) = document(home.path()).unwrap();
            assert_eq!(locale_value(&doc).unwrap(), None);
            assert_eq!(doc["desktop"]["unknown"].as_str(), Some("keep"));
            finish_language_apply(home.path(), &mut prefs, &EditMode::Disable).unwrap();
            let status = get_status(config_dir(home.path())).unwrap();
            assert_eq!(status.locale, "auto");
            assert!(!status.enabled);
            assert_eq!(status.pending_enabled, None);
        }
    }

    #[test]
    fn failed_launch_after_config_write_keeps_honest_retry_state_for_both_switch_targets() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        let result: Result<()> = complete_after_quit(
            || {
                edit_pending(home.path(), &EditMode::Enable);
                Err(CodexxError::Config("fixture launch failure".into()))
            },
            || Ok(()),
        );
        assert!(result.is_err());
        let status = get_status(config_dir(home.path())).unwrap();
        assert!(status.enabled);
        assert_eq!(status.pending_enabled, Some(true));
        let mut prefs = edit_pending(home.path(), &EditMode::Enable);
        assert_eq!(prefs.previous.as_deref(), Some("fr-FR"));
        finish_language_apply(home.path(), &mut prefs, &EditMode::Enable).unwrap();

        save_preferences(config_dir(home.path()), Locale::Auto, false).unwrap();
        edit_pending(home.path(), &EditMode::Disable);
        let status = get_status(config_dir(home.path())).unwrap();
        assert_eq!(status.locale, "fr-FR");
        assert_eq!(status.pending_enabled, Some(false));
        let mut prefs = edit_pending(home.path(), &EditMode::Disable);
        assert_eq!(
            locale_value(&document(home.path()).unwrap().1)
                .unwrap()
                .as_deref(),
            Some("fr-FR")
        );
        finish_language_apply(home.path(), &mut prefs, &EditMode::Disable).unwrap();
        assert_eq!(
            get_status(config_dir(home.path())).unwrap().pending_enabled,
            None
        );
    }

    #[test]
    fn failed_config_commit_restores_original_pending_and_does_not_claim_external_target() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "fr-FR");
        save_preferences(config_dir(home.path()), Locale::Chinese, true).unwrap();
        let (bytes, mut staged) = read_preferences(home.path()).unwrap();
        staged.pending.as_mut().unwrap().written_locale = Some(LocaleSnapshot {
            locale: Some("zh-CN".into()),
        });
        let result = commit_language_intent(home.path(), bytes.as_deref(), &staged, || {
            fixture_locale(home.path(), "zh-CN");
            Err(CodexxError::Config("fixture CAS failure".into()))
        });
        assert!(result.is_err());
        let (after, prefs) = read_preferences(home.path()).unwrap();
        assert_eq!(after, bytes);
        assert!(prefs.pending.as_ref().unwrap().written_locale.is_none());
        let status = get_status(config_dir(home.path())).unwrap();
        assert!(status.enabled);
        assert_eq!(status.pending_enabled, None);
    }

    #[test]
    fn restart_plan_uses_actual_chinese_even_when_legacy_preferences_are_disabled() {
        let home = tempfile::tempdir().unwrap();
        fixture_locale(home.path(), "zh-CN");
        let mut prefs = Preferences::default();
        let actual = Some("zh-CN".into());
        let mode = plan_apply(&prefs, &actual, false).unwrap();
        assert_eq!(mode, EditMode::Restart(actual));
        edit_stopped_locale(home.path(), &mut prefs, None, &mode, || Ok(())).unwrap();
        assert_eq!(
            locale_value(&document(home.path()).unwrap().1)
                .unwrap()
                .as_deref(),
            Some("zh-CN")
        );
        assert!(!prefs_path(home.path()).unwrap().exists());
        fixture_locale(home.path(), "ja-JP");
        assert!(edit_stopped_locale(home.path(), &mut prefs, None, &mode, || Ok(())).is_err());
        assert_eq!(
            locale_value(&document(home.path()).unwrap().1)
                .unwrap()
                .as_deref(),
            Some("ja-JP")
        );
    }

    #[test]
    fn toml_language_edit_preserves_comments_unknown_fields_and_other_desktop_settings() {
        let mut doc="# fixture\nmodel='test'\n[desktop]\nunknown_custom='keep' # comment\nlocaleOverride='fr-FR'\n[mcp_servers.fixture]\ncommand='keep'\n".parse::<DocumentMut>().unwrap();
        change_locale(&mut doc, Some("zh-CN")).unwrap();
        assert_eq!(locale_value(&doc).unwrap(), Some("zh-CN".into()));
        assert!(doc.to_string().contains("unknown_custom='keep' # comment"));
        assert_eq!(
            doc["mcp_servers"]["fixture"]["command"].as_str(),
            Some("keep")
        );
        change_locale(&mut doc, None).unwrap();
        assert!(doc["desktop"].get("localeOverride").is_none());
        assert_eq!(doc["desktop"]["unknown_custom"].as_str(), Some("keep"));
    }
    #[test]
    fn rejects_invalid_desktop_shape_without_replacing_it() {
        let mut doc = "desktop='user value'".parse::<DocumentMut>().unwrap();
        let before = doc.to_string();
        assert!(change_locale(&mut doc, Some("zh-CN")).is_err());
        assert_eq!(doc.to_string(), before);
    }
    #[test]
    fn generated_script_parameters_are_data_and_only_supported_locales_deserialize() {
        let script = code(Locale::Chinese, &"a".repeat(64));
        assert!(script.contains("\"locale\":\"zh-CN\""));
        assert!(!script.contains("__CODEXX_LANGUAGE_CONFIG__"));
        assert!(serde_json::from_str::<Locale>("\"unsupported\"").is_err());
    }

    #[test]
    fn private_config_apply_and_restore_preserve_unrelated_edits_and_previous_locale() {
        let home = tempfile::tempdir().unwrap();
        let path = crate::config_path(home.path());
        std::fs::write(&path,"# private fixture\nmodel='fixture'\n[desktop]\nlocaleOverride='fr-FR'\nunknown='keep' # comment\n[mcp_servers.fixture]\ncommand='fixture-tool'\n").unwrap();
        let mut prefs = Preferences {
            locale: Locale::Chinese,
            enabled: true,
            ..Preferences::default()
        };
        edit_stopped_locale(home.path(), &mut prefs, None, &EditMode::Enable, || Ok(())).unwrap();
        let (_, doc) = document(home.path()).unwrap();
        assert_eq!(locale_value(&doc).unwrap(), Some("zh-CN".into()));
        assert_eq!(prefs.previous, Some("fr-FR".into()));
        assert!(prefs.previous_captured);
        let mut changed = doc;
        changed["desktop"]["unknown"] = value("external edit");
        std::fs::write(&path, changed.to_string()).unwrap();
        let (bytes, _) = read_preferences(home.path()).unwrap();
        edit_stopped_locale(
            home.path(),
            &mut prefs,
            bytes.as_deref(),
            &EditMode::Restore,
            || Ok(()),
        )
        .unwrap();
        let (_, restored) = document(home.path()).unwrap();
        assert_eq!(locale_value(&restored).unwrap(), Some("fr-FR".into()));
        assert_eq!(
            restored["desktop"]["unknown"].as_str(),
            Some("external edit")
        );
        assert_eq!(
            restored["mcp_servers"]["fixture"]["command"].as_str(),
            Some("fixture-tool")
        );
        assert!(!prefs.previous_captured);
    }

    #[test]
    fn restore_does_not_overwrite_a_later_official_language_change() {
        let home = tempfile::tempdir().unwrap();
        let path = crate::config_path(home.path());
        std::fs::write(&path, "[desktop]\nlocaleOverride='ja-JP'\nunknown='keep'\n").unwrap();
        let mut prefs = Preferences {
            locale: Locale::Chinese,
            enabled: true,
            previous: Some("en-US".into()),
            previous_captured: true,
            applied: Some("zh-CN".into()),
            ..Preferences::default()
        };
        write_preferences(home.path(), None, &prefs).unwrap();
        let (bytes, _) = read_preferences(home.path()).unwrap();
        edit_stopped_locale(
            home.path(),
            &mut prefs,
            bytes.as_deref(),
            &EditMode::Restore,
            || Ok(()),
        )
        .unwrap();
        let (_, doc) = document(home.path()).unwrap();
        assert_eq!(locale_value(&doc).unwrap(), Some("ja-JP".into()));
        assert_eq!(doc["desktop"]["unknown"].as_str(), Some("keep"));
    }

    #[test]
    fn running_app_and_changed_preferences_fail_before_any_config_write() {
        let home = tempfile::tempdir().unwrap();
        let path = crate::config_path(home.path());
        let original = "[desktop]\nlocaleOverride='en-US'\n";
        std::fs::write(&path, original).unwrap();
        let mut prefs = Preferences {
            locale: Locale::Chinese,
            enabled: true,
            ..Preferences::default()
        };
        assert!(
            edit_stopped_locale(home.path(), &mut prefs, None, &EditMode::Enable, || Err(
                "fixture app still running".into()
            ))
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!prefs_path(home.path()).unwrap().exists());
        write_preferences(home.path(), None, &prefs).unwrap();
        assert!(
            edit_stopped_locale(home.path(), &mut prefs, None, &EditMode::Enable, || Ok(()))
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn failed_config_commit_rolls_back_intent_and_cannot_claim_external_locale() {
        let home = tempfile::tempdir().unwrap();
        let path = crate::config_path(home.path());
        std::fs::write(&path, "[desktop]\nlocaleOverride='fr-FR'\n").unwrap();
        let prefs = Preferences {
            locale: Locale::Chinese,
            enabled: true,
            previous: Some("fr-FR".into()),
            previous_captured: true,
            applied: Some("zh-CN".into()),
            ..Preferences::default()
        };
        let result = commit_language_intent(home.path(), None, &prefs, || {
            std::fs::write(
                &path,
                "[desktop]\nlocaleOverride='zh-CN' # external write\n",
            )
            .unwrap();
            Err(CodexxError::Config("fixture concurrent config edit".into()))
        });
        assert!(result.is_err());
        let (bytes, mut restored) = read_preferences(home.path()).unwrap();
        assert!(bytes.is_none());
        assert!(!restored.previous_captured);
        edit_stopped_locale(home.path(), &mut restored, None, &EditMode::Restore, || {
            Ok(())
        })
        .unwrap();
        let (_, doc) = document(home.path()).unwrap();
        assert_eq!(locale_value(&doc).unwrap(), Some("zh-CN".into()));
    }

    #[test]
    fn intent_rollback_preserves_concurrent_external_preference_changes() {
        let home = tempfile::tempdir().unwrap();
        let prefs = Preferences {
            locale: Locale::Chinese,
            ..Preferences::default()
        };
        let path = prefs_path(home.path()).unwrap();
        let result = commit_language_intent(home.path(), None, &prefs, || {
            std::fs::write(&path, br#"{"locale":"en-US","external_field":"keep"}"#).unwrap();
            Err(CodexxError::Config("fixture failed write".into()))
        });
        assert!(result.is_err());
        let (_, after) = read_preferences(home.path()).unwrap();
        assert_eq!(after.locale, Locale::English);
        assert_eq!(after.extra.get("external_field"), Some(&json!("keep")));
    }

    #[test]
    fn failure_after_quit_attempts_normal_start_and_reports_recovery_failure() {
        let calls = std::cell::Cell::new(0);
        let result: Result<()> = complete_after_quit(
            || Err(CodexxError::Config("fixture launch/config failure".into())),
            || {
                calls.set(calls.get() + 1);
                Ok(())
            },
        );
        assert!(result.unwrap_err().to_string().contains("已恢复正常启动"));
        assert_eq!(calls.get(), 1);
        let result: Result<()> = complete_after_quit(
            || Err(CodexxError::Config("fixture failure".into())),
            || Err("fixture normal launch failure".into()),
        );
        assert!(result.unwrap_err().to_string().contains("手动打开"));
        assert_eq!(
            complete_after_quit(
                || Ok(7),
                || panic!("successful action must not restart twice")
            )
            .unwrap(),
            7
        );
    }

    #[test]
    fn another_scope_is_rejected_before_the_desktop_operation() {
        assert!(scope_available(Some("scope-a"), "scope-b").is_err());
        assert!(scope_available(Some("scope-a"), "scope-a").is_ok());
        assert!(scope_available(None, "scope-b").is_ok());
    }

    #[test]
    fn prepared_supervision_drops_without_launching_any_application() {
        let home = tempfile::tempdir().unwrap();
        let app = desktop::DesktopApp {
            path: home.path().join("fixture.app"),
            executable: home.path().join("never-execute"),
            bundle_id: "com.openai.codex".into(),
            name: "fixture".into(),
            version: "fixture".into(),
        };
        let pending = PreparedRuntime::prepare(home.path(), app, Locale::Chinese).unwrap();
        let port = pending.port;
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
        drop(pending);
        assert!(std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok());
    }
}
