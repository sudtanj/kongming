//! MCP OAuth behind a firewall: the redirect goes to the loopback address and
//! the user pastes the address-bar URL back; with a public URL the callback
//! completes on its own. Runs against a mock authorization server that serves
//! no metadata, so the conventional /authorize, /token and /register paths
//! are used.

mod support;

use std::sync::Arc;

use axum::extract::{Form, State};
use axum::routing::post;
use axum::{Json, Router};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::collections::HashMap;
use support::{serve, spawn_app, str_of};

#[derive(Clone, Default)]
struct AuthMock {
    token_forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

async fn start_auth_server() -> (String, AuthMock) {
    let mock = AuthMock::default();
    let router = Router::new()
        .route("/register", post(|| async { Json(json!({"client_id": "client-1"})) }))
        .route(
            "/token",
            post(|State(m): State<AuthMock>, Form(f): Form<HashMap<String, String>>| async move {
                let ok = f.get("code").map(String::as_str) == Some("the-code");
                m.token_forms.lock().push(f);
                if ok {
                    (StatusCode::OK, Json(json!({"access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600})))
                } else {
                    (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"})))
                }
            }),
        )
        .with_state(mock.clone());
    (serve(router).await, mock)
}

fn query_param(url: &str, key: &str) -> String {
    let u = url::Url::parse(url).expect("authorize url");
    u.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned()).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn pasted_loopback_redirect_completes_sign_in() {
    let (auth, mock) = start_auth_server().await;
    let app = spawn_app(vec![]).await;
    let created =
        app.post("/api/mcp", json!({"name": "Remote", "url": format!("{auth}/mcp"), "transport": "http", "authType": "oauth"})).await;
    let id = str_of(&created, "id").to_string();

    let started = app.post(&format!("/api/mcp/{id}/oauth/start"), json!({})).await;
    assert_eq!(started["manual"], json!(true), "{started}");
    let redirect = str_of(&started, "redirectUri").to_string();
    assert!(redirect.starts_with("http://localhost:") && redirect.ends_with("/api/mcp/oauth/callback"), "{redirect}");
    let authorize = str_of(&started, "url").to_string();
    assert!(authorize.starts_with(&format!("{auth}/authorize?")), "{authorize}");
    assert_eq!(query_param(&authorize, "redirect_uri"), redirect);
    assert_eq!(query_param(&authorize, "client_id"), "client-1");
    assert_eq!(query_param(&authorize, "code_challenge_method"), "S256");
    let state = query_param(&authorize, "state");
    assert!(!state.is_empty());

    // A paste with only an error is rejected and keeps the sign-in pending.
    let (status, body) = app.post_raw(&format!("/api/mcp/{id}/oauth/complete"), json!({"url": format!("{redirect}?state={state}")})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let pasted = format!("{redirect}?code=the-code&state={state}");
    let done = app.post(&format!("/api/mcp/{id}/oauth/complete"), json!({"url": pasted})).await;
    assert_eq!(done["ok"], json!(true));
    assert_eq!(done["server"]["oauthConnected"], json!(true), "{done}");
    assert!(done["server"].get("oauth").is_none(), "tokens are never exposed");

    let forms = mock.token_forms.lock().clone();
    assert_eq!(forms.len(), 1);
    assert_eq!(forms[0].get("redirect_uri"), Some(&redirect));
    assert_eq!(forms[0].get("client_id").map(String::as_str), Some("client-1"));
    assert!(forms[0].get("code_verifier").is_some_and(|v| v.len() >= 43));

    // The state is single use.
    let (status, _) = app.post_raw(&format!("/api/mcp/{id}/oauth/complete"), json!({"url": pasted})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn bare_code_uses_the_pending_sign_in_of_that_server() {
    let (auth, _mock) = start_auth_server().await;
    let app = spawn_app(vec![]).await;
    let created =
        app.post("/api/mcp", json!({"name": "Remote", "url": format!("{auth}/mcp"), "transport": "http", "authType": "oauth"})).await;
    let id = str_of(&created, "id").to_string();

    let (status, body) = app.post_raw(&format!("/api/mcp/{id}/oauth/complete"), json!({"code": "the-code"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "nothing pending yet: {body}");

    app.post(&format!("/api/mcp/{id}/oauth/start"), json!({})).await;
    let done = app.post(&format!("/api/mcp/{id}/oauth/complete"), json!({"code": "the-code"})).await;
    assert_eq!(done["server"]["oauthConnected"], json!(true), "{done}");
}

#[tokio::test(flavor = "multi_thread")]
async fn public_url_uses_the_automatic_callback() {
    let (auth, _mock) = start_auth_server().await;
    let app = spawn_app(vec![]).await;
    app.put("/api/settings", json!({"publicUrl": app.base_url})).await;
    let created =
        app.post("/api/mcp", json!({"name": "Remote", "url": format!("{auth}/mcp"), "transport": "http", "authType": "oauth"})).await;
    let id = str_of(&created, "id").to_string();

    let started = app.post(&format!("/api/mcp/{id}/oauth/start"), json!({})).await;
    assert_eq!(started["manual"], json!(false));
    let redirect = str_of(&started, "redirectUri").to_string();
    assert_eq!(redirect, format!("{}/api/mcp/oauth/callback", app.base_url));
    let state = query_param(str_of(&started, "url"), "state");

    let no_redirect = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let resp = no_redirect.get(format!("{redirect}?code=the-code&state={state}")).send().await.unwrap();
    assert!(resp.status().is_redirection(), "{}", resp.status());
    let location = resp.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(location.contains("oauth=ok"), "{location}");
    let list = app.get("/api/mcp").await;
    let server = list.as_array().unwrap().iter().find(|s| s["id"] == json!(id)).unwrap();
    assert_eq!(server["oauthConnected"], json!(true), "{server}");
}
