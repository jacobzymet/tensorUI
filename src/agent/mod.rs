//! Agent mode: OpenAI-compatible `tool_calls` + `role: tool` (Anthropic via translator).
//! XML `<tool_call>` in content is accepted only as a fallback for local models.

pub mod browser;
pub mod chat;
pub mod fs;
pub mod media;
pub mod search;
pub mod skills;
pub mod terminal;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};

use dom_smoothie::{Config, Readability, TextMode};
use futures_util::{StreamExt, future::join_all, stream};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
const MAX_CONCURRENT_TOOL_CALLS: usize = 8;
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

fn approval_waiters() -> &'static Mutex<HashMap<String, oneshot::Sender<bool>>> {
    static WAITERS: OnceLock<Mutex<HashMap<String, oneshot::Sender<bool>>>> = OnceLock::new();
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

pub fn submit_tool_approval(id: &str, allow: bool) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("approval id is required".into());
    }
    let sender = approval_waiters()
        .lock()
        .map_err(|_| "approval waiters lock poisoned".to_string())?
        .remove(id)
        .ok_or_else(|| "No pending tool approval for that id (it may have expired).".to_string())?;
    sender
        .send(allow)
        .map_err(|_| "Agent turn is no longer waiting for approval.".to_string())
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

struct ApprovalWaitGuard {
    id: String,
}

impl Drop for ApprovalWaitGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = approval_waiters().lock() {
            map.remove(&self.id);
        }
    }
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

struct EphemeralBrowserGuard {
    id: Option<String>,
}

impl Drop for EphemeralBrowserGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            tokio::spawn(async move {
                let _ = browser::close_session(&id).await;
            });
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
const DEFAULT_SEARCH_RESULTS: usize = 6;
const MAX_SEARCH_RESULTS: usize = 20;
const MAX_PAGE_BYTES: u64 = 1_500_000;
const DEFAULT_FETCH_URL_MAX_CHARS: usize = 8_000;
const MIN_PAGE_FETCH_CHARS: usize = 1_000;
const MAX_PAGE_FETCH_CHARS: usize = 200_000;
const PAGE_FETCH_TIMEOUT: Duration = Duration::from_secs(8);

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
pub enum WebSearchDepth {
    #[default]
    Off,
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
    /// Accepted for saved settings only; searches are remapped away from Yandex.
    Yandex,
    Wikipedia,
}

/// Which upstream the agent uses for `web_search`.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    /// SearXNG when configured, otherwise DuckDuckGo HTML/Lite.
    #[default]
    Auto,
    Duckduckgo,
    Searxng,
    /// Parallel Search API (`https://api.parallel.ai/v1/search`).
    Parallel,
    /// TinyFish Search API (`https://api.search.tinyfish.ai`), free via Monid partnership.
    Tinyfish,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchParallelMode {
    Turbo,
    #[default]
    Fast,
    Basic,
    Advanced,
}

impl WebSearchParallelMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Turbo => "turbo",
            Self::Fast => "fast",
            Self::Basic => "basic",
            Self::Advanced => "advanced",
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

fn default_fetch_url_max_chars() -> usize {
    DEFAULT_FETCH_URL_MAX_CHARS
}

fn default_terminal_timeout_secs() -> u64 {
    30
}

fn default_approval_mode() -> ApprovalMode {
    ApprovalMode::Manual
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Manual,
    AutoSafe,
}

fn clamp_page_fetch_chars(value: usize) -> usize {
    value.clamp(MIN_PAGE_FETCH_CHARS, MAX_PAGE_FETCH_CHARS)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentSkills {
    #[serde(default)]
    pub web_search: bool,
    #[serde(default)]
    pub web_search_depth: WebSearchDepth,
    #[serde(default)]
    pub web_search_backend: WebSearchBackend,
    #[serde(default)]
    pub web_search_provider: WebSearchProvider,
    #[serde(default)]
    pub web_search_searxng: String,
    #[serde(default)]
    pub web_search_parallel_api_key: String,
    #[serde(default)]
    pub web_search_parallel_mode: WebSearchParallelMode,
    #[serde(default)]
    pub web_search_tinyfish_api_key: String,
    #[serde(default = "default_web_search_max_results")]
    pub web_search_max_results: usize,
    #[serde(default = "default_web_search_region")]
    pub web_search_region: String,
    #[serde(default)]
    pub web_search_safesearch: WebSearchSafeSearch,
    #[serde(default)]
    pub web_search_recency: WebSearchRecency,
    /// When non-zero, overrides per-depth search page scrape character caps.
    #[serde(default)]
    pub web_search_page_max_chars: usize,
    #[serde(default)]
    pub fetch_url: bool,
    #[serde(default = "default_fetch_url_max_chars")]
    pub fetch_url_max_chars: usize,
    #[serde(default = "default_approval_mode")]
    pub approval_mode: ApprovalMode,
    #[serde(default)]
    pub filesystem: bool,
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub terminal: bool,
    #[serde(default = "default_terminal_timeout_secs")]
    pub terminal_timeout_secs: u64,
    #[serde(default)]
    pub browser: bool,
    /// Set by the server from the live conversation id. Ignored if the client sends it.
    #[serde(default, skip_deserializing)]
    pub session_id: String,
}

impl Default for AgentSkills {
    fn default() -> Self {
        Self {
            web_search: false,
            web_search_depth: WebSearchDepth::default(),
            web_search_backend: WebSearchBackend::default(),
            web_search_provider: WebSearchProvider::default(),
            web_search_searxng: String::new(),
            web_search_parallel_api_key: String::new(),
            web_search_parallel_mode: WebSearchParallelMode::default(),
            web_search_tinyfish_api_key: String::new(),
            web_search_max_results: default_web_search_max_results(),
            web_search_region: default_web_search_region(),
            web_search_safesearch: WebSearchSafeSearch::default(),
            web_search_recency: WebSearchRecency::default(),
            web_search_page_max_chars: 0,
            fetch_url: false,
            fetch_url_max_chars: default_fetch_url_max_chars(),
            approval_mode: ApprovalMode::Manual,
            filesystem: false,
            workspace_root: String::new(),
            terminal: false,
            terminal_timeout_secs: default_terminal_timeout_secs(),
            browser: false,
            session_id: String::new(),
        }
    }
}

impl AgentSkills {
    pub fn any_enabled(&self) -> bool {
        self.web_search || self.fetch_url || self.filesystem || self.terminal || self.browser
    }

    fn filesystem_ready(&self) -> bool {
        self.filesystem && !self.workspace_root.trim().is_empty()
    }

    fn terminal_ready(&self) -> bool {
        self.terminal && !self.workspace_root.trim().is_empty()
    }

    fn fetch_url_char_limit(&self) -> usize {
        clamp_page_fetch_chars(if self.fetch_url_max_chars == 0 {
            DEFAULT_FETCH_URL_MAX_CHARS
        } else {
            self.fetch_url_max_chars
        })
    }

    fn search_scrape_plan(&self) -> Option<(usize, usize)> {
        self.web_search_depth.scrape_plan().map(|(pages, preset)| {
            let chars = if self.web_search_page_max_chars == 0 {
                preset
            } else {
                clamp_page_fetch_chars(self.web_search_page_max_chars)
            };
            (pages, chars)
        })
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
    tools: Vec<ToolCall>,
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
    results: &[(ToolCall, String)],
) {
    if results.is_empty() {
        return;
    }
    let content = if turn.content.trim().is_empty() {
        Value::Null
    } else {
        Value::String(turn.content.clone())
    };
    let tool_calls: Vec<Value> = results
        .iter()
        .map(|(call, _)| {
            let arguments = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
            json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": arguments
                }
            })
        })
        .collect();
    messages.push(json!({
        "role": "assistant",
        "content": content,
        "tool_calls": tool_calls
    }));
    for (call, result) in results {
        messages.push(json!({
            "role": "tool",
            "tool_call_id": call.id,
            "name": call.name,
            "content": result
        }));
    }
}

