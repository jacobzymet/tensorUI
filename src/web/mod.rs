mod embed;

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, OriginalUri, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use zeroize::Zeroize;

use crate::{
    agent::{self, AgentRequest},
    app::App,
    attachments::{self, ExtractRequest},
    chat, encryption_transition,
    providers::{
        ApiStyle, ProviderHealth, ProviderHealthKind, ProviderPublic, RemoteModelOption,
        apply_thinking_control, enrich_local_catalog, find_provider_for_base,
        normalize_openai_base, probe_provider_endpoint, probe_provider_style,
    },
    store::{self, StorageMode},
    system,
};
pub(crate) use embed::APP_ICON_PNG;
use embed::{
    CHAT_CSS, CHAT_HTML, CHAT_JS, HIGHLIGHT_JS, MARKED_JS, OPTIONAL_FONTS_JS, ORB_JS, PURIFY_JS,
    SETTINGS_HTML, XTERM_CSS, XTERM_FIT_JS, XTERM_JS,
};

const CHAT_REQUEST_LIMIT: usize = 16 * 1024 * 1024;
const CHAT_STORE_LIMIT: usize = 64 * 1024 * 1024;
const CHAT_PREFERENCES_LIMIT: usize = 4 * 1024 * 1024;
const ATTACHMENT_REQUEST_LIMIT: usize = 16 * 1024 * 1024;
const _: () = assert!(ATTACHMENT_REQUEST_LIMIT > 8 * 1024 * 1024 * 4 / 3);
const _: () = assert!(CHAT_PREFERENCES_LIMIT > 1024 * 1024 * 4 / 3);
const _: () = assert!(CHAT_STORE_LIMIT > CHAT_REQUEST_LIMIT);

pub type SharedApp = Arc<Mutex<App>>;

pub const INSTANCE_MARKER: &str = "tensorui";

const SESSION_COOKIE: &str = "tensorui_session";

#[derive(Clone)]
struct ApiSecurity {
    authorities: Vec<String>,
    origins: Vec<String>,
    token: String,
}

impl ApiSecurity {
    fn new(addr: std::net::SocketAddr) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("session token: {error}"))?;
        use base64::Engine as _;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let authorities = vec![
            addr.to_string(),
            format!("{}:{}", crate::config::PUBLIC_UI_HOST, addr.port()),
        ];
        Ok(Self {
            origins: authorities
                .iter()
                .map(|authority| format!("http://{authority}"))
                .collect(),
            authorities,
            token,
        })
    }
}

fn secret_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn request_token<B>(request: &Request<B>) -> Option<&str> {
    if let Some(value) = request
        .headers()
        .get("x-tensorui-token")
        .and_then(|value| value.to_str().ok())
    {
        return Some(value);
    }
    request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix(&format!("{SESSION_COOKIE}=")))
}

async fn secure_local_request(
    State(security): State<ApiSecurity>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let host_ok = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            security
                .authorities
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
        });
    if !host_ok {
        return (StatusCode::MISDIRECTED_REQUEST, "invalid Host header").into_response();
    }

    let is_api = request.uri().path().starts_with("/api/");
    if is_api {
        let origin_ok = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|origin| security.origins.iter().any(|allowed| origin == allowed));
        let token_ok =
            request_token(&request).is_some_and(|token| secret_eq(token, &security.token));
        if !origin_ok || !token_ok {
            return (StatusCode::UNAUTHORIZED, "unauthorized local API request").into_response();
        }
    }

    let issue_cookie = !is_api && request.method() == axum::http::Method::GET;
    let mut response = next.run(request).await;
    if issue_cookie {
        let value = format!(
            "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/",
            security.token
        );
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

async fn require_unlocked_api(
    State(app): State<SharedApp>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let locked = app
        .lock()
        .map(|guard| guard.encryption_enabled() && !guard.encryption_unlocked())
        .unwrap_or(true);
    if !locked {
        return next.run(request).await;
    }

    if locked_api_request_allowed(request.method(), request.uri().path()) {
        return next.run(request).await;
    }

    (
        StatusCode::LOCKED,
        Json(serde_json::json!({
            "error": "Encrypted local data is locked. Unlock it before using this API.",
            "code": "encrypted_locked"
        })),
    )
        .into_response()
}

fn locked_api_request_allowed(method: &axum::http::Method, path: &str) -> bool {
    (path == "/api/state" && *method == axum::http::Method::GET)
        || (path == "/api/data" && *method == axum::http::Method::GET)
        || (path == "/api/data/encryption/unlock" && *method == axum::http::Method::POST)
        || (path == "/api/focus" && *method == axum::http::Method::POST)
        || (path == "/api/open-url" && *method == axum::http::Method::POST)
}

static PROVIDER_CACHE_WARM_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static PROVIDER_CACHE_WARM_ALLOWED: AtomicBool = AtomicBool::new(true);
const WARM_CONCURRENCY: usize = 4;

fn schedule_provider_cache_warm(app: SharedApp) {
    if !PROVIDER_CACHE_WARM_ALLOWED.load(Ordering::SeqCst) {
        return;
    }
    let needs_warm = app
        .lock()
        .map(|guard| guard.remote_caches_need_warm())
        .unwrap_or(false);
    if !needs_warm {
        return;
    }
    if PROVIDER_CACHE_WARM_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    tokio::task::spawn_blocking(move || {
        struct ClearInFlight;
        impl Drop for ClearInFlight {
            fn drop(&mut self) {
                PROVIDER_CACHE_WARM_IN_FLIGHT.store(false, Ordering::SeqCst);
            }
        }
        let _clear = ClearInFlight;

        let targets = match app.lock() {
            Ok(guard) => guard.provider_warm_targets(),
            Err(_) => return,
        };
        warm_provider_targets(&app, targets);
    });
}

fn warm_provider_targets(
    app: &SharedApp,
    targets: Vec<(ApiStyle, String, zeroize::Zeroizing<String>, bool)>,
) {
    if targets.is_empty() {
        return;
    }
    let workers = WARM_CONCURRENCY.min(targets.len()).max(1);
    let queue = Mutex::new(targets.into_iter().collect::<VecDeque<_>>());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if !PROVIDER_CACHE_WARM_ALLOWED.load(Ordering::SeqCst) {
                        return;
                    }
                    let Some((style, base, token, insecure)) =
                        queue.lock().ok().and_then(|mut queue| queue.pop_front())
                    else {
                        return;
                    };
                    let (health, catalog) =
                        crate::http::with_insecure_provider_tls(insecure, || {
                            probe_provider_endpoint(&base, &token, style)
                        });
                    if let Ok(guard) = app.lock() {
                        guard.store_remote_health(style, &base, &token, health);
                        guard.store_remote_catalog(style, &base, &token, catalog.clone());
                    }
                    let catalog = crate::http::with_insecure_provider_tls(insecure, || {
                        enrich_local_catalog(&base, &token, style, catalog)
                    });
                    if let Ok(guard) = app.lock() {
                        guard.store_remote_catalog(style, &base, &token, catalog);
                    }
                }
            });
        }
    });
}

