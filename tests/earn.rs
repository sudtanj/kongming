//! Earnings: a scan lists a marketplace, keeps real paid work (not agent ads,
//! not tiny jobs), and the lead agent rates it and sends one digest on
//! Telegram. Bidding through the provider's API waits for the user's go-ahead,
//! uses the saved key without showing it to the model, and never touches money
//! paths. Runs against a mock marketplace, a mock Bot API and the scripted LLM.

mod support;

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{json, Value};
use support::{role, serve, spawn_app_with, str_of, text, tool_call, TestApp};

const KEY: &str = "tk_secret_123";

#[derive(Clone, Default)]
struct Mock {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Mock {
    fn calls(&self, method: &str) -> Vec<Value> {
        self.calls.lock().iter().filter(|(m, _)| m == method).map(|(_, b)| b.clone()).collect()
    }
}

/// A toku-shaped marketplace: one real job, one agent ad, one job too small to bother with.
async fn market() -> (String, Mock) {
    let mock = Mock::default();
    let router = Router::new()
        .route(
            "/api/agents/jobs",
            get(|| async {
                Json(json!({"jobPosts": [
                    {"id": "a1", "title": "Write 5 product descriptions", "description": "For a shoe shop, 100 words each.", "budgetCents": 4500, "bidCount": 2, "tags": ["writing"], "status": "OPEN"},
                    {"id": "a2", "title": "AVAILABLE: Content retainer", "description": "Hire me", "budgetCents": 24900, "status": "OPEN"},
                    {"id": "a3", "title": "Say hello", "description": "x", "budgetCents": 50, "status": "OPEN"}
                ], "total": 3}))
            }),
        )
        .route(
            "/api/agents/jobs/:id/bids",
            post(|State(m): State<Mock>, Path(id): Path<String>, headers: HeaderMap, Json(body): Json<Value>| async move {
                let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                m.calls.lock().push(("bid".into(), json!({"job": id, "auth": auth, "body": body})));
                // Echo the key back: the tool must not show it to the model.
                Json(json!({"id": "bid1", "status": "PENDING", "echo": auth}))
            }),
        )
        .with_state(mock.clone());
    (serve(router).await, mock)
}

async fn bot_api() -> (String, Mock) {
    let mock = Mock::default();
    let router = Router::new()
        .route(
            "/:bot/:method",
            post(
                |State(m): State<Mock>, Path((_, method)): Path<(String, String)>, Json(body): Json<Value>| async move {
                    m.calls.lock().push((method.clone(), body));
                    match method.as_str() {
                        "getMe" => {
                            Json(json!({"ok": true, "result": {"id": 123, "is_bot": true, "username": "kongming_bot"}}))
                        }
                        "sendMessage" => Json(json!({"ok": true, "result": {"message_id": 1}})),
                        _ => Json(json!({"ok": true, "result": true})),
                    }
                },
            ),
        )
        .with_state(mock.clone());
    (serve(router).await, mock)
}

/// Every provider off except the mock marketplace (as toku).
fn only_toku(base: &str) -> Value {
    let mut providers = serde_json::Map::new();
    for p in kongming::earn::PROVIDERS {
        providers.insert(p.id.into(), json!({"enabled": false}));
    }
    providers.insert("toku".into(), json!({"enabled": true, "baseUrl": base, "apiKey": KEY}));
    json!({"enabled": true, "minUsd": 5, "providers": providers})
}

async fn last_tool_result(app: &TestApp, sid: &str) -> (String, bool) {
    let msgs = app.messages(sid).await;
    let m = msgs.iter().rev().find(|m| role(m) == "tool").expect("a tool result");
    (str_of(&m["parts"][0], "content").to_string(), m["parts"][0]["isError"] == json!(true))
}

#[tokio::test(flavor = "multi_thread")]
async fn scan_rates_new_work_and_bids_only_after_the_user_takes_it() {
    let (market_url, market) = market().await;
    let (bot_url, bot) = bot_api().await;
    let app = spawn_app_with(vec![], &["--telegram-api", &bot_url]).await;
    app.disable_reflection().await;

    // The key is stored but never shown back.
    let shown = app.put("/api/earn", only_toku(&market_url)).await;
    assert_eq!(shown["settings"]["providers"]["toku"]["keySet"], json!(true), "{shown}");
    assert!(!shown.to_string().contains(KEY), "{shown}");
    let (status, body) = app.put_raw("/api/earn", json!({"scanMinutes": 5})).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");

    // Without Telegram a scan only collects: the ad and the 50-cent job are dropped.
    let report = kongming::earn::scan(&app.state, false).await.unwrap();
    assert_eq!((report.listed, report.new), (3, 1), "{report:?}");
    assert_eq!(app.llm.request_count(), 0);
    let d = app.get("/api/earn").await;
    let lead = d["leads"][0].clone();
    assert_eq!(d["leads"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(lead["status"], json!("new"));
    assert_eq!(lead["budgetUsd"], json!(45.0));
    assert_eq!(lead["url"], json!("https://www.toku.agency/jobs/a1"));
    assert_eq!(
        d["providers"].as_array().unwrap().iter().find(|p| p["id"] == json!("toku")).unwrap()["status"]["kept"],
        json!(1)
    );
    let r = str_of(&lead, "ref").to_string();
    let id = str_of(&lead, "id").to_string();

    // With Telegram, the lead agent rates it and sends one digest.
    app.put(
        "/api/settings",
        json!({"telegram": {"token": "123:good", "allowedUsers": "42"}, "proactive": {"quietHours": ""}}),
    )
    .await;
    app.llm.push(vec![
        tool_call("earn", json!({"op": "rate", "ref": r, "score": 8, "shortlist": true, "note": "easy writing job"})),
        tool_call(
            "notify_user",
            json!({"message": format!("1. `{r}` Write 5 product descriptions, $45 on toku. Reply `take {r}`."), "topic": "Paid work test", "requested": true}),
        ),
        text("shortlisted 1"),
    ]);
    let report = kongming::earn::scan(&app.state, false).await.unwrap();
    assert_eq!((report.new, report.shortlisted), (0, 1), "{report:?}");
    let sent = bot.calls("sendMessage");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(str_of(&sent[0], "text").contains(&r), "{sent:?}");
    let req = app.llm.requests().last().cloned().unwrap();
    let prompt = req["messages"].to_string();
    assert!(prompt.contains("Earnings scan") && prompt.contains("Write 5 product descriptions"), "{prompt}");
    assert!(!prompt.contains("AVAILABLE"), "ads never reach the model");

    // Bidding waits for the user.
    let sid = app.create_session().await;
    let bid = json!({"op": "request", "provider": "toku", "method": "POST", "path": "/api/agents/jobs/a1/bids", "ref": r, "body": {"priceCents": 4500, "message": "I can do this today."}});
    app.llm.push(vec![tool_call("earn", bid.clone()), text("waiting")]);
    app.send_wait(&sid, "bid on it").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("approves"), "{result}");
    assert!(market.calls("bid").is_empty());

    // "take <ref>": approved in the user's conversation, then the bid goes out with the key.
    app.llm.push(vec![
        tool_call("earn", json!({"op": "status", "ref": r, "status": "approved"})),
        tool_call("earn", bid),
        text("bid sent"),
    ]);
    app.send_wait(&sid, &format!("take {r}")).await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("HTTP 200"), "{result}");
    assert!(!result.contains(KEY), "the key never reaches the model: {result}");
    let bids = market.calls("bid");
    assert_eq!(bids.len(), 1);
    assert_eq!(bids[0]["auth"], json!(format!("Bearer {KEY}")));
    assert_eq!(bids[0]["body"]["priceCents"], json!(4500));
    let lead = app.state.db.earn_lead(&id).await.unwrap().unwrap();
    assert_eq!(lead.status, "working");

    // Money and accounts are never the agent's business.
    app.llm.push(vec![
        tool_call("earn", json!({"op": "request", "provider": "toku", "method": "POST", "path": "/api/agents/wallet/withdraw", "ref": r, "body": {"amountCents": 4500}})),
        text("no"),
    ]);
    app.send_wait(&sid, "withdraw it").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("money"), "{result}");

    // The dashboard moves leads too.
    let resp = app
        .client
        .patch(app.url(&format!("/api/earn/leads/{r}")))
        .json(&json!({"status": "paid"}))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let lead: Value = resp.json().await.unwrap();
    assert_eq!(lead["status"], json!("paid"));
    let activity = app.get("/api/improvements").await;
    assert!(activity.as_array().unwrap().iter().any(|i| i["kind"] == json!("earn_scan")), "{activity}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_earn_tool_is_only_there_while_earning_is_on() {
    let (market_url, _market) = market().await;
    let app = spawn_app_with(vec![], &[]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    app.llm.push(vec![text("hi")]);
    app.send_wait(&sid, "hello").await;
    let names = support::tool_names(&app.llm.requests().last().cloned().unwrap());
    assert!(!names.iter().any(|n| n == "earn"), "{names:?}");

    app.put("/api/earn", only_toku(&market_url)).await;
    app.llm.push(vec![text("hi")]);
    app.send_wait(&sid, "hello again").await;
    let names = support::tool_names(&app.llm.requests().last().cloned().unwrap());
    assert!(names.iter().any(|n| n == "earn"), "{names:?}");
}
