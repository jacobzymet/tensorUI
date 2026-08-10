//! Agent mode: OpenAI-compatible `tool_calls` + `role: tool` (Anthropic via translator).
//! XML `<tool_call>` in content is accepted only as a fallback for local models.

pub mod chat;
pub mod skills;

use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    anthropic::{self, AnthropicSseTranslator},
    chat::{ChatStream, StreamFail, open_llm_sse, send_sse, stream_from_worker},
    http,
    providers::ApiStyle,
    skills::UserSkill,
};

const MAX_AGENT_ROUNDS: usize = 6;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(25);
const PAGE_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_SEARCH_RESULTS: usize = 6;
const MAX_PAGE_BYTES: u64 = 1_500_000;
const FETCH_URL_MAX_CHARS: usize = 8_000;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRequest {
    pub messages: Vec<Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent: bool,
    #[serde(default)]
    pub skills: AgentSkills,
    #[serde(default)]
    pub chat_template_kwargs: Option<Value>,
    #[serde(default)]
    pub thinking_budget_tokens: Option<i64>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchDepth {
    Off,
    #[default]
    Auto,
    Light,
    Standard,
    Deep,
}

impl WebSearchDepth {
    fn scrape_plan(self) -> Option<(usize, usize)> {
        match self {
            Self::Off => None,
            Self::Auto => Some((3, 2800)),
            Self::Light => Some((2, 1600)),
            Self::Standard => Some((4, 3200)),
            Self::Deep => Some((6, 4800)),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Standard => "standard",
            Self::Deep => "deep",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentSkills {
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub web_search_depth: WebSearchDepth,
    #[serde(default)]
    pub fetch_url: bool,
}

impl AgentSkills {
    pub fn any_enabled(&self) -> bool {
        self.web_search || self.fetch_url
    }
}

#[derive(Debug, Clone)]
struct ToolCall {
    id: String,
    name: String,
    arguments: Value,
}

#[derive(Debug, Clone)]
struct StreamedTurn {
    content: String,
    reasoning: String,
    tool: Option<ToolCall>,
}

impl StreamedTurn {
    fn assistant_text(&self) -> String {
        if self.reasoning.is_empty() {
            self.content.clone()
        } else if self.content.is_empty() {
            format!("<think>{}</think>", self.reasoning)
        } else {
            format!("<think>{}</think>\n{}", self.reasoning, self.content)
        }
    }
}

fn append_openai_tool_exchange(
    messages: &mut Vec<Value>,
    turn: &StreamedTurn,
    call: &ToolCall,
    result: &str,
) {
    let content = if turn.content.trim().is_empty() {
        Value::Null
    } else {
        Value::String(turn.content.clone())
    };
    let arguments = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
    messages.push(json!({
        "role": "assistant",
        "content": content,
        "tool_calls": [{
            "id": call.id,
            "type": "function",
            "function": {
                "name": call.name,
                "arguments": arguments
            }
        }]
    }));
    messages.push(json!({
        "role": "tool",
        "tool_call_id": call.id,
        "name": call.name,
        "content": result
    }));
}

fn new_tool_call_id() -> String {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "call_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

pub fn stream_agent(
    api_base: &str,
    api_key: Option<&str>,
    style: ApiStyle,
    mut request: AgentRequest,
    user_skills: Vec<UserSkill>,
) -> ChatStream {
    let api_base = api_base.trim_end_matches('/').to_string();
    let api_key = api_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    stream_from_worker(move |tx| async move {
        run_agent_loop(
            &api_base,
            api_key.as_deref(),
            style,
            &mut request,
            &user_skills,
            &tx,
        )
        .await
    })
}

async fn run_agent_loop(
    api_base: &str,
    api_key: Option<&str>,
    style: ApiStyle,
    request: &mut AgentRequest,
    user_skills: &[UserSkill],
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail> {
    inject_agent_system_prompt(&mut request.messages, &request.skills, user_skills);
    send_sse(
        tx,
        sse_agent(json!({
            "phase": "status",
            "message": "Agent mode"
        })),
    )
    .await?;

    for round in 0..MAX_AGENT_ROUNDS {
        send_sse(
            tx,
            sse_agent(json!({
                "phase": "status",
                "message": if round == 0 { "Processing…" } else { "Continuing…" }
            })),
        )
        .await?;

        let turn = stream_once(api_base, api_key, style, request, user_skills, tx).await?;

        if let Some(call) = turn.tool.clone() {
            if !capability_allowed(&call.name, &request.skills, user_skills) {
                return Err(StreamFail::Other(format!(
                    "Capability '{}' is not enabled.",
                    call.name
                )));
            }

            send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "tool_call",
                    "name": call.name,
                    "arguments": call.arguments,
                })),
            )
            .await?;

            let outcome = execute_tool(&call, &request.skills, user_skills)
                .await
                .map_err(StreamFail::Other)?;
            let preview = outcome.ui_text.chars().take(240).collect::<String>();
            // Cap the UI payload; the model still receives the full `result` below.
            let ui_result: String = outcome.ui_text.chars().take(32_000).collect();
            let mut payload = json!({
                "phase": "tool_result",
                "name": call.name,
                "ok": true,
                "preview": preview,
                "result": ui_result,
            });
            if let Some(note) = &outcome.note {
                payload["note"] = json!(note);
            }
            send_sse(tx, sse_agent(payload)).await?;

            append_openai_tool_exchange(&mut request.messages, &turn, &call, &outcome.text);
            continue;
        }

        // Reasoning-only / empty visible replies used to end the turn blank in the UI.
        // Nudge the model to either call a tool or answer plainly.
        if turn.content.trim().is_empty() {
            send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "status",
                    "message": "Retrying…"
                })),
            )
            .await?;
            request.messages.push(json!({
                "role": "assistant",
                "content": turn.assistant_text(),
            }));
            request.messages.push(json!({
                "role": "user",
                "content": "You did not produce a user-visible answer or a tool call. Call a tool if you need a capability, or answer the user directly.",
            }));
            continue;
        }

        send_sse(tx, b"data: [DONE]\n\n".to_vec()).await?;
        return Ok(());
    }

    Err(StreamFail::Other(
        "Agent stopped after too many tool rounds.".into(),
    ))
}

