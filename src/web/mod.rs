//! HTTP server: embedded single-page UI, REST API and WebSocket.

pub mod api;
pub mod auth;
pub mod ws;

use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;

use crate::state::Shared;

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const STYLE_CSS: &str = include_str!("../../web/style.css");
const LOGO_SVG: &str = include_str!("../../web/logo.svg");

fn asset(body: &'static str, content_type: &'static str, cache: bool) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(if cache { "public, max-age=3600" } else { "no-cache" }));
    r
}

async fn index() -> Response {
    asset(INDEX_HTML, "text/html; charset=utf-8", false)
}
async fn app_js() -> Response {
    asset(APP_JS, "application/javascript; charset=utf-8", false)
}
async fn style_css() -> Response {
    asset(STYLE_CSS, "text/css; charset=utf-8", false)
}
async fn logo_svg() -> Response {
    asset(LOGO_SVG, "image/svg+xml", true)
}
async fn manifest() -> Response {
    asset(
        r##"{"name":"kongming","short_name":"kongming","start_url":"/","display":"standalone","background_color":"#0b0d10","theme_color":"#0b0d10","icons":[{"src":"/logo.svg","sizes":"any","type":"image/svg+xml"}]}"##,
        "application/manifest+json",
        true,
    )
}
async fn health(axum::extract::State(state): axum::extract::State<Shared>) -> Response {
    match state.db.turso.ping().await {
        Ok(ms) => axum::Json(serde_json::json!({"ok": true, "tursoMs": ms})).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({"ok": false, "error": format!("{e:#}")}))).into_response(),
    }
}

pub fn router(state: Shared) -> Router {
    let protected = Router::new()
        .route("/info", get(api::info))
        .route("/sessions", get(api::list_sessions).post(api::create_session))
        .route("/sessions/:id", get(api::get_session).patch(api::patch_session).delete(api::delete_session))
        .route("/sessions/:id/messages", post(api::send_message))
        .route("/sessions/:id/clear", post(api::clear_session))
        .route("/sessions/:id/stop", post(api::stop_session))
        .route("/agents", get(api::list_agents).post(api::create_agent))
        .route("/agents/:id", get(api::get_agent).put(api::update_agent).delete(api::delete_agent))
        .route("/skills", get(api::list_skills))
        .route("/skills/import", post(api::import_skill))
        .route("/skills/:name", get(api::get_skill).put(api::put_skill).delete(api::delete_skill))
        .route("/memories", get(api::list_memories).post(api::create_memory))
        .route("/memories/:id", delete(api::delete_memory))
        .route("/mcp", get(api::list_mcp).post(api::create_mcp))
        .route("/mcp/:id", axum::routing::put(api::update_mcp).delete(api::delete_mcp))
        .route("/mcp/:id/refresh", post(api::refresh_mcp))
        .route("/mcp/:id/call", post(api::call_mcp_tool))
        .route("/mcp/:id/oauth/start", post(api::mcp_oauth_start).get(api::mcp_oauth_start))
        .route("/mcp/:id/oauth/disconnect", post(api::mcp_oauth_disconnect))
        .route("/settings", get(api::get_settings).put(api::put_settings))
        .route("/settings/test-llm", post(api::test_llm))
        .route("/cron", get(api::list_cron).post(api::create_cron))
        .route("/cron/:id", axum::routing::put(api::update_cron).delete(api::delete_cron))
        .route("/cron/:id/run", post(api::run_cron))
        .route("/tasks", get(api::list_tasks))
        .route("/improvements", get(api::list_improvements))
        .route("/usage", get(api::usage))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let public = Router::new()
        .route("/auth/status", get(api::auth_status))
        .route("/auth/login", post(api::login))
        .route("/auth/logout", post(api::logout))
        .route("/mcp/oauth/callback", get(api::mcp_oauth_callback));

    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/logo.svg", get(logo_svg))
        .route("/manifest.webmanifest", get(manifest))
        .route("/health", get(health))
        .route("/ws", get(ws::handler))
        .nest("/api", public.merge(protected))
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .with_state(state)
}
