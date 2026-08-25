//! Shared reqwest clients. TLS verification is the default for every connection.

use std::{
    cell::Cell,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::OnceLock,
    time::Duration,
};

use reqwest::{Client, Url};

const APP_UA: &str = concat!("tensorui/", env!("CARGO_PKG_VERSION"));

/// Desktop Chrome identity for public fetches (search / page scrape).
/// Keep the major version in sync across UA + Sec-CH-UA.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/139.0.0.0 Safari/537.36";
const BROWSER_SEC_CH_UA: &str =
    "\"Not;A=Brand\";v=\"99\", \"Google Chrome\";v=\"139\", \"Chromium\";v=\"139\"";
const BROWSER_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,\
    image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";
const BROWSER_ACCEPT_LANG: &str = "en-US,en;q=0.9";

static PUBLIC_CLIENT: OnceLock<Client> = OnceLock::new();

thread_local! {
    static INSECURE_PROVIDER_TLS: Cell<bool> = const { Cell::new(false) };
}

/// Attach Chrome-like navigation headers so page fetches look like a normal document load.
pub fn apply_browser_navigation_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("Accept", BROWSER_ACCEPT)
        .header("Accept-Language", BROWSER_ACCEPT_LANG)
        .header("Upgrade-Insecure-Requests", "1")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1")
        .header("Sec-CH-UA", BROWSER_SEC_CH_UA)
        .header("Sec-CH-UA-Mobile", "?0")
        .header("Sec-CH-UA-Platform", "\"Windows\"")
        .header("Cache-Control", "max-age=0")
}

/// Apply a saved provider's explicit insecure-certificate choice to synchronous
/// probes performed inside `work`.
pub fn with_insecure_provider_tls<T>(allow: bool, work: impl FnOnce() -> T) -> T {
    INSECURE_PROVIDER_TLS.with(|flag| {
        let previous = flag.replace(allow);
        struct Restore<'a>(&'a Cell<bool>, bool);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                self.0.set(self.1);
            }
        }
        let _restore = Restore(flag, previous);
        work()
    })
}

/// Classify private/lab URLs for provider capability probing only. This does
/// not affect certificate verification.
pub fn url_is_private_or_local(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.parse::<IpAddr>().is_ok_and(ip_is_non_public)
}

fn build_client(timeout: Duration, insecure: bool, user_agent: &str) -> Client {
    let mut builder = Client::builder().timeout(timeout).user_agent(user_agent);
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().expect("reqwest client")
}

const LLM_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

static SECURE_BLOCKING_LLM: OnceLock<reqwest::blocking::Client> = OnceLock::new();
static INSECURE_BLOCKING_LLM: OnceLock<reqwest::blocking::Client> = OnceLock::new();

fn build_blocking_client(
    timeout: Duration,
    insecure: bool,
    user_agent: &str,
) -> reqwest::blocking::Client {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(user_agent);
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().expect("reqwest blocking client")
}

/// Async client for LLM APIs. Invalid certificates require explicit opt-in.
pub fn llm_client(timeout: Duration, allow_insecure_tls: bool) -> Client {
    build_client(timeout, allow_insecure_tls, APP_UA)
}

/// Blocking client for provider probes. Reused so TLS sessions stay warm.
/// The insecure-certificate opt-in is scoped by `with_insecure_provider_tls`.
/// Callers should still set a per-request timeout; the pool default is 2s.
pub fn llm_blocking_client(_api_base: &str, _timeout: Duration) -> reqwest::blocking::Client {
    let insecure = INSECURE_PROVIDER_TLS.with(Cell::get);
    let slot = if insecure {
        &INSECURE_BLOCKING_LLM
    } else {
        &SECURE_BLOCKING_LLM
    };
    slot.get_or_init(|| build_blocking_client(LLM_PROBE_TIMEOUT, insecure, APP_UA))
        .clone()
}

