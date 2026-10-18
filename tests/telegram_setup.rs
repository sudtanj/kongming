//! Telegram set up from the dashboard: saving a token checks it with getMe and
//! points Telegram at kongming (webhook with a public HTTPS URL, otherwise long
//! polling). Runs against a mock Bot API.

mod support;

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{poll_until, serve, spawn_app_with, TestApp};

const GOOD: &str = "123:good";

#[derive(Clone, Default)]
struct BotApi {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl BotApi {
    fn methods(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(m, _)| m.clone()).collect()
    }

    fn last(&self, method: &str) -> Option<Value> {
        self.calls.lock().iter().rev().find(|(m, _)| m == method).map(|(_, b)| b.clone())
    }
}

async fn start_bot_api() -> (String, BotApi) {
    let api = BotApi::default();
    let router =
        Router::new()
            .route(
                "/:bot/:method",
                post(
                    |State(api): State<BotApi>,
                     Path((bot, method)): Path<(String, String)>,
                     Json(body): Json<Value>| async move {
                        if bot != format!("bot{GOOD}") {
                            return Json(json!({"ok": false, "error_code": 401, "description": "Unauthorized"}));
                        }
                        api.calls.lock().push((method.clone(), body));
                        match method.as_str() {
                            "getMe" => Json(
                                json!({"ok": true, "result": {"id": 123, "is_bot": true, "username": "kongming_bot"}}),
                            ),
                            "sendMessage" => Json(json!({"ok": true, "result": {"message_id": 1}})),
                            _ => Json(json!({"ok": true, "result": true})),
                        }
                    },
                ),
            )
            .with_state(api.clone());
    (serve(router).await, api)
}

async fn app_with_bot_api() -> (TestApp, BotApi) {
    let (url, api) = start_bot_api().await;
    (spawn_app_with(vec![], &["--telegram-api", &url]).await, api)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_token_is_not_stored() {
    let (app, api) = app_with_bot_api().await;
    let (status, body) = app.put_raw("/api/settings", json!({"telegram": {"token": "999:bad"}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("Unauthorized"), "{body}");
    assert!(!body.to_string().contains("999:bad"), "the token never shows up in errors: {body}");
    let settings = app.get("/api/settings").await;
    assert_eq!(settings["telegram"]["tokenSet"], json!(false));
    assert!(api.methods().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_public_url_the_bot_uses_long_polling() {
    let (app, api) = app_with_bot_api().await;
    let settings = app.put("/api/settings", json!({"telegram": {"token": GOOD, "allowedUsers": "42"}})).await;
    let tg = &settings["telegram"];
    assert_eq!(tg["tokenSet"], json!(true));
    assert!(tg.get("token").is_none(), "the token is never sent back");
    assert_eq!(tg["status"]["ok"], json!(true), "{tg}");
    assert_eq!(tg["status"]["bot"], json!("kongming_bot"));
    assert_eq!(tg["status"]["mode"], json!("polling"));
    assert_eq!(tg["allowedUsers"], json!([42]));
    let methods = api.methods();
    assert!(methods.contains(&"setMyCommands".to_string()), "{methods:?}");
    assert!(methods.contains(&"deleteWebhook".to_string()), "a leftover webhook would block getUpdates: {methods:?}");
    assert!(!methods.contains(&"setWebhook".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn with_a_public_https_url_the_webhook_is_registered_and_checked() {
    let (app, api) = app_with_bot_api().await;
    app.put("/api/settings", json!({"publicUrl": "https://kongming.example.com/"})).await;
    let settings = app.put("/api/settings", json!({"telegram": {"token": GOOD}})).await;
    let tg = &settings["telegram"];
    assert_eq!(tg["status"]["mode"], json!("webhook"), "{tg}");
    let url = "https://kongming.example.com/api/telegram/webhook";
    assert_eq!(tg["status"]["webhookUrl"], json!(url));
    let set = api.last("setWebhook").expect("setWebhook was called");
    assert_eq!(set["url"], json!(url));
    let secret = set["secret_token"].as_str().expect("secret token").to_string();
    assert!(secret.len() >= 32);

    let update =
        json!({"update_id": 1, "message": {"message_id": 5, "chat": {"id": 77}, "from": {"id": 77}, "text": "hi"}});
    let hook = app.url("/api/telegram/webhook");
    let wrong =
        app.plain.post(&hook).header("x-telegram-bot-api-secret-token", "nope").json(&update).send().await.unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let missing = app.plain.post(&hook).json(&update).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let ok =
        app.plain.post(&hook).header("x-telegram-bot-api-secret-token", &secret).json(&update).send().await.unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    // User 77 is not allowed: the bot answers with their id.
    let reply = poll_until("the private-bot reply", || {
        let api = api.clone();
        async move { api.last("sendMessage") }
    })
    .await;
    assert_eq!(reply["chat_id"], json!(77));
    assert!(reply["text"].as_str().unwrap_or("").contains("77"), "{reply}");

    // Switching Telegram off removes the webhook and closes the endpoint.
    let before = api.methods().iter().filter(|m| *m == "deleteWebhook").count();
    let settings = app.put("/api/settings", json!({"telegram": {"enabled": false}})).await;
    assert!(settings["telegram"]["status"].is_null(), "{settings}");
    assert_eq!(api.methods().iter().filter(|m| *m == "deleteWebhook").count(), before + 1);
    let closed =
        app.plain.post(&hook).header("x-telegram-bot-api-secret-token", &secret).json(&update).send().await.unwrap();
    assert_eq!(closed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_without_an_llm_set_up_points_to_settings() {
    let (app, _api) = app_with_bot_api().await;
    app.state.db.set_setting("llm", &json!({})).await.unwrap();
    let err = app.state.provider().await.err().expect("no provider without settings");
    assert!(err.to_string().contains("Settings"), "{err}");
}
