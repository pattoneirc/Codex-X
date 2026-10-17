//! Read-only balances for saved providers and the current native provider.
//! Request destinations are fixed official URLs, never supplied over IPC.
use crate::error::{CodexxError, Result};
use crate::providers::{self, SavedProvider};
use chrono::{SecondsFormat, Utc};
use reqwest::blocking::Client;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use toml_edit::{DocumentMut, Item};

const SUCCESS_TTL: Duration = Duration::from_secs(300);
const FAILURE_TTL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BalanceAmount {
    pub(crate) currency: String,
    /// Exact base-10 text. Never route financial values through f64.
    pub(crate) amount: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderBalanceResult {
    pub(crate) source: String,
    pub(crate) provider_id: String,
    pub(crate) vendor: Option<&'static str>,
    pub(crate) supported: bool,
    pub(crate) balance_kind: Option<&'static str>,
    pub(crate) balances: Vec<BalanceAmount>,
    pub(crate) checked_at: Option<String>,
    pub(crate) error_code: Option<&'static str>,
}

#[derive(Clone, Copy)]
enum Vendor {
    DeepSeek,
    OpenRouter,
    SiliconFlow,
    Moonshot,
}
impl Vendor {
    fn name(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::SiliconFlow => "siliconflow",
            Self::Moonshot => "moonshot",
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::DeepSeek => "https://api.deepseek.com/user/balance",
            Self::OpenRouter => "https://openrouter.ai/api/v1/key",
            Self::SiliconFlow => "https://api.siliconflow.cn/v1/user/info",
            Self::Moonshot => "https://api.moonshot.cn/v1/users/me/balance",
        }
    }
}

fn vendor_for_provider(provider: &SavedProvider) -> Option<Vendor> {
    // This flag controls Codex authentication, and legacy third-party routes
    // can also set it. Eligibility comes from the official API URL below;
    // official login profiles are never enumerated by this module.
    let url = reqwest::Url::parse(provider.base_url.trim()).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let normal_path = matches!(
        path,
        "" | "/v1" | "/v1/chat/completions" | "/v1/responses" | "/v1/messages"
    );
    match url.host_str()? {
        "api.deepseek.com" if normal_path => Some(Vendor::DeepSeek),
        "api.siliconflow.cn" if normal_path => Some(Vendor::SiliconFlow),
        "api.moonshot.cn" if normal_path => Some(Vendor::Moonshot),
        "openrouter.ai"
            if matches!(
                path,
                "" | "/api/v1"
                    | "/api/v1/chat/completions"
                    | "/api/v1/responses"
                    | "/api/v1/messages"
            ) =>
        {
            Some(Vendor::OpenRouter)
        }
        _ => None,
    }
}

fn environment_value(name: &str) -> std::result::Result<String, &'static str> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("invalid_credential");
    }
    std::env::var(name).map_err(|_| "missing_key")
}

