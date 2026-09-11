//! Check GitHub Releases for a newer Tensor version and install it in place.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as TokioMutex, Notify};

use crate::http;

const GITHUB_OWNER: &str = "jacobzymet";
const GITHUB_REPO: &str = "tensorUI";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_GITHUB_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 80 * 1024 * 1024;
const RESTART_BIND_ATTEMPTS: u32 = 40;
const RESTART_BIND_DELAY: Duration = Duration::from_millis(50);
const RESTART_SPAWN_DELAY: Duration = Duration::from_millis(350);
const APPLY_RESPONSE_DELAY: Duration = Duration::from_millis(450);
const UPDATE_RESTART_FLAG: &str = "--update-restart";

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub can_install: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_blocked: Option<String>,
    pub release_name: Option<String>,
    pub release_url: Option<String>,
    /// True when this build's version is newer than the latest GitHub Release.
    pub development_ahead: bool,
    pub checked: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub ok: bool,
    pub restarting: bool,
    pub version: String,
}

#[derive(Debug, Clone)]
struct ReleaseOffer {
    status: UpdateStatus,
    asset_name: Option<String>,
    asset_url: Option<String>,
    asset_sha256: Option<String>,
    sums_url: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedCheck {
    at: Instant,
    offer: ReleaseOffer,
}

#[derive(Debug, Clone)]
struct RestartPlan {
    exe: PathBuf,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    detached: bool,
}

static CACHE: OnceLock<Mutex<Option<CachedCheck>>> = OnceLock::new();
static APPLYING: OnceLock<TokioMutex<()>> = OnceLock::new();
static RESTART_PLAN: OnceLock<Mutex<Option<RestartPlan>>> = OnceLock::new();
static RESTART_NOTIFY: OnceLock<Notify> = OnceLock::new();
static RESTART_FLAG: AtomicBool = AtomicBool::new(false);

fn cache() -> &'static Mutex<Option<CachedCheck>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

fn applying() -> &'static TokioMutex<()> {
    APPLYING.get_or_init(|| TokioMutex::new(()))
}

fn restart_plan() -> &'static Mutex<Option<RestartPlan>> {
    RESTART_PLAN.get_or_init(|| Mutex::new(None))
}

fn restart_notify() -> &'static Notify {
    RESTART_NOTIFY.get_or_init(Notify::new)
}

/// Normalize tags like `v1.2.3`, `V1.2.3-beta.1` → comparable core + pre parts.
fn normalize_version(raw: &str) -> String {
    raw.trim().trim_start_matches(['v', 'V']).trim().to_string()
}

fn parse_semver_parts(raw: &str) -> Option<(Vec<u64>, Option<String>)> {
    let normalized = normalize_version(raw);
    if normalized.is_empty() {
        return None;
    }
    let (without_build, build) = normalized
        .split_once('+')
        .map_or((normalized.as_str(), None), |(version, build)| {
            (version, Some(build))
        });
    if build.is_some_and(|build| !valid_dot_identifiers(build, false)) {
        return None;
    }
    let (core, pre) = match without_build.split_once('-') {
        Some((core, rest)) => (core.to_string(), Some(rest.to_string())),
        None => (without_build.to_string(), None),
    };
    if pre
        .as_deref()
        .is_some_and(|pre| !valid_dot_identifiers(pre, true))
    {
        return None;
    }
    let mut parts = Vec::new();
    for piece in core.split('.') {
        if piece.len() > 1 && piece.starts_with('0') {
            return None;
        }
        let n = piece.parse::<u64>().ok()?;
        parts.push(n);
    }
    if parts.is_empty() {
        return None;
    }
    while parts.len() < 3 {
        parts.push(0);
    }
    Some((parts, pre))
}

