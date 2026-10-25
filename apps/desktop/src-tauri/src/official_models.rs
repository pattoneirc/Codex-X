//! Account-independent model picker data from an isolated local Codex CLI.
//! No real Codex home, authentication snapshot or running app-server is used.
use crate::platform;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

// Fallback checked against https://developers.openai.com/api/docs/models on
// 2026-10-24. Native isDefault wins; neither catalog promises account access.
const BUNDLED: &str = include_str!("../../src/data/official-models.json");
const SUCCESS_TTL: Duration = Duration::from_secs(15 * 60);
const FAILURE_TTL: Duration = Duration::from_secs(60);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_CANDIDATES: usize = 8;
const MAX_LINE_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_MESSAGES: usize = 128;
const MAX_PAGES: usize = 16;
const MAX_MODELS: usize = 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialModel {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) is_default: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OfficialModelsResult {
    pub(crate) models: Vec<OfficialModel>,
    pub(crate) recommended_model: Option<String>,
    pub(crate) source: String,
    pub(crate) warning: Option<String>,
}

fn bundled(warning: Option<&str>) -> OfficialModelsResult {
    let mut catalog: OfficialModelsResult =
        serde_json::from_str(BUNDLED).expect("bundled official models must be valid JSON");
    catalog.warning = warning.map(ToString::to_string);
    catalog
}

struct CachedCatalog {
    fingerprint: String,
    completed: Instant,
    result: OfficialModelsResult,
}

fn cache() -> &'static Mutex<Option<CachedCatalog>> {
    static CACHE: OnceLock<Mutex<Option<CachedCatalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn resolve_program(candidate: &Path) -> Option<PathBuf> {
    if candidate.components().count() > 1 {
        if !candidate.is_absolute() {
            return None;
        }
        return candidate.is_file().then(|| {
            candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.to_path_buf())
        });
    }
    let search = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&search) {
        // Do not search a project-relative PATH entry: the catalog child must
        // never accidentally execute a script from the user's current project.
        if !directory.is_absolute() {
            continue;
        }
        let path = directory.join(candidate);
        if path.is_file() {
            return Some(path.canonicalize().unwrap_or(path));
        }
        #[cfg(target_os = "windows")]
        if candidate.extension().is_none() {
            for suffix in ["exe", "cmd", "bat"] {
                let path = path.with_extension(suffix);
                if path.is_file() {
                    return Some(path.canonicalize().unwrap_or(path));
                }
            }
        }
    }
    None
}