fn credential(
    provider: &SavedProvider,
    detected: bool,
) -> std::result::Result<String, &'static str> {
    let doc = provider
        .toml_config
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            text.parse::<DocumentMut>()
                .map_err(|_| "invalid_configuration")
        })
        .transpose()?;
    let table = doc.as_ref().and_then(|doc| {
        let id = doc
            .get("model_provider")
            .and_then(Item::as_str)
            .unwrap_or(&provider.id);
        doc.get("model_providers")?
            .as_table_like()?
            .get(id)?
            .as_table_like()
    });
    // Only the official API's Bearer credential is relevant. Do not forward
    // arbitrary provider headers (Host, cookies, routing headers, etc.).
    let mut custom_authorization = None;
    if let Some(table) = table {
        for (field, from_env) in [("http_headers", false), ("env_http_headers", true)] {
            if let Some(item) = table.get(field) {
                let headers = item.as_table_like().ok_or("invalid_configuration")?;
                for (name, item) in headers.iter() {
                    if name.eq_ignore_ascii_case("authorization") {
                        if custom_authorization.is_some() {
                            return Err("invalid_credential");
                        }
                        let value = item.as_str().ok_or("invalid_credential")?;
                        custom_authorization = Some(if from_env {
                            environment_value(value)?
                        } else {
                            value.to_string()
                        });
                    }
                }
            }
        }
    }
    // The detected-provider helper may carry a legacy auth.json fallback key.
    // An explicit native env_key must take precedence over that fallback, so an
    // unrelated credential cannot win over the route's own environment key.
    let native_key = if detected && custom_authorization.is_none() {
        if let Some(key) = table
            .and_then(|table| table.get("experimental_bearer_token"))
            .and_then(Item::as_str)
            .filter(|key| !key.trim().is_empty())
        {
            Some(key.to_string())
        } else if let Some(name) = table
            .and_then(|table| table.get("env_key"))
            .and_then(Item::as_str)
        {
            Some(environment_value(name)?)
        } else {
            None
        }
    } else {
        None
    };
    let key = if let Some(authorization) = custom_authorization {
        let (scheme, key) = authorization.split_once(' ').ok_or("invalid_credential")?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err("invalid_credential");
        }
        key.to_string()
    } else if let Some(key) = native_key {
        key
    } else if let Some(key) = provider
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
    {
        key.to_string()
    } else if let Some(key) = table
        .and_then(|table| table.get("experimental_bearer_token"))
        .and_then(Item::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            doc.as_ref()
                .and_then(|doc| providers::experimental_bearer_token_from_doc(doc, None))
        })
    {
        key
    } else if let Some(name) = table
        .and_then(|table| table.get("env_key"))
        .and_then(Item::as_str)
    {
        environment_value(name)?
    } else {
        return Err("missing_key");
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("missing_key");
    }
    if key.len() > 16 * 1024 || key.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("invalid_credential");
    }
    Ok(key.to_string())
}

#[derive(Clone)]
struct BalanceValue {
    kind: Option<&'static str>,
    balances: Vec<BalanceAmount>,
    checked_at: String,
    error: Option<&'static str>,
}
impl BalanceValue {
    fn failure(error: &'static str) -> Self {
        Self {
            kind: None,
            balances: Vec::new(),
            checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            error: Some(error),
        }
    }
}
type CacheEntry = Mutex<Option<(Instant, BalanceValue)>>;
fn cache() -> &'static Mutex<HashMap<String, Arc<CacheEntry>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<CacheEntry>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_balance(vendor: Vendor, key: &str, force: bool, requested_at: Instant) -> BalanceValue {
    let mut digest = Sha256::new();
    digest.update(vendor.endpoint().as_bytes());
    digest.update([0]);
    digest.update(key.as_bytes());
    let identity = format!("{:x}", digest.finalize());
    let slot = {
        let mut entries = cache().lock().unwrap_or_else(|p| p.into_inner());
        if !entries.contains_key(&identity) && entries.len() >= MAX_CACHE_ENTRIES {
            entries.retain(|_, entry| {
                Arc::strong_count(entry) > 1
                    || entry.try_lock().map_or(true, |value| {
                        value
                            .as_ref()
                            .is_some_and(|(at, _)| at.elapsed() < SUCCESS_TTL)
                    })
            });
        }
        if !entries.contains_key(&identity) && entries.len() >= MAX_CACHE_ENTRIES {
            return BalanceValue::failure("busy");
        }
        Arc::clone(
            entries
                .entry(identity)
                .or_insert_with(|| Arc::new(Mutex::new(None))),
        )
    };
    // A lock per credential, held for the bounded request, deduplicates copies
    // and overlapping IPC calls without blocking unrelated accounts.
    let mut entry = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, value)) = entry.as_ref() {
        let ttl = if value.error.is_some() {
            FAILURE_TTL
        } else {
            SUCCESS_TTL
        };
        // A manual refresh bypasses an older TTL entry, but not a request
        // that finished after this IPC call began. Queued copies of the same
        // credential therefore share one refresh rather than serially forcing
        // another HTTP request after acquiring this lock.
        if *at >= requested_at || (!force && at.elapsed() < ttl) {
            return value.clone();
        }
    }
    let value = request_balance(vendor, key).unwrap_or_else(BalanceValue::failure);
    *entry = Some((Instant::now(), value.clone()));
    value
}

