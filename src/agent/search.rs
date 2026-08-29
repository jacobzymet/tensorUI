//! Native web search: Parallel, TinyFish, optional SearXNG, then DuckDuckGo HTML + Lite.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use scraper::{Html, Selector};
use serde_json::{json, Value};
use tokio::time::timeout;

use super::{AgentSkills, SearchHit, WebSearchProvider, WebSearchRecency, WebSearchSafeSearch};
use crate::http;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);
const SEARXNG_TIMEOUT: Duration = Duration::from_secs(8);
const PARALLEL_TIMEOUT: Duration = Duration::from_secs(30);
const PARALLEL_SEARCH_URL: &str = "https://api.parallel.ai/v1/search";
const PARALLEL_MCP_URL: &str = "https://search.parallel.ai/mcp";
const TINYFISH_TIMEOUT: Duration = Duration::from_secs(20);
const TINYFISH_SEARCH_URL: &str = "https://api.search.tinyfish.ai/";
const DDG_LITE: &str = "https://lite.duckduckgo.com/lite/";
const DDG_HTML: &str = "https://html.duckduckgo.com/html/";

pub(super) async fn search_web(
    query: &str,
    skills: &AgentSkills,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let query = collapse_ws(query);
    if query.is_empty() {
        return Err("web_search requires a non-empty \"query\" string.".into());
    }
    let limit = skills.search_result_count();

    match skills.web_search_provider {
        WebSearchProvider::Parallel => search_parallel_only(&query, skills, limit).await,
        WebSearchProvider::Tinyfish => search_tinyfish_only(&query, skills, limit).await,
        WebSearchProvider::Duckduckgo => search_duckduckgo_only(&query, skills, limit).await,
        WebSearchProvider::Searxng => search_searxng_only(&query, skills, limit).await,
        WebSearchProvider::Auto => search_auto(&query, skills, limit).await,
    }
}

async fn search_parallel_only(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let hits = parallel_hits(query, skills, limit).await?;
    if hits.is_empty() {
        return Err(format!("Search returned no pages matching {query:?}."));
    }
    let mode = skills.web_search_parallel_mode.as_str();
    let note = if parallel_api_key(skills).is_some() {
        format!("Parallel · {mode}")
    } else {
        "Parallel · free MCP".into()
    };
    Ok((hits, "Parallel".into(), note))
}

async fn search_tinyfish_only(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let hits = tinyfish_hits(query, skills, limit).await?;
    if hits.is_empty() {
        return Err(format!("Search returned no pages matching {query:?}."));
    }
    Ok((hits, "TinyFish".into(), "TinyFish · Monid".into()))
}

async fn search_duckduckgo_only(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let mut errors = Vec::new();
    let by_source = search_duckduckgo(query, skills, &mut errors).await;
    finalize_sources(by_source, errors, query, limit, false, None, "DuckDuckGo")
}

async fn search_searxng_only(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let mut errors = Vec::new();
    let endpoint = match parse_searxng_endpoint(&skills.web_search_searxng) {
        Ok(Some(url)) => url,
        Ok(None) => {
            return Err(
                "SearXNG provider selected, but no instance URL is set in Agent Capabilities."
                    .into(),
            );
        }
        Err(error) => return Err(format!("searxng: {error}")),
    };
    let mut by_source = Vec::new();
    match searxng_hits(&endpoint, query, skills, limit).await {
        Ok(hits) if !hits.is_empty() => by_source.push(("searxng".into(), hits)),
        Ok(_) => errors.push("searxng: no usable links".into()),
        Err(error) => errors.push(format!("searxng: {error}")),
    }
    finalize_sources(by_source, errors, query, limit, false, None, "SearXNG")
}

async fn search_auto(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<(Vec<SearchHit>, String, String), String> {
    let mut errors: Vec<String> = Vec::new();
    let searx_requested = !skills.web_search_searxng.trim().is_empty();
    let searx_endpoint = match parse_searxng_endpoint(&skills.web_search_searxng) {
        Ok(url) => url,
        Err(error) => {
            errors.push(format!("searxng: {error}"));
            None
        }
    };

    let mut by_source: Vec<(String, Vec<SearchHit>)> = Vec::new();
    if let Some(url) = &searx_endpoint {
        match searxng_hits(url, query, skills, limit).await {
            Ok(hits) if !hits.is_empty() => {
                by_source.push(("searxng".into(), hits));
            }
            Ok(_) => errors.push("searxng: no usable links".into()),
            Err(error) => errors.push(format!("searxng: {error}")),
        }
    }
    let searx_error = errors
        .iter()
        .find(|line| line.starts_with("searxng:"))
        .cloned();

    let mut fell_back = false;
    if by_source.is_empty() {
        fell_back = searx_requested;
        by_source = search_duckduckgo(query, skills, &mut errors).await;
    }

    finalize_sources(
        by_source,
        errors,
        query,
        limit,
        fell_back,
        searx_error,
        if searx_requested {
            "SearXNG"
        } else {
            "DuckDuckGo"
        },
    )
}

fn finalize_sources(
    by_source: Vec<(String, Vec<SearchHit>)>,
    errors: Vec<String>,
    query: &str,
    limit: usize,
    fell_back: bool,
    searx_error: Option<String>,
    preferred_label: &str,
) -> Result<(Vec<SearchHit>, String, String), String> {
    if by_source.is_empty() {
        let detail = if errors.is_empty() {
            "no search endpoint returned usable links".into()
        } else {
            errors.join("; ")
        };
        let hint = if preferred_label == "DuckDuckGo" && !fell_back {
            " DuckDuckGo HTML/Lite may be blocked — set a SearXNG instance URL or switch the search provider to Parallel or TinyFish in Agent Capabilities."
        } else {
            ""
        };
        return Err(format!("Web search failed ({detail}).{hint}"));
    }

    let used_searx = by_source.iter().any(|(name, _)| name == "searxng");
    let engine = if used_searx { "SearXNG" } else { "DuckDuckGo" };
    let note = if fell_back {
        let why = searx_error
            .as_deref()
            .map(strip_searxng_prefix)
            .unwrap_or_else(|| "unavailable".into());
        format!("{engine} · SearXNG failed ({why})")
    } else {
        engine.to_string()
    };
    let merged = merge_sources(by_source, query, limit);
    if merged.is_empty() {
        return Err(format!("Search returned no pages matching {query:?}."));
    }
    Ok((merged, engine.to_string(), note))
}

fn strip_searxng_prefix(line: &str) -> String {
    let stripped = line.strip_prefix("searxng: ").unwrap_or(line).trim();
    const MAX: usize = 90;
    let count = stripped.chars().count();
    if count <= MAX {
        stripped.to_string()
    } else {
        let mut out: String = stripped.chars().take(MAX).collect();
        out.push('…');
        out
    }
}

fn parallel_api_key(skills: &AgentSkills) -> Option<String> {
    let from_settings = skills.web_search_parallel_api_key.trim();
    if !from_settings.is_empty() {
        return Some(from_settings.to_string());
    }
    std::env::var("PARALLEL_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn parallel_hits(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    match parallel_api_key(skills) {
        Some(api_key) => parallel_rest_hits(query, skills, limit, &api_key).await,
        None => parallel_mcp_hits(query, limit).await,
    }
}

async fn parallel_rest_hits(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
    api_key: &str,
) -> Result<Vec<SearchHit>, String> {
    let mut body = json!({
        "objective": query,
        "search_queries": [query],
        "mode": skills.web_search_parallel_mode.as_str(),
        "advanced_settings": {
            "max_results": limit,
        }
    });

    if let Some(location) = parallel_location(&skills.search_region()) {
        body["advanced_settings"]["location"] = json!(location);
    }
    if let Some(after) = recency_after_date(skills.web_search_recency) {
        body["advanced_settings"]["source_policy"] = json!({ "after_date": after });
    }

    let client = http::public_client();
    let request = client
        .post(PARALLEL_SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("x-api-key", api_key)
        .json(&body);
    let response_body = send_parallel_json(request, PARALLEL_TIMEOUT).await?;
    Ok(parse_parallel_json(&response_body, limit))
}

/// Free Parallel Search MCP (`https://search.parallel.ai/mcp`) — no API key required.
async fn parallel_mcp_hits(query: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let client = http::public_client();
    let session_id = parallel_mcp_initialize(&client).await?;

    let _ = client
        .post(PARALLEL_MCP_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .send()
        .await;

    let call_body = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "web_search",
            "arguments": {
                "objective": query,
                "search_queries": [query]
            }
        }
    });
    let request = client
        .post(PARALLEL_MCP_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("Mcp-Session-Id", &session_id)
        .json(&call_body);
    let response = timeout(PARALLEL_TIMEOUT, request.send())
        .await
        .map_err(|_| "Parallel MCP search timed out".to_string())?
        .map_err(|error| format!("Parallel MCP search failed: {error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Parallel MCP response was not text: {error}"))?;

    let _ = client
        .delete(PARALLEL_MCP_URL)
        .header("Mcp-Session-Id", &session_id)
        .send()
        .await;

    if status != 200 {
        return Err(format!(
            "Parallel MCP HTTP {status}: {}",
            truncate_chars(&collapse_ws(&text), 160)
        ));
    }

    let envelope: Value = serde_json::from_str(mcp_json_payload(&text)?)
        .map_err(|error| format!("Invalid Parallel MCP JSON: {error}"))?;
    if let Some(message) = envelope
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Err(format!("Parallel MCP: {}", truncate_chars(message, 160)));
    }
    let search_body = parallel_mcp_search_body(&envelope)?;
    Ok(parse_parallel_json(&search_body, limit))
}