async fn pause_provider_cache_warm() {
    PROVIDER_CACHE_WARM_ALLOWED.store(false, Ordering::SeqCst);
    while PROVIDER_CACHE_WARM_IN_FLIGHT.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

pub async fn serve(app: SharedApp, listener: TcpListener) -> anyhow::Result<()> {
    // Probe providers before the first browser poll so Chat doesn't flash unreachable.
    schedule_provider_cache_warm(Arc::clone(&app));

    let security = ApiSecurity::new(listener.local_addr()?)?;
    let api = Router::new()
        .route(
            "/api/chat/completions",
            post(chat_completions).layer(DefaultBodyLimit::max(CHAT_REQUEST_LIMIT)),
        )
        .route("/api/chat/live/{conversation_id}", get(chat_live))
        .route("/api/chat/cancel", post(chat_cancel))
        .route("/api/chat/clarify", post(chat_clarify))
        .route("/api/chat/steer", post(chat_steer))
        .route("/api/chat/approve", post(chat_approve))
        .route("/api/terminal/open", post(terminal_open))
        .route("/api/terminal/ws/{id}", get(terminal_ws))
        .route("/api/terminal/close", post(terminal_close))
        .route("/api/workspace/pick", post(pick_workspace_folder))
        .route("/api/chat/title", post(chat_title))
        .route("/api/state", get(state))
        .route("/api/ui/theme", post(set_ui_theme))
        .route("/api/ui/appearance", post(set_ui_appearance))
        .route("/api/ui/appearance/reset", post(reset_ui_appearance))
        .route("/api/providers", get(list_providers).post(create_provider))
        .route("/api/providers/test", post(test_provider))
        .route(
            "/api/providers/{id}",
            axum::routing::patch(update_provider).delete(delete_provider),
        )
        .route("/api/providers/{id}/activate", post(activate_provider))
        .route("/api/local-llms", get(local_llms_status))
        .route("/api/local-llms/cache", get(local_llms_cache))
        .route("/api/local-llms/start", post(local_llms_start))
        .route("/api/local-llms/stop", post(local_llms_stop))
        .route("/api/focus", post(focus))
        .route("/api/open-url", post(open_url))
        .route("/api/data", get(data_info).post(set_storage_mode))
        .route("/api/data/open", post(open_data_dir))
        .route("/api/data/encryption/enable", post(enable_encryption))
        .route("/api/data/encryption/disable", post(disable_encryption))
        .route("/api/data/encryption/unlock", post(unlock_encryption))
        .route("/api/data/encryption/lock", post(lock_encryption))
        .route(
            "/api/data/store",
            get(get_chat_store)
                .put(put_chat_store)
                .layer(DefaultBodyLimit::max(CHAT_STORE_LIMIT)),
        )
        .route(
            "/api/data/preferences",
            get(get_chat_preferences)
                .put(put_chat_preferences)
                .layer(DefaultBodyLimit::max(CHAT_PREFERENCES_LIMIT)),
        )
        .route("/api/skills", get(list_skills).post(create_skill))
        .route("/api/skills/import", post(import_skill))
        .route(
            "/api/skills/{id}",
            axum::routing::patch(update_skill).delete(delete_skill),
        )
        .route(
            "/api/attachments/extract",
            post(extract_attachment).layer(DefaultBodyLimit::max(ATTACHMENT_REQUEST_LIMIT)),
        )
        .route("/api/updates/check", get(check_updates))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&app),
            require_unlocked_api,
        ));

    let router = Router::new()
        .route("/", get(chat_page))
        .route("/notifications", get(chat_page))
        .route("/projects", get(chat_page))
        .route("/ghost", get(chat_page))
        .route("/loops", get(chat_page))
        .route("/loops/c/{id}", get(chat_page))
        .route("/bots", get(chat_page))
        .route("/bots/c/{id}", get(chat_page))
        .route("/c/{id}", get(chat_page))
        .route("/p/{id}", get(chat_page))
        .route("/p/{id}/ghost", get(chat_page))
        .route("/settings", get(settings_page))
        .route("/admin", get(|| async { Redirect::permanent("/settings") }))
        .route("/chat", get(|| async { Redirect::permanent("/") }))
        .route("/chat.css", get(chat_stylesheet))
        .route("/chat.js", get(chat_script))
        .route("/orb.js", get(orb_script))
        .route("/prompts.js", get(prompts_script))
        .route("/highlight.min.js", get(highlight_script))
        .route("/marked.min.js", get(marked_script))
        .route("/purify.min.js", get(purify_script))
        .route("/xterm.css", get(xterm_stylesheet))
        .route("/xterm.min.js", get(xterm_script))
        .route("/xterm-addon-fit.min.js", get(xterm_fit_script))
        .route("/optional-fonts.js", get(optional_fonts_script))
        .route("/browser-favicon.png", get(app_icon_png))
        .route("/favicon.ico", get(app_icon_png))
        // Back-compat alias for older cached HTML.
        .route("/ti.png", get(app_icon_png))
        .merge(api)
        .layer(middleware::from_fn_with_state(
            security,
            secure_local_request,
        ))
        .with_state(app);

    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let desktop_quit = async {
        loop {
            if crate::desktop::quit_requested() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
        _ = desktop_quit => {},
    }
}

async fn chat_page(State(app): State<SharedApp>, OriginalUri(uri): OriginalUri) -> Response {
    let locked = app
        .lock()
        .map(|guard| guard.encryption_enabled() && !guard.encryption_unlocked())
        .unwrap_or(true);
    if locked && uri.path() != "/" {
        Redirect::temporary("/").into_response()
    } else {
        Html(CHAT_HTML).into_response()
    }
}

async fn settings_page(State(app): State<SharedApp>, OriginalUri(uri): OriginalUri) -> Response {
    let locked = app
        .lock()
        .map(|guard| guard.encryption_enabled() && !guard.encryption_unlocked())
        .unwrap_or(true);
    if locked {
        Redirect::temporary("/").into_response()
    } else if !uri
        .query()
        .is_some_and(|query| query.split('&').any(|part| part == "embedded=1"))
    {
        Redirect::temporary("/?settings=providers").into_response()
    } else {
        Html(SETTINGS_HTML).into_response()
    }
}

async fn chat_stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        CHAT_CSS,
    )
}

async fn chat_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        CHAT_JS,
    )
}

async fn orb_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        ORB_JS,
    )
}

async fn prompts_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        crate::prompts::frontend_js(),
    )
}

async fn highlight_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        HIGHLIGHT_JS,
    )
}

async fn marked_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        MARKED_JS,
    )
}

async fn purify_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        PURIFY_JS,
    )
}

