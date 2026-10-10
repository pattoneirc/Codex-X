//! Alternate official update transport; manifest selection and package signature
//! verification remain in tauri-plugin-updater. No version badge authorizes an
//! installation and no caller-supplied credentials cross into the API channel.
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::time::Duration;
use tauri::{Manager, Resource, ResourceId, Webview};
use tauri_plugin_updater::{Update, UpdaterExt};

const GITHUB_LATEST: &str = "https://api.github.com/repos/yynxxxxx/Codex-X/releases/latest";
const GITHUB_API_ASSETS: &str = "https://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/";
const METADATA_LIMIT: usize = 2 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OnlineUpdateFailureKind {
    Timeout,
    Network,
    Platform,
    InvalidRelease,
    PublicKey,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OnlineUpdateError {
    pub(crate) kind: OnlineUpdateFailureKind,
    pub(crate) message: &'static str,
}

impl fmt::Display for OnlineUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}
impl std::error::Error for OnlineUpdateError {}

fn failure(kind: OnlineUpdateFailureKind) -> OnlineUpdateError {
    let message = match kind {
        OnlineUpdateFailureKind::Timeout => "The online update request timed out. Retry the online check.",
        OnlineUpdateFailureKind::Network => "The online update service could not be reached because of a network error. Retry the online check.",
        OnlineUpdateFailureKind::Platform => "The online update platform or architecture is unsupported by this release. Retry the online check.",
        OnlineUpdateFailureKind::InvalidRelease => "The online release manifest is invalid or unavailable. Retry the online check.",
        OnlineUpdateFailureKind::PublicKey => "The online update signature or public key is invalid. Retry the online check.",
        OnlineUpdateFailureKind::Unknown => "The online update check did not finish. Retry the online check.",
    };
    OnlineUpdateError { kind, message }
}

fn sdk_failure(error: tauri_plugin_updater::Error) -> OnlineUpdateError {
    use tauri_plugin_updater::Error;
    let kind = match error {
        Error::Reqwest(error) => {
            if error.is_timeout() {
                OnlineUpdateFailureKind::Timeout
            } else {
                OnlineUpdateFailureKind::Network
            }
        }
        Error::Network(_) => OnlineUpdateFailureKind::Network,
        Error::UnsupportedArch
        | Error::UnsupportedOs
        | Error::TargetNotFound(_)
        | Error::TargetsNotFound(_) => OnlineUpdateFailureKind::Platform,
        Error::Minisign(_) | Error::Base64(_) | Error::SignatureUtf8(_) => {
            OnlineUpdateFailureKind::PublicKey
        }
        Error::Serialization(_)
        | Error::Semver(_)
        | Error::ReleaseNotFound
        | Error::UrlParse(_)
        | Error::EmptyEndpoints
        | Error::InsecureTransportProtocol
        | Error::Http(_)
        | Error::InvalidHeaderName(_)
        | Error::InvalidHeaderValue(_)
        | Error::FormatDate => OnlineUpdateFailureKind::InvalidRelease,
        _ => OnlineUpdateFailureKind::Unknown,
    };
    failure(kind)
}

fn request_failure(error: reqwest::Error) -> OnlineUpdateError {
    failure(if error.is_timeout() {
        OnlineUpdateFailureKind::Timeout
    } else {
        OnlineUpdateFailureKind::Network
    })
}

/// The resource table owns and drops both the SDK update and its alternate URLs.
/// Candidate URLs are never held in a global map or returned to the frontend.
/// `download_candidates` excludes the original `update.download_url`.
pub(crate) struct OwnedOnlineUpdate {
    pub(crate) update: Update,
    pub(crate) download_candidates: Vec<Url>,
}
impl Resource for OwnedOnlineUpdate {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OnlineUpdateMetadata {
    rid: ResourceId,
    current_version: String,
    version: String,
    date: Option<String>,
    body: Option<String>,
    raw_json: serde_json::Value,
}

#[derive(Clone)]
struct CheckOptions {
    headers: HeaderMap,
    timeout: Duration,
    proxy: Option<Url>,
    target: Option<String>,
    allow_downgrades: bool,
}

fn metadata_timeout(milliseconds: Option<u64>) -> Duration {
    Duration::from_millis(
        milliseconds
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .clamp(1, MAX_TIMEOUT_MS),
    )
}

impl CheckOptions {
    fn new(
        headers: Option<Vec<(String, String)>>,
        timeout: Option<u64>,
        proxy: Option<String>,
        target: Option<String>,
        allow_downgrades: Option<bool>,
    ) -> Result<Self, OnlineUpdateError> {
        let mut parsed_headers = HeaderMap::new();
        for (name, value) in headers.unwrap_or_default() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
            let value = HeaderValue::from_str(&value)
                .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
            parsed_headers.insert(name, value);
        }
        let proxy = proxy
            .map(|value| {
                Url::parse(&value).map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))
            })
            .transpose()?;
        Ok(Self {
            headers: parsed_headers,
            timeout: metadata_timeout(timeout),
            proxy,
            target,
            allow_downgrades: allow_downgrades.unwrap_or(false),
        })
    }
}

fn api_client(
    timeout: Duration,
    proxy: Option<&Url>,
    no_proxy: bool,
) -> Result<Client, OnlineUpdateError> {
    crate::remote::ensure_crypto_provider();
    let mut builder = Client::builder()
        .http1_only()
        .timeout(timeout.min(Duration::from_millis(MAX_TIMEOUT_MS)))
        .user_agent("Codex-X-online-updater")
        .redirect(reqwest::redirect::Policy::limited(5));
    // A default reqwest client retains the system/environment proxy. Only an
    // explicit SDK no_proxy or explicit proxy overrides it; do not mutate env.
    if no_proxy {
        builder = builder.no_proxy();
    } else if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy.as_str()).map_err(request_failure)?);
    }
    builder.build().map_err(request_failure)
}

async fn bounded_json(client: &Client, url: Url) -> Result<serde_json::Value, OnlineUpdateError> {
    bounded_json_with_accept(client, url, "application/vnd.github+json").await
}