fn is_recoverable_tool(name: &str) -> bool {
    matches!(
        name,
        "web_search"
            | "fetch_url"
            | "read_file"
            | "list_dir"
            | "glob"
            | "grep"
            | "write_file"
            | "str_replace"
            | "delete_file"
            | "run_terminal"
            | "browser_navigate"
            | "browser_snapshot"
            | "browser_click"
            | "browser_type"
            | "browser_press"
            | "browser_wait"
            | "browser_screenshot"
            | "browser_evaluate"
            | "browser_close"
            | "show_image"
    )
}

fn lookup_retry_guidance(name: &str, err: &str) -> String {
    if name == "web_search" {
        format!(
            "{err}\n\nSearch did not return results. You may try one simpler query, or continue without live sources."
        )
    } else if name == "fetch_url" {
        format!(
            "{err}\n\nFetch failed. Try a different URL from prior search results, or search again."
        )
    } else {
        format!("{err}\n\nFix the arguments and retry, or continue without this tool.")
    }
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
    allow_insecure_tls: bool,
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
            allow_insecure_tls,
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
    allow_insecure_tls: bool,
    request: &mut AgentRequest,
    user_skills: &[UserSkill],
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail> {
    request.apply_deep_research();
    request.normalize_force_tools();
    let ephemeral_browser = request.skills.browser && request.skills.session_id.trim().is_empty();
    if ephemeral_browser {
        request.skills.session_id = format!("ephemeral_{}", new_tool_call_id());
    }
    let _browser_guard = EphemeralBrowserGuard {
        id: ephemeral_browser.then(|| request.skills.session_id.clone()),
    };
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
                allow_insecure_tls,
            },
            tx,
        )
        .await?;

        if !turn.tools.is_empty() {
            let calls = turn.tools.clone();
            send_content_clear(tx, &turn).await?;

            if force_final && !calls.iter().any(|call| call.name == "ask_user") {
                tool_rounds += calls.len();
                let err = crate::prompts::trim_prompt(crate::prompts::agent::NUDGE_FORCE_FINAL);
                let mut results = Vec::new();
                for call in &calls {
                    send_tool_call(tx, call, false).await?;
                    let outcome = ToolOutcome::soft_failure(err);
                    send_tool_result(tx, call, &outcome).await?;
                    results.push((call.clone(), err.to_string()));
                }
                append_openai_tool_exchange(&mut request.messages, &turn, &results);
                continue;
            }

            if let Some(ask_idx) = calls.iter().position(|call| call.name == "ask_user") {
                if !request.deep_research {
                    return Err(StreamFail::Other(
                        "Capability 'ask_user' is only available in deep research.".into(),
                    ));
                }
                tool_rounds += calls.len();
                let ask = &calls[ask_idx];
                let questions = normalize_ask_user_questions(&ask.arguments);
                if questions.is_empty() {
                    let err = ask_user_format_error(&ask.arguments);
                    let mut results = Vec::new();
                    for call in &calls {
                        send_tool_call(tx, call, false).await?;
                        let text = if call.name == "ask_user" {
                            err.clone()
                        } else {
                            "ask_user failed this turn; retry this tool after clarifying questions."
                                .into()
                        };
                        let outcome = ToolOutcome::soft_failure(&text);
                        send_tool_result(tx, call, &outcome).await?;
                        results.push((call.clone(), text));
                    }
                    append_openai_tool_exchange(&mut request.messages, &turn, &results);
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
                let mut results = Vec::new();
                for (index, call) in calls.iter().enumerate() {
                    let text = if index == ask_idx {
                        summary.clone()
                    } else {
                        "Clarifying questions were asked this turn. Call this tool again after the user answers.".into()
                    };
                    results.push((call.clone(), text));
                }
                append_openai_tool_exchange(&mut request.messages, &turn, &results);
                continue;
            }

            if await_clarify {
                let err = "Deep research requires ask_user first (1–2 clarifying questions) before web_search or fetch_url.";
                tool_rounds += calls.len();
                let mut results = Vec::new();
                for call in &calls {
                    send_tool_call(tx, call, false).await?;
                    let outcome = ToolOutcome::soft_failure(err);
                    send_tool_result(tx, call, &outcome).await?;
                    results.push((call.clone(), err.to_string()));
                }
                append_openai_tool_exchange(&mut request.messages, &turn, &results);
                continue;
            }

            for call in &calls {
                if !capability_allowed(&call.name, &request.skills, user_skills) {
                    return Err(StreamFail::Other(format!(
                        "Capability '{}' is not enabled.",
                        call.name
                    )));
                }
                pending_force.remove(&call.name);
            }
            force_retries = 0;

            tool_rounds += calls.len();
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

            let mut approval_rx = HashMap::new();
            for call in &calls {
                if needs_approval(&call.name, request.skills.approval_mode) {
                    approval_rx.insert(call.id.clone(), arm_tool_approval(&call.id)?);
                }
            }
            for call in &calls {
                send_tool_call(tx, call, approval_rx.contains_key(&call.id)).await?;
            }

            let executed = approve_and_execute_tools(
                calls,
                &request.skills,
                user_skills,
                tx,
                approval_rx,
            )
            .await?;
            let mut results = Vec::new();
            for (call, outcome) in executed {
                if call.name == "web_search" && outcome.ok {
                    deep_searches += 1;
                }
                let mut model_result = outcome.text;
                if request.deep_research {
                    model_result.push_str(&deep_research_continue_note(
                        request.deep_research_output,
                        deep_searches,
                        search_cap,
                    ));
                }
                results.push((call, model_result));
            }
            append_openai_tool_exchange(&mut request.messages, &turn, &results);
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
        "read_file" | "list_dir" | "glob" | "grep" | "write_file" | "str_replace" | "delete_file" => {
            skills.filesystem_ready()
        }
        "run_terminal" => skills.terminal_ready(),
        name if browser::is_browser_tool(name) => skills.browser,
        "show_image" => true,
        _ => false,
    }
}

fn tool_risk(name: &str) -> &'static str {
    match name {
        "write_file" | "str_replace" | "delete_file" => "write",
        "run_terminal" => "terminal",
        name if browser::is_browser_tool(name) => "browser",
        _ => "safe",
    }
}

fn needs_approval(name: &str, mode: ApprovalMode) -> bool {
    if name == "ask_user" {
        return false;
    }
    match mode {
        ApprovalMode::Manual => true,
        ApprovalMode::AutoSafe => matches!(tool_risk(name), "write" | "terminal" | "browser"),
    }
}