fn capability_allowed(name: &str, skills: &AgentSkills, user_skills: &[UserSkill]) -> bool {
    match name {
        "web_search" => skills.web_search,
        "fetch_url" => skills.fetch_url,
        "activate_skill" | "read_skill" => !user_skills.is_empty(),
        _ => false,
    }
}

pub fn should_run_agent(request: &AgentRequest, user_skills: &[UserSkill]) -> bool {
    request.agent && (request.skills.any_enabled() || !user_skills.is_empty())
}

fn inject_agent_system_prompt(
    messages: &mut Vec<Value>,
    skills: &AgentSkills,
    user_skills: &[UserSkill],
) {
    let block = agent_system_block(skills, user_skills);
    if block.is_empty() {
        return;
    }
    if let Some(first) = messages.first_mut()
        && first.get("role").and_then(|r| r.as_str()) == Some("system")
        && let Some(content) = first.get("content").and_then(|c| c.as_str())
    {
        let merged = format!("{content}\n\n{block}");
        first
            .as_object_mut()
            .unwrap()
            .insert("content".into(), Value::String(merged));
        return;
    }
    messages.insert(0, json!({ "role": "system", "content": block }));
}

fn agent_system_block(skills: &AgentSkills, user_skills: &[UserSkill]) -> String {
    let mut lines: Vec<String> = vec![
        "You are running in agent mode with tools.".into(),
        "Call a tool when it helps; do not invent tool results. After a tool result, call another tool or answer normally.".into(),
    ];
    if skills.web_search {
        let depth = skills.web_search_depth;
        let depth_note = match depth.scrape_plan() {
            None => {
                "Returns titles, URLs, and snippets only (page fetch depth is off).".to_string()
            }
            Some((pages, chars)) => format!(
                "Also opens up to {pages} result pages and extracts ~{chars} characters of text each (depth: {}).",
                depth.label()
            ),
        };
        let mut web_line =
            format!("Tool web_search — search the public web via DuckDuckGo. {depth_note}");
        if skills.fetch_url {
            web_line.push_str(
                " After results arrive, you may call fetch_url on promising http(s) URLs if you need more detail than the snippets/excerpts already provide."
            );
        }
        lines.push(web_line);
    }
    if skills.fetch_url {
        let mut fetch_line = format!(
            "Tool fetch_url — open one http(s) URL and extract readable text (~{FETCH_URL_MAX_CHARS} characters)."
        );
        if skills.web_search {
            fetch_line.push_str(
                " Useful after web_search when a specific result page deserves a fuller read, or when the user already gave a concrete URL."
            );
        } else {
            fetch_line.push_str(" Prefer when the user already gave a concrete URL.");
        }
        lines.push(fetch_line);
    }
    if !user_skills.is_empty() {
        lines.push(
            "Tool activate_skill — load a skill's full SKILL.md. read_skill is an alias.".into(),
        );
        lines.push(crate::skills::user_skills_catalog_block(user_skills));
    }
    lines.join("\n")
}

pub fn inject_skill_catalog_into_messages(messages: &mut Vec<Value>, user_skills: &[UserSkill]) {
    let block = crate::skills::user_skills_catalog_block(user_skills);
    if block.is_empty() {
        return;
    }
    let note = format!(
        "{block}\n\nTo load full skill instructions you need agent mode (toggle Agent, or @web_search / @fetch_url), which exposes activate_skill."
    );
    if let Some(first) = messages.first_mut()
        && first.get("role").and_then(|r| r.as_str()) == Some("system")
        && let Some(content) = first.get("content").and_then(|c| c.as_str())
    {
        let merged = format!("{content}\n\n{note}");
        first
            .as_object_mut()
            .unwrap()
            .insert("content".into(), Value::String(merged));
        return;
    }
    messages.insert(0, json!({ "role": "system", "content": note }));
}