fn executable_candidates() -> (Vec<PathBuf>, String) {
    let mut programs = Vec::new();
    let mut seen = HashSet::new();
    let mut digest = Sha256::new();
    for candidate in platform::official_model_executable_candidates() {
        let Some(program) = resolve_program(&candidate) else {
            continue;
        };
        if !seen.insert(program.clone()) {
            continue;
        }
        digest.update(program.as_os_str().as_encoded_bytes());
        digest.update([0]);
        if let Ok(metadata) = fs::metadata(&program) {
            digest.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified().and_then(|time| {
                time.duration_since(UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                digest.update(modified.as_nanos().to_le_bytes());
            }
        }
        programs.push(program);
        if programs.len() == MAX_CANDIDATES {
            break;
        }
    }
    (programs, format!("{:x}", digest.finalize()))
}

#[derive(Clone, Copy)]
enum CatalogError {
    Unavailable,
    Timeout,
    InvalidResponse,
}

struct CatalogChild {
    child: Child,
    stdin: Option<ChildStdin>,
    receiver: Option<mpsc::Receiver<std::result::Result<Vec<u8>, CatalogError>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: mpsc::Receiver<()>,
    messages: usize,
}

impl CatalogChild {
    fn start(
        program: &Path,
        home: &Path,
        workspace: &Path,
    ) -> std::result::Result<Self, CatalogError> {
        let mut command = platform::official_model_command(program, &["app-server"]);
        command.env_clear();
        // Only basic OS execution state survives. API keys, access tokens,
        // provider overrides, host sockets and parent-app identity do not.
        for name in [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "ComSpec",
            "PATHEXT",
            "LANG",
            "LC_ALL",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let private_home = workspace.join("home");
        let temp = workspace.join("tmp");
        let sqlite = workspace.join("sqlite");
        for directory in [&private_home, &temp, &sqlite] {
            fs::create_dir_all(directory).map_err(|_| CatalogError::Unavailable)?;
        }
        for (name, path) in [
            ("HOME", private_home.clone()),
            ("USERPROFILE", private_home),
            ("APPDATA", workspace.join("appdata")),
            ("LOCALAPPDATA", workspace.join("localappdata")),
            ("XDG_CONFIG_HOME", workspace.join("config")),
            ("XDG_CACHE_HOME", workspace.join("cache")),
            ("XDG_DATA_HOME", workspace.join("data")),
            ("XDG_STATE_HOME", workspace.join("state")),
        ] {
            fs::create_dir_all(&path).map_err(|_| CatalogError::Unavailable)?;
            command.env(name, path);
        }
        command
            .env("CODEX_HOME", home)
            .env("CODEX_SQLITE_HOME", sqlite)
            .env("TMP", &temp)
            .env("TEMP", &temp)
            .env("TMPDIR", &temp)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| CatalogError::Unavailable)?;
        let stdin = child.stdin.take();
        let Some(stdout) = child.stdout.take() else {
            platform::terminate_official_model_child(&mut child);
            return Err(CatalogError::Unavailable);
        };
        let (sender, receiver) = mpsc::sync_channel(4);
        let (done, reader_done) = mpsc::channel();
        let reader = match std::thread::Builder::new()
            .name("official-model-catalog-output".into())
            .spawn(move || {
                let mut output = BufReader::new(stdout);
                let mut total = 0usize;
                loop {
                    let mut bytes = Vec::new();
                    let read = output
                        .by_ref()
                        .take((MAX_LINE_BYTES + 1) as u64)
                        .read_until(b'\n', &mut bytes);
                    let item = match read {
                        Ok(0) => break,
                        Ok(_)
                            if bytes.len() <= MAX_LINE_BYTES
                                && total.saturating_add(bytes.len()) <= MAX_OUTPUT_BYTES =>
                        {
                            total += bytes.len();
                            Ok(bytes)
                        }
                        _ => Err(CatalogError::InvalidResponse),
                    };
                    let failed = item.is_err();
                    if sender.send(item).is_err() || failed {
                        break;
                    }
                }
                let _ = done.send(());
            }) {
            Ok(reader) => reader,
            Err(_) => {
                drop(stdin);
                platform::terminate_official_model_child(&mut child);
                return Err(CatalogError::Unavailable);
            }
        };
        let session = Self {
            child,
            stdin,
            receiver: Some(receiver),
            reader: Some(reader),
            reader_done,
            messages: 0,
        };
        if session.stdin.is_none() {
            return Err(CatalogError::Unavailable);
        }
        Ok(session)
    }

    fn send(&mut self, value: Value) -> std::result::Result<(), CatalogError> {
        let mut line = serde_json::to_vec(&value).map_err(|_| CatalogError::InvalidResponse)?;
        line.push(b'\n');
        if line.len() > 2048 {
            return Err(CatalogError::InvalidResponse);
        }
        let stdin = self.stdin.as_mut().ok_or(CatalogError::Unavailable)?;
        stdin
            .write_all(&line)
            .and_then(|_| stdin.flush())
            .map_err(|_| CatalogError::Unavailable)
    }

    fn response(&mut self, id: u64, deadline: Instant) -> std::result::Result<Value, CatalogError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(CatalogError::Timeout)?;
            let bytes = self
                .receiver
                .as_ref()
                .ok_or(CatalogError::Unavailable)?
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => CatalogError::Timeout,
                    _ => CatalogError::Unavailable,
                })??;
            self.messages += 1;
            if self.messages > MAX_MESSAGES {
                return Err(CatalogError::InvalidResponse);
            }
            if bytes.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let message: Value =
                serde_json::from_slice(&bytes).map_err(|_| CatalogError::InvalidResponse)?;
            // Never service server-initiated requests, tools or authentication.
            if message.get("method").is_some() && message.get("id").is_some() {
                return Err(CatalogError::Unavailable);
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if message.get("error").is_some() {
                return Err(CatalogError::Unavailable);
            }
            return message
                .get("result")
                .cloned()
                .ok_or(CatalogError::InvalidResponse);
        }
    }
}

