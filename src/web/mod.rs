mod embed;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::{
    agent::{self, AgentRequest},
    app::App,
    chat,
    providers::{
        ApiStyle, ProviderHealth, ProviderHealthKind, ProviderPublic, RemoteModelOption,
        normalize_openai_base, probe_provider_catalog, probe_provider_health,
        probe_provider_style, provider_base_same_host,
    },
    store::{self, StorageMode},
    system,
};
use embed::{
    APP_ICON_PNG, CHAT_HTML, HIGHLIGHT_JS, MARKED_JS, ORB_JS, PURIFY_JS, SETTINGS_HTML,
    UI_MARK_DARK_PNG, UI_MARK_LIGHT_PNG,
};

pub type SharedApp = Arc<Mutex<App>>;

pub const INSTANCE_MARKER: &str = "tensorui";

static PROVIDER_CACHE_WARM_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn schedule_provider_cache_warm(app: SharedApp) {
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

        // Snapshot stale targets under a short lock; probe without holding App.
        let (health_targets, catalog_target) = match app.lock() {
            Ok(guard) => guard.provider_warm_targets(),
            Err(_) => return,
        };

        let health_results: Vec<_> = health_targets
            .into_iter()
            .map(|(style, base, token)| {
                let health = probe_provider_health(&base, &token, style);
                (style, base, token, health)
            })
            .collect();
        let catalog_result = catalog_target.map(|(style, base, token)| {
            let catalog = probe_provider_catalog(&base, &token, style, &[]);
            (style, base, token, catalog)
        });

        if let Ok(guard) = app.lock() {
            for (style, base, token, health) in health_results {
                guard.store_remote_health(style, &base, &token, health);
            }
            if let Some((style, base, token, catalog)) = catalog_result {
                guard.store_remote_catalog(style, &base, &token, catalog);
            }
        }
    });
}

pub async fn serve(app: SharedApp, listener: TcpListener) -> anyhow::Result<()> {
    // Probe providers before the first browser poll so Chat doesn't flash unreachable.
    schedule_provider_cache_warm(Arc::clone(&app));

    let api = Router::new()
        .route("/api/chat/completions", post(chat_completions))
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
        .route("/api/focus", post(focus))
        .route("/api/data", get(data_info).post(set_storage_mode))
        .route("/api/data/open", post(open_data_dir))
        .route("/api/data/encryption/enable", post(enable_encryption))
        .route("/api/data/encryption/disable", post(disable_encryption))
        .route("/api/data/encryption/unlock", post(unlock_encryption))
        .route("/api/data/encryption/lock", post(lock_encryption))
        .route("/api/data/store", get(get_chat_store).put(put_chat_store))
        .route(
            "/api/data/preferences",
            get(get_chat_preferences).put(put_chat_preferences),
        )
        .route("/api/skills", get(list_skills).post(create_skill))
        .route("/api/skills/import", post(import_skill))
        .route(
            "/api/skills/{id}",
            axum::routing::patch(update_skill).delete(delete_skill),
        );

    let router = Router::new()
        .route("/", get(chat_page))
        .route("/settings", get(settings_page))
        .route("/admin", get(|| async { Redirect::permanent("/settings") }))
        .route("/chat", get(|| async { Redirect::permanent("/") }))
        .route("/orb.js", get(orb_script))
        .route("/highlight.min.js", get(highlight_script))
        .route("/marked.min.js", get(marked_script))
        .route("/purify.min.js", get(purify_script))
        .route("/browser-favicon.png", get(app_icon_png))
        .route("/icon-darkmode.png", get(ui_mark_dark))
        .route("/icon-lightmode.png", get(ui_mark_light))
        .route("/favicon.ico", get(app_icon_png))
        // Back-compat aliases for older cached HTML.
        .route("/ti.png", get(app_icon_png))
        .route("/ti-transparent-bg-white.png", get(ui_mark_dark))
        .route("/ti-transparent-bg-black.png", get(ui_mark_light))
        .merge(api)
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

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn chat_page() -> Html<&'static str> {
    Html(CHAT_HTML)
}

async fn settings_page() -> Html<&'static str> {
    Html(SETTINGS_HTML)
}

async fn orb_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        ORB_JS,
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

async fn app_icon_png() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], APP_ICON_PNG)
}