#[derive(Debug, Default, Clone)]
struct AccumToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// With `--jinja`, llama-server may lift `<tool_call>` into native `delta.tool_calls`;
/// we re-synthesize XML for `extract_tool_call`. Once detected, withhold answer deltas
/// and send `content_clear` so the UI does not keep tool XML.
async fn stream_once(
    api_base: &str,
    api_key: Option<&str>,
    style: ApiStyle,
    request: &AgentRequest,
    user_skills: &[UserSkill],
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<StreamedTurn, StreamFail> {
    let model = request
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("local");
    let mut payload = json!({
        "model": model,
        "stream": true,
        "messages": request.messages,
    });
    if let Some(object) = payload.as_object_mut() {
        let tools = openai_tools_payload(&request.skills, user_skills);
        if !tools.is_empty() {
            object.insert("tools".into(), Value::Array(tools));
            if style == ApiStyle::Openai {
                object.insert("tool_choice".into(), json!("auto"));
            }
        }
        if let Some(kwargs) = &request.chat_template_kwargs {
            object.insert("chat_template_kwargs".into(), kwargs.clone());
        }
        if let Some(budget) = request.thinking_budget_tokens {
            object.insert("thinking_budget_tokens".into(), json!(budget));
        }
        if let Some(effort) = &request.reasoning_effort {
            object.insert("reasoning_effort".into(), json!(effort));
        }
    }

    let url = match style {
        ApiStyle::Openai => format!("{api_base}/chat/completions"),
        ApiStyle::Anthropic => format!("{api_base}/messages"),
    };
    let body = match style {
        ApiStyle::Openai => payload,
        ApiStyle::Anthropic => {
            anthropic::openai_to_anthropic_messages(&payload).map_err(StreamFail::Other)?
        }
    };

    let token = api_key.map(str::trim).unwrap_or("");
    let response = open_llm_sse(api_base, &url, style, token, &body).await?;
    let mut byte_stream = response.bytes_stream();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut native_tools: Vec<Option<AccumToolCall>> = Vec::new();
    let mut forwarding = true;

    match style {
        ApiStyle::Openai => {
            consume_openai_sse_agent(
                &mut byte_stream,
                &mut content,
                &mut reasoning,
                &mut native_tools,
                &mut forwarding,
                tx,
            )
            .await?;
        }
        ApiStyle::Anthropic => {
            consume_anthropic_as_openai_agent(
                &mut byte_stream,
                &mut content,
                &mut reasoning,
                &mut native_tools,
                &mut forwarding,
                tx,
            )
            .await?;
        }
    }
    // Dropping the body here (or earlier on cancel) aborts the upstream request.
    drop(byte_stream);

    let turn = resolve_streamed_turn(&reasoning, &content, &native_tools);
    if turn.content.trim().is_empty() && turn.reasoning.trim().is_empty() && turn.tool.is_none() {
        return Err(StreamFail::Other(
            "Model returned no message content.".into(),
        ));
    }
    Ok(turn)
}

async fn consume_openai_sse_agent<S, B>(
    stream: &mut S,
    content: &mut String,
    reasoning: &mut String,
    native_tools: &mut Vec<Option<AccumToolCall>>,
    forwarding: &mut bool,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail>
where
    S: StreamExt<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut buffer = String::new();
    while let Some(next) = stream.next().await {
        let chunk = next.map_err(|error| StreamFail::Other(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(chunk.as_ref()));
        while let Some(idx) = buffer.find('\n') {
            let mut line = buffer[..idx].to_string();
            buffer.drain(..=idx);
            if line.ends_with('\r') {
                line.pop();
            }
            apply_openai_sse_line(&line, content, reasoning, native_tools, forwarding, tx).await?;
        }
    }
    Ok(())
}

async fn consume_anthropic_as_openai_agent<S, B>(
    stream: &mut S,
    content: &mut String,
    reasoning: &mut String,
    native_tools: &mut Vec<Option<AccumToolCall>>,
    forwarding: &mut bool,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail>
where
    S: StreamExt<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut buffer = String::new();
    let mut translator = AnthropicSseTranslator::default();
    while let Some(next) = stream.next().await {
        let chunk = next.map_err(|error| StreamFail::Other(error.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(chunk.as_ref()));
        while let Some(idx) = buffer.find('\n') {
            let mut line = buffer[..idx].to_string();
            buffer.drain(..=idx);
            if line.ends_with('\r') {
                line.pop();
            }
            for frame in translator.push_line(&line).map_err(StreamFail::Other)? {
                apply_openai_sse_frame(&frame, content, reasoning, native_tools, forwarding, tx)
                    .await?;
            }
            if translator.is_finished() {
                return Ok(());
            }
        }
    }
    Ok(())
}

async fn apply_openai_sse_frame(
    frame: &[u8],
    content: &mut String,
    reasoning: &mut String,
    native_tools: &mut Vec<Option<AccumToolCall>>,
    forwarding: &mut bool,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail> {
    let Ok(text) = std::str::from_utf8(frame) else {
        return Ok(());
    };
    for line in text.lines() {
        apply_openai_sse_line(line, content, reasoning, native_tools, forwarding, tx).await?;
    }
    Ok(())
}

async fn apply_openai_sse_line(
    trimmed: &str,
    content: &mut String,
    reasoning: &mut String,
    native_tools: &mut Vec<Option<AccumToolCall>>,
    forwarding: &mut bool,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail> {
    if trimmed.is_empty() || !trimmed.starts_with("data:") {
        return Ok(());
    }
    let data = trimmed[5..].trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Ok(());
    };
    let Some(delta) = value.pointer("/choices/0/delta") else {
        return Ok(());
    };

    if let Some(chunk) =
        delta_string(delta, "reasoning_content").or_else(|| delta_string(delta, "reasoning"))
    {
        reasoning.push_str(chunk);
        if *forwarding {
            let frame = json!({
                "choices": [{ "delta": { "reasoning_content": chunk }, "index": 0 }]
            });
            send_sse(tx, format!("data: {frame}\n\n").into_bytes()).await?;
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            merge_tool_call_delta(native_tools, tc);
        }
        if *forwarding && native_tools.iter().flatten().any(|c| !c.name.is_empty()) {
            *forwarding = false;
            send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
        }
    }

    let Some(chunk) = delta_string(delta, "content") else {
        return Ok(());
    };
    if chunk.is_empty() {
        return Ok(());
    }

    content.push_str(chunk);

    if *forwarding && content.contains("<tool_call>") {
        *forwarding = false;
        send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
        return Ok(());
    }
    if !*forwarding {
        return Ok(());
    }

    let frame = json!({
        "choices": [{ "delta": { "content": chunk }, "index": 0 }]
    });
    send_sse(tx, format!("data: {frame}\n\n").into_bytes()).await
}

fn delta_string<'a>(delta: &'a Value, key: &str) -> Option<&'a str> {
    delta
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn append_tool_arguments(slot: &mut AccumToolCall, args: &Value) {
    match args {
        Value::String(text) => slot.arguments.push_str(text),
        other => {
            slot.arguments = other.to_string();
        }
    }
}

fn merge_tool_call_delta(slots: &mut Vec<Option<AccumToolCall>>, delta: &Value) {
    let index = delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    while slots.len() <= index {
        slots.push(None);
    }
    let slot = slots[index].get_or_insert_with(AccumToolCall::default);

    if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
        let id = id.trim();
        if !id.is_empty() {
            slot.id = id.to_string();
        }
    }

    if let Some(func) = delta.get("function") {
        if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
            let name = name.trim();
            if !name.is_empty() {
                slot.name = name.to_string();
            }
        }
        if let Some(args) = func.get("arguments") {
            append_tool_arguments(slot, args);
        }
    }

    // llama.cpp may flatten name + arguments on the delta.
    if let Some(name) = delta.get("name").and_then(|v| v.as_str()) {
        let name = name.trim();
        if !name.is_empty() {
            slot.name = name.to_string();
        }
    }
    if let Some(args) = delta.get("arguments") {
        append_tool_arguments(slot, args);
    }
}

fn openai_tools_payload(skills: &AgentSkills, user_skills: &[UserSkill]) -> Vec<Value> {
    let mut tools = Vec::new();
    if skills.web_search {
        let mut description = String::from(
            "Search the public web via DuckDuckGo and read result pages. Use when the user wants you to look something up and has not given a specific URL.",
        );
        if skills.fetch_url {
            description.push_str(
                " After you get results, you may call fetch_url on promising result URLs if you need more detail than snippets/excerpts.",
            );
        }
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "web_search",
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search terms"
                        }
                    },
                    "required": ["query"]
                }
            }
        }));
    }
    if skills.fetch_url {
        let description = if skills.web_search {
            "Open one specific http(s) URL and extract readable page text. Use after web_search for a fuller read of a promising result, or when the user already gave a concrete URL."
        } else {
            "Open one specific http(s) URL and extract readable page text. Prefer when the user already gave a concrete URL."
        };
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Absolute http(s) URL to fetch"
                        }
                    },
                    "required": ["url"]
                }
            }
        }));
    }
    if !user_skills.is_empty() {
        let names: Vec<String> = user_skills.iter().map(|skill| skill.name.clone()).collect();
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "activate_skill",
                "description": "Load a skill's full SKILL.md instructions into context. Only activate skills that match the user's request.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": format!(
                                "Skill name or id. Available: {}",
                                names.join(", ")
                            )
                        }
                    },
                    "required": ["name"]
                }
            }
        }));
    }
    tools
}