fn tool_call_summary(call: &ToolCall) -> String {
    match call.name.as_str() {
        "web_search" => call
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "fetch_url" => call
            .arguments
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "activate_skill" | "read_skill" => call
            .arguments
            .get("name")
            .or_else(|| call.arguments.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "run_terminal" => terminal::tool_summary(&call.arguments),
        "show_image" => media::tool_summary(&call.arguments),
        name if browser::is_browser_tool(name) => browser::tool_summary(name, &call.arguments),
        _ => fs::tool_summary(&call.name, &call.arguments),
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
        trim_prompt(agent::IMAGES).to_string(),
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
        let depth_note = match skills.search_scrape_plan() {
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
        let max_chars = skills.fetch_url_char_limit().to_string();
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
    if skills.filesystem_ready() {
        lines.push(fill(
            agent::FILESYSTEM,
            &[("workspace", &fs::workspace_prompt_line(&skills.workspace_root))],
        ));
    } else if skills.filesystem {
        lines.push(trim_prompt(agent::FILESYSTEM_NO_WORKSPACE).to_string());
    }
    if skills.terminal_ready() {
        let timeout = terminal::clamp_timeout_secs(skills.terminal_timeout_secs).to_string();
        lines.push(fill(agent::TERMINAL, &[("timeout", &timeout)]));
    } else if skills.terminal {
        lines.push(trim_prompt(agent::TERMINAL_NO_WORKSPACE).to_string());
    }
    if skills.browser {
        lines.push(trim_prompt(agent::BROWSER).to_string());
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
    announced: bool,
    emit_at: usize,
}

/// With `--jinja`, llama-server may lift `<tool_call>` into native `delta.tool_calls`;
/// we re-synthesize XML for `extract_tool_calls`. Once a tool starts we stop
/// forwarding further content deltas, after first forwarding any preface text
/// (including content that shares a delta with `tool_calls`).
struct StreamOnceTools<'a> {
    pending_force: &'a [String],
    allow_tools: bool,
    allow_insecure_tls: bool,
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
    let response = open_llm_sse(
        api_base,
        &url,
        style,
        token,
        &body,
        tools.allow_insecure_tls,
    )
    .await?;
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
    if turn.content.trim().is_empty() && turn.reasoning.trim().is_empty() && turn.tools.is_empty() {
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
        announce_preparing_tools(native_tools, tx).await?;
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

async fn announce_preparing_tools(
    native_tools: &mut [Option<AccumToolCall>],
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<(), StreamFail> {
    for slot in native_tools.iter_mut().flatten() {
        if slot.name.trim().is_empty() {
            continue;
        }
        if slot.id.trim().is_empty() {
            slot.id = new_tool_call_id();
        }
        let grown = slot.arguments.len().saturating_sub(slot.emit_at) >= 96;
        if slot.announced && !grown {
            continue;
        }
        slot.announced = true;
        slot.emit_at = slot.arguments.len();
        let arguments = preview_tool_arguments(&slot.arguments);
        send_sse(
            tx,
            sse_agent(json!({
                "phase": "tool_prepare",
                "id": slot.id,
                "name": slot.name,
                "arguments": arguments,
            })),
        )
        .await?;
    }
    Ok(())
}

fn preview_tool_arguments(raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return json!({});
    }
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return value;
    }
    let mut map = serde_json::Map::new();
    for key in [
        "path",
        "content",
        "old_string",
        "new_string",
        "command",
        "query",
        "url",
        "pattern",
        "cwd",
        "ref",
        "text",
        "key",
        "expression",
        "selector",
    ] {
        if let Some(value) = json_string_field(raw, key) {
            map.insert(key.to_string(), json!(value));
        }
    }
    Value::Object(map)
}

fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = raw.find(&needle)?;
    let rest = raw[idx + needle.len()..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => break,
            }
        } else if ch == '"' {
            break;
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

fn merge_tool_call_delta(slots: &mut Vec<Option<AccumToolCall>>, delta: &Value) {
    let index = delta.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    while slots.len() <= index {
        slots.push(None);
    }
    let slot = slots[index].get_or_insert_with(AccumToolCall::default);

    if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
        let id = id.trim();
        if !id.is_empty() && (!slot.announced || slot.id.trim().is_empty()) {
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
                        "recency": {
                            "type": "string",
                            "enum": ["any", "day", "week", "month", "year"],
                            "description": "Optional freshness filter"
                        }
                    },
                    "required": ["query"]
                }
            }
        }));
    }
    if skills.fetch_url {
        let max_chars = skills.fetch_url_char_limit().to_string();
        let raw_desc = if skills.web_search {
            tools::FETCH_URL_WITH_SEARCH
        } else {
            tools::FETCH_URL
        };
        let description = crate::prompts::fill(raw_desc, &[("max_chars", &max_chars)]);
        tools_out.push(json!({
            "type": "function",
            "function": {
                "name": "fetch_url",
                "description": description.trim(),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Absolute http(s) URL to fetch"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Optional character offset to start reading from (for paginating through long pages). Default is 0."
                        }
                    },
                    "required": ["url"]
                }
            }
        }));
    }
    if skills.filesystem_ready() {
        tools_out.push(function_tool(
            "read_file",
            trim_prompt(tools::READ_FILE),
            json!({
                "path": { "type": "string" },
                "offset": { "type": "integer", "description": "Optional 0-based line offset" },
                "limit": { "type": "integer", "description": "Optional max lines to return" }
            }),
            &["path"],
        ));
        tools_out.push(function_tool(
            "list_dir",
            trim_prompt(tools::LIST_DIR),
            json!({ "path": { "type": "string" } }),
            &[],
        ));
        tools_out.push(function_tool(
            "glob",
            trim_prompt(tools::GLOB),
            json!({ "pattern": { "type": "string" } }),
            &["pattern"],
        ));
        tools_out.push(function_tool(
            "grep",
            trim_prompt(tools::GREP),
            json!({
                "query": { "type": "string" },
                "glob": { "type": "string" },
                "case_insensitive": { "type": "boolean" }
            }),
            &["query"],
        ));
        tools_out.push(function_tool(
            "write_file",
            trim_prompt(tools::WRITE_FILE),
            json!({
                "path": { "type": "string" },
                "content": { "type": "string" }
            }),
            &["path", "content"],
        ));
        tools_out.push(function_tool(
            "str_replace",
            trim_prompt(tools::STR_REPLACE),
            json!({
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" },
                "replace_all": { "type": "boolean" }
            }),
            &["path", "old_string", "new_string"],
        ));
        tools_out.push(function_tool(
            "delete_file",
            trim_prompt(tools::DELETE_FILE),
            json!({ "path": { "type": "string" } }),
            &["path"],
        ));
    }
    if skills.terminal_ready() {
        tools_out.push(function_tool(
            "run_terminal",
            trim_prompt(tools::RUN_TERMINAL),
            json!({
                "command": { "type": "string" },
                "cwd": { "type": "string", "description": "Optional path relative to the workspace" }
            }),
            &["command"],
        ));
    }
    if skills.browser {
        tools_out.push(function_tool(
            "browser_navigate",
            trim_prompt(tools::BROWSER_NAVIGATE),
            json!({ "url": { "type": "string", "description": "Absolute http(s) URL" } }),
            &["url"],
        ));
        tools_out.push(function_tool(
            "browser_snapshot",
            trim_prompt(tools::BROWSER_SNAPSHOT),
            json!({}),
            &[],
        ));
        tools_out.push(function_tool(
            "browser_click",
            trim_prompt(tools::BROWSER_CLICK),
            json!({ "ref": { "type": "string", "description": "Snapshot ref such as e12" } }),
            &["ref"],
        ));
        tools_out.push(function_tool(
            "browser_type",
            trim_prompt(tools::BROWSER_TYPE),
            json!({
                "ref": { "type": "string" },
                "text": { "type": "string" },
                "submit": { "type": "boolean", "description": "If true, submit the form / press Enter after typing" }
            }),
            &["ref", "text"],
        ));
        tools_out.push(function_tool(
            "browser_press",
            trim_prompt(tools::BROWSER_PRESS),
            json!({
                "key": { "type": "string", "description": "Enter, Tab, Escape, ArrowDown, …" },
                "ref": { "type": "string" }
            }),
            &["key"],
        ));
        tools_out.push(function_tool(
            "browser_wait",
            trim_prompt(tools::BROWSER_WAIT),
            json!({
                "time_ms": { "type": "integer", "description": "Milliseconds to wait (50–30000)" },
                "selector": { "type": "string", "description": "Optional CSS selector to wait for" }
            }),
            &[],
        ));
        tools_out.push(function_tool(
            "browser_screenshot",
            trim_prompt(tools::BROWSER_SCREENSHOT),
            json!({ "path": { "type": "string", "description": "Optional workspace-relative PNG path" } }),
            &[],
        ));
        tools_out.push(function_tool(
            "browser_evaluate",
            trim_prompt(tools::BROWSER_EVALUATE),
            json!({ "expression": { "type": "string", "description": "JavaScript expression to evaluate" } }),
            &["expression"],
        ));
        tools_out.push(function_tool(
            "browser_close",
            trim_prompt(tools::BROWSER_CLOSE),
            json!({}),
            &[],
        ));
    }
    tools_out.push(function_tool(
        "show_image",
        trim_prompt(tools::SHOW_IMAGE),
        json!({
            "path": { "type": "string", "description": "Workspace-relative image file" },
            "url": { "type": "string", "description": "Absolute http(s) image URL" }
        }),
        &[],
    ));
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

