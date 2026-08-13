//! Agent mode: OpenAI-compatible `tool_calls` + `role: tool` (Anthropic via translator).
//! XML `<tool_call>` in content is accepted only as a fallback for local models.

pub mod chat;
pub mod skills;

use std::{
    collections::{HashMap, HashSet},
    io::ErrorKind,
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::{
    anthropic::{self, AnthropicSseTranslator},
    chat::{ChatStream, StreamFail, open_llm_sse, send_sse, stream_from_worker},
    http,
    providers::ApiStyle,
    skills::UserSkill,
};

/// After this many tool calls in one turn, surface a once-only UI notice that
/// the model may be looping. There is no hard tool-round ceiling — Stop is the
/// escape hatch.
const TOOL_LOOP_NOTICE_AFTER: usize = 25;
const DEEP_RESEARCH_TOOL_LOOP_NOTICE_AFTER: usize = 40;
const DEEP_RESEARCH_MIN_RESULTS: usize = 10;
/// Soft cap: after this many web_search calls, stop offering tools and demand a final answer.
const DEEP_RESEARCH_BRIEF_SEARCH_CAP: usize = 4;
const DEEP_RESEARCH_LONG_SEARCH_CAP: usize = 10;
/// How many times we nudge after a blank visible reply before giving up.
const MAX_EMPTY_RETRIES: usize = 2;
/// Extra nudges once research already has sources and only the write-up is missing.
const MAX_ANSWER_RETRIES: usize = 4;
const CLARIFY_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Prefix for mid-turn user guidance injected between tool rounds.
const STEER_MARKER: &str = "[USER STEER]";

fn clarify_waiters() -> &'static Mutex<HashMap<String, oneshot::Sender<Value>>> {
    static WAITERS: OnceLock<Mutex<HashMap<String, oneshot::Sender<Value>>>> = OnceLock::new();
    WAITERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn steer_sessions() -> &'static Mutex<HashMap<String, mpsc::UnboundedSender<SteerPayload>>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, mpsc::UnboundedSender<SteerPayload>>>> =
        OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone)]
struct SteerPayload {
    text: String,
    client_id: Option<String>,
}

pub fn submit_clarify_answers(id: &str, answers: Value) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("clarify id is required".into());
    }
    let sender = clarify_waiters()
        .lock()
        .map_err(|_| "clarify waiters lock poisoned".to_string())?
        .remove(id)
        .ok_or_else(|| "No pending clarification for that id (it may have expired).".to_string())?;
    sender
        .send(answers)
        .map_err(|_| "Research turn is no longer waiting for answers.".to_string())
}

/// Queue mid-turn guidance for an active agent run. Injected before the next
/// LLM call (typically after the current tool round finishes)—does not abort.
pub fn submit_steer(id: &str, text: &str, client_id: Option<&str>) -> Result<(), String> {
    let id = id.trim();
    let text = text.trim();
    if id.is_empty() {
        return Err("steer id is required".into());
    }
    if text.is_empty() {
        return Err("steer text is required".into());
    }
    let sender = steer_sessions()
        .lock()
        .map_err(|_| "steer sessions lock poisoned".to_string())?
        .get(id)
        .cloned()
        .ok_or_else(|| "No active agent turn for that id (it may have finished).".to_string())?;
    let client_id = client_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    sender
        .send(SteerPayload {
            text: text.to_string(),
            client_id,
        })
        .map_err(|_| "Agent turn is no longer accepting steering.".to_string())
}

struct ClarifyWaitGuard {
    id: String,
}

impl Drop for ClarifyWaitGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = clarify_waiters().lock() {
            map.remove(&self.id);
        }
    }
}

struct SteerSessionGuard {
    id: String,
}

impl Drop for SteerSessionGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = steer_sessions().lock() {
            map.remove(&self.id);
        }
    }
}