fn tool_call_from_native(slots: &[Option<AccumToolCall>]) -> Option<ToolCall> {
    let call = slots.iter().flatten().find(|c| !c.name.is_empty())?;
    let arguments = if call.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(&call.arguments)
            .unwrap_or_else(|_| json!({ "raw": call.arguments }))
    };
    Some(ToolCall {
        id: if call.id.trim().is_empty() {
            new_tool_call_id()
        } else {
            call.id.clone()
        },
        name: call.name.clone(),
        arguments,
    })
}

fn resolve_streamed_turn(
    reasoning: &str,
    content: &str,
    native_tools: &[Option<AccumToolCall>],
) -> StreamedTurn {
    let visible = strip_think_blocks(content);
    let tool = tool_call_from_native(native_tools).or_else(|| extract_tool_call(&visible));
    let content = if tool.is_some() {
        // Drop fallback XML from the assistant text channel when we resolved a tool.
        strip_tool_call_xml(&visible)
    } else {
        content.to_string()
    };
    StreamedTurn {
        content,
        reasoning: reasoning.to_string(),
        tool,
    }
}

fn strip_tool_call_xml(text: &str) -> String {
    let mut out = text.to_string();
    while let Some(start) = out.find("<tool_call>") {
        let after = start + "<tool_call>".len();
        if let Some(rel) = out[after..].find("</tool_call>") {
            let end = after + rel + "</tool_call>".len();
            out.replace_range(start..end, "");
        } else {
            out.replace_range(start.., "");
            break;
        }
    }
    out.trim().to_string()
}

fn sse_agent(payload: Value) -> Vec<u8> {
    format!("event: agent\ndata: {payload}\n\n").into_bytes()
}

fn strip_think_blocks(text: &str) -> String {
    let mut out = text.to_string();
    for (open, close) in [("<think>", "</think>"), ("<thinking>", "</thinking>")] {
        while let Some(start) = out.find(open) {
            if let Some(rel) = out[start + open.len()..].find(close) {
                let end = start + open.len() + rel + close.len();
                out.replace_range(start..end, "");
            } else {
                // Unclosed think: drop the opener only. Wiping the rest used to
                // delete a trailing <tool_call> synthesized after partial thoughts.
                out.replace_range(start..start + open.len(), "");
                break;
            }
        }
    }
    out
}

fn extract_tool_call(text: &str) -> Option<ToolCall> {
    let start = text.rfind("<tool_call>")?;
    let after = start + "<tool_call>".len();
    let end = text[after..].find("</tool_call>")? + after;
    let raw = text[after..end].trim();
    let value: Value = serde_json::from_str(raw).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let arguments = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
    Some(ToolCall {
        id: new_tool_call_id(),
        name,
        arguments,
    })
}

struct ToolOutcome {
    /// Full result passed back to the model, including any operational guidance.
    text: String,
    /// User-visible result. This deliberately excludes model-only instructions.
    ui_text: String,
    /// Short UI-facing note (e.g. search backend fallback).
    note: Option<String>,
}