fn json_request(
    client: &Client,
    url: &'static str,
    key: &str,
) -> std::result::Result<Value, &'static str> {
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| "invalid_credential")?;
    authorization.set_sensitive(true);
    let response = client
        .get(url)
        .header(ACCEPT, "application/json")
        .header(AUTHORIZATION, authorization)
        .send()
        .map_err(|error| {
            if error.is_timeout() {
                "timeout"
            } else {
                "network_error"
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            300..=399 => "redirect_blocked",
            401 => "authentication_failed",
            403 => "permission_denied",
            429 => "rate_limited",
            _ => "http_error",
        });
    }
    if response
        .content_length()
        .is_some_and(|size| size > MAX_RESPONSE_BYTES)
    {
        return Err("response_too_large");
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "network_error")?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("response_too_large");
    }
    // serde_json's arbitrary_precision feature preserves the original numeric
    // lexeme, including OpenRouter and Moonshot's JSON number balances.
    serde_json::from_slice(&bytes).map_err(|_| "invalid_response")
}

fn amount(currency: &str, value: &Value) -> std::result::Result<BalanceAmount, &'static str> {
    Ok(BalanceAmount {
        currency: currency.to_string(),
        amount: Decimal::from_value(value)?.text(),
    })
}

fn request_balance(vendor: Vendor, key: &str) -> std::result::Result<BalanceValue, &'static str> {
    crate::remote::ensure_crypto_provider();
    // Same fresh-client system/ENV proxy behavior and five-second timeout as
    // remote_client, with credentials additionally confined by no redirects.
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Codex-X")
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "network_error")?;
    let payload = json_request(&client, vendor.endpoint(), key)?;
    let mut kind = "balance";
    let balances = match vendor {
        // https://api-docs.deepseek.com/api/get-user-balance
        Vendor::DeepSeek => {
            let infos = payload
                .get("balance_infos")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty() && items.len() <= 4)
                .ok_or("invalid_response")?;
            let mut balances = Vec::new();
            for info in infos {
                let currency = info
                    .get("currency")
                    .and_then(Value::as_str)
                    .filter(|currency| matches!(*currency, "CNY" | "USD"))
                    .ok_or("invalid_response")?;
                if balances
                    .iter()
                    .any(|item: &BalanceAmount| item.currency == currency)
                {
                    return Err("invalid_response");
                }
                balances.push(amount(
                    currency,
                    info.get("total_balance").ok_or("invalid_response")?,
                )?);
            }
            balances
        }
        // https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key
        // https://openrouter.ai/docs/api/api-reference/credits/get-credits
        Vendor::OpenRouter => {
            let data = payload.get("data").ok_or("invalid_response")?;
            let remaining = data.get("limit_remaining").ok_or("invalid_response")?;
            if !remaining.is_null() {
                kind = "credit_limit";
                vec![amount("USD", remaining)?]
            } else {
                // Management permission may be unavailable to ordinary model
                // keys. Surface its short error code, never request a new key.
                let credits = json_request(&client, "https://openrouter.ai/api/v1/credits", key)?;
                let data = credits.get("data").ok_or("invalid_response")?;
                let total =
                    Decimal::from_value(data.get("total_credits").ok_or("invalid_response")?)?;
                let usage =
                    Decimal::from_value(data.get("total_usage").ok_or("invalid_response")?)?;
                vec![BalanceAmount {
                    currency: "USD".into(),
                    amount: total.subtract(usage)?.text(),
                }]
            }
        }
        // Official CN OpenAPI: github.com/siliconflow/siliconcloud/openapi.yaml
        // CN monetary units: docs.siliconflow.cn/docs/userguide/faqs/rate-limit-and-upgradation
        Vendor::SiliconFlow => {
            if payload.get("status").and_then(Value::as_bool) != Some(true)
                || payload.get("code").and_then(Value::as_u64) != Some(20000)
            {
                return Err("api_error");
            }
            vec![amount(
                "CNY",
                payload
                    .get("data")
                    .and_then(|data| data.get("totalBalance"))
                    .ok_or("invalid_response")?,
            )?]
        }
        // https://platform.moonshot.cn/docs/api/balance (CN keys only)
        Vendor::Moonshot => {
            if payload.get("status").and_then(Value::as_bool) != Some(true)
                || payload.get("code").and_then(Value::as_u64) != Some(0)
            {
                return Err("api_error");
            }
            vec![amount(
                "CNY",
                payload
                    .get("data")
                    .and_then(|data| data.get("available_balance"))
                    .ok_or("invalid_response")?,
            )?]
        }
    };
    Ok(BalanceValue {
        kind: Some(kind),
        balances,
        checked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        error: None,
    })
}

