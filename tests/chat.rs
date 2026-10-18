//! A plain chat turn over the HTTP API against the OpenAI-style mock.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{part_types, role, spawn_app, str_of, system_prompt, text, tool_call, within};

#[tokio::test(flavor = "multi_thread")]
async fn simple_turn_round_trips_through_the_mock() {
    let app = spawn_app(vec![text("hello there")]).await;
    app.disable_reflection().await;

    let session = app.post("/api/sessions", json!({})).await;
    let sid = str_of(&session, "id").to_string();
    assert!(!sid.is_empty());
    assert_eq!(session["channel"], json!("web"));
    assert_eq!(session["title"], json!(""));
    let agent = app.default_agent().await;
    assert_eq!(session["agentId"], json!(agent.id));

    let reply = app.send_wait(&sid, "hi").await;
    assert_eq!(reply["text"], json!("hello there"));
    assert_eq!(reply["stopReason"], json!("stop"));
    assert!(reply["error"].is_null());
    assert_eq!(reply["agent"]["handle"], json!("kongming"));

    // History.
    let detail = app.get(&format!("/api/sessions/{sid}")).await;
    assert_eq!(detail["session"]["title"], json!("hi"), "title is taken from the first message");
    assert_eq!(detail["running"], json!(false));
    assert_eq!(detail["agent"]["id"], json!(agent.id));
    let msgs = detail["messages"].as_array().expect("messages");
    assert_eq!(msgs.len(), 2);
    assert_eq!(role(&msgs[0]), "user");
    assert_eq!(msgs[0]["parts"], json!([{"type": "text", "text": "hi"}]));
    assert_eq!(msgs[0]["seq"], json!(1));
    assert_eq!(role(&msgs[1]), "assistant");
    assert_eq!(part_types(&msgs[1]), vec!["text".to_string()]);
    assert_eq!(msgs[1]["parts"][0]["text"], json!("hello there"));
    assert_eq!(msgs[1]["agentId"], json!(agent.id));
    assert_eq!(msgs[1]["seq"], json!(2));

    // What the model was sent.
    let reqs = app.llm.requests();
    assert!(!reqs.is_empty(), "the mock saw no request");
    let first = &reqs[0];
    assert_eq!(first["model"], json!("test-model"));
    assert_eq!(first["stream"], json!(true));
    let sys = system_prompt(first);
    assert!(sys.contains("# Identity"), "system prompt: {sys}");
    assert!(sys.contains("(@kongming)"), "system prompt names the agent handle: {sys}");
    assert!(sys.contains("# Skills"), "system prompt has a skills section: {sys}");
    assert!(sys.contains("kongming web app"), "web channel conventions: {sys}");
    let messages = first["messages"].as_array().unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last["role"], json!("user"));
    assert_eq!(last["content"], json!("hi"));
    // Built-in tools are offered.
    let names = support::tool_names(first);
    assert!(names.contains(&"current_time".to_string()), "tools: {names:?}");
    assert!(names.contains(&"skill_manage".to_string()), "tools: {names:?}");
    assert!(!names.contains(&"delegate_task".to_string()), "no reports yet: {names:?}");

    // The session list shows it with the agent handle, and usage was recorded.
    let list = app.get("/api/sessions").await;
    let listed = list.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["agentHandle"], json!("kongming"));
    assert_eq!(listed[0]["title"], json!("hi"));
    let usage = app.get("/api/usage").await;
    assert_eq!(usage["day"]["requests"], json!(1));
    assert_eq!(usage["day"]["promptTokens"], json!(10));
}