impl ToolOutcome {
    fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            ui_text: text.clone(),
            text,
            note: None,
        }
    }

    fn with_ui_text(
        text: impl Into<String>,
        ui_text: impl Into<String>,
        note: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            ui_text: ui_text.into(),
            note: Some(note.into()),
        }
    }
}

async fn execute_tool(
    call: &ToolCall,
    skills: &AgentSkills,
    user_skills: &[UserSkill],
) -> Result<ToolOutcome, String> {
    match call.name.as_str() {
        "web_search" => {
            let query = call
                .arguments
                .get("query")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "web_search requires a non-empty \"query\" string.".to_string())?;
            let (result, note) = duckduckgo_search(query, skills.web_search_depth).await?;
            let ui_result = result.clone();
            let mut model_result = result;
            if skills.fetch_url {
                model_result.push_str(
                    "\n\nNote: fetch_url is available. If a result URL looks useful and you need more detail than the snippets/excerpts above, call fetch_url on that URL before answering.",
                );
            }
            Ok(ToolOutcome::with_ui_text(model_result, ui_result, note))
        }
        "fetch_url" => {
            let url = call
                .arguments
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "fetch_url requires a non-empty \"url\" string.".to_string())?;
            Ok(ToolOutcome::text(fetch_single_url(url).await?))
        }
        "activate_skill" | "read_skill" => {
            let key = call
                .arguments
                .get("name")
                .or_else(|| call.arguments.get("id"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "activate_skill requires a non-empty \"name\" (or \"id\") string.".to_string()
                })?;
            let skill = crate::skills::find_skill(user_skills, key).ok_or_else(|| {
                format!("Unknown skill '{key}'. Use a name or id from the available skills list.")
            })?;
            Ok(ToolOutcome::text(skill.full_instructions()))
        }
        other => Err(format!("Unknown capability '{other}'.")),
    }
}

async fn fetch_single_url(url: &str) -> Result<String, String> {
    if !scrapeable_url(url) {
        return Err(
            "fetch_url only supports http(s) pages (not files like PDF, images, or archives)."
                .into(),
        );
    }
    let text = fetch_page_text(url, FETCH_URL_MAX_CHARS).await?;
    if text.trim().is_empty() {
        return Err(format!("Fetched {url} but extracted no readable text."));
    }
    Ok(format!(
        "Fetched page text from {url} (up to {FETCH_URL_MAX_CHARS} characters):\n{text}"
    ))
}

#[derive(Debug)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Debug, Clone, Copy)]
enum SearchBackend {
    Lite,
    Html,
    InstantAnswer { html_blocked: bool },
}

impl SearchBackend {
    fn ui_note(self) -> &'static str {
        match self {
            Self::Lite => "via DuckDuckGo Lite",
            Self::Html => "via DuckDuckGo",
            Self::InstantAnswer { html_blocked: true } => {
                "fell back to Instant Answer (HTML search blocked)"
            }
            Self::InstantAnswer {
                html_blocked: false,
            } => "fell back to Instant Answer (no HTML results)",
        }
    }

    fn result_label(self) -> &'static str {
        match self {
            Self::Lite => "DuckDuckGo Lite",
            Self::Html => "DuckDuckGo",
            Self::InstantAnswer { html_blocked: true } => {
                "DuckDuckGo Instant Answer — HTML search blocked"
            }
            Self::InstantAnswer {
                html_blocked: false,
            } => "DuckDuckGo Instant Answer — no HTML results",
        }
    }
}

async fn duckduckgo_search(
    query: &str,
    depth: WebSearchDepth,
) -> Result<(String, String), String> {
    // Prefer HTML results, but never try to defeat DDG captchas — fall through to
    // Instant Answer (official keyless JSON API) when the HTML endpoints challenge us.
    let mut blocked = false;
    let mut hits = Vec::new();
    let mut backend = SearchBackend::Lite;

    match duckduckgo_html_hits(query, "https://lite.duckduckgo.com/lite/", true).await {
        Ok(parsed) if !parsed.is_empty() => {
            hits = parsed;
            backend = SearchBackend::Lite;
        }
        Ok(_) => {}
        Err(DuckHtmlError::Blocked) => blocked = true,
        Err(DuckHtmlError::Other) => {}
    }

    if hits.is_empty() {
        match duckduckgo_html_hits(query, "https://html.duckduckgo.com/html/", false).await {
            Ok(parsed) if !parsed.is_empty() => {
                hits = parsed;
                backend = SearchBackend::Html;
            }
            Ok(_) => {}
            Err(DuckHtmlError::Blocked) => blocked = true,
            Err(DuckHtmlError::Other) => {}
        }
    }

    if hits.is_empty() {
        backend = SearchBackend::InstantAnswer {
            html_blocked: blocked,
        };
        return match duckduckgo_instant_answer(query, depth, backend).await {
            Ok(out) => Ok((out, backend.ui_note().to_string())),
            Err(ia_err) if blocked => Err(format!(
                "DuckDuckGo challenged the HTML search (captcha/bot check) and Instant Answer also failed: {ia_err}"
            )),
            Err(ia_err) => Err(ia_err),
        };
    }

    let mut out = format!(
        "Web search results for {query:?} ({}):\n",
        backend.result_label()
    );
    for (index, hit) in hits.iter().take(MAX_SEARCH_RESULTS).enumerate() {
        out.push_str(&format!(
            "\n{}. {}\n   URL: {}\n   {}\n",
            index + 1,
            hit.title,
            hit.url,
            hit.snippet
        ));
    }
    append_scraped_pages(&mut out, &hits, depth).await;
    Ok((out, backend.ui_note().to_string()))
}

#[derive(Debug)]
enum DuckHtmlError {
    Blocked,
    Other,
}

