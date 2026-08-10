use std::io::{BufRead, BufReader, Read};

use serde_json::{json, Value};

pub const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u64 = 8192;

pub fn openai_to_anthropic_messages(payload: &Value) -> Result<Value, String> {
    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("claude-sonnet-4-5")
        .to_string();

    let max_tokens = payload
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            payload
                .get("max_completion_tokens")
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(DEFAULT_MAX_TOKENS)
        .max(1);

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();

    let source = payload
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for msg in source {
        let role = msg
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        match role.as_str() {
            "system" | "developer" => {
                let content = message_content_to_text(&msg);
                if !content.trim().is_empty() {
                    system_parts.push(content);
                }
            }
            "user" => {
                let content = message_content_to_text(&msg);
                if !content.trim().is_empty() {
                    push_anthropic_message(&mut messages, "user", &content);
                }
            }
            "assistant" => push_anthropic_assistant(&mut messages, &msg),
            "tool" => push_anthropic_tool_result(&mut messages, &msg),
            _ => {}
        }
    }

    if messages.is_empty() {
        return Err("messages must not be empty".into());
    }
    if messages
        .first()
        .and_then(|m| m.get("role"))
        .and_then(|v| v.as_str())
        != Some("user")
    {
        messages.insert(
            0,
            json!({
                "role": "user",
                "content": "(continued)",
            }),
        );
    }

    let mut out = json!({
        "model": model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if let Some(object) = out.as_object_mut() {
        if !system_parts.is_empty() {
            object.insert("system".into(), Value::String(system_parts.join("\n\n")));
        }
        if let Some(tools) = openai_tools_to_anthropic(payload.get("tools")) {
            object.insert("tools".into(), Value::Array(tools));
        }
        if let Some(effort) = payload.get("reasoning_effort").and_then(|v| v.as_str()) {
            let budget = match effort {
                "low" => Some(1024u64),
                "medium" => Some(4096),
                "high" | "max" => Some(10000),
                _ => None,
            };
            if let Some(tokens) = budget {
                object.insert(
                    "thinking".into(),
                    json!({ "type": "enabled", "budget_tokens": tokens }),
                );
            }
        }
    }
    Ok(out)
}

fn openai_tools_to_anthropic(tools: Option<&Value>) -> Option<Vec<Value>> {
    let tools = tools?.as_array()?;
    let converted: Vec<Value> = tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name")?.as_str()?;
            let description = function
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let input_schema = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
            Some(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }))
        })
        .collect();
    if converted.is_empty() {
        None
    } else {
        Some(converted)
    }
}

fn message_content_to_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    return Some(text.to_string());
                }
                if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                    return part
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        _ => String::new(),
    }
}

fn push_anthropic_message(messages: &mut Vec<Value>, role: &str, content: &str) {
    if let Some(last) = messages.last_mut() {
        if last.get("role").and_then(|v| v.as_str()) == Some(role) {
            if let Some(existing) = last.get("content").and_then(|v| v.as_str()) {
                let merged = if existing.is_empty() {
                    content.to_string()
                } else {
                    format!("{existing}\n\n{content}")
                };
                last.as_object_mut()
                    .map(|obj| obj.insert("content".into(), Value::String(merged)));
                return;
            }
        }
    }
    messages.push(json!({
        "role": role,
        "content": content,
    }));
}

fn push_anthropic_assistant(messages: &mut Vec<Value>, msg: &Value) {
    let text = message_content_to_text(msg);
    let tool_calls = msg
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if tool_calls.is_empty() {
        if !text.trim().is_empty() {
            push_anthropic_message(messages, "assistant", &text);
        }
        return;
    }

    let mut blocks = Vec::new();
    if !text.trim().is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    for (index, tc) in tool_calls.iter().enumerate() {
        let id = tc
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| format!("toolu_{index}"));
        let name = tc
            .pointer("/function/name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }
        let input = match tc.pointer("/function/arguments") {
            Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or_else(|_| json!({})),
            Some(other) => other.clone(),
            None => json!({}),
        };
        blocks.push(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }));
    }
    if !blocks.is_empty() {
        messages.push(json!({
            "role": "assistant",
            "content": blocks,
        }));
    }
}

fn push_anthropic_tool_result(messages: &mut Vec<Value>, msg: &Value) {
    let content = message_content_to_text(msg);
    let tool_use_id = msg
        .get("tool_call_id")
        .and_then(|v| v.as_str())
        .unwrap_or("toolu_0");
    let block = json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
    });
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(|v| v.as_str()) == Some("user")
        && let Some(Value::Array(parts)) = last.get_mut("content")
        && parts
            .iter()
            .all(|part| part.get("type").and_then(|v| v.as_str()) == Some("tool_result"))
    {
        parts.push(block);
        return;
    }
    messages.push(json!({
        "role": "user",
        "content": [block],
    }));
}

/// Incremental Anthropic SSE → OpenAI-style SSE frames (cancel-safe in async pumps).
#[derive(Debug, Default)]
pub struct AnthropicSseTranslator {
    event_name: String,
    content: String,
    reasoning: String,
    finished: bool,
    tool_index: Option<u32>,
    tool_id: String,
    tool_name: String,
}

