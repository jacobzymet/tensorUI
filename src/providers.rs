use std::{
    collections::{HashMap, hash_map::RandomState},
    hash::BuildHasher,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{anthropic, http};

/// How long a probe result is considered fresh. Expired entries are still served
/// (stale-while-revalidate) so Chat polling never briefly sees an empty catalog.
const HEALTH_CACHE: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const LOCAL_PROBE_TIMEOUT: Duration = Duration::from_millis(800);

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
    /// Explicit opt-in for self-signed or otherwise invalid HTTPS certificates.
    #[serde(default)]
    pub allow_insecure_tls: bool,
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
            allow_insecure_tls: false,
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
    pub allow_insecure_tls: bool,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderUpsertOptions {
    pub allow_insecure_tls: Option<bool>,
    pub activate: bool,
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
        options: ProviderUpsertOptions,
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
            if let Some(allow) = options.allow_insecure_tls {
                provider.allow_insecure_tls = allow;
            }
            if let Some(token) = token {
                provider.token = token.to_string();
            }
            let updated = provider.clone();
            if options.activate {
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
        if options.activate || self.items.len() == 1 {
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
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return "••••••••".into();
    }
    format!(
        "{}…{}",
        chars[..4].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
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
    /// Advertised request dialect for controllable reasoning. Absent means
    /// reasoning may exist, but TensorMI Harness cannot safely control its intensity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_control: Option<String>,
    /// Exact effort values accepted by this model, normalized to the UI set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_efforts: Vec<String>,
    #[serde(default)]
    pub thinking_can_disable: bool,
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

/// Translate TensorMI Harness's canonical effort into the one request dialect this
/// model explicitly advertised. Unsupported values deliberately become Auto
/// (no field) instead of being guessed or rounded.
pub fn apply_thinking_control(body: &mut serde_json::Value, model: Option<&RemoteModelOption>) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    let Some(requested) = object
        .remove("thinking_effort")
        .and_then(|value| value.as_str().map(str::to_string))
    else {
        return;
    };
    let Some(model) = model else { return };
    let effort = match requested.as_str() {
        "off" if model.thinking_can_disable => "none",
        "low" | "medium" | "high" | "max"
            if model
                .thinking_efforts
                .iter()
                .any(|value| value == &requested) =>
        {
            requested.as_str()
        }
        _ => return,
    };
    match model.thinking_control.as_deref() {
        Some("reasoning") => {
            object.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
        }
        Some("reasoning_effort") => {
            object.insert("reasoning_effort".into(), serde_json::json!(effort));
        }
        Some("chat_template") => {
            let kwargs = object
                .entry("chat_template_kwargs")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(kwargs) = kwargs.as_object_mut() {
                kwargs.insert(
                    "enable_thinking".into(),
                    serde_json::json!(effort != "none"),
                );
                if effort != "none" {
                    kwargs.insert("reasoning_effort".into(), serde_json::json!(effort));
                }
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Default)]
struct ThinkingCapabilities {
    supported: bool,
    control: Option<String>,
    efforts: Vec<String>,
    can_disable: bool,
}

fn merge_thinking_capabilities(
    target: &mut HashMap<String, ThinkingCapabilities>,
    model: &str,
    incoming: ThinkingCapabilities,
) {
    let current = target.entry(model.to_string()).or_default();
    current.supported |= incoming.supported;
    if current.control.is_none() && incoming.control.is_some() {
        current.control = incoming.control;
        current.efforts = incoming.efforts;
        current.can_disable = incoming.can_disable;
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct CacheKey(u64, u64);

#[derive(Debug, Default)]
struct CacheKeyer {
    first: RandomState,
    second: RandomState,
}

impl CacheKeyer {
    fn key(&self, style: ApiStyle, base: &str, token: &str) -> CacheKey {
        let material = (style.as_str(), base, token);
        CacheKey(
            self.first.hash_one(material),
            self.second.hash_one(material),
        )
    }
}

#[derive(Debug, Default)]
pub struct HealthCache {
    last: Mutex<HashMap<CacheKey, (Instant, ProviderHealth)>>,
    keys: CacheKeyer,
}

#[derive(Debug, Default)]
pub struct CatalogCache {
    last: Mutex<HashMap<CacheKey, (Instant, Vec<RemoteModelOption>)>>,
    keys: CacheKeyer,
}

impl HealthCache {
    pub fn clear(&self) {
        if let Ok(mut guard) = self.last.lock() {
            for (_, (_, mut health)) in guard.drain() {
                health.model.zeroize();
                health.status.zeroize();
                health.error.zeroize();
            }
        }
    }

    pub fn peek(&self, style: ApiStyle, base: &str, token: &str) -> Option<ProviderHealth> {
        let key = self.keys.key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return None;
        };
        guard.get(&key).map(|(_, health)| health.clone())
    }

    pub fn is_fresh(&self, style: ApiStyle, base: &str, token: &str) -> bool {
        let key = self.keys.key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return false;
        };
        matches!(guard.get(&key), Some((at, _)) if at.elapsed() < HEALTH_CACHE)
    }

    pub fn put(&self, style: ApiStyle, base: &str, token: &str, health: ProviderHealth) {
        let key = self.keys.key(style, base, token);
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
    pub fn clear(&self) {
        if let Ok(mut guard) = self.last.lock() {
            for (_, (_, mut catalog)) in guard.drain() {
                for model in &mut catalog {
                    model.id.zeroize();
                    model.model.zeroize();
                    model.base.zeroize();
                    model.label.zeroize();
                    model.thinking_control.zeroize();
                    model.thinking_efforts.zeroize();
                    model.provider_id.zeroize();
                    model.provider_name.zeroize();
                }
            }
        }
    }

    pub fn peek(&self, style: ApiStyle, base: &str, token: &str) -> Option<Vec<RemoteModelOption>> {
        let key = self.keys.key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return None;
        };
        guard.get(&key).map(|(_, catalog)| catalog.clone())
    }

    pub fn is_fresh(&self, style: ApiStyle, base: &str, token: &str) -> bool {
        let key = self.keys.key(style, base, token);
        let Ok(guard) = self.last.lock() else {
            return false;
        };
        matches!(guard.get(&key), Some((at, _)) if at.elapsed() < HEALTH_CACHE)
    }

    pub fn put(&self, style: ApiStyle, base: &str, token: &str, catalog: Vec<RemoteModelOption>) {
        let key = self.keys.key(style, base, token);
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

    pub fn probe(&self, style: ApiStyle, base: &str, token: &str) -> Vec<RemoteModelOption> {
        if self.is_fresh(style, base, token)
            && let Some(catalog) = self.peek(style, base, token)
        {
            return catalog;
        }
        let catalog = probe_provider_catalog(base, token, style);
        self.put(style, base, token, catalog.clone());
        self.peek(style, base, token).unwrap_or(catalog)
    }
}

pub fn probe_provider_health(base: &str, token: &str, style: ApiStyle) -> ProviderHealth {
    probe_provider_endpoint(base, token, style).0
}

/// One `GET {base}/models`. Health and capability flags are derived from that JSON.
pub fn probe_provider_endpoint(
    base: &str,
    token: &str,
    style: ApiStyle,
) -> (ProviderHealth, Vec<RemoteModelOption>) {
    catalog_and_health_from_payload(
        base,
        style,
        fetch_models_payload(base, token, style, PROBE_TIMEOUT),
    )
}

pub fn probe_provider_catalog(base: &str, token: &str, style: ApiStyle) -> Vec<RemoteModelOption> {
    probe_provider_endpoint(base, token, style).1
}

#[derive(Debug, Clone)]
pub struct ProviderStyleProbe {
    pub api_style: ApiStyle,
    pub health: ProviderHealth,
    pub detected: bool,
    pub catalog: Vec<RemoteModelOption>,
}

pub fn probe_provider_style(
    base: &str,
    token: &str,
    forced: Option<ApiStyle>,
) -> ProviderStyleProbe {
    if let Some(style) = forced {
        let (health, catalog) = probe_provider_endpoint(base, token, style);
        return ProviderStyleProbe {
            api_style: style,
            health,
            detected: false,
            catalog,
        };
    }
    let (style, health, catalog) = detect_provider_api_style(base, token);
    ProviderStyleProbe {
        api_style: style,
        health,
        detected: true,
        catalog,
    }
}

pub fn detect_provider_api_style(
    base: &str,
    token: &str,
) -> (ApiStyle, ProviderHealth, Vec<RemoteModelOption>) {
    let timeout = PROBE_TIMEOUT;
    let (openai_payload, anthropic_payload, openai_route, anthropic_route) =
        std::thread::scope(|scope| {
            let openai_payload =
                scope.spawn(|| fetch_models_payload(base, token, ApiStyle::Openai, timeout));
            let anthropic_payload =
                scope.spawn(|| fetch_models_payload(base, token, ApiStyle::Anthropic, timeout));
            let openai_route =
                scope.spawn(|| style_route_signal(base, token, ApiStyle::Openai, timeout));
            let anthropic_route =
                scope.spawn(|| style_route_signal(base, token, ApiStyle::Anthropic, timeout));
            (
                openai_payload
                    .join()
                    .unwrap_or_else(|_| Err("style probe failed".into())),
                anthropic_payload
                    .join()
                    .unwrap_or_else(|_| Err("style probe failed".into())),
                openai_route.join().unwrap_or(RouteSignal::Unknown),
                anthropic_route.join().unwrap_or(RouteSignal::Unknown),
            )
        });

    let openai_models = openai_payload
        .as_ref()
        .ok()
        .and_then(|body| model_ids_from_body(body).ok());
    let anthropic_models = anthropic_payload
        .as_ref()
        .ok()
        .and_then(|body| model_ids_from_body(body).ok());

    let mut openai_score = score_style_candidate(
        base,
        token,
        ApiStyle::Openai,
        openai_models.as_ref(),
        openai_route,
    );
    let mut anthropic_score = score_style_candidate(
        base,
        token,
        ApiStyle::Anthropic,
        anthropic_models.as_ref(),
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

    let (style, winner, fallback) = if prefer_anthropic {
        (ApiStyle::Anthropic, anthropic_payload, openai_payload)
    } else {
        (ApiStyle::Openai, openai_payload, anthropic_payload)
    };
    let (mut health, catalog) = catalog_and_health_from_payload(base, style, winner);
    if !health.ok {
        health = catalog_and_health_from_payload(base, style, fallback).0;
    }
    (style, health, catalog)
}

fn catalog_and_health_from_payload(
    base: &str,
    style: ApiStyle,
    payload: Result<serde_json::Value, String>,
) -> (ProviderHealth, Vec<RemoteModelOption>) {
    match payload {
        Ok(body) => {
            let catalog = catalog_from_models_body(base, style, &body);
            let health = if catalog.is_empty() {
                health_from_models_result(Err("Remote /models returned no models".into()))
            } else {
                health_from_models_result(Ok(catalog
                    .iter()
                    .map(|item| item.model.clone())
                    .collect()))
            };
            (health, catalog)
        }
        Err(error) => (health_from_models_result(Err(error)), Vec::new()),
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
    let mut request = client.get(&url).timeout(timeout);
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

fn probe_http_error(error: reqwest::Error) -> String {
    let kind = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "failed to connect"
    } else {
        ""
    };
    let mut msg = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(inner) = source {
        let next = inner.to_string();
        if !next.is_empty() && !msg.contains(&next) {
            msg = format!("{msg}: {next}");
        }
        source = inner.source();
    }
    if kind.is_empty() || msg.to_ascii_lowercase().contains(kind) {
        msg
    } else {
        format!("{kind}: {msg}")
    }
}

fn catalog_from_models_body(
    api_base: &str,
    style: ApiStyle,
    body: &serde_json::Value,
) -> Vec<RemoteModelOption> {
    let Some(primary) = normalize_provider_base(api_base, style) else {
        return Vec::new();
    };
    let Ok(models) = model_ids_from_body(body) else {
        return Vec::new();
    };
    let thinking = thinking_from_models_body(body);
    let attachments = attachments_from_models_body(body, style);
    let contexts = contexts_from_models_body(body);
    let port = split_openai_base(&primary)
        .map(|(_, _, port)| port)
        .unwrap_or(80);
    let mut out = Vec::with_capacity(models.len());
    let mut seen = std::collections::HashSet::new();
    for model in models {
        let id = format!("remote|{primary}|{model}");
        if !seen.insert(id.clone()) {
            continue;
        }
        let thinking = thinking.get(&model).cloned().unwrap_or_default();
        let attachments_supported = attachments.get(&model).copied().unwrap_or(false);
        let context_length = contexts
            .get(&model)
            .copied()
            .or_else(|| context_length_fuzzy(&contexts, &model));
        out.push(RemoteModelOption {
            id,
            model: model.clone(),
            base: primary.clone(),
            port,
            ready: true,
            label: model,
            thinking_supported: thinking.supported,
            thinking_control: thinking.control,
            thinking_efforts: thinking.efforts,
            thinking_can_disable: thinking.can_disable,
            attachments_supported,
            context_length,
            provider_id: String::new(),
            provider_name: String::new(),
        });
    }
    out.sort_by(|a, b| a.model.cmp(&b.model));
    out
}

/// Local-only capability routes (`/props`, Ollama `/api/show`, LM Studio).
/// Safe to run after the `/models` catalog is already stored so the UI is not blocked.
pub fn enrich_local_catalog(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    mut catalog: Vec<RemoteModelOption>,
) -> Vec<RemoteModelOption> {
    if catalog.is_empty() || !base_is_local(api_base) {
        return catalog;
    }
    let models: Vec<String> = catalog.iter().map(|item| item.model.clone()).collect();
    let thinking = local_thinking_support(api_base, token, style, &models, LOCAL_PROBE_TIMEOUT);
    let attachments =
        local_attachments_support(api_base, token, style, &models, LOCAL_PROBE_TIMEOUT);
    for item in &mut catalog {
        if let Some(capabilities) = thinking.get(&item.model) {
            item.thinking_supported |= capabilities.supported;
            if item.thinking_control.is_none() && capabilities.control.is_some() {
                item.thinking_control = capabilities.control.clone();
                item.thinking_efforts = capabilities.efforts.clone();
                item.thinking_can_disable = capabilities.can_disable;
            }
        }
        if let Some(supported) = attachments.get(&item.model).copied() {
            item.attachments_supported |= supported;
        }
    }
    catalog
}

/// Resolve whether each model supports controllable reasoning / thinking.
///
/// Uses host/API capability metadata only — never model-id allowlists:
/// 1. Fields on `GET {base}/models` (`reasoning`, `supported_parameters`, `capabilities`)
/// 2. llama-server `GET {root}/props` (chat template caps)
/// 3. Ollama `POST {root}/api/show` → `capabilities` includes `"thinking"`
/// 4. LM Studio `GET {root}/api/v1/models` → `capabilities.reasoning`
///
/// Anthropic Messages can translate `reasoning_effort`, but the models list does
/// not say which Claude variants accept extended thinking — so we never invent
/// a control dialect from ApiStyle alone (unlike image attachments).
fn local_thinking_support(
    api_base: &str,
    token: &str,
    style: ApiStyle,
    models: &[String],
    timeout: Duration,
) -> HashMap<String, ThinkingCapabilities> {
    let mut out: HashMap<String, ThinkingCapabilities> = HashMap::new();
    if models.is_empty() {
        return out;
    }

    let Some(root) = props_root_from_openai_base(api_base) else {
        return out;
    };

    let probe_timeout = timeout.min(LOCAL_PROBE_TIMEOUT);

    // llama-server /props. Confirm the route exists once before asking per
    // model, otherwise a server without it costs one request per model.
    if style == ApiStyle::Openai && root_has_get_path(&root, token, "/props", probe_timeout) {
        if models.len() == 1 {
            if let Some(body) = fetch_remote_props_body(&root, token, None, timeout) {
                merge_thinking_capabilities(
                    &mut out,
                    &models[0],
                    thinking_capabilities_from_props(&body),
                );
            }
        } else {
            let shared = fetch_remote_props_body(&root, token, None, timeout)
                .map(|body| thinking_capabilities_from_props(&body));
            for model in models {
                if let Some(capabilities) =
                    fetch_remote_props_body(&root, token, Some(model), timeout)
                        .map(|body| thinking_capabilities_from_props(&body))
                        .or_else(|| shared.clone())
                {
                    merge_thinking_capabilities(&mut out, model, capabilities);
                }
            }
        }
    }

    if root_has_get_path(&root, token, "/api/tags", probe_timeout) {
        for model in models {
            if let Some(supported) =
                fetch_ollama_thinking_capability(&root, token, model, probe_timeout)
            {
                out.entry(model.clone()).or_default().supported |= supported;
            }
        }
    }

    if let Some(from_lms) = fetch_lmstudio_thinking_map(&root, token, probe_timeout) {
        for model in models {
            if let Some(capabilities) = from_lms.get(model).cloned() {
                merge_thinking_capabilities(&mut out, model, capabilities);
            } else {
                let leaf = model.rsplit('/').next().unwrap_or(model);
                if let Some((_, capabilities)) = from_lms
                    .iter()
                    .find(|(key, _)| key.as_str() == leaf || key.ends_with(&format!("/{leaf}")))
                {
                    merge_thinking_capabilities(&mut out, model, capabilities.clone());
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
    let mut request = client.get(&url).timeout(timeout);
    if !token.trim().is_empty() {
        request = request.header("Authorization", &format!("Bearer {}", token.trim()));
    }
    match request.send() {
        Ok(response) => response.status().as_u16() == 200,
        Err(_) => false,
    }
}

fn thinking_from_models_body(body: &serde_json::Value) -> HashMap<String, ThinkingCapabilities> {
    let Some(data) = body.get("data").and_then(|v| v.as_array()) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(capabilities) = thinking_capabilities_from_model_object(entry) {
            out.insert(id.to_string(), capabilities);
        }
    }
    out
}

fn thinking_capabilities_from_model_object(
    entry: &serde_json::Value,
) -> Option<ThinkingCapabilities> {
    // A structured reasoning descriptor is the safest source: it can advertise
    // exact accepted efforts and whether disabling is legal without identifying
    // the provider that produced it.
    if let Some(reasoning) = entry.get("reasoning") {
        if let Some(object) = reasoning.as_object() {
            // Only trust an explicit boolean. Missing `mandatory` must not unlock Off.
            let can_disable = object.get("mandatory").and_then(|v| v.as_bool()) == Some(false);
            let efforts = match object.get("supported_efforts") {
                Some(serde_json::Value::Array(values)) => normalize_thinking_efforts(values),
                // null = "host accepts its full set" — we only expose the shared UI
                // subset that every such host is known to accept (never invent `max`).
                Some(serde_json::Value::Null) => standard_thinking_efforts(),
                _ => Vec::new(),
            };
            return Some(ThinkingCapabilities {
                supported: true,
                control: (!efforts.is_empty() || can_disable).then(|| "reasoning".to_string()),
                efforts,
                can_disable,
            });
        }
        if let Some(flag) = reasoning.as_bool() {
            return Some(ThinkingCapabilities {
                supported: flag,
                ..Default::default()
            });
        }
    }
    // A parameter name proves reasoning exists, but does not prove which effort
    // values are legal. Keep it informational unless richer metadata follows.
    if let Some(params) = entry.get("supported_parameters").and_then(|v| v.as_array()) {
        let has = |name: &str| params.iter().any(|p| p.as_str() == Some(name));
        if has("reasoning_effort") {
            return Some(ThinkingCapabilities {
                supported: true,
                ..Default::default()
            });
        }
        if has("reasoning") || has("include_reasoning") || has("thinking") || has("thinking_budget")
        {
            return Some(ThinkingCapabilities {
                supported: true,
                ..Default::default()
            });
        }
    }
    // Nested capabilities.reasoning / capabilities.thinking.
    if let Some(caps) = entry.get("capabilities") {
        if let Some(reasoning) = caps.get("reasoning") {
            if let Some(object) = reasoning.as_object() {
                let options = object.get("allowed_options").and_then(|v| v.as_array());
                let efforts = options
                    .map(|values| normalize_thinking_efforts(values))
                    .unwrap_or_default();
                let can_disable = options.is_some_and(|values| {
                    values.iter().any(|value| value.as_str() == Some("none"))
                });
                return Some(ThinkingCapabilities {
                    supported: true,
                    control: (!efforts.is_empty() || can_disable)
                        .then(|| "reasoning_effort".into()),
                    efforts,
                    can_disable,
                });
            }
            if let Some(flag) = reasoning.as_bool() {
                return Some(ThinkingCapabilities {
                    supported: flag,
                    ..Default::default()
                });
            }
        }
        if caps.get("thinking").and_then(|v| v.as_bool()) == Some(true) {
            return Some(ThinkingCapabilities {
                supported: true,
                ..Default::default()
            });
        }
    }
    None
}

fn normalize_thinking_efforts(values: &[serde_json::Value]) -> Vec<String> {
    const UI_EFFORTS: &[&str] = &["low", "medium", "high", "max"];
    UI_EFFORTS
        .iter()
        .filter(|effort| values.iter().any(|value| value.as_str() == Some(**effort)))
        .map(|effort| (*effort).to_string())
        .collect()
}

fn standard_thinking_efforts() -> Vec<String> {
    ["low", "medium", "high"]
        .into_iter()
        .map(str::to_string)
        .collect()
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
    let mut request = client.post(&url).timeout(timeout);
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
fn local_attachments_support(
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

    let Some(root) = props_root_from_openai_base(api_base) else {
        return out;
    };
    let probe_timeout = timeout.min(LOCAL_PROBE_TIMEOUT);

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

fn attachments_from_models_body(
    body: &serde_json::Value,
    style: ApiStyle,
) -> HashMap<String, bool> {
    let mut out = HashMap::new();
    let Some(data) = body.get("data").and_then(|v| v.as_array()) else {
        return out;
    };
    for entry in data {
        let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if style == ApiStyle::Anthropic {
            out.insert(id.to_string(), true);
        }
        if let Some(supported) = attachments_hint_from_model_object(entry) {
            out.insert(id.to_string(), supported);
        }
    }
    out
}

fn contexts_from_models_body(body: &serde_json::Value) -> HashMap<String, u64> {
    let mut out = HashMap::new();
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
    let mut request = client.get(&url).timeout(timeout);
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
) -> Option<HashMap<String, ThinkingCapabilities>> {
    let root = root.trim_end_matches('/');
    let url = format!("{root}/api/v1/models");
    let client = http::llm_blocking_client(root, timeout);
    let mut request = client.get(&url).timeout(timeout);
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
        let Some(capabilities) = thinking_capabilities_from_model_object(entry) else {
            continue;
        };
        out.insert(id.to_string(), capabilities);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// True when a provider looks like a local inference server (llama.cpp,
/// Ollama, LM Studio). Their capability routes — `/props`, `/api/tags`,
/// `/api/v1/models` — exist nowhere else, so probing them against a cloud API
/// is pure latency. At one request per model that is also a large enough burst
/// to get rate-limited or blocked, which takes the real requests down with it.
fn base_is_local(api_base: &str) -> bool {
    // Reuse the app's private/lab-host classification so inference servers on
    // a LAN keep their Ollama/LM Studio capability detection too. The explicit
    // host check also covers bracketed IPv6 loopback URLs.
    http::url_is_private_or_local(api_base)
        || split_openai_base(api_base).is_some_and(|(_, host, _)| host_is_local(&host))
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
    let mut request = client.get(&url).timeout(timeout);
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

fn fetch_models_payload(
    base: &str,
    token: &str,
    style: ApiStyle,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let Some(base) = normalize_provider_base(base, style) else {
        return Err("No provider URL configured".into());
    };
    let url = format!("{base}/models");
    let client = http::llm_blocking_client(&base, timeout);
    let mut request = client.get(&url).timeout(timeout);
    for (name, value) in provider_auth_headers(style, token) {
        request = request.header(&name, &value);
    }
    let response = request.send().map_err(probe_http_error)?;
    if response.status().as_u16() != 200 {
        return Err(format!("Remote responded with {}", response.status()));
    }
    response
        .json::<serde_json::Value>()
        .map_err(|error| error.to_string())
}

fn model_ids_from_body(body: &serde_json::Value) -> Result<Vec<String>, String> {
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

/// True for loopback and common local hostnames.
fn host_is_local(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
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
    thinking_capabilities_from_props(body).supported
}

fn thinking_capabilities_from_props(body: &str) -> ThinkingCapabilities {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return ThinkingCapabilities::default();
    };
    let template = value
        .get("chat_template")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let mut supports_effort = jinja_mentions_ident(template, "reasoning_effort");
    let mut can_disable = jinja_mentions_ident(template, "enable_thinking");
    let mut reports_thinking = false;
    if let Some(caps) = value.get("chat_template_caps").and_then(|v| v.as_object()) {
        supports_effort |= caps
            .get("supports_reasoning_effort")
            .and_then(|v| v.as_bool())
            == Some(true);
        can_disable |= caps
            .get("supports_enable_thinking")
            .and_then(|v| v.as_bool())
            == Some(true);
        reports_thinking = ["supports_thinking", "supports_preserve_reasoning"]
            .iter()
            .any(|key| caps.get(*key).and_then(|v| v.as_bool()) == Some(true));
    }
    let supported =
        supports_effort || can_disable || reports_thinking || template_suggests_thinking(template);
    ThinkingCapabilities {
        supported,
        control: (supports_effort || can_disable).then(|| "chat_template".into()),
        efforts: if supports_effort {
            vec!["low".into(), "medium".into(), "high".into()]
        } else {
            Vec::new()
        },
        can_disable,
    }
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
    fn masks_unicode_tokens_without_byte_slicing() {
        assert_eq!(mask_token("🔑🔑🔑🔑abcdef🔒🔒🔒🔒"), "🔑🔑🔑🔑…🔒🔒🔒🔒");
        assert_eq!(mask_token("éééé"), "••••••••");
    }

    fn reasoning_model(
        control: Option<&str>,
        efforts: &[&str],
        can_disable: bool,
    ) -> RemoteModelOption {
        RemoteModelOption {
            id: "remote|test|model".into(),
            model: "model".into(),
            base: "https://example.com/v1".into(),
            port: 443,
            ready: true,
            label: "model".into(),
            thinking_supported: true,
            thinking_control: control.map(str::to_string),
            thinking_efforts: efforts.iter().map(|value| (*value).into()).collect(),
            thinking_can_disable: can_disable,
            attachments_supported: false,
            context_length: None,
            provider_id: String::new(),
            provider_name: String::new(),
        }
    }

    #[test]
    fn catalog_from_models_uses_configured_base_and_capability_fields() {
        let body = serde_json::json!({
            "data": [{
                "id": "openai/o4-mini",
                "reasoning": { "supported_efforts": ["high", "medium", "low"] },
                "capabilities": { "vision": true },
                "context_length": 128000
            }]
        });
        let catalog =
            catalog_from_models_body("https://openrouter.ai/api/v1", ApiStyle::Openai, &body);
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].base, "https://openrouter.ai/api/v1");
        assert_eq!(catalog[0].port, 443);
        assert_eq!(catalog[0].model, "openai/o4-mini");
        assert!(catalog[0].thinking_supported);
        assert!(catalog[0].attachments_supported);
        assert_eq!(catalog[0].context_length, Some(128000));
    }

    #[test]
    fn catalog_from_models_preserves_local_path_prefix() {
        let body = serde_json::json!({ "data": [{ "id": "local-model" }] });
        let catalog =
            catalog_from_models_body("http://127.0.0.1:8099/custom/v1", ApiStyle::Openai, &body);
        assert_eq!(catalog[0].base, "http://127.0.0.1:8099/custom/v1");
        assert_eq!(catalog[0].port, 8099);
    }

    #[test]
    fn local_bases_allow_inference_server_probes() {
        for base in [
            "http://127.0.0.1:8099/v1",
            "http://localhost:11434/v1",
            "http://[::1]:8080/v1",
            "http://192.168.1.20:11434/v1",
            "http://inference-box.local:1234/v1",
        ] {
            assert!(base_is_local(base), "{base} should be treated as local");
        }
    }

    #[test]
    fn cloud_bases_skip_inference_server_probes() {
        // These would otherwise cost one /props request per model — hundreds
        // of serial requests against a catalog the size of OpenRouter's.
        for base in [
            "https://openrouter.ai/api/v1",
            "https://api.openai.com/v1",
            "https://example.internal:8080/v1",
        ] {
            assert!(!base_is_local(base), "{base} should not be probed locally");
        }
    }

    #[test]
    fn thinking_hint_from_openrouter_style_object() {
        let entry = serde_json::json!({
            "id": "openai/o4-mini",
            "reasoning": { "supported_efforts": ["high", "medium", "low"] }
        });
        assert!(
            thinking_capabilities_from_model_object(&entry)
                .unwrap()
                .supported
        );
        let params = serde_json::json!({
            "id": "x",
            "supported_parameters": ["temperature", "reasoning_effort"]
        });
        let caps = thinking_capabilities_from_model_object(&params).unwrap();
        assert!(caps.supported);
        assert!(caps.control.is_none());
        let plain = serde_json::json!({ "id": "gpt-4o", "supported_parameters": ["temperature"] });
        assert!(thinking_capabilities_from_model_object(&plain).is_none());
    }

    #[test]
    fn structured_reasoning_metadata_preserves_exact_controls() {
        let entry = serde_json::json!({
            "reasoning": {
                "supported_efforts": ["high", "low"],
                "mandatory": true
            }
        });
        let caps = thinking_capabilities_from_model_object(&entry).unwrap();
        assert_eq!(caps.control.as_deref(), Some("reasoning"));
        assert_eq!(caps.efforts, ["low", "high"]);
        assert!(!caps.can_disable);

        let optional = serde_json::json!({
            "reasoning": {
                "supported_efforts": ["medium"],
                "mandatory": false
            }
        });
        let caps = thinking_capabilities_from_model_object(&optional).unwrap();
        assert!(caps.can_disable);
        assert_eq!(caps.efforts, ["medium"]);

        // Missing mandatory must not unlock Off; null efforts never invent `max`.
        let open = serde_json::json!({ "reasoning": { "supported_efforts": null } });
        let caps = thinking_capabilities_from_model_object(&open).unwrap();
        assert_eq!(caps.control.as_deref(), Some("reasoning"));
        assert_eq!(caps.efforts, ["low", "medium", "high"]);
        assert!(!caps.can_disable);
    }

    #[test]
    fn canonical_effort_emits_only_advertised_wire_shape() {
        let unified = reasoning_model(Some("reasoning"), &["low", "high", "max"], true);
        let mut body = serde_json::json!({ "thinking_effort": "max" });
        apply_thinking_control(&mut body, Some(&unified));
        assert_eq!(
            body,
            serde_json::json!({ "reasoning": { "effort": "max" } })
        );

        let legacy = reasoning_model(Some("reasoning_effort"), &["low", "high"], false);
        let mut body = serde_json::json!({ "thinking_effort": "high" });
        apply_thinking_control(&mut body, Some(&legacy));
        assert_eq!(body, serde_json::json!({ "reasoning_effort": "high" }));

        let disableable = reasoning_model(Some("reasoning_effort"), &["low", "high"], true);
        let mut body = serde_json::json!({ "thinking_effort": "off" });
        apply_thinking_control(&mut body, Some(&disableable));
        assert_eq!(body, serde_json::json!({ "reasoning_effort": "none" }));
    }

    #[test]
    fn unsupported_or_mandatory_off_effort_is_safely_omitted() {
        let mandatory = reasoning_model(Some("reasoning"), &["low", "high"], false);
        for effort in ["off", "max", "invalid"] {
            let mut body = serde_json::json!({ "thinking_effort": effort });
            apply_thinking_control(&mut body, Some(&mandatory));
            assert_eq!(body, serde_json::json!({}), "{effort} must be omitted");
        }
        let mut unknown = serde_json::json!({ "thinking_effort": "high" });
        apply_thinking_control(&mut unknown, None);
        assert_eq!(unknown, serde_json::json!({}));
    }

    #[test]
    fn local_template_control_stays_inside_template_kwargs() {
        let local = reasoning_model(Some("chat_template"), &["low", "medium", "high"], true);
        let mut body = serde_json::json!({ "thinking_effort": "medium" });
        apply_thinking_control(&mut body, Some(&local));
        assert_eq!(
            body,
            serde_json::json!({
                "chat_template_kwargs": {
                    "enable_thinking": true,
                    "reasoning_effort": "medium"
                }
            })
        );
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
    fn connect_failures_are_waiting_not_unreachable() {
        // reqwest 0.12 often omits "connection refused" / "timed out" from Display.
        assert_eq!(
            classify_provider_error(
                "failed to connect: error sending request for url (http://127.0.0.1:8080/v1/models)"
            ),
            ProviderHealthKind::Waiting
        );
        assert_eq!(
            classify_provider_error(
                "timed out: error sending request for url (https://openrouter.ai/api/v1/models)"
            ),
            ProviderHealthKind::Waiting
        );
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

    #[test]
    fn provider_cache_keys_do_not_retain_raw_credentials() {
        let cache = HealthCache::default();
        cache.put(
            ApiStyle::Openai,
            "https://private.example/v1",
            "credential-must-not-be-retained",
            ProviderHealth {
                ok: true,
                kind: ProviderHealthKind::Ready,
                model: None,
                status: Some("ready".into()),
                error: None,
            },
        );
        let debug = format!("{cache:?}");
        assert!(!debug.contains("credential-must-not-be-retained"));
        assert!(!debug.contains("private.example"));
    }
}