async fn duckduckgo_html_hits(
    query: &str,
    endpoint: &str,
    lite: bool,
) -> Result<Vec<SearchHit>, DuckHtmlError> {
    let client = http::public_client();
    let response = if lite {
        apply_browser_page_headers(
            client
                .get(endpoint)
                .timeout(SEARCH_TIMEOUT)
                .query(&[("q", query)]),
        )
        .send()
        .await
        .map_err(|_| DuckHtmlError::Other)?
    } else {
        match apply_browser_page_headers(
            client
                .post(endpoint)
                .timeout(SEARCH_TIMEOUT),
        )
        .form(&[("q", query), ("b", "")])
        .send()
        .await
        {
            Ok(response) => response,
            Err(_) => apply_browser_page_headers(
                client
                    .get(endpoint)
                    .timeout(SEARCH_TIMEOUT)
                    .query(&[("q", query)]),
            )
            .send()
            .await
            .map_err(|_| DuckHtmlError::Other)?,
        }
    };

    let status = response.status().as_u16();
    if status != 200 && status != 202 {
        return Err(DuckHtmlError::Other);
    }

    let html = response.text().await.map_err(|_| DuckHtmlError::Other)?;

    if html.contains("anomaly.js") || html.contains("Please complete the captcha") {
        return Err(DuckHtmlError::Blocked);
    }

    let hits = if lite {
        parse_ddg_lite_html(&html)
    } else {
        parse_ddg_html(&html)
    };
    Ok(hits)
}