async fn parallel_mcp_initialize(client: &reqwest::Client) -> Result<String, String> {
    let request = client
        .post(PARALLEL_MCP_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "tensorui",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }));
    let response = timeout(PARALLEL_TIMEOUT, request.send())
        .await
        .map_err(|_| "Parallel MCP initialize timed out".to_string())?
        .map_err(|error| format!("Parallel MCP initialize failed: {error}"))?;
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .or_else(|| response.headers().get("Mcp-Session-Id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Parallel MCP did not return Mcp-Session-Id".to_string())?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("Parallel MCP initialize body: {error}"))?;
    if status != 200 {
        return Err(format!(
            "Parallel MCP initialize HTTP {status}: {}",
            truncate_chars(&collapse_ws(&text), 160)
        ));
    }
    Ok(session_id)
}

fn mcp_json_payload(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return Ok(trimmed);
    }
    // Streamable HTTP may return SSE frames.
    for line in trimmed.lines() {
        let line = line.trim();
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim();
            if data.starts_with('{') {
                return Ok(data);
            }
        }
    }
    Err("Parallel MCP response had no JSON payload".into())
}

fn parallel_mcp_search_body(envelope: &Value) -> Result<Value, String> {
    let content = envelope
        .pointer("/result/content")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Parallel MCP result missing content".to_string())?;
    for item in content {
        if item.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        let Some(text) = item.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            if value.get("results").is_some() {
                return Ok(value);
            }
        }
    }
    Err("Parallel MCP result did not include search JSON".into())
}

async fn send_parallel_json(
    request: reqwest::RequestBuilder,
    wait: Duration,
) -> Result<Value, String> {
    let response = timeout(wait, request.send())
        .await
        .map_err(|_| "Parallel search timed out".to_string())?
        .map_err(|error| format!("Parallel search failed: {error}"))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|error| format!("Parallel response was not text: {error}"))?;
    if status != 200 {
        let detail = parallel_error_detail(&body).unwrap_or_else(|| {
            let trimmed = collapse_ws(&body);
            if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {}", truncate_chars(&trimmed, 160))
            }
        });
        return Err(detail);
    }
    serde_json::from_str(&body).map_err(|error| format!("Invalid Parallel JSON: {error}"))
}

fn parallel_error_detail(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(format!("Parallel API: {}", truncate_chars(message, 160)))
}

fn parse_parallel_json(body: &Value, limit: usize) -> Vec<SearchHit> {
    let Some(results) = body.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for item in results {
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if url.is_empty() || reject_result_url(url) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(collapse_ws)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| url.to_string());
        let snippet = item
            .get("excerpts")
            .and_then(|v| v.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.as_str())
                    .map(collapse_ws)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" … ")
            })
            .unwrap_or_default();
        let snippet = truncate_chars(&snippet, 1200);
        if let Some(hit) = clean_hit(title, url.to_string(), snippet) {
            hits.push(hit);
            if hits.len() >= limit {
                break;
            }
        }
    }
    hits
}

fn parallel_location(region: &str) -> Option<String> {
    let region = region.trim().to_ascii_lowercase();
    let country = region.split('-').next().unwrap_or("");
    if country.len() == 2 && country != "wt" && country.bytes().all(|b| b.is_ascii_lowercase()) {
        Some(country.to_string())
    } else {
        None
    }
}

fn recency_after_date(value: WebSearchRecency) -> Option<String> {
    let days = match value {
        WebSearchRecency::Any => return None,
        WebSearchRecency::Day => 1,
        WebSearchRecency::Week => 7,
        WebSearchRecency::Month => 30,
        WebSearchRecency::Year => 365,
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let secs = now.saturating_sub(days * 86_400);
    Some(unix_ymd(secs))
}

fn unix_ymd(secs: u64) -> String {
    // Civil from days (Howard Hinnant) — UTC calendar date.
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn truncate_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn tinyfish_api_key(skills: &AgentSkills) -> Option<String> {
    let from_settings = skills.web_search_tinyfish_api_key.trim();
    if !from_settings.is_empty() {
        return Some(from_settings.to_string());
    }
    std::env::var("TINYFISH_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn tinyfish_hits(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let api_key = tinyfish_api_key(skills).ok_or_else(|| {
        "TinyFish provider selected, but no API key is set. Add one under Agent Capabilities → Web search (free via TinyFish / Monid), or set the TINYFISH_API_KEY environment variable."
            .to_string()
    })?;

    let mut params: Vec<(&str, String)> = vec![("query", query.to_string())];
    if let Some((location, language)) = tinyfish_geo(&skills.search_region()) {
        params.push(("location", location));
        params.push(("language", language));
    }
    if let Some(minutes) = recency_minutes(skills.web_search_recency) {
        params.push(("recency_minutes", minutes.to_string()));
    }

    let client = http::public_client();
    let request = client
        .get(TINYFISH_SEARCH_URL)
        .header("X-API-Key", api_key)
        .query(&params);
    let response_body = send_tinyfish_json(request, TINYFISH_TIMEOUT).await?;
    Ok(parse_tinyfish_json(&response_body, limit))
}

async fn send_tinyfish_json(
    request: reqwest::RequestBuilder,
    wait: Duration,
) -> Result<Value, String> {
    let response = timeout(wait, request.send())
        .await
        .map_err(|_| "TinyFish search timed out".to_string())?
        .map_err(|error| format!("TinyFish search failed: {error}"))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|error| format!("TinyFish response was not text: {error}"))?;
    if status != 200 {
        let detail = tinyfish_error_detail(&body).unwrap_or_else(|| {
            let trimmed = collapse_ws(&body);
            if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {}", truncate_chars(&trimmed, 160))
            }
        });
        return Err(detail);
    }
    serde_json::from_str(&body).map_err(|error| format!("Invalid TinyFish JSON: {error}"))
}

fn tinyfish_error_detail(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let code = value
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    Some(match code {
        Some(code) => format!("TinyFish API ({code}): {}", truncate_chars(message, 140)),
        None => format!("TinyFish API: {}", truncate_chars(message, 160)),
    })
}