async fn optional_fonts_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        OPTIONAL_FONTS_JS,
    )
}

async fn xterm_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        XTERM_JS,
    )
}

async fn xterm_fit_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        XTERM_FIT_JS,
    )
}

async fn xterm_stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        XTERM_CSS,
    )
}

async fn app_icon_png() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], APP_ICON_PNG)
}

#[derive(Debug, Deserialize)]
struct ThemeRequest {
    theme: String,
}

#[derive(Debug, Deserialize)]
struct AppearanceRequest {
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    font_body: Option<String>,
    #[serde(default)]
    font_display: Option<String>,
    #[serde(default)]
    font_mono: Option<String>,
    #[serde(default)]
    font_scale: Option<String>,
}

async fn set_ui_theme(
    State(app): State<SharedApp>,
    Json(body): Json<ThemeRequest>,
) -> Result<Json<AppState>, ApiError> {
    let theme = crate::config::UiTheme::parse(&body.theme)
        .ok_or_else(|| ApiError::bad_request("theme must be \"dark\", \"light\", or \"system\""))?;
    with_app(app, |app| app.set_ui_theme(theme))
}

async fn set_ui_appearance(
    State(app): State<SharedApp>,
    Json(body): Json<AppearanceRequest>,
) -> Result<Json<AppState>, ApiError> {
    let theme = match body.theme.as_deref() {
        None => None,
        Some(value) => Some(crate::config::UiTheme::parse(value).ok_or_else(|| {
            ApiError::bad_request("theme must be \"dark\", \"light\", or \"system\"")
        })?),
    };
    let font_scale = match body.font_scale.as_deref() {
        None => None,
        Some(value) => Some(crate::config::UiFontScale::parse(value).ok_or_else(|| {
            ApiError::bad_request("font_scale must be compact, default, or large")
        })?),
    };
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    app.set_ui_appearance(
        theme,
        body.font_body,
        body.font_display,
        body.font_mono,
        font_scale,
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(AppState::from_app(&app)))
}

async fn reset_ui_appearance(State(app): State<SharedApp>) -> Result<Json<AppState>, ApiError> {
    with_app(app, |app| app.reset_ui_appearance())
}

#[derive(Debug, Deserialize)]
struct ChatTitleBody {
    message: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    remote_base: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatTitleResponse {
    title: String,
}

async fn chat_title(
    State(app): State<SharedApp>,
    Json(body): Json<ChatTitleBody>,
) -> Result<Json<ChatTitleResponse>, ApiError> {
    let message = body.message.trim();
    if message.is_empty() {
        return Err(ApiError::bad_request("message must not be empty"));
    }
    let model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    let (api_base, token, api_style, allow_insecure_tls) = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let providers = &app.config.providers;
        if let Some(requested) = body.remote_base.as_deref() {
            let Some(linked) = find_provider_for_base(&providers.items, requested) else {
                return Err(ApiError::bad_request(
                    "No provider is configured for that model.",
                ));
            };
            let api_base = normalize_openai_base(requested)
                .ok_or_else(|| ApiError::bad_request("Invalid model API base."))?;
            (
                api_base,
                linked.token.clone(),
                linked.api_style,
                linked.allow_insecure_tls,
            )
        } else {
            let Some(active) = providers.active() else {
                return Err(ApiError::bad_request(
                    "No provider configured. Add one in Settings > Providers.",
                ));
            };
            let api_base = normalize_openai_base(&active.base)
                .ok_or_else(|| ApiError::bad_request("Active provider has an invalid base URL."))?;
            (
                api_base,
                active.token.clone(),
                active.api_style,
                active.allow_insecure_tls,
            )
        }
    };

    let title = chat::generate_chat_title(
        &api_base,
        &token,
        api_style,
        model.as_deref(),
        message,
        allow_insecure_tls,
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(Json(ChatTitleResponse { title }))
}

async fn chat_completions(
    State(app): State<SharedApp>,
    Json(mut body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    let remote_base_override = body
        .as_object_mut()
        .and_then(|obj| obj.remove("remote_base"))
        .and_then(|v| v.as_str().map(str::to_string));
    let remote_model = body
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let (remote, user_skills, thinking_model) = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let user_skills = app.enabled_user_skills();
        let providers = &app.config.providers;
        let remote = if let Some(requested) = remote_base_override.as_deref() {
            let Some(linked) = find_provider_for_base(&providers.items, requested) else {
                return Err(ApiError::bad_request(
                    "No provider is configured for that model.",
                ));
            };
            let api_base = normalize_openai_base(requested)
                .ok_or_else(|| ApiError::bad_request("Invalid model API base."))?;
            Some((
                api_base,
                linked.token.clone(),
                linked.api_style,
                linked.allow_insecure_tls,
            ))
        } else {
            let Some(active) = providers.active() else {
                return Err(ApiError::bad_request(
                    "No provider configured. Add one in Settings > Providers.",
                ));
            };
            let api_base = normalize_openai_base(&active.base)
                .ok_or_else(|| ApiError::bad_request("Active provider has an invalid base URL."))?;
            Some((
                api_base,
                active.token.clone(),
                active.api_style,
                active.allow_insecure_tls,
            ))
        };
        let thinking_model = remote.as_ref().and_then(|(base, _, _, _)| {
            remote_model.as_deref().and_then(|model| {
                app.remote_model_catalog_cached()
                    .into_iter()
                    .find(|option| {
                        option.model == model
                            && normalize_openai_base(&option.base).as_ref() == Some(base)
                    })
            })
        });
        (remote, user_skills, thinking_model)
    };

    let Some((api_base, token, api_style, allow_insecure_tls)) = remote else {
        return Err(ApiError::bad_request(
            "No provider configured. Add one in Settings > Providers.",
        ));
    };

    if let Some(model) = remote_model
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("model".to_string(), serde_json::Value::String(model));
    }
    apply_thinking_control(&mut body, thinking_model.as_ref());
    let conversation_id = body
        .as_object_mut()
        .and_then(|obj| obj.remove("conversation_id"))
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|id| !id.is_empty());
    let conversation_id = conversation_id.map(validate_live_id).transpose()?;
    let key = (!token.trim().is_empty()).then_some(token.as_str());
    let wants_agent = body.get("agent").and_then(|v| v.as_bool()).unwrap_or(false)
        || body
            .get("deep_research")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    let deep_research = body
        .get("deep_research")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let deep_research_output = body
        .get("deep_research_output")
        .and_then(|v| v.as_str())
        .filter(|value| *value == "brief")
        .unwrap_or("long")
        .to_string();
    let turn_model = body
        .get("model")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(id) = conversation_id.as_deref()
        && let Some(existing) = crate::live::hub().info(id)
        && !existing.finished
    {
        return Err(ApiError::conflict(
            "A response is already in progress for that conversation.",
        ));
    }
    let stream = match serde_json::from_value::<AgentRequest>(body.clone()) {
        Ok(mut request) if agent::should_run_agent(&request, &user_skills) => {
            if request.messages.is_empty() {
                return Err(ApiError::bad_request("messages must not be empty"));
            }
            if let Some(id) = conversation_id.as_deref() {
                request.skills.session_id = id.to_string();
            }
            request.model_context_window_tokens = thinking_model
                .as_ref()
                .and_then(|model| model.context_length)
                .filter(|length| *length > 0)
                .and_then(|length| usize::try_from(length).ok());
            agent::stream_agent(
                &api_base,
                key,
                api_style,
                allow_insecure_tls,
                request,
                user_skills,
            )
        }
        Ok(_) => {
            if let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
                agent::inject_skill_catalog_into_messages(messages, &user_skills);
            }
            chat::stream_remote_completion(&api_base, &token, api_style, allow_insecure_tls, body)
        }
        Err(error) if wants_agent => {
            return Err(ApiError::bad_request(format!(
                "Invalid agent request: {error}"
            )));
        }
        Err(_) => {
            if let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
                agent::inject_skill_catalog_into_messages(messages, &user_skills);
            }
            chat::stream_remote_completion(&api_base, &token, api_style, allow_insecure_tls, body)
        }
    };

    let stream = if let Some(conversation_id) = conversation_id {
        let info = crate::live::LiveTurnInfo {
            conversation_id,
            turn_id: crate::live::new_turn_id(),
            agent: wants_agent,
            deep_research,
            deep_research_output,
            model: turn_model,
            finished: false,
        };
        crate::live::hub().start(info, stream).map_err(|_| {
            ApiError::conflict("A response is already in progress for that conversation.")
        })?
    } else {
        stream
    };

    sse_response(stream)
}

