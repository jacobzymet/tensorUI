use std::{future::Future, io, pin::Pin, time::Duration};

use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::{
    anthropic::{self, AnthropicSseTranslator},
    http,
    providers::{self, ApiStyle},
};

/// Generation on CPU can be very slow (see README), so allow a generous
/// window for the whole streamed response rather than a short request timeout.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub(crate) const CHANNEL_CAPACITY: usize = 32;

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
        while !worker_done {
            tokio::select! {
                item = rx.recv() => {
                    match item {
                        Some(frame) => yield frame,
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

pub fn stream_completion(
    api_base: &str,
    api_key: Option<&str>,
    mut payload: serde_json::Value,
) -> ChatStream {
    let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
    let token = api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .unwrap_or("")
        .to_string();
    let api_base = api_base.to_string();
    stream_from_worker(move |tx| async move {
        if let Some(object) = payload.as_object_mut() {
            object.insert("stream".into(), serde_json::json!(true));
            object
                .entry("model")
                .or_insert_with(|| serde_json::json!("local"));
        }
        proxy_openai_sse(&api_base, &url, &token, &payload, &tx, "llama-server").await
    })
}

pub fn stream_remote_completion(
    api_base: &str,
    token: &str,
    style: ApiStyle,
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
                let url = format!("{api_base}/chat/completions");
                proxy_openai_sse(&api_base, &url, &token, &payload, &tx, "remote LLM").await
            }
            ApiStyle::Anthropic => {
                let url = format!("{api_base}/messages");
                let anth =
                    anthropic::openai_to_anthropic_messages(&payload).map_err(StreamFail::Other)?;
                proxy_anthropic_sse(&api_base, &url, &token, &anth, &tx).await
            }
        }
    })
}

pub(crate) async fn open_llm_sse(
    api_base: &str,
    url: &str,
    style: ApiStyle,
    token: &str,
    payload: &serde_json::Value,
) -> Result<reqwest::Response, StreamFail> {
    let client = http::llm_client(api_base, REQUEST_TIMEOUT);
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
        let body = response.text().await.unwrap_or_default();
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
) -> Result<(), StreamFail> {
    let response = match open_llm_sse(api_base, url, ApiStyle::Openai, token, payload).await {
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
) -> Result<(), StreamFail> {
    let response = open_llm_sse(api_base, url, ApiStyle::Anthropic, token, payload).await?;
    let mut byte_stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut translator = AnthropicSseTranslator::default();

    while let Some(next) = byte_stream.next().await {
        let chunk = next.map_err(|error| StreamFail::Other(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buffer.find('\n') {
            let mut line = buffer[..idx].to_string();
            buffer.drain(..=idx);
            if line.ends_with('\r') {
                line.pop();
            }
            for frame in translator.push_line(&line).map_err(StreamFail::Other)? {
                send_sse(tx, frame).await?;
            }
            if translator.is_finished() {
                send_sse(tx, translator.finish_frames()).await?;
                return Ok(());
            }
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

const TITLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Ask the active provider for a short session title from the first user message.
pub async fn generate_chat_title(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    model: Option<&str>,
    user_message: &str,
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
    // Tiny completion only — never invite reasoning. Title failure must not
    // affect the main chat; prefer keeping the provisional slug over a CoT leak.
    let mut payload = serde_json::json!({
        "model": model_name,
        "stream": false,
        "max_tokens": 24,
        "temperature": 0.2,
        "messages": [
            {
                "role": "system",
                "content": "Reply with a chat title only. Maximum 6 words. No quotes, markdown, punctuation, or explanation."
            },
            {
                "role": "user",
                "content": format!("Title this chat:\n\n{snippet}")
            }
        ]
    });
    // OpenAI-compat / local templates: ask the host to skip thinking. Anthropic
    // conversion ignores these and simply omits a thinking block for "none".
    if let Some(object) = payload.as_object_mut() {
        object.insert("reasoning_effort".into(), serde_json::json!("none"));
        object.insert(
            "chat_template_kwargs".into(),
            serde_json::json!({ "enable_thinking": false }),
        );
    }

    let value = post_title_completion(api_base, token, style, &payload).await?;
    let raw = match style {
        ApiStyle::Openai => extract_openai_title_text(&value),
        ApiStyle::Anthropic => extract_anthropic_text(&value),
    };
    sanitize_chat_title(&raw).ok_or_else(|| {
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
) -> Result<serde_json::Value, String> {
    let client = http::llm_client(api_base, TITLE_TIMEOUT);
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
            let mut request = client.post(&url).json(&body);
            for (name, value) in providers::provider_auth_headers(style, &token) {
                request = request.header(name, value);
            }
            request.send().await.map_err(|error| error.to_string())
        }
    };

    let response = send(body.clone()).await?;
    let response = if style == ApiStyle::Openai
        && response.status().as_u16() == 400
        && payload.get("reasoning_effort").is_some()
    {
        // Some strict OpenAI-compat hosts reject unknown reasoning fields —
        // retry the same tiny request without them rather than failing the title.
        let mut bare = payload.clone();
        if let Some(object) = bare.as_object_mut() {
            object.remove("reasoning_effort");
            object.remove("chat_template_kwargs");
        }
        send(bare).await?
    } else {
        response
    };

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("title request failed ({status}): {text}"));
    }
    response.json().await.map_err(|error| error.to_string())
}

fn truncate_for_error(raw: &str) -> String {
    let compact = raw.replace('\n', "\\n");
    if compact.chars().count() <= 120 {
        compact
    } else {
        format!("{}…", compact.chars().take(120).collect::<String>())
    }
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
    // Prefer the last short line — models often put the title after leftover prose.
    let title = candidates.pop()?;
    let truncated: String = title.chars().take(60).collect();
    Some(if truncated.chars().count() < title.chars().count() {
        format!("{}…", truncated.trim_end())
    } else {
        truncated
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
    use super::{extract_openai_title_text, sanitize_chat_title};
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
}