fn parse_tinyfish_json(body: &Value, limit: usize) -> Vec<SearchHit> {
    let Some(results) = body.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for item in results {
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if url.is_empty() || reject_result_url(url) {
            continue;
        }
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .map(collapse_ws)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| url.to_string());
        let mut snippet = item
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(collapse_ws)
            .unwrap_or_default();
        if let Some(site) = item
            .get("site_name")
            .and_then(|v| v.as_str())
            .map(collapse_ws)
            .filter(|s| !s.is_empty())
        {
            if snippet.is_empty() {
                snippet = site;
            } else if !snippet.to_ascii_lowercase().contains(&site.to_ascii_lowercase()) {
                snippet = format!("{site} — {snippet}");
            }
        }
        if let Some(date) = item
            .get("date")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            snippet = if snippet.is_empty() {
                date.to_string()
            } else {
                format!("{date} · {snippet}")
            };
        }
        let snippet = truncate_chars(&snippet, 1200);
        if let Some(hit) = clean_hit(title, url.to_string(), snippet) {
            hits.push(hit);
            if hits.len() >= limit {
                break;
            }
        }
    }
    hits
}

fn tinyfish_geo(region: &str) -> Option<(String, String)> {
    let region = region.trim().to_ascii_lowercase();
    let mut parts = region.split('-');
    let country = parts.next().unwrap_or("");
    let lang = parts.next().unwrap_or("");
    if country.len() != 2
        || lang.len() != 2
        || country == "wt"
        || !country.bytes().all(|b| b.is_ascii_lowercase())
        || !lang.bytes().all(|b| b.is_ascii_lowercase())
    {
        return None;
    }
    Some((country.to_ascii_uppercase(), lang.to_string()))
}

fn recency_minutes(value: WebSearchRecency) -> Option<u32> {
    match value {
        WebSearchRecency::Any => None,
        WebSearchRecency::Day => Some(1_440),
        WebSearchRecency::Week => Some(10_080),
        WebSearchRecency::Month => Some(43_200),
        WebSearchRecency::Year => Some(525_600),
    }
}

async fn search_duckduckgo(
    query: &str,
    skills: &AgentSkills,
    errors: &mut Vec<String>,
) -> Vec<(String, Vec<SearchHit>)> {
    let lite_f = ddg_lite_hits(query, skills);
    let html_get_f = ddg_html_get_hits(query, skills);
    let html_post_f = ddg_html_post_hits(query, skills);
    let (lite, html_get, html_post) = tokio::join!(lite_f, html_get_f, html_post_f);
    let mut by_source = Vec::new();
    push_source(&mut by_source, errors, "duckduckgo-lite", lite);
    push_source(&mut by_source, errors, "duckduckgo", html_get);
    push_source(&mut by_source, errors, "duckduckgo-post", html_post);
    by_source
}

fn query_looks_like_news(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words
        .iter()
        .any(|word| matches!(*word, "how" | "tutorial" | "recipe"))
    {
        return false;
    }
    words.iter().any(|word| {
        matches!(
            *word,
            "news" | "headline" | "headlines" | "breaking" | "latest" | "today" | "tonight"
        )
    })
}

pub(super) fn reject_result_url(url: &str) -> bool {
    is_junk_url(url) || is_listing_url(url)
}

fn push_source(
    by_source: &mut Vec<(String, Vec<SearchHit>)>,
    errors: &mut Vec<String>,
    name: &str,
    result: Result<Vec<SearchHit>, String>,
) {
    match result {
        Ok(hits) if !hits.is_empty() => by_source.push((name.into(), hits)),
        Ok(_) => {}
        Err(error) => errors.push(format!("{name}: {error}")),
    }
}

fn accept_language(region: &str) -> String {
    let region = region.trim().to_ascii_lowercase();
    let mut parts = region.split('-');
    let country = parts.next().unwrap_or("");
    let lang = parts.next().unwrap_or("");
    if country.len() == 2
        && lang.len() == 2
        && country != "wt"
        && lang != "wt"
        && country.bytes().all(|b| b.is_ascii_lowercase())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
    {
        let cc = country.to_ascii_uppercase();
        if lang == "en" && cc == "US" {
            return "en-US,en;q=0.9".into();
        }
        return format!("{lang}-{cc},{lang};q=0.9,en;q=0.5");
    }
    "en-US,en;q=0.9".into()
}

fn nav_headers(req: reqwest::RequestBuilder, skills: &AgentSkills) -> reqwest::RequestBuilder {
    http::apply_browser_navigation_headers_lang(req, &accept_language(&skills.search_region()))
}

fn ddg_params(query: &str, skills: &AgentSkills) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("q", query.to_string()),
        ("kl", skills.search_region()),
        ("kp", safesearch_kp(skills.web_search_safesearch).into()),
    ];
    if let Some(df) = recency_df(skills.web_search_recency) {
        params.push(("df", df.into()));
    }
    params
}

fn safesearch_kp(value: WebSearchSafeSearch) -> &'static str {
    match value {
        WebSearchSafeSearch::On => "1",
        WebSearchSafeSearch::Moderate => "-1",
        WebSearchSafeSearch::Off => "-2",
    }
}

fn recency_df(value: WebSearchRecency) -> Option<&'static str> {
    match value {
        WebSearchRecency::Any => None,
        WebSearchRecency::Day => Some("d"),
        WebSearchRecency::Week => Some("w"),
        WebSearchRecency::Month => Some("m"),
        WebSearchRecency::Year => Some("y"),
    }
}

fn searxng_safesearch(value: WebSearchSafeSearch) -> &'static str {
    match value {
        WebSearchSafeSearch::On => "2",
        WebSearchSafeSearch::Moderate => "1",
        WebSearchSafeSearch::Off => "0",
    }
}

fn searxng_time_range(value: WebSearchRecency) -> Option<&'static str> {
    match value {
        WebSearchRecency::Any => None,
        WebSearchRecency::Day => Some("day"),
        WebSearchRecency::Week => Some("week"),
        WebSearchRecency::Month => Some("month"),
        WebSearchRecency::Year => Some("year"),
    }
}

async fn ddg_lite_hits(query: &str, skills: &AgentSkills) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let params = ddg_params(query, skills);
    let query_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let request = nav_headers(
        client
            .get(DDG_LITE)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://lite.duckduckgo.com/")
            .query(&query_refs),
        skills,
    );
    let html = send_html(request, true, REQUEST_TIMEOUT).await?;
    Ok(parse_ddg_lite_html(&html))
}

async fn ddg_html_post_hits(query: &str, skills: &AgentSkills) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let mut form = ddg_params(query, skills);
    form.push(("b", String::new()));
    let post = nav_headers(
        client
            .post(DDG_HTML)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://html.duckduckgo.com/html/")
            .form(&form),
        skills,
    );
    let html = send_html(post, true, REQUEST_TIMEOUT).await?;
    Ok(parse_ddg_html(&html))
}

async fn ddg_html_get_hits(query: &str, skills: &AgentSkills) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let params = ddg_params(query, skills);
    let query_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let get = nav_headers(
        client
            .get(DDG_HTML)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://html.duckduckgo.com/html/")
            .query(&query_refs),
        skills,
    );
    let html = send_html(get, true, REQUEST_TIMEOUT).await?;
    Ok(parse_ddg_html(&html))
}