fn sse_response(stream: crate::agent::chat::ChatStream) -> Result<Response, ApiError> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

async fn chat_live(Path(conversation_id): Path<String>) -> Result<Response, ApiError> {
    let id = validate_live_id(conversation_id)?;
    let Some(stream) = crate::live::hub().subscribe(&id) else {
        return Err(ApiError::not_found("No live turn for that conversation"));
    };
    sse_response(stream)
}

#[derive(Debug, Deserialize)]
struct CancelTurnBody {
    conversation_id: String,
    turn_id: Option<String>,
}

async fn chat_cancel(
    Json(body): Json<CancelTurnBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = validate_live_id(body.conversation_id)?;
    let turn_id = body.turn_id.map(validate_live_id).transpose()?;
    let cancelled = crate::live::hub().cancel(&id, turn_id.as_deref());
    Ok(Json(
        serde_json::json!({ "ok": true, "cancelled": cancelled }),
    ))
}

fn validate_live_id(raw: impl AsRef<str>) -> Result<String, ApiError> {
    let id = raw.as_ref().trim();
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ApiError::bad_request("Invalid live turn identifier"));
    }
    Ok(id.to_string())
}

#[derive(Debug, Deserialize)]
struct ClarifyAnswersBody {
    id: String,
    answers: serde_json::Value,
}

async fn chat_clarify(
    Json(body): Json<ClarifyAnswersBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    agent::submit_clarify_answers(&body.id, body.answers).map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct SteerBody {
    id: String,
    text: String,
    #[serde(default)]
    client_id: Option<String>,
}

async fn chat_steer(Json(body): Json<SteerBody>) -> Result<Json<serde_json::Value>, ApiError> {
    agent::submit_steer(&body.id, &body.text, body.client_id.as_deref())
        .map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct ApproveBody {
    id: String,
    #[serde(default)]
    allow: bool,
}

async fn chat_approve(Json(body): Json<ApproveBody>) -> Result<Json<serde_json::Value>, ApiError> {
    agent::submit_tool_approval(&body.id, body.allow).map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct TerminalOpenBody {
    workspace: String,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

async fn terminal_open(
    Json(body): Json<TerminalOpenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let opened = agent::terminal::open_session(
        &body.workspace,
        body.cols.unwrap_or(0),
        body.rows.unwrap_or(0),
    )
    .await
    .map_err(ApiError::bad_request)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "id": opened.id,
        "cwd": opened.cwd,
        "title": opened.title,
    })))
}

#[derive(Debug, Deserialize)]
struct TerminalResizeCtrl {
    #[serde(rename = "type")]
    kind: String,
    cols: Option<u16>,
    rows: Option<u16>,
}

async fn terminal_ws(ws: WebSocketUpgrade, Path(id): Path<String>) -> Result<Response, ApiError> {
    let id = validate_live_id(id)?;
    let io = agent::terminal::attach_session(&id)
        .await
        .ok_or_else(|| ApiError::not_found("Terminal session is not open"))?;
    Ok(ws.on_upgrade(move |socket| terminal_socket(socket, io)))
}

