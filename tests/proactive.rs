//! Proactive messages: `notify_user` reaches the user on Telegram (once per
//! topic, not when switched off unless they asked), `schedule` plans reminders
//! and watches, and a check-in looks at the watch list. Runs against a mock
//! Bot API and the scripted LLM.

mod support;

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use parking_lot::Mutex;
use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{role, serve, spawn_app_with, str_of, system_prompt, text, tool_call, TestApp};

const GOOD: &str = "123:good";

#[derive(Clone, Default)]
struct BotApi {
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl BotApi {
    fn sent(&self) -> Vec<Value> {
        self.calls.lock().iter().filter(|(m, _)| m == "sendMessage").map(|(_, b)| b.clone()).collect()
    }
}

async fn app_with_telegram() -> (TestApp, BotApi) {
    let api = BotApi::default();
    let router = Router::new()
        .route(
            "/:bot/:method",
            post(|State(api): State<BotApi>, Path((_, method)): Path<(String, String)>, Json(body): Json<Value>| async move {
                api.calls.lock().push((method.clone(), body));
                match method.as_str() {
                    "getMe" => Json(json!({"ok": true, "result": {"id": 123, "is_bot": true, "username": "kongming_bot"}})),
                    "sendMessage" => Json(json!({"ok": true, "result": {"message_id": 1}})),
                    _ => Json(json!({"ok": true, "result": true})),
                }
            }),
        )
        .with_state(api.clone());
    let url = serve(router).await;
    let app = spawn_app_with(vec![], &["--telegram-api", &url]).await;
    app.disable_reflection().await;
    app.put(
        "/api/settings",
        json!({"telegram": {"token": GOOD, "allowedUsers": "42"}, "proactive": {"quietHours": ""}}),
    )
    .await;
    (app, api)
}

/// The content of the latest tool result in a session.
async fn last_tool_result(app: &TestApp, sid: &str) -> (String, bool) {
    let msgs = app.messages(sid).await;
    let m = msgs.iter().rev().find(|m| role(m) == "tool").expect("a tool result");
    (str_of(&m["parts"][0], "content").to_string(), m["parts"][0]["isError"] == json!(true))
}

#[tokio::test(flavor = "multi_thread")]
async fn notify_user_reaches_telegram_once_per_topic() {
    let (app, api) = app_with_telegram().await;
    let sid = app.create_session().await;

    let note =
        json!({"message": "The **RTX 5070** dropped to $499: https://shop.example/5070", "topic": "RTX 5070 price"});
    app.llm.push(vec![tool_call("notify_user", note.clone()), text("told")]);
    assert_eq!(app.send_wait(&sid, "keep an eye on it").await["text"], json!("told"));
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("Sent to 1"), "{result}");
    let sent = api.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["chat_id"], json!(42));
    assert_eq!(sent[0]["disable_notification"], json!(false), "no quiet hours set");
    let body = str_of(&sent[0], "text");
    assert!(body.starts_with("💡") && body.contains("<b>RTX 5070</b>"), "{body}");

    // The message is part of the Telegram conversation, so a reply has context.
    let (chat_sid, _) = app.state.db.telegram_chat(42).await.unwrap().expect("chat session");
    let chat = app.messages(&chat_sid).await;
    assert!(
        chat.iter().any(|m| role(m) == "assistant" && str_of(&m["parts"][0], "text").contains("RTX 5070")),
        "{chat:?}"
    );
    let activity = app.get("/api/improvements").await;
    assert!(activity.as_array().unwrap().iter().any(|i| i["kind"] == json!("notification")), "{activity}");
    let settings = app.get("/api/settings").await;
    assert_eq!(settings["proactive"]["recent"][0]["topic"], json!("RTX 5070 price"), "{settings}");

    // The same topic (spelled loosely) is not sent again.
    let again = json!({"message": "Still $499.", "topic": "  rtx 5070 PRICE "});
    app.llm.push(vec![tool_call("notify_user", again), text("ok")]);
    app.send_wait(&sid, "again").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("already told"), "{result}");
    assert_eq!(api.sent().len(), 1);

    // Switched off: only what the user asked for still goes out.
    app.put("/api/settings", json!({"proactive": {"enabled": false}})).await;
    let release = json!({"message": "v2 is out.", "topic": "Release"});
    app.llm.push(vec![tool_call("notify_user", release.clone()), text("ok")]);
    app.send_wait(&sid, "news?").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("switched proactive messages off"), "{result}");
    let mut asked = release;
    asked["requested"] = json!(true);
    app.llm.push(vec![tool_call("notify_user", asked), text("ok")]);
    app.send_wait(&sid, "news?").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("Sent to 1"), "{result}");
    assert_eq!(api.sent().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_hours_send_silently_and_settings_are_checked() {
    let (app, api) = app_with_telegram().await;
    let (status, body) = app.put_raw("/api/settings", json!({"proactive": {"quietHours": "late"}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(str_of(&body, "error").contains("quietHours"), "{body}");
    let (status, _) = app.put_raw("/api/settings", json!({"proactive": {"intervalMinutes": 3}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Quiet all day except the hour before this one.
    let settings = kongming::proactive::Settings::default();
    let hour = settings.local(kongming::db::now_ms()).hour() as u32;
    let quiet = format!("{hour}-{}", (hour + 23) % 24);
    let saved = app.put("/api/settings", json!({"proactive": {"quietHours": quiet, "utcOffset": "+00:00"}})).await;
    assert_eq!(saved["proactive"]["quietHours"], json!(quiet));

    let sid = app.create_session().await;
    let note = json!({"message": "Your build finished.", "topic": "Build done"});
    app.llm.push(vec![tool_call("notify_user", note), text("ok")]);
    app.send_wait(&sid, "build it").await;
    let (result, _) = last_tool_result(&app, &sid).await;
    assert!(result.contains("silently"), "{result}");
    assert_eq!(api.sent()[0]["disable_notification"], json!(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn schedule_creates_reminders_and_watches() {
    let (app, _api) = app_with_telegram().await;
    let sid = app.create_session().await;
    let before = kongming::db::now_ms();

    let reminder = json!({"op": "create", "name": "Stand-up", "when": "in:30", "task": "Remind the user: stand-up in 5 minutes.", "notify": "always"});
    app.llm.push(vec![tool_call("schedule", reminder), text("ok")]);
    app.send_wait(&sid, "remind me").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("scheduled `Stand-up`"), "{result}");
    let jobs = app.get("/api/cron").await;
    let job = &jobs[0];
    assert!(str_of(job, "schedule").starts_with("at:"), "{job}");
    assert_eq!(job["deliverTo"], json!("42"));
    assert_eq!(job["prompt"], json!("Remind the user: stand-up in 5 minutes."));
    let next = job["nextRunAt"].as_i64().unwrap();
    assert!(next >= before + 30 * 60_000 && next < before + 31 * 60_000, "{job}");

    let watch = json!({"op": "create", "name": "Price watch", "when": "every:60", "task": "Check https://shop.example/5070 and tell the user when it is under $450."});
    app.llm.push(vec![tool_call("schedule", watch), text("ok")]);
    app.send_wait(&sid, "watch the price").await;
    let jobs = app.get("/api/cron").await;
    let watch = jobs.as_array().unwrap().iter().find(|j| j["name"] == json!("Price watch")).expect("watch job");
    assert!(watch["deliverTo"].is_null(), "a watch only messages when it finds something");
    assert!(str_of(watch, "prompt").contains("notify_user"), "{watch}");

    app.llm.push(vec![
        tool_call("schedule", json!({"op": "create", "name": "X", "when": "tomorrow", "task": "x"})),
        text("ok"),
    ]);
    app.send_wait(&sid, "bad").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("when must be"), "{result}");

    app.llm.push(vec![tool_call("schedule", json!({"op": "cancel", "name": "stand-up"})), text("ok")]);
    app.send_wait(&sid, "cancel it").await;
    let jobs = app.get("/api/cron").await;
    assert_eq!(jobs.as_array().unwrap().len(), 1, "{jobs}");
    app.llm.push(vec![tool_call("schedule", json!({"op": "list"})), text("ok")]);
    app.send_wait(&sid, "what is planned?").await;
    let (result, _) = last_tool_result(&app, &sid).await;
    assert!(result.contains("Price watch") && !result.contains("Stand-up"), "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_in_reads_the_watch_list_and_cleans_up_after_itself() {
    let (app, api) = app_with_telegram().await;
    app.state.db.save_memory(None, "watch", "Tell the user when kongming v2 is released", &[], 3).await.unwrap();

    // Nothing worth sending: no message, and the scheduled check-in leaves no conversation behind.
    app.llm.push(vec![text("nothing to report")]);
    assert_eq!(kongming::proactive::check_in(&app.state, false).await.unwrap(), 0);
    let req = app.llm.requests().last().cloned().expect("a model request");
    let sys = system_prompt(&req);
    assert!(sys.contains("proactive check-in"), "{sys}");
    let user = req["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == json!("user")).unwrap().clone();
    let user = user["content"].to_string();
    assert!(user.contains("Watch list") && user.contains("kongming v2"), "{user}");
    assert!(api.sent().is_empty());
    let sessions = app.get("/api/sessions").await;
    assert!(!sessions.to_string().contains("\"proactive\""), "{sessions}");

    // Something to say: it is sent and the conversation stays.
    let note = json!({"message": "kongming v2 is out: https://example.com/v2", "topic": "kongming v2 released", "requested": true});
    app.llm.push(vec![tool_call("notify_user", note), text("sent the v2 release")]);
    assert_eq!(kongming::proactive::check_in(&app.state, false).await.unwrap(), 1);
    assert_eq!(api.sent().len(), 1);
    let sessions = app.get("/api/sessions").await;
    assert!(sessions.to_string().contains("Check-in"), "{sessions}");
    let activity = app.get("/api/improvements").await;
    assert!(activity.as_array().unwrap().iter().any(|i| i["kind"] == json!("check_in")), "{activity}");
}