async fn ui_mark_dark() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], UI_MARK_DARK_PNG)
}

async fn ui_mark_light() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], UI_MARK_LIGHT_PNG)
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
    let theme = crate::config::UiTheme::parse(&body.theme).ok_or_else(|| {
        ApiError::bad_request("theme must be \"dark\", \"light\", or \"system\"")
    })?;
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
    let (api_base, token, api_style) = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let providers = &app.config.providers;
        if let Some(requested) = body.remote_base.as_deref() {
            let Some(linked) = providers
                .items
                .iter()
                .find(|p| provider_base_same_host(&p.base, requested))
                .or_else(|| providers.active())
            else {
                return Err(ApiError::bad_request(
                    "No provider is configured for that model.",
                ));
            };
            if !provider_base_same_host(&linked.base, requested) {
                return Err(ApiError::bad_request(
                    "Model must be on a configured provider.",
                ));
            }
            let api_base = normalize_openai_base(requested)
                .ok_or_else(|| ApiError::bad_request("Invalid model API base."))?;
            (api_base, linked.token.clone(), linked.api_style)
        } else {
            let Some(active) = providers.active() else {
                return Err(ApiError::bad_request(
                    "No provider configured. Add one in Settings.",
                ));
            };
            let api_base = normalize_openai_base(&active.base).ok_or_else(|| {
                ApiError::bad_request("Active provider has an invalid base URL.")
            })?;
            (api_base, active.token.clone(), active.api_style)
        }
    };

    let title = chat::generate_chat_title(
        &api_base,
        &token,
        api_style,
        model.as_deref(),
        message,
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

    let (remote, user_skills) = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let user_skills = app.enabled_user_skills();
        let providers = &app.config.providers;
        let remote = if let Some(requested) = remote_base_override.as_deref() {
            let Some(linked) = providers
                .items
                .iter()
                .find(|p| provider_base_same_host(&p.base, requested))
                .or_else(|| providers.active())
            else {
                return Err(ApiError::bad_request(
                    "No provider is configured for that model.",
                ));
            };
            if !provider_base_same_host(&linked.base, requested) {
                return Err(ApiError::bad_request(
                    "Model must be on a configured provider.",
                ));
            }
            let api_base = normalize_openai_base(requested)
                .ok_or_else(|| ApiError::bad_request("Invalid model API base."))?;
            Some((api_base, linked.token.clone(), linked.api_style))
        } else {
            let Some(active) = providers.active() else {
                return Err(ApiError::bad_request(
                    "No provider configured. Add one in Settings.",
                ));
            };
            let api_base = normalize_openai_base(&active.base).ok_or_else(|| {
                ApiError::bad_request("Active provider has an invalid base URL.")
            })?;
            Some((api_base, active.token.clone(), active.api_style))
        };
        (remote, user_skills)
    };

    let Some((api_base, token, api_style)) = remote else {
        return Err(ApiError::bad_request(
            "No provider configured. Add one in Settings.",
        ));
    };

    if let Some(model) = remote_model
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("model".to_string(), serde_json::Value::String(model));
    }
    let key = (!token.trim().is_empty()).then_some(token.as_str());
    let stream = match serde_json::from_value::<AgentRequest>(body.clone()) {
        Ok(request) if agent::should_run_agent(&request, &user_skills) => {
            if request.messages.is_empty() {
                return Err(ApiError::bad_request("messages must not be empty"));
            }
            agent::stream_agent(&api_base, key, api_style, request, user_skills)
        }
        _ => {
            if let Some(messages) = body.get_mut("messages").and_then(|v| v.as_array_mut()) {
                agent::inject_skill_catalog_into_messages(messages, &user_skills);
            }
            chat::stream_remote_completion(&api_base, &token, api_style, body)
        }
    };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(response)
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
}