fn parse_searxng_endpoint(raw: &str) -> Result<Option<reqwest::Url>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > 300 {
        return Err("SearXNG URL is too long.".into());
    }
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else if raw.starts_with("localhost")
        || raw.starts_with("127.")
        || raw.starts_with("[::1]")
        || raw.starts_with("[::]")
    {
        format!("http://{raw}")
    } else {
        format!("https://{raw}")
    };
    let mut url = reqwest::Url::parse(&candidate)
        .map_err(|_| "SearXNG URL is invalid. Use http(s)://host[:port][/path].".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("SearXNG URL must be HTTP or HTTPS.".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("SearXNG URL must not contain credentials.".into());
    }
    if url.host_str().is_none() {
        return Err("SearXNG URL has no host.".into());
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.is_empty() {
        path = "/search".into();
    } else if !path.ends_with("/search") {
        path.push_str("/search");
    }
    url.set_path(&path);
    Ok(Some(url))
}

async fn searxng_hits(
    endpoint: &reqwest::Url,
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let (json, html) = tokio::join!(
        searxng_json_hits(endpoint, query, skills, limit),
        searxng_html_hits(endpoint, query, skills),
    );
    match (json, html) {
        (Ok(hits), _) if !hits.is_empty() => Ok(hits),
        (_, Ok(hits)) if !hits.is_empty() => Ok(hits),
        (Err(json_err), Err(html_err)) if json_err == html_err => Err(json_err),
        (Err(json_err), Err(html_err)) => Err(format!("{json_err}; html: {html_err}")),
        (Ok(_), Err(html_err)) => Err(html_err),
        (Err(json_err), Ok(_)) => Err(json_err),
        (Ok(_), Ok(_)) => Err("no usable links".into()),
    }
}

fn searxng_query_params(
    query: &str,
    skills: &AgentSkills,
    json: bool,
) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("q", query.to_string()),
        ("categories", "general".into()),
        ("language", searxng_language(&skills.search_region()).into()),
        (
            "safesearch",
            searxng_safesearch(skills.web_search_safesearch).into(),
        ),
        ("pageno", "1".into()),
    ];
    if json {
        params.push(("format", "json".into()));
    }
    if let Some(range) = searxng_time_range(skills.web_search_recency) {
        params.push(("time_range", range.into()));
    }
    params
}

fn searxng_language(region: &str) -> String {
    let region = region.trim().to_ascii_lowercase();
    let mut parts = region.split('-');
    let country = parts.next().unwrap_or("");
    let lang = parts.next().unwrap_or("");
    if country == "wt" || lang == "wt" || lang.len() != 2 {
        return "en-US".into();
    }
    if country.len() == 2 && country.bytes().all(|b| b.is_ascii_lowercase()) {
        return format!("{lang}-{}", country.to_ascii_uppercase());
    }
    lang.to_string()
}

fn searxng_request(
    endpoint: &reqwest::Url,
    query: &str,
    skills: &AgentSkills,
    json: bool,
) -> reqwest::RequestBuilder {
    let params = searxng_query_params(query, skills, json);
    let query_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let host = endpoint.host_str().unwrap_or("");
    let origin = match endpoint.port() {
        Some(port) => format!("{}://{host}:{port}", endpoint.scheme()),
        None => format!("{}://{host}", endpoint.scheme()),
    };
    let client = http::search_client();
    let req = client
        .get(endpoint.as_str())
        .timeout(SEARXNG_TIMEOUT)
        .header("Referer", format!("{origin}/"))
        .query(&query_refs);
    nav_headers(req, skills)
}

async fn searxng_json_hits(
    endpoint: &reqwest::Url,
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let body = send_json(searxng_request(endpoint, query, skills, true), SEARXNG_TIMEOUT).await?;
    Ok(parse_searxng_json(&body, limit))
}

async fn searxng_html_hits(
    endpoint: &reqwest::Url,
    query: &str,
    skills: &AgentSkills,
) -> Result<Vec<SearchHit>, String> {
    let html = send_html(searxng_request(endpoint, query, skills, false), false, SEARXNG_TIMEOUT).await?;
    Ok(parse_searxng_html(&html))
}

fn parse_searxng_json(body: &Value, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    if let Some(answers) = body.get("answers").and_then(Value::as_array) {
        for answer in answers.iter().take(2) {
            let (title, url, snippet) = match answer {
                Value::String(text) => ("Direct answer".to_string(), String::new(), text.clone()),
                Value::Object(obj) => (
                    obj.get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("Direct answer")
                        .to_string(),
                    obj.get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    obj.get("answer")
                        .and_then(Value::as_str)
                        .or_else(|| obj.get("content").and_then(Value::as_str))
                        .unwrap_or("")
                        .to_string(),
                ),
                _ => continue,
            };
            if let Some(hit) = featured_hit(
                collapse_ws(&title),
                decode_result_href(&url),
                collapse_ws(&snippet),
            ) {
                hits.push(hit);
            }
        }
    }
    if let Some(results) = body.get("results").and_then(Value::as_array) {
        for item in results.iter().take(limit.max(8)) {
            let title = item.get("title").and_then(Value::as_str).unwrap_or("");
            let url = item.get("url").and_then(Value::as_str).unwrap_or("");
            let snippet = item
                .get("content")
                .and_then(Value::as_str)
                .or_else(|| item.get("snippet").and_then(Value::as_str))
                .unwrap_or("");
            if let Some(hit) = clean_hit(
                collapse_ws(title),
                decode_result_href(url),
                collapse_ws(&strip_tags(snippet)),
            ) {
                hits.push(hit);
            }
        }
    }
    hits
}

fn parse_searxng_html(html: &str) -> Vec<SearchHit> {
    let mut hits = parse_searxng_scraper(html);
    if hits.is_empty() {
        hits = parse_ddg_html(html);
    }
    hits
}

fn parse_searxng_scraper(html: &str) -> Vec<SearchHit> {
    let Ok(article_sel) = Selector::parse("article.result, article[class*='result']") else {
        return Vec::new();
    };
    let Ok(link_sel) = Selector::parse("h3 a, h4 a, a.url_wrapper, a[href]") else {
        return Vec::new();
    };
    let snippet_sel = Selector::parse("p.content, p.result-content, .content").ok();
    let document = Html::parse_document(html);
    let mut hits = Vec::new();
    for item in document.select(&article_sel) {
        let link = item.select(&link_sel).find(|a| {
            let href = a.value().attr("href").unwrap_or("");
            href.starts_with("http://") || href.starts_with("https://")
        });
        let Some(link) = link else {
            continue;
        };
        let href = link.value().attr("href").unwrap_or("");
        let title = collapse_ws(&link.text().collect::<String>());
        let snippet = snippet_sel
            .as_ref()
            .and_then(|sel| item.select(sel).next())
            .map(|p| collapse_ws(&p.text().collect::<String>()))
            .unwrap_or_default();
        if let Some(hit) = clean_hit(title, decode_result_href(href), snippet) {
            hits.push(hit);
        }
    }
    hits
}

fn query_is_mostly_latin(query: &str) -> bool {
    let letters: Vec<char> = query.chars().filter(|ch| ch.is_alphabetic()).collect();
    if letters.is_empty() {
        return true;
    }
    let latin = letters.iter().filter(|ch| ch.is_ascii_alphabetic()).count();
    latin * 100 / letters.len() >= 60
}

async fn send_html(
    request: reqwest::RequestBuilder,
    ddg_captcha: bool,
    wait: Duration,
) -> Result<String, String> {
    let response = timeout(wait, request.send())
        .await
        .map_err(|_| "Search request timed out".to_string())?
        .map_err(|error| format!("Search request failed: {error}"))?;
    let status = response.status().as_u16();
    if status != 200 && status != 202 {
        return Err(format!("Search HTTP {status}"));
    }
    let html = response
        .text()
        .await
        .map_err(|error| format!("Search response was not text: {error}"))?;
    if ddg_captcha && is_ddg_challenge(&html) {
        return Err("DuckDuckGo challenged the search (captcha)".into());
    }
    Ok(html)
}