struct Decimal {
    units: i128,
    scale: u32,
}
impl Decimal {
    fn from_value(value: &Value) -> std::result::Result<Self, &'static str> {
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => return Err("invalid_response"),
        };
        Self::parse(&text)
    }
    fn parse(text: &str) -> std::result::Result<Self, &'static str> {
        if text.is_empty() || text.len() > 128 || text.trim() != text {
            return Err("invalid_response");
        }
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |text| (true, text));
        let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
            Some(index) => (
                &unsigned[..index],
                unsigned[index + 1..]
                    .parse::<i32>()
                    .map_err(|_| "invalid_response")?,
            ),
            None => (unsigned, 0),
        };
        if exponent.unsigned_abs() > 36 {
            return Err("invalid_response");
        }
        let (integer, fraction) = mantissa
            .split_once('.')
            .map_or((mantissa, ""), |parts| parts);
        if integer.is_empty()
            || !integer.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
            || (mantissa.contains('.') && fraction.is_empty())
        {
            return Err("invalid_response");
        }
        let digits = format!("{integer}{fraction}");
        let mut units = digits.parse::<i128>().map_err(|_| "invalid_response")?;
        if negative {
            units = -units;
        }
        let scale = fraction.len() as i32 - exponent;
        let scale = if scale < 0 {
            units = units
                .checked_mul(
                    10_i128
                        .checked_pow((-scale) as u32)
                        .ok_or("invalid_response")?,
                )
                .ok_or("invalid_response")?;
            0
        } else if scale > 36 {
            return Err("invalid_response");
        } else {
            scale as u32
        };
        Ok(Self { units, scale }.normalized())
    }
    fn normalized(mut self) -> Self {
        while self.scale > 0 && self.units % 10 == 0 {
            self.units /= 10;
            self.scale -= 1;
        }
        self
    }
    fn subtract(self, other: Self) -> std::result::Result<Self, &'static str> {
        let scale = self.scale.max(other.scale);
        let align = |number: Self| {
            number
                .units
                .checked_mul(10_i128.checked_pow(scale - number.scale)?)
        };
        let units = align(self)
            .and_then(|left| align(other).and_then(|right| left.checked_sub(right)))
            .ok_or("invalid_response")?;
        Ok(Self { units, scale }.normalized())
    }
    fn text(&self) -> String {
        let digits = self.units.unsigned_abs().to_string();
        let sign = if self.units < 0 { "-" } else { "" };
        if self.scale == 0 {
            return format!("{sign}{digits}");
        }
        let scale = self.scale as usize;
        if digits.len() <= scale {
            format!("{sign}0.{}{digits}", "0".repeat(scale - digits.len()))
        } else {
            let at = digits.len() - scale;
            format!("{sign}{}.{}", &digits[..at], &digits[at..])
        }
    }
}

fn provider_result(
    source: &str,
    provider: &SavedProvider,
    force: bool,
    requested_at: Instant,
) -> ProviderBalanceResult {
    let mut result = ProviderBalanceResult {
        source: source.into(),
        provider_id: if source == "detected" {
            format!("detected-{}", provider.id)
        } else {
            provider.id.clone()
        },
        vendor: None,
        supported: false,
        balance_kind: None,
        balances: Vec::new(),
        checked_at: None,
        error_code: None,
    };
    let Some(vendor) = vendor_for_provider(provider) else {
        return result;
    };
    result.vendor = Some(vendor.name());
    result.supported = true;
    let key = match credential(provider, source == "detected") {
        Ok(key) => key,
        Err(code) => {
            result.error_code = Some(code);
            return result;
        }
    };
    let value = cached_balance(vendor, &key, force, requested_at);
    result.balance_kind = value.kind;
    result.balances = value.balances;
    result.checked_at = Some(value.checked_at);
    result.error_code = value.error;
    result
}