async fn bounded_json_with_accept(
    client: &Client,
    url: Url,
    accept: &'static str,
) -> Result<serde_json::Value, OnlineUpdateError> {
    let mut response = client
        .get(url)
        .header(ACCEPT, accept)
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(request_failure)?;
    if !response.status().is_success() {
        return Err(failure(OnlineUpdateFailureKind::Network));
    }
    if response
        .content_length()
        .is_some_and(|length| length > METADATA_LIMIT as u64)
    {
        return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_failure)? {
        if chunk.len() > METADATA_LIMIT.saturating_sub(body.len()) {
            return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GithubAsset>,
}
#[derive(Debug, Deserialize)]
struct GithubAsset {
    id: u64,
    name: String,
    browser_download_url: String,
    state: String,
}

impl GithubRelease {
    fn parse(value: serde_json::Value) -> Result<Self, OnlineUpdateError> {
        let release: Self = serde_json::from_value(value)
            .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
        if release.draft
            || release.prerelease
            || release.assets.len() > 1024
            || semver::Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .is_err()
        {
            return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
        }
        Ok(release)
    }

    fn manifest_url(&self) -> Result<Url, OnlineUpdateError> {
        let mut matches = self
            .assets
            .iter()
            .filter(|asset| asset.name == "latest.json" && asset.state == "uploaded");
        let asset = matches
            .next()
            .ok_or_else(|| failure(OnlineUpdateFailureKind::InvalidRelease))?;
        if matches.next().is_some() || !asset_matches(self, asset, "latest.json") {
            return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
        }
        api_asset_url(asset.id)
    }
}

fn api_asset_url(id: u64) -> Result<Url, OnlineUpdateError> {
    if id == 0 {
        return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
    }
    Url::parse(&format!("{GITHUB_API_ASSETS}{id}"))
        .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))
}

fn is_api_asset_url(url: &Url) -> bool {
    if url.scheme() != "https"
        || url.host_str() != Some("api.github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(segments) = url.path_segments().map(|parts| parts.collect::<Vec<_>>()) else {
        return false;
    };
    segments.len() == 6
        && segments[..5] == ["repos", "yynxxxxx", "Codex-X", "releases", "assets"]
        && !segments[5].is_empty()
        && segments[5].bytes().all(|byte| byte.is_ascii_digit())
        && segments[5].parse::<u64>().is_ok_and(|id| id > 0)
}

struct DownloadIdentity {
    tag: String,
    name: String,
}

fn download_identity(url: &Url) -> Option<DownloadIdentity> {
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments = url.path_segments()?.collect::<Vec<_>>();
    if segments.len() != 6 || segments[..4] != ["yynxxxxx", "Codex-X", "releases", "download"] {
        return None;
    }
    let tag = percent_encoding::percent_decode_str(segments[4])
        .decode_utf8()
        .ok()?
        .into_owned();
    let name = percent_encoding::percent_decode_str(segments[5])
        .decode_utf8()
        .ok()?
        .into_owned();
    if tag.is_empty()
        || name.is_empty()
        || name.len() > 512
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return None;
    }
    semver::Version::parse(tag.strip_prefix('v').unwrap_or(&tag)).ok()?;
    Some(DownloadIdentity { tag, name })
}

fn asset_matches(release: &GithubRelease, asset: &GithubAsset, expected_name: &str) -> bool {
    Url::parse(&asset.browser_download_url)
        .ok()
        .and_then(|url| download_identity(&url))
        .is_some_and(|identity| {
            asset.id > 0
                && asset.state == "uploaded"
                && asset.name == expected_name
                && identity.tag == release.tag_name
                && identity.name == expected_name
        })
}

fn download_candidates(
    release: &GithubRelease,
    url: &Url,
    version: &str,
) -> Result<Vec<Url>, OnlineUpdateError> {
    let Some(identity) = download_identity(url) else {
        return Ok(Vec::new());
    };
    let version = semver::Version::parse(version.strip_prefix('v').unwrap_or(version))
        .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
    let tag_version =
        semver::Version::parse(identity.tag.strip_prefix('v').unwrap_or(&identity.tag))
            .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
    if identity.tag != release.tag_name || tag_version != version {
        return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
    }
    let mut matches = release
        .assets
        .iter()
        .filter(|asset| asset_matches(release, asset, &identity.name));
    let Some(asset) = matches.next() else {
        return Ok(Vec::new());
    };
    if matches.next().is_some() {
        return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
    }
    Ok(vec![api_asset_url(asset.id)?])
}

trait ManifestUpdate {
    fn download_url(&self) -> &Url;
    fn version(&self) -> &str;
}
impl ManifestUpdate for Update {
    fn download_url(&self) -> &Url {
        &self.download_url
    }
    fn version(&self) -> &str {
        &self.version
    }
}
struct CheckedChannel<T> {
    update: Option<T>,
    candidates: Vec<Url>,
}

// Injectable SDK check seam: tests may use the SDK's RemoteRelease parser with
// local HTTP fixtures without constructing an app or installing any package.
async fn check_with_fallback<T, Check, Checked, Discover, Discovered>(
    mut check: Check,
    discover: Discover,
) -> Result<CheckedChannel<T>, OnlineUpdateError>
where
    T: ManifestUpdate,
    Check: FnMut(Option<Url>) -> Checked,
    Checked: Future<Output = Result<Option<T>, OnlineUpdateError>>,
    Discover: FnOnce() -> Discovered,
    Discovered: Future<Output = Result<GithubRelease, OnlineUpdateError>>,
{
    let primary_error = match check(None).await {
        Ok(update) => {
            return Ok(CheckedChannel {
                update,
                candidates: Vec::new(),
            })
        }
        Err(error) => error,
    };
    let release = discover()
        .await
        .map_err(|error| preferred_error(primary_error.clone(), error))?;
    let endpoint = release.manifest_url()?;
    let update = check(Some(endpoint))
        .await
        .map_err(|error| preferred_error(primary_error, error))?;
    let candidates = update
        .as_ref()
        .map(|update| download_candidates(&release, update.download_url(), update.version()))
        .transpose()?
        .unwrap_or_default();
    Ok(CheckedChannel { update, candidates })
}

