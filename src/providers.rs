use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{anthropic, http};

const SCAN_PORTS: &[u16] = &[8080, 8081, 8090, 3000, 11434];
/// How long a probe result is considered fresh. Expired entries are still served
/// (stale-while-revalidate) so Chat polling never briefly sees an empty catalog.
const HEALTH_CACHE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApiStyle {
    #[default]
    Openai,
    Anthropic,
}

impl ApiStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" | "oai" | "compatible" => Some(Self::Openai),
            "anthropic" | "claude" | "messages" => Some(Self::Anthropic),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI-compatible",
            Self::Anthropic => "Anthropic Messages",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub api_style: ApiStyle,
}

impl Provider {
    pub fn new(
        name: impl Into<String>,
        base: impl Into<String>,
        token: impl Into<String>,
        api_style: ApiStyle,
    ) -> Self {
        Self {
            id: generate_provider_id(),
            name: sanitize_name(name),
            base: base.into(),
            token: token.into(),
            api_style,
        }
    }

    pub fn chat_url(&self) -> String {
        let base = self.base.trim_end_matches('/');
        match self.api_style {
            ApiStyle::Openai => format!("{base}/chat/completions"),
            ApiStyle::Anthropic => format!("{base}/messages"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPublic {
    pub id: String,
    pub name: String,
    pub base: String,
    pub api_style: &'static str,
    pub token_set: bool,
    pub token_masked: String,
    pub active: bool,
    pub health: Option<ProviderHealth>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ProvidersConfig {
    #[serde(default, rename = "providers", alias = "remotes")]
    pub items: Vec<Provider>,
    #[serde(default, alias = "active_remote_id")]
    pub active_provider_id: String,
}

impl ProvidersConfig {
    pub fn migrate(&mut self) {
        self.items.retain(|p| !p.base.trim().is_empty());
        for provider in &mut self.items {
            if provider.id.trim().is_empty() {
                provider.id = generate_provider_id();
            }
            if provider.name.trim().is_empty() {
                provider.name = label_from_base(&provider.base);
            } else {
                provider.name = sanitize_name(&provider.name);
            }
            if let Some(normalized) = normalize_provider_base(&provider.base, provider.api_style) {
                provider.base = normalized;
            }
        }
        if self.active_provider_id.trim().is_empty()
            || !self.items.iter().any(|p| p.id == self.active_provider_id)
        {
            self.active_provider_id = self.items.first().map(|p| p.id.clone()).unwrap_or_default();
        }
    }

    pub fn active(&self) -> Option<&Provider> {
        if self.items.is_empty() {
            return None;
        }
        self.items
            .iter()
            .find(|p| p.id == self.active_provider_id)
            .or_else(|| self.items.first())
    }

    pub fn set_active(&mut self, id: &str) -> Result<(), String> {
        self.migrate();
        if !self.items.iter().any(|p| p.id == id) {
            return Err("Provider not found.".into());
        }
        self.active_provider_id = id.to_string();
        Ok(())
    }

    pub fn upsert(
        &mut self,
        id: Option<&str>,
        name: &str,
        base: &str,
        token: Option<&str>,
        api_style: ApiStyle,
        activate: bool,
    ) -> Result<Provider, String> {
        self.migrate();
        let Some(normalized) = normalize_provider_base(base, api_style) else {
            return Err("Enter an API base URL (usually ending in /v1).".into());
        };
        let cleaned_name = {
            let n = sanitize_name(name);
            if n.is_empty() {
                label_from_base(&normalized)
            } else {
                n
            }
        };

        let target_id = id.map(str::to_string).or_else(|| {
            self.items
                .iter()
                .find(|p| {
                    p.api_style == api_style
                        && normalize_provider_base(&p.base, p.api_style).as_deref()
                            == Some(normalized.as_str())
                })
                .map(|p| p.id.clone())
        });

        if let Some(id) = target_id {
            let provider = self
                .items
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| "Provider not found.".to_string())?;
            provider.name = cleaned_name;
            provider.base = normalized;
            provider.api_style = api_style;
            if let Some(token) = token {
                provider.token = token.to_string();
            }
            let updated = provider.clone();
            if activate {
                self.active_provider_id = updated.id.clone();
            }
            return Ok(updated);
        }

        if self.items.len() >= 32 {
            return Err("Maximum of 32 providers reached.".into());
        }
        let provider = Provider::new(cleaned_name, normalized, token.unwrap_or(""), api_style);
        let created = provider.clone();
        self.items.push(provider);
        if activate || self.items.len() == 1 {
            self.active_provider_id = created.id.clone();
        }
        Ok(created)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.migrate();
        if !self.items.iter().any(|p| p.id == id) {
            return Err("Provider not found.".into());
        }
        self.items.retain(|p| p.id != id);
        if self.active_provider_id == id {
            self.active_provider_id = self.items.first().map(|p| p.id.clone()).unwrap_or_default();
        }
        Ok(())
    }
}

pub fn normalize_provider_base(raw: &str, _style: ApiStyle) -> Option<String> {
    let mut base = raw.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return None;
    }
    if !base.contains("://") {
        base = format!("http://{base}");
    }
    if !base.ends_with("/v1") {
        base = format!("{base}/v1");
    }
    Some(base)
}

pub fn normalize_openai_base(raw: &str) -> Option<String> {
    normalize_provider_base(raw, ApiStyle::Openai)
}

pub fn provider_auth_headers(style: ApiStyle, token: &str) -> Vec<(String, String)> {
    let token = token.trim();
    match style {
        ApiStyle::Openai => {
            if token.is_empty() {
                Vec::new()
            } else {
                vec![("Authorization".into(), format!("Bearer {token}"))]
            }
        }
        ApiStyle::Anthropic => {
            let mut headers = vec![(
                "anthropic-version".into(),
                anthropic::ANTHROPIC_VERSION.into(),
            )];
            if !token.is_empty() {
                headers.push(("x-api-key".into(), token.into()));
            }
            headers
        }
    }
}

fn label_from_base(base: &str) -> String {
    let trimmed = base
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .trim_end_matches("/v1");
    let host = trimmed.split('/').next().unwrap_or(trimmed);
    let short = host.split(':').next().unwrap_or(host);
    let label = sanitize_name(short);
    if label.is_empty() {
        "Provider".into()
    } else {
        label
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn generate_provider_id() -> String {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return format!("provider-{}", unix_now());
    }
    format!(
        "provider-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn sanitize_name(raw: impl Into<String>) -> String {
    let name = raw.into();
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(64).collect()
}

pub fn mask_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.len() <= 8 {
        return "••••••••".into();
    }
    format!("{}…{}", &trimmed[..4], &trimmed[trimmed.len() - 4..])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthKind {
    Ready,
    Waiting,
    Auth,
    Empty,
    Error,
}

impl ProviderHealthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Waiting => "waiting",
            Self::Auth => "auth",
            Self::Empty => "empty",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderHealth {
    pub ok: bool,
    pub kind: ProviderHealthKind,
    pub model: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteModelOption {
    pub id: String,
    pub model: String,
    pub base: String,
    pub port: u16,
    pub ready: bool,
    pub label: String,
    pub thinking_supported: bool,
    /// Native multimodal / vision attachments (images) when known from host metadata.
    #[serde(default)]
    pub attachments_supported: bool,
    /// Reported context window in tokens, when the host exposes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub provider_name: String,
}

#[derive(Debug, Default)]
pub struct HealthCache {
    last: Mutex<HashMap<String, (Instant, ProviderHealth)>>,
}

#[derive(Debug, Default)]
pub struct CatalogCache {
    last: Mutex<HashMap<String, (Instant, Vec<RemoteModelOption>)>>,
}

fn cache_key(style: ApiStyle, base: &str, token: &str) -> String {
    format!("{}|{base}|{token}", style.as_str())
}

impl HealthCache {
    pub fn peek(&self, style: ApiStyle, base: &str, token: &str) -> Option<ProviderHealth> {
        let key = cache_key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return None;
        };
        guard.get(&key).map(|(_, health)| health.clone())
    }

    pub fn is_fresh(&self, style: ApiStyle, base: &str, token: &str) -> bool {
        let key = cache_key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return false;
        };
        matches!(guard.get(&key), Some((at, _)) if at.elapsed() < HEALTH_CACHE)
    }

    pub fn put(&self, style: ApiStyle, base: &str, token: &str, health: ProviderHealth) {
        let key = cache_key(style, base, token);
        if let Ok(mut guard) = self.last.lock() {
            guard.insert(key, (Instant::now(), health));
        }
    }

    pub fn probe(&self, style: ApiStyle, base: &str, token: &str) -> ProviderHealth {
        if self.is_fresh(style, base, token)
            && let Some(health) = self.peek(style, base, token)
        {
            return health;
        }
        let health = probe_provider_health(base, token, style);
        self.put(style, base, token, health.clone());
        health
    }
}

impl CatalogCache {
    pub fn peek(&self, style: ApiStyle, base: &str, token: &str) -> Option<Vec<RemoteModelOption>> {
        let key = cache_key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return None;
        };
        guard.get(&key).map(|(_, catalog)| catalog.clone())
    }

    pub fn is_fresh(&self, style: ApiStyle, base: &str, token: &str) -> bool {
        let key = cache_key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return false;
        };
        matches!(guard.get(&key), Some((at, _)) if at.elapsed() < HEALTH_CACHE)
    }

    pub fn put(&self, style: ApiStyle, base: &str, token: &str, catalog: Vec<RemoteModelOption>) {
        let key = cache_key(style, base, token);
        if let Ok(mut guard) = self.last.lock() {
            // A transient probe failure often yields []. Don't wipe a good catalog —
            // just refresh the TTL so we retry later without flickering the selector.
            if catalog.is_empty()
                && let Some((at, existing)) = guard.get_mut(&key)
                && !existing.is_empty()
            {
                *at = Instant::now();
                return;
            }
            guard.insert(key, (Instant::now(), catalog));
        }
    }

    pub fn probe(
        &self,
        style: ApiStyle,
        base: &str,
        token: &str,
        extra_ports: &[u16],
    ) -> Vec<RemoteModelOption> {
        if self.is_fresh(style, base, token)
            && let Some(catalog) = self.peek(style, base, token)
        {
            return catalog;
        }
        let catalog = probe_provider_catalog(base, token, style, extra_ports);
        self.put(style, base, token, catalog.clone());
        self.peek(style, base, token).unwrap_or(catalog)
    }
}

pub fn probe_provider_health(base: &str, token: &str, style: ApiStyle) -> ProviderHealth {
    match fetch_remote_models(base, token, style) {
        Ok(models) => ProviderHealth {
            ok: true,
            kind: ProviderHealthKind::Ready,
            model: models.first().cloned(),
            status: Some("ready".into()),
            error: None,
        },
        Err(error) => {
            let kind = classify_provider_error(&error);
            ProviderHealth {
                ok: false,
                kind,
                model: None,
                status: Some(kind.as_str().into()),
                error: Some(error),
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderStyleProbe {
    pub api_style: ApiStyle,
    pub health: ProviderHealth,
    pub detected: bool,
}

pub fn probe_provider_style(
    base: &str,
    token: &str,
    forced: Option<ApiStyle>,
) -> ProviderStyleProbe {
    if let Some(style) = forced {
        return ProviderStyleProbe {
            api_style: style,
            health: probe_provider_health(base, token, style),
            detected: false,
        };
    }
    let (style, health) = detect_provider_api_style(base, token);
    ProviderStyleProbe {
        api_style: style,
        health,
        detected: true,
    }
}

pub fn detect_provider_api_style(base: &str, token: &str) -> (ApiStyle, ProviderHealth) {
    let timeout = Duration::from_secs(2);
    let openai_models = fetch_remote_models_with_timeout(base, token, ApiStyle::Openai, timeout);
    let anthropic_models =
        fetch_remote_models_with_timeout(base, token, ApiStyle::Anthropic, timeout);
    let openai_route = style_route_signal(base, token, ApiStyle::Openai, timeout);
    let anthropic_route = style_route_signal(base, token, ApiStyle::Anthropic, timeout);

    let mut openai_score = score_style_candidate(
        base,
        token,
        ApiStyle::Openai,
        openai_models.as_ref().ok(),
        openai_route,
    );
    let mut anthropic_score = score_style_candidate(
        base,
        token,
        ApiStyle::Anthropic,
        anthropic_models.as_ref().ok(),
        anthropic_route,
    );

    // Strong exclusive signals win even when the other style's /models also
    // succeeds (common on open local servers that ignore unknown headers).
    if openai_route == RouteSignal::Present && anthropic_route != RouteSignal::Present {
        openai_score += 3;
    }
    if anthropic_route == RouteSignal::Present && openai_route != RouteSignal::Present {
        anthropic_score += 3;
    }

    let prefer_anthropic = anthropic_score > openai_score
        || (anthropic_score == openai_score
            && style_hint(base, token) == Some(ApiStyle::Anthropic));

    if prefer_anthropic {
        (
            ApiStyle::Anthropic,
            health_from_models_result(anthropic_models.or(openai_models)),
        )
    } else {
        (
            ApiStyle::Openai,
            health_from_models_result(openai_models.or(anthropic_models)),
        )
    }
}

fn health_from_models_result(result: Result<Vec<String>, String>) -> ProviderHealth {
    match result {
        Ok(models) => ProviderHealth {
            ok: true,
            kind: ProviderHealthKind::Ready,
            model: models.first().cloned(),
            status: Some("ready".into()),
            error: None,
        },
        Err(error) => {
            let kind = classify_provider_error(&error);
            ProviderHealth {
                ok: false,
                kind,
                model: None,
                status: Some(kind.as_str().into()),
                error: Some(error),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteSignal {
    Present,
    Missing,
    Unknown,
}

fn style_route_signal(base: &str, token: &str, style: ApiStyle, timeout: Duration) -> RouteSignal {
    let Some(base) = normalize_provider_base(base, style) else {
        return RouteSignal::Unknown;
    };
    let path = match style {
        ApiStyle::Openai => "chat/completions",
        ApiStyle::Anthropic => "messages",
    };
    let url = format!("{base}/{path}");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    match request.send() {
        Ok(response) => classify_route_status(response.status().as_u16()),
        Err(error) => {
            if let Some(status) = error.status() {
                classify_route_status(status.as_u16())
            } else {
                RouteSignal::Unknown
            }
        }
    }
}

fn classify_route_status(status: u16) -> RouteSignal {
    match status {
        404 | 410 => RouteSignal::Missing,
        200..=299 | 400..=403 | 405 | 415 | 422 | 429 => RouteSignal::Present,
        _ => RouteSignal::Unknown,
    }
}

fn score_style_candidate(
    base: &str,
    token: &str,
    style: ApiStyle,
    models_ok: Option<&Vec<String>>,
    route: RouteSignal,
) -> i32 {
    let mut score = 0;
    if models_ok.is_some() {
        score += 4;
    }
    match route {
        RouteSignal::Present => score += 2,
        RouteSignal::Missing => score -= 2,
        RouteSignal::Unknown => {}
    }
    if style_hint(base, token) == Some(style) {
        score += 2;
    }
    score
}

fn style_hint(base: &str, token: &str) -> Option<ApiStyle> {
    let token = token.trim().to_ascii_lowercase();
    if token.starts_with("sk-ant-") {
        return Some(ApiStyle::Anthropic);
    }
    let base = base.to_ascii_lowercase();
    if base.contains("anthropic") || base.contains("claude") {
        return Some(ApiStyle::Anthropic);
    }
    if base.contains("openai.com")
        || base.contains("openai.azure")
        || base.contains("ollama")
        || base.contains("groq.com")
        || base.contains("googleapis.com")
        || base.contains("openrouter.ai")
        || base.contains("together.xyz")
        || base.contains("fireworks.ai")
        || base.contains("localhost")
        || base.contains("127.0.0.1")
    {
        return Some(ApiStyle::Openai);
    }
    None
}

pub fn classify_provider_error(error: &str) -> ProviderHealthKind {
    let e = error.to_ascii_lowercase();
    if e.contains("no remote url") || e.contains("no provider") {
        return ProviderHealthKind::Error;
    }
    if e.contains("401")
        || e.contains("403")
        || e.contains("unauthorized")
        || e.contains("forbidden")
    {
        return ProviderHealthKind::Auth;
    }
    if e.contains("returned no models") || e.contains("no models") {
        return ProviderHealthKind::Empty;
    }
    if e.contains("name or service not known")
        || e.contains("nodename nor servname")
        || e.contains("no such host")
        || e.contains("getaddrinfo")
        || e.contains("dns error")
    {
        return ProviderHealthKind::Error;
    }
    if e.contains("timeout")
        || e.contains("timed out")
        || e.contains("connection refused")
        || e.contains("actively refused")
        || e.contains("failed to connect")
        || e.contains("connection reset")
        || e.contains("network unreachable")
        || e.contains("network is unreachable")
        || e.contains("host is down")
        || e.contains("no route to host")
    {
        return ProviderHealthKind::Waiting;
    }
    ProviderHealthKind::Error
}

fn fetch_remote_models(base: &str, token: &str, style: ApiStyle) -> Result<Vec<String>, String> {
    fetch_remote_models_with_timeout(base, token, style, Duration::from_secs(2))
}

/// Resolve whether each model supports controllable reasoning / thinking.
///
/// Uses host/API capability metadata only — never model-id allowlists:
/// 1. Fields on `GET {base}/models` (`reasoning`, `supported_parameters`, `capabilities`)
/// 2. llama-server `GET {root}/props` (chat template caps)
/// 3. Ollama `POST {root}/api/show` → `capabilities` includes `"thinking"`
/// 4. LM Studio `GET {root}/api/v1/models` → `capabilities.reasoning`
/// 5. Anthropic Messages style: API exposes a thinking parameter (protocol-level)
fn fetch_remote_thinking_support(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    models: &[String],
    timeout: Duration,
) -> HashMap<String, bool> {
    let mut out: HashMap<String, bool> = HashMap::new();
    if models.is_empty() {
        return out;
    }

    // Anthropic Messages API has a first-class thinking control; enable unless a
    // later probe explicitly marks a model unsupported.
    if style == ApiStyle::Anthropic {
        for model in models {
            out.insert(model.clone(), true);
        }
    }

    if let Some(from_list) = thinking_from_models_endpoint(api_base, token, style, timeout) {
        for (model, supported) in from_list {
            out.insert(model, supported);
        }
    }

    let Some(root) = props_root_from_openai_base(api_base) else {
        return out;
    };

    // llama-server /props (only meaningful when the route exists).
    if style == ApiStyle::Openai {
        if models.len() == 1 {
            if let Some(body) = fetch_remote_props_body(&root, token, None, timeout) {
                out.insert(models[0].clone(), thinking_support_from_props(&body));
            }
        } else {
            let shared = fetch_remote_props_body(&root, token, None, timeout)
                .map(|body| thinking_support_from_props(&body));
            for model in models {
                if let Some(supported) = fetch_remote_props_body(&root, token, Some(model), timeout)
                    .map(|body| thinking_support_from_props(&body))
                    .or(shared)
                {
                    out.insert(model.clone(), supported);
                }
            }
        }
    }

    let probe_timeout = timeout.min(Duration::from_millis(800));

    // Ollama native show API (skip entirely when /api/tags is absent).
    if root_has_get_path(&root, token, "/api/tags", probe_timeout) {
        for model in models {
            if let Some(supported) =
                fetch_ollama_thinking_capability(&root, token, model, probe_timeout)
            {
                out.insert(model.clone(), supported);
            }
        }
    }

    // LM Studio native models API (reasoning options object).
    if let Some(from_lms) = fetch_lmstudio_thinking_map(&root, token, probe_timeout) {
        for model in models {
            if let Some(supported) = from_lms.get(model).copied() {
                out.insert(model.clone(), supported);
            } else {
                // LM Studio keys sometimes omit publisher prefix.
                let leaf = model.rsplit('/').next().unwrap_or(model);
                if let Some((_, supported)) = from_lms
                    .iter()
                    .find(|(key, _)| key.as_str() == leaf || key.ends_with(&format!("/{leaf}")))
                {
                    out.insert(model.clone(), *supported);
                }
            }
        }
    }

    out
}

fn root_has_get_path(root: &str, token: &str, path: &str, timeout: Duration) -> bool {
    let root = root.trim_end_matches('/');
    let url = format!("{root}{path}");
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.get(&url);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    match request.send() {
        Ok(response) => response.status().as_u16() == 200,
        Err(_) => false,
    }
}

fn thinking_from_models_endpoint(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    timeout: Duration,
) -> Option<HashMap<String, bool>> {
    let base = normalize_provider_base(api_base, style)?;
    let url = format!("{base}/models");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    let response = request.send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = response.json::<serde_json::Value>().ok()?;
    let data = body.get("data")?.as_array()?;
    let mut out = HashMap::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(supported) = thinking_hint_from_model_object(entry) {
            out.insert(id.to_string(), supported);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn thinking_hint_from_model_object(entry: &serde_json::Value) -> Option<bool> {
    // OpenRouter: top-level `reasoning` object.
    if let Some(reasoning) = entry.get("reasoning") {
        if reasoning.is_object() {
            return Some(true);
        }
        if let Some(flag) = reasoning.as_bool() {
            return Some(flag);
        }
    }
    // OpenRouter / others: supported_parameters includes reasoning knobs.
    if let Some(params) = entry.get("supported_parameters").and_then(|v| v.as_array()) {
        let hit = params.iter().any(|p| {
            p.as_str().is_some_and(|s| {
                matches!(
                    s,
                    "reasoning"
                        | "reasoning_effort"
                        | "include_reasoning"
                        | "thinking"
                        | "thinking_budget"
                )
            })
        });
        if hit {
            return Some(true);
        }
    }
    // Nested capabilities.reasoning / capabilities.thinking.
    if let Some(caps) = entry.get("capabilities") {
        if let Some(reasoning) = caps.get("reasoning") {
            if reasoning.is_object() {
                return Some(true);
            }
            if let Some(flag) = reasoning.as_bool() {
                return Some(flag);
            }
        }
        if caps.get("thinking").and_then(|v| v.as_bool()) == Some(true) {
            return Some(true);
        }
    }
    None
}

fn fetch_ollama_thinking_capability(
    root: &str,
    token: &str,
    model: &str,
    timeout: Duration,
) -> Option<bool> {
    let body = fetch_ollama_show_body(root, token, model, timeout)?;
    let caps = body.get("capabilities")?.as_array()?;
    Some(caps.iter().any(|c| {
        c.as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("thinking"))
    }))
}

fn fetch_ollama_show_body(
    root: &str,
    token: &str,
    model: &str,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let root = root.trim_end_matches('/');
    let url = format!("{root}/api/show");
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.post(&url);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    let payload = serde_json::json!({ "model": model });
    let response = request.json(&payload).send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response.json::<serde_json::Value>().ok()
}

/// Resolve whether each model accepts native image / multimodal attachments.
///
/// Uses host/API capability metadata only — never model-id allowlists:
/// 1. Fields on `GET {base}/models` (`capabilities.vision`, modalities, architecture)
/// 2. Ollama `POST {root}/api/show` → `capabilities` includes `"vision"`
/// 3. LM Studio `GET {root}/api/v1/models` → `capabilities.vision`
/// 4. Anthropic Messages style: protocol supports image content blocks
fn fetch_remote_attachments_support(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    models: &[String],
    timeout: Duration,
) -> HashMap<String, bool> {
    let mut out: HashMap<String, bool> = HashMap::new();
    if models.is_empty() {
        return out;
    }

    if style == ApiStyle::Anthropic {
        for model in models {
            out.insert(model.clone(), true);
        }
    }

    if let Some(from_list) = attachments_from_models_endpoint(api_base, token, style, timeout) {
        for (model, supported) in from_list {
            out.insert(model, supported);
        }
    }

    let Some(root) = props_root_from_openai_base(api_base) else {
        return out;
    };
    let probe_timeout = timeout.min(Duration::from_millis(800));

    if root_has_get_path(&root, token, "/api/tags", probe_timeout) {
        for model in models {
            if let Some(supported) =
                fetch_ollama_vision_capability(&root, token, model, probe_timeout)
            {
                out.insert(model.clone(), supported);
            }
        }
    }

    if let Some(from_lms) = fetch_lmstudio_vision_map(&root, token, probe_timeout) {
        for model in models {
            if let Some(supported) = from_lms.get(model).copied() {
                out.insert(model.clone(), supported);
            } else {
                let leaf = model.rsplit('/').next().unwrap_or(model);
                if let Some((_, supported)) = from_lms
                    .iter()
                    .find(|(key, _)| key.as_str() == leaf || key.ends_with(&format!("/{leaf}")))
                {
                    out.insert(model.clone(), *supported);
                }
            }
        }
    }

    out
}

fn fetch_remote_context_lengths(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    timeout: Duration,
) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Some(base) = normalize_provider_base(api_base, style) else {
        return out;
    };
    let url = format!("{base}/models");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    let Ok(response) = request.send() else {
        return out;
    };
    if response.status().as_u16() != 200 {
        return out;
    }
    let Ok(body) = response.json::<serde_json::Value>() else {
        return out;
    };
    let Some(data) = body.get("data").and_then(|v| v.as_array()) else {
        return out;
    };
    for entry in data {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(len) = context_length_from_model_object(entry) {
            out.insert(id.to_string(), len);
        }
    }
    out
}

fn context_length_fuzzy(map: &HashMap<String, u64>, model: &str) -> Option<u64> {
    if let Some(len) = map.get(model) {
        return Some(*len);
    }
    let leaf = model.rsplit('/').next().unwrap_or(model);
    map.iter()
        .find(|(key, _)| key.as_str() == leaf || key.ends_with(&format!("/{leaf}")))
        .map(|(_, len)| *len)
}

fn attachments_from_models_endpoint(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    timeout: Duration,
) -> Option<HashMap<String, bool>> {
    let base = normalize_provider_base(api_base, style)?;
    let url = format!("{base}/models");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    let response = request.send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = response.json::<serde_json::Value>().ok()?;
    let data = body.get("data")?.as_array()?;
    let mut out = HashMap::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(supported) = attachments_hint_from_model_object(entry) {
            out.insert(id.to_string(), supported);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn attachments_hint_from_model_object(entry: &serde_json::Value) -> Option<bool> {
    if let Some(caps) = entry.get("capabilities") {
        if let Some(flag) = caps.get("vision").and_then(|v| v.as_bool()) {
            return Some(flag);
        }
        if let Some(flag) = caps.get("image").and_then(|v| v.as_bool()) {
            return Some(flag);
        }
        if let Some(flag) = caps.get("multimodal").and_then(|v| v.as_bool()) {
            return Some(flag);
        }
        if let Some(arr) = caps.as_array() {
            let hit = arr.iter().any(|c| {
                c.as_str().is_some_and(|s| {
                    matches!(
                        s.to_ascii_lowercase().as_str(),
                        "vision" | "image" | "multimodal" | "images"
                    )
                })
            });
            if hit {
                return Some(true);
            }
        }
    }
    if modality_list_has_image(entry.get("input_modalities"))
        || modality_list_has_image(entry.get("modalities"))
    {
        return Some(true);
    }
    if let Some(arch) = entry.get("architecture")
        && (modality_list_has_image(arch.get("input_modalities"))
            || modality_value_has_image(arch.get("modality"))
            || modality_value_has_image(arch.get("modalities")))
    {
        return Some(true);
    }
    if modality_value_has_image(entry.get("modality")) {
        return Some(true);
    }
    None
}

fn modality_list_has_image(value: Option<&serde_json::Value>) -> bool {
    let Some(arr) = value.and_then(|v| v.as_array()) else {
        return false;
    };
    arr.iter().any(|item| {
        item.as_str().is_some_and(|s| {
            let lower = s.to_ascii_lowercase();
            lower.contains("image") || lower.contains("vision")
        })
    })
}

fn modality_value_has_image(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::String(s)) => {
            let lower = s.to_ascii_lowercase();
            lower.contains("image") || lower.contains("vision")
        }
        Some(serde_json::Value::Array(_)) => modality_list_has_image(value),
        _ => false,
    }
}

fn context_length_from_model_object(entry: &serde_json::Value) -> Option<u64> {
    const KEYS: &[&str] = &[
        "context_length",
        "context_window",
        "max_model_len",
        "max_context_length",
        "n_ctx",
        "n_ctx_train",
    ];
    for key in KEYS {
        if let Some(len) = entry.get(*key).and_then(|v| v.as_u64()).filter(|n| *n > 0) {
            return Some(len);
        }
    }
    if let Some(meta) = entry.get("meta").or_else(|| entry.get("metadata")) {
        for key in KEYS {
            if let Some(len) = meta.get(*key).and_then(|v| v.as_u64()).filter(|n| *n > 0) {
                return Some(len);
            }
        }
    }
    if let Some(len) = entry
        .pointer("/architecture/context_length")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
    {
        return Some(len);
    }
    None
}

fn fetch_ollama_vision_capability(
    root: &str,
    token: &str,
    model: &str,
    timeout: Duration,
) -> Option<bool> {
    let body = fetch_ollama_show_body(root, token, model, timeout)?;
    let caps = body.get("capabilities")?.as_array()?;
    Some(
        caps.iter()
            .any(|c| c.as_str().is_some_and(|s| s.eq_ignore_ascii_case("vision"))),
    )
}

fn fetch_lmstudio_vision_map(
    root: &str,
    token: &str,
    timeout: Duration,
) -> Option<HashMap<String, bool>> {
    let root = root.trim_end_matches('/');
    let url = format!("{root}/api/v1/models");
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.get(&url);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    let response = request.send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = response.json::<serde_json::Value>().ok()?;
    let models = body
        .get("models")
        .and_then(|v| v.as_array())
        .or_else(|| body.get("data").and_then(|v| v.as_array()))?;
    let mut out = HashMap::new();
    for entry in models {
        let id = entry
            .get("key")
            .or_else(|| entry.get("id"))
            .or_else(|| entry.get("name"))
            .and_then(|v| v.as_str())?;
        let caps = entry.get("capabilities")?;
        let supported = caps
            .get("vision")
            .and_then(|v| v.as_bool())
            .or_else(|| caps.get("image").and_then(|v| v.as_bool()))
            .or_else(|| caps.get("multimodal").and_then(|v| v.as_bool()));
        if let Some(flag) = supported {
            out.insert(id.to_string(), flag);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn fetch_lmstudio_thinking_map(
    root: &str,
    token: &str,
    timeout: Duration,
) -> Option<HashMap<String, bool>> {
    let root = root.trim_end_matches('/');
    let url = format!("{root}/api/v1/models");
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.get(&url);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    let response = request.send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let body = response.json::<serde_json::Value>().ok()?;
    let models = body
        .get("models")
        .and_then(|v| v.as_array())
        .or_else(|| body.get("data").and_then(|v| v.as_array()))?;
    let mut out = HashMap::new();
    for entry in models {
        let id = entry
            .get("key")
            .or_else(|| entry.get("id"))
            .or_else(|| entry.get("name"))
            .and_then(|v| v.as_str())?;
        let caps = entry.get("capabilities");
        let supported = match caps {
            Some(caps) => {
                if let Some(reasoning) = caps.get("reasoning") {
                    if let Some(obj) = reasoning.as_object() {
                        // Present reasoning config ⇒ model can think.
                        // If allowed_options is only empty, treat as unsupported.
                        if let Some(opts) = obj.get("allowed_options").and_then(|v| v.as_array()) {
                            !opts.is_empty()
                        } else {
                            true
                        }
                    } else {
                        reasoning.as_bool().unwrap_or(false)
                    }
                } else {
                    caps.get("thinking")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                }
            }
            None => continue,
        };
        out.insert(id.to_string(), supported);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn props_root_from_openai_base(base: &str) -> Option<String> {
    let base = normalize_openai_base(base)?;
    Some(
        base.trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string(),
    )
}

fn fetch_remote_props_body(
    root: &str,
    token: &str,
    model: Option<&str>,
    timeout: Duration,
) -> Option<String> {
    let url = match model {
        Some(model) if !model.is_empty() => {
            format!("{root}/props?model={}", urlencoding_path(model))
        }
        _ => format!("{root}/props"),
    };
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.get(&url);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    let response = request.send().ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response.text().ok()
}

fn urlencoding_path(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            b'/' => out.push_str("%2F"),
            b':' => out.push_str("%3A"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn fetch_remote_models_with_timeout(
    base: &str,
    token: &str,
    style: ApiStyle,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let Some(base) = normalize_provider_base(base, style) else {
        return Err("No provider URL configured".into());
    };
    let url = format!("{base}/models");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    if response.status().as_u16() != 200 {
        return Err(format!("Remote responded with {}", response.status()));
    }
    let body = response
        .json::<serde_json::Value>()
        .map_err(|error| error.to_string())?;
    let models = body
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if models.is_empty() {
        Err("Remote /models returned no models".into())
    } else {
        Ok(models)
    }
}

pub fn probe_provider_catalog(
    base: &str,
    token: &str,
    style: ApiStyle,
    extra_ports: &[u16],
) -> Vec<RemoteModelOption> {
    let Some(primary) = normalize_provider_base(base, style) else {
        return Vec::new();
    };
    let Some((scheme, host, primary_port)) = split_openai_base(&primary) else {
        return Vec::new();
    };

    let mut ports = Vec::new();
    ports.push(primary_port);
    if style == ApiStyle::Openai {
        for port in extra_ports {
            ports.push(*port);
        }
        for port in SCAN_PORTS {
            ports.push(*port);
        }
        for delta in 1..=4u16 {
            ports.push(primary_port.saturating_add(delta));
            if primary_port > delta {
                ports.push(primary_port - delta);
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();

    let sibling_timeout = Duration::from_millis(450);
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for port in ports {
        let candidate = format!("{scheme}://{host}:{port}/v1");
        let timeout = if port == primary_port {
            Duration::from_secs(2)
        } else {
            sibling_timeout
        };
        let Ok(models) = fetch_remote_models_with_timeout(&candidate, token, style, timeout) else {
            continue;
        };
        let thinking = fetch_remote_thinking_support(&candidate, token, style, &models, timeout);
        let attachments =
            fetch_remote_attachments_support(&candidate, token, style, &models, timeout);
        let contexts = fetch_remote_context_lengths(&candidate, token, style, timeout);
        for model in models {
            let id = format!("remote|{candidate}|{model}");
            if !seen.insert(id.clone()) {
                continue;
            }
            let thinking_supported = thinking.get(&model).copied().unwrap_or(false);
            let attachments_supported = attachments.get(&model).copied().unwrap_or(false);
            let context_length = contexts
                .get(&model)
                .copied()
                .or_else(|| context_length_fuzzy(&contexts, &model));
            out.push(RemoteModelOption {
                id,
                model: model.clone(),
                base: candidate.clone(),
                port,
                ready: true,
                label: model,
                thinking_supported,
                attachments_supported,
                context_length,
                provider_id: String::new(),
                provider_name: String::new(),
            });
        }
    }
    out.sort_by(|a, b| a.port.cmp(&b.port).then(a.model.cmp(&b.model)));
    out.sort_by(|a, b| {
        let a_primary = a.base == primary;
        let b_primary = b.base == primary;
        b_primary
            .cmp(&a_primary)
            .then(a.model.cmp(&b.model))
            .then(a.port.cmp(&b.port))
    });
    let mut deduped = Vec::with_capacity(out.len());
    let mut seen_models = std::collections::HashSet::new();
    for item in out {
        if seen_models.insert(item.model.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

pub fn split_openai_base(base: &str) -> Option<(String, String, u16)> {
    let base = normalize_openai_base(base)?;
    let url = base.trim_end_matches("/v1");
    let without_scheme = url.split("://").nth(1)?;
    let scheme = url.split("://").next()?.to_string();
    let authority = without_scheme.split('/').next()?;
    let (host, port) = if let Some((h, p)) = authority.rsplit_once(':') {
        let port = p.parse().ok()?;
        (h.to_string(), port)
    } else {
        let port = if scheme == "https" { 443 } else { 80 };
        (authority.to_string(), port)
    };
    Some((scheme, host, port))
}

pub fn provider_base_same_host(linked: &str, candidate: &str) -> bool {
    let Some((_, host_a, _)) = split_openai_base(linked) else {
        return false;
    };
    let Some((_, host_b, _)) = split_openai_base(candidate) else {
        return false;
    };
    host_a.eq_ignore_ascii_case(&host_b)
}

/// Prefer an exact normalized base match, then same-host, for multi-provider catalogs.
pub fn find_provider_for_base<'a>(
    providers: &'a [Provider],
    requested: &str,
) -> Option<&'a Provider> {
    providers
        .iter()
        .find(|p| {
            normalize_openai_base(requested).is_some_and(|norm| {
                normalize_openai_base(&p.base)
                    .as_ref()
                    .is_some_and(|base| base.eq_ignore_ascii_case(&norm))
            })
        })
        .or_else(|| {
            providers
                .iter()
                .find(|p| provider_base_same_host(&p.base, requested))
        })
}

pub fn thinking_support_from_props(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    if let Some(caps) = value.get("chat_template_caps").and_then(|v| v.as_object()) {
        const FLAGS: &[&str] = &[
            "supports_thinking",
            "supports_enable_thinking",
            "supports_reasoning_effort",
            "supports_preserve_reasoning",
        ];
        if FLAGS
            .iter()
            .any(|key| caps.get(*key).and_then(|v| v.as_bool()) == Some(true))
        {
            return true;
        }
    }
    value
        .get("chat_template")
        .and_then(|v| v.as_str())
        .is_some_and(template_suggests_thinking)
}

fn template_suggests_thinking(template: &str) -> bool {
    if template.is_empty() {
        return false;
    }
    const KWARGS: &[&str] = &["enable_thinking", "reasoning_effort", "thinking_budget"];
    for kwarg in KWARGS {
        if jinja_mentions_ident(template, kwarg) {
            return true;
        }
    }
    let lower = template.to_ascii_lowercase();
    if lower.contains("{% if")
        && (lower.contains("enable_thinking")
            || lower.contains("enable thinking")
            || lower.contains("ns.enable_thinking")
            || (lower.contains("thinking")
                && (lower.contains(" is not") || lower.contains("==") || lower.contains("!=")))
            || (lower.contains("reasoning")
                && (lower.contains(" is not") || lower.contains("==") || lower.contains("!="))))
    {
        return true;
    }
    const PAIRS: &[(&str, Option<&str>)] = &[
        ("<think>", Some("</think>")),
        ("<thinking>", Some("</thinking>")),
        ("<|think|>", Some("</|think|>")),
        ("<seed:think>", Some("</seed:think>")),
        ("<|channel|>thought", Some("<|channel|>")),
        ("<|channel|>analysis", Some("<|channel|>")),
        ("<|channel>thought", Some("<|channel|>")),
        ("reasoning_content", None),
        ("redacted_thinking", None),
    ];
    for (start, end) in PAIRS {
        if template.contains(start) && end.is_none_or(|e| template.contains(e)) {
            return true;
        }
    }
    false
}

fn jinja_mentions_ident(template: &str, ident: &str) -> bool {
    let mut rest = template;
    while let Some(open) = rest.find("{{").or_else(|| rest.find("{%")) {
        let chunk = &rest[open..];
        let close = if chunk.starts_with("{{") {
            chunk.find("}}").map(|i| i + 2)
        } else {
            chunk.find("%}").map(|i| i + 2)
        };
        let Some(end) = close else { break };
        let block = &chunk[..end];
        if block
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|tok| tok.eq_ignore_ascii_case(ident))
        {
            return true;
        }
        rest = &chunk[end..];
    }
    template
        .to_ascii_lowercase()
        .contains(&ident.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_hint_from_openrouter_style_object() {
        let entry = serde_json::json!({
            "id": "openai/o4-mini",
            "reasoning": { "supported_efforts": ["high", "medium", "low"] }
        });
        assert_eq!(thinking_hint_from_model_object(&entry), Some(true));
        let params = serde_json::json!({
            "id": "x",
            "supported_parameters": ["temperature", "reasoning_effort"]
        });
        assert_eq!(thinking_hint_from_model_object(&params), Some(true));
        let plain = serde_json::json!({ "id": "gpt-4o", "supported_parameters": ["temperature"] });
        assert_eq!(thinking_hint_from_model_object(&plain), None);
    }

    #[test]
    fn style_hint_from_token_and_host() {
        assert_eq!(
            style_hint("https://example.com/v1", "sk-ant-abc"),
            Some(ApiStyle::Anthropic)
        );
        assert_eq!(
            style_hint("https://api.anthropic.com/v1", ""),
            Some(ApiStyle::Anthropic)
        );
        assert_eq!(
            style_hint("https://api.openai.com/v1", "sk-abc"),
            Some(ApiStyle::Openai)
        );
        assert_eq!(
            style_hint("http://127.0.0.1:11434/v1", ""),
            Some(ApiStyle::Openai)
        );
    }

    #[test]
    fn classify_route_status_distinguishes_missing() {
        assert_eq!(classify_route_status(404), RouteSignal::Missing);
        assert_eq!(classify_route_status(405), RouteSignal::Present);
        assert_eq!(classify_route_status(401), RouteSignal::Present);
        assert_eq!(classify_route_status(200), RouteSignal::Present);
    }

    #[test]
    fn attachments_hint_from_vision_capabilities() {
        let vision = serde_json::json!({
            "id": "llava",
            "capabilities": { "vision": true }
        });
        assert_eq!(attachments_hint_from_model_object(&vision), Some(true));
        let modality = serde_json::json!({
            "id": "gpt-4o",
            "architecture": { "modality": "text+image->text" }
        });
        assert_eq!(attachments_hint_from_model_object(&modality), Some(true));
        let plain = serde_json::json!({ "id": "llama3", "supported_parameters": ["temperature"] });
        assert_eq!(attachments_hint_from_model_object(&plain), None);
    }

    #[test]
    fn context_length_from_common_fields() {
        let entry = serde_json::json!({
            "id": "m",
            "context_length": 128000
        });
        assert_eq!(context_length_from_model_object(&entry), Some(128000));
    }
}