fn read_balances_at(
    home: &Path,
    continue_query: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<ProviderBalanceResult>> {
    let mut targets: Vec<_> = providers::list_saved_providers_inner()
        .map_err(|_| CodexxError::Config("读取已保存供应商失败，请稍后重试。".into()))?
        .into_iter()
        .map(|provider| ("local", provider))
        .collect();
    if let Some(provider) = providers::detected_live_custom_provider(home)
        .map_err(|_| CodexxError::Config("读取当前供应商配置失败，请检查配置格式后重试。".into()))?
    {
        targets.push(("detected", provider));
    }
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    // Bound concurrent network work. Duplicate account credentials still share
    // the same cache lock even when represented by multiple cards/sources.
    let workers = targets.len().min(4);
    let batches = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|offset| {
                let targets = &targets;
                scope.spawn(move || {
                    (offset..targets.len())
                        .step_by(workers)
                        .take_while(|_| continue_query())
                        .map(|index| {
                            (
                                index,
                                provider_result(
                                    targets[index].0,
                                    &targets[index].1,
                                    false,
                                    Instant::now(),
                                ),
                            )
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| CodexxError::Config("读取供应商余额失败，请稍后重试。".into()))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let mut results: Vec<_> = batches.into_iter().flatten().collect();
    results.sort_by_key(|(index, _)| *index);
    Ok(results.into_iter().map(|(_, result)| result).collect())
}

#[derive(Clone)]
struct MonitorTarget {
    home: PathBuf,
    scope: String,
    generation: u64,
}

#[derive(Default)]
struct MonitorState {
    target: Option<MonitorTarget>,
    next_refresh: Option<Instant>,
}

struct BalanceMonitor {
    state: Mutex<MonitorState>,
    changed: Condvar,
}

fn refresh_delay(result: &Result<Vec<ProviderBalanceResult>>) -> Duration {
    let Ok(rows) = result else {
        return FAILURE_TTL;
    };
    let now = Utc::now();
    rows.iter()
        .filter(|row| row.supported)
        .map(|row| {
            let ttl = if row.error_code.is_some() {
                FAILURE_TTL
            } else {
                SUCCESS_TTL
            };
            let elapsed = row
                .checked_at
                .as_deref()
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|checked| {
                    now.signed_duration_since(checked.with_timezone(&Utc))
                        .to_std()
                        .unwrap_or(Duration::ZERO)
                });
            elapsed.map_or(ttl, |elapsed| {
                // checkedAt is second-precision; allow one extra second so
                // the credential cache's monotonic TTL has certainly expired.
                // Reading warm/manual-refresh results must not reset a full TTL.
                ttl.saturating_sub(elapsed)
                    .saturating_add(Duration::from_secs(1))
            })
        })
        .min()
        .unwrap_or(SUCCESS_TTL)
}

impl BalanceMonitor {
    fn select(&self, home: PathBuf) -> u64 {
        let scope = crate::paths::normalized_path_scope(&home);
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(target) = &state.target {
            if target.scope == scope {
                return target.generation;
            }
        }
        let generation = state
            .target
            .as_ref()
            .map_or(1, |target| target.generation.wrapping_add(1));
        state.target = Some(MonitorTarget {
            home,
            scope,
            generation,
        });
        // The selecting IPC call performs the first query. Schedule only once
        // it has completed, rather than racing it or counting from its start.
        state.next_refresh = None;
        self.changed.notify_one();
        generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .target
            .as_ref()
            .is_some_and(|target| target.generation == generation)
    }

    fn completed(&self, generation: u64, delay: Duration, native: bool) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state
            .target
            .as_ref()
            .is_some_and(|target| target.generation == generation)
            && (native || state.next_refresh.is_none())
        {
            // Reading a warm cache from the renderer must not continually
            // postpone the independent native deadline.
            state.next_refresh = Some(Instant::now() + delay);
            self.changed.notify_one();
        }
    }

    fn run(&self) {
        loop {
            let target = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                loop {
                    if let (Some(target), Some(deadline)) =
                        (state.target.clone(), state.next_refresh)
                    {
                        let now = Instant::now();
                        if now >= deadline {
                            state.next_refresh = None;
                            break target;
                        }
                        let (next, _) = self
                            .changed
                            .wait_timeout(state, deadline.saturating_duration_since(now))
                            .unwrap_or_else(|p| p.into_inner());
                        state = next;
                    } else {
                        state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
                    }
                }
            };
            // Re-enumerate current saved/native providers every round. A scope
            // change cancels queued old work between cards; active HTTP calls
            // retain their existing bounded timeout and official-only URLs.
            let result = read_balances_at(&target.home, &|| self.is_current(target.generation));
            self.completed(target.generation, refresh_delay(&result), true);
        }
    }
}

fn native_monitor() -> Result<&'static Arc<BalanceMonitor>> {
    static MONITOR: OnceLock<std::result::Result<Arc<BalanceMonitor>, String>> = OnceLock::new();
    MONITOR
        .get_or_init(|| {
            let monitor = Arc::new(BalanceMonitor {
                state: Mutex::new(MonitorState::default()),
                changed: Condvar::new(),
            });
            let worker = Arc::clone(&monitor);
            std::thread::Builder::new()
                .name("provider-balance-monitor".into())
                .spawn(move || worker.run())
                .map_err(|_| "无法启动供应商余额后台刷新，请重启 Codex-X 后重试。".to_string())?;
            Ok(monitor)
        })
        .as_ref()
        .map_err(|message| CodexxError::Config(message.clone()))
}

/// The IPC wrapper should run this blocking function on spawn_blocking.
/// No URL, API key, arbitrary header or official account ID is accepted.
pub(crate) fn get_provider_balances_inner(
    config_dir: Option<String>,
) -> Result<Vec<ProviderBalanceResult>> {
    let home = crate::resolve_codex_dir(config_dir)?;
    let monitor = native_monitor()?;
    let generation = monitor.select(home.clone());
    let result = read_balances_at(&home, &|| monitor.is_current(generation));
    monitor.completed(generation, refresh_delay(&result), false);
    result
}

/// Refresh a single persisted/current card. All connection details are read
/// again from trusted local provider storage, never accepted from the caller.
pub(crate) fn refresh_provider_balance_inner(
    config_dir: Option<String>,
    source: String,
    provider_id: String,
) -> Result<ProviderBalanceResult> {
    // Include provider lookup in the concurrency window: overlapping calls
    // may spend different amounts of time reading the store/current config.
    let requested_at = Instant::now();
    if !matches!(source.as_str(), "local" | "detected") {
        return Err(CodexxError::Config(
            "仅支持刷新已保存或当前检测到的第三方供应商余额。".into(),
        ));
    }
    let home = crate::resolve_codex_dir(config_dir)?;
    let provider = if source == "local" {
        providers::list_saved_providers_inner()
            .map_err(|_| CodexxError::Config("读取已保存供应商失败，请稍后重试。".into()))?
            .into_iter()
            .find(|provider| provider.id == provider_id)
    } else {
        providers::detected_live_custom_provider(&home)
            .map_err(|_| {
                CodexxError::Config("读取当前供应商配置失败，请检查配置格式后重试。".into())
            })?
            .filter(|provider| format!("detected-{}", provider.id) == provider_id)
    }
    .ok_or_else(|| {
        CodexxError::Config("该供应商已不存在或当前配置已改变，请刷新供应商列表。".into())
    })?;
    if vendor_for_provider(&provider).is_none() {
        return Err(CodexxError::Config("该供应商不支持余额刷新。".into()));
    }
    // Deliberately leave the independent five-minute monitor deadline alone.
    Ok(provider_result(&source, &provider, true, requested_at))
}