fn preferred_error(primary: OnlineUpdateError, fallback: OnlineUpdateError) -> OnlineUpdateError {
    if matches!(
        primary.kind,
        OnlineUpdateFailureKind::Platform | OnlineUpdateFailureKind::PublicKey
    ) {
        primary
    } else {
        fallback
    }
}

async fn sdk_check(
    webview: &Webview,
    options: &CheckOptions,
    endpoint: Option<Url>,
) -> Result<Option<Update>, OnlineUpdateError> {
    crate::remote::ensure_crypto_provider();
    let mut builder = webview
        .updater_builder()
        .timeout(options.timeout)
        .configure_client(|client| client.http1_only());
    if let Some(endpoint) = endpoint {
        if !is_api_asset_url(&endpoint) {
            return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
        }
        // Neither plugin defaults nor caller headers are forwarded cross-origin.
        builder = builder
            .clear_headers()
            .endpoints(vec![endpoint])
            .map_err(sdk_failure)?
            .header("Accept", "application/octet-stream")
            .map_err(sdk_failure)?;
    } else {
        for (name, value) in &options.headers {
            builder = builder
                .header(name.clone(), value.clone())
                .map_err(sdk_failure)?;
        }
    }
    if let Some(proxy) = &options.proxy {
        builder = builder.proxy(proxy.clone());
    }
    if let Some(target) = &options.target {
        builder = builder.target(target.clone());
    }
    if options.allow_downgrades {
        builder = builder.version_comparator(|current, release| current != release.version);
    }
    let mut update = builder
        .build()
        .map_err(sdk_failure)?
        .check()
        .await
        .map_err(sdk_failure)?;
    if let Some(update) = &mut update {
        update.timeout = Some(options.timeout);
    }
    Ok(update)
}

#[tauri::command]
pub(crate) async fn check_online_app_update(
    webview: Webview,
    headers: Option<Vec<(String, String)>>,
    timeout: Option<u64>,
    proxy: Option<String>,
    target: Option<String>,
    allow_downgrades: Option<bool>,
) -> Result<Option<OnlineUpdateMetadata>, OnlineUpdateError> {
    let options = CheckOptions::new(headers, timeout, proxy, target, allow_downgrades)?;
    let checked = check_with_fallback(
        |endpoint| {
            let webview = webview.clone();
            let options = options.clone();
            async move { sdk_check(&webview, &options, endpoint).await }
        },
        || async {
            let client = api_client(options.timeout, options.proxy.as_ref(), false)?;
            GithubRelease::parse(
                bounded_json(&client, Url::parse(GITHUB_LATEST).expect("fixed API URL")).await?,
            )
        },
    )
    .await?;
    let Some(update) = checked.update else {
        return Ok(None);
    };
    let date = update
        .date
        .map(|date| {
            DateTime::<Utc>::from_timestamp(date.unix_timestamp(), date.nanosecond())
                .map(|date| date.to_rfc3339_opts(SecondsFormat::AutoSi, true))
                .ok_or_else(|| failure(OnlineUpdateFailureKind::InvalidRelease))
        })
        .transpose()?;
    let mut metadata = OnlineUpdateMetadata {
        rid: 0,
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        date,
        body: update.body.clone(),
        raw_json: update.raw_json.clone(),
    };
    metadata.rid = webview.resources_table().add(OwnedOnlineUpdate {
        update,
        download_candidates: checked.candidates,
    });
    Ok(Some(metadata))
}

/// Lazily discover an API binary URL only after the original download has a
/// transport failure. A manifest from another repository gets no alternate.
pub(crate) async fn discover_download_candidates(
    update: &Update,
) -> Result<Vec<Url>, OnlineUpdateError> {
    let Some(identity) = download_identity(&update.download_url) else {
        return Ok(Vec::new());
    };
    let mut endpoint = Url::parse("https://api.github.com/repos/yynxxxxx/Codex-X/releases/tags/")
        .expect("fixed API URL");
    endpoint
        .path_segments_mut()
        .expect("hierarchical API URL")
        .pop_if_empty()
        .push(&identity.tag);
    let client = api_client(
        update
            .timeout
            .unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS)),
        update.proxy.as_ref(),
        update.no_proxy,
    )?;
    let release = GithubRelease::parse(bounded_json(&client, endpoint).await?)?;
    download_candidates(&release, &update.download_url, &update.version)
}

/// Clone the SDK object so its signature, public key, TLS policy, configured
/// client callback and proxy remain unchanged. No credentials cross origins.
pub(crate) fn api_download_update(
    update: &Update,
    candidate: &Url,
) -> Result<Update, OnlineUpdateError> {
    if !is_api_asset_url(candidate) {
        return Err(failure(OnlineUpdateFailureKind::InvalidRelease));
    }
    let mut alternate = update.clone();
    alternate.download_url = candidate.clone();
    alternate.headers = api_download_headers();
    Ok(alternate)
}

fn api_download_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
    headers
}

