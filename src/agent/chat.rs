use std::{future::Future, io, pin::Pin, time::Duration};

use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::{
    anthropic::{self, AnthropicSseTranslator},
    http,
    providers::{self, ApiStyle, RemoteModelOption},
};

/// Generation on CPU can be very slow (see README), so allow a generous
/// window for the whole streamed response rather than a short request timeout.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub(crate) const CHANNEL_CAPACITY: usize = 32;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;
const MAX_SSE_LINE_BYTES: usize = 1024 * 1024;
#[cfg(not(test))]
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5 * 60);
#[cfg(test)]
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(40);

/// SSE byte frames. Work runs as a child of this stream: client disconnect
/// drops the body → drops the worker → drops the upstream HTTP response.
pub type ChatStream = Pin<Box<dyn futures_util::Stream<Item = Result<Vec<u8>, io::Error>> + Send>>;

#[derive(Debug)]
pub(crate) enum StreamFail {
    /// Consumer stopped taking frames (disconnect / backpressure drop).
    Cancelled,
    Other(String),
}

impl StreamFail {
    pub(crate) fn into_message(self) -> Option<String> {
        match self {
            Self::Cancelled => None,
            Self::Other(message) => Some(message),
        }
    }
}

/// Run `worker` as a child future of the response stream.
/// Frames go through a channel only for nested `yield`; cancel is drop-based.
pub(crate) fn stream_from_worker<F, Fut>(worker: F) -> ChatStream
where
    F: FnOnce(mpsc::Sender<Result<Vec<u8>, io::Error>>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), StreamFail>> + Send + 'static,
{
    Box::pin(async_stream::stream! {
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut worker = std::pin::pin!(worker(tx));
        let mut worker_done = false;
        let mut received_frame = false;
        let first_frame_timeout = tokio::time::sleep(FIRST_FRAME_TIMEOUT);
        tokio::pin!(first_frame_timeout);
        while !worker_done {
            tokio::select! {
                _ = &mut first_frame_timeout, if !received_frame => {
                    yield Ok(sse_error("The model did not start responding within 5 minutes"));
                    worker_done = true;
                }
                item = rx.recv() => {
                    match item {
                        Some(frame) => {
                            received_frame = true;
                            yield frame;
                        }
                        None => worker_done = true,
                    }
                }
                result = &mut worker => {
                    worker_done = true;
                    if let Err(fail) = result
                        && let Some(message) = fail.into_message()
                    {
                        yield Ok(sse_error(&message));
                    }
                }
            }
        }
        while let Ok(frame) = rx.try_recv() {
            yield frame;
        }
    })
}

pub fn stream_remote_completion(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    allow_insecure_tls: bool,
    mut payload: serde_json::Value,
) -> ChatStream {
    let api_base = api_base.trim_end_matches('/').to_string();
    let token = token.trim().to_string();
    stream_from_worker(move |tx| async move {
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".into(), serde_json::json!(true));
            object.remove("agent");
            object.remove("skills");
            object
                .entry("model")
                .or_insert_with(|| serde_json::json!("local"));
        }
        match style {
            ApiStyle::Openai => {
                if let Some(object) = payload.as_object_mut() {
                    object.insert(
                        "stream_options".into(),
                        serde_json::json!({ "include_usage": true }),
                    );
                }
                let url = format!("{api_base}/chat/completions");
                proxy_openai_sse(
                    &api_base,
                    &url,
                    &token,
                    &payload,
                    &tx,
                    "remote LLM",
                    allow_insecure_tls,
                )
                .await
            }
            ApiStyle::Anthropic => {
                let url = format!("{api_base}/messages");
                let anth =
                    anthropic::openai_to_anthropic_messages(&payload).map_err(StreamFail::Other)?;
                proxy_anthropic_sse(&api_base, &url, &token, &anth, &tx, allow_insecure_tls).await
            }
        }
    })
}

