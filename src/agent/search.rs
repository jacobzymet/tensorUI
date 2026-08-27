//! Native web search: DuckDuckGo HTML + Lite, optional SearXNG.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use scraper::{Html, Selector};
use serde_json::Value;
use tokio::time::timeout;

use super::{AgentSkills, SearchHit, WebSearchRecency, WebSearchSafeSearch};
use crate::http;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);
const DDG_LITE: &str = "https://lite.duckduckgo.com/lite/";
const DDG_HTML: &str = "https://html.duckduckgo.com/html/";

pub(super) async fn search_web(
    query: &str,
    skills: &AgentSkills,
) -> Result<(Vec<SearchHit>, String), String> {
    let query = collapse_ws(query);
    if query.is_empty() {
        return Err("web_search requires a non-empty \"query\" string.".into());
    }
    let limit = skills.search_result_count();
    let mut errors: Vec<String> = Vec::new();
    let searx_endpoint = match parse_searxng_endpoint(&skills.web_search_searxng) {
        Ok(url) => url,
        Err(error) => {
            errors.push(format!("searxng: {error}"));
            None
        }
    };

    let lite_f = ddg_lite_hits(&query, skills);
    let html_get_f = ddg_html_get_hits(&query, skills);
    let html_post_f = ddg_html_post_hits(&query, skills);
    let searx_f = async {
        match &searx_endpoint {
            Some(url) => searxng_hits(url, &query, skills, limit).await,
            None => Ok(Vec::new()),
        }
    };

    let (lite, html_get, html_post, searx) = tokio::join!(lite_f, html_get_f, html_post_f, searx_f);

    let mut by_source: Vec<(String, Vec<SearchHit>)> = Vec::new();
    push_source(&mut by_source, &mut errors, "duckduckgo-lite", lite);
    push_source(&mut by_source, &mut errors, "duckduckgo", html_get);
    push_source(&mut by_source, &mut errors, "duckduckgo-post", html_post);
    if searx_endpoint.is_some() {
        push_source(&mut by_source, &mut errors, "searxng", searx);
    }

    if by_source.is_empty() {
        let detail = if errors.is_empty() {
            "no search endpoint returned usable links".into()
        } else {
            errors.join("; ")
        };
        let hint = if searx_endpoint.is_none() {
            " DuckDuckGo HTML/Lite may be blocked — set a SearXNG instance URL in Agent Capabilities if you have one."
        } else {
            ""
        };
        return Err(format!("Web search failed ({detail}).{hint}"));
    }

    let engine = by_source
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join("+");
    let merged = merge_sources(by_source, &query, limit);
    if merged.is_empty() {
        return Err(format!("Search returned no pages matching {query:?}."));
    }
    Ok((merged, engine))
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
    let html = send_html(request, true).await?;
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
    let html = send_html(post, true).await?;
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
    let html = send_html(get, true).await?;
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
    match searxng_json_hits(endpoint, query, skills, limit).await {
        Ok(hits) if !hits.is_empty() => return Ok(hits),
        Ok(_) => {}
        Err(_) => {}
    }
    searxng_html_hits(endpoint, query, skills).await
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
        .timeout(REQUEST_TIMEOUT)
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
    let body = send_json(searxng_request(endpoint, query, skills, true)).await?;
    Ok(parse_searxng_json(&body, limit))
}

async fn searxng_html_hits(
    endpoint: &reqwest::Url,
    query: &str,
    skills: &AgentSkills,
) -> Result<Vec<SearchHit>, String> {
    let html = send_html(searxng_request(endpoint, query, skills, false), false).await?;
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

async fn send_html(request: reqwest::RequestBuilder, ddg_captcha: bool) -> Result<String, String> {
    let response = timeout(REQUEST_TIMEOUT, request.send())
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

async fn send_json(request: reqwest::RequestBuilder) -> Result<Value, String> {
    let response = timeout(REQUEST_TIMEOUT, request.send())
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
        let (hits, engine) = search_web("latest Grok news", &skills)
            .await
            .expect("search");
        assert!(!hits.is_empty(), "engine={engine}");
        assert!(
            engine.contains("duckduckgo") || engine.contains("searxng"),
            "expected DuckDuckGo or SearXNG, got {engine}"
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
