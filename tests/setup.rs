//! Paste it and Kongming sets it up: keys in a person's message go to the
//! vault before the model or the history sees them, and the lead agent's
//! `setup` tool configures them by reference through the dashboard's own
//! code paths. Runs against the scripted LLM and the MCP mock.

mod support;

use kongming::agent::TurnOptions;
use serde_json::{json, Value};
use support::{role, spawn_app, str_of, text, tool_call, tool_names, McpMock, TestApp};

const KEY: &str = "tk_example_not_a_real_key_42";

fn user_text(msgs: &[Value]) -> String {
    let m = msgs.iter().find(|m| role(m) == "user").expect("a user message");
    str_of(&m["parts"][0], "text").to_string()
}

async fn last_tool_result(app: &TestApp, sid: &str) -> (String, bool) {
    let msgs = app.messages(sid).await;
    let m = msgs.iter().rev().find(|m| role(m) == "tool").expect("a tool result");
    (str_of(&m["parts"][0], "content").to_string(), m["parts"][0]["isError"] == json!(true))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_key_never_reaches_the_model_and_is_configured_by_reference() {
    let app = spawn_app(vec![]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;

    app.llm.push(vec![text("Got it.")]);
    app.send_wait(&sid, &format!("here is my toku api key: {KEY} please use it")).await;
    let stored = user_text(&app.messages(&sid).await);
    assert!(!stored.contains(KEY), "{stored}");
    assert!(stored.contains("[secret:") && stored.contains("…y_42]"), "{stored}");
    let req = app.llm.requests().last().cloned().unwrap();
    assert!(!req.to_string().contains(KEY), "the model never sees the key");
    assert!(tool_names(&req).iter().any(|n| n == "setup"), "the lead can set things up with the user");
    assert!(support::system_prompt(&req).contains("# Setup"));

    // The model hands the reference to setup; the key is saved for toku.
    let start = stored.find("[secret:").unwrap();
    let reference = &stored[start..start + stored[start..].find(']').unwrap() + 1];
    app.llm.push(vec![
        tool_call("setup", json!({"op": "earn", "provider": "toku", "api_key": reference})),
        text("Saved your toku key."),
    ]);
    app.send_wait(&sid, "yes, for toku").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("toku") && result.contains("key set"), "{result}");
    assert!(!result.contains(KEY));
    let earn = app.get("/api/earn").await;
    assert_eq!(earn["settings"]["providers"]["toku"]["keySet"], json!(true), "{earn}");
    let saved = kongming::earn::settings(&app.state).await;
    assert_eq!(saved.providers["toku"].api_key.as_deref(), Some(KEY));
    assert!(saved.providers["toku"].enabled);

    // Nothing the dashboard or the model reads carries the value.
    assert!(app.llm.requests().iter().all(|r| !r.to_string().contains(KEY)));
    assert!(!app.get("/api/settings").await.to_string().contains(KEY));
    assert!(!app.get(&format!("/api/sessions/{sid}")).await.to_string().contains(KEY));
    let activity = app.get("/api/improvements").await;
    assert!(activity.as_array().unwrap().iter().any(|i| i["kind"] == json!("setup")), "{activity}");

    // An unknown reference is an error the model can act on.
    app.llm.push(vec![
        tool_call("setup", json!({"op": "earn", "provider": "toku", "api_key": "secret:nope1234"})),
        text("Please paste it again."),
    ]);
    app.send_wait(&sid, "again").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(err && result.contains("paste it again"), "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_mcp_config_adds_remote_servers_and_refuses_local_ones() {
    let mcp = McpMock::start(true).await;
    let app = spawn_app(vec![]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let config = json!({"mcpServers": {
        "echo": {"type": "http", "url": mcp.endpoint(), "headers": {"Authorization": "Bearer good"}},
        "files": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"]}
    }})
    .to_string();
    app.llm.push(vec![tool_call("setup", json!({"op": "mcp_add", "config": config})), text("Connected echo.")]);
    app.send_wait(&sid, "add these").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err, "{result}");
    assert!(result.contains("echo") && result.contains("connected") && result.contains("1 tools"), "{result}");
    assert!(result.contains("files") && result.contains("stdio"), "{result}");

    let servers = app.get("/api/mcp").await;
    let servers = servers.as_array().unwrap();
    assert_eq!(servers.len(), 1, "{servers:?}");
    assert_eq!(servers[0]["name"], json!("echo"));
    assert_eq!(servers[0]["authType"], json!("token"));
    assert_eq!(servers[0]["status"], json!("connected"));
    assert_eq!(servers[0]["headers"], json!({}), "the bearer token is kept as the token, not a header");

    // Pasting it again replaces the server instead of failing on the name.
    app.llm.push(vec![
        tool_call("setup", json!({"op": "mcp_add", "url": mcp.endpoint(), "name": "echo", "token": "good"})),
        text("Updated."),
    ]);
    app.send_wait(&sid, "once more").await;
    let (result, err) = last_tool_result(&app, &sid).await;
    assert!(!err && result.contains("updated"), "{result}");
    assert_eq!(app.get("/api/mcp").await.as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_is_only_for_the_lead_talking_with_the_user() {
    let app = spawn_app(vec![]).await;
    app.disable_reflection().await;
    let lead = app.default_agent().await;
    let report = app.hire("researcher", "Research analyst", Some(&lead.id)).await;

    // A delegated task: the report does not get it.
    let sid = app.create_session().await;
    app.llm.push(vec![
        tool_call("delegate_task", json!({"agent": "researcher", "task": "find x"})),
        text("child result"),
        text("done"),
    ]);
    app.send_wait(&sid, "find x").await;
    let reqs = app.llm.requests();
    assert!(tool_names(&reqs[0]).iter().any(|n| n == "setup"));
    assert!(!tool_names(&reqs[1]).iter().any(|n| n == "setup"), "{:?}", tool_names(&reqs[1]));
    assert!(support::system_prompt(&reqs[1]).contains(&report.handle));

    // A scheduled run of the lead itself: nobody is there to paste anything.
    let cron = app.state.db.create_session(&lead.id, "cron", None, None, "job").await.unwrap();
    app.llm.push(vec![text("ran")]);
    kongming::agent::run_turn(
        app.state.clone(),
        cron,
        "check the news".into(),
        TurnOptions { depth: 0, no_reflect: true, agent_override: Some(lead.clone()) },
    )
    .await
    .unwrap();
    let req = app.llm.requests().last().cloned().unwrap();
    assert!(!tool_names(&req).iter().any(|n| n == "setup"));
    assert!(!support::system_prompt(&req).contains("# Setup"));
}