fn function_tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required
            }
        }
    })
}

fn tool_calls_from_native(slots: &[Option<AccumToolCall>]) -> Vec<ToolCall> {
    slots
        .iter()
        .flatten()
        .filter(|call| !call.name.is_empty())
        .map(|call| {
            let arguments = if call.arguments.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str::<Value>(&call.arguments)
                    .unwrap_or_else(|_| json!({ "raw": call.arguments }))
            };
            ToolCall {
                id: if call.id.trim().is_empty() {
                    new_tool_call_id()
                } else {
                    call.id.clone()
                },
                name: call.name.clone(),
                arguments,
            }
        })
        .collect()
}

fn resolve_streamed_turn(
    reasoning: &str,
    content: &str,
    native_tools: &[Option<AccumToolCall>],
) -> StreamedTurn {
    let visible = strip_think_blocks(content);
    let mut tools = tool_calls_from_native(native_tools);
    if tools.is_empty() {
        tools = extract_tool_calls(&visible);
    }
    let content = if !tools.is_empty() {
        strip_tool_call_xml(&visible)
    } else {
        content.to_string()
    };
    StreamedTurn {
        content,
        reasoning: reasoning.to_string(),
        tools,
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

async fn send_content_clear(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    turn: &StreamedTurn,
) -> Result<(), StreamFail> {
    let mut clear = json!({ "phase": "content_clear" });
    if !turn.content.trim().is_empty() {
        clear["text"] = json!(turn.content);
    }
    if !turn.reasoning.trim().is_empty() {
        clear["reasoning"] = json!(turn.reasoning);
    }
    send_sse(tx, sse_agent(clear)).await
}

async fn send_tool_call(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    call: &ToolCall,
    needs_approval: bool,
) -> Result<(), StreamFail> {
    let mut payload = json!({
        "phase": "tool_call",
        "id": call.id,
        "name": call.name,
        "arguments": call.arguments,
        "needs_approval": needs_approval,
    });
    if needs_approval {
        payload["risk"] = json!(tool_risk(&call.name));
        payload["summary"] = json!(tool_call_summary(call));
    }
    send_sse(tx, sse_agent(payload)).await
}

async fn send_tool_approval(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    call: &ToolCall,
) -> Result<(), StreamFail> {
    send_sse(
        tx,
        sse_agent(json!({
            "phase": "tool_approval",
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
            "risk": tool_risk(&call.name),
            "summary": tool_call_summary(call),
        })),
    )
    .await
}

async fn send_tool_executing(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    call: &ToolCall,
) -> Result<(), StreamFail> {
    send_sse(
        tx,
        sse_agent(json!({
            "phase": "tool_executing",
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
            "summary": tool_call_summary(call),
        })),
    )
    .await
}

async fn send_tool_result(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    call: &ToolCall,
    outcome: &ToolOutcome,
) -> Result<(), StreamFail> {
    let preview = outcome.ui_text.chars().take(240).collect::<String>();
    let ui_result: String = outcome.ui_text.chars().take(32_000).collect();
    let mut payload = json!({
        "phase": "tool_result",
        "id": call.id,
        "name": call.name,
        "ok": outcome.ok,
        "preview": preview,
        "result": ui_result,
    });
    if let Some(note) = &outcome.note {
        payload["note"] = json!(note);
    }
    if let Some(image) = &outcome.image {
        payload["image"] = json!(image);
    }
    if let Some(image_id) = &outcome.image_id {
        payload["image_id"] = json!(image_id);
    }
    send_sse(tx, sse_agent(payload)).await
}

fn arm_tool_approval(approval_id: &str) -> Result<oneshot::Receiver<bool>, StreamFail> {
    let (answer_tx, answer_rx) = oneshot::channel();
    approval_waiters()
        .lock()
        .map_err(|_| StreamFail::Other("approval waiters lock poisoned".into()))?
        .insert(approval_id.to_string(), answer_tx);
    Ok(answer_rx)
}

async fn wait_for_approval(
    approval_id: &str,
    mut answer_rx: oneshot::Receiver<bool>,
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
) -> Result<bool, StreamFail> {
    let _guard = ApprovalWaitGuard {
        id: approval_id.to_string(),
    };
    let started = std::time::Instant::now();
    loop {
        if tx.is_closed() {
            return Err(StreamFail::Other("Approval cancelled.".into()));
        }
        if started.elapsed() >= CLARIFY_WAIT_TIMEOUT {
            return Err(StreamFail::Other(
                "Timed out waiting for tool approval.".into(),
            ));
        }
        match timeout(Duration::from_millis(400), &mut answer_rx).await {
            Ok(Ok(allow)) => return Ok(allow),
            Ok(Err(_)) => {
                return Err(StreamFail::Other("Approval cancelled.".into()));
            }
            Err(_) => continue,
        }
    }
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

fn extract_tool_calls(text: &str) -> Vec<ToolCall> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<tool_call>") {
        let after = start + "<tool_call>".len();
        let Some(rel) = rest[after..].find("</tool_call>") else {
            break;
        };
        let raw = rest[after..after + rel].trim();
        if let Some(call) = parse_tool_call_json(raw) {
            out.push(call);
        }
        rest = &rest[after + rel + "</tool_call>".len()..];
    }
    out
}

fn parse_tool_call_json(raw: &str) -> Option<ToolCall> {
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
    /// Optional data-URL image the UI can show and the model can embed via image_id.
    image: Option<String>,
    image_id: Option<String>,
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
            image: None,
            image_id: None,
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
            image: None,
            image_id: None,
            ok: true,
        }
    }

    fn soft_failure(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            ui_text: text.clone(),
            text,
            note: Some("retry with a different query".into()),
            image: None,
            image_id: None,
            ok: false,
        }
    }
}

