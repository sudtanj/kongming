//! HTTP server: embedded single-page UI, REST API and WebSocket.

pub mod api;
pub mod auth;
pub mod ws;

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::state::Shared;

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const STYLE_CSS: &str = include_str!("../../web/style.css");
const LOGO_SVG: &str = include_str!("../../web/logo.svg");

fn asset(body: &'static str, content_type: &'static str, cache: bool) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if cache { "public, max-age=3600" } else { "no-cache" }),
    );
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
        Err(e) => {
            (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({"ok": false, "error": format!("{e:#}")})))
                .into_response()
        }
    }
}

/// `host[:port]` of an origin-like value, lowercased.
fn authority_of(v: &str) -> Option<String> {
    let u = url::Url::parse(v).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// The authority named by the Origin header, or the Referer when there is no
/// Origin. `None`: the request carries neither (not from a browser page);
/// a value that is not an address (`null`) gives an empty string, which
/// matches nothing.
pub fn origin_authority(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::ORIGIN).or_else(|| headers.get(header::REFERER))?;
    Some(v.to_str().ok().and_then(authority_of).unwrap_or_default())
}

/// Whether `authority` is what the request was addressed to (Host, or
/// X-Forwarded-Host behind a proxy; a browser cannot set either cross-site).
pub fn names_this_host(headers: &HeaderMap, authority: &str) -> bool {
    [header::HOST.as_str(), "x-forwarded-host"]
        .iter()
        .filter_map(|h| headers.get(*h).and_then(|v| v.to_str().ok()))
        .any(|h| h.trim().eq_ignore_ascii_case(authority))
}

/// Whether the configured public URL is at `authority` (host and port).
async fn is_public_authority(state: &Shared, authority: &str) -> bool {
    state.public_url().await.and_then(|u| authority_of(&u)).is_some_and(|a| a == authority)
}

/// DNS rebinding: with no password, a Host that is a name other than
/// localhost or the public URL's must not reach the API. Not enforced
/// without a public URL to compare against.
async fn rebinding(state: &Shared, headers: &HeaderMap) -> bool {
    if !state.cfg.password.is_empty() {
        return false;
    }
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else { return false };
    let Ok(parsed) = url::Url::parse(&format!("http://{host}")) else { return true };
    let Some(url::Host::Domain(name)) = parsed.host() else { return false };
    let name = name.to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return false;
    }
    match state.public_url().await.and_then(|u| url::Url::parse(&u).ok()) {
        Some(public) => public.host_str().map(str::to_ascii_lowercase).as_deref() != Some(name.as_str()),
        None => false,
    }
}

fn csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        // The one inline script in index.html is allowed by its hash.
        let script =
            INDEX_HTML.split_once("<script>").and_then(|(_, r)| r.split_once("</script>")).map_or("", |(s, _)| s);
        let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script.as_bytes()));
        format!(
            "default-src 'self'; script-src 'self' 'sha256-{hash}'; style-src 'self' 'unsafe-inline'; \
             img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
             frame-ancestors 'none'; form-action 'self'"
        )
    })
}

/// Cross-site and rebinding guards on the way in, security headers on the way out.
/// The Telegram webhook and the OAuth callback are called by other sites on purpose.
async fn guard(axum::extract::State(state): axum::extract::State<Shared>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let exempt = matches!(path, "/api/telegram/webhook" | "/api/mcp/oauth/callback");
    if !exempt {
        let writes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
        let refuse = |msg: &str| (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"error": msg}))).into_response();
        if let (true, Some(origin)) = (writes || path == "/ws", origin_authority(req.headers())) {
            if !names_this_host(req.headers(), &origin) && !is_public_authority(&state, &origin).await {
                return refuse("cross-site request refused");
            }
        }
        if rebinding(&state, req.headers()).await {
            return refuse("unexpected Host header");
        }
    }
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    if let Ok(v) = HeaderValue::from_str(csp()) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    resp
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
        .route("/mcp/:id/oauth/complete", post(api::mcp_oauth_complete))
        .route("/mcp/:id/oauth/disconnect", post(api::mcp_oauth_disconnect))
        .route("/settings", get(api::get_settings).put(api::put_settings))
        .route("/settings/test-llm", post(api::test_llm))
        .route("/proactive/run", post(api::run_check_in))
        .route("/earn", get(api::get_earn).put(api::put_earn))
        .route("/earn/scan", post(api::run_earn_scan))
        .route("/earn/leads/:id", axum::routing::patch(api::patch_earn_lead))
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
        .route("/mcp/oauth/callback", get(api::mcp_oauth_callback))
        // Authenticated by the secret header Telegram sends (see telegram::webhook).
        .route("/telegram/webhook", post(crate::telegram::webhook));

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
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}