pub(crate) async fn open_llm_sse(
    _api_base: &str,
    url: &str,
    style: ApiStyle,
    token: &str,
    payload: &serde_json::Value,
    allow_insecure_tls: bool,
) -> Result<reqwest::Response, StreamFail> {
    let client = http::llm_client(REQUEST_TIMEOUT, allow_insecure_tls);
    let mut request = client.post(url).json(payload);
    for (name, value) in providers::provider_auth_headers(style, token) {
        request = request.header(name, value);
    }

    let response = request
        .send()
        .await
        .map_err(|error| StreamFail::Other(error.to_string()))?;

    if response.status() != reqwest::StatusCode::OK {
        let status = response.status();
        let body = response_text_limited(response).await;
        return Err(StreamFail::Other(format!(
            "LLM API responded with {status}: {body}"
        )));
    }
    Ok(response)
}

async fn proxy_openai_sse(
    api_base: &str,
    url: &str,
    token: &str,
    payload: &serde_json::Value,
    tx: &mpsc::Sender<Result<Vec<u8>, io::Error>>,
    upstream_label: &str,
    allow_insecure_tls: bool,
) -> Result<(), StreamFail> {
    let response = match open_llm_sse(
        api_base,
        url,
        ApiStyle::Openai,
        token,
        payload,
        allow_insecure_tls,
    )
    .await
    {
        Ok(response) => response,
        Err(StreamFail::Other(message)) => {
            return Err(StreamFail::Other(message.replacen(
                "LLM API",
                upstream_label,
                1,
            )));
        }
        Err(StreamFail::Cancelled) => return Err(StreamFail::Cancelled),
    };
    forward_raw_sse(response, tx).await
}

async fn proxy_anthropic_sse(
    api_base: &str,
    url: &str,
    token: &str,
    payload: &serde_json::Value,
    tx: &mpsc::Sender<Result<Vec<u8>, io::Error>>,
    allow_insecure_tls: bool,
) -> Result<(), StreamFail> {
    let response = open_llm_sse(
        api_base,
        url,
        ApiStyle::Anthropic,
        token,
        payload,
        allow_insecure_tls,
    )
    .await?;
    let mut byte_stream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut translator = AnthropicSseTranslator::default();

    while let Some(next) = byte_stream.next().await {
        let chunk = next.map_err(|error| StreamFail::Other(error.to_string()))?;
        buffer.extend_from_slice(&chunk);
        let mut consumed = 0;
        while let Some(relative) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            if relative > MAX_SSE_LINE_BYTES {
                return Err(StreamFail::Other(
                    "Model SSE line exceeded the safety limit.".into(),
                ));
            }
            let end = consumed + relative;
            let mut line = &buffer[consumed..end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let line = String::from_utf8_lossy(line);
            for frame in translator.push_line(&line).map_err(StreamFail::Other)? {
                send_sse(tx, frame).await?;
            }
            if translator.is_finished() {
                send_sse(tx, translator.finish_frames()).await?;
                return Ok(());
            }
            consumed = end + 1;
        }
        if consumed != 0 {
            buffer = buffer.split_off(consumed);
        }
        if buffer.len() > MAX_SSE_LINE_BYTES {
            return Err(StreamFail::Other(
                "Model SSE line exceeded the safety limit.".into(),
            ));
        }
    }
    if !buffer.is_empty() {
        let line = String::from_utf8_lossy(&buffer);
        for frame in translator
            .push_line(line.trim_end_matches('\r'))
            .map_err(StreamFail::Other)?
        {
            send_sse(tx, frame).await?;
        }
    }
    send_sse(tx, translator.finish_frames()).await?;
    Ok(())
}

async fn forward_raw_sse(
    response: reqwest::Response,
    tx: &mpsc::Sender<Result<Vec<u8>, io::Error>>,
) -> Result<(), StreamFail> {
    let mut stream = response.bytes_stream();
    while let Some(next) = stream.next().await {
        match next {
            Ok(chunk) => send_sse(tx, chunk.to_vec()).await?,
            Err(error) => return Err(StreamFail::Other(error.to_string())),
        }
    }
    Ok(())
}

pub(crate) fn sse_error(message: &str) -> Vec<u8> {
    let payload = serde_json::json!({ "error": message });
    format!("event: error\ndata: {payload}\n\n").into_bytes()
}

pub(crate) async fn send_sse(
    tx: &mpsc::Sender<Result<Vec<u8>, io::Error>>,
    frame: Vec<u8>,
) -> Result<(), StreamFail> {
    if tx.send(Ok(frame)).await.is_err() {
        Err(StreamFail::Cancelled)
    } else {
        Ok(())
    }
}

