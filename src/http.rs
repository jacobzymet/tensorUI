//! Shared reqwest clients. LLM traffic may skip TLS verify for private/lab hosts;
//! public HTTPS (search, page fetch) always verifies.

use std::{net::IpAddr, sync::OnceLock, time::Duration};

use reqwest::{Client, Url};

const APP_UA: &str = concat!("tensorui/", env!("CARGO_PKG_VERSION"));
/// Browser-like UA for public page fetches. Some CDNs return 403/406 to library UAs.
const SEARCH_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

static PUBLIC_CLIENT: OnceLock<Client> = OnceLock::new();

/// True when the URL targets a loopback / private / lab host where self-signed
/// certs are common. Public internet hosts keep certificate verification on.
pub fn url_allows_insecure_tls(url: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unspecified(),
        };
    }
    false
}

fn build_client(timeout: Duration, insecure: bool, user_agent: &str) -> Client {
    let mut builder = Client::builder().timeout(timeout).user_agent(user_agent);
    if insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder.build().expect("reqwest client")
}

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

/// Async client for LLM APIs. Insecure TLS only when `api_base` is private/lab.
pub fn llm_client(api_base: &str, timeout: Duration) -> Client {
    build_client(timeout, url_allows_insecure_tls(api_base), APP_UA)
}

/// Blocking client for provider probes (run on `spawn_blocking`).
pub fn llm_blocking_client(api_base: &str, timeout: Duration) -> reqwest::blocking::Client {
    build_blocking_client(timeout, url_allows_insecure_tls(api_base), APP_UA)
}

/// Async client for public HTTPS (search / page fetch). Always verifies TLS.
pub fn public_client() -> Client {
    PUBLIC_CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(Duration::from_secs(60))
                .user_agent(SEARCH_UA)
                // Cookie jar helps sites that set a device/geo cookie before serving HTML.
                .cookie_store(true)
                // Prefer HTTP/1.1 — some news CDNs fingerprint HTTP/2 stacks and answer 406.
                .http1_only()
                .build()
                .expect("reqwest public client")
        })
        .clone()
}

/// Blocking client for one-off local focus pings.
pub fn app_blocking_client(timeout: Duration) -> reqwest::blocking::Client {
    build_blocking_client(timeout, true, APP_UA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_tls_for_private_hosts_only() {
        assert!(url_allows_insecure_tls("https://127.0.0.1:1234/v1"));
        assert!(url_allows_insecure_tls("http://localhost:8080"));
        assert!(url_allows_insecure_tls("https://192.168.1.10:1234/v1"));
        assert!(url_allows_insecure_tls("https://10.0.0.5/v1"));
        assert!(!url_allows_insecure_tls("https://api.openai.com/v1"));
        assert!(!url_allows_insecure_tls("https://api.anthropic.com"));
    }
}
