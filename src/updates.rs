//! Check GitHub Releases for a newer TensorMI Harness version.

use std::cmp::Ordering;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::http;

const GITHUB_OWNER: &str = "jacobzymet";
const GITHUB_REPO: &str = "tensorUI";
const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub update_available: bool,
    pub release_name: Option<String>,
    pub release_url: Option<String>,
    pub checked: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedCheck {
    at: Instant,
    status: UpdateStatus,
}

static CACHE: OnceLock<Mutex<Option<CachedCheck>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<CachedCheck>> {
    CACHE.get_or_init(|| Mutex::new(None))
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

fn status_up_to_date() -> UpdateStatus {
    UpdateStatus {
        current: current_version(),
        latest: None,
        update_available: false,
        release_name: None,
        release_url: None,
        checked: true,
        error: None,
    }
}

async fn fetch_latest_release() -> Result<UpdateStatus, String> {
    let url = format!("https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest");
    let client = http::public_client();
    let response = client
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(
            "User-Agent",
            concat!("tensorui/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|error| format!("could not reach GitHub: {error}"))?;

    let status = response.status();
    if status.as_u16() == 404 {
        // No published releases yet — not an error, just nothing to update to.
        return Ok(status_up_to_date());
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = body.trim();
        if detail.is_empty() {
            return Err(format!("GitHub returned HTTP {status}"));
        }
        return Err(format!("GitHub returned HTTP {status}: {detail}"));
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("invalid GitHub response: {error}"))?;

    if payload
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || payload
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        return Ok(status_up_to_date());
    }

    let tag = payload
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "latest release is missing a tag".to_string())?;
    let release_name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let release_url = payload
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest")
        });

    let current = current_version();
    let latest = normalize_version(tag);
    let update_available = is_newer(&latest, &current);

    Ok(UpdateStatus {
        current,
        latest: Some(latest),
        update_available,
        release_name,
        release_url: Some(release_url),
        checked: true,
        error: None,
    })
}

/// Return a cached or freshly fetched update status.
pub async fn check(force: bool) -> UpdateStatus {
    if !force
        && let Ok(guard) = cache().lock()
        && let Some(cached) = guard.as_ref()
        && cached.at.elapsed() < CACHE_TTL
    {
        return cached.status.clone();
    }

    let status = match fetch_latest_release().await {
        Ok(status) => status,
        Err(error) => UpdateStatus {
            current: current_version(),
            latest: None,
            update_available: false,
            release_name: None,
            release_url: Some(format!(
                "https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases"
            )),
            checked: true,
            error: Some(error),
        },
    };

    if let Ok(mut guard) = cache().lock() {
        *guard = Some(CachedCheck {
            at: Instant::now(),
            status: status.clone(),
        });
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