const TITLE_TIMEOUT: Duration = Duration::from_secs(45);
const TITLE_MAX_TOKENS: u32 = 192;

/// Ask the active provider for a short session title from the first user message.
pub async fn generate_chat_title(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    model: Option<&str>,
    user_message: &str,
    allow_insecure_tls: bool,
    thinking_model: Option<&RemoteModelOption>,
) -> Result<String, String> {
    let api_base = api_base.trim_end_matches('/');
    let snippet: String = user_message.chars().take(240).collect();
    if snippet.trim().is_empty() {
        return Err("message is empty".into());
    }
    let model_name = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("local");
    // Keep the request small, but leave room for hosts that still spend
    // tokens on hidden reasoning before the title line.
    let mut payload = serde_json::json!({
        "model": model_name,
        "stream": false,
        "max_tokens": TITLE_MAX_TOKENS,
        "temperature": 0,
        "thinking_effort": "off",
        "messages": [
            {
                "role": "system",
                "content": crate::prompts::trim_prompt(crate::prompts::title::SYSTEM)
            },
            {
                "role": "user",
                "content": crate::prompts::fill(
                    crate::prompts::title::USER,
                    &[("snippet", &snippet)]
                )
            }
        ]
    });
    providers::apply_thinking_control(&mut payload, thinking_model);
    // Catalog Off is a no-op when thinking is mandatory. Still try the
    // common disable knobs so a 6-word title is not eaten by CoT.
    if payload.get("reasoning").is_none()
        && payload.get("reasoning_effort").is_none()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("reasoning_effort".into(), serde_json::json!("none"));
        object.insert("reasoning".into(), serde_json::json!({ "effort": "none" }));
        object.insert(
            "chat_template_kwargs".into(),
            serde_json::json!({ "enable_thinking": false }),
        );
    }

    let value = post_title_completion(api_base, token, style, &payload, allow_insecure_tls).await?;
    let raw = match style {
        ApiStyle::Openai => extract_openai_title_text(&value),
        ApiStyle::Anthropic => extract_anthropic_text(&value),
    };
    sanitize_chat_title(&raw)
        .or_else(|| sanitize_chat_title(&extract_openai_reasoning_text(&value)))
        .ok_or_else(|| {
            format!(
                "model returned an empty title (raw={})",
                truncate_for_error(&raw)
            )
        })
}