fn valid_dot_identifiers(raw: &str, reject_numeric_leading_zero: bool) -> bool {
    raw.split('.').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && !(reject_numeric_leading_zero
                && part.len() > 1
                && part.starts_with('0')
                && part.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

fn compare_prerelease(left: &str, right: &str) -> Ordering {
    let left: Vec<&str> = left.split('.').collect();
    let right: Vec<&str> = right.split('.').collect();
    for (a, b) in left.iter().zip(&right) {
        let a_numeric = a.bytes().all(|byte| byte.is_ascii_digit());
        let b_numeric = b.bytes().all(|byte| byte.is_ascii_digit());
        let ordering = match (a_numeric, b_numeric) {
            (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => a.cmp(b),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// True when `latest` is a newer release than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let Some((mut latest_parts, latest_pre)) = parse_semver_parts(latest) else {
        return false;
    };
    let Some((mut current_parts, current_pre)) = parse_semver_parts(current) else {
        return false;
    };
    let max_len = latest_parts.len().max(current_parts.len());
    latest_parts.resize(max_len, 0);
    current_parts.resize(max_len, 0);
    if latest_parts != current_parts {
        return latest_parts > current_parts;
    }
    // Same numeric core: a release without prerelease beats one with.
    match (latest_pre, current_pre) {
        (None, Some(_)) => true,
        (Some(a), Some(b)) => compare_prerelease(&a, &b).is_gt(),
        _ => false,
    }
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

pub fn bind_retry_budget(retry: bool) -> (u32, Duration) {
    if retry {
        (RESTART_BIND_ATTEMPTS, RESTART_BIND_DELAY)
    } else {
        (1, Duration::ZERO)
    }
}

/// Remove leftover `.old` binaries from a previous in-place replace.
pub fn cleanup_previous_install() {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else {
        return;
    };
    if let Some(name) = exe.file_name() {
        let mut old = PathBuf::from(dir);
        old.push(name);
        old.set_extension(match exe.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => format!("{ext}.old"),
            None => "old".to_string(),
        });
        let _ = fs::remove_file(old);
    }
    let _ = fs::remove_file(dir.join(".tensor-update-write-test"));
}

fn status_up_to_date() -> UpdateStatus {
    UpdateStatus {
        current: current_version(),
        latest: None,
        update_available: false,
        can_install: false,
        install_blocked: None,
        release_name: None,
        release_url: None,
        development_ahead: false,
        checked: true,
        error: None,
    }
}

fn github_api_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", concat!("tensor/", env!("CARGO_PKG_VERSION")))
}

fn download_client() -> Result<reqwest::Client, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    reqwest::Client::builder()
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(concat!("tensor/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(16))
        // Keep release archives byte-for-byte. Auto-decompress would corrupt
        // `.zip` / `.tar.gz` when GitHub sends Content-Encoding: gzip.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()
        .map_err(|error| format!("could not build download client: {error}"))
}

pub(crate) fn release_archive_target() -> Option<(&'static str, &'static str)> {
    match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => Some(("x86_64-linux-gnu", "tar.gz")),
        ("linux", "aarch64") => Some(("aarch64-linux-gnu", "tar.gz")),
        ("macos", "aarch64") => Some(("aarch64-apple-darwin", "tar.gz")),
        ("macos", "x86_64") => Some(("x86_64-apple-darwin", "tar.gz")),
        ("windows", "x86_64") => Some(("x86_64-pc-windows-msvc", "zip")),
        ("windows", "aarch64") => Some(("x86_64-pc-windows-msvc", "zip")),
        _ => None,
    }
}

pub(crate) fn looks_like_cargo_build(path: &Path) -> bool {
    let parts: Vec<_> = path.iter().collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == "target" && (pair[1] == "debug" || pair[1] == "release"))
}

fn default_user_binary() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let local = env::var_os("LOCALAPPDATA")?;
        Some(
            PathBuf::from(local)
                .join("tensor")
                .join("bin")
                .join("tensor.exe"),
        )
    }
    #[cfg(not(windows))]
    {
        let home = env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join(".local")
                .join("bin")
                .join("tensor"),
        )
    }
}

fn dir_is_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".tensor-update-write-test");
    match fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn install_destination() -> Result<PathBuf, String> {
    if let Ok(current) = env::current_exe()
        && let Some(dir) = current.parent()
        && !looks_like_cargo_build(&current)
        && dir_is_writable(dir)
    {
        return Ok(current);
    }
    let dest = default_user_binary()
        .ok_or_else(|| "Could not locate a writable Tensor install folder.".to_string())?;
    let dir = dest
        .parent()
        .ok_or_else(|| "Could not locate a writable Tensor install folder.".to_string())?;
    if dir_is_writable(dir) {
        return Ok(dest);
    }
    Err("Tensor cannot write to its install folder. Reinstall with the install script.".to_string())
}

fn install_block_reason() -> Option<String> {
    if release_archive_target().is_none() {
        return Some("No GitHub release is published for this platform.".to_string());
    }
    install_destination().err()
}

fn trusted_github_download_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != "https" || parsed.username() != "" || parsed.password().is_some() {
        return false;
    }
    parsed.host_str() == Some("github.com")
        && parsed
            .path()
            .starts_with(&format!("/{GITHUB_OWNER}/{GITHUB_REPO}/"))
}

