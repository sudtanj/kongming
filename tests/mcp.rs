//! Remote MCP servers over Streamable HTTP: registration, discovery, direct
//! calls, agent use and authentication failures.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{spawn_app, str_of, system_prompt, text, tool_call, tool_names, McpMock};

#[tokio::test(flavor = "multi_thread")]
async fn register_refresh_and_call_an_mcp_server() {
    let mcp = McpMock::start(true).await;
    let app = spawn_app(vec![tool_call("mcp__echo__echo", json!({"text": "via agent"})), text("ok")]).await;
    app.disable_reflection().await;

    let created = app
        .post("/api/mcp", json!({"name": "Echo", "url": mcp.endpoint(), "transport": "http", "authType": "token", "token": "good"}))
        .await;
    let id = str_of(&created, "id").to_string();
    assert_eq!(created["hasToken"], json!(true));
    assert!(created.get("token").is_none(), "token is never exposed");
    assert_eq!(created["authType"], json!("token"));

    let refreshed = app.post(&format!("/api/mcp/{id}/refresh"), json!({})).await;
    assert_eq!(refreshed["status"], json!("connected"), "{refreshed}");
    assert_eq!(refreshed["toolCount"], json!(1));
    assert_eq!(refreshed["tools"][0]["name"], json!("echo"));
    assert!(refreshed.get("error").is_none(), "{refreshed}");
    assert!(refreshed["lastError"].is_null());

    let list = app.get("/api/mcp").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["status"], json!("connected"));

    // The handshake happened: initialize, notifications/initialized, tools/list.
    let methods: Vec<String> = mcp.calls().iter().map(|c| str_of(c, "method").to_string()).collect();
    assert!(methods.contains(&"initialize".to_string()), "{methods:?}");
    assert!(methods.contains(&"notifications/initialized".to_string()), "{methods:?}");
    assert!(methods.contains(&"tools/list".to_string()), "{methods:?}");

    // Direct call.
    let called = app.post(&format!("/api/mcp/{id}/call"), json!({"tool": "echo", "args": {"text": "hi"}})).await;
    assert_eq!(called["content"], json!("echo: hi"));
    assert_eq!(called["isError"], json!(false));
    let (status, body) = app.post_raw(&format!("/api/mcp/{id}/call"), json!({"args": {}})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // Through the agent.
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "echo something").await["text"], json!("ok"));
    let msgs = app.messages(&sid).await;
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[1]["parts"][0]["name"], json!("mcp__echo__echo"));
    let result = &msgs[2]["parts"][0];
    assert_eq!(result["type"], json!("tool_result"));
    assert_eq!(result["content"], json!("echo: via agent"));
    assert_eq!(result["isError"], json!(false));
    let reqs = app.llm.requests();
    let names = tool_names(&reqs[0]);
    assert!(names.contains(&"mcp__echo__echo".to_string()), "{names:?}");
    let spec = reqs[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == json!("mcp__echo__echo"))
        .cloned()
        .unwrap();
    assert_eq!(spec["function"]["parameters"]["properties"]["text"]["type"], json!("string"));
    assert!(str_of(&spec["function"], "description").starts_with("[Echo]"));
    let sys = system_prompt(&reqs[0]);
    assert!(sys.contains("# Connected services"), "{sys}");
    assert!(sys.contains("Echo"), "{sys}");
    let last_call = mcp.calls().into_iter().filter(|c| c["method"] == json!("tools/call")).next_back().unwrap();
    assert_eq!(last_call["params"]["arguments"]["text"], json!("via agent"));

    // Disabling the server hides its tools and refresh reports it.
    app.put(&format!("/api/mcp/{id}"), json!({"enabled": false})).await;
    let refreshed = app.post(&format!("/api/mcp/{id}/refresh"), json!({})).await;
    assert_eq!(refreshed["status"], json!("disabled"));
    assert!(app.state.mcp.tools(None).await.unwrap().is_empty());
    app.delete(&format!("/api/mcp/{id}")).await;
    assert!(app.get("/api/mcp").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_token_is_reported_as_unauthorized() {
    let mcp = McpMock::start(true).await;
    let app = spawn_app(vec![]).await;
    let created = app
        .post("/api/mcp", json!({"name": "Bad", "url": mcp.endpoint(), "transport": "http", "authType": "token", "token": "nope"}))
        .await;
    let id = str_of(&created, "id").to_string();
    let refreshed = app.post(&format!("/api/mcp/{id}/refresh"), json!({})).await;
    assert_eq!(refreshed["status"], json!("unauthorized"), "{refreshed}");
    assert_eq!(refreshed["toolCount"], json!(0));
    assert!(str_of(&refreshed, "error").contains("unauthorized"), "{refreshed}");
    assert!(str_of(&refreshed, "lastError").contains("401"), "{refreshed}");

    // A direct call fails too.
    let (status, body) = app.post_raw(&format!("/api/mcp/{id}/call"), json!({"tool": "echo", "args": {}})).await;
    assert!(!status.is_success(), "{body}");

    // Fixing the token makes it work.
    app.put(&format!("/api/mcp/{id}"), json!({"token": "good"})).await;
    let refreshed = app.post(&format!("/api/mcp/{id}/refresh"), json!({})).await;
    assert_eq!(refreshed["status"], json!("connected"), "{refreshed}");
    assert_eq!(refreshed["toolCount"], json!(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn server_without_auth_and_input_validation() {
    let mcp = McpMock::start(false).await;
    let app = spawn_app(vec![]).await;

    let (status, body) = app.post_raw("/api/mcp", json!({"name": "NoUrl"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = app.post_raw("/api/mcp", json!({"name": "Stdio", "url": "stdio://local"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = app.post_raw("/api/mcp", json!({"name": "X", "url": mcp.endpoint(), "transport": "grpc"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let created = app.post("/api/mcp", json!({"name": "Open", "url": mcp.endpoint()})).await;
    assert_eq!(created["authType"], json!("none"));
    assert_eq!(created["hasToken"], json!(false));
    let id = str_of(&created, "id").to_string();
    let (status, body) = app.post_raw("/api/mcp", json!({"name": "Open", "url": mcp.endpoint()})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate name: {body}");

    let refreshed = app.post(&format!("/api/mcp/{id}/refresh"), json!({})).await;
    assert_eq!(refreshed["status"], json!("connected"), "{refreshed}");
    let tools = app.state.mcp.tools(None).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec.name, "mcp__open__echo");
    let found = app.state.mcp.find_tool("mcp__open__echo").await.unwrap().expect("tool found");
    assert_eq!(found.server_id, id);
    assert!(app.state.mcp.find_tool("mcp__other__echo").await.unwrap().is_none());

    // Unknown tool on the server is surfaced as an error.
    let (status, body) = app.post_raw(&format!("/api/mcp/{id}/call"), json!({"tool": "nope", "args": {}})).await;
    assert!(!status.is_success(), "{body}");
    assert!(str_of(&body, "error").contains("unknown tool"), "{body}");

    let missing: Value = app.post_raw("/api/mcp/doesnotexist/refresh", json!({})).await.1;
    assert!(missing.get("error").is_some(), "{missing}");
}