#[derive(Debug, Serialize)]
struct TestProviderResponse {
    ok: bool,
    base: String,
    api_style: &'static str,
    detected: bool,
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
        } else if let Some(id) = body.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
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

    let probe_base = base.clone();
    let probe_token = token.clone();
    let (probe, catalog) = tokio::task::spawn_blocking(move || {
        let probe = probe_provider_style(&probe_base, &probe_token, forced_style);
        let catalog = if probe.health.ok || matches!(probe.health.kind, ProviderHealthKind::Empty)
        {
            Some(probe_provider_catalog(
                &probe_base,
                &probe_token,
                probe.api_style,
                &[],
            ))
        } else {
            None
        };
        (probe, catalog)
    })
    .await
    .map_err(|error| ApiError::bad_request(format!("connection test failed: {error}")))?;

    {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        app.store_remote_health(probe.api_style, &base, &token, probe.health.clone());
        if let Some(catalog) = catalog {
            app.store_remote_catalog(probe.api_style, &base, &token, catalog);
        }
    }

    Ok(Json(TestProviderResponse {
        ok: probe.health.ok || matches!(probe.health.kind, ProviderHealthKind::Empty),
        base,
        api_style: probe.api_style.as_str(),
        detected: probe.detected,
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
        tokio::task::spawn_blocking(move || {
            probe_provider_style(&probe_base, &probe_token, None).api_style
        })
        .await
        .map_err(|error| ApiError::bad_request(format!("style detection failed: {error}")))?
    };

    let response = {
        let mut app = app.lock().map_err(|_| ApiError::lock())?;
        app.create_provider(&body.name, &base, &token, api_style, body.activate)
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
struct DataInfo {
    storage: &'static str,
    browser_storage: bool,
    encryption_enabled: bool,
    encryption_unlocked: bool,
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
    DataInfo {
        storage: app.storage_mode().as_str(),
        browser_storage: app.storage_mode().is_browser(),
        encryption_enabled: app.encryption_enabled(),
        encryption_unlocked: app.encryption_unlocked(),
        data_dir: root.display().to_string(),
        config_path: app.config_path.display().to_string(),
        chats_path: store::chats_path(&root).display().to_string(),
        preferences_path: store::preferences_path(&root).display().to_string(),
        skills_dir: root.join("chat-skills").display().to_string(),
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
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    let mode = if body.browser_storage {
        StorageMode::Browser
    } else {
        StorageMode::Disk
    };
    app.set_storage_mode(mode);
    Ok(Json(data_info_from_app(&app)))
}

async fn open_data_dir(State(app): State<SharedApp>) -> Result<Json<serde_json::Value>, ApiError> {
    let path = {
        let app = app.lock().map_err(|_| ApiError::lock())?;
        let root = app.data_dir();
        store::ensure_data_dir(&root).map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
        root
    };
    system::open_in_file_manager(&path)
        .map_err(|error| ApiError::bad_request(format!("could not open folder: {error}")))?;
    Ok(Json(serde_json::json!({ "ok": true, "path": path.display().to_string() })))
}

#[derive(Debug, Deserialize)]
struct PassphraseBody {
    passphrase: String,
    #[serde(default)]
    passphrase_confirm: Option<String>,
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
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    app.unlock_disk_encryption(&body.passphrase)
        .map_err(ApiError::bad_request)?;
    Ok(Json(data_info_from_app(&app)))
}

async fn lock_encryption(State(app): State<SharedApp>) -> Result<Json<DataInfo>, ApiError> {
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
    Ok(Json(InstanceInfo {
        app: INSTANCE_MARKER,
        version: env!("CARGO_PKG_VERSION"),
        focused: false,
    }))
}

fn with_app(app: SharedApp, action: impl FnOnce(&mut App)) -> Result<Json<AppState>, ApiError> {
    let mut app = app.lock().map_err(|_| ApiError::lock())?;
    action(&mut app);
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
    config_path: String,
    network: NetworkSummary,
    providers: Vec<ProviderPublic>,
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
    fn from_app(app: &App) -> Self {
        let providers = &app.config.providers;
        let remotes = app.public_providers();
        let remote_saved = !providers.items.is_empty();
        let active = providers.active();
        let via_remote = active.is_some();
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
        let remote_checking = remote_saved
            && active.is_some_and(|remote| !remote.base.trim().is_empty())
            && health.is_none()
            && !catalog_known;
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
        Self {
            version: app.app_version(),
            theme: app.config.ui.theme.as_str(),
            font_body: app.config.ui.font_body.clone(),
            font_display: app.config.ui.font_display.clone(),
            font_mono: app.config.ui.font_mono.clone(),
            font_scale: app.config.ui.font_scale.as_str(),
            thinking_supported: app.thinking_supported(),
            config_path: app.config_path.display().to_string(),
            network: NetworkSummary::from_app(app),
            providers: app.public_providers(),
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