fn next_reply_image_id() -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    format!("img_{n}")
}

fn with_reply_image(mut outcome: ToolOutcome, data_url: String) -> ToolOutcome {
    let id = next_reply_image_id();
    outcome.image = Some(data_url);
    outcome.image_id = Some(id.clone());
    outcome.text.push_str(&format!(
        "\n\nImage {id} is ready for the user-visible reply. Include it on its own line as markdown: ![short caption]({id})"
    ));
    outcome
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
            let search_skills = search_call_overrides(skills, &call.arguments);
            let (result, note) = run_web_search(query, &search_skills).await?;
            let ui_result = result.clone();
            let mut model_result = result;
            if skills.fetch_url {
                model_result.push_str(
                    "\n\nNote: fetch_url is available. If result URLs look useful, you may call fetch_url on several of them in the same turn (they run in parallel).",
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
            let offset = call
                .arguments
                .get("offset")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
                })
                .unwrap_or(0) as usize;
            let custom_max = call
                .arguments
                .get("max_chars")
                .or_else(|| call.arguments.get("limit"))
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
                })
                .map(|n| n as usize);
            Ok(ToolOutcome::text(
                fetch_single_url(url, skills, offset, custom_max).await?,
            ))
        }
        "read_file" | "list_dir" | "glob" | "grep" | "write_file" | "str_replace" | "delete_file" => {
            let name = call.name.clone();
            let args = call.arguments.clone();
            let root = skills.workspace_root.clone();
            tokio::task::spawn_blocking(move || execute_fs_tool(&name, &root, &args))
                .await
                .map_err(|err| err.to_string())?
                .map(ToolOutcome::text)
        }
        "run_terminal" => Ok(ToolOutcome::text(
            terminal::run_agent_command(
                &skills.workspace_root,
                &call.arguments,
                skills.terminal_timeout_secs,
            )
            .await?,
        )),
        name if browser::is_browser_tool(name) => {
            let out = browser::execute(
                name,
                &call.arguments,
                &skills.session_id,
                &skills.workspace_root,
            )
            .await?;
            let mut outcome = ToolOutcome::text(out.text);
            if let Some(data_url) = out.image_data_url {
                outcome = with_reply_image(outcome, data_url);
            }
            Ok(outcome)
        }
        "show_image" => {
            let (bytes, mime) = media::load_image(&call.arguments, &skills.workspace_root).await?;
            let data_url = media::to_data_url(&bytes, mime);
            let summary = format!(
                "Loaded {} ({} bytes). Include it in your user-visible reply as markdown.",
                mime,
                bytes.len()
            );
            Ok(with_reply_image(ToolOutcome::text(summary), data_url))
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

fn execute_fs_tool(name: &str, root: &str, args: &Value) -> Result<String, String> {
    let ws = fs::Workspace::open(root)?;
    match name {
        "read_file" => fs::read_file(&ws, args),
        "list_dir" => fs::list_dir(&ws, args),
        "glob" => fs::glob_files(&ws, args),
        "grep" => fs::grep_files(&ws, args),
        "write_file" => fs::write_file(&ws, args),
        "str_replace" => fs::str_replace(&ws, args),
        "delete_file" => fs::delete_file(&ws, args),
        other => Err(format!("Unknown file tool '{other}'.")),
    }
}

async fn approve_and_execute_tools(
    calls: Vec<ToolCall>,
    skills: &AgentSkills,
    user_skills: &[UserSkill],
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    mut approval_rx: HashMap<String, oneshot::Receiver<bool>>,
) -> Result<Vec<(ToolCall, ToolOutcome)>, StreamFail> {
    let mut auto = Vec::new();
    let mut pending = Vec::new();
    for call in calls {
        if approval_rx.contains_key(&call.id) {
            send_tool_approval(tx, &call).await?;
            pending.push(call);
        } else {
            auto.push(call);
        }
    }
    let mut denied = Vec::new();
    let mut executed = Vec::new();
    if !auto.is_empty() {
        let batch = execute_tool_batch(auto, skills, user_skills).await?;
        emit_executed_tools(tx, &batch).await?;
        executed.extend(batch);
    }
    for call in pending {
        let rx = approval_rx
            .remove(&call.id)
            .ok_or_else(|| StreamFail::Other("Missing tool approval waiter.".into()))?;
        let allow = wait_for_approval(&call.id, rx, tx).await?;
        if allow {
            send_tool_executing(tx, &call).await?;
            let batch = execute_tool_batch(vec![call], skills, user_skills).await?;
            emit_executed_tools(tx, &batch).await?;
            executed.extend(batch);
        } else {
            let outcome = ToolOutcome::soft_failure(
                "The user denied this tool call. Continue without it, or ask them to approve a smaller request.",
            );
            send_tool_result(tx, &call, &outcome).await?;
            denied.push((call, outcome));
        }
    }
    let mut combined = denied;
    combined.extend(executed);
    Ok(combined)
}

async fn emit_executed_tools(
    tx: &mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    batch: &[(ToolCall, ToolOutcome)],
) -> Result<(), StreamFail> {
    for (call, outcome) in batch {
        if call.name == "run_terminal" {
            send_sse(
                tx,
                sse_agent(json!({
                    "phase": "terminal",
                    "command": terminal::tool_summary(&call.arguments),
                    "output": outcome.ui_text,
                    "ok": outcome.ok,
                    "source": "agent",
                })),
            )
            .await?;
        }
        send_tool_result(tx, call, outcome).await?;
    }
    Ok(())
}

async fn execute_tool_batch(
    calls: Vec<ToolCall>,
    skills: &AgentSkills,
    user_skills: &[UserSkill],
) -> Result<Vec<(ToolCall, ToolOutcome)>, StreamFail> {
    let skills = skills.clone();
    let user_skills = user_skills.to_vec();
    let mut finished: Vec<(usize, ToolCall, Result<ToolOutcome, String>)> =
        stream::iter(calls.into_iter().enumerate())
            .map(|(index, call)| {
                let skills = skills.clone();
                let user_skills = user_skills.clone();
                async move {
                    let outcome = match execute_tool(&call, &skills, &user_skills).await {
                        Ok(outcome) => Ok(outcome),
                        Err(err) if is_recoverable_tool(&call.name) => Ok(ToolOutcome::soft_failure(
                            lookup_retry_guidance(&call.name, &err),
                        )),
                        Err(err) => Err(err),
                    };
                    (index, call, outcome)
                }
            })
            .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS)
            .collect()
            .await;
    if let Some((_, _, Err(err))) = finished.iter().find(|(_, _, result)| result.is_err()) {
        return Err(StreamFail::Other(err.clone()));
    }
    finished.sort_by_key(|(index, _, _)| *index);
    Ok(finished
        .into_iter()
        .map(|(_, call, outcome)| (call, outcome.expect("lookup errors were converted")))
        .collect())
}