pub(crate) fn parse_github_digest(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix("sha256:")?;
    if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

pub(crate) fn pick_release_asset(
    payload: &serde_json::Value,
    target: &str,
) -> (Option<ReleaseAsset>, Option<String>) {
    let Some(assets) = payload.get("assets").and_then(|value| value.as_array()) else {
        return (None, None);
    };
    let tar_suffix = format!("-{target}.tar.gz");
    let zip_suffix = format!("-{target}.zip");
    let mut preferred = None;
    let mut legacy = None;
    let mut sums = None;
    for asset in assets {
        let Some(name) = asset.get("name").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(url) = asset
            .get("browser_download_url")
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        if !trusted_github_download_url(url) {
            continue;
        }
        if name == "SHA256SUMS" {
            sums = Some(url.to_string());
            continue;
        }
        let matches_target = name.ends_with(&tar_suffix) || name.ends_with(&zip_suffix);
        if !matches_target {
            continue;
        }
        let picked = ReleaseAsset {
            name: name.to_string(),
            url: url.to_string(),
            sha256: asset
                .get("digest")
                .and_then(|value| value.as_str())
                .and_then(parse_github_digest),
        };
        if name.starts_with("tensor-") {
            preferred = Some(picked);
        } else if name.starts_with("tensorui-") {
            legacy = Some(picked);
        }
    }
    (preferred.or(legacy), sums)
}

fn decorate_status(mut status: UpdateStatus, has_asset: bool) -> UpdateStatus {
    if !status.update_available {
        status.can_install = false;
        status.install_blocked = None;
        return status;
    }
    if !has_asset {
        status.can_install = false;
        status.install_blocked = Some(format!(
            "No GitHub archive matches this platform ({}).",
            release_archive_target()
                .map(|(target, _)| target)
                .unwrap_or("unknown")
        ));
        return status;
    }
    match install_block_reason() {
        Some(reason) => {
            status.can_install = false;
            status.install_blocked = Some(reason);
        }
        None => {
            status.can_install = true;
            status.install_blocked = None;
        }
    }
    status
}

async fn fetch_latest_offer() -> Result<ReleaseOffer, String> {
    let url = format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");
    let client = http::public_client();
    let response = github_api_headers(client.get(&url))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("could not reach GitHub: {error}"))?;

    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(ReleaseOffer {
            status: status_up_to_date(),
            asset_name: None,
            asset_url: None,
            asset_sha256: None,
            sums_url: None,
        });
    }
    if !status.is_success() {
        let body = http::response_bytes_limited(response, MAX_GITHUB_RESPONSE_BYTES)
            .await
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&body);
        let detail = body.trim();
        if detail.is_empty() {
            return Err(format!("GitHub returned HTTP {status}"));
        }
        return Err(format!("GitHub returned HTTP {status}: {detail}"));
    }

    let bytes = http::response_bytes_limited(response, MAX_GITHUB_RESPONSE_BYTES)
        .await
        .map_err(|error| format!("invalid GitHub response: {error}"))?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid GitHub response: {error}"))?;

    if payload
        .get("draft")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
        || payload
            .get("prerelease")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    {
        return Ok(ReleaseOffer {
            status: status_up_to_date(),
            asset_name: None,
            asset_url: None,
            asset_sha256: None,
            sums_url: None,
        });
    }

    let tag = payload
        .get("tag_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "latest release is missing a tag".to_string())?;
    let release_name = payload
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let release_url = payload
        .get("html_url")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
        });

    let current = current_version();
    let latest = normalize_version(tag);
    let update_available = is_newer(&latest, &current);
    let development_ahead = is_newer(&current, &latest);
    let (asset, sums_url) = release_archive_target()
        .map(|(target, _)| pick_release_asset(&payload, target))
        .unwrap_or((None, None));
    let (asset_name, asset_url, asset_sha256) = match asset {
        Some(asset) => (Some(asset.name), Some(asset.url), asset.sha256),
        None => (None, None, None),
    };
    let status = decorate_status(
        UpdateStatus {
            current,
            latest: Some(latest),
            update_available,
            can_install: false,
            install_blocked: None,
            release_name,
            release_url: Some(release_url),
            development_ahead,
            checked: true,
            error: None,
        },
        asset_url.is_some(),
    );

    Ok(ReleaseOffer {
        status,
        asset_name,
        asset_url,
        asset_sha256,
        sums_url,
    })
}