#[cfg(test)]
pub(crate) mod sdk_test_fixture {
    use super::*;
    use base64::Engine;
    use serde_json::json;
    use tauri::test::MockRuntime;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct SignedFixture {
        pub(crate) test_only: bool,
        pub(crate) expected_version: String,
        pub(crate) public_key_base64: String,
        pub(crate) signature_base64: String,
        pub(crate) payload_base64: String,
        pub(crate) sha256: String,
    }
    impl SignedFixture {
        pub(crate) fn payload(&self) -> Vec<u8> {
            base64::engine::general_purpose::STANDARD
                .decode(&self.payload_base64)
                .unwrap()
        }
        pub(crate) fn manifest(&self, package_url: &Url) -> serde_json::Value {
            json!({"version":self.expected_version,"notes":"Signed SDK fixture update","pub_date":"2026-10-01T07:32:25Z", "platforms":{"windows-x86_64":{"url":package_url,"signature":self.signature_base64}}})
        }
    }
    pub(crate) fn signed_fixture() -> SignedFixture {
        let fixture: SignedFixture =
            serde_json::from_str(include_str!("testdata/online-update-fixture.json")).unwrap();
        assert!(fixture.test_only);
        fixture
    }
    pub(crate) fn mock_app() -> tauri::App<MockRuntime> {
        let fixture = signed_fixture();
        let mut context: tauri::Context<MockRuntime> =
            tauri::test::mock_context(tauri::test::noop_assets());
        context.package_info_mut().version = "0.3.21".parse().unwrap();
        context.config_mut().identifier = "com.codexx.test.online-updater".into();
        context.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey":fixture.public_key_base64,"endpoints":[],
                "dangerousInsecureTransportProtocol":true
            }),
        );
        tauri::test::mock_builder()
            .plugin(
                tauri_plugin_updater::Builder::new()
                    .pubkey(fixture.public_key_base64)
                    .build(),
            )
            .build(context)
            .unwrap()
    }
    pub(crate) fn updater_builder(
        app: &tauri::AppHandle<MockRuntime>,
        endpoint: Url,
    ) -> tauri_plugin_updater::UpdaterBuilder {
        crate::remote::ensure_crypto_provider();
        app.updater_builder()
            .target("windows-x86_64")
            .endpoints(vec![endpoint])
            .unwrap()
            .executable_path(
                std::env::temp_dir().join("codex-x-sdk-test/App/Contents/MacOS/codex-x"),
            )
            .timeout(Duration::from_secs(2))
            .no_proxy()
            .configure_client(|client| client.http1_only())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Instant;

    const DOWNLOAD: &str = "https://github.com/yynxxxxx/Codex-X/releases/download/v0.3.24/Codex-X-0.3.24-windows-x64.exe";

    struct FixtureReply {
        status: u16,
        body: Vec<u8>,
        length: bool,
    }
    impl FixtureReply {
        fn json(body: serde_json::Value) -> Self {
            Self {
                status: 200,
                body: body.to_string().into_bytes(),
                length: true,
            }
        }
    }
    struct HttpFixture {
        root: Url,
        requests: Arc<Mutex<Vec<String>>>,
        worker: Option<thread::JoinHandle<()>>,
    }
    impl HttpFixture {
        fn new(replies: Vec<FixtureReply>) -> Self {
            Self::with_responses(|_| replies)
        }
        fn with_responses(make: impl FnOnce(&Url) -> Vec<FixtureReply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let root = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let replies = make(&root);
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                for reply in replies {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                thread::sleep(Duration::from_millis(5))
                            }
                            Err(error) => panic!("local update fixture accept: {error}"),
                        }
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let count = stream.read(&mut buffer).unwrap();
                        assert!(count > 0 && request.len() + count <= 16 * 1024);
                        request.extend_from_slice(&buffer[..count]);
                    }
                    captured
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(request).unwrap());
                    let length = if reply.length {
                        format!("Content-Length: {}\r\n", reply.body.len())
                    } else {
                        String::new()
                    };
                    let mut response = format!("HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\n{length}Connection: close\r\n\r\n", reply.status).into_bytes();
                    response.extend_from_slice(&reply.body);
                    // Oversized-body tests may intentionally close while writing.
                    let _ = stream.write_all(&response);
                }
            });
            Self {
                root,
                requests,
                worker: Some(worker),
            }
        }
        fn client(&self) -> Client {
            crate::remote::ensure_crypto_provider();
            Client::builder()
                .http1_only()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .user_agent("Codex-X-fixture")
                .build()
                .unwrap()
        }
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }
    struct TlsReply {
        path: &'static str,
        status: u16,
        body: Vec<u8>,
    }
    impl TlsReply {
        fn json(path: &'static str, value: serde_json::Value) -> Self {
            Self {
                path,
                status: 200,
                body: value.to_string().into_bytes(),
            }
        }
        fn unavailable(path: &'static str) -> Self {
            Self {
                path,
                status: 503,
                body: b"{}".to_vec(),
            }
        }
        fn binary(path: &'static str, bytes: Vec<u8>) -> Self {
            Self {
                path,
                status: 200,
                body: bytes,
            }
        }
    }
    struct TlsFixture {
        address: std::net::SocketAddr,
        certificate: reqwest::Certificate,
        requests: Arc<Mutex<Vec<String>>>,
        worker: Option<thread::JoinHandle<()>>,
        _temporary_certificate: tempfile::TempDir,
    }
    impl TlsFixture {
        fn new(replies: Vec<TlsReply>) -> Self {
            crate::remote::ensure_crypto_provider();
            let directory = tempfile::Builder::new()
                .prefix("codex-x-sdk-local-tls-")
                .tempdir()
                .unwrap();
            let config = directory.path().join("openssl-fixture.cnf");
            std::fs::write(&config, "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=fixture\n[dn]\nCN=github.com\n[fixture]\nsubjectAltName=DNS:github.com,DNS:api.github.com\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
            let key = directory.path().join("fixture-key.pem");
            let certificate = directory.path().join("fixture-cert.pem");
            let key_der = directory.path().join("fixture-key.der");
            let certificate_der = directory.path().join("fixture-cert.der");
            let openssl = |arguments: Vec<std::ffi::OsString>| {
                let result = std::process::Command::new("openssl")
                    .args(arguments)
                    .env_remove("OPENSSL_CONF")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .expect(
                        "OpenSSL is needed only to create the ephemeral local TLS test certificate",
                    );
                assert!(
                    result.success(),
                    "temporary fixture TLS certificate generation failed"
                );
            };
            openssl(vec![
                "req".into(),
                "-x509".into(),
                "-newkey".into(),
                "rsa:2048".into(),
                "-sha256".into(),
                "-nodes".into(),
                "-days".into(),
                "2".into(),
                "-config".into(),
                config.into_os_string(),
                "-keyout".into(),
                key.as_os_str().into(),
                "-out".into(),
                certificate.as_os_str().into(),
            ]);
            openssl(vec![
                "x509".into(),
                "-in".into(),
                certificate.into_os_string(),
                "-outform".into(),
                "DER".into(),
                "-out".into(),
                certificate_der.as_os_str().into(),
            ]);
            openssl(vec![
                "pkcs8".into(),
                "-topk8".into(),
                "-nocrypt".into(),
                "-in".into(),
                key.into_os_string(),
                "-outform".into(),
                "DER".into(),
                "-out".into(),
                key_der.as_os_str().into(),
            ]);
            let certificate_bytes = std::fs::read(certificate_der).unwrap();
            let client_certificate = reqwest::Certificate::from_der(&certificate_bytes).unwrap();
            let mut server = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(certificate_bytes)],
                    rustls::pki_types::PrivatePkcs8KeyDer::from(std::fs::read(key_der).unwrap())
                        .into(),
                )
                .unwrap();
            server.alpn_protocols = vec![b"http/1.1".to_vec()];
            let server = Arc::new(server);
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                for reply in replies {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let tcp = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && Instant::now() < deadline =>
                            {
                                thread::sleep(Duration::from_millis(5))
                            }
                            Err(error) => panic!("local TLS update fixture accept: {error}"),
                        }
                    };
                    tcp.set_nonblocking(false).unwrap();
                    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let connection = rustls::ServerConnection::new(Arc::clone(&server)).unwrap();
                    let mut stream = rustls::StreamOwned::new(connection, tcp);
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let count = stream
                            .read(&mut buffer)
                            .expect("read local HTTPS fixture request");
                        assert!(count > 0 && request.len() + count <= 16 * 1024);
                        request.extend_from_slice(&buffer[..count]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(
                        request.starts_with(&format!("GET {} HTTP/1.1", reply.path)),
                        "unexpected local HTTPS fixture path"
                    );
                    captured.lock().unwrap().push(request);
                    let mut response = format!("HTTP/1.1 {} Fixture\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reply.status, reply.body.len()).into_bytes();
                    response.extend(reply.body);
                    stream.write_all(&response).unwrap();
                    stream.flush().unwrap();
                }
            });
            Self {
                address,
                certificate: client_certificate,
                requests,
                worker: Some(worker),
                _temporary_certificate: directory,
            }
        }
        fn client(&self) -> Client {
            Client::builder()
                .http1_only()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .add_root_certificate(self.certificate.clone())
                .resolve("github.com", self.address)
                .resolve("api.github.com", self.address)
                .build()
                .unwrap()
        }
        fn updater_builder(
            &self,
            app: &tauri::AppHandle<tauri::test::MockRuntime>,
            endpoint: Url,
            calls: Arc<AtomicUsize>,
        ) -> tauri_plugin_updater::UpdaterBuilder {
            let certificate = self.certificate.clone();
            let address = self.address;
            sdk_test_fixture::updater_builder(app, endpoint).configure_client(move |client| {
                calls.fetch_add(1, Ordering::SeqCst);
                client
                    .http1_only()
                    .no_proxy()
                    .timeout(Duration::from_secs(2))
                    .add_root_certificate(certificate.clone())
                    .resolve("github.com", address)
                    .resolve("api.github.com", address)
            })
        }
    }
    impl Drop for TlsFixture {
        fn drop(&mut self) {
            if let Some(worker) = self.worker.take() {
                let joined = worker.join();
                if !thread::panicking() {
                    assert!(joined.is_ok(), "local TLS fixture worker failed");
                }
            }
        }
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        crate::remote::ensure_crypto_provider();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
    fn index() -> serde_json::Value {
        json!({"tag_name":"v0.3.24","draft":false,"prerelease":false,"assets":[
            {"id":71,"name":"latest.json","state":"uploaded","browser_download_url":"https://github.com/yynxxxxx/Codex-X/releases/download/v0.3.24/latest.json"},
            {"id":72,"name":"Codex-X-0.3.24-windows-x64.exe","state":"uploaded","browser_download_url":DOWNLOAD}
        ]})
    }
    fn manifest() -> serde_json::Value {
        json!({"version":"0.3.24","notes":"Fixture signed-manifest notes","pub_date":"2026-10-01T07:32:25Z","platforms":{"windows-x86_64":{"url":DOWNLOAD,"signature":"fixture-sdk-signature"}}})
    }
    struct FixtureUpdate {
        version: String,
        download_url: Url,
        signature: String,
    }
    impl ManifestUpdate for FixtureUpdate {
        fn version(&self) -> &str {
            &self.version
        }
        fn download_url(&self) -> &Url {
            &self.download_url
        }
    }
    fn selected_manifest(
        value: serde_json::Value,
        target: &str,
    ) -> Result<FixtureUpdate, OnlineUpdateError> {
        // Use the vendor's real parser and target selection; package crypto is
        // intentionally left to Update::download in production, never rewritten.
        let release: tauri_plugin_updater::RemoteRelease = serde_json::from_value(value)
            .map_err(|_| failure(OnlineUpdateFailureKind::InvalidRelease))?;
        Ok(FixtureUpdate {
            version: release.version.to_string(),
            download_url: release.download_url(target).map_err(sdk_failure)?.clone(),
            signature: release.signature(target).map_err(sdk_failure)?.clone(),
        })
    }

    #[test]
    fn failed_primary_check_automatically_reads_real_http_api_manifest_and_returns_a_sdk_selected_update(
    ) {
        let fixture = HttpFixture::new(vec![
            FixtureReply::json(index()),
            FixtureReply::json(manifest()),
        ]);
        let client = fixture.client();
        let attempts = Arc::new(AtomicUsize::new(0));
        let checked = run(check_with_fallback(
            |endpoint| {
                let attempts = Arc::clone(&attempts);
                let client = client.clone();
                let root = fixture.root.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        assert!(endpoint.is_none());
                        return Err(failure(OnlineUpdateFailureKind::Network));
                    }
                    let endpoint = endpoint.unwrap();
                    assert_eq!(
                        endpoint.as_str(),
                        "https://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/71"
                    );
                    assert!(is_api_asset_url(&endpoint));
                    // This seam injects only the SDK transport result. Map its
                    // official endpoint to a loopback fixture; never use the net.
                    let local = root.join(endpoint.path()).unwrap();
                    let value =
                        bounded_json_with_accept(&client, local, "application/octet-stream")
                            .await?;
                    selected_manifest(value, "windows-x86_64").map(Some)
                }
            },
            || async {
                GithubRelease::parse(
                    bounded_json(
                        &client,
                        fixture
                            .root
                            .join("repos/yynxxxxx/Codex-X/releases/latest")
                            .unwrap(),
                    )
                    .await?,
                )
            },
        ))
        .unwrap();
        let update = checked.update.unwrap();
        assert_eq!(update.version, "0.3.24");
        assert_eq!(update.download_url.as_str(), DOWNLOAD);
        assert_eq!(update.signature, "fixture-sdk-signature");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(checked.candidates, vec![api_asset_url(72).unwrap()]);
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].starts_with("GET /repos/yynxxxxx/Codex-X/releases/latest HTTP/1.1"));
        assert!(requests[1].starts_with("GET /repos/yynxxxxx/Codex-X/releases/assets/71 HTTP/1.1"));
        assert!(requests[1]
            .to_ascii_lowercase()
            .contains("accept: application/octet-stream"));
        for request in requests.iter() {
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
        }
    }

    #[test]
    fn successful_primary_checks_and_no_update_never_fetch_an_api_badge() {
        for available in [true, false] {
            let checked = run(check_with_fallback(
                |endpoint| async move {
                    assert!(endpoint.is_none());
                    if available {
                        selected_manifest(manifest(), "windows-x86_64").map(Some)
                    } else {
                        Ok(None)
                    }
                },
                || async { panic!("API discovery must stay lazy when SDK check succeeded") },
            ))
            .unwrap();
            assert_eq!(checked.update.is_some(), available);
            assert!(checked.candidates.is_empty());
        }
    }

    #[test]
    fn version_badges_missing_manifests_and_wrong_platforms_cannot_create_an_update() {
        let mut badge = index();
        badge["assets"] = json!([]);
        let result = run(check_with_fallback::<FixtureUpdate, _, _, _, _>(
            |endpoint| async move {
                assert!(endpoint.is_none());
                Err(failure(OnlineUpdateFailureKind::Network))
            },
            || async { GithubRelease::parse(badge) },
        ));
        assert_eq!(
            result.err().unwrap().kind,
            OnlineUpdateFailureKind::InvalidRelease
        );
        assert_eq!(
            selected_manifest(manifest(), "unpublished-platform")
                .err()
                .unwrap()
                .kind,
            OnlineUpdateFailureKind::Platform
        );
        assert_eq!(
            selected_manifest(json!({"version":"0.3.24"}), "windows-x86_64")
                .err()
                .unwrap()
                .kind,
            OnlineUpdateFailureKind::InvalidRelease
        );
    }

    #[test]
    fn candidates_are_bound_to_the_exact_official_repo_tag_asset_and_sdk_version() {
        let release = GithubRelease::parse(index()).unwrap();
        let original = Url::parse(DOWNLOAD).unwrap();
        assert_eq!(
            download_candidates(&release, &original, "0.3.24").unwrap(),
            vec![api_asset_url(72).unwrap()]
        );
        assert_eq!(
            download_candidates(&release, &original, "0.3.25")
                .err()
                .unwrap()
                .kind,
            OnlineUpdateFailureKind::InvalidRelease
        );
        for url in [
            "https://github.com/other/Codex-X/releases/download/v0.3.24/asset.exe",
            "https://github.com/yynxxxxx/Other/releases/download/v0.3.24/asset.exe",
            "https://github.com.evil.test/yynxxxxx/Codex-X/releases/download/v0.3.24/asset.exe",
            "https://secret@github.com/yynxxxxx/Codex-X/releases/download/v0.3.24/asset.exe",
            "https://github.com/yynxxxxx/Codex-X/releases/download/v0.3.24/asset.exe?token=secret",
            "http://github.com/yynxxxxx/Codex-X/releases/download/v0.3.24/asset.exe",
        ] {
            assert!(download_identity(&Url::parse(url).unwrap()).is_none());
            assert!(
                download_candidates(&release, &Url::parse(url).unwrap(), "0.3.24")
                    .unwrap()
                    .is_empty()
            );
        }
        let mut foreign = index();
        foreign["assets"][0]["browser_download_url"] =
            json!("https://github.com/other/Codex-X/releases/download/v0.3.24/latest.json");
        assert_eq!(
            GithubRelease::parse(foreign)
                .unwrap()
                .manifest_url()
                .err()
                .unwrap()
                .kind,
            OnlineUpdateFailureKind::InvalidRelease
        );
        let mut duplicate = index();
        let duplicate_asset = duplicate["assets"][0].clone();
        duplicate["assets"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_asset);
        assert!(GithubRelease::parse(duplicate)
            .unwrap()
            .manifest_url()
            .is_err());
        let mut mismatched = index();
        mismatched["tag_name"] = json!("v0.3.25");
        assert!(download_candidates(
            &GithubRelease::parse(mismatched).unwrap(),
            &original,
            "0.3.24"
        )
        .is_err());
    }

    #[test]
    fn api_candidates_and_cross_origin_headers_cannot_accept_arbitrary_hosts_or_credentials() {
        assert!(is_api_asset_url(&api_asset_url(72).unwrap()));
        for url in [
            "https://api.github.com/repos/other/Codex-X/releases/assets/72",
            "https://api.github.com/repos/yynxxxxx/Other/releases/assets/72",
            "https://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/0",
            "https://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/72/extra",
            "https://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/72?token=secret",
            "https://user:secret@api.github.com/repos/yynxxxxx/Codex-X/releases/assets/72",
            "https://api.github.com.evil.test/repos/yynxxxxx/Codex-X/releases/assets/72",
            "http://api.github.com/repos/yynxxxxx/Codex-X/releases/assets/72",
        ] {
            assert!(!is_api_asset_url(&Url::parse(url).unwrap()));
        }
        let headers = api_download_headers();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers.get(ACCEPT).unwrap(), "application/octet-stream");
        assert!(!headers.contains_key("authorization"));
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("x-api-key"));
    }

    #[test]
    fn own_http_metadata_reads_reject_oversized_announced_and_streamed_bodies() {
        for length in [true, false] {
            let fixture = HttpFixture::new(vec![FixtureReply {
                status: 200,
                body: json!({"padding": "p".repeat(METADATA_LIMIT)})
                    .to_string()
                    .into_bytes(),
                length,
            }]);
            let error = run(bounded_json(&fixture.client(), fixture.root.clone())).unwrap_err();
            assert_eq!(error.kind, OnlineUpdateFailureKind::InvalidRelease);
        }
    }

    #[test]
    fn fallback_failures_preserve_safe_native_classification_without_urls_or_tokens() {
        let error = run(check_with_fallback::<FixtureUpdate, _, _, _, _>(
            |_endpoint| async { Err(failure(OnlineUpdateFailureKind::PublicKey)) },
            || async { Err(failure(OnlineUpdateFailureKind::Network)) },
        ))
        .err()
        .unwrap();
        assert_eq!(error.kind, OnlineUpdateFailureKind::PublicKey);
        let encoded = serde_json::to_string(&error).unwrap();
        assert!(!encoded.contains("https://"));
        assert!(!encoded.contains("Authorization"));
        assert!(!encoded.contains("token"));
        let fixture = HttpFixture::new(vec![FixtureReply {
            status: 403,
            body: b"private-token".to_vec(),
            length: true,
        }]);
        let error = run(bounded_json(&fixture.client(), fixture.root.clone())).unwrap_err();
        assert_eq!(error.kind, OnlineUpdateFailureKind::Network);
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("private-token"));
    }

    #[test]
    fn check_options_keep_sdk_headers_proxy_target_and_downgrade_settings_with_bounded_timeout() {
        let options = CheckOptions::new(
            Some(vec![
                ("Authorization".into(), "Bearer private-fixture".into()),
                ("Cookie".into(), "fixture=private".into()),
            ]),
            Some(1234),
            Some("http://user:private@127.0.0.1:12345".into()),
            Some("windows-x86_64".into()),
            Some(true),
        )
        .unwrap();
        assert_eq!(options.timeout, Duration::from_millis(1234));
        assert_eq!(
            options.headers.get("authorization").unwrap(),
            "Bearer private-fixture"
        );
        assert_eq!(options.headers.get("cookie").unwrap(), "fixture=private");
        assert_eq!(options.proxy.unwrap().host_str(), Some("127.0.0.1"));
        assert_eq!(options.target.as_deref(), Some("windows-x86_64"));
        assert!(options.allow_downgrades);
        assert_eq!(
            metadata_timeout(None),
            Duration::from_millis(DEFAULT_TIMEOUT_MS)
        );
        assert_eq!(
            metadata_timeout(Some(u64::MAX)),
            Duration::from_millis(MAX_TIMEOUT_MS)
        );
        assert_eq!(
            CheckOptions::new(
                Some(vec![("bad name".into(), "private".into())]),
                None,
                None,
                None,
                None
            )
            .err()
            .unwrap()
            .kind,
            OnlineUpdateFailureKind::InvalidRelease
        );
    }

    #[test]
    fn real_sdk_check_and_download_validate_the_signed_fixture_and_reject_modified_bytes() {
        use sha2::{Digest, Sha256};
        let signed = sdk_test_fixture::signed_fixture();
        let payload = signed.payload();
        assert_eq!(format!("{:x}", Sha256::digest(&payload)), signed.sha256);
        for corrupt in [false, true] {
            let mut served = payload.clone();
            if corrupt {
                served[255] ^= 1;
            }
            let fixture = HttpFixture::with_responses(|root| {
                vec![
                    FixtureReply::json(signed.manifest(&root.join("package.exe").unwrap())),
                    FixtureReply {
                        status: 200,
                        body: served,
                        length: true,
                    },
                ]
            });
            let app = sdk_test_fixture::mock_app();
            let update = run(sdk_test_fixture::updater_builder(
                app.handle(),
                fixture.root.join("manifest.json").unwrap(),
            )
            .build()
            .unwrap()
            .check())
            .unwrap()
            .unwrap();
            assert_eq!(update.current_version, "0.3.21");
            assert_eq!(update.version, signed.expected_version);
            assert_eq!(update.target, "windows-x86_64");
            assert_eq!(update.signature, signed.signature_base64);
            let date = update.date.unwrap();
            assert_eq!(
                DateTime::<Utc>::from_timestamp(date.unix_timestamp(), date.nanosecond())
                    .unwrap()
                    .to_rfc3339_opts(SecondsFormat::AutoSi, true),
                "2026-10-01T07:32:25Z"
            );
            assert_eq!(update.raw_json["version"], "0.3.24");
            let result = run(update.download(|_, _| {}, || {}));
            if corrupt {
                assert!(
                    matches!(result, Err(tauri_plugin_updater::Error::Minisign(_))),
                    "actual SDK Minisign verification must reject a changed packet"
                );
            } else {
                assert_eq!(result.unwrap(), payload);
            }
            assert_eq!(fixture.requests.lock().unwrap().len(), 2);
        }
    }

    #[test]
    fn real_sdk_recovers_manifest_and_package_over_the_official_api_and_rejects_bad_signatures() {
        const PRIMARY: &str = "/yynxxxxx/Codex-X/releases/latest/download/latest.json";
        const ORIGINAL_PACKAGE: &str =
            "/yynxxxxx/Codex-X/releases/download/v0.3.24/Codex-X-0.3.24-windows-x64.exe";
        let signed = sdk_test_fixture::signed_fixture();
        let payload = signed.payload();
        for corrupt in [false, true] {
            let mut served = payload.clone();
            if corrupt {
                served[255] ^= 1;
            }
            let fixture = TlsFixture::new(vec![
                TlsReply::unavailable(PRIMARY),
                TlsReply::json("/repos/yynxxxxx/Codex-X/releases/latest", index()),
                TlsReply::json(
                    "/repos/yynxxxxx/Codex-X/releases/assets/71",
                    signed.manifest(&Url::parse(DOWNLOAD).unwrap()),
                ),
                TlsReply::unavailable(ORIGINAL_PACKAGE),
                TlsReply::binary("/repos/yynxxxxx/Codex-X/releases/assets/72", served),
            ]);
            let app = sdk_test_fixture::mock_app();
            let calls = Arc::new(AtomicUsize::new(0));
            let checked = run(check_with_fallback(
                |endpoint| {
                    let fallback = endpoint.is_some();
                    let endpoint = endpoint.unwrap_or_else(|| Url::parse("https://github.com/yynxxxxx/Codex-X/releases/latest/download/latest.json").unwrap());
                    if fallback { assert!(is_api_asset_url(&endpoint)); }
                    let mut builder = fixture.updater_builder(app.handle(), endpoint, Arc::clone(&calls));
                    if fallback {
                        builder = builder.clear_headers().header("Accept", "application/octet-stream").unwrap();
                    } else {
                        builder = builder.header("Authorization", "Bearer private-fixture").unwrap().header("Cookie", "fixture=private").unwrap();
                    }
                    async move { builder.build().map_err(sdk_failure)?.check().await.map_err(sdk_failure) }
                },
                || async { GithubRelease::parse(bounded_json(&fixture.client(), Url::parse(GITHUB_LATEST).unwrap()).await?) },
            )).unwrap();
            assert_eq!(checked.candidates, vec![api_asset_url(72).unwrap()]);
            let mut update = checked.update.unwrap();
            assert_eq!(update.current_version, "0.3.21");
            assert_eq!(update.version, signed.expected_version);
            assert_eq!(update.target, "windows-x86_64");
            assert_eq!(update.signature, signed.signature_base64);
            update.timeout = Some(Duration::from_secs(2));
            // If a future regression loses the sandbox client callback, this
            // explicit local proxy prevents a real request from leaving tests.
            let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
            update.proxy =
                Some(Url::parse(&format!("http://{}", blocker.local_addr().unwrap())).unwrap());
            update.no_proxy = false;
            update.headers.insert(
                "authorization",
                HeaderValue::from_static("Bearer private-download-fixture"),
            );
            update
                .headers
                .insert("cookie", HeaderValue::from_static("fixture=private"));
            update.headers.insert(
                "x-api-key",
                HeaderValue::from_static("private-download-fixture"),
            );
            let result = run(crate::app_update::verify_download_recovery_for_test(
                &update,
                checked.candidates,
            ));
            if corrupt {
                assert_eq!(
                    serde_json::to_value(result.unwrap_err()).unwrap()["stage"],
                    "verify"
                );
            } else {
                let bytes = result.unwrap();
                assert_eq!(bytes, payload);
                assert_eq!(&bytes[..2], b"MZ");
                assert_eq!(&bytes[64..68], b"PE\0\0");
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                4,
                "the same sandbox HTTP/1+TLS callback must survive the API SDK clone"
            );
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(requests.len(), 5);
            assert!(requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer private-fixture"));
            assert!(requests[3]
                .to_ascii_lowercase()
                .contains("authorization: bearer private-download-fixture"));
            for index in [1, 2, 4] {
                let api = requests[index].to_ascii_lowercase();
                assert!(api.contains("host: api.github.com"));
                assert!(!api.contains("authorization:"));
                assert!(!api.contains("cookie:"));
                assert!(!api.contains("x-api-key:"));
            }
            assert!(requests[2]
                .to_ascii_lowercase()
                .contains("accept: application/octet-stream"));
            assert!(requests[4]
                .to_ascii_lowercase()
                .contains("accept: application/octet-stream"));
        }
    }

    #[test]
    fn real_sdk_api_clone_preserves_signature_client_tls_proxy_and_update_metadata() {
        let signed = sdk_test_fixture::signed_fixture();
        let payload = signed.payload();
        let fixture = TlsFixture::new(vec![
            TlsReply::json(
                "/manifest.json",
                signed.manifest(&Url::parse(DOWNLOAD).unwrap()),
            ),
            TlsReply::binary(
                "/yynxxxxx/Codex-X/releases/download/v0.3.24/Codex-X-0.3.24-windows-x64.exe",
                payload.clone(),
            ),
            TlsReply::binary(
                "/repos/yynxxxxx/Codex-X/releases/assets/72",
                payload.clone(),
            ),
        ]);
        let app = sdk_test_fixture::mock_app();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut update = run(fixture
            .updater_builder(
                app.handle(),
                Url::parse("https://github.com/manifest.json").unwrap(),
                Arc::clone(&calls),
            )
            .build()
            .unwrap()
            .check())
        .unwrap()
        .unwrap();
        update.timeout = Some(Duration::from_secs(2));
        let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
        update.proxy =
            Some(Url::parse(&format!("http://{}", blocker.local_addr().unwrap())).unwrap());
        update.no_proxy = false;
        update.headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer private-fixture"),
        );
        assert_eq!(run(update.download(|_, _| {}, || {})).unwrap(), payload);
        let alternate = api_download_update(&update, &api_asset_url(72).unwrap()).unwrap();
        assert_eq!(alternate.signature, update.signature);
        assert_eq!(alternate.proxy, update.proxy);
        assert_eq!(alternate.no_proxy, update.no_proxy);
        assert_eq!(alternate.timeout, update.timeout);
        assert_eq!(alternate.current_version, update.current_version);
        assert_eq!(alternate.version, update.version);
        assert_eq!(alternate.date, update.date);
        assert_eq!(alternate.body, update.body);
        assert_eq!(alternate.target, update.target);
        assert_eq!(alternate.raw_json, update.raw_json);
        assert!(!alternate.headers.contains_key("authorization"));
        assert_eq!(run(alternate.download(|_, _| {}, || {})).unwrap(), payload);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let requests = fixture.requests.lock().unwrap();
        assert!(!requests[2].to_ascii_lowercase().contains("authorization:"));
        assert!(requests[2].starts_with("GET /repos/yynxxxxx/Codex-X/releases/assets/72 HTTP/1.1"));
    }
}
