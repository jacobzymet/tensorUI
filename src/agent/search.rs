//! Native web search: DuckDuckGo first, international Bing if DDG is unreachable.

use std::time::Duration;

use base64::Engine;
use scraper::{Html, Selector};
use serde_json::Value;
use tokio::time::timeout;

use super::{
    AgentSkills, SearchHit, WebSearchBackend, WebSearchKind, WebSearchRecency, WebSearchSafeSearch,
};
use crate::http;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(4);
const DDG_LITE: &str = "https://lite.duckduckgo.com/lite/";
const DDG_HTML: &str = "https://html.duckduckgo.com/html/";
const BING_GLOBAL: &str = "https://global.bing.com/search";

pub(super) async fn search_web(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<(Vec<SearchHit>, String), String> {
    let query = collapse_ws(query);
    if query.is_empty() {
        return Err("web_search requires a non-empty \"query\" string.".into());
    }
    let limit = skills.search_result_count();
    let region = skills.search_region();
    match skills.web_search_backend {
        WebSearchBackend::Wikipedia => {
            let hits = rank_hits(wikipedia_hits(&query, &region, limit).await?, &query, limit);
            if hits.is_empty() {
                return Err(format!("Wikipedia returned no pages matching {query:?}."));
            }
            return Ok((hits, "wikipedia".into()));
        }
        WebSearchBackend::Bing => {
            return finish_hits(
                bing_global_hits(&query, skills, limit).await?,
                &query,
                limit,
                "bing",
            );
        }
        _ => {}
    }

    let (lite, html_post, html_get) = tokio::join!(
        ddg_lite_hits(&query, skills, kind),
        ddg_html_post_hits(&query, skills, kind),
        ddg_html_get_hits(&query, skills, kind),
    );

    let mut by_source: Vec<(String, Vec<SearchHit>)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    push_source(&mut by_source, &mut errors, "duckduckgo-lite", lite);
    push_source(&mut by_source, &mut errors, "duckduckgo", html_post);
    push_source(&mut by_source, &mut errors, "duckduckgo-get", html_get);

    let want_bing = allow_bing_fallback(skills.web_search_backend);
    if want_bing && ranked_or_empty(&by_source, &query, limit).is_empty() {
        match bing_global_hits(&query, skills, limit).await {
            Ok(hits) if !hits.is_empty() => by_source.push(("bing".into(), hits)),
            Ok(_) => {}
            Err(error) => errors.push(format!("bing: {error}")),
        }
    }

    if by_source.is_empty() {
        let detail = if errors.is_empty() {
            "no search endpoint returned usable links".into()
        } else {
            errors.join("; ")
        };
        return Err(format!(
            "Web search failed ({detail}). DuckDuckGo may be blocked — use a VPN, or leave Auto on so international Bing can fill in."
        ));
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

fn ranked_or_empty(
    sources: &[(String, Vec<SearchHit>)],
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    if sources.is_empty() {
        return Vec::new();
    }
    merge_sources(sources.to_vec(), query, limit)
}

fn allow_bing_fallback(backend: WebSearchBackend) -> bool {
    !matches!(backend, WebSearchBackend::Duckduckgo)
}

fn finish_hits(
    hits: Vec<SearchHit>,
    query: &str,
    limit: usize,
    engine: &str,
) -> Result<(Vec<SearchHit>, String), String> {
    let ranked = rank_hits(hits, query, limit);
    if ranked.is_empty() {
        return Err(format!("{engine} returned no pages matching {query:?}."));
    }
    Ok((ranked, engine.into()))
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

fn ddg_params(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Vec<(&'static str, String)> {
    let mut params = vec![
        ("q", query.to_string()),
        ("kl", skills.search_region()),
        ("kp", safesearch_kp(skills.web_search_safesearch).into()),
    ];
    if let Some(df) = recency_df(skills.web_search_recency) {
        params.push(("df", df.into()));
    }
    if kind == WebSearchKind::News {
        params.push(("iar", "news".into()));
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

async fn ddg_lite_hits(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let params = ddg_params(query, skills, kind);
    let query_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let request = http::apply_browser_navigation_headers(
        client
            .get(DDG_LITE)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://lite.duckduckgo.com/")
            .query(&query_refs),
    );
    let html = send_html(request, true).await?;
    Ok(parse_ddg_lite_html(&html))
}

async fn ddg_html_post_hits(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let mut form = ddg_params(query, skills, kind);
    form.push(("b", String::new()));
    let post = http::apply_browser_navigation_headers(
        client
            .post(DDG_HTML)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://html.duckduckgo.com/html/")
            .form(&form),
    );
    let html = send_html(post, true).await?;
    Ok(parse_ddg_html(&html))
}

async fn ddg_html_get_hits(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<Vec<SearchHit>, String> {
    let client = http::search_client();
    let params = ddg_params(query, skills, kind);
    let query_refs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let get = http::apply_browser_navigation_headers(
        client
            .get(DDG_HTML)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://html.duckduckgo.com/html/")
            .query(&query_refs),
    );
    let html = send_html(get, true).await?;
    Ok(parse_ddg_html(&html))
}

async fn wikipedia_hits(query: &str, region: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    let lang = region
        .split('-')
        .nth(1)
        .filter(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_lowercase()))
        .unwrap_or("en");
    let url = format!("https://{lang}.wikipedia.org/w/api.php");
    let client = http::search_client();
    let request = client.get(&url).timeout(REQUEST_TIMEOUT).query(&[
        ("action", "opensearch"),
        ("profile", "fuzzy"),
        ("limit", &limit.to_string()),
        ("search", query),
    ]);
    let body: Value = timeout(REQUEST_TIMEOUT, request.send())
        .await
        .map_err(|_| "Wikipedia search timed out".to_string())?
        .map_err(|error| format!("Wikipedia search failed: {error}"))?
        .json()
        .await
        .map_err(|error| format!("Invalid Wikipedia JSON: {error}"))?;
    let titles = body.get(1).and_then(Value::as_array);
    let snippets = body.get(2).and_then(Value::as_array);
    let urls = body.get(3).and_then(Value::as_array);
    let mut hits = Vec::new();
    if let (Some(titles), Some(urls)) = (titles, urls) {
        for (index, title) in titles.iter().enumerate() {
            let title = title.as_str().unwrap_or("").trim();
            let url = urls.get(index).and_then(Value::as_str).unwrap_or("").trim();
            if title.is_empty() || url.is_empty() {
                continue;
            }
            let snippet = snippets
                .and_then(|items| items.get(index))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            hits.push(SearchHit {
                title: title.to_string(),
                url: url.to_string(),
                snippet: collapse_ws(snippet),
            });
        }
    }
    Ok(hits)
}

async fn bing_global_hits(
    query: &str,
    skills: &AgentSkills,
    limit: usize,
) -> Result<Vec<SearchHit>, String> {
    let (mkt, set_lang, cc) = bing_market(query, &skills.search_region());
    let count = limit.to_string();
    let client = http::bing_client();
    let request = http::apply_browser_navigation_headers(
        client
            .get(BING_GLOBAL)
            .timeout(REQUEST_TIMEOUT)
            .header("Referer", "https://global.bing.com/")
            .query(&[
                ("q", query),
                ("mkt", mkt.as_str()),
                ("setmkt", mkt.as_str()),
                ("setLang", set_lang.as_str()),
                ("cc", cc.as_str()),
                ("ensearch", "1"),
                ("count", count.as_str()),
            ]),
    );
    let html = send_html(request, false).await?;
    Ok(parse_bing_html(&html))
}

fn bing_market(query: &str, region: &str) -> (String, String, String) {
    if query_is_mostly_latin(query) {
        return ("en-US".into(), "en".into(), "US".into());
    }
    let region = region.trim().to_ascii_lowercase();
    let mut parts = region.split('-');
    let country = parts.next().unwrap_or("");
    let lang = parts.next().unwrap_or("");
    if country.len() == 2
        && lang.len() == 2
        && country != "wt"
        && country.bytes().all(|b| b.is_ascii_lowercase())
        && lang.bytes().all(|b| b.is_ascii_lowercase())
    {
        let cc = country.to_ascii_uppercase();
        return (format!("{lang}-{cc}"), lang.to_string(), cc);
    }
    ("en-US".into(), "en".into(), "US".into())
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
    if ddg_captcha && (html.contains("anomaly.js") || html.contains("Please complete the captcha"))
    {
        return Err("DuckDuckGo challenged the search (captcha)".into());
    }
    Ok(html)
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

fn parse_bing_html(html: &str) -> Vec<SearchHit> {
    let mut hits = parse_bing_scraper(html);
    if hits.len() < 2 {
        let fallback = parse_bing_string(html);
        if fallback.len() > hits.len() {
            hits = fallback;
        }
    }
    hits
}

fn parse_bing_scraper(html: &str) -> Vec<SearchHit> {
    let Ok(algo_sel) = Selector::parse("li.b_algo") else {
        return Vec::new();
    };
    let Ok(title_sel) = Selector::parse("h2 a, a.tilk") else {
        return Vec::new();
    };
    let Ok(href_sel) = Selector::parse("a.tilk, h2 a") else {
        return Vec::new();
    };
    let snippet_sel = Selector::parse(".b_caption p, .b_lineclamp, .b_snippet").ok();
    let document = Html::parse_document(html);
    let mut hits = Vec::new();
    for item in document.select(&algo_sel) {
        let href = item
            .select(&href_sel)
            .find_map(|a| a.value().attr("href"))
            .unwrap_or("");
        let title = item
            .select(&title_sel)
            .next()
            .map(|a| collapse_ws(&a.text().collect::<String>()))
            .unwrap_or_default();
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

fn parse_bing_string(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("b_algo") {
        rest = &rest[idx..];
        let block_end = rest.find("</li>").unwrap_or(rest.len().min(4500));
        let block = &rest[..block_end];
        let href = href_with_class(block, "tilk")
            .or_else(|| first_result_href(block))
            .unwrap_or("");
        let title = between(block, "<h2", "</h2>")
            .map(strip_tags)
            .map(|s| collapse_ws(&s))
            .unwrap_or_default();
        let snippet = between(block, "b_caption", "</p>")
            .or_else(|| between(block, "b_lineclamp", "</"))
            .map(strip_tags)
            .map(|s| collapse_ws(&s))
            .unwrap_or_default();
        if let Some(hit) = clean_hit(title, decode_result_href(href), snippet) {
            hits.push(hit);
        }
        rest = &rest[1..];
    }
    hits
}

fn href_with_class<'a>(block: &'a str, class: &str) -> Option<&'a str> {
    let needle = format!("class=\"{class}\"");
    let idx = block.find(&needle).or_else(|| {
        let alt = format!("class='{class}'");
        block.find(&alt)
    })?;
    let start = idx.saturating_sub(240);
    let end = (idx + 480).min(block.len());
    let window = &block[start..end];
    attr_after(window, "href=\"").or_else(|| attr_after(window, "href='"))
}

fn first_result_href(block: &str) -> Option<&str> {
    let mut rest = block;
    while let Some(idx) = rest.find("href=\"") {
        rest = &rest[idx..];
        let value = attr_after(rest, "href=\"")?;
        if value.starts_with("http") || value.contains("uddg=") || value.contains("/ck/") {
            return Some(value);
        }
        rest = &rest[1..];
    }
    None
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

fn clean_hit(title: String, url: String, snippet: String) -> Option<SearchHit> {
    if title.is_empty() || url.is_empty() {
        return None;
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    if is_junk_url(&url) {
        return None;
    }
    Some(SearchHit {
        title,
        url,
        snippet,
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
    {
        return true;
    }
    if host.contains("google.")
        && (lower.contains("/search") || lower.contains("/url?") || lower.contains("/aclk"))
    {
        return true;
    }
    lower.contains("/aclick") || lower.contains("/aclk")
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

fn merge_sources(
    sources: Vec<(String, Vec<SearchHit>)>,
    query: &str,
    limit: usize,
) -> Vec<SearchHit> {
    let mut scores: Vec<(f32, SearchHit)> = Vec::new();
    for (engine, hits) in sources {
        let weight = match engine.as_str() {
            "duckduckgo" => 1.2,
            "duckduckgo-get" => 1.15,
            "duckduckgo-lite" => 1.1,
            "bing" => 1.0,
            "wikipedia" => 0.85,
            _ => 0.9,
        };
        for (rank, hit) in hits.into_iter().enumerate() {
            let key = canonical_url(&hit.url);
            if let Some((_, existing)) = scores
                .iter_mut()
                .find(|(_, item)| canonical_url(&item.url) == key)
            {
                if hit.snippet.len() > existing.snippet.len() {
                    *existing = hit;
                }
                continue;
            }
            let mut score = weight / (20.0 + rank as f32);
            score += relevance_bonus(query, &hit);
            scores.push((score, hit));
        }
    }
    scores.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    rank_hits(
        scores.into_iter().map(|(_, hit)| hit).collect(),
        query,
        limit,
    )
}

fn relevance_bonus(query: &str, hit: &SearchHit) -> f32 {
    let tokens = distinctive_tokens(query);
    if tokens.is_empty() {
        return 0.0;
    }
    let blob = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_ascii_lowercase();
    let overlap = tokens
        .iter()
        .filter(|token| blob.contains(token.as_str()))
        .count();
    0.08 * overlap.min(4) as f32
}

fn rank_hits(hits: Vec<SearchHit>, query: &str, limit: usize) -> Vec<SearchHit> {
    let tokens = distinctive_tokens(query);
    let mut scored: Vec<(i32, SearchHit)> = hits
        .into_iter()
        .map(|hit| {
            let blob = format!("{} {} {}", hit.title, hit.snippet, hit.url).to_ascii_lowercase();
            let overlap = tokens
                .iter()
                .filter(|token| blob.contains(token.as_str()))
                .count() as i32;
            (overlap, hit)
        })
        .collect();
    if !tokens.is_empty() {
        let relevant: Vec<(i32, SearchHit)> = scored
            .iter()
            .filter(|(overlap, _)| *overlap > 0)
            .cloned()
            .collect();
        if relevant.is_empty() {
            scored.clear();
        } else {
            scored = relevant;
        }
        if query_is_mostly_latin(query) {
            let latin: Vec<(i32, SearchHit)> = scored
                .iter()
                .filter(|(_, hit)| query_is_mostly_latin(&hit.title))
                .cloned()
                .collect();
            if !latin.is_empty() {
                scored = latin;
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    }
    let mut out = Vec::new();
    let mut per_host: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
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

fn distinctive_tokens(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "about", "what", "when",
        "where", "which", "have", "has", "was", "were", "are", "latest", "official",
    ];
    query
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3 && !STOP.contains(token))
        .map(str::to_string)
        .collect()
}

fn canonical_url(url: &str) -> String {
    let mut value = url.trim().to_ascii_lowercase();
    if let Some(stripped) = value.strip_prefix("https://") {
        value = stripped.to_string();
    } else if let Some(stripped) = value.strip_prefix("http://") {
        value = stripped.to_string();
    }
    if let Some(stripped) = value.strip_prefix("www.") {
        value = stripped.to_string();
    }
    if let Some((path, _)) = value.split_once('?') {
        value = path.to_string();
    }
    value.trim_end_matches('/').to_string()
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
            SearchHit {
                title: "xAI Grok".into(),
                url: "https://x.ai/grok".into(),
                snippet: "Grok announcement".into(),
            },
            SearchHit {
                title: "xAI newsroom".into(),
                url: "https://x.ai/news".into(),
                snippet: "Grok update".into(),
            },
            SearchHit {
                title: "Grok chatbot".into(),
                url: "https://en.wikipedia.org/wiki/Grok".into(),
                snippet: "xAI".into(),
            },
            SearchHit {
                title: "老挝石 印章石料".into(),
                url: "https://shop.example/stone".into(),
                snippet: "e-commerce".into(),
            },
        ];
        let ranked = rank_hits(hits, "xAI Grok news", 6);
        assert!(ranked.iter().all(|hit| !hit.url.contains("shop.example")));
        assert!(ranked.iter().any(|hit| hit.url.contains("x.ai")));
    }

    #[test]
    fn drops_cjk_titles_for_english_queries_when_enough_latin() {
        let hits = vec![
            SearchHit {
                title: "About Grok on X".into(),
                url: "https://help.x.com/grok".into(),
                snippet: "xAI".into(),
            },
            SearchHit {
                title: "xAI Grok newsroom".into(),
                url: "https://x.ai/news".into(),
                snippet: "Grok".into(),
            },
            SearchHit {
                title: "Grok chatbot".into(),
                url: "https://en.wikipedia.org/wiki/Grok".into(),
                snippet: "xAI".into(),
            },
            SearchHit {
                title: "首页 | GROK官网".into(),
                url: "https://grok-cn.top/".into(),
                snippet: "Grok".into(),
            },
        ];
        let ranked = rank_hits(hits, "Grok xAI latest news", 6);
        assert!(ranked.iter().all(|hit| !hit.url.contains("grok-cn.top")));
    }

    #[test]
    fn drops_unrelated_chinese_pages_for_english_query() {
        let hits = vec![
            SearchHit {
                title: "冒险岛游戏金币不够用怎么办".into(),
                url: "https://zhidao.baidu.com/question/123".into(),
                snippet: "2020-03-12".into(),
            },
            SearchHit {
                title: "秀米编辑器怎么设置背景图".into(),
                url: "https://jingyan.baidu.com/article/456".into(),
                snippet: "2018-10-03".into(),
            },
            SearchHit {
                title: "如何将秀米的内容导出到微信公众号".into(),
                url: "https://www.zhihu.com/question/789".into(),
                snippet: "2021-01-19".into(),
            },
        ];
        let ranked = rank_hits(hits, "latest Grok news", 6);
        assert!(ranked.is_empty());
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
    fn parses_bing_results_and_drops_tracking_host() {
        let html = r#"
            <li class="b_algo">
              <h2><a href="https://www.bing.com/ck/a?!&&p=abc&amp;u=a1aHR0cHM6Ly94LmFpL2Jsb2cvZ3Jvay&amp;ntb=1">xAI Grok news</a></h2>
              <div class="b_caption"><p>Official Grok update from xAI.</p></div>
            </li>
            <li class="b_algo">
              <h2><a class="tilk" href="https://help.x.com/en/using-x/about-grok">About Grok</a></h2>
              <div class="b_caption"><p>Help Center</p></div>
            </li>
        "#;
        let hits = parse_bing_html(html);
        assert!(hits.iter().any(|hit| hit.url == "https://x.ai/blog/grok"));
        assert!(hits.iter().any(|hit| hit.url.contains("help.x.com")));
        assert!(hits.iter().all(|hit| !hit.url.contains("bing.com")));
    }

    #[test]
    fn latin_queries_force_us_bing_market() {
        let (mkt, lang, cc) = bing_market("latest Grok news", "cn-zh");
        assert_eq!(
            (mkt.as_str(), lang.as_str(), cc.as_str()),
            ("en-US", "en", "US")
        );
        let (mkt, lang, cc) = bing_market("北京 天气", "cn-zh");
        assert_eq!(
            (mkt.as_str(), lang.as_str(), cc.as_str()),
            ("zh-CN", "zh", "CN")
        );
    }

    #[test]
    fn drops_search_engine_host_urls() {
        assert!(is_junk_url("https://www.bing.com/ck/a?!&&p=abc&u=missing"));
        assert!(is_junk_url("https://cn.bing.com/search?q=x"));
    }

    #[tokio::test]
    #[ignore = "hits the network"]
    async fn live_search_returns_hits() {
        let skills = AgentSkills::default();
        let (hits, engine) = search_web("latest Grok news", &skills, WebSearchKind::Web)
            .await
            .expect("search");
        assert!(!hits.is_empty(), "engine={engine}");
        assert!(
            engine.contains("duckduckgo") || engine.contains("bing"),
            "expected DuckDuckGo or international Bing, got {engine}"
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