fn format_page_window(url: &str, full_text: &str, offset: usize, max_chars: usize) -> String {
    let total_chars = full_text.chars().count();
    if total_chars == 0 {
        return format!("Fetched {url} but extracted no readable text.");
    }
    if offset >= total_chars {
        return format!(
            "Fetched {url} at offset {offset}, but the page ends at {total_chars} total characters (no further content)."
        );
    }
    let slice: String = full_text.chars().skip(offset).take(max_chars).collect();
    let slice_len = slice.chars().count();
    let end_offset = offset + slice_len;

    let prefix = if offset > 0 {
        format!("[Continued from offset {offset} of {total_chars} total]\n\n")
    } else {
        String::new()
    };

    let suffix = if end_offset < total_chars {
        format!(
            "\n\n[Showing characters {offset}..{end_offset} of {total_chars} total. Call fetch_url with url=\"{url}\" and offset={end_offset} to read the next chunk]"
        )
    } else if offset > 0 {
        format!("\n\n[End of page reached: {total_chars} total characters]")
    } else {
        String::new()
    };

    let header = if total_chars <= max_chars && offset == 0 {
        format!("Fetched page text from {url} ({total_chars} characters):\n")
    } else {
        format!(
            "Fetched page text from {url} (characters {offset}..{end_offset} of {total_chars} total):\n"
        )
    };

    format!("{header}{prefix}{slice}{suffix}")
}

async fn fetch_single_url(
    url: &str,
    skills: &AgentSkills,
    offset: usize,
    custom_max: Option<usize>,
) -> Result<String, String> {
    if !scrapeable_url(url) {
        return Err(
            "fetch_url only supports http(s) pages (not files like PDF, images, or archives)."
                .into(),
        );
    }
    let configured_limit = skills.fetch_url_char_limit();
    let max_chars = custom_max
        .map(|n| clamp_page_fetch_chars(n).min(configured_limit))
        .unwrap_or(configured_limit);

    let full_text = fetch_raw_page_text(url).await?;
    if full_text.trim().is_empty() {
        return Err(format!("Fetched {url} but extracted no readable text."));
    }
    Ok(format_page_window(url, &full_text, offset, max_chars))
}

#[derive(Debug, Clone, Default)]
struct SearchHit {
    title: String,
    url: String,
    snippet: String,
    featured: bool,
}

fn parse_enum_arg<T: for<'de> Deserialize<'de>>(args: &Value, key: &str) -> Option<T> {
    let raw = args.get(key)?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    serde_json::from_value(json!(raw.to_ascii_lowercase())).ok()
}

fn search_call_overrides(skills: &AgentSkills, args: &Value) -> AgentSkills {
    let mut next = skills.clone();
    if let Some(recency) = parse_enum_arg::<WebSearchRecency>(args, "recency") {
        next.web_search_recency = recency;
    }
    next
}

async fn run_web_search(query: &str, skills: &AgentSkills) -> Result<(String, String), String> {
    let result_count = skills.search_result_count();
    let (hits, engine, note) = search::search_web(query, skills).await?;
    if hits.is_empty() {
        return Ok((
            format!("No results found for {query:?}."),
            format!("via web · {note}"),
        ));
    }
    let mut out = format!(
        "Web search results for {query:?} ({engine}, {}, {}):\n",
        skills.search_region(),
        skills.web_search_safesearch.as_str(),
    );
    let featured: Vec<&SearchHit> = hits.iter().filter(|hit| hit.featured).collect();
    let rest: Vec<&SearchHit> = hits.iter().filter(|hit| !hit.featured).collect();
    if !featured.is_empty() {
        out.push_str("\nDirect answer:\n");
        for hit in &featured {
            out.push_str(&format!("\n{}\n", hit.title));
            if !hit.url.is_empty() {
                out.push_str(&format!("   URL: {}\n", hit.url));
            }
            if !hit.snippet.is_empty() {
                out.push_str(&format!("   {}\n", hit.snippet));
            }
        }
    }
    for (index, hit) in rest.iter().take(result_count).enumerate() {
        out.push_str(&format!(
            "\n{}. {}\n   URL: {}\n   {}\n",
            index + 1,
            hit.title,
            hit.url,
            hit.snippet
        ));
    }
    let scrape_hits: Vec<SearchHit> = rest.into_iter().cloned().collect();
    append_scraped_pages(&mut out, &scrape_hits, skills).await;
    Ok((out, format!("via web · {note}")))
}