async fn terminal_socket(socket: WebSocket, io: agent::terminal::SessionIo) {
    let (mut sink, mut stream) = socket.split();
    let agent::terminal::SessionIo {
        to_pty,
        mut stdout,
        replay,
    } = io;
    if !replay.is_empty() && sink.send(Message::Binary(replay.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = stdout.recv() => {
                match msg {
                    Ok(chunk) => {
                        if sink.send(Message::Binary(chunk.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if data.is_empty() {
                            continue;
                        }
                        if to_pty.send(agent::terminal::ToPty::Data(data.to_vec())).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if apply_terminal_ctrl(&to_pty, text.as_str()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

fn apply_terminal_ctrl(
    to_pty: &std::sync::mpsc::Sender<agent::terminal::ToPty>,
    text: &str,
) -> Result<(), ()> {
    let Ok(ctrl) = serde_json::from_str::<TerminalResizeCtrl>(text) else {
        return if to_pty
            .send(agent::terminal::ToPty::Data(text.as_bytes().to_vec()))
            .is_err()
        {
            Err(())
        } else {
            Ok(())
        };
    };
    if ctrl.kind != "resize" {
        return Ok(());
    }
    let (cols, rows) =
        agent::terminal::clamp_pty_size(ctrl.cols.unwrap_or(0), ctrl.rows.unwrap_or(0));
    to_pty
        .send(agent::terminal::ToPty::Resize { cols, rows })
        .map_err(|_| ())
}

#[derive(Debug, Deserialize)]
struct TerminalCloseBody {
    id: String,
}

async fn terminal_close(
    Json(body): Json<TerminalCloseBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    agent::terminal::close_session(&body.id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn pick_workspace_folder() -> Result<Json<serde_json::Value>, ApiError> {
    let picked = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose workspace folder")
            .pick_folder()
    })
    .await
    .map_err(|error| ApiError::bad_request(format!("folder picker: {error}")))?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "path": picked.map(|path| path.display().to_string()),
    })))
}

#[derive(Debug, Deserialize)]
struct CreateProviderBody {
    #[serde(default)]
    name: String,
    base: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    api_style: Option<ApiStyle>,
    #[serde(default)]
    allow_insecure_tls: bool,
    #[serde(default = "default_true")]
    activate: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct UpdateProviderBody {
    name: Option<String>,
    base: Option<String>,
    token: Option<String>,
    api_style: Option<ApiStyle>,
    allow_insecure_tls: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TestProviderBody {
    base: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    api_style: Option<ApiStyle>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    allow_insecure_tls: bool,
}

#[derive(Debug, Serialize)]
struct TestProviderResponse {
    ok: bool,
    base: String,
    api_style: &'static str,
    detected: bool,
    /// How many models this endpoint offers — the useful fact to report back,
    /// rather than an arbitrary sample model name.
    models: usize,
    health: ProviderHealth,
}

#[derive(Debug, Serialize)]
struct ProvidersResponse {
    providers: Vec<ProviderPublic>,
    state: AppState,
}

async fn list_providers(State(app): State<SharedApp>) -> Result<Json<ProvidersResponse>, ApiError> {
    let response = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        ProvidersResponse {
            providers: app.public_providers(),
            state: AppState::from_app(&app),
        }
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(response))
}

async fn test_provider(
    State(app): State<SharedApp>,
    Json(body): Json<TestProviderBody>,
) -> Result<Json<TestProviderResponse>, ApiError> {
    let base = normalize_openai_base(&body.base)
        .ok_or_else(|| ApiError::bad_request("Enter a valid base URL ending in /v1."))?;

    let token = {
        let trimmed = body.token.trim().to_string();
        if !trimmed.is_empty() {
            trimmed
        } else if let Some(id) = body
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            let app = app.lock().map_err(|_| ApiError::lock())?;
            app.config
                .providers
                .items
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.token.clone())
                .ok_or_else(|| ApiError::bad_request("Provider not found."))?
        } else {
            String::new()
        }
    };
    let forced_style = body.api_style;
    let allow_insecure_tls = body.allow_insecure_tls;

    let probe_base = base.clone();
    let probe_token = token.clone();
    let probe = tokio::task::spawn_blocking(move || {
        crate::http::with_insecure_provider_tls(allow_insecure_tls, || {
            let probe = probe_provider_style(&probe_base, &probe_token, forced_style);
            let catalog =
                if probe.health.ok || matches!(probe.health.kind, ProviderHealthKind::Empty) {
                    enrich_local_catalog(
                        &probe_base,
                        &probe_token,
                        probe.api_style,
                        probe.catalog.clone(),
                    )
                } else {
                    probe.catalog.clone()
                };
            (probe, catalog)
        })
    })
    .await
    .map_err(|error| ApiError::bad_request(format!("connection test failed: {error}")))?;
    let (probe, catalog) = probe;

    let model_count = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        app.store_remote_health(probe.api_style, &base, &token, probe.health.clone());
        let model_count = catalog.len();
        app.store_remote_catalog(probe.api_style, &base, &token, catalog);
        model_count
    };

    Ok(Json(TestProviderResponse {
        ok: probe.health.ok || matches!(probe.health.kind, ProviderHealthKind::Empty),
        base,
        api_style: probe.api_style.as_str(),
        detected: probe.detected,
        models: model_count,
        health: probe.health,
    }))
}

async fn create_provider(
    State(app): State<SharedApp>,
    Json(body): Json<CreateProviderBody>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    let base = normalize_openai_base(&body.base)
        .ok_or_else(|| ApiError::bad_request("Enter a valid base URL ending in /v1."))?;
    let token = body.token.clone();
    let forced_style = body.api_style;
    let api_style = if let Some(style) = forced_style {
        style
    } else {
        let probe_base = base.clone();
        let probe_token = token.clone();
        let insecure = body.allow_insecure_tls;
        tokio::task::spawn_blocking(move || {
            crate::http::with_insecure_provider_tls(insecure, || {
                probe_provider_style(&probe_base, &probe_token, None).api_style
            })
        })
        .await
        .map_err(|error| ApiError::bad_request(format!("style detection failed: {error}")))?
    };

    let response = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.create_provider(
            &body.name,
            &base,
            &token,
            api_style,
            body.allow_insecure_tls,
            body.activate,
        )
        .map_err(ApiError::bad_request)?;
        ProvidersResponse {
            providers: app.public_providers(),
            state: AppState::from_app(&app),
        }
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(response))
}

async fn update_provider(
    State(app): State<SharedApp>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProviderBody>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    let response = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.update_provider(
            &id,
            body.name.as_deref(),
            body.base.as_deref(),
            body.token.as_deref(),
            body.api_style,
            body.allow_insecure_tls,
        )
        .map_err(ApiError::bad_request)?;
        ProvidersResponse {
            providers: app.public_providers(),
            state: AppState::from_app(&app),
        }
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(response))
}

async fn delete_provider(
    State(app): State<SharedApp>,
    Path(id): Path<String>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    let response = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.delete_provider(&id).map_err(ApiError::bad_request)?;
        ProvidersResponse {
            providers: app.public_providers(),
            state: AppState::from_app(&app),
        }
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(response))
}

async fn activate_provider(
    State(app): State<SharedApp>,
    Path(id): Path<String>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    let response = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.activate_provider(&id).map_err(ApiError::bad_request)?;
        ProvidersResponse {
            providers: app.public_providers(),
            state: AppState::from_app(&app),
        }
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(response))
}

#[derive(Debug, Serialize)]
struct LocalLlmsStatusResponse {
    install: crate::local_llm::LlamaServerInstall,
    running: Option<crate::local_llm::RunningLocalLlm>,
    default_threads: u32,
    default_port: u16,
    cache_dir: Option<String>,
    state: AppState,
}

#[derive(Debug, Serialize)]
struct LocalLlmsCacheResponse {
    cache_dir: Option<String>,
    models: Vec<crate::local_llm::CachedModel>,
}

#[derive(Debug, Deserialize)]
struct StartLocalLlmBody {
    #[serde(default)]
    hf: Option<String>,
    #[serde(default)]
    model_path: Option<String>,
    #[serde(default = "default_true")]
    mmap: bool,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    threads: Option<u32>,
}

async fn local_llms_status(
    State(app): State<SharedApp>,
) -> Result<Json<LocalLlmsStatusResponse>, ApiError> {
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    let install = crate::local_llm::detect_llama_server();
    let running = app.local_llm.status();
    let cache_dir = crate::local_llm::llama_cache_dir()
        .ok()
        .map(|p| p.display().to_string());
    let default_threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
        .saturating_sub(2)
        .max(1);
    Ok(Json(LocalLlmsStatusResponse {
        install,
        running,
        default_threads,
        default_port: 8080,
        cache_dir,
        state: AppState::from_app(&app),
    }))
}

