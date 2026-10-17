//! Cookie authentication and the public routes.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{spawn_app, within};

#[tokio::test(flavor = "multi_thread")]
async fn protected_routes_require_a_valid_session() {
    let app = spawn_app(vec![]).await;

    // No cookie at all.
    let r = within(app.plain.get(app.url("/api/info")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["error"], json!("unauthorized"));

    // Garbage cookie.
    let r = within(app.plain.get(app.url("/api/sessions")).header("cookie", "kongming_session=nope.nope").send())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Wrong password.
    let r =
        within(app.plain.post(app.url("/api/auth/login")).json(&json!({"password": "wrong"})).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Correct password: token in the body and a Set-Cookie header.
    let r = within(app.plain.post(app.url("/api/auth/login")).json(&json!({"password": "pw"})).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let set_cookie = r.headers().get("set-cookie").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    assert!(set_cookie.starts_with("kongming_session="), "set-cookie: {set_cookie}");
    assert!(set_cookie.contains("HttpOnly"));
    let body: Value = r.json().await.unwrap();
    let token = body["token"].as_str().expect("token").to_string();
    assert!(kongming::web::auth::verify(&app.state.secret, &token));

    // The token works as a cookie, as a bearer token and as a query parameter.
    let cookie = format!("kongming_session={token}");
    let r = within(app.plain.get(app.url("/api/info")).header("cookie", &cookie).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["name"], json!("kongming"));
    assert_eq!(info["authRequired"], json!(true));
    assert_eq!(info["agents"], json!(1));
    assert_eq!(info["llm"]["model"], json!("test-model"));
    assert_eq!(info["llm"]["apiKeySet"], json!(true));
    assert!(info["llm"].get("apiKey").is_none(), "the key itself is never returned");

    let r = within(app.plain.get(app.url("/api/info")).bearer_auth(&token).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = within(app.plain.get(app.url(&format!("/api/info?token={token}"))).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // The harness client is already logged in.
    let info = app.get("/api/info").await;
    assert_eq!(info["version"], json!(env!("CARGO_PKG_VERSION")));
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_status_reflects_the_cookie() {
    let app = spawn_app(vec![]).await;
    let r = within(app.plain.get(app.url("/api/auth/status")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["authRequired"], json!(true));
    assert_eq!(v["authenticated"], json!(false));

    let v = app.get("/api/auth/status").await;
    assert_eq!(v["authenticated"], json!(true));

    // Logout clears the cookie (the client keeps its default header, so only check the response).
    let r = within(app.client.post(app.url("/api/auth/logout")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cleared = r.headers().get("set-cookie").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    assert!(cleared.contains("Max-Age=0"), "logout cookie: {cleared}");
}

#[tokio::test(flavor = "multi_thread")]
async fn public_routes_work_without_a_cookie() {
    let app = spawn_app(vec![]).await;

    let r = within(app.plain.get(app.url("/health")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["ok"], json!(true));
    assert!(v["tursoMs"].is_number());

    // The OAuth callback is public: without parameters it is a 400, never a 401.
    let r = within(app.plain.get(app.url("/api/mcp/oauth/callback")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    // With an error from the authorization server it redirects back to the UI.
    let no_redirect = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let r =
        within(no_redirect.get(app.url("/api/mcp/oauth/callback?error=access_denied&error_description=nope")).send())
            .await
            .unwrap();
    assert!(r.status().is_redirection(), "status {}", r.status());
    let loc = r.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    assert!(loc.contains("oauth=error"), "location: {loc}");

    // Unknown state with code+state: also a redirect carrying the error.
    let r = within(no_redirect.get(app.url("/api/mcp/oauth/callback?code=abc&state=unknown")).send()).await.unwrap();
    assert!(r.status().is_redirection(), "status {}", r.status());

    // Static UI assets and the manifest are public too.
    let r = within(app.plain.get(app.url("/manifest.webmanifest")).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}
