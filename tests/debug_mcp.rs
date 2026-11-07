//! The read-only debug MCP server: token lifecycle, authentication and redaction.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{spawn_app, within, TestApp};

async fn rpc(app: &TestApp, token: &str, body: Value) -> Value {
    let r = within(app.plain.post(app.url("/mcp")).bearer_auth(token).json(&body).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
    r.json().await.unwrap()
}

fn tool_text(reply: &Value) -> &str {
    assert_ne!(reply["result"]["isError"], json!(true), "{reply}");
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn debug_mcp_is_off_until_a_token_exists_and_never_shows_secrets() {
    let app = spawn_app(vec![]).await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});

    let r = within(app.plain.post(app.url("/mcp")).bearer_auth("anything").json(&ping).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    let llm = json!({
        "kind": "openai",
        "baseUrl": format!("{}/v1", app.llm.url),
        "apiKey": "sk-fake-secret-123",
        "model": "test-model",
    });
    app.state.db.set_setting("llm", &llm).await.unwrap();

    let made = app.post("/api/debug-mcp/token", json!({})).await;
    let token = made["token"].as_str().unwrap().to_string();
    let url = made["url"].as_str().unwrap();
    assert!(url.ends_with("/mcp"));
    assert!(made["command"].as_str().unwrap().contains(&format!("Authorization: Bearer {token}")));
    assert_eq!(app.get("/api/debug-mcp").await["enabled"], json!(true));

    // Only the hash is stored.
    let stored = app.state.db.setting("debug_mcp_token_hash").await.unwrap().unwrap();
    assert_eq!(stored, json!(kongming::web::debug_mcp::hash_token(&token)));

    // Wrong bearer, no bearer, and the dashboard cookie alone are refused.
    let r = within(app.plain.post(app.url("/mcp")).bearer_auth("wrong").json(&ping).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = within(app.client.post(app.url("/mcp")).json(&ping).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app.plain.post(app.url("/mcp")).bearer_auth(&token).header("origin", "https://evil.example");
    let r = within(r.json(&ping).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = within(app.plain.get(app.url("/mcp")).bearer_auth(&token).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);

    let hello = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}});
    let init = rpc(&app, &token, hello).await;
    assert_eq!(init["result"]["protocolVersion"], json!("2025-03-26"));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("kongming"));
    let r = within(
        app.plain
            .post(app.url("/mcp"))
            .bearer_auth(&token)
            .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);

    let list = rpc(&app, &token, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["get_status", "get_config", "get_logs"]);
    let unknown = rpc(&app, &token, json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).await;
    assert_eq!(unknown["error"]["code"], json!(-32601));

    let call = |name: &str, args: Value| json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": name, "arguments": args}});
    let config = rpc(&app, &token, call("get_config", json!({}))).await;
    let text = tool_text(&config);
    assert!(text.contains("[redacted, 18 chars]"), "{text}");
    assert!(!text.contains("sk-fake-secret-123"));
    assert!(!text.contains(&kongming::web::debug_mcp::hash_token(&token)));

    let status = rpc(&app, &token, call("get_status", json!({}))).await;
    let status: Value = serde_json::from_str(tool_text(&status)).unwrap();
    assert_eq!(status["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(status["llm"]["model"], json!("test-model"));
    assert!(!status.to_string().contains("sk-fake-secret-123"));

    let logs = rpc(&app, &token, call("get_logs", json!({"limit": 5, "level": "warn"}))).await;
    tool_text(&logs);
    let bad = rpc(&app, &token, call("get_logs", json!({"level": "loud"}))).await;
    assert_eq!(bad["result"]["isError"], json!(true));

    app.delete("/api/debug-mcp/token").await;
    let r = within(app.plain.post(app.url("/mcp")).bearer_auth(&token).json(&ping).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}