async fn post_title_completion(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    payload: &serde_json::Value,
    allow_insecure_tls: bool,
) -> Result<serde_json::Value, String> {
    let client = http::llm_client(TITLE_TIMEOUT, allow_insecure_tls);
    let (url, body) = match style {
        ApiStyle::Openai => (format!("{api_base}/chat/completions"), payload.clone()),
        ApiStyle::Anthropic => (
            format!("{api_base}/messages"),
            anthropic::openai_to_anthropic_messages(payload)?,
        ),
    };

    let send = |body: serde_json::Value| {
        let client = client.clone();
        let url = url.clone();
        let token = token.to_string();
        async move {
            let mut request = client.post(&url).timeout(TITLE_TIMEOUT).json(&body);
            for (name, value) in providers::provider_auth_headers(style, &token) {
                request = request.header(name, value);
            }
            request.send().await.map_err(|error| error.to_string())
        }
    };

    let response = send(body.clone()).await?;
    let has_thinking_knobs = payload.as_object().is_some_and(|object| {
        object.contains_key("reasoning_effort")
            || object.contains_key("reasoning")
            || object.contains_key("chat_template_kwargs")
            || object.contains_key("thinking_effort")
    });
    let response =
        if style == ApiStyle::Openai && response.status().as_u16() == 400 && has_thinking_knobs {
            // Some strict OpenAI-compat hosts reject unknown reasoning fields —
            // retry the same tiny request without them rather than failing the title.
            let mut bare = payload.clone();
            if let Some(object) = bare.as_object_mut() {
                object.remove("reasoning_effort");
                object.remove("chat_template_kwargs");
                object.remove("reasoning");
                object.remove("thinking_effort");
            }
            send(bare).await?
        } else {
            response
        };

    if !response.status().is_success() {
        let status = response.status();
        let text = response_text_limited(response).await;
        return Err(format!("title request failed ({status}): {text}"));
    }
    let text = response_text_limited(response).await;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

fn truncate_for_error(raw: &str) -> String {
    let compact = raw.replace('\n', "\\n");
    if compact.chars().count() <= 120 {
        compact
    } else {
        format!("{}…", compact.chars().take(120).collect::<String>())
    }
}

async fn response_text_limited(response: reqwest::Response) -> String {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::with_capacity(MAX_ERROR_BODY_BYTES);
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        let take = remaining.min(chunk.len());
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn extract_openai_reasoning_text(value: &serde_json::Value) -> String {
    let Some(message) = value.pointer("/choices/0/message") else {
        return String::new();
    };
    for key in ["reasoning_content", "reasoning", "thinking"] {
        let raw = match message.get(key) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Object(map)) => map
                .get("content")
                .or_else(|| map.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            Some(other) => json_content_to_text(other),
            None => continue,
        };
        let stripped = strip_think_blocks(&raw);
        if !stripped.trim().is_empty() {
            return stripped;
        }
    }
    String::new()
}

fn extract_openai_title_text(value: &serde_json::Value) -> String {
    let message = value.pointer("/choices/0/message");
    let mut text = message
        .map(|msg| json_content_to_text(msg.get("content").unwrap_or(&serde_json::Value::Null)))
        .unwrap_or_default();
    // Never fall back to reasoning/thinking fields — those paraphrase the title
    // prompt ("The user wants a short chat title…") and leak into the sidebar.
    if text.trim().is_empty()
        && let Some(legacy) = value.pointer("/choices/0/text").and_then(|v| v.as_str())
    {
        text = legacy.to_string();
    }
    strip_think_blocks(&text)
}

fn json_content_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.as_str() {
                    return Some(text.to_string());
                }
                let ty = part.get("type").and_then(|v| v.as_str()).unwrap_or("text");
                if matches!(ty, "text" | "output_text") {
                    part.get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn extract_anthropic_text(value: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let Some(items) = value.get("content").and_then(|v| v.as_array()) {
        for item in items {
            let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(ty, "text" | "output_text")
                && let Some(text) = item.get("text").and_then(|v| v.as_str())
            {
                parts.push(text);
            }
        }
    }
    strip_think_blocks(&parts.join(""))
}

fn strip_think_blocks(raw: &str) -> String {
    let mut out = raw.to_string();
    // Closed and unclosed think / reasoning tags from local templates.
    for (open, close) in [
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reason>", "</reason>"),
        ("<reasoning>", "</reasoning>"),
    ] {
        while let Some(start) = out.find(open) {
            if let Some(rel_end) = out[start + open.len()..].find(close) {
                let end = start + open.len() + rel_end + close.len();
                out.replace_range(start..end, " ");
            } else {
                out.replace_range(start.., " ");
                break;
            }
        }
    }
    out
}

fn sanitize_chat_title(raw: &str) -> Option<String> {
    let cleaned = strip_think_blocks(raw);
    let mut candidates = Vec::new();
    for line in cleaned.lines() {
        let mut title = line.trim().to_string();
        if title.is_empty() || title.starts_with('<') {
            continue;
        }
        title = title
            .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*' | '#' | '“' | '”' | '‘' | '’'))
            .trim()
            .to_string();
        for prefix in ["Title:", "title:", "Chat title:", "CHAT TITLE:"] {
            if let Some(rest) = title.strip_prefix(prefix) {
                title = rest.trim().to_string();
            }
        }
        title = title
            .trim_end_matches(['.', '!', '?', ':', ';'])
            .trim()
            .to_string();
        if title.is_empty() || title_looks_like_prompt_echo(&title) {
            continue;
        }
        // Titles are ≤6 words by contract; allow a little slack, reject prose.
        if title.split_whitespace().count() > 8 {
            continue;
        }
        candidates.push(title);
    }
    if candidates.is_empty()
        && let Some(fragment) = last_short_title_fragment(&cleaned)
    {
        candidates.push(fragment);
    }
    // Prefer the last short line — models often put the title after leftover prose.
    let title = candidates.pop()?;
    let truncated: String = title.chars().take(60).collect();
    Some(if truncated.chars().count() < title.chars().count() {
        format!("{}…", truncated.trim_end())
    } else {
        truncated
    })
}

fn last_short_title_fragment(text: &str) -> Option<String> {
    let compact = text.replace(['\n', '\r'], " ");
    compact.split(['.', '!', '?', ';']).rev().find_map(|part| {
        let mut title = part
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*' | '#' | '“' | '”' | '‘' | '’'))
            .trim()
            .to_string();
        for prefix in ["Title:", "title:", "Chat title:", "CHAT TITLE:"] {
            if let Some(rest) = title.strip_prefix(prefix) {
                title = rest.trim().to_string();
            }
        }
        title = title
            .trim_end_matches(['.', '!', '?', ':', ';'])
            .trim()
            .to_string();
        if title.is_empty() || title_looks_like_prompt_echo(&title) {
            return None;
        }
        let words = title.split_whitespace().count();
        (words > 0 && words <= 8).then_some(title)
    })
}

fn title_looks_like_prompt_echo(title: &str) -> bool {
    let lower = title.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "short chat title",
        "at most 6",
        "at most six",
        "no quotes",
        "no markdown",
        "no explanation",
        "the user wants",
        "write a title",
        "title this chat",
        "reply with",
        "only the title",
        "maximum 6",
    ];
    MARKERS.iter().any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod title_tests {
    use super::{
        extract_openai_reasoning_text, extract_openai_title_text, sanitize_chat_title,
        stream_from_worker,
    };
    use futures_util::StreamExt;
    use serde_json::json;

    #[test]
    fn cleans_quoted_titles() {
        assert_eq!(
            sanitize_chat_title("  \"Rust async tips\"\n").as_deref(),
            Some("Rust async tips")
        );
        assert_eq!(
            sanitize_chat_title("Title: Debugging SSE streams.").as_deref(),
            Some("Debugging SSE streams")
        );
    }

    #[test]
    fn strips_think_blocks_before_title() {
        assert_eq!(
            sanitize_chat_title("<think>plan</think>\nMorning greeting").as_deref(),
            Some("Morning greeting")
        );
    }

    #[test]
    fn rejects_reasoning_prompt_echo_as_title() {
        assert_eq!(
            sanitize_chat_title(
                "The user wants a short chat title, at most 6 words, no explanation"
            ),
            None
        );
        let value = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "The user wants a short chat title, at most 6 words"
                }
            }]
        });
        assert_eq!(
            sanitize_chat_title(&extract_openai_title_text(&value)),
            None
        );
    }

    #[test]
    fn prefers_title_line_after_prose() {
        assert_eq!(
            sanitize_chat_title("Sure, here you go\nRust async tips").as_deref(),
            Some("Rust async tips")
        );
    }

    #[test]
    fn pulls_short_title_from_trailing_sentence() {
        assert_eq!(
            sanitize_chat_title(
                "The user asked whether DeepSeek Flash supports PDFs natively. DeepSeek Flash PDF Support"
            )
            .as_deref(),
            Some("DeepSeek Flash PDF Support")
        );
    }

    #[test]
    fn uses_reasoning_text_when_content_is_empty() {
        let value = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "The user asked about PDF support.\nDeepSeek Flash PDF Support"
                }
            }]
        });
        assert_eq!(extract_openai_title_text(&value).trim(), "");
        assert_eq!(
            sanitize_chat_title(&extract_openai_reasoning_text(&value)).as_deref(),
            Some("DeepSeek Flash PDF Support")
        );
    }

    #[test]
    fn extracts_array_content_parts() {
        let value = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": [{ "type": "text", "text": "Morning greeting" }]
                }
            }]
        });
        assert_eq!(
            sanitize_chat_title(&extract_openai_title_text(&value)).as_deref(),
            Some("Morning greeting")
        );
    }

    #[tokio::test]
    async fn silent_worker_ends_with_a_visible_timeout_error() {
        let stream = stream_from_worker(|tx| async move {
            futures_util::future::pending::<()>().await;
            drop(tx);
            Ok(())
        });
        let joined = stream
            .map(|frame| String::from_utf8_lossy(&frame.expect("frame")).into_owned())
            .collect::<String>()
            .await;
        assert!(joined.contains("did not start responding"));
    }
}