fn is_ddg_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("anomaly.js")
        || lower.contains("please complete the captcha")
        || lower.contains("unfortunately, bots use duckduckgo")
        || lower.contains("select all squares containing a duck")
        || lower.contains("error-lite+")
}

async fn send_json(request: reqwest::RequestBuilder, wait: Duration) -> Result<Value, String> {
    let response = timeout(wait, request.send())
        .await
        .map_err(|_| "Search request timed out".to_string())?
        .map_err(|error| format!("Search request failed: {error}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("Search HTTP {status}"));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Invalid JSON: {error}"))
}

fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    let mut hits = parse_ddg_scraper(html, "a.result__a");
    if hits.is_empty() {
        hits = parse_ddg_results(html, "result__a", "result__snippet", "</a>");
    }
    hits
}

fn parse_ddg_lite_html(html: &str) -> Vec<SearchHit> {
    let mut hits = parse_ddg_scraper(html, "a.result-link");
    if hits.is_empty() {
        hits = parse_ddg_results(html, "result-link", "result-snippet", "</td>");
    }
    if hits.is_empty() {
        hits = parse_ddg_html(html);
    }
    hits
}

fn parse_ddg_scraper(html: &str, selector: &str) -> Vec<SearchHit> {
    let Ok(link_sel) = Selector::parse(selector) else {
        return Vec::new();
    };
    let document = Html::parse_document(html);
    let mut hits = Vec::new();
    for el in document.select(&link_sel) {
        let href = el.value().attr("href").unwrap_or("");
        let title = collapse_ws(&el.text().collect::<String>());
        let snippet = nearby_snippet(el);
        if let Some(hit) = clean_hit(title, decode_result_href(href), snippet) {
            hits.push(hit);
        }
    }
    hits
}

fn nearby_snippet(el: scraper::ElementRef<'_>) -> String {
    let snippet_sel = match Selector::parse(".result__snippet, .result-snippet") {
        Ok(sel) => sel,
        Err(_) => return String::new(),
    };
    let mut node = el.parent();
    for _ in 0..10 {
        let Some(parent) = node else {
            break;
        };
        if let Some(elem) = scraper::ElementRef::wrap(parent)
            && let Some(snip) = elem.select(&snippet_sel).next()
        {
            let text = collapse_ws(&snip.text().collect::<String>());
            if !text.is_empty() {
                return text;
            }
        }
        node = parent.parent();
    }
    String::new()
}

fn parse_ddg_results(
    html: &str,
    link_class: &str,
    snippet_class: &str,
    snippet_close: &str,
) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;
    let class_needle = format!("class=\"{link_class}\"");
    let class_needle_sq = format!("class='{link_class}'");
    while let Some(idx) = rest
        .find(&class_needle)
        .or_else(|| rest.find(&class_needle_sq))
    {
        let window_start = rest[..idx].rfind('<').unwrap_or(idx);
        rest = &rest[window_start..];
        let href = match attr_after(rest, "href=\"").or_else(|| attr_after(rest, "href='")) {
            Some(value) => value,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title_html = match between(rest, ">", "</a>") {
            Some(value) => value,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title = collapse_ws(&strip_tags(title_html));
        let url = decode_result_href(href);
        let snippet = rest
            .find(snippet_class)
            .and_then(|start| {
                let slice = &rest[start..];
                let window = &slice[..slice.len().min(1600)];
                between(window, ">", snippet_close)
                    .or_else(|| between(window, ">", "</"))
                    .map(strip_tags)
                    .map(|s| collapse_ws(&s))
            })
            .unwrap_or_default();
        if let Some(hit) = clean_hit(title, url, snippet) {
            hits.push(hit);
        }
        rest = &rest[1..];
    }
    hits
}

fn featured_hit(title: String, url: String, snippet: String) -> Option<SearchHit> {
    let title = collapse_ws(&title);
    let snippet = collapse_ws(&snippet);
    if title.is_empty() && snippet.is_empty() {
        return None;
    }
    let url = if (url.starts_with("http://") || url.starts_with("https://")) && !is_junk_url(&url) {
        url
    } else {
        String::new()
    };
    let title = if title.is_empty() {
        snippet.chars().take(80).collect()
    } else {
        title
    };
    Some(SearchHit {
        title,
        url,
        snippet,
        featured: true,
    })
}

fn clean_hit(title: String, url: String, snippet: String) -> Option<SearchHit> {
    if title.is_empty() || url.is_empty() {
        return None;
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    if is_junk_url(&url) || is_listing_url(&url) {
        return None;
    }
    Some(SearchHit {
        title,
        url,
        snippet,
        featured: false,
    })
}

fn is_junk_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let host = host_of(&lower);
    if host.is_empty() {
        return true;
    }
    if host.contains("duckduckgo.com")
        || host == "bing.com"
        || host.ends_with(".bing.com")
        || host == "search.yahoo.com"
        || host == "search.brave.com"
        || host == "cn.bing.com"
        || host == "news.google.com"
        || host == "news.yahoo.com"
        || host.ends_with(".news.yahoo.com")
    {
        return true;
    }
    if host.contains("google.")
        && (lower.contains("/search")
            || lower.contains("/url?")
            || lower.contains("/aclk")
            || lower.contains("/topics")
            || lower.contains("/news"))
    {
        return true;
    }
    if host.starts_with("accounts.")
        || host.starts_with("login.")
        || host.starts_with("auth.")
        || host.starts_with("signin.")
        || host.starts_with("signup.")
    {
        return true;
    }
    let path = lower
        .split("://")
        .nth(1)
        .unwrap_or(&lower)
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or("");
    if path.contains("sign-in")
        || path.contains("signin")
        || path.contains("login")
        || path.contains("signup")
        || path.contains("oauth")
    {
        return true;
    }
    lower.contains("/aclick") || lower.contains("/aclk")
}

fn is_listing_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let path = lower
        .split("://")
        .nth(1)
        .unwrap_or(&lower)
        .split_once('/')
        .map(|(_, path)| path)
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .any(|segment| {
            matches!(
                segment,
                "tag"
                    | "tags"
                    | "topic"
                    | "topics"
                    | "category"
                    | "categories"
                    | "section"
                    | "sections"
                    | "label"
                    | "labels"
            )
        })
}