async fn local_llms_cache(
    State(_app): State<SharedApp>,
) -> Result<Json<LocalLlmsCacheResponse>, ApiError> {
    let models = crate::local_llm::list_cached_models().map_err(ApiError::bad_request)?;
    let cache_dir = crate::local_llm::llama_cache_dir()
        .ok()
        .map(|p| p.display().to_string());
    Ok(Json(LocalLlmsCacheResponse { cache_dir, models }))
}

async fn local_llms_start(
    State(app): State<SharedApp>,
    Json(body): Json<StartLocalLlmBody>,
) -> Result<Json<LocalLlmsStatusResponse>, ApiError> {
    {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        let meta = crate::local_llm::start_local_llm(
            &mut app.local_llm,
            crate::local_llm::StartLocalLlm {
                hf: body.hf,
                model_path: body.model_path,
                mmap: body.mmap,
                port: body.port,
                threads: body.threads,
                host: Some("127.0.0.1".into()),
            },
        )
        .map_err(ApiError::bad_request)?;
        if let Err(error) = app.ensure_local_llama_provider(&meta.base_url) {
            let _ = app.local_llm.stop();
            return Err(ApiError::bad_request(error));
        }
    }
    schedule_provider_cache_warm(Arc::clone(&app));
    local_llms_status(State(app)).await
}

async fn local_llms_stop(
    State(app): State<SharedApp>,
) -> Result<Json<LocalLlmsStatusResponse>, ApiError> {
    {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.local_llm.stop().map_err(ApiError::bad_request)?;
    }
    local_llms_status(State(app)).await
}

#[derive(Debug, Serialize)]
struct DataInfo {
    storage: &'static str,
    browser_storage: bool,
    encryption_enabled: bool,
    encryption_unlocked: bool,
    encryption_transition_pending: bool,
    data_dir: String,
    config_path: String,
    chats_path: String,
    preferences_path: String,
    skills_dir: String,
    os: &'static str,
    open_label: &'static str,
}

#[derive(Debug, Deserialize)]
struct SetStorageBody {
    browser_storage: bool,
}

fn host_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn open_folder_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Open in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Open in Finder"
    } else {
        "Open in Files"
    }
}

fn data_info_from_app(app: &App) -> DataInfo {
    let root = app.data_dir();
    let _ = store::ensure_data_dir(&root);
    let locked = app.encryption_enabled() && !app.encryption_unlocked();
    let visible_path = |path: &std::path::Path| {
        if locked {
            String::new()
        } else {
            path.display().to_string()
        }
    };
    DataInfo {
        storage: app.storage_mode().as_str(),
        browser_storage: false,
        encryption_enabled: app.encryption_enabled(),
        encryption_unlocked: app.encryption_unlocked(),
        encryption_transition_pending: encryption_transition::exists(&root),
        data_dir: visible_path(&root),
        config_path: visible_path(&app.config_path),
        chats_path: visible_path(&store::chats_path(&root)),
        preferences_path: visible_path(&store::preferences_path(&root)),
        skills_dir: visible_path(&root.join("chat-skills")),
        os: host_os(),
        open_label: open_folder_label(),
    }
}

async fn data_info(State(app): State<SharedApp>) -> Result<Json<DataInfo>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    Ok(Json(data_info_from_app(&app)))
}

async fn set_storage_mode(
    State(app): State<SharedApp>,
    Json(body): Json<SetStorageBody>,
) -> Result<Json<DataInfo>, ApiError> {
    if body.browser_storage {
        return Err(ApiError::bad_request(
            "Browser localStorage mode has been removed; local data is stored on disk.",
        ));
    }
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    app.set_storage_mode(StorageMode::Disk)
        .map_err(ApiError::bad_request)?;
    Ok(Json(data_info_from_app(&app)))
}

async fn open_data_dir(State(app): State<SharedApp>) -> Result<Json<serde_json::Value>, ApiError> {
    let path = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let root = app.data_dir();
        store::ensure_data_dir(&root)
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
        root
    };
    system::open_in_file_manager(&path)
        .map_err(|error| ApiError::bad_request(format!("could not open folder: {error}")))?;
    Ok(Json(
        serde_json::json!({ "ok": true, "path": path.display().to_string() }),
    ))
}

#[derive(Debug, Deserialize)]
struct PassphraseBody {
    passphrase: String,
    #[serde(default)]
    passphrase_confirm: Option<String>,
}

impl Drop for PassphraseBody {
    fn drop(&mut self) {
        self.passphrase.zeroize();
        if let Some(confirm) = self.passphrase_confirm.as_mut() {
            confirm.zeroize();
        }
    }
}

fn store_api_error(message: String) -> ApiError {
    if message.starts_with("Local data is encrypted") {
        ApiError::forbidden_code(message, "encrypted_locked")
    } else {
        ApiError::bad_request(message)
    }
}

async fn enable_encryption(
    State(app): State<SharedApp>,
    Json(body): Json<PassphraseBody>,
) -> Result<Json<DataInfo>, ApiError> {
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    // Do not trim passphrases — leading/trailing spaces are significant.
    app.enable_disk_encryption(
        &body.passphrase,
        body.passphrase_confirm.as_deref().unwrap_or(""),
    )
    .map_err(ApiError::bad_request)?;
    Ok(Json(data_info_from_app(&app)))
}

async fn disable_encryption(
    State(app): State<SharedApp>,
    Json(body): Json<PassphraseBody>,
) -> Result<Json<DataInfo>, ApiError> {
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    app.disable_disk_encryption(&body.passphrase)
        .map_err(ApiError::bad_request)?;
    Ok(Json(data_info_from_app(&app)))
}

async fn unlock_encryption(
    State(app): State<SharedApp>,
    Json(body): Json<PassphraseBody>,
) -> Result<Json<DataInfo>, ApiError> {
    let info = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.unlock_disk_encryption(&body.passphrase)
            .map_err(ApiError::bad_request)?;
        data_info_from_app(&app)
    };
    PROVIDER_CACHE_WARM_ALLOWED.store(true, Ordering::SeqCst);
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(info))
}

async fn lock_encryption(State(app): State<SharedApp>) -> Result<Json<DataInfo>, ApiError> {
    pause_provider_cache_warm().await;
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    app.lock_disk_encryption();
    Ok(Json(data_info_from_app(&app)))
}

async fn get_chat_store(State(app): State<SharedApp>) -> Result<Json<serde_json::Value>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let value = app.load_chat_store().map_err(store_api_error)?;
    Ok(Json(value))
}

async fn put_chat_store(
    State(app): State<SharedApp>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    app.save_chat_store(body).map_err(store_api_error)?;
    let value = app.load_chat_store().map_err(store_api_error)?;
    Ok(Json(value))
}

async fn get_chat_preferences(
    State(app): State<SharedApp>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let value = app.load_chat_preferences().map_err(store_api_error)?;
    Ok(Json(value))
}