async fn append_scraped_pages(out: &mut String, hits: &[SearchHit], skills: &AgentSkills) {
    let Some((page_count, max_chars)) = skills.search_scrape_plan() else {
        return;
    };
    let depth = skills.web_search_depth;
    let targets: Vec<SearchHit> = hits
        .iter()
        .filter(|hit| scrapeable_url(&hit.url))
        .take(page_count)
        .cloned()
        .collect();
    if targets.is_empty() {
        return;
    }

    out.push_str(&format!(
        "\n--- Fetched page text (depth: {}, up to {} pages) ---\n",
        depth.label(),
        page_count
    ));

    let fetches = targets.iter().enumerate().map(|(index, hit)| {
        let url = hit.url.clone();
        let title = hit.title.clone();
        async move {
            let outcome = timeout(PAGE_FETCH_TIMEOUT, fetch_page_text(&url, max_chars)).await;
            (index, title, url, outcome)
        }
    });
    for (index, title, url, outcome) in join_all(fetches).await {
        match outcome {
            Ok(Ok(text)) if !text.trim().is_empty() => {
                out.push_str(&format!("\n[{}] {title} ({url})\n{text}\n", index + 1,));
            }
            Ok(Ok(_)) => {
                out.push_str(&format!(
                    "\n[{}] {title} ({url})\n(no extractable text)\n",
                    index + 1,
                ));
            }
            Ok(Err(error)) => {
                out.push_str(&format!(
                    "\n[{}] {title} ({url})\n(fetch failed: {error})\n",
                    index + 1,
                ));
            }
            Err(_) => {
                out.push_str(&format!(
                    "\n[{}] {title} ({url})\n(fetch timed out)\n",
                    index + 1,
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
    if search::reject_result_url(url) {
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

async fn fetch_raw_page_text(url: &str) -> Result<String, String> {
    let mut response = http::safe_public_get(url, false).await?;

    let mut status = response.status().as_u16();
    // Negotiate again with a looser Accept — Akamai/news stacks sometimes 406 the first try.
    if matches!(status, 406 | 403) {
        response = http::safe_public_get(url, true).await?;
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
    Ok(html_to_full_text(&html, Some(url)))
}

async fn fetch_page_text(url: &str, max_chars: usize) -> Result<String, String> {
    let full = fetch_raw_page_text(url).await?;
    Ok(truncate_chars(&full, max_chars))
}

fn html_to_full_text(html: &str, page_url: Option<&str>) -> String {
    if let Some(text) = readability_main_text(html, page_url)
        && text.chars().count() >= 80
    {
        return text;
    }
    scraper_main_text(html)
}

fn readability_main_text(html: &str, page_url: Option<&str>) -> Option<String> {
    let cfg = Config {
        text_mode: TextMode::Formatted,
        char_threshold: 80,
        ..Config::default()
    };
    let mut reader = Readability::new(html, page_url, Some(cfg)).ok()?;
    let article = reader.parse().ok()?;
    let mut parts = Vec::new();
    let title = collapse_ws(&article.title);
    if !title.is_empty() {
        parts.push(title);
    }
    if let Some(byline) = article.byline {
        let byline = collapse_ws(&byline);
        if !byline.is_empty() {
            parts.push(byline);
        }
    }
    let body = normalize_extracted_text(&article.text_content);
    if !body.is_empty() {
        parts.push(body);
    }
    let joined = parts.join("\n\n");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn scraper_main_text(html: &str) -> String {
    let document = Html::parse_document(html);
    let mut best = String::new();
    for selector in ["article", "main", "[role=main]", "body"] {
        let Ok(sel) = Selector::parse(selector) else {
            continue;
        };
        for el in document.select(&sel) {
            let text = element_block_text(el);
            if text.chars().count() > best.chars().count() {
                best = text;
            }
        }
        if best.chars().count() >= 80 {
            break;
        }
    }
    best
}

fn element_block_text(el: scraper::ElementRef<'_>) -> String {
    let Ok(block_sel) = Selector::parse("p, h1, h2, h3, h4, h5, h6, li, blockquote, td, th") else {
        return normalize_extracted_text(&el.text().collect::<String>());
    };
    let mut lines = Vec::new();
    for node in el.select(&block_sel) {
        if node
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|ancestor| {
                matches!(
                    ancestor.value().name(),
                    "nav"
                        | "footer"
                        | "header"
                        | "aside"
                        | "script"
                        | "style"
                        | "form"
                        | "noscript"
                )
            })
        {
            continue;
        }
        let text = collapse_ws(&node.text().collect::<String>());
        if !text.is_empty() {
            lines.push(text);
        }
    }
    if lines.is_empty() {
        normalize_extracted_text(&el.text().collect::<String>())
    } else {
        lines.join("\n")
    }
}

fn normalize_extracted_text(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let collapsed = collapse_ws(line);
        if collapsed.is_empty() {
            if lines.last().is_some_and(|prev: &String| !prev.is_empty()) {
                lines.push(String::new());
            }
            continue;
        }
        lines.push(collapsed);
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
fn html_to_readable_text(html: &str, max_chars: usize) -> String {
    let joined = html_to_full_text(html, None);
    truncate_chars(&joined, max_chars)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
        let call = &extract_tool_calls(text)[0];
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
        assert_eq!(turn.tools.len(), 1);
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
        let call = &turn.tools[0];
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments["query"], "elon musk");
    }

    #[test]
    fn unclosed_think_does_not_swallow_tool_call() {
        let text = "<think>planning\n<tool_call>\n{\"name\":\"web_search\",\"arguments\":{\"query\":\"x\"}}\n</tool_call>";
        let visible = strip_think_blocks(text);
        let call = &extract_tool_calls(&visible)[0];
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
        assert_eq!(names, ["web_search", "fetch_url", "show_image"]);
        assert!(capability_allowed("show_image", &skills, &[]));
    }

    #[test]
    fn finalize_keeps_reasoning_when_content_empty() {
        let turn = resolve_streamed_turn("planning a search", "", &[]);
        assert!(
            turn.assistant_text()
                .contains("<think>planning a search</think>")
        );
        assert!(turn.tools.is_empty());
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
        assert_eq!(turn.tools.len(), 1);
        assert_eq!(turn.tools[0].arguments["query"], "from-native");
    }

    #[test]
    fn strips_think_before_tool_parse() {
        let text = r#"<think>plan</think>
<tool_call>
{"name":"web_search","arguments":{"query":"hi"}}
</tool_call>"#;
        let visible = strip_think_blocks(text);
        let call = &extract_tool_calls(&visible)[0];
        assert_eq!(call.arguments["query"], "hi");
    }

    #[test]
    fn extracts_parallel_xml_tool_calls() {
        let text = r#"
<tool_call>
{"name":"web_search","arguments":{"query":"alpha"}}
</tool_call>
<tool_call>
{"name":"fetch_url","arguments":{"url":"https://example.com/a"}}
</tool_call>
<tool_call>
{"name":"web_search","arguments":{"query":"beta"}}
</tool_call>"#;
        let calls = extract_tool_calls(text);
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments["query"], "alpha");
        assert_eq!(calls[1].name, "fetch_url");
        assert_eq!(calls[2].arguments["query"], "beta");
    }

    #[test]
    fn resolves_parallel_native_tool_calls() {
        let mut slots = Vec::new();
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 0,
                "id": "call_a",
                "function": { "name": "web_search", "arguments": "{\"query\":\"one\"}" }
            }),
        );
        merge_tool_call_delta(
            &mut slots,
            &json!({
                "index": 1,
                "id": "call_b",
                "function": { "name": "fetch_url", "arguments": "{\"url\":\"https://example.com\"}" }
            }),
        );
        let turn = resolve_streamed_turn("", "", &slots);
        assert_eq!(turn.tools.len(), 2);
        assert_eq!(turn.tools[0].id, "call_a");
        assert_eq!(turn.tools[0].arguments["query"], "one");
        assert_eq!(turn.tools[1].id, "call_b");
        assert_eq!(turn.tools[1].name, "fetch_url");
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
    fn extracts_article_and_drops_nav_chrome() {
        let html = r#"<html><body>
        <nav>RoyaltyKing Charles III Queen Camilla Prince William Princess Catherine Celebrity Style Royal Style</nav>
        <article><p>Elon Musk spoke at a Tesla event about vehicle production. The company posted delivery numbers for the quarter.</p><p>More reporting on the factory expansion followed later in the day.</p><p>Analysts said the update was the first detailed look at the plan this month.</p></article>
        <footer>All rights reserved Hello Magazine</footer>
        </body></html>"#;
        let text = html_to_readable_text(html, 2000);
        assert!(text.contains("Tesla event"));
        assert!(!text.contains("King Charles"));
        assert!(!text.contains("Celebrity Style"));
    }

    #[test]
    fn scrape_plan_defaults_to_snippets_only() {
        assert_eq!(WebSearchDepth::default(), WebSearchDepth::Off);
        assert!(WebSearchDepth::Off.scrape_plan().is_none());
        assert_eq!(WebSearchDepth::Auto.scrape_plan(), Some((3, 2800)));
        assert_eq!(WebSearchDepth::Deep.scrape_plan(), Some((6, 4800)));
    }

    #[test]
    fn skips_binary_result_urls() {
        assert!(scrapeable_url("https://example.com/story"));
        assert!(!scrapeable_url("https://example.com/file.pdf"));
        assert!(!scrapeable_url("ftp://example.com/a"));
        assert!(!scrapeable_url(
            "https://news.google.com/topics/CAAqJggKIiBDQkFTRWdvSUwyMHZNRGx1YlY4U0FtVnVHZ0pWVXlnQVAB"
        ));
        assert!(!scrapeable_url(
            "https://www.hellomagazine.com/tags/elon-musk/"
        ));
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
    fn filesystem_and_terminal_are_gated() {
        let off = AgentSkills::default();
        let on = AgentSkills {
            filesystem: true,
            workspace_root: "/tmp/ws".into(),
            terminal: true,
            ..AgentSkills::default()
        };
        let filesystem_no_folder = AgentSkills {
            filesystem: true,
            ..AgentSkills::default()
        };
        let terminal_no_folder = AgentSkills {
            terminal: true,
            ..AgentSkills::default()
        };
        assert!(!capability_allowed("read_file", &off, &[]));
        assert!(!capability_allowed("write_file", &filesystem_no_folder, &[]));
        assert!(!capability_allowed("read_file", &filesystem_no_folder, &[]));
        assert!(capability_allowed("read_file", &on, &[]));
        assert!(capability_allowed("str_replace", &on, &[]));
        assert!(!capability_allowed("run_terminal", &terminal_no_folder, &[]));
        assert!(capability_allowed("run_terminal", &on, &[]));
        assert!(!capability_allowed("run_terminal", &off, &[]));
        assert!(on.any_enabled());
        assert!(needs_approval("web_search", ApprovalMode::Manual));
        assert!(!needs_approval("web_search", ApprovalMode::AutoSafe));
        assert!(needs_approval("write_file", ApprovalMode::AutoSafe));
        assert!(needs_approval("run_terminal", ApprovalMode::AutoSafe));
        assert!(!needs_approval("ask_user", ApprovalMode::Manual));
        assert_eq!(tool_risk("run_terminal"), "terminal");
        assert_eq!(tool_risk("read_file"), "safe");
    }

    #[test]
    fn browser_capability_is_gated() {
        let off = AgentSkills::default();
        let on = AgentSkills {
            browser: true,
            ..AgentSkills::default()
        };
        assert!(!capability_allowed("browser_navigate", &off, &[]));
        assert!(capability_allowed("browser_navigate", &on, &[]));
        assert!(capability_allowed("browser_snapshot", &on, &[]));
        assert!(on.any_enabled());
        assert!(needs_approval("browser_navigate", ApprovalMode::AutoSafe));
        assert_eq!(tool_risk("browser_click"), "browser");
        let tools = openai_tools_payload(&on, &[], false);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"browser_navigate"));
        assert!(names.contains(&"browser_snapshot"));
        assert!(names.contains(&"browser_close"));
        assert!(names.contains(&"show_image"));
    }

    #[test]
    fn preview_arguments_reads_partial_json() {
        let raw = r#"{"path":"script.js","content":"console.log(1);"#;
        let value = preview_tool_arguments(raw);
        assert_eq!(value["path"], "script.js");
        assert_eq!(value["content"], "console.log(1);");
        let complete = preview_tool_arguments(r#"{"path":"a.html","content":"<p>hi</p>"}"#);
        assert_eq!(complete["path"], "a.html");
        assert_eq!(complete["content"], "<p>hi</p>");
    }

    #[tokio::test]
    async fn approval_can_be_clicked_before_the_wait_loop() {
        let id = "call_arm_early";
        let rx = arm_tool_approval(id).expect("arm");
        submit_tool_approval(id, true).expect("submit");
        let (tx, _keep) = tokio::sync::mpsc::channel(1);
        let allow = wait_for_approval(id, rx, &tx).await.expect("wait");
        assert!(allow);
        assert!(
            submit_tool_approval(id, true).is_err(),
            "duplicate allow should not find a waiter"
        );
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
        let next = search_call_overrides(&skills, &args);
        assert_eq!(next.web_search_backend, WebSearchBackend::Auto);
        assert_eq!(next.web_search_provider, WebSearchProvider::Auto);
        assert_eq!(next.web_search_recency, WebSearchRecency::Week);
    }

    #[test]
    fn web_search_deserializes_parallel_provider() {
        let skills: AgentSkills = serde_json::from_value(json!({
            "web_search": true,
            "web_search_provider": "parallel",
            "web_search_parallel_api_key": "pk_test",
            "web_search_parallel_mode": "advanced"
        }))
        .unwrap();
        assert_eq!(skills.web_search_provider, WebSearchProvider::Parallel);
        assert_eq!(skills.web_search_parallel_api_key, "pk_test");
        assert_eq!(skills.web_search_parallel_mode, WebSearchParallelMode::Advanced);
    }

    #[test]
    fn web_search_deserializes_tinyfish_provider() {
        let skills: AgentSkills = serde_json::from_value(json!({
            "web_search": true,
            "web_search_provider": "tinyfish",
            "web_search_tinyfish_api_key": "tf_test"
        }))
        .unwrap();
        assert_eq!(skills.web_search_provider, WebSearchProvider::Tinyfish);
        assert_eq!(skills.web_search_tinyfish_api_key, "tf_test");
    }

    #[test]
    fn web_search_does_not_infer_news_from_query() {
        let skills = AgentSkills::default();
        let args = json!({ "query": "latest elon musk news" });
        let next = search_call_overrides(&skills, &args);
        assert_eq!(next.web_search_recency, WebSearchRecency::Any);
    }

    #[test]
    fn web_search_never_queries_yandex() {
        // Saved "yandex" still deserializes; search.rs has no Yandex path.
        let skills: AgentSkills =
            serde_json::from_value(json!({ "web_search_backend": "yandex" })).unwrap();
        assert_eq!(skills.web_search_backend, WebSearchBackend::Yandex);
    }

    #[test]
    fn web_search_tool_exposes_recency_not_kind() {
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
        assert!(props.get("query").is_some());
        assert!(props.get("backend").is_none());
        assert!(props.get("recency").is_some());
        assert!(props.get("kind").is_none());
        let description = search["function"]["description"].as_str().unwrap();
        assert!(description.contains("snippets"));
        assert!(!description.contains("kind"));
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
            tools: Vec::new(),
        };
        assert!(turn.visible_text().is_empty());
        let turn = StreamedTurn {
            content: "<think>plan</think>\nAndrew Tate is…".into(),
            reasoning: String::new(),
            tools: Vec::new(),
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
    fn fetch_url_tool_schema_includes_offset_parameter() {
        let skills = AgentSkills {
            fetch_url: true,
            ..AgentSkills::default()
        };
        let tools = openai_tools_payload(&skills, &[], true);
        let fetch = tools
            .iter()
            .find(|tool| tool["function"]["name"] == "fetch_url")
            .expect("fetch_url tool");
        let props = &fetch["function"]["parameters"]["properties"];
        assert!(props.get("url").is_some());
        assert!(props.get("offset").is_some());
        let desc = fetch["function"]["description"].as_str().unwrap();
        assert!(desc.contains("~8000 characters"));
    }

    #[test]
    fn format_page_window_paginates_and_formats_correctly() {
        let sample = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

        // 1. Short page fits in one chunk
        let full = format_page_window("https://example.com/test", sample, 0, 50);
        assert!(full.contains("Fetched page text from https://example.com/test (36 characters):"));
        assert!(full.contains(sample));
        assert!(!full.contains("[Showing characters"));

        // 2. First chunk of a longer page
        let chunk1 = format_page_window("https://example.com/test", sample, 0, 10);
        assert!(chunk1.contains("characters 0..10 of 36 total"));
        assert!(chunk1.contains("ABCDEFGHIJ"));
        assert!(
            chunk1.contains("Call fetch_url with url=\"https://example.com/test\" and offset=10")
        );

        // 3. Middle chunk
        let chunk2 = format_page_window("https://example.com/test", sample, 10, 10);
        assert!(chunk2.contains("characters 10..20 of 36 total"));
        assert!(chunk2.contains("[Continued from offset 10 of 36 total]"));
        assert!(chunk2.contains("KLMNOPQRST"));
        assert!(chunk2.contains("offset=20"));

        // 4. Final chunk reaching end
        let chunk_end = format_page_window("https://example.com/test", sample, 30, 10);
        assert!(chunk_end.contains("characters 30..36 of 36 total"));
        assert!(chunk_end.contains("456789"));
        assert!(chunk_end.contains("[End of page reached: 36 total characters]"));

        // 5. Offset beyond end of page
        let past_end = format_page_window("https://example.com/test", sample, 40, 10);
        assert!(past_end.contains("offset 40, but the page ends at 36 total characters"));
    }
}