impl Drop for CatalogChild {
    fn drop(&mut self) {
        self.stdin.take();
        // Release a reader blocked on the bounded output channel before reaping.
        self.receiver.take();
        platform::terminate_official_model_child(&mut self.child);
        // An inherited pipe must not turn cleanup into an unbounded join.
        if self
            .reader_done
            .recv_timeout(Duration::from_millis(250))
            .is_ok()
        {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

fn model_id(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|id| {
        !id.is_empty()
            && id.len() <= 256
            && !id.chars().any(|c| c.is_control() || c.is_whitespace())
    })
}

fn query_program(
    program: &Path,
    deadline: Instant,
) -> std::result::Result<OfficialModelsResult, CatalogError> {
    let workspace = tempfile::Builder::new()
        .prefix("codexx-model-catalog-")
        .tempdir()
        .map_err(|_| CatalogError::Unavailable)?;
    let home = workspace.path().join("codex");
    fs::create_dir_all(&home).map_err(|_| CatalogError::Unavailable)?;
    // Never copy a real config/auth/cache. The file store belongs to this new
    // temporary home; no keychain/auto credential store is requested.
    fs::write(home.join("config.toml"), "model_provider = \"openai\"\ncli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\n[analytics]\nenabled = false\n").map_err(|_| CatalogError::Unavailable)?;
    let mut session = CatalogChild::start(program, &home, workspace.path())?;
    session.send(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex_x_model_catalog","title":"Codex-X model catalog","version":env!("CARGO_PKG_VERSION")},"capabilities":null}}))?;
    let initialized = session.response(1, deadline)?;
    if let Some(returned) = initialized.get("codexHome") {
        let returned = returned.as_str().ok_or(CatalogError::InvalidResponse)?;
        let returned = Path::new(returned)
            .canonicalize()
            .map_err(|_| CatalogError::Unavailable)?;
        let expected = home.canonicalize().map_err(|_| CatalogError::Unavailable)?;
        if returned != expected {
            return Err(CatalogError::Unavailable);
        }
    }
    session.send(json!({"method":"initialized"}))?;
    let mut models: Vec<OfficialModel> = Vec::new();
    let mut indexes = HashMap::<String, usize>::new();
    let mut cursor: Option<String> = None;
    let mut cursors = HashSet::new();
    let mut total_models = 0usize;
    for page in 0..MAX_PAGES {
        if Instant::now() >= deadline {
            return Err(CatalogError::Timeout);
        }
        let id = 10 + page as u64;
        let mut params = json!({"limit":100,"includeHidden":false});
        if let Some(cursor) = &cursor {
            params["cursor"] = json!(cursor);
        }
        session.send(json!({"id":id,"method":"model/list","params":params}))?;
        let response = session.response(id, deadline)?;
        let data = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or(CatalogError::InvalidResponse)?;
        total_models = total_models.saturating_add(data.len());
        if total_models > MAX_MODELS {
            return Err(CatalogError::InvalidResponse);
        }
        for entry in data {
            match entry.get("hidden") {
                Some(Value::Bool(false)) | None => {}
                _ => continue,
            }
            // `model` is the invocation slug; `id` is a legacy fallback only.
            let Some(id) = model_id(entry.get("model")).or_else(|| model_id(entry.get("id")))
            else {
                continue;
            };
            let is_default = entry
                .get("isDefault")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(index) = indexes.get(id) {
                models[*index].is_default |= is_default;
                continue;
            }
            let display_name = entry
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| {
                    !name.is_empty()
                        && name.chars().count() <= 200
                        && !name.chars().any(char::is_control)
                })
                .unwrap_or(id);
            indexes.insert(id.to_string(), models.len());
            models.push(OfficialModel {
                id: id.to_string(),
                display_name: display_name.to_string(),
                is_default,
            });
        }
        match response.get("nextCursor") {
            None | Some(Value::Null) => {
                if models.is_empty() {
                    return Err(CatalogError::InvalidResponse);
                }
                let recommended_model = models
                    .iter()
                    .find(|model| model.is_default)
                    .map(|model| model.id.clone());
                return Ok(OfficialModelsResult {
                    models,
                    recommended_model,
                    source: "local".into(),
                    warning: None,
                });
            }
            Some(Value::String(next))
                if !next.is_empty() && next.len() <= 512 && !next.chars().any(char::is_control) =>
            {
                if !cursors.insert(next.clone()) {
                    return Err(CatalogError::InvalidResponse);
                }
                cursor = Some(next.clone());
            }
            _ => return Err(CatalogError::InvalidResponse),
        }
    }
    Err(CatalogError::InvalidResponse)
}

/// No configDir, account ID, URL or credential is accepted. The caller should
/// use spawn_blocking; failures return the shared, clearly labeled fallback.
pub(crate) fn list_official_models(force_refresh: bool) -> OfficialModelsResult {
    let requested_at = Instant::now();
    let (programs, fingerprint) = executable_candidates();
    let mut cached = cache().lock().unwrap_or_else(|p| p.into_inner());
    if let Some(entry) = cached.as_ref() {
        let ttl = if entry.result.source == "local" {
            SUCCESS_TTL
        } else {
            FAILURE_TTL
        };
        if entry.fingerprint == fingerprint
            && (entry.completed >= requested_at
                || (!force_refresh && entry.completed.elapsed() < ttl))
        {
            return entry.result.clone();
        }
    }
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let mut result = None;
    for program in &programs {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let attempt_deadline = deadline.min(now + ATTEMPT_TIMEOUT);
        if let Ok(catalog) = query_program(program, attempt_deadline) {
            result = Some(catalog);
            break;
        }
    }
    let result = result.unwrap_or_else(|| {
        bundled(Some(if programs.is_empty() {
            "未找到可用的本机 Codex CLI，显示内置参考模型；这不是当前账号的可用权限列表。"
        } else {
            "暂时无法读取本机模型目录，显示内置参考模型；这不是当前账号的可用权限列表。"
        }))
    });
    *cached = Some(CachedCatalog {
        fingerprint,
        completed: Instant::now(),
        result: result.clone(),
    });
    result
}