async fn load_offer(force: bool) -> Result<ReleaseOffer, String> {
    if !force
        && let Ok(guard) = cache().lock()
        && let Some(cached) = guard.as_ref()
        && cached.at.elapsed() < CACHE_TTL
    {
        return Ok(cached.offer.clone());
    }

    let offer = fetch_latest_offer().await?;
    if let Ok(mut guard) = cache().lock() {
        *guard = Some(CachedCheck {
            at: Instant::now(),
            offer: offer.clone(),
        });
    }
    Ok(offer)
}

/// Return a cached or freshly fetched update status.
pub async fn check(force: bool) -> UpdateStatus {
    match load_offer(force).await {
        Ok(offer) => offer.status,
        Err(error) => UpdateStatus {
            current: current_version(),
            latest: None,
            update_available: false,
            can_install: false,
            install_blocked: None,
            release_name: None,
            release_url: Some(format!(
                "https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases"
            )),
            development_ahead: false,
            checked: true,
            error: Some(error),
        },
    }
}

pub(crate) fn checksum_for_asset(sums: &str, asset_name: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            && name == asset_name
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

pub(crate) fn expected_archive_sha256(
    sums_text: Option<&str>,
    asset_name: &str,
    github_digest: Option<&str>,
) -> Result<String, String> {
    let from_sums = sums_text.and_then(|text| checksum_for_asset(text, asset_name));
    if sums_text.is_some() && from_sums.is_none() {
        return Err(format!("release checksum file does not list {asset_name}"));
    }
    match (from_sums, github_digest.map(str::to_string)) {
        (Some(sums), Some(digest)) if sums != digest => {
            Err("release checksum file does not match GitHub's asset digest".to_string())
        }
        (Some(sums), _) => Ok(sums),
        (None, Some(digest)) => Ok(digest),
        (None, None) => {
            Err("latest release does not include a checksum for this archive".to_string())
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn looks_like_native_binary(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    #[cfg(windows)]
    {
        bytes.starts_with(b"MZ")
    }
    #[cfg(target_os = "linux")]
    {
        bytes.starts_with(&[0x7f, b'E', b'L', b'F'])
    }
    #[cfg(target_os = "macos")]
    {
        matches!(
            &bytes[0..4],
            b"\xcf\xfa\xed\xfe"
                | b"\xce\xfa\xed\xfe"
                | b"\xfe\xed\xfa\xcf"
                | b"\xfe\xed\xfa\xce"
                | b"\xca\xfe\xba\xbe"
        )
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        true
    }
}

fn entry_file_name(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

fn is_app_binary_name(name: &str) -> Option<bool> {
    match name {
        "tensor" | "tensor.exe" => Some(true),
        "tensorui" | "tensorui.exe" => Some(false),
        _ => None,
    }
}

fn read_limited(reader: &mut impl Read, max: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|error| format!("could not read archive entry: {error}"))?;
        if read == 0 {
            break;
        }
        if out.len().saturating_add(read) > max {
            return Err("release binary is larger than expected".to_string());
        }
        out.extend_from_slice(&buf[..read]);
    }
    Ok(out)
}

pub(crate) fn extract_app_binary(archive: &[u8], asset_name: &str) -> Result<Vec<u8>, String> {
    if asset_name.ends_with(".zip") {
        extract_from_zip(archive)
    } else if asset_name.ends_with(".tar.gz") || asset_name.ends_with(".tgz") {
        extract_from_tar_gz(archive)
    } else {
        Err("unsupported release archive".to_string())
    }
}

fn extract_from_zip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| format!("could not read zip archive: {error}"))?;
    let mut preferred = None;
    let mut legacy = None;
    for index in 0..zip.len() {
        let file = zip
            .by_index(index)
            .map_err(|error| format!("could not read zip entry: {error}"))?;
        if file.is_dir() {
            continue;
        }
        let Some(name) = entry_file_name(file.name()) else {
            continue;
        };
        match is_app_binary_name(name) {
            Some(true) => preferred = Some(index),
            Some(false) => legacy = Some(index),
            None => {}
        }
    }
    let index = preferred
        .or(legacy)
        .ok_or_else(|| "archive did not contain a tensor executable".to_string())?;
    let mut file = zip
        .by_index(index)
        .map_err(|error| format!("could not read zip entry: {error}"))?;
    let binary = read_limited(&mut file, MAX_BINARY_BYTES)?;
    if !looks_like_native_binary(&binary) {
        return Err("archive executable is not a native Tensor build".to_string());
    }
    Ok(binary)
}

fn extract_from_tar_gz(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut preferred = None;
    let mut legacy = None;
    for entry in archive
        .entries()
        .map_err(|error| format!("could not read tar archive: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("could not read tar entry: {error}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|error| format!("tar entry path: {error}"))?;
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        match is_app_binary_name(name) {
            Some(true) => {
                preferred = Some(read_limited(&mut entry, MAX_BINARY_BYTES)?);
                break;
            }
            Some(false) if legacy.is_none() => {
                legacy = Some(read_limited(&mut entry, MAX_BINARY_BYTES)?);
            }
            Some(false) | None => {}
        }
    }
    let binary = preferred
        .or(legacy)
        .ok_or_else(|| "archive did not contain a tensor executable".to_string())?;
    if !looks_like_native_binary(&binary) {
        return Err("archive executable is not a native Tensor build".to_string());
    }
    Ok(binary)
}

pub(crate) fn replace_executable(dest: &Path, new_bytes: &[u8]) -> Result<(), String> {
    let dir = dest
        .parent()
        .ok_or_else(|| "could not locate the Tensor install folder".to_string())?;
    fs::create_dir_all(dir)
        .map_err(|error| format!("could not create the Tensor install folder: {error}"))?;
    let file_name = dest
        .file_name()
        .ok_or_else(|| "could not locate the Tensor executable".to_string())?;
    let staged = dir.join(format!("{}.new", file_name.to_string_lossy()));
    let backup = {
        let mut path = dir.join(file_name);
        path.set_extension(match dest.extension().and_then(|ext| ext.to_str()) {
            Some(ext) => format!("{ext}.old"),
            None => "old".to_string(),
        });
        path
    };
    fs::write(&staged, new_bytes).map_err(|error| {
        let _ = fs::remove_file(&staged);
        format!("could not write the new Tensor binary: {error}")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)) {
            let _ = fs::remove_file(&staged);
            return Err(format!(
                "could not mark the new Tensor binary executable: {error}"
            ));
        }
        if let Err(error) = fs::rename(&staged, dest) {
            let _ = fs::remove_file(&staged);
            return Err(format!("could not replace Tensor: {error}"));
        }
        let _ = fs::remove_file(&backup);
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = fs::remove_file(&backup);
        if dest.exists()
            && let Err(error) = fs::rename(dest, &backup)
        {
            let _ = fs::remove_file(&staged);
            return Err(format!(
                "could not replace the running app ({error}). Quit other Tensor windows and retry."
            ));
        }
        if let Err(error) = fs::rename(&staged, dest) {
            if backup.exists() {
                let _ = fs::rename(&backup, dest);
            }
            let _ = fs::remove_file(&staged);
            return Err(format!("could not install the new Tensor binary: {error}"));
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = fs::remove_file(&staged);
        Err("self-update is not supported on this platform".to_string())
    }
}

fn restart_args() -> Vec<OsString> {
    env::args_os()
        .skip(1)
        .filter(|arg| arg != UPDATE_RESTART_FLAG)
        .collect()
}

fn arm_restart(exe: PathBuf) {
    let plan = RestartPlan {
        exe,
        args: restart_args(),
        cwd: env::current_dir().ok(),
        detached: crate::desktop::is_desktop_shell(),
    };
    if let Ok(mut guard) = restart_plan().lock() {
        *guard = Some(plan);
    }
}

fn take_restart_plan() -> Option<RestartPlan> {
    restart_plan()
        .lock()
        .ok()
        .and_then(|mut guard| guard.take())
}

pub async fn wait_for_restart_request() {
    if RESTART_FLAG.load(AtomicOrdering::SeqCst) {
        return;
    }
    restart_notify().notified().await;
}

fn request_app_restart() {
    RESTART_FLAG.store(true, AtomicOrdering::SeqCst);
    restart_notify().notify_waiters();
    if crate::desktop::is_desktop_shell() {
        crate::desktop::request_quit();
    }
}

pub fn spawn_restart_if_pending() {
    let Some(plan) = take_restart_plan() else {
        return;
    };
    thread::sleep(RESTART_SPAWN_DELAY);
    let mut cmd = Command::new(&plan.exe);
    cmd.args(&plan.args).arg(UPDATE_RESTART_FLAG);
    if let Some(cwd) = &plan.cwd {
        cmd.current_dir(cwd);
    }
    if plan.detached {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x00000008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
    } else {
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    if let Err(error) = cmd.spawn() {
        eprintln!("could not restart Tensor: {error}");
    }
}

async fn download_bytes(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    if !trusted_github_download_url(url) {
        return Err("refusing to download update from an untrusted URL".to_string());
    }
    let client = download_client()?;
    let response = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .header("Accept-Encoding", "identity")
        .send()
        .await
        .map_err(|error| format!("could not download update: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "could not download update (HTTP {})",
            response.status()
        ));
    }
    http::response_bytes_limited(response, max_bytes)
        .await
        .map_err(|error| format!("could not download update: {error}"))
}

/// Download the latest matching release, replace this executable, and arm a restart.
pub async fn apply() -> Result<ApplyResult, String> {
    let Ok(_guard) = applying().try_lock() else {
        return Err("An update is already installing.".to_string());
    };
    let offer = load_offer(true).await?;
    if !offer.status.update_available {
        return Err("Tensor is already up to date.".to_string());
    }
    if !offer.status.can_install {
        return Err(offer.status.install_blocked.unwrap_or_else(|| {
            "This copy of Tensor cannot install the update automatically.".to_string()
        }));
    }
    let asset_name = offer
        .asset_name
        .ok_or_else(|| "latest release is missing a downloadable archive".to_string())?;
    let asset_url = offer
        .asset_url
        .ok_or_else(|| "latest release is missing a downloadable archive".to_string())?;
    let archive = download_bytes(&asset_url, MAX_ARCHIVE_BYTES).await?;
    let sums_text = if let Some(sums_url) = offer.sums_url {
        let sums = download_bytes(&sums_url, 64 * 1024).await?;
        Some(
            String::from_utf8(sums)
                .map_err(|_| "release checksum file is not valid text".to_string())?,
        )
    } else {
        None
    };
    let expected = expected_archive_sha256(
        sums_text.as_deref(),
        &asset_name,
        offer.asset_sha256.as_deref(),
    )?;
    if sha256_hex(&archive) != expected {
        return Err("release archive checksum mismatch".to_string());
    }
    let binary = extract_app_binary(&archive, &asset_name)?;
    let dest = install_destination()?;
    replace_executable(&dest, &binary)?;
    arm_restart(dest);
    let version = offer.status.latest.unwrap_or_else(current_version);
    Ok(ApplyResult {
        ok: true,
        restarting: true,
        version,
    })
}

pub fn schedule_restart_after_response() {
    tokio::spawn(async {
        tokio::time::sleep(APPLY_RESPONSE_DELAY).await;
        request_app_restart();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn newer_patch_and_minor() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn prerelease_ordering() {
        assert!(is_newer("1.0.0", "1.0.0-beta"));
        assert!(is_newer("1.0.0-rc.2", "1.0.0-rc.1"));
        assert!(is_newer("1.0.0-rc.10", "1.0.0-rc.2"));
        assert!(is_newer("1.0.0-beta.1", "1.0.0-beta"));
        assert!(is_newer("1.0.0-beta", "1.0.0-2"));
        assert!(!is_newer("1.0.0-beta", "1.0.0"));
        assert!(!is_newer("1.0.0-rc.2", "1.0.0-rc.10"));
        assert!(!is_newer("1.0.0-alpha..1", "1.0.0-alpha"));
        assert!(!is_newer("2.0.0-alpha..1", "1.0.0"));
        assert!(!is_newer("1.0.0-alpha.01", "1.0.0-alpha.1"));
        assert!(is_newer("1.0.0-999999999999999999999999999999", "1.0.0-10"));
    }

    #[test]
    fn build_metadata_does_not_affect_precedence() {
        assert!(!is_newer("1.0.0+new-build", "1.0.0+old-build"));
        assert!(is_newer("1.0.1+build.7", "1.0.0+build.9"));
        assert!(!is_newer("2.0.0+", "1.0.0"));
    }

    #[test]
    fn cargo_build_paths() {
        assert!(looks_like_cargo_build(
            &PathBuf::from("home")
                .join("me")
                .join("tensorui2")
                .join("target")
                .join("debug")
                .join("tensor")
        ));
        assert!(looks_like_cargo_build(
            &PathBuf::from("src")
                .join("tensorui2")
                .join("target")
                .join("release")
                .join("tensor.exe")
        ));
        assert!(!looks_like_cargo_build(
            &PathBuf::from("Users")
                .join("me")
                .join("AppData")
                .join("Local")
                .join("tensor")
                .join("bin")
                .join("tensor.exe")
        ));
        assert!(!looks_like_cargo_build(
            &PathBuf::from("home")
                .join("me")
                .join(".local")
                .join("bin")
                .join("tensor")
        ));
    }

    #[test]
    fn checksum_parser_accepts_gnu_and_star_names() {
        let sums = "\
abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd  tensor-0.4.0-x86_64-linux-gnu.tar.gz
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *tensorui-0.3.0-x86_64-pc-windows-msvc.zip
";
        assert_eq!(
            checksum_for_asset(sums, "tensor-0.4.0-x86_64-linux-gnu.tar.gz").as_deref(),
            Some("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd")
        );
        assert_eq!(
            checksum_for_asset(sums, "tensorui-0.3.0-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert!(checksum_for_asset(sums, "missing.tar.gz").is_none());
    }

    #[test]
    fn github_digest_parser() {
        assert_eq!(
            parse_github_digest(
                "sha256:3dae93fc74a6146ea06fe2fea2bb4cd56ed07a4c8a82a0374b0b8c7e9cb305eb"
            )
            .as_deref(),
            Some("3dae93fc74a6146ea06fe2fea2bb4cd56ed07a4c8a82a0374b0b8c7e9cb305eb")
        );
        assert!(parse_github_digest("md5:abc").is_none());
        assert!(parse_github_digest("sha256:not-a-hash").is_none());
    }

    #[test]
    fn checksum_prefers_sums_and_requires_a_hash() {
        let sums = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  tensor-0.4.0-x86_64-linux-gnu.tar.gz
";
        assert_eq!(
            expected_archive_sha256(
                Some(sums),
                "tensor-0.4.0-x86_64-linux-gnu.tar.gz",
                Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            )
            .unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            expected_archive_sha256(
                None,
                "tensorui-0.3.0-x86_64-pc-windows-msvc.zip",
                Some("3dae93fc74a6146ea06fe2fea2bb4cd56ed07a4c8a82a0374b0b8c7e9cb305eb")
            )
            .unwrap(),
            "3dae93fc74a6146ea06fe2fea2bb4cd56ed07a4c8a82a0374b0b8c7e9cb305eb"
        );
        let unused_digest = "aa".repeat(32);
        assert!(
            expected_archive_sha256(Some(sums), "missing.tar.gz", Some(unused_digest.as_str()))
                .is_err()
        );
        assert!(expected_archive_sha256(None, "tensor.tar.gz", None).is_err());
        assert!(
            expected_archive_sha256(
                Some(sums),
                "tensor-0.4.0-x86_64-linux-gnu.tar.gz",
                Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            )
            .unwrap_err()
            .contains("does not match")
        );
    }

    #[test]
    fn picks_tensor_asset_over_legacy_name() {
        let payload = serde_json::json!({
            "assets": [
                {
                    "name": "tensorui-0.3.0-x86_64-linux-gnu.tar.gz",
                    "browser_download_url": "https://github.com/jacobzymet/tensorUI/releases/download/v0.3.0/tensorui-0.3.0-x86_64-linux-gnu.tar.gz"
                },
                {
                    "name": "tensor-0.4.0-x86_64-linux-gnu.tar.gz",
                    "browser_download_url": "https://github.com/jacobzymet/tensorUI/releases/download/v0.4.0/tensor-0.4.0-x86_64-linux-gnu.tar.gz",
                    "digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                {
                    "name": "SHA256SUMS",
                    "browser_download_url": "https://github.com/jacobzymet/tensorUI/releases/download/v0.4.0/SHA256SUMS"
                }
            ]
        });
        let (asset, sums) = pick_release_asset(&payload, "x86_64-linux-gnu");
        let asset = asset.unwrap();
        assert_eq!(asset.name, "tensor-0.4.0-x86_64-linux-gnu.tar.gz");
        assert_eq!(
            asset.sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert!(sums.unwrap().ends_with("/SHA256SUMS"));
    }

    #[test]
    fn rejects_untrusted_asset_host() {
        let payload = serde_json::json!({
            "assets": [{
                "name": "tensor-0.4.0-x86_64-linux-gnu.tar.gz",
                "browser_download_url": "https://evil.example/tensor-0.4.0-x86_64-linux-gnu.tar.gz"
            }]
        });
        let (asset, _) = pick_release_asset(&payload, "x86_64-linux-gnu");
        assert!(asset.is_none());
    }

    #[test]
    #[ignore = "downloads a published GitHub Release archive"]
    fn extracts_live_windows_release_archive() {
        let bytes = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(download_bytes(
                "https://github.com/jacobzymet/tensorUI/releases/download/v0.3.0/tensorui-0.3.0-x86_64-pc-windows-msvc.zip",
                MAX_ARCHIVE_BYTES,
            ))
            .expect("could not download the published Windows archive");
        assert_eq!(
            sha256_hex(&bytes),
            "3dae93fc74a6146ea06fe2fea2bb4cd56ed07a4c8a82a0374b0b8c7e9cb305eb",
            "downloaded bytes must match GitHub's published asset digest"
        );
        let binary =
            extract_app_binary(&bytes, "tensorui-0.3.0-x86_64-pc-windows-msvc.zip").unwrap();
        assert!(
            binary.starts_with(b"MZ"),
            "published Windows archive must contain a PE binary"
        );
        assert!(
            binary.len() > 1_000_000,
            "published Windows binary looks too small: {}",
            binary.len()
        );
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tensor.exe");
        replace_executable(&dest, &binary).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), binary);
    }

    #[test]
    fn extracts_preferred_binary_from_zip() {
        let (preferred_name, preferred, legacy_name, legacy) = if cfg!(windows) {
            (
                "tensor.exe",
                &b"MZ tensor"[..],
                "tensorui.exe",
                &b"MZ legacy"[..],
            )
        } else if cfg!(target_os = "macos") {
            (
                "tensor",
                &b"\xcf\xfa\xed\xfe tensor"[..],
                "tensorui",
                &b"\xcf\xfa\xed\xfe legacy"[..],
            )
        } else {
            (
                "tensor",
                &b"\x7fELF tensor"[..],
                "tensorui",
                &b"\x7fELF legacy"[..],
            )
        };
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("tensor-0.4.0/README.md", options).unwrap();
            zip.write_all(b"docs").unwrap();
            zip.start_file(format!("tensor-0.4.0/{legacy_name}"), options)
                .unwrap();
            zip.write_all(legacy).unwrap();
            zip.start_file(format!("tensor-0.4.0/{preferred_name}"), options)
                .unwrap();
            zip.write_all(preferred).unwrap();
            zip.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let binary = extract_app_binary(&bytes, "tensor-0.4.0-x86_64-pc-windows-msvc.zip").unwrap();
        assert_eq!(binary, preferred);
    }

    #[test]
    fn cargo_test_binary_looks_like_a_cargo_build() {
        let exe = env::current_exe().unwrap();
        assert!(
            looks_like_cargo_build(&exe),
            "test exe should be under target/debug: {}",
            exe.display()
        );
    }

    #[test]
    fn extracts_preferred_binary_from_tar_gz() {
        let (preferred_name, preferred, legacy_name, legacy) = if cfg!(windows) {
            (
                "tensor.exe",
                &b"MZ tensor"[..],
                "tensorui.exe",
                &b"MZ legacy"[..],
            )
        } else if cfg!(target_os = "macos") {
            (
                "tensor",
                &b"\xcf\xfa\xed\xfe tensor"[..],
                "tensorui",
                &b"\xcf\xfa\xed\xfe legacy"[..],
            )
        } else {
            (
                "tensor",
                &b"\x7fELF tensor"[..],
                "tensorui",
                &b"\x7fELF legacy"[..],
            )
        };
        let mut encoded = Cursor::new(Vec::new());
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut encoded, flate2::Compression::default());
            let mut tar = tar::Builder::new(encoder);
            for (name, data) in [
                ("README.md", b"docs".as_slice()),
                (legacy_name, legacy),
                (preferred_name, preferred),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(
                    &mut header,
                    format!("tensor-0.4.0-x86_64-linux-gnu/{name}"),
                    data,
                )
                .unwrap();
            }
            tar.finish().unwrap();
        }
        let bytes = encoded.into_inner();
        let binary = extract_app_binary(&bytes, "tensor-0.4.0-x86_64-linux-gnu.tar.gz").unwrap();
        assert_eq!(binary, preferred);
    }

    #[test]
    fn replace_executable_creates_missing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join(if cfg!(windows) {
            "tensor.exe"
        } else {
            "tensor"
        });
        replace_executable(&dest, b"MZ new").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"MZ new");
    }

    #[test]
    fn replace_executable_overwrites_file() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(if cfg!(windows) {
            "tensor.exe"
        } else {
            "tensor"
        });
        fs::write(&exe, b"old").unwrap();
        replace_executable(&exe, b"MZ new").unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"MZ new");
    }
}