fn new_steer_id() -> String {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    format!(
        "steer_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn drain_steers(rx: &mut mpsc::UnboundedReceiver<SteerPayload>) -> Vec<SteerPayload> {
    let mut out = Vec::new();
    while let Ok(payload) = rx.try_recv() {
        let trimmed = payload.text.trim().to_string();
        if !trimmed.is_empty() {
            out.push(SteerPayload {
                text: trimmed,
                client_id: payload.client_id,
            });
        }
    }
    out
}

fn format_steer_content(text: &str) -> String {
    format!("{STEER_MARKER}\n{}", text.trim())
}

fn append_steer_messages(messages: &mut Vec<Value>, steers: &[SteerPayload]) {
    for payload in steers {
        messages.push(json!({
            "role": "user",
            "content": format_steer_content(&payload.text),
        }));
    }
}
const PAGE_TIMEOUT: Duration = Duration::from_secs(12);
const DEFAULT_SEARCH_RESULTS: usize = 6;
const MAX_SEARCH_RESULTS: usize = 20;
const MAX_PAGE_BYTES: u64 = 1_500_000;
const FETCH_URL_MAX_CHARS: usize = 8_000;
const DDGS_TIMEOUT: Duration = Duration::from_secs(45);
/// Retry flaky DDGS calls before handing a soft failure back to the model.
const DDGS_ATTEMPTS: usize = 3;
const DDGS_RETRY_DELAY_MS: u64 = 450;

#[derive(Debug, Clone, Deserialize)]
pub struct AgentRequest {
    pub messages: Vec<Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub agent: bool,
    #[serde(default)]
    pub deep_research: bool,
    #[serde(default)]
    pub deep_research_output: DeepResearchOutput,
    #[serde(default)]
    pub skills: AgentSkills,
    #[serde(default)]
    pub force_tools: Vec<String>,
    #[serde(default)]
    pub chat_template_kwargs: Option<Value>,
    #[serde(default)]
    pub thinking_budget_tokens: Option<i64>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning: Option<Value>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeepResearchOutput {
    #[default]
    Long,
    Brief,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WebSearchKind {
    #[default]
    Web,
    News,
}

impl WebSearchKind {
    fn label(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::News => "news",
        }
    }
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

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchBackend {
    #[default]
    Auto,
    Duckduckgo,
    Brave,
    Bing,
    Google,
    Mojeek,
    Startpage,
    Yahoo,
    Yandex,
    Wikipedia,
}

impl WebSearchBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Duckduckgo => "duckduckgo",
            Self::Brave => "brave",
            Self::Bing => "bing",
            Self::Google => "google",
            Self::Mojeek => "mojeek",
            Self::Startpage => "startpage",
            Self::Yahoo => "yahoo",
            Self::Yandex => "yandex",
            Self::Wikipedia => "wikipedia",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchSafeSearch {
    On,
    #[default]
    Moderate,
    Off,
}

impl WebSearchSafeSearch {
    fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Moderate => "moderate",
            Self::Off => "off",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchRecency {
    #[default]
    Any,
    Day,
    Week,
    Month,
    Year,
}

impl WebSearchRecency {
    fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

fn default_web_search_max_results() -> usize {
    DEFAULT_SEARCH_RESULTS
}

fn default_web_search_region() -> String {
    "us-en".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentSkills {
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub web_search_depth: WebSearchDepth,
    #[serde(default)]
    pub web_search_backend: WebSearchBackend,
    #[serde(default = "default_web_search_max_results")]
    pub web_search_max_results: usize,
    #[serde(default = "default_web_search_region")]
    pub web_search_region: String,
    #[serde(default)]
    pub web_search_safesearch: WebSearchSafeSearch,
    #[serde(default)]
    pub web_search_recency: WebSearchRecency,
    #[serde(default)]
    pub fetch_url: bool,
}

impl Default for AgentSkills {
    fn default() -> Self {
        Self {
            web_search: false,
            web_search_depth: WebSearchDepth::default(),
            web_search_backend: WebSearchBackend::default(),
            web_search_max_results: default_web_search_max_results(),
            web_search_region: default_web_search_region(),
            web_search_safesearch: WebSearchSafeSearch::default(),
            web_search_recency: WebSearchRecency::default(),
            fetch_url: false,
        }
    }
}

impl AgentSkills {
    pub fn any_enabled(&self) -> bool {
        self.web_search || self.fetch_url
    }

    fn search_result_count(&self) -> usize {
        self.web_search_max_results.clamp(1, MAX_SEARCH_RESULTS)
    }

    fn search_region(&self) -> String {
        let region = self.web_search_region.trim().to_ascii_lowercase();
        if region.len() == 5
            && region.as_bytes()[2] == b'-'
            && region
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 2 || byte.is_ascii_lowercase())
        {
            region
        } else {
            default_web_search_region()
        }
    }
}

impl AgentRequest {
    fn apply_deep_research(&mut self) {
        if !self.deep_research {
            return;
        }
        self.agent = true;
        self.skills.web_search = true;
        self.skills.fetch_url = true;
        self.skills.web_search_depth = WebSearchDepth::Deep;
        if self.skills.web_search_max_results < DEEP_RESEARCH_MIN_RESULTS {
            self.skills.web_search_max_results = DEEP_RESEARCH_MIN_RESULTS;
        }
    }

    fn normalize_force_tools(&mut self) {
        let mut seen = HashSet::new();
        self.force_tools.retain(|name| {
            let allowed = match name.as_str() {
                "web_search" => self.skills.web_search,
                "fetch_url" => self.skills.fetch_url,
                "ask_user" => self.deep_research,
                _ => false,
            };
            allowed && seen.insert(name.clone())
        });
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

    fn visible_text(&self) -> String {
        strip_think_blocks(&self.content).trim().to_string()
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
    request.apply_deep_research();
    request.normalize_force_tools();
    inject_agent_system_prompt(
        &mut request.messages,
        &request.skills,
        user_skills,
        request.deep_research,
        request.deep_research_output,
        &request.force_tools,
    );
    send_sse(
        tx,
        sse_agent(json!({
            "phase": "status",
            "message": if request.deep_research {
                "Deep research"
            } else if !request.force_tools.is_empty() {
                "Agent · required skills"
            } else {
                "Agent mode"
            }
        })),
    )
    .await?;

    let steer_id = new_steer_id();
    let (steer_tx, mut steer_rx) = mpsc::unbounded_channel();
    {
        let mut map = steer_sessions()
            .lock()
            .map_err(|_| StreamFail::Other("steer sessions lock poisoned".into()))?;
        map.insert(steer_id.clone(), steer_tx);
    }
    let _steer_guard = SteerSessionGuard {
        id: steer_id.clone(),
    };
    send_sse(
        tx,
        sse_agent(json!({
            "phase": "steer_ready",
            "id": steer_id,
        })),
    )
    .await?;

    let mut tool_rounds = 0usize;
    let mut empty_retries = 0usize;
    let mut force_retries = 0usize;
    let mut loop_notice_sent = false;
    let mut pending_force: HashSet<String> = request.force_tools.iter().cloned().collect();
    let mut await_clarify = request.deep_research;
    let mut deep_searches = 0usize;
    let search_cap = deep_research_search_cap(request.deep_research_output);

    loop {
        let steers = drain_steers(&mut steer_rx);
        if !steers.is_empty() {
            append_steer_messages(&mut request.messages, &steers);
            for payload in &steers {
                let mut event = json!({
                    "phase": "steer",
                    "content": payload.text,
                });
                if let Some(client_id) = &payload.client_id {
                    event["client_id"] = json!(client_id);
                }
                send_sse(tx, sse_agent(event)).await?;
            }
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "status",
                    "message": if steers.len() == 1 {
                        "Steering…".to_string()
                    } else {
                        format!("Steering ({} notes)…", steers.len())
                    },
                })),
            )
            .await?;
        }
        let force_final = request.deep_research
            && deep_searches > 0
            && (deep_searches >= search_cap || empty_retries > 0);
        let mut pending_list = pending_force_list(&pending_force, &request.force_tools);
        if await_clarify {
            pending_list = vec!["ask_user".into()];
        } else if force_final {
            pending_list.clear();
        } else if request.deep_research
            && deep_searches == 0
            && !pending_list.iter().any(|n| n == "web_search")
        {
            // After clarify (or if clarify somehow skipped), require at least one search.
            pending_list = vec!["web_search".into()];
        }
        let allow_tools = !force_final;
        let status_message = if await_clarify && tool_rounds == 0 && empty_retries == 0 {
            "Refining research goal…".to_string()
        } else if force_final {
            "Writing answer…".to_string()
        } else if request.deep_research {
            if tool_rounds == 0 && empty_retries == 0 && force_retries == 0 {
                "Researching…".to_string()
            } else {
                "Still researching…".to_string()
            }
        } else if !pending_list.is_empty() {
            format!("Required: {}", pending_list.join(", "))
        } else if tool_rounds == 0 && empty_retries == 0 {
            "Processing…".to_string()
        } else {
            "Continuing…".to_string()
        };
        send_sse(
            tx,
            sse_agent(json!({
                "phase": "status",
                "message": status_message
            })),
        )
        .await?;

        let turn = stream_once(
            api_base,
            api_key,
            style,
            request,
            user_skills,
            StreamOnceTools {
                pending_force: &pending_list,
                allow_tools,
            },
            tx,
        )
        .await?;

        if let Some(call) = turn.tool.clone() {
            if force_final && call.name != "ask_user" {
                tool_rounds += 1;
                let err = crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_FORCE_FINAL);
                let mut clear = json!({ "phase": "content_clear" });
                if !turn.content.trim().is_empty() {
                    clear["text"] = json!(turn.content);
                }
                if !turn.reasoning.trim().is_empty() {
                    clear["reasoning"] = json!(turn.reasoning);
                }
                send_sse(tx, sse_agent(clear)).await?;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "tool_call",
                        "name": call.name,
                        "arguments": call.arguments,
                    })),
                )
                .await?;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "tool_result",
                        "name": call.name,
                        "ok": false,
                        "preview": err,
                        "result": err,
                    })),
                )
                .await?;
                append_openai_tool_exchange(&mut request.messages, &turn, &call, err);
                continue;
            }
            if call.name == "ask_user" {
                if !request.deep_research {
                    return Err(StreamFail::Other(
                        "Capability 'ask_user' is only available in deep research.".into(),
                    ));
                }
                tool_rounds += 1;
                let mut clear = json!({ "phase": "content_clear" });
                if !turn.content.trim().is_empty() {
                    clear["text"] = json!(turn.content);
                }
                if !turn.reasoning.trim().is_empty() {
                    clear["reasoning"] = json!(turn.reasoning);
                }
                send_sse(tx, sse_agent(clear)).await?;

                let questions = normalize_ask_user_questions(&call.arguments);
                if questions.is_empty() {
                    let err = ask_user_format_error(&call.arguments);
                    send_sse(
                        tx,
                        sse_agent(json!({
                            "phase": "tool_call",
                            "name": "ask_user",
                            "arguments": call.arguments,
                        })),
                    )
                    .await?;
                    send_sse(
                        tx,
                        sse_agent(json!({
                            "phase": "tool_result",
                            "name": "ask_user",
                            "ok": false,
                            "preview": err,
                            "result": err,
                        })),
                    )
                    .await?;
                    append_openai_tool_exchange(&mut request.messages, &turn, &call, &err);
                    continue;
                }

                let clarify_id = new_tool_call_id();
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "clarify",
                        "id": clarify_id,
                        "questions": questions,
                    })),
                )
                .await?;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "status",
                        "message": "Waiting for your answers…"
                    })),
                )
                .await?;

                let answers = wait_for_clarify_answers(&clarify_id, tx).await?;
                let summary = format_clarify_answers_for_model(&questions, &answers);
                await_clarify = false;
                pending_force.insert("web_search".into());
                force_retries = 0;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "clarify_done",
                        "id": clarify_id,
                        "answers": answers,
                        "summary": summary,
                    })),
                )
                .await?;
                append_openai_tool_exchange(&mut request.messages, &turn, &call, &summary);
                continue;
            }

            if await_clarify {
                let err = "Deep research requires ask_user first (1–2 clarifying questions) before web_search or fetch_url.";
                tool_rounds += 1;
                let mut clear = json!({ "phase": "content_clear" });
                if !turn.content.trim().is_empty() {
                    clear["text"] = json!(turn.content);
                }
                if !turn.reasoning.trim().is_empty() {
                    clear["reasoning"] = json!(turn.reasoning);
                }
                send_sse(tx, sse_agent(clear)).await?;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "tool_call",
                        "name": call.name,
                        "arguments": call.arguments,
                    })),
                )
                .await?;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "tool_result",
                        "name": call.name,
                        "ok": false,
                        "preview": err,
                        "result": err,
                    })),
                )
                .await?;
                append_openai_tool_exchange(&mut request.messages, &turn, &call, err);
                continue;
            }

            if !capability_allowed(&call.name, &request.skills, user_skills) {
                return Err(StreamFail::Other(format!(
                    "Capability '{}' is not enabled.",
                    call.name
                )));
            }
            pending_force.remove(&call.name);
            force_retries = 0;

            tool_rounds += 1;
            let loop_after = if request.deep_research {
                DEEP_RESEARCH_TOOL_LOOP_NOTICE_AFTER
            } else {
                TOOL_LOOP_NOTICE_AFTER
            };
            if !loop_notice_sent && tool_rounds >= loop_after {
                loop_notice_sent = true;
                send_sse(
                    tx,
                    sse_agent(json!({
                        "phase": "notice",
                        "message": format!(
                            "{loop_after} tool steps so far — the model may be stuck looping. Press Stop if this isn't making progress."
                        ),
                    })),
                )
                .await?;
            }
            // Authoritative seal: mid-stream forwarding can miss preface text when
            // content and tool_calls share a delta. Always re-emit the resolved
            // non-tool content so the UI keeps "I'll search for…" narration.
            let mut clear = json!({ "phase": "content_clear" });
            if !turn.content.trim().is_empty() {
                clear["text"] = json!(turn.content);
            }
            if !turn.reasoning.trim().is_empty() {
                clear["reasoning"] = json!(turn.reasoning);
            }
            send_sse(tx, sse_agent(clear)).await?;
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "tool_call",
                    "name": call.name,
                    "arguments": call.arguments,
                })),
            )
            .await?;

            let outcome = match execute_tool(&call, &request.skills, user_skills).await {
                Ok(outcome) => outcome,
                Err(err) if call.name == "web_search" || call.name == "fetch_url" => {
                    // Never abort the whole turn on a single lookup miss — feed it
                    // back so the model can reformulate and continue researching.
                    let guidance = if call.name == "web_search" {
                        format!(
                            "{err}\n\nSearch failed. Do not stop. Try again with a different query (drop speculative dates), another backend, or kind=news vs web."
                        )
                    } else {
                        format!(
                            "{err}\n\nFetch failed. Try a different URL from prior search results, or search again."
                        )
                    };
                    ToolOutcome::soft_failure(guidance)
                }
                Err(err) => return Err(StreamFail::Other(err)),
            };
            if call.name == "web_search" && outcome.ok {
                deep_searches += 1;
            }
            let preview = outcome.ui_text.chars().take(240).collect::<String>();
            // Cap the UI payload; the model still receives the full `result` below.
            let ui_result: String = outcome.ui_text.chars().take(32_000).collect();
            let mut payload = json!({
                "phase": "tool_result",
                "name": call.name,
                "ok": outcome.ok,
                "preview": preview,
                "result": ui_result,
            });
            if let Some(note) = &outcome.note {
                payload["note"] = json!(note);
            }
            send_sse(tx, sse_agent(payload)).await?;

            let mut model_result = outcome.text;
            if request.deep_research {
                model_result.push_str(&deep_research_continue_note(
                    request.deep_research_output,
                    deep_searches,
                    search_cap,
                ));
            }
            append_openai_tool_exchange(&mut request.messages, &turn, &call, &model_result);
            continue;
        }

        let needs_deep_search = request.deep_research && deep_searches == 0;
        if await_clarify || !pending_force.is_empty() || needs_deep_search {
            if force_retries >= MAX_EMPTY_RETRIES {
                return Err(StreamFail::Other(if await_clarify {
                    "Deep research did not ask clarifying questions before answering.".into()
                } else if needs_deep_search {
                    "Deep research finished without searching the web.".into()
                } else {
                    format!(
                        "Agent did not use required skill(s): {}.",
                        pending_force_list(&pending_force, &request.force_tools).join(", ")
                    )
                }));
            }
            force_retries += 1;
            let pending = if await_clarify {
                vec!["ask_user".into()]
            } else if needs_deep_search {
                vec!["web_search".into()]
            } else {
                pending_force_list(&pending_force, &request.force_tools)
            };
            send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "status",
                    "message": if await_clarify {
                        "Waiting to refine the research goal…".into()
                    } else {
                        format!("Waiting for: {}", pending.join(", "))
                    }
                })),
            )
            .await?;
            request.messages.push(json!({
                "role": "assistant",
                "content": turn.assistant_text(),
            }));
            request.messages.push(json!({
                "role": "user",
                "content": if await_clarify {
                    crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_ASK_USER_FIRST)
                        .to_string()
                } else if needs_deep_search {
                    crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_MUST_SEARCH)
                        .to_string()
                } else {
                    crate::prompts::fill(
                        crate::prompts::agent::NUDGE_REQUIRED_TOOLS,
                        &[("tools", &pending.join(" and "))],
                    )
                },
            }));
            continue;
        }

        // Reasoning-only / think-only / blank replies used to end the turn empty in the UI.
        // Nudge the model to either call a tool or write a user-visible answer.
        if turn.visible_text().is_empty() {
            let answer_budget = if request.deep_research && deep_searches > 0 {
                MAX_ANSWER_RETRIES
            } else {
                MAX_EMPTY_RETRIES
            };
            if empty_retries >= answer_budget {
                return Err(StreamFail::Other(
                    "Agent produced no user-visible answer.".into(),
                ));
            }
            empty_retries += 1;
            send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "status",
                    "message": "Waiting for final answer…"
                })),
            )
            .await?;
            request.messages.push(json!({
                "role": "assistant",
                "content": turn.assistant_text(),
            }));
            request.messages.push(json!({
                "role": "user",
                "content": if request.deep_research && deep_searches > 0 {
                    crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_STOP_AND_ANSWER)
                        .to_string()
                } else if request.deep_research {
                    crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_EMPTY_DEEP).to_string()
                } else {
                    crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_EMPTY).to_string()
                },
            }));
            continue;
        }

        send_sse(tx, b"data: [DONE]\n\n".to_vec()).await?;
        return Ok(());
    }
}