#[tokio::test(flavor = "multi_thread")]
async fn second_turn_sends_the_history() {
    let app = spawn_app(vec![text("first"), text("second")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "one").await["text"], json!("first"));
    assert_eq!(app.send_wait(&sid, "two").await["text"], json!("second"));
    let reqs = app.llm.requests();
    assert!(reqs.len() >= 2);
    let msgs = reqs[1]["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap_or("")).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert_eq!(msgs[2]["content"], json!("first"));
    assert_eq!(msgs[3]["content"], json!("two"));
    assert_eq!(app.messages(&sid).await.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_task_is_compacted_between_tool_rounds() {
    // A one-token budget: compaction is due as soon as enough rounds pile up.
    let mut script: Vec<_> = (0..6).map(|_| tool_call("current_time", json!({}))).collect();
    script.push(text("SUMMARY: read the clock six times"));
    script.push(text("done"));
    let app = support::spawn_app_with(script, &["--context-tokens", "1"]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "keep reading the clock").await["text"], json!("done"));

    // Six tool rounds, the compaction, then the last round on the compacted history.
    let reqs = app.llm.requests();
    assert_eq!(reqs.len(), 8);
    assert!(system_prompt(&reqs[6]).contains("Summarize the conversation"), "{}", reqs[6]);
    assert!(reqs[6].to_string().contains("keep reading the clock"));
    let msgs = reqs[7]["messages"].as_array().unwrap();
    // System prompt, the summary restating the request, the 12 newest messages
    // (a tool call first, so no acknowledgement in between).
    assert_eq!(msgs.len(), 14, "{msgs:?}");
    let summary = msgs[1]["content"].as_str().unwrap_or("");
    assert!(summary.contains("SUMMARY: read the clock six times"), "{summary}");
    assert!(summary.contains("[The request you are working on now]\nkeep reading the clock"), "{summary}");
    assert_eq!(msgs[2]["role"], json!("assistant"));
    assert!(msgs[2]["tool_calls"].is_array(), "{}", msgs[2]);

    let s = app.state.db.session(&sid).await.unwrap().unwrap();
    assert_eq!(s.summary_upto, 1, "only the request was folded");
    // Nothing is lost from the stored conversation.
    assert_eq!(app.messages(&sid).await.len(), 14);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_validates_input_and_session() {
    let app = spawn_app(vec![]).await;
    let sid = app.create_session().await;
    let (status, body) = app.post_raw(&format!("/api/sessions/{sid}/messages?wait=true"), json!({"text": "  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = app.post_raw("/api/sessions/nope/messages?wait=true", json!({"text": "hi"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.post_raw("/api/sessions", json!({"agentId": "ghost"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Without wait the request is accepted and the turn runs in the background.
    app.llm.push(vec![text("bg")]);
    let v = app.post(&format!("/api/sessions/{sid}/messages"), json!({"text": "later"})).await;
    assert_eq!(v["accepted"], json!(true));
    let app_ref = &app;
    let sid_ref = sid.as_str();
    support::poll_until("background turn", || async move {
        let msgs = app_ref.messages(sid_ref).await;
        if msgs.len() >= 2 && !app_ref.state.is_running(sid_ref) {
            Some(())
        } else {
            None
        }
    })
    .await;
    let msgs = app.messages(&sid).await;
    assert_eq!(msgs[1]["parts"][0]["text"], json!("bg"));
}

#[tokio::test(flavor = "multi_thread")]
async fn session_management_endpoints() {
    let app = spawn_app(vec![text("a")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    app.send_wait(&sid, "hello").await;

    let patched = app.client.patch(app.url(&format!("/api/sessions/{sid}"))).json(&json!({"title": "Renamed"})).send();
    let r = within(patched).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["title"], json!("Renamed"));

    let stopped = app.post(&format!("/api/sessions/{sid}/stop"), json!({})).await;
    assert_eq!(stopped["stopped"], json!(false), "nothing was running");

    app.post(&format!("/api/sessions/{sid}/clear"), json!({})).await;
    assert!(app.messages(&sid).await.is_empty());

    app.delete(&format!("/api/sessions/{sid}")).await;
    let r = within(app.client.get(app.url(&format!("/api/sessions/{sid}"))).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert!(app.get("/api/sessions").await.as_array().unwrap().is_empty());
}