async fn put_chat_preferences(
    State(app): State<SharedApp>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    app.save_chat_preferences(body).map_err(store_api_error)?;
    let value = app.load_chat_preferences().map_err(store_api_error)?;
    Ok(Json(value))
}

#[derive(Debug, Serialize)]
struct SkillsState {
    skills: Vec<crate::skills::UserSkillPublic>,
}

#[derive(Debug, Deserialize)]
struct ImportSkillBody {
    content: String,
    #[serde(default)]
    filename: Option<String>,
}

async fn list_skills(State(app): State<SharedApp>) -> Result<Json<SkillsState>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let skills = app
        .list_user_skills()
        .map_err(ApiError::bad_request)?
        .into_iter()
        .map(|skill| skill.to_public())
        .collect();
    Ok(Json(SkillsState { skills }))
}

async fn create_skill(
    State(app): State<SharedApp>,
    Json(body): Json<crate::skills::SkillUpsert>,
) -> Result<Json<crate::skills::UserSkillPublic>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let skill = app.create_user_skill(body).map_err(ApiError::bad_request)?;
    Ok(Json(skill.to_public()))
}

async fn import_skill(
    State(app): State<SharedApp>,
    Json(body): Json<ImportSkillBody>,
) -> Result<Json<crate::skills::UserSkillPublic>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let skill = app
        .import_user_skill(body.filename.as_deref(), &body.content)
        .map_err(ApiError::bad_request)?;
    Ok(Json(skill.to_public()))
}

async fn extract_attachment(
    Json(body): Json<ExtractRequest>,
) -> Result<Json<attachments::ExtractResponse>, ApiError> {
    let extracted = tokio::task::spawn_blocking(move || attachments::extract_attachment(body))
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?
        .map_err(ApiError::bad_request)?;
    Ok(Json(extracted))
}

async fn update_skill(
    State(app): State<SharedApp>,
    Path(id): Path<String>,
    Json(body): Json<crate::skills::SkillUpsert>,
) -> Result<Json<crate::skills::UserSkillPublic>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    let skill = app
        .update_user_skill(&id, body)
        .map_err(ApiError::bad_request)?;
    Ok(Json(skill.to_public()))
}

async fn delete_skill(
    State(app): State<SharedApp>,
    Path(id): Path<String>,
) -> Result<Json<SkillsState>, ApiError> {
    let app = app.lock().map_err(|_| ApiError::lock())?;
    app.delete_user_skill(&id).map_err(ApiError::bad_request)?;
    let skills = app
        .list_user_skills()
        .map_err(ApiError::bad_request)?
        .into_iter()
        .map(|skill| skill.to_public())
        .collect();
    Ok(Json(SkillsState { skills }))
}

async fn state(State(app): State<SharedApp>) -> Result<Json<AppState>, ApiError> {
    let state = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        AppState::from_app(&app)
    };
    schedule_provider_cache_warm(Arc::clone(&app));
    Ok(Json(state))
}

#[derive(Debug, Serialize)]
struct InstanceInfo {
    app: &'static str,
    version: &'static str,
    focused: bool,
}

async fn focus(State(_app): State<SharedApp>) -> Result<Json<InstanceInfo>, ApiError> {
    let focused = crate::desktop::request_focus();
    Ok(Json(InstanceInfo {
        app: INSTANCE_MARKER,
        version: env!("CARGO_PKG_VERSION"),
        focused,
    }))
}

#[derive(Debug, Deserialize)]
struct OpenUrlBody {
    url: String,
}

async fn open_url(Json(body): Json<OpenUrlBody>) -> Result<Json<serde_json::Value>, ApiError> {
    let url = body.url.trim();
    if !system::is_openable_external_url(url) {
        return Err(ApiError::bad_request("unsupported url"));
    }
    system::open_in_browser(url)
        .map_err(|error| ApiError::bad_request(format!("could not open link: {error}")))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct UpdateCheckQuery {
    #[serde(default)]
    force: Option<String>,
}

async fn check_updates(
    axum::extract::Query(query): axum::extract::Query<UpdateCheckQuery>,
) -> Result<Json<crate::updates::UpdateStatus>, ApiError> {
    let force = matches!(
        query.force.as_deref().map(str::trim),
        Some("1") | Some("true") | Some("yes") | Some("on")
    );
    Ok(Json(crate::updates::check(force).await))
}

fn with_app(
    app: SharedApp,
    action: impl FnOnce(&mut App) -> Result<(), String>,
) -> Result<Json<AppState>, ApiError> {
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    action(&mut app).map_err(ApiError::bad_request)?;
    Ok(Json(AppState::from_app(&app)))
}

#[derive(Debug, Serialize)]
struct AppState {
    version: &'static str,
    theme: &'static str,
    font_body: String,
    font_display: String,
    font_mono: String,
    font_scale: &'static str,
    thinking_supported: bool,
    encryption_enabled: bool,
    encryption_unlocked: bool,
    config_path: String,
    network: NetworkSummary,
    providers: Vec<ProviderPublic>,
    live_turns: Vec<crate::live::LiveTurnInfo>,
}

#[derive(Debug, Serialize)]
struct NetworkSummary {
    via_remote: bool,
    remote_base: String,
    remote_label: String,
    remote_name: String,
    active_remote_id: String,
    remote_count: usize,
    remote_saved: bool,
    remote_ok: bool,
    remote_checking: bool,
    remote_kind: Option<&'static str>,
    remote_error: Option<String>,
    remote_model: Option<String>,
    remote_status: Option<String>,
    remote_models: Vec<RemoteModelOption>,
    remotes: Vec<ProviderPublic>,
    inference_mode: &'static str,
}

impl NetworkSummary {
    fn locked() -> Self {
        Self {
            via_remote: false,
            remote_base: String::new(),
            remote_label: String::new(),
            remote_name: String::new(),
            active_remote_id: String::new(),
            remote_count: 0,
            remote_saved: false,
            remote_ok: false,
            remote_checking: false,
            remote_kind: None,
            remote_error: None,
            remote_model: None,
            remote_status: None,
            remote_models: Vec::new(),
            remotes: Vec::new(),
            inference_mode: "locked",
        }
    }

    fn from_app(app: &App) -> Self {
        let providers = &app.config.providers;
        let remotes = app.public_providers();
        let remote_saved = !providers.items.is_empty();
        let active = providers.active();
        let via_remote = remote_saved;
        let remote_label = active
            .map(|remote| {
                remote
                    .base
                    .trim()
                    .trim_start_matches("http://")
                    .trim_start_matches("https://")
                    .trim_end_matches('/')
                    .to_string()
            })
            .unwrap_or_default();
        let remote_name = active.map(|remote| remote.name.clone()).unwrap_or_default();
        let health = if remote_saved {
            app.remote_health_cached()
        } else {
            None
        };
        let catalog_known = remote_saved && app.remote_model_catalog_peek().is_some();
        let remote_models = if remote_saved {
            app.remote_model_catalog_cached()
        } else {
            Vec::new()
        };
        // Never-probed ≠ unreachable. Only show failure after a real probe result.
        let remote_checking = remote_saved && health.is_none() && !catalog_known;
        let remote_ok = health.as_ref().is_some_and(|h| h.ok) || !remote_models.is_empty();
        let remote_model = remote_models
            .first()
            .map(|m| m.model.clone())
            .or_else(|| health.as_ref().and_then(|h| h.model.clone()));
        Self {
            via_remote,
            remote_base: active.map(|r| r.base.clone()).unwrap_or_default(),
            remote_label,
            remote_name,
            active_remote_id: active.map(|r| r.id.clone()).unwrap_or_default(),
            remote_count: providers.items.len(),
            remote_saved,
            remote_ok,
            remote_checking,
            remote_kind: if remote_checking {
                Some("checking")
            } else if !remote_models.is_empty() {
                Some("ready")
            } else {
                health.as_ref().map(|h| h.kind.as_str())
            },
            remote_error: if remote_checking {
                None
            } else {
                health.as_ref().and_then(|h| h.error.clone())
            },
            remote_model,
            remote_status: if remote_checking {
                Some("checking".into())
            } else {
                health
                    .as_ref()
                    .and_then(|h| h.status.clone())
                    .or_else(|| (!remote_models.is_empty()).then(|| "ready".to_string()))
            },
            remote_models,
            remotes,
            inference_mode: if via_remote { "remote" } else { "local" },
        }
    }
}

impl AppState {
    fn from_app(app: &App) -> Self {
        if app.encryption_enabled() && !app.encryption_unlocked() {
            return Self {
                version: app.app_version(),
                theme: "dark",
                font_body: crate::config::DEFAULT_UI_FONT_BODY.into(),
                font_display: crate::config::DEFAULT_UI_FONT_DISPLAY.into(),
                font_mono: crate::config::DEFAULT_UI_FONT_MONO.into(),
                font_scale: "default",
                thinking_supported: false,
                encryption_enabled: true,
                encryption_unlocked: false,
                config_path: String::new(),
                network: NetworkSummary::locked(),
                providers: Vec::new(),
                live_turns: Vec::new(),
            };
        }
        Self {
            version: app.app_version(),
            theme: app.config.ui.theme.as_str(),
            font_body: app.config.ui.font_body.clone(),
            font_display: app.config.ui.font_display.clone(),
            font_mono: app.config.ui.font_mono.clone(),
            font_scale: app.config.ui.font_scale.as_str(),
            thinking_supported: app.thinking_supported(),
            encryption_enabled: app.encryption_enabled(),
            encryption_unlocked: app.encryption_unlocked(),
            config_path: app.config_path.display().to_string(),
            network: NetworkSummary::from_app(app),
            providers: app.public_providers(),
            live_turns: crate::live::hub().list(),
        }
    }
}

struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<&'static str>,
}