/// Async client for public HTTPS (search / page fetch). Always verifies TLS.
pub fn public_client() -> Client {
    PUBLIC_CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(60))
                .user_agent(BROWSER_UA)
                // Cookie jar helps sites that set a device/geo cookie before serving HTML.
                .cookie_store(true)
                // Prefer HTTP/1.1 — some news CDNs fingerprint HTTP/2 stacks and answer 406.
                .http1_only()
                .build()
                .expect("reqwest public client")
        })
        .clone()
}

static SEARCH_CLIENT: OnceLock<Client> = OnceLock::new();
static BING_CLIENT: OnceLock<Client> = OnceLock::new();

fn with_search_tls(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    #[cfg(target_os = "macos")]
    {
        builder.use_native_tls()
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder
    }
}

pub fn search_client() -> Client {
    SEARCH_CLIENT
        .get_or_init(|| {
            with_search_tls(
                Client::builder()
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(8))
                    .user_agent(BROWSER_UA)
                    .cookie_store(true),
            )
            .build()
            .expect("reqwest search client")
        })
        .clone()
}

pub fn bing_client() -> Client {
    BING_CLIENT
        .get_or_init(|| {
            with_search_tls(
                Client::builder()
                    .connect_timeout(Duration::from_secs(2))
                    .timeout(Duration::from_secs(8))
                    .user_agent(BROWSER_UA),
            )
            .build()
            .expect("reqwest bing client")
        })
        .clone()
}

/// Blocking client for one-off local focus pings.
pub fn app_blocking_client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(APP_UA)
        .cookie_store(true)
        .build()
        .expect("reqwest blocking client")
}

fn ipv4_is_non_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240
}

pub fn ip_is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_is_non_public(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return ipv4_is_non_public(v4);
            }
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
        }
    }
}

const MAX_SAFE_REDIRECTS: usize = 10;

/// GET a public HTTP(S) page while preventing SSRF and DNS rebinding. Each
/// redirect is resolved and validated independently, and the connection is
/// pinned to the addresses that passed validation.
pub async fn safe_public_get(url: &str, loose_accept: bool) -> Result<reqwest::Response, String> {
    let mut current = Url::parse(url).map_err(|_| "invalid URL".to_string())?;
    for redirect_count in 0..=MAX_SAFE_REDIRECTS {
        if !matches!(current.scheme(), "http" | "https")
            || !current.username().is_empty()
            || current.password().is_some()
        {
            return Err("URL must be HTTP(S) and must not contain credentials".into());
        }
        let host = current
            .host_str()
            .ok_or_else(|| "URL has no host".to_string())?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
            return Err("fetch_url blocked a local or private destination".into());
        }
        let port = current
            .port_or_known_default()
            .ok_or_else(|| "URL has no valid port".to_string())?;
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| format!("DNS resolution failed: {error}"))?
            .collect();
        if addresses.is_empty() || addresses.iter().any(|addr| ip_is_non_public(addr.ip())) {
            return Err(
                "fetch_url blocked a local, private, link-local, or reserved destination".into(),
            );
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent(BROWSER_UA)
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(&host, &addresses)
            .http1_only()
            .build()
            .map_err(|error| format!("could not build guarded HTTP client: {error}"))?;
        let mut request = apply_browser_navigation_headers(client.get(current.clone()));
        if loose_accept {
            request = request.header("Accept", "*/*");
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        if response.status().is_redirection() {
            if redirect_count == MAX_SAFE_REDIRECTS {
                return Err("too many redirects".into());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "redirect response has no valid Location".to_string())?;
            current = current
                .join(location)
                .map_err(|_| "redirect has an invalid destination".to_string())?;
            continue;
        }
        return Ok(response);
    }
    Err("too many redirects".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_public_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "192.168.1.2",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(ip_is_non_public(ip.parse().unwrap()), "{ip}");
        }
        assert!(!ip_is_non_public("93.184.216.34".parse().unwrap()));
    }
}