impl AnthropicSseTranslator {
    pub fn push_line(&mut self, trimmed: &str) -> Result<Vec<Vec<u8>>, String> {
        if self.finished {
            return Ok(Vec::new());
        }
        if trimmed.is_empty() {
            self.event_name.clear();
            return Ok(Vec::new());
        }
        if let Some(rest) = trimmed.strip_prefix("event:") {
            self.event_name = rest.trim().to_string();
            return Ok(Vec::new());
        }
        let Some(data) = trimmed.strip_prefix("data:") else {
            return Ok(Vec::new());
        };
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(payload) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        let kind = payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or(self.event_name.as_str());

        match kind {
            "content_block_start" => {
                let block = payload.get("content_block").cloned().unwrap_or(Value::Null);
                if block.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    let index = payload
                        .get("index")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    self.tool_index = Some(index);
                    self.tool_id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.tool_name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !self.tool_name.is_empty() {
                        return Ok(vec![openai_tool_call_start_delta(
                            index,
                            &self.tool_id,
                            &self.tool_name,
                        )]);
                    }
                }
                Ok(Vec::new())
            }
            "content_block_delta" => {
                let delta = payload.get("delta").cloned().unwrap_or(Value::Null);
                let dtype = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if dtype == "text_delta" {
                    if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                        if text.is_empty() {
                            return Ok(Vec::new());
                        }
                        self.content.push_str(text);
                        return Ok(vec![openai_content_delta(text)]);
                    }
                } else if dtype == "thinking_delta"
                    && let Some(text) = delta.get("thinking").and_then(|v| v.as_str())
                {
                    if text.is_empty() {
                        return Ok(Vec::new());
                    }
                    self.reasoning.push_str(text);
                    return Ok(vec![openai_reasoning_delta(text)]);
                } else if dtype == "input_json_delta"
                    && let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str())
                {
                    let index = self.tool_index.unwrap_or(0);
                    return Ok(vec![openai_tool_call_args_delta(index, partial)]);
                }
                Ok(Vec::new())
            }
            "error" => {
                let message = payload
                    .pointer("/error/message")
                    .and_then(|v| v.as_str())
                    .or_else(|| payload.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("Anthropic API error");
                Err(message.to_string())
            }
            "message_stop" => {
                self.finished = true;
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn finish_frames(&self) -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    pub fn combined_text(&self) -> String {
        if self.reasoning.is_empty() {
            self.content.clone()
        } else {
            format!("<think>{}</think>{}", self.reasoning, self.content)
        }
    }
}

pub fn relay_anthropic_sse_as_openai<R: Read>(
    reader: R,
    mut emit: impl FnMut(Vec<u8>) -> bool,
) -> Result<String, String> {
    let mut lines = BufReader::new(reader);
    let mut line = String::new();
    let mut translator = AnthropicSseTranslator::default();

    loop {
        line.clear();
        let read = lines
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        for frame in translator.push_line(trimmed)? {
            if !emit(frame) {
                return Ok(translator.combined_text());
            }
        }
        if translator.finished {
            break;
        }
    }

    let _ = emit(translator.finish_frames());
    Ok(translator.combined_text())
}

fn openai_content_delta(text: &str) -> Vec<u8> {
    let payload = json!({
        "choices": [{ "delta": { "content": text } }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

fn openai_reasoning_delta(text: &str) -> Vec<u8> {
    let payload = json!({
        "choices": [{ "delta": { "reasoning_content": text } }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

fn openai_tool_call_start_delta(index: u32, id: &str, name: &str) -> Vec<u8> {
    let payload = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": index,
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": "" }
                }]
            }
        }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

fn openai_tool_call_args_delta(index: u32, arguments: &str) -> Vec<u8> {
    let payload = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": index,
                    "function": { "arguments": arguments }
                }]
            }
        }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_system_and_user_messages() {
        let body = json!({
            "model": "claude-test",
            "messages": [
                { "role": "system", "content": "Be brief." },
                { "role": "user", "content": "Hi" },
                { "role": "assistant", "content": "Hello" },
                { "role": "user", "content": "Bye" }
            ]
        });
        let out = openai_to_anthropic_messages(&body).unwrap();
        assert_eq!(out["model"], "claude-test");
        assert_eq!(out["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(out["system"], "Be brief.");
        assert_eq!(out["messages"].as_array().unwrap().len(), 3);
        assert_eq!(out["messages"][0]["role"], "user");
    }

    #[test]
    fn converts_openai_tool_exchange() {
        let body = json!({
            "model": "claude-test",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "web_search",
                    "description": "Search",
                    "parameters": {
                        "type": "object",
                        "properties": { "query": { "type": "string" } },
                        "required": ["query"]
                    }
                }
            }],
            "messages": [
                { "role": "user", "content": "search cats" },
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "web_search",
                            "arguments": "{\"query\":\"cats\"}"
                        }
                    }]
                },
                {
                    "role": "tool",
                    "tool_call_id": "call_1",
                    "content": "results"
                }
            ]
        });
        let out = openai_to_anthropic_messages(&body).unwrap();
        assert_eq!(out["tools"][0]["name"], "web_search");
        let assistant = &out["messages"][1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"][0]["type"], "tool_use");
        assert_eq!(assistant["content"][0]["id"], "call_1");
        let tool_user = &out["messages"][2];
        assert_eq!(tool_user["role"], "user");
        assert_eq!(tool_user["content"][0]["type"], "tool_result");
        assert_eq!(tool_user["content"][0]["tool_use_id"], "call_1");
    }

    #[test]
    fn relays_text_deltas() {
        let upstream = "\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"!\"}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";
        let mut frames = Vec::new();
        let text = relay_anthropic_sse_as_openai(upstream.as_bytes(), |frame| {
            frames.push(String::from_utf8_lossy(&frame).into_owned());
            true
        })
        .unwrap();
        assert_eq!(text, "Hi!");
        assert!(frames.iter().any(|f| f.contains("Hi")));
        assert!(frames.iter().any(|f| f.contains("[DONE]")));
    }
}