impl ApiError {
    fn lock() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "could not lock app state".to_string(),
            code: None,
        }
    }

    fn bad_request(message: impl AsRef<str>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.as_ref().to_string(),
            code: None,
        }
    }

    fn not_found(message: impl AsRef<str>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.as_ref().to_string(),
            code: None,
        }
    }

    fn conflict(message: impl AsRef<str>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.as_ref().to_string(),
            code: None,
        }
    }

    fn forbidden_code(message: impl AsRef<str>, code: &'static str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.as_ref().to_string(),
            code: Some(code),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.message });
        if let Some(code) = self.code {
            body.as_object_mut()
                .map(|obj| obj.insert("code".into(), serde_json::Value::String(code.into())));
        }
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_api_allowlist_is_minimal_and_exact() {
        use axum::http::Method;

        assert!(locked_api_request_allowed(&Method::GET, "/api/state"));
        assert!(locked_api_request_allowed(&Method::GET, "/api/data"));
        assert!(locked_api_request_allowed(
            &Method::POST,
            "/api/data/encryption/unlock"
        ));
        assert!(locked_api_request_allowed(&Method::POST, "/api/focus"));
        assert!(locked_api_request_allowed(&Method::POST, "/api/open-url"));

        for (method, path) in [
            (Method::GET, "/api/providers"),
            (Method::POST, "/api/providers/test"),
            (Method::POST, "/api/chat/completions"),
            (Method::GET, "/api/chat/live/abc"),
            (Method::POST, "/api/chat/cancel"),
            (Method::GET, "/api/data/store"),
            (Method::GET, "/api/data/preferences"),
            (Method::GET, "/api/skills"),
            (Method::GET, "/api/local-llms"),
            (Method::POST, "/api/data/open"),
            (Method::POST, "/api/data/encryption/lock"),
            (Method::POST, "/api/ui/appearance"),
            (Method::GET, "/api/updates/check"),
        ] {
            assert!(
                !locked_api_request_allowed(&method, path),
                "{method} {path} must remain unavailable while locked"
            );
        }
    }

    #[test]
    fn locked_state_redacts_paths_providers_models_and_preferences() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("config.toml");
        let mut app = App::new(crate::config::Config::default(), config_path).unwrap();
        app.create_provider(
            "Private provider",
            "https://private.example/v1",
            "secret-token",
            ApiStyle::Openai,
            false,
            true,
        )
        .unwrap();
        app.enable_disk_encryption("test passphrase", "test passphrase")
            .unwrap();
        app.lock_disk_encryption();

        let state = AppState::from_app(&app);
        assert!(state.encryption_enabled);
        assert!(!state.encryption_unlocked);
        assert!(state.config_path.is_empty());
        assert!(state.providers.is_empty());
        assert!(state.network.remote_base.is_empty());
        assert!(state.network.remote_name.is_empty());
        assert!(state.network.remote_models.is_empty());
        assert_eq!(state.network.remote_count, 0);
        assert_eq!(state.network.inference_mode, "locked");
        assert!(!state.thinking_supported);

        let data = data_info_from_app(&app);
        assert!(data.data_dir.is_empty());
        assert!(data.config_path.is_empty());
        assert!(data.chats_path.is_empty());
        assert!(data.preferences_path.is_empty());
        assert!(data.skills_dir.is_empty());
        assert!(data.encryption_enabled);
        assert!(!data.encryption_unlocked);
    }

    #[test]
    fn live_identifiers_are_small_and_path_safe() {
        assert_eq!(
            validate_live_id(" turn_abc-123.test ").ok().as_deref(),
            Some("turn_abc-123.test")
        );
        for invalid in ["", " ", "contains/slash", "contains space", "💥"] {
            assert!(validate_live_id(invalid).is_err(), "{invalid:?} must fail");
        }
        assert!(validate_live_id("x".repeat(129)).is_err());
    }
}