async fn duckduckgo_instant_answer(
    query: &str,
    depth: WebSearchDepth,
    backend: SearchBackend,
) -> Result<String, String> {
    let client = http::public_client();
    let response = client
        .get("https://api.duckduckgo.com/")
        .timeout(SEARCH_TIMEOUT)
        .query(&[
            ("q", query),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ])
        .send()
        .await
        .map_err(|error| format!("DuckDuckGo Instant Answer failed: {error}"))?;

    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("Invalid Instant Answer JSON: {error}"))?;

    let mut lines = vec![format!(
        "Web search results for {query:?} ({}):",
        backend.result_label()
    )];
    let mut hits = Vec::new();
    if let Some(text) = body.get("AbstractText").and_then(|v| v.as_str())
        && !text.is_empty()
    {
        let url = body
            .get("AbstractURL")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        lines.push(format!("\nSummary: {text}"));
        if !url.is_empty() {
            lines.push(format!("Source: {url}"));
            hits.push(SearchHit {
                title: "Abstract".into(),
                url: url.to_string(),
                snippet: text.to_string(),
            });
        }
    }

    let mut count = 0usize;
    if let Some(topics) = body.get("RelatedTopics").and_then(|v| v.as_array()) {
        for topic in topics {
            if count >= MAX_SEARCH_RESULTS {
                break;
            }
            if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                let url = topic.get("FirstURL").and_then(|v| v.as_str()).unwrap_or("");
                count += 1;
                lines.push(format!("\n{count}. {text}"));
                if !url.is_empty() {
                    lines.push(format!("   URL: {url}"));
                    hits.push(SearchHit {
                        title: format!("Related {count}"),
                        url: url.to_string(),
                        snippet: text.to_string(),
                    });
                }
            } else if let Some(nested) = topic.get("Topics").and_then(|v| v.as_array()) {
                for item in nested {
                    if count >= MAX_SEARCH_RESULTS {
                        break;
                    }
                    if let Some(text) = item.get("Text").and_then(|v| v.as_str()) {
                        let url = item.get("FirstURL").and_then(|v| v.as_str()).unwrap_or("");
                        count += 1;
                        lines.push(format!("\n{count}. {text}"));
                        if !url.is_empty() {
                            lines.push(format!("   URL: {url}"));
                            hits.push(SearchHit {
                                title: format!("Related {count}"),
                                url: url.to_string(),
                                snippet: text.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    if lines.len() == 1 {
        lines.push("\nNo results found.".into());
    }
    let mut out = lines.join("\n");
    append_scraped_pages(&mut out, &hits, depth).await;
    Ok(out)
}

async fn append_scraped_pages(out: &mut String, hits: &[SearchHit], depth: WebSearchDepth) {
    let Some((page_count, max_chars)) = depth.scrape_plan() else {
        return;
    };
    let targets: Vec<&SearchHit> = hits
        .iter()
        .filter(|hit| scrapeable_url(&hit.url))
        .take(page_count)
        .collect();
    if targets.is_empty() {
        return;
    }

    out.push_str(&format!(
        "\n--- Fetched page text (depth: {}, up to {} pages) ---\n",
        depth.label(),
        page_count
    ));

    for (index, hit) in targets.iter().enumerate() {
        match fetch_page_text(&hit.url, max_chars).await {
            Ok(text) if !text.trim().is_empty() => {
                out.push_str(&format!(
                    "\n[{}] {} ({})\n{}\n",
                    index + 1,
                    hit.title,
                    hit.url,
                    text
                ));
            }
            Ok(_) => {
                out.push_str(&format!(
                    "\n[{}] {} ({})\n(no extractable text)\n",
                    index + 1,
                    hit.title,
                    hit.url
                ));
            }
            Err(error) => {
                out.push_str(&format!(
                    "\n[{}] {} ({})\n(fetch failed: {error})\n",
                    index + 1,
                    hit.title,
                    hit.url
                ));
            }
        }
    }
}

fn scrapeable_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    let path = lower.split('?').next().unwrap_or(&lower);
    const SKIP_EXT: &[&str] = &[
        ".pdf", ".zip", ".gz", ".tgz", ".rar", ".7z", ".exe", ".dmg", ".apk", ".mp3", ".mp4",
        ".mov", ".avi", ".mkv", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg", ".ico", ".css",
        ".js", ".mjs", ".json", ".xml", ".rss", ".atom", ".woff", ".woff2", ".ttf",
    ];
    !SKIP_EXT.iter().any(|ext| path.ends_with(ext))
}

/// Chrome-like Accept / navigation headers. Sparse Accept values get 406 from some CDNs.
fn apply_browser_page_headers(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    http::apply_browser_navigation_headers(req)
}

async fn fetch_page_text(url: &str, max_chars: usize) -> Result<String, String> {
    let client = http::public_client();
    let mut response = apply_browser_page_headers(client.get(url).timeout(PAGE_TIMEOUT))
        .send()
        .await
        .map_err(|error| format!("{error}"))?;

    let mut status = response.status().as_u16();
    // Negotiate again with a looser Accept — Akamai/news stacks sometimes 406 the first try.
    if matches!(status, 406 | 403) {
        response = apply_browser_page_headers(client.get(url).timeout(PAGE_TIMEOUT))
            .header("Accept", "*/*")
            .send()
            .await
            .map_err(|error| format!("{error}"))?;
        status = response.status().as_u16();
    }

    if status != 200 && status != 203 && status != 206 {
        return Err(format!("HTTP {status} fetching {url}"));
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.is_empty()
        && !(content_type.contains("text/html")
            || content_type.contains("application/xhtml")
            || content_type.contains("text/plain")
            || content_type.contains("text/xml")
            || content_type.contains("application/xml"))
    {
        return Err(format!(
            "unsupported content-type ({content_type}) fetching {url}"
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("read failed: {error}"))?;
    let html = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PAGE_BYTES as usize)]);
    Ok(html_to_readable_text(&html, max_chars))
}

fn html_to_readable_text(html: &str, max_chars: usize) -> String {
    let mut cleaned = remove_tag_blocks(html, "script");
    cleaned = remove_tag_blocks(&cleaned, "style");
    cleaned = remove_tag_blocks(&cleaned, "noscript");
    cleaned = remove_tag_blocks(&cleaned, "svg");
    cleaned = remove_tag_blocks(&cleaned, "template");

    for marker in [
        "</p>",
        "</div>",
        "</section>",
        "</article>",
        "</li>",
        "</tr>",
        "</h1>",
        "</h2>",
        "</h3>",
        "</h4>",
        "</h5>",
        "</h6>",
        "</blockquote>",
        "<br>",
        "<br/>",
        "<br />",
        "<hr>",
        "<hr/>",
        "<hr />",
    ] {
        cleaned = cleaned.replace(marker, "\n");
        cleaned = cleaned.replace(&marker.to_ascii_uppercase(), "\n");
    }

    let text = strip_tags(&cleaned);
    let mut lines = Vec::new();
    for line in text.lines() {
        let collapsed = collapse_ws(line);
        if collapsed.is_empty() {
            if lines.last().is_some_and(|prev: &String| !prev.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        let lower = collapsed.to_ascii_lowercase();
        if lower == "skip to content"
            || lower == "skip to main content"
            || lower == "advertisement"
            || lower.starts_with("cookie") && lower.len() < 80
        {
            continue;
        }
        lines.push(collapsed);
    }

    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    let joined = lines.join("\n");
    truncate_chars(&joined, max_chars)
}

fn remove_tag_blocks(html: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut rest = html;
    let mut out = String::with_capacity(html.len());
    while let Some(start) = find_ignore_ascii_case(rest, &open) {
        out.push_str(&rest[..start]);
        let after_open = &rest[start..];
        match find_ignore_ascii_case(after_open, &close) {
            Some(rel) => rest = &after_open[rel + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let n = needle.as_bytes();
    let h = haystack.as_bytes();
    if h.len() < n.len() {
        return None;
    }
    'outer: for i in 0..=(h.len() - n.len()) {
        for (a, b) in h[i..i + n.len()].iter().zip(n.iter()) {
            if !a.eq_ignore_ascii_case(b) {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn parse_ddg_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("result__a") {
        rest = &rest[idx..];
        let href = match attr_after(rest, "href=\"") {
            Some(v) => v,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title_html = match between(rest, ">", "</a>") {
            Some(v) => v,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title = collapse_ws(&strip_tags(title_html));
        let url = decode_ddg_href(href);
        let snippet = rest
            .find("result__snippet")
            .and_then(|s| {
                let slice = &rest[s..];
                let window = &slice[..slice.len().min(1200)];
                between(window, ">", "</")
                    .map(strip_tags)
                    .map(|s| collapse_ws(&s))
            })
            .unwrap_or_default();

        if !title.is_empty() && !url.is_empty() {
            hits.push(SearchHit {
                title,
                url,
                snippet,
            });
        }
        if hits.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        rest = &rest[1..];
    }
    hits
}

/// Lite results use plain result-link anchors rather than `result__a`.
fn parse_ddg_lite_html(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("class=\"result-link\"") {
        let window_start = rest[..idx].rfind('<').unwrap_or(idx);
        rest = &rest[window_start..];
        let href = match attr_after(rest, "href=\"") {
            Some(v) => v,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title_html = match between(rest, ">", "</a>") {
            Some(v) => v,
            None => {
                rest = &rest[1..];
                continue;
            }
        };
        let title = collapse_ws(&strip_tags(title_html));
        let url = decode_ddg_href(href);
        let snippet = rest
            .find("class=\"result-snippet\"")
            .and_then(|s| {
                let slice = &rest[s..];
                let window = &slice[..slice.len().min(1200)];
                between(window, ">", "</td>")
                    .or_else(|| between(window, ">", "</"))
                    .map(strip_tags)
                    .map(|s| collapse_ws(&s))
            })
            .unwrap_or_default();
        if !title.is_empty()
            && !url.is_empty()
            && (url.starts_with("http://") || url.starts_with("https://"))
        {
            hits.push(SearchHit {
                title,
                url,
                snippet,
            });
        }
        if hits.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        rest = &rest[1..];
    }
    if hits.is_empty() {
        // Older lite markup sometimes omits result-link class.
        return parse_ddg_html(html);
    }
    hits
}

fn attr_after<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let start = s.find(key)? + key.len();
    let end = s[start..].find('"')? + start;
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

fn decode_ddg_href(href: &str) -> String {
    let full = if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        href.to_string()
    };
    if let Some(idx) = full.find("uddg=") {
        let start = idx + "uddg=".len();
        let end = full[start..]
            .find('&')
            .map(|i| start + i)
            .unwrap_or(full.len());
        return percent_decode(&full[start..end]);
    }
    full
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
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
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
    fn parses_tool_call_json() {
        let text = r#"Sure.
<tool_call>
{"name":"web_search","arguments":{"query":"rust async"}}
</tool_call>"#;
        let call = extract_tool_call(text).unwrap();
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "rust async");
    }

    #[test]
    fn synthesizes_native_openai_tool_calls() {
        let mut slots = Vec::new();
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "web_search", "arguments": "" }
            }),
        );
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "function": { "arguments": "{\"query\":\"latest news on grok\"}" }
            }),
        );
        let turn = resolve_streamed_turn("", "", &slots);
        let call = turn.tool.unwrap();
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "latest news on grok");
    }

    #[test]
    fn synthesizes_flattened_tool_call_deltas() {
        let mut slots = Vec::new();
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "name": "web_search",
                "arguments": "{\"query\":\"elon musk\"}"
            }),
        );
        let turn = resolve_streamed_turn("", "", &slots);
        let call = turn.tool.unwrap();
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "elon musk");
    }

    #[test]
    fn unclosed_think_does_not_swallow_tool_call() {
        let text = "<think>planning\n<tool_call>\n{\"name\":\"web_search\",\"arguments\":{\"query\":\"x\"}}\n</tool_call>";
        let visible = strip_think_blocks(text);
        let call = extract_tool_call(&visible).unwrap();
        assert_eq!(call.arguments["query"], "x");
    }

    #[test]
    fn openai_tools_include_enabled_skills() {
        let skills = AgentSkills {
            web_search: true,
            fetch_url: true,
            ..AgentSkills::default()
        };
        let tools = openai_tools_payload(&skills, &[]);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(names, ["web_search", "fetch_url"]);
    }

    #[test]
    fn finalize_keeps_reasoning_when_content_empty() {
        let turn = resolve_streamed_turn("planning a search", "", &[]);
        assert!(
            turn.assistant_text()
                .contains("<think>planning a search</think>")
        );
        assert!(turn.tool.is_none());
    }

    #[test]
    fn finalize_prefers_native_over_inline_xml() {
        let mut slots = Vec::new();
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "function": {
                    "name": "web_search",
                    "arguments": "{\"query\":\"from-native\"}"
                }
            }),
        );
        let inline = "<tool_call>\n{\"name\":\"web_search\",\"arguments\":{\"query\":\"from-xml\"}}\n</tool_call>";
        let turn = resolve_streamed_turn("", inline, &slots);
        let call = turn.tool.unwrap();
        assert_eq!(call.arguments["query"], "from-native");
    }

    #[test]
    fn strips_think_before_tool_parse() {
        let text = r#"<think>plan</think>
<tool_call>
{"name":"web_search","arguments":{"query":"hi"}}
</tool_call>"#;
        let visible = strip_think_blocks(text);
        let call = extract_tool_call(&visible).unwrap();
        assert_eq!(call.arguments["query"], "hi");
    }

    #[test]
    fn decodes_ddg_redirect() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpath&rut=x";
        assert_eq!(decode_ddg_href(href), "https://example.com/path");
    }

    #[test]
    fn extracts_readable_text_from_html() {
        let html = r#"<html><head><style>.x{color:red}</style><script>evil()</script></head>
        <body><nav>Skip to content</nav><article><h1>Hello</h1><p>World <b>there</b>.</p></article></body></html>"#;
        let text = html_to_readable_text(html, 500);
        assert!(text.contains("Hello"));
        assert!(text.contains("World there."));
        assert!(!text.contains("evil"));
        assert!(!text.contains("color:red"));
    }

    #[test]
    fn scrape_plan_auto_fetches_pages() {
        assert!(WebSearchDepth::Off.scrape_plan().is_none());
        assert_eq!(WebSearchDepth::Auto.scrape_plan(), Some((3, 2800)));
        assert_eq!(WebSearchDepth::Deep.scrape_plan(), Some((6, 4800)));
    }

    #[test]
    fn skips_binary_result_urls() {
        assert!(scrapeable_url("https://example.com/story"));
        assert!(!scrapeable_url("https://example.com/file.pdf"));
        assert!(!scrapeable_url("ftp://example.com/a"));
    }

    #[test]
    fn fetch_url_capability_gated() {
        let off = AgentSkills::default();
        let on = AgentSkills {
            fetch_url: true,
            ..AgentSkills::default()
        };
        assert!(!capability_allowed("fetch_url", &off, &[]));
        assert!(capability_allowed("fetch_url", &on, &[]));
        assert!(on.any_enabled());
        assert!(!off.any_enabled());
    }

    #[test]
    fn fetch_url_rejects_bad_targets() {
        assert!(!scrapeable_url("ftp://example.com/a"));
        assert!(!scrapeable_url("https://example.com/doc.pdf"));
        assert!(scrapeable_url("https://example.com/article"));
    }
}