fn is_preferred_news_host(host: &str) -> bool {
    const HOSTS: &[&str] = &[
        "reuters.com",
        "apnews.com",
        "associatedpress.com",
        "bbc.com",
        "bbc.co.uk",
        "nytimes.com",
        "washingtonpost.com",
        "wsj.com",
        "bloomberg.com",
        "ft.com",
        "theguardian.com",
        "npr.org",
        "cnbc.com",
        "cnn.com",
        "nbcnews.com",
        "abcnews.go.com",
        "theverge.com",
        "arstechnica.com",
        "techcrunch.com",
        "wired.com",
        "axios.com",
        "politico.com",
        "forbes.com",
        "aljazeera.com",
        "semafor.com",
        "theinformation.com",
    ];
    HOSTS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn is_tabloid_host(host: &str) -> bool {
    const HOSTS: &[&str] = &[
        "hellomagazine.com",
        "people.com",
        "eonline.com",
        "tmz.com",
        "okmagazine.com",
        "usmagazine.com",
        "popsugar.com",
        "pagesix.com",
    ];
    HOSTS
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

fn source_quality_bonus(query: &str, hit: &SearchHit) -> f32 {
    let host = host_of(&hit.url);
    let mut bonus = 0.0;
    if is_listing_url(&hit.url) {
        bonus -= 0.6;
    }
    if query_looks_like_news(query) {
        if is_preferred_news_host(&host) {
            bonus += 0.25;
        }
        if is_tabloid_host(&host) {
            bonus -= 0.3;
        }
    }
    bonus
}

fn host_of(url: &str) -> String {
    let rest = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("");
    rest.trim_start_matches("www.").to_ascii_lowercase()
}

fn decode_result_href(href: &str) -> String {
    unwrap_bing_destination(&decode_ddg_href(href))
}

fn decode_ddg_href(href: &str) -> String {
    let unescaped = html_unescape(href);
    let full = if let Some(rest) = unescaped.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        unescaped
    };
    for key in ["uddg=", "uddg3="] {
        if let Some(idx) = full.find(key) {
            let start = idx + key.len();
            let end = full[start..]
                .find('&')
                .map(|i| start + i)
                .unwrap_or(full.len());
            let decoded = percent_decode(&full[start..end]);
            if decoded.starts_with("http://") || decoded.starts_with("https://") {
                return decoded;
            }
        }
    }
    full
}

fn unwrap_bing_destination(url: &str) -> String {
    let host = host_of(&url.to_ascii_lowercase());
    if !(host == "bing.com" || host.ends_with(".bing.com")) {
        return url.to_string();
    }
    let Some(raw) = query_param(url, "u") else {
        return url.to_string();
    };
    let payload = raw.strip_prefix("a1").unwrap_or(&raw);
    let Some(bytes) = decode_b64(payload) else {
        return url.to_string();
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return url.to_string();
    };
    let text = text.trim();
    if text.starts_with("http://") || text.starts_with("https://") {
        text.to_string()
    } else {
        url.to_string()
    }
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let prefix = format!("{key}=");
    for part in query.split('&') {
        if let Some(value) = part.strip_prefix(&prefix) {
            return Some(percent_decode(value));
        }
    }
    None
}

fn decode_b64(payload: &str) -> Option<Vec<u8>> {
    const ENGINE: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let mut padded = payload.replace('-', "+").replace('_', "/");
    while !padded.is_empty() && padded.len() % 4 != 0 {
        padded.push('=');
    }
    ENGINE.decode(padded).ok()
}

fn hit_key(hit: &SearchHit) -> String {
    let url = canonical_url(&hit.url);
    if url.is_empty() {
        format!("title:{}", hit.title.to_ascii_lowercase())
    } else {
        url
    }
}

fn merge_sources(
    sources: Vec<(String, Vec<SearchHit>)>,
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let mut featured: Vec<SearchHit> = Vec::new();
    let mut scores: Vec<(f32, SearchHit)> = Vec::new();
    for (engine, hits) in sources {
        let weight = match engine.as_str() {
            "parallel" => 1.35,
            "tinyfish" => 1.35,
            "duckduckgo" => 1.2,
            "duckduckgo-post" => 1.15,
            "duckduckgo-lite" => 1.1,
            "searxng" => 1.25,
            _ => 0.9,
        };
        for (rank, hit) in hits.into_iter().enumerate() {
            let key = hit_key(&hit);
            if hit.featured {
                if let Some(existing) = featured.iter_mut().find(|item| hit_key(item) == key) {
                    if hit.snippet.len() > existing.snippet.len() {
                        *existing = hit;
                    }
                } else {
                    featured.push(hit);
                }
                continue;
            }
            if let Some((_, existing)) = scores.iter_mut().find(|(_, item)| hit_key(item) == key) {
                if hit.snippet.len() > existing.snippet.len() {
                    *existing = hit;
                }
                continue;
            }
            let mut score = weight / (20.0 + rank as f32);
            score += relevance_bonus(query, &hit);
            score += source_quality_bonus(query, &hit);
            scores.push((score, hit));
        }
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged = featured;
    merged.extend(scores.into_iter().map(|(_, hit)| hit));
    rank_hits(merged, query, limit)
}

fn relevance_bonus(query: &str, hit: &SearchHit) -> f32 {
    let tokens = distinctive_tokens(query);
    if tokens.is_empty() {
        return 0.0;
    }
    let blob = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_lowercase();
    let overlap = tokens
        .iter()
        .filter(|token| blob.contains(token.as_str()))
        .count();
    0.08 * overlap.min(4) as f32
}

fn rank_hits(hits: Vec<SearchHit>, query: &str, limit: usize) -> Vec<SearchHit> {
    let mut featured = Vec::new();
    let mut rest = Vec::new();
    for hit in hits {
        if hit.featured {
            let key = hit_key(&hit);
            if featured.iter().any(|item: &SearchHit| hit_key(item) == key) {
                continue;
            }
            featured.push(hit);
        } else {
            rest.push(hit);
        }
    }
    let pin_cap = 2.min(limit.max(1));
    featured.truncate(pin_cap);

    let ranked = rank_web_hits(rest, query, limit);
    let mut out = featured;
    for hit in ranked {
        if out.iter().any(|item| hit_key(item) == hit_key(&hit)) {
            continue;
        }
        out.push(hit);
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn rank_web_hits(hits: Vec<SearchHit>, query: &str, limit: usize) -> Vec<SearchHit> {
    let tokens = distinctive_tokens(query);
    let mut scored: Vec<(i32, SearchHit)> = hits
        .into_iter()
        .map(|hit| {
            let blob = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_lowercase();
            let overlap = tokens
                .iter()
                .filter(|token| blob.contains(token.as_str()))
                .count() as i32;
            (overlap, hit)
        })
        .collect();
    if !tokens.is_empty() {
        if query_is_mostly_latin(query) {
            scored.retain(|(overlap, hit)| *overlap > 0 || query_is_mostly_latin(&hit.title));
            let latin: Vec<(i32, SearchHit)> = scored
                .iter()
                .filter(|(_, hit)| query_is_mostly_latin(&hit.title))
                .cloned()
                .collect();
            if !latin.is_empty() {
                scored = latin;
            }
        }
        if query_looks_like_news(query) {
            let articles: Vec<(i32, SearchHit)> = scored
                .iter()
                .filter(|(_, hit)| {
                    !is_listing_url(&hit.url) && !is_tabloid_host(&host_of(&hit.url))
                })
                .cloned()
                .collect();
            if articles.len() >= 3 {
                scored = articles;
            } else {
                let not_listing: Vec<(i32, SearchHit)> = scored
                    .iter()
                    .filter(|(_, hit)| !is_listing_url(&hit.url))
                    .cloned()
                    .collect();
                if !not_listing.is_empty() {
                    scored = not_listing;
                }
            }
        }
        if query_looks_like_news(query) {
            scored.sort_by(|a, b| {
                let news_a = is_preferred_news_host(&host_of(&a.1.url));
                let news_b = is_preferred_news_host(&host_of(&b.1.url));
                news_b
                    .cmp(&news_a)
                    .then_with(|| b.0.cmp(&a.0))
                    .then_with(|| a.1.title.cmp(&b.1.title))
            });
        } else {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
        }
    }
    let mut out = Vec::new();
    let mut per_host: HashMap<String, usize> = HashMap::new();
    for (_, hit) in scored {
        let host = host_of(&hit.url);
        let count = per_host.entry(host).or_insert(0);
        if *count >= 2 && out.len() < limit {
            continue;
        }
        *count += 1;
        out.push(hit);
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

fn distinctive_tokens(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "about", "what", "when",
        "where", "which", "have", "has", "was", "were", "are", "latest", "official",
    ];
    let mut tokens: Vec<String> = query
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3 && !STOP.contains(token))
        .map(str::to_string)
        .collect();
    let cjk: Vec<char> = query.chars().filter(|ch| is_cjk(*ch)).collect();
    if cjk.len() >= 2 {
        for window in cjk.windows(2) {
            tokens.push(window.iter().collect());
        }
    } else if cjk.len() == 1 {
        tokens.push(cjk[0].to_string());
    }
    tokens
}

fn canonical_url(url: &str) -> String {
    let trimmed = url.trim();
    let without_frag = trimmed
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or(trimmed);
    let (path_part, query_part) = without_frag.split_once('?').unwrap_or((without_frag, ""));
    let mut value = path_part.to_ascii_lowercase();
    if let Some(stripped) = value.strip_prefix("https://") {
        value = stripped.to_string();
    } else if let Some(stripped) = value.strip_prefix("http://") {
        value = stripped.to_string();
    }
    if let Some(stripped) = value.strip_prefix("www.") {
        value = stripped.to_string();
    }
    let mut value = value.trim_end_matches('/').to_string();
    if !query_part.is_empty() {
        value.push('?');
        value.push_str(query_part);
    }
    value
}

fn attr_after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let start = s.find(key)? + key.len();
    let quote = if key.ends_with('\'') { '\'' } else { '"' };
    let end = s[start..].find(quote)? + start;
    Some(&s[start..end])
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    html_unescape(&out)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, url: &str, snippet: &str) -> SearchHit {
        SearchHit {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            featured: false,
        }
    }

    #[test]
    fn unwraps_ddg_redirect() {
        let url =
            decode_ddg_href("//duckduckgo.com/l/?uddg=https%3A%2F%2Fx.ai%2Fblog%2Fgrok&rut=1");
        assert_eq!(url, "https://x.ai/blog/grok");
    }

    #[test]
    fn parses_lite_results() {
        let html = r#"
            <a class="result-link" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fx.ai%2Fgrok">xAI Grok announcement</a>
            <td class="result-snippet">Official Grok news from xAI this week.</td>
            <a class="result-link" href="https://example.com/unrelated">Lao stone seals</a>
            <td class="result-snippet">印章石料 shopping</td>
        "#;
        let hits = rank_hits(parse_ddg_lite_html(html), "xAI Grok news", 6);
        assert_eq!(hits[0].url, "https://x.ai/grok");
        assert!(hits.iter().any(|hit| hit.url.contains("x.ai")));
    }

    #[test]
    fn drops_zero_overlap_when_enough_relevant() {
        let hits = vec![
            hit("xAI Grok", "https://x.ai/grok", "Grok announcement"),
            hit("xAI newsroom", "https://x.ai/news", "Grok update"),
            hit("Grok chatbot", "https://en.wikipedia.org/wiki/Grok", "xAI"),
            hit(
                "老挝石 印章石料",
                "https://shop.example/stone",
                "e-commerce",
            ),
        ];
        let ranked = rank_hits(hits, "xAI Grok news", 6);
        assert!(ranked.iter().all(|hit| !hit.url.contains("shop.example")));
        assert!(ranked.iter().any(|hit| hit.url.contains("x.ai")));
    }

    #[test]
    fn keeps_english_zero_overlap_when_under_limit() {
        let hits = vec![
            hit("xAI Grok", "https://x.ai/grok", "Grok announcement"),
            hit(
                "Company blog",
                "https://x.ai/blog",
                "quarterly notes from the team",
            ),
        ];
        let ranked = rank_hits(hits, "xAI Grok news", 6);
        assert!(ranked.iter().any(|hit| hit.url.ends_with("/grok")));
        assert!(ranked.iter().any(|hit| hit.url.ends_with("/blog")));
    }

    #[test]
    fn drops_cjk_titles_for_english_queries_when_enough_latin() {
        let hits = vec![
            hit("About Grok on X", "https://help.x.com/grok", "xAI"),
            hit("xAI Grok newsroom", "https://x.ai/news", "Grok"),
            hit("Grok chatbot", "https://en.wikipedia.org/wiki/Grok", "xAI"),
            hit("首页 | GROK官网", "https://grok-cn.top/", "Grok"),
        ];
        let ranked = rank_hits(hits, "Grok xAI latest news", 6);
        assert!(ranked.iter().all(|hit| !hit.url.contains("grok-cn.top")));
    }

    #[test]
    fn drops_unrelated_chinese_pages_for_english_query() {
        let hits = vec![
            hit(
                "冒险岛游戏金币不够用怎么办",
                "https://zhidao.baidu.com/question/123",
                "2020-03-12",
            ),
            hit(
                "秀米编辑器怎么设置背景图",
                "https://jingyan.baidu.com/article/456",
                "2018-10-03",
            ),
            hit(
                "如何将秀米的内容导出到微信公众号",
                "https://www.zhihu.com/question/789",
                "2021-01-19",
            ),
        ];
        let ranked = rank_hits(hits, "latest Grok news", 6);
        assert!(ranked.is_empty());
    }

    #[test]
    fn cjk_query_has_tokens() {
        let tokens = distinctive_tokens("北京天气");
        assert!(!tokens.is_empty());
        assert!(
            tokens
                .iter()
                .any(|token| token == "北京" || token == "天气")
        );
    }

    #[test]
    fn canonical_url_keeps_query_string() {
        assert_eq!(
            canonical_url("https://www.youtube.com/watch?v=dQw4w9wg"),
            "youtube.com/watch?v=dQw4w9wg"
        );
    }

    #[test]
    fn accept_language_follows_region() {
        assert_eq!(accept_language("us-en"), "en-US,en;q=0.9");
        assert_eq!(accept_language("cn-zh"), "zh-CN,zh;q=0.9,en;q=0.5");
        assert_eq!(accept_language("wt-wt"), "en-US,en;q=0.9");
    }

    #[test]
    fn searxng_language_follows_region() {
        assert_eq!(searxng_language("us-en"), "en-US");
        assert_eq!(searxng_language("cn-zh"), "zh-CN");
        assert_eq!(searxng_language("wt-wt"), "en-US");
    }

    #[test]
    fn unwraps_bing_ck_tracking_url() {
        let href = "https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9oZWxwLnguY29tL2VuL3VzaW5nLXgvYWJvdXQtZ3Jvay&amp;ntb=1";
        assert_eq!(
            decode_result_href(href),
            "https://help.x.com/en/using-x/about-grok"
        );
        assert!(!is_junk_url(&decode_result_href(href)));
        assert!(is_junk_url("https://www.bing.com/ck/a?!&&p=abc&u=missing"));
    }

    #[test]
    fn parses_searxng_json_and_html() {
        let body = serde_json::json!({
            "answers": [{ "answer": "Grok is a chatbot by xAI", "url": "https://x.ai/" }],
            "results": [
                { "title": "xAI Grok", "url": "https://x.ai/grok", "content": "Official product page." },
                { "title": "Sign in", "url": "https://accounts.x.ai/sign-in", "content": "Create an account" }
            ]
        });
        let hits = parse_searxng_json(&body, 6);
        assert!(hits.iter().any(|hit| hit.featured
            && hit.snippet.contains("chatbot")
            && hit.url == "https://x.ai/"));
        assert!(hits.iter().any(|hit| hit.url == "https://x.ai/grok"));
        assert!(hits.iter().all(|hit| !hit.url.contains("accounts.x.ai")));

        let html = r#"
            <article class="result">
              <h3><a href="https://x.ai/blog/grok">xAI Grok news</a></h3>
              <p class="content">Official Grok update from xAI.</p>
            </article>
            <article class="result">
              <h3><a href="https://help.x.com/en/using-x/about-grok">About Grok</a></h3>
              <p class="content">Help Center</p>
            </article>
        "#;
        let hits = parse_searxng_html(html);
        assert!(hits.iter().any(|hit| hit.url == "https://x.ai/blog/grok"));
        assert!(hits.iter().any(|hit| hit.url.contains("help.x.com")));
    }

    #[test]
    fn parses_tinyfish_json_results() {
        let body = serde_json::json!({
            "query": "web automation",
            "page": 0,
            "total_results": 2,
            "results": [
                {
                    "position": 1,
                    "site_name": "tinyfish.ai",
                    "title": "TinyFish",
                    "snippet": "AI web automation",
                    "url": "https://tinyfish.ai"
                },
                {
                    "position": 2,
                    "site_name": "example.com",
                    "title": "News",
                    "snippet": "Breaking update",
                    "url": "https://example.com/news",
                    "date": "2026-08-01"
                },
                {
                    "position": 3,
                    "site_name": "bing.com",
                    "title": "Junk",
                    "snippet": "drop",
                    "url": "https://www.bing.com/search?q=x"
                }
            ]
        });
        let hits = parse_tinyfish_json(&body, 6);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://tinyfish.ai");
        assert!(hits[0].snippet.contains("tinyfish.ai"));
        assert_eq!(hits[1].url, "https://example.com/news");
        assert!(hits[1].snippet.starts_with("2026-08-01"));
    }

    #[test]
    fn tinyfish_geo_and_recency_helpers() {
        assert_eq!(
            tinyfish_geo("us-en"),
            Some(("US".into(), "en".into()))
        );
        assert_eq!(tinyfish_geo("wt-wt"), None);
        assert_eq!(recency_minutes(WebSearchRecency::Day), Some(1_440));
        assert!(recency_minutes(WebSearchRecency::Any).is_none());
    }

    #[test]
    fn parses_parallel_mcp_tool_envelope() {
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{
                    "type": "text",
                    "text": "{\"search_id\":\"s1\",\"results\":[{\"url\":\"https://parallel.ai/\",\"title\":\"Parallel\",\"excerpts\":[\"Hello\"]}]}"
                }]
            }
        });
        let body = parallel_mcp_search_body(&envelope).unwrap();
        let hits = parse_parallel_json(&body, 6);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://parallel.ai/");
    }

    #[test]
    fn parses_parallel_json_results() {
        let body = serde_json::json!({
            "search_id": "search_test",
            "session_id": "session_test",
            "results": [
                {
                    "url": "https://parallel.ai/products/search",
                    "title": "Parallel Search",
                    "excerpts": ["First excerpt.", "Second excerpt."]
                },
                {
                    "url": "https://www.bing.com/search?q=x",
                    "title": "Junk",
                    "excerpts": ["should drop"]
                },
                {
                    "url": "https://example.com/article",
                    "title": null,
                    "excerpts": ["Body text"]
                }
            ]
        });
        let hits = parse_parallel_json(&body, 6);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://parallel.ai/products/search");
        assert!(hits[0].snippet.contains("First excerpt"));
        assert!(hits[0].snippet.contains("Second excerpt"));
        assert_eq!(hits[1].url, "https://example.com/article");
        assert_eq!(hits[1].title, "https://example.com/article");
    }

    #[test]
    fn parallel_location_and_recency_helpers() {
        assert_eq!(parallel_location("us-en").as_deref(), Some("us"));
        assert_eq!(parallel_location("wt-wt"), None);
        assert!(recency_after_date(WebSearchRecency::Any).is_none());
        let week = recency_after_date(WebSearchRecency::Week).unwrap();
        assert_eq!(week.len(), 10);
        assert_eq!(&week[4..5], "-");
        assert_eq!(unix_ymd(0), "1970-01-01");
    }

    #[test]
    fn normalizes_searxng_instance_url() {
        assert_eq!(
            parse_searxng_endpoint("https://searx.example.com")
                .unwrap()
                .unwrap()
                .as_str(),
            "https://searx.example.com/search"
        );
        assert_eq!(
            parse_searxng_endpoint("http://127.0.0.1:8080/searxng/")
                .unwrap()
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8080/searxng/search"
        );
        assert!(parse_searxng_endpoint("").unwrap().is_none());
        assert!(parse_searxng_endpoint("javascript:alert(1)").is_err());
        assert!(is_ddg_challenge(
            "Unfortunately, bots use DuckDuckGo too. Select all squares containing a duck."
        ));
    }

    #[test]
    fn drops_auth_and_signin_urls() {
        assert!(is_junk_url("https://accounts.x.ai/sign-in"));
        assert!(is_junk_url("https://login.microsoftonline.com/"));
        assert!(!is_junk_url("https://x.ai/blog/grok"));
    }

    #[test]
    fn merge_pins_featured_first() {
        let sources = vec![(
            "bing".into(),
            vec![
                SearchHit {
                    title: "Grok".into(),
                    url: "https://x.ai/".into(),
                    snippet: "chatbot by xAI".into(),
                    featured: true,
                },
                hit("News", "https://x.ai/blog", "Grok news"),
            ],
        )];
        let merged = merge_sources(sources, "Grok news", 6);
        assert!(merged[0].featured);
        assert!(merged.iter().any(|hit| hit.url.contains("/blog")));
    }

    #[test]
    fn merge_keeps_hits_when_other_sources_are_empty() {
        let sources = vec![
            ("duckduckgo-lite".into(), Vec::new()),
            (
                "searxng".into(),
                vec![hit("Grok", "https://x.ai/grok", "xAI chatbot")],
            ),
        ];
        let merged = merge_sources(sources, "Grok news", 6);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].url, "https://x.ai/grok");
    }

    #[test]
    fn drops_search_engine_host_urls() {
        assert!(is_junk_url("https://www.bing.com/ck/a?!&&p=abc&u=missing"));
        assert!(is_junk_url("https://cn.bing.com/search?q=x"));
        assert!(is_junk_url(
            "https://news.google.com/topics/CAAqJggKIiBDQkFTRWdvSUwyMHZNRGx1YlY4U0FtVnVHZ0pWVXlnQVAB"
        ));
        assert!(is_listing_url(
            "https://www.hellomagazine.com/tags/elon-musk/"
        ));
        assert!(!is_listing_url(
            "https://www.reuters.com/technology/elon-musk-tesla-2026-08-27/"
        ));
        assert!(reject_result_url(
            "https://www.hellomagazine.com/tags/elon-musk/"
        ));
    }

    #[test]
    fn newsy_queries_are_detected_for_ranking() {
        assert!(query_looks_like_news("latest elon musk news"));
        assert!(query_looks_like_news("breaking tesla news today"));
        assert!(!query_looks_like_news("how to install rust"));
    }

    #[test]
    fn news_ranking_prefers_wire_report_over_tag_page() {
        let hits = vec![
            hit(
                "Elon Musk tag",
                "https://www.hellomagazine.com/tags/elon-musk/",
                "Latest Elon Musk news and photos",
            ),
            hit(
                "Musk unveils plan",
                "https://www.reuters.com/technology/musk-plan-2026-08-27/",
                "Elon Musk news from Tesla",
            ),
            hit(
                "Google topic",
                "https://news.google.com/topics/CAAqJgg",
                "Elon Musk latest news",
            ),
        ];
        let ranked = rank_hits(hits, "latest elon musk news", 6);
        assert!(ranked.iter().any(|hit| hit.url.contains("reuters.com")));
        assert!(
            ranked
                .iter()
                .all(|hit| !hit.url.contains("hellomagazine.com")
                    && !hit.url.contains("news.google.com"))
        );
        assert_eq!(ranked[0].url.contains("reuters.com"), true);
    }

    #[tokio::test]
    #[ignore = "hits the network"]
    async fn live_search_returns_hits() {
        let skills = AgentSkills::default();
        let (hits, engine, note) = search_web("latest Grok news", &skills)
            .await
            .expect("search");
        assert!(!hits.is_empty(), "engine={engine} note={note}");
        let blob = format!("{engine} {note}").to_ascii_lowercase();
        assert!(
            blob.contains("duckduckgo") || blob.contains("searx"),
            "expected DuckDuckGo or SearXNG, got engine={engine} note={note}"
        );
        assert!(hits.iter().all(|hit| !hit.title.contains("class=")));
        assert!(
            hits.iter().any(|hit| {
                let blob = format!("{} {}", hit.title, hit.url).to_ascii_lowercase();
                blob.contains("grok") || blob.contains("x.ai") || blob.contains("xai")
            }),
            "engine={engine} hits={hits:?}"
        );
    }
}