fn pending_force_list(pending: &HashSet<String>, preferred_order: &[String]) -> Vec<String> {
    let mut list: Vec<String> = preferred_order
        .iter()
        .filter(|name| pending.contains(name.as_str()))
        .cloned()
        .collect();
    for name in pending {
        if !list.iter().any(|item| item == name) {
            list.push(name.clone());
        }
    }
    list
}

fn tool_choice_for_pending(pending: &[String]) -> Value {
    match pending {
        [] => json!("auto"),
        [name] => json!({
            "type": "function",
            "function": { "name": name }
        }),
        _ => json!("required"),
    }
}

fn capability_allowed(name: &str, skills: &AgentSkills, user_skills: &[UserSkill]) -> bool {
    match name {
        "web_search" => skills.web_search,
        "fetch_url" => skills.fetch_url,
        "ask_user" => true,
        "activate_skill" | "read_skill" => !user_skills.is_empty(),
        _ => false,
    }
}

pub fn should_run_agent(request: &AgentRequest, user_skills: &[UserSkill]) -> bool {
    if request.deep_research {
        return true;
    }
    request.agent && (request.skills.any_enabled() || !user_skills.is_empty())
}

fn inject_agent_system_prompt(
    messages: &mut Vec<Value>,
    skills: &AgentSkills,
    user_skills: &[UserSkill],
    deep_research: bool,
    deep_research_output: DeepResearchOutput,
    force_tools: &[String],
) {
    let mut block = agent_system_block(skills, user_skills, deep_research, force_tools);
    if deep_research {
        if !block.is_empty() {
            block.push_str("\n\n");
        }
        block.push_str(&deep_research_system_block(deep_research_output));
    }
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

fn agent_system_block(
    skills: &AgentSkills,
    user_skills: &[UserSkill],
    deep_research: bool,
    force_tools: &[String],
) -> String {
    use crate::prompts::{agent, fill, trim_prompt};

    let mut lines: Vec<String> = vec![
        trim_prompt(if deep_research {
            agent::INTRO_DEEP_RESEARCH
        } else {
            agent::INTRO
        })
        .to_string(),
        trim_prompt(agent::CORE).to_string(),
        trim_prompt(agent::STEER).to_string(),
    ];
    if !force_tools.is_empty() {
        lines.push(fill(
            agent::REQUIRED_TOOLS,
            &[("tools", &force_tools.join(", "))],
        ));
    } else if deep_research {
        lines.push(trim_prompt(agent::POLICY_DEEP_RESEARCH).to_string());
    } else {
        lines.push(trim_prompt(agent::POLICY_OPTIONAL).to_string());
    }
    if skills.web_search {
        let depth = skills.web_search_depth;
        let depth_note = match depth.scrape_plan() {
            None => trim_prompt(agent::WEB_SEARCH_DEPTH_OFF).to_string(),
            Some((pages, chars)) => {
                let pages = pages.to_string();
                let chars = chars.to_string();
                fill(
                    agent::WEB_SEARCH_DEPTH_ON,
                    &[
                        ("pages", &pages),
                        ("chars", &chars),
                        ("depth", depth.label()),
                    ],
                )
            }
        };
        let recency_note = match skills.web_search_recency {
            WebSearchRecency::Any => "any date".to_string(),
            value => format!("past {}", value.as_str()),
        };
        let max_results = skills.search_result_count().to_string();
        let region = skills.search_region();
        let mut web_line = fill(
            agent::WEB_SEARCH,
            &[
                ("backend", skills.web_search_backend.as_str()),
                ("region", &region),
                ("safesearch", skills.web_search_safesearch.as_str()),
                ("recency", &recency_note),
                ("max_results", &max_results),
                ("depth_note", &depth_note),
            ],
        );
        if skills.fetch_url {
            web_line.push(' ');
            web_line.push_str(trim_prompt(agent::WEB_SEARCH_FOLLOW_FETCH));
        }
        lines.push(web_line);
    }
    if skills.fetch_url {
        let max_chars = FETCH_URL_MAX_CHARS.to_string();
        let mut fetch_line = fill(agent::FETCH_URL, &[("max_chars", &max_chars)]);
        fetch_line.push(' ');
        if skills.web_search {
            fetch_line.push_str(trim_prompt(agent::FETCH_URL_WITH_SEARCH));
        } else {
            fetch_line.push_str(trim_prompt(agent::FETCH_URL_ALONE));
        }
        lines.push(fetch_line);
    }
    if skills.web_search || skills.fetch_url {
        lines.push(trim_prompt(agent::CITATIONS).to_string());
    }
    if !user_skills.is_empty() {
        lines.push(trim_prompt(agent::SKILLS_ACTIVATE).to_string());
        lines.push(crate::skills::user_skills_catalog_block(user_skills));
    }
    lines.join("\n")
}

fn deep_research_system_block(output: DeepResearchOutput) -> String {
    use crate::prompts::{agent, fill, trim_prompt};

    let output_line = match output {
        DeepResearchOutput::Long => trim_prompt(agent::DEEP_RESEARCH_OUTPUT_LONG),
        DeepResearchOutput::Brief => trim_prompt(agent::DEEP_RESEARCH_OUTPUT_BRIEF),
    };
    fill(agent::DEEP_RESEARCH, &[("output_line", output_line)])
}

fn deep_research_search_cap(output: DeepResearchOutput) -> usize {
    match output {
        DeepResearchOutput::Brief => DEEP_RESEARCH_BRIEF_SEARCH_CAP,
        DeepResearchOutput::Long => DEEP_RESEARCH_LONG_SEARCH_CAP,
    }
}

fn deep_research_continue_note(
    output: DeepResearchOutput,
    deep_searches: usize,
    search_cap: usize,
) -> String {
    use crate::prompts::{agent, trim_prompt};

    let body = if deep_searches >= search_cap {
        trim_prompt(agent::CONTINUE_ENOUGH)
    } else {
        match output {
            DeepResearchOutput::Long => trim_prompt(agent::CONTINUE_LONG),
            DeepResearchOutput::Brief => trim_prompt(agent::CONTINUE_BRIEF),
        }
    };
    format!("\n\n{body}")
}

pub fn inject_skill_catalog_into_messages(messages: &mut Vec<Value>, user_skills: &[UserSkill]) {
    let block = crate::skills::user_skills_catalog_block(user_skills);
    if block.is_empty() {
        return;
    }
    let note = format!(
        "{block}\n\n{}",
        crate::prompts::trim_prompt(crate::prompts::agent::SKILLS_NEED_AGENT)
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
/// we re-synthesize XML for `extract_tool_call`. Once a tool starts we stop
/// forwarding further content deltas, after first forwarding any preface text
/// (including content that shares a delta with `tool_calls`).
struct StreamOnceTools<'a> {
    pending_force: &'a [String],
    allow_tools: bool,
}

async fn stream_once(
    api_base: &str,
    api_key: Option<&str>,
    style: ApiStyle,
    request: &AgentRequest,
    user_skills: &[UserSkill],
    tools: StreamOnceTools<'_>,
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
        if tools.allow_tools {
            let tool_defs =
                openai_tools_payload(&request.skills, user_skills, request.deep_research);
            if !tool_defs.is_empty() {
                object.insert("tools".into(), Value::Array(tool_defs));
                object.insert(
                    "tool_choice".into(),
                    tool_choice_for_pending(tools.pending_force),
                );
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
        if let Some(reasoning) = &request.reasoning {
            object.insert("reasoning".into(), reasoning.clone());
        }
        if style == ApiStyle::Openai {
            object.insert("stream_options".into(), json!({ "include_usage": true }));
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

    if value.get("usage").is_some() || value.get("timings").is_some() {
        let mut out = serde_json::Map::new();
        out.insert("choices".into(), json!([]));
        if let Some(usage) = value.get("usage") {
            out.insert("usage".into(), usage.clone());
        }
        if let Some(timings) = value.get("timings") {
            out.insert("timings".into(), timings.clone());
        }
        if let Some(model) = value.get("model") {
            out.insert("model".into(), model.clone());
        }
        let frame = Value::Object(out);
        send_sse(tx, format!("data: {frame}\n\n").into_bytes()).await?;
    }

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

    // Forward visible content BEFORE handling tool_calls so a same-delta preface
    // (text + tool_calls) is not wiped by content_clear.
    if let Some(chunk) = delta_string(delta, "content")
        && !chunk.is_empty()
    {
        let before_len = content.len();
        content.push_str(chunk);

        if *forwarding {
            if let Some(tag_at) = content.find("<tool_call>") {
                // Forward only the portion of this chunk that precedes the tag.
                let forward_end = tag_at.saturating_sub(before_len);
                let prefix = &chunk[..forward_end];
                if !prefix.is_empty() {
                    let frame = json!({
                        "choices": [{ "delta": { "content": prefix }, "index": 0 }]
                    });
                    send_sse(tx, format!("data: {frame}\n\n").into_bytes()).await?;
                }
                *forwarding = false;
                send_sse(tx, sse_agent(json!({ "phase": "content_clear" }))).await?;
            } else {
                let frame = json!({
                    "choices": [{ "delta": { "content": chunk }, "index": 0 }]
                });
                send_sse(tx, format!("data: {frame}\n\n").into_bytes()).await?;
            }
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

    Ok(())
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

fn openai_tools_payload(
    skills: &AgentSkills,
    user_skills: &[UserSkill],
    deep_research: bool,
) -> Vec<Value> {
    use crate::prompts::{tools, trim_prompt};

    let mut tools_out = Vec::new();
    if deep_research {
        tools_out.push(json!({
            "type": "function",
            "function": {
                "name": "ask_user",
                "description": trim_prompt(tools::ASK_USER),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 2,
                            "description": "1–2 multiple-choice questions",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "header": {
                                        "type": "string",
                                        "description": "Short chip label (max ~12 characters), e.g. Scope, Audience, Time"
                                    },
                                    "question": {
                                        "type": "string",
                                        "description": "Full question shown to the user"
                                    },
                                    "options": {
                                        "type": "array",
                                        "minItems": 2,
                                        "maxItems": 4,
                                        "description": "2–4 choices; each is {label, description?}",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": { "type": "string" },
                                                "description": { "type": "string" }
                                            },
                                            "required": ["label"]
                                        }
                                    },
                                    "multiSelect": {
                                        "type": "boolean",
                                        "description": "If true, user may pick multiple options"
                                    }
                                },
                                "required": ["question", "options"]
                            }
                        }
                    },
                    "required": ["questions"]
                }
            }
        }));
    }
    if skills.web_search {
        let mut description = if deep_research {
            trim_prompt(tools::WEB_SEARCH_DEEP).to_string()
        } else {
            trim_prompt(tools::WEB_SEARCH).to_string()
        };
        if skills.fetch_url {
            description.push(' ');
            description.push_str(trim_prompt(tools::WEB_SEARCH_FETCH_SUFFIX));
        }
        tools_out.push(json!({
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
                        },
                        "backend": {
                            "type": "string",
                            "description": "Optional engine override: auto, duckduckgo, brave, bing, google, mojeek, startpage, yahoo, yandex, wikipedia"
                        },
                        "recency": {
                            "type": "string",
                            "description": "Optional recency: any, day, week, month, year"
                        },
                        "kind": {
                            "type": "string",
                            "description": "web (default) or news"
                        }
                    },
                    "required": ["query"]
                }
            }
        }));
    }
    if skills.fetch_url {
        let description = if skills.web_search {
            trim_prompt(tools::FETCH_URL_WITH_SEARCH)
        } else {
            trim_prompt(tools::FETCH_URL)
        };
        tools_out.push(json!({
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
        tools_out.push(json!({
            "type": "function",
            "function": {
                "name": "activate_skill",
                "description": trim_prompt(tools::ACTIVATE_SKILL),
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
    tools_out
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

async fn wait_for_clarify_answers(
    clarify_id: &str,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<Value, StreamFail> {
    let (answer_tx, mut answer_rx) = oneshot::channel();
    {
        let mut map = clarify_waiters()
            .lock()
            .map_err(|_| StreamFail::Other("clarify waiters lock poisoned".into()))?;
        map.insert(clarify_id.to_string(), answer_tx);
    }
    let _guard = ClarifyWaitGuard {
        id: clarify_id.to_string(),
    };

    let started = std::time::Instant::now();
    loop {
        if tx.is_closed() {
            return Err(StreamFail::Other("Clarification cancelled.".into()));
        }
        if started.elapsed() >= CLARIFY_WAIT_TIMEOUT {
            return Err(StreamFail::Other(
                "Timed out waiting for clarifying answers.".into(),
            ));
        }
        match timeout(Duration::from_millis(400), &mut answer_rx).await {
            Ok(Ok(answers)) => return Ok(answers),
            Ok(Err(_)) => {
                return Err(StreamFail::Other("Clarification cancelled.".into()));
            }
            Err(_) => continue,
        }
    }
}

fn coerce_ask_user_args(args: &Value) -> Value {
    if args.get("questions").is_some() {
        return args.clone();
    }
    if let Some(raw) = args.get("raw").and_then(|v| v.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
            return coerce_ask_user_args(&parsed);
        }
        if let Some(start) = raw.find('{')
            && let Some(end) = raw.rfind('}')
            && end > start
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw[start..=end])
        {
            return coerce_ask_user_args(&parsed);
        }
    }
    if args.as_array().is_some() {
        return json!({ "questions": args });
    }
    if args.get("question").is_some() || args.get("prompt").is_some() || args.get("text").is_some()
    {
        return json!({ "questions": [args] });
    }
    args.clone()
}

fn option_label_description(opt: &Value) -> Option<(String, String)> {
    match opt {
        Value::String(s) => {
            let label = s.trim();
            if label.is_empty() {
                None
            } else {
                Some((label.to_string(), String::new()))
            }
        }
        Value::Object(map) => {
            let label = ["label", "text", "name", "title", "value"]
                .iter()
                .find_map(|key| {
                    map.get(*key)
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })?;
            let description = ["description", "desc", "detail", "subtitle"]
                .iter()
                .find_map(|key| {
                    map.get(*key)
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            Some((label, description))
        }
        _ => None,
    }
}

fn normalize_ask_user_questions(args: &Value) -> Vec<Value> {
    let args = coerce_ask_user_args(args);
    let Some(raw) = args.get("questions") else {
        return Vec::new();
    };
    let items: Vec<&Value> = match raw {
        Value::Array(list) => list.iter().take(2).collect(),
        Value::Object(_) => vec![raw],
        _ => return Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let question = ["question", "prompt", "text", "q"].iter().find_map(|key| {
                item.get(*key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })?;
            let options_raw = ["options", "choices", "answers", "choices_list"]
                .iter()
                .find_map(|key| item.get(*key).and_then(|v| v.as_array()))?;
            let options = options_raw
                .iter()
                .filter_map(|opt| {
                    let (label, description) = option_label_description(opt)?;
                    Some(json!({
                        "label": label,
                        "description": description,
                    }))
                })
                .take(4)
                .collect::<Vec<_>>();
            if options.len() < 2 {
                return None;
            }
            let header = ["header", "title", "label", "id"]
                .iter()
                .find_map(|key| {
                    item.get(*key)
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.chars().take(12).collect::<String>())
                })
                .unwrap_or_else(|| "Question".into());
            let multi = item
                .get("multiSelect")
                .or_else(|| item.get("multi_select"))
                .or_else(|| item.get("allow_multiple"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(json!({
                "header": header,
                "question": question,
                "options": options,
                "multiSelect": multi,
            }))
        })
        .collect()
}

fn ask_user_format_error(args: &Value) -> String {
    let hint = "Retry ask_user with {\"questions\":[{\"header\":\"Scope\",\"question\":\"…?\",\"options\":[{\"label\":\"A\",\"description\":\"…\"},{\"label\":\"B\",\"description\":\"…\"}]}]} — 1–2 questions, each with 2–4 options that have a label.";
    let coerced = coerce_ask_user_args(args);
    if coerced.get("questions").is_none() {
        return format!("ask_user missing questions array. {hint}");
    }
    format!(
        "ask_user questions were incomplete (need question text + at least 2 labeled options each). {hint}"
    )
}

fn format_clarify_answers_for_model(questions: &[Value], answers: &Value) -> String {
    let mut lines = vec!["User clarifying answers:".to_string()];
    if let Some(map) = answers.as_object() {
        for (idx, question) in questions.iter().enumerate() {
            let key = question
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let header = question
                .get("header")
                .and_then(|v| v.as_str())
                .unwrap_or("Question");
            let value = map
                .get(key)
                .or_else(|| map.get(&idx.to_string()))
                .cloned()
                .unwrap_or(Value::Null);
            let rendered = match value {
                Value::String(s) => s,
                Value::Array(items) => items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            let rendered = rendered.trim();
            if rendered.is_empty() {
                lines.push(format!("- {header}: (no answer)"));
            } else {
                lines.push(format!("- {header} ({key}): {rendered}"));
            }
        }
        if let Some(extra) = map.get("response").and_then(|v| v.as_str()) {
            let extra = extra.trim();
            if !extra.is_empty() {
                lines.push(format!("- Additional note: {extra}"));
            }
        }
    } else if let Some(text) = answers.as_str() {
        lines.push(text.to_string());
    } else {
        lines.push(answers.to_string());
    }
    lines.push(
        "Proceed with deep research using these preferences. Call web_search / fetch_url as needed."
            .into(),
    );
    lines.join("\n")
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
    /// False when the tool failed softly and the model should try another approach.
    ok: bool,
}

impl ToolOutcome {
    fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            ui_text: text.clone(),
            text,
            note: None,
            ok: true,
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
            ok: true,
        }
    }

    fn soft_failure(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            ui_text: text.clone(),
            text,
            note: Some("retry with a different query".into()),
            ok: false,
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
            let (search_skills, kind) = search_call_overrides(skills, &call.arguments);
            let (result, note) = ddgs_web_search(query, &search_skills, kind).await?;
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

#[derive(Deserialize)]
struct DdgsSearchResponse {
    results: Vec<DdgsSearchHit>,
}

#[derive(Deserialize)]
struct DdgsSearchHit {
    title: String,
    url: String,
    snippet: String,
}

fn parse_enum_arg<T: for<'de> Deserialize<'de>>(args: &Value, key: &str) -> Option<T> {
    let raw = args.get(key)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    serde_json::from_value(json!(raw.to_ascii_lowercase())).ok()
}

fn search_call_overrides(skills: &AgentSkills, args: &Value) -> (AgentSkills, WebSearchKind) {
    let mut next = skills.clone();
    if let Some(backend) = parse_enum_arg::<WebSearchBackend>(args, "backend") {
        next.web_search_backend = backend;
    }
    if let Some(recency) = parse_enum_arg::<WebSearchRecency>(args, "recency") {
        next.web_search_recency = recency;
    }
    let kind = parse_enum_arg::<WebSearchKind>(args, "kind").unwrap_or_default();
    (next, kind)
}

async fn ddgs_web_search(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<(String, String), String> {
    let result_count = skills.search_result_count();
    let mut last_error = None::<String>;

    for attempt in 1..=DDGS_ATTEMPTS {
        match ddgs_search_hits(query, skills, kind).await {
            Ok(hits) if !hits.is_empty() => {
                let kind_label = kind.label();
                let mut out = format!(
                    "{} search results for {query:?} (DDGS, {kind_label}, {}, {}, {}):\n",
                    if kind == WebSearchKind::News {
                        "News"
                    } else {
                        "Web"
                    },
                    skills.web_search_backend.as_str(),
                    skills.search_region(),
                    skills.web_search_safesearch.as_str(),
                );
                for (index, hit) in hits.iter().take(result_count).enumerate() {
                    out.push_str(&format!(
                        "\n{}. {}\n   URL: {}\n   {}\n",
                        index + 1,
                        hit.title,
                        hit.url,
                        hit.snippet
                    ));
                }
                append_scraped_pages(&mut out, &hits, skills.web_search_depth).await;
                return Ok((
                    out,
                    format!(
                        "via DDGS · {kind_label} · {}",
                        skills.web_search_backend.as_str()
                    ),
                ));
            }
            Ok(_) => {
                last_error = Some("No results found.".into());
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
        if attempt < DDGS_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(DDGS_RETRY_DELAY_MS * attempt as u64)).await;
        }
    }

    let detail = last_error.unwrap_or_else(|| "unknown search error".into());
    // Soft failure: caller / agent loop should keep going with a reformulated query.
    Err(format!(
        "DDGS search failed after {DDGS_ATTEMPTS} attempts: {detail}"
    ))
}

async fn ddgs_search_hits(
    query: &str,
    skills: &AgentSkills,
    kind: WebSearchKind,
) -> Result<Vec<SearchHit>, String> {
    let result_count = skills.search_result_count();
    let request = serde_json::to_vec(&json!({
        "protocol": 1,
        "query": query,
        "max_results": result_count,
        "backend": skills.web_search_backend.as_str(),
        "region": skills.search_region(),
        "safesearch": skills.web_search_safesearch.as_str(),
        "recency": skills.web_search_recency.as_str(),
        "kind": kind.label(),
    }))
    .map_err(|error| format!("Could not encode DDGS request: {error}"))?;

    let mut candidates: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
    let helper_name = if cfg!(windows) {
        "tensorui-search.exe"
    } else {
        "tensorui-search"
    };

    // Release archives ship a self-contained helper beside TensorMI Harness. The env
    // override and app-data location also leave room for managed/helper updates.
    if let Some(path) = std::env::var_os("TENSORUI_SEARCH_HELPER").filter(|v| !v.is_empty()) {
        candidates.push((path.into(), Vec::new()));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push((directory.join(helper_name), Vec::new()));
    }
    candidates.push((
        crate::store::data_dir()
            .join("search-helper")
            .join(helper_name),
        Vec::new(),
    ));

    // Source-development fallback. Unlike CARGO_MANIFEST_DIR, this does not
    // bake a developer machine's absolute repository path into release builds.
    let working_directory = std::env::current_dir().unwrap_or_else(|_| ".".into());
    if cfg!(windows) {
        candidates.push((
            working_directory
                .join(".venv")
                .join("Scripts")
                .join("python.exe"),
            vec!["-c".into(), include_str!("ddgs_search.py").into()],
        ));
        candidates.push((
            "py".into(),
            vec![
                "-3".into(),
                "-c".into(),
                include_str!("ddgs_search.py").into(),
            ],
        ));
        candidates.push((
            "python".into(),
            vec!["-c".into(), include_str!("ddgs_search.py").into()],
        ));
    } else {
        candidates.push((
            working_directory.join(".venv").join("bin").join("python"),
            vec!["-c".into(), include_str!("ddgs_search.py").into()],
        ));
        candidates.push((
            "python3".into(),
            vec!["-c".into(), include_str!("ddgs_search.py").into()],
        ));
    }

    let mut found_runtime = false;
    let mut missing_ddgs = false;
    let mut unsupported_python = false;
    let mut missing_python_runtime = false;
    let mut runtime_error = None;
    for (program, prefix) in candidates {
        let child = Command::new(&program)
            .args(prefix)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        let mut child = match child {
            Ok(child) => {
                found_runtime = true;
                child
            }
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!("Could not start {}: {error}", program.display()));
            }
        };

        let mut stdin = child.stdin.take().ok_or_else(|| {
            format!(
                "Could not open stdin for search helper {}",
                program.display()
            )
        })?;
        stdin
            .write_all(&request)
            .await
            .map_err(|error| format!("Could not send request to {}: {error}", program.display()))?;
        drop(stdin);

        let output = tokio::time::timeout(DDGS_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| "DDGS search timed out after 45 seconds.".to_string())?
            .map_err(|error| format!("Search helper {} failed: {error}", program.display()))?;

        if output.status.success() {
            let response: DdgsSearchResponse = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("Invalid DDGS response: {error}"))?;
            return Ok(response
                .results
                .into_iter()
                .filter(|hit| !hit.title.trim().is_empty() && scrapeable_url(&hit.url))
                .take(result_count)
                .map(|hit| SearchHit {
                    title: hit.title,
                    url: hit.url,
                    snippet: hit.snippet,
                })
                .collect());
        }

        let detail = String::from_utf8_lossy(&output.stderr);
        if detail.contains("TENSORUI_DDGS_NOT_INSTALLED") {
            missing_ddgs = true;
            continue;
        }
        if detail.contains("TENSORUI_DDGS_PYTHON_VERSION") {
            unsupported_python = true;
            continue;
        }
        let lower_detail = detail.to_ascii_lowercase();
        if lower_detail.contains("no installed python found")
            || lower_detail.contains("python was not found")
            || lower_detail.contains("no suitable python runtime found")
        {
            missing_python_runtime = true;
            continue;
        }
        let message = detail
            .strip_prefix("TENSORUI_DDGS_ERROR:")
            .unwrap_or(detail.trim())
            .trim();
        runtime_error = Some(truncate_chars(message, 360));
    }

    if let Some(error) = runtime_error {
        return Err(format!("DDGS search failed: {error}"));
    }
    if missing_ddgs {
        return Err(ddgs_setup_message("The `ddgs` package is not installed."));
    }
    if unsupported_python {
        return Err(ddgs_setup_message("Python 3.10 or newer is required."));
    }
    if missing_python_runtime {
        return Err(ddgs_setup_message(
            "The bundled search helper was not found and Python was not available.",
        ));
    }
    if !found_runtime {
        return Err(ddgs_setup_message(
            "The bundled search helper and a compatible Python runtime were not found.",
        ));
    }
    Err(ddgs_setup_message(
        "No usable search helper or Python installation was found.",
    ))
}

fn ddgs_setup_message(reason: &str) -> String {
    format!(
        "{reason} Official release archives include `tensorui-search` beside the main executable. For a source checkout, install Python 3.10+ and set up DDGS: Windows: `py -3 -m venv .venv` then `.venv\\Scripts\\python -m pip install -r requirements-search.txt`; macOS/Linux: `python3 -m venv .venv` then `./.venv/bin/python -m pip install -r requirements-search.txt`."
    )
}

/* Retired direct DuckDuckGo HTTP implementation. DDGS now owns provider selection,
request handling, parsing, and failover.
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

*/
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

/* Retired direct DuckDuckGo HTML parsers.
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

*/
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

/* Redirect decoder used only by the retired direct parser.
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

*/
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
    fn strip_tool_call_keeps_preface_text() {
        let text = "I'll look that up.\n<tool_call>\n{\"name\":\"web_search\",\"arguments\":{\"query\":\"x\"}}\n</tool_call>";
        assert_eq!(strip_tool_call_xml(text), "I'll look that up.");
    }

    #[test]
    fn resolve_tool_turn_keeps_preface_in_content() {
        let mut slots = Vec::new();
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "id": "call_1",
                "type": "function",
                "function": { "name": "web_search", "arguments": "{\"query\":\"x\"}" }
            }),
        );
        let turn = resolve_streamed_turn("", "Checking sources first.", &slots);
        assert!(turn.tool.is_some());
        assert_eq!(turn.content, "Checking sources first.");
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
        let tools = openai_tools_payload(&skills, &[], false);
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

    #[test]
    fn deep_research_enables_agent_and_search() {
        let mut request: AgentRequest = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "deep_research": true,
            "deep_research_output": "brief"
        }))
        .unwrap();
        assert!(should_run_agent(&request, &[]));
        request.apply_deep_research();
        assert!(request.skills.web_search);
        assert!(request.skills.fetch_url);
        assert_eq!(request.skills.web_search_depth, WebSearchDepth::Deep);
        assert!(request.skills.web_search_max_results >= DEEP_RESEARCH_MIN_RESULTS);
        assert_eq!(request.deep_research_output, DeepResearchOutput::Brief);
    }

    #[test]
    fn web_search_accepts_per_call_overrides() {
        let skills = AgentSkills::default();
        let args = json!({
            "query": "x",
            "backend": "google",
            "recency": "week",
            "kind": "news"
        });
        let (next, kind) = search_call_overrides(&skills, &args);
        assert_eq!(next.web_search_backend, WebSearchBackend::Google);
        assert_eq!(next.web_search_recency, WebSearchRecency::Week);
        assert_eq!(kind, WebSearchKind::News);
    }

    #[test]
    fn web_search_tool_exposes_search_method_args() {
        let skills = AgentSkills {
            web_search: true,
            ..AgentSkills::default()
        };
        let tools = openai_tools_payload(&skills, &[], true);
        let search = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "web_search")
            .expect("web_search tool");
        let props = &search["function"]["parameters"]["properties"];
        assert!(props.get("backend").is_some());
        assert!(props.get("recency").is_some());
        assert!(props.get("kind").is_some());
        let description = search["function"]["description"].as_str().unwrap();
        assert!(description.contains("kind=news"));
    }

    #[test]
    fn agent_request_deserializes_without_force_tools() {
        let request: AgentRequest = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": "hi"}],
            "agent": true,
            "skills": { "web_search": true },
            "deep_research": true
        }))
        .expect("force_tools should default");
        assert!(request.force_tools.is_empty());
        assert!(should_run_agent(&request, &[]));
    }

    #[test]
    fn normalize_force_tools_keeps_enabled_only() {
        let mut request: AgentRequest = serde_json::from_value(json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "agent": true,
            "skills": { "web_search": true, "fetch_url": false },
            "force_tools": ["web_search", "fetch_url", "web_search", "nope"]
        }))
        .unwrap();
        request.normalize_force_tools();
        assert_eq!(request.force_tools, vec!["web_search".to_string()]);
    }

    #[test]
    fn tool_choice_requires_forced_skills() {
        assert_eq!(tool_choice_for_pending(&[]), json!("auto"));
        assert_eq!(
            tool_choice_for_pending(&["web_search".into()]),
            json!({
                "type": "function",
                "function": { "name": "web_search" }
            })
        );
        assert_eq!(
            tool_choice_for_pending(&["web_search".into(), "fetch_url".into()]),
            json!("required")
        );
    }

    #[test]
    fn visible_text_ignores_think_only_content() {
        let turn = StreamedTurn {
            content: "<think>planning only</think>".into(),
            reasoning: String::new(),
            tool: None,
        };
        assert!(turn.visible_text().is_empty());
        let turn = StreamedTurn {
            content: "<think>plan</think>\nAndrew Tate is…".into(),
            reasoning: String::new(),
            tool: None,
        };
        assert!(turn.visible_text().contains("Andrew Tate"));
    }

    #[test]
    fn ask_user_accepts_common_alternate_shapes() {
        let string_opts = json!({
            "questions": [{
                "header": "Scope",
                "question": "What should we cover?",
                "options": ["Romania case", "UK proceedings", "Overview"]
            }]
        });
        let normalized = normalize_ask_user_questions(&string_opts);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0]["options"].as_array().unwrap().len(), 3);
        assert_eq!(normalized[0]["options"][0]["label"], "Romania case");

        let top_level = json!({
            "prompt": "How recent?",
            "choices": [
                {"text": "Past week"},
                {"name": "Past year"}
            ]
        });
        assert_eq!(normalize_ask_user_questions(&top_level).len(), 1);

        let raw = json!({
            "raw": "{\"questions\":[{\"question\":\"Focus?\",\"options\":[{\"label\":\"A\"},{\"label\":\"B\"}]}]}"
        });
        assert_eq!(normalize_ask_user_questions(&raw).len(), 1);
    }

    #[test]
    fn agent_prompt_marks_required_tools() {
        let skills = AgentSkills {
            web_search: true,
            ..AgentSkills::default()
        };
        let block = agent_system_block(&skills, &[], false, &["web_search".into()]);
        assert!(block.contains("Required tools for this turn: web_search"));
        let optional = agent_system_block(&skills, &[], false, &[]);
        assert!(optional.contains("Use them when needed"));
    }
}
