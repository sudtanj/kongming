//! Small and local models through the agent loop: calls written as text,
//! inline reasoning, near-miss tool names, repeated failures, nudges and the
//! closing summary at the step limit; small-model mode (sampling, fewer
//! tools, MCP tools behind a search), memory recall and deduplication, and
//! the recent conversations in the prompt.

mod support;

use serde_json::{json, Value};
use support::{cut, part_types, role, spawn_app, spawn_app_with, str_of, text, tool_call};

fn tool_results(msgs: &[Value]) -> Vec<&Value> {
    msgs.iter().filter(|m| role(m) == "tool").map(|m| &m["parts"][0]).collect()
}

/// The `messages` of a recorded request.
fn sent(req: &Value) -> Vec<Value> {
    req["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_written_as_text_is_run_and_left_out_of_the_reply() {
    let call = r#"Let me check.
<tool_call>{"name": "current_time", "arguments": {}}</tool_call>"#;
    let app = spawn_app(vec![text(call), text("It is noon.")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "what time is it?").await;
    assert_eq!(reply["text"], json!("It is noon."), "{reply}");

    let msgs = app.messages(&sid).await;
    let roles: Vec<&str> = msgs.iter().map(role).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
    assert_eq!(part_types(&msgs[1]), vec!["text".to_string(), "tool_call".to_string()]);
    assert_eq!(msgs[1]["parts"][0]["text"], json!("Let me check."));
    assert_eq!(msgs[1]["parts"][1]["name"], json!("current_time"));
    assert!(str_of(&msgs[1]["parts"][1], "id").starts_with("call_text_"), "{}", msgs[1]);
    let result = tool_results(&msgs)[0];
    assert_eq!(result["isError"], json!(false), "{result}");
    assert!(str_of(result, "content").contains("UTC"), "{result}");
    assert!(!json!(msgs).to_string().contains("<tool_call>"), "the tag is not stored");

    // The model sees a real tool call in the history.
    let second = sent(&app.llm.requests()[1]);
    let assistant = second.iter().find(|m| role(m) == "assistant").expect("assistant message");
    assert_eq!(assistant["content"], json!("Let me check."));
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], json!("current_time"));
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_think_blocks_are_reasoning_not_reply() {
    let app = spawn_app(vec![text("<think>secret plan</think>Hello")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "hi").await;
    assert_eq!(reply["text"], json!("Hello"), "{reply}");
    let msgs = app.messages(&sid).await;
    assert_eq!(part_types(&msgs[1]), vec!["thinking".to_string(), "text".to_string()]);
    assert_eq!(msgs[1]["parts"][0]["text"], json!("secret plan"));
    assert_eq!(msgs[1]["parts"][1]["text"], json!("Hello"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_misspelled_tool_name_is_repaired() {
    let app = spawn_app(vec![tool_call("CurrentTime", json!({})), text("done")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "time?").await["text"], json!("done"));
    let msgs = app.messages(&sid).await;
    assert_eq!(msgs[1]["parts"][0]["name"], json!("current_time"));
    let result = tool_results(&msgs)[0];
    assert_eq!(result["isError"], json!(false), "{result}");
    assert_eq!(result["name"], json!("current_time"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_failing_call_is_flagged_then_not_run() {
    let missing = || tool_call("skill_view", json!({"name": "nope"}));
    let app = spawn_app(vec![missing(), missing(), missing(), missing(), text("giving up")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "show the nope skill").await["text"], json!("giving up"));
    let msgs = app.messages(&sid).await;
    let results = tool_results(&msgs);
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|r| r["isError"] == json!(true)));
    let content = |i: usize| str_of(results[i], "content").to_string();
    assert!(content(0).contains("no skill named 'nope'"), "{}", content(0));
    assert!(!content(0).contains("You already called"), "{}", content(0));
    assert!(content(1).contains("You already called skill_view with these arguments"), "{}", content(1));
    assert!(content(3).contains("not run"), "{}", content(3));
    assert!(!content(3).contains("no skill named"), "the fourth call never ran: {}", content(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn rounds_of_only_invalid_calls_end_the_turn() {
    let bad = || tool_call("nope_one", json!({}));
    let app = spawn_app(vec![bad(), bad(), bad(), text("never")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "do it").await;
    assert_eq!(reply["stopReason"], json!("invalid_tool_calls"), "{reply}");
    assert!(str_of(&reply, "text").contains("all invalid"), "{reply}");
    assert_eq!(app.llm.request_count(), 3);
    // The unknown-tool error lists what is available.
    let msgs = app.messages(&sid).await;
    let first = str_of(tool_results(&msgs)[0], "content").to_string();
    assert!(first.contains("unknown tool nope_one") && first.contains("current_time"), "{first}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_reply_is_retried_with_a_nudge() {
    let app = spawn_app(vec![text(""), text("hi")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "hello").await["text"], json!("hi"));
    let retry = sent(&app.llm.requests()[1]);
    let last = retry.last().expect("messages");
    assert_eq!(role(last), "user");
    assert_eq!(last["content"], json!("Reply to the user now, or call one of your tools."));
    // The nudge is not part of the stored conversation.
    assert_eq!(app.messages(&sid).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reply_cut_at_the_output_limit_is_continued() {
    let app = spawn_app(vec![cut("Hello wor"), text("ld!")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "greet me").await["text"], json!("Hello world!"));
    let msgs = sent(&app.llm.requests()[1]);
    let n = msgs.len();
    assert_eq!(msgs[n - 2]["content"], json!("Hello wor"));
    assert_eq!(msgs[n - 1]["content"], json!("Continue exactly where you left off. Do not restart."));
    let stored = app.messages(&sid).await;
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[1]["parts"][0]["text"], json!("Hello world!"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_step_limit_ends_with_a_summary_from_the_model() {
    let replies = vec![tool_call("current_time", json!({})), text("I read the clock; nothing is left to do.")];
    let app = spawn_app_with(replies, &["--max-iterations", "1", "--auto-continue", "0"]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "check the time").await;
    assert_eq!(reply["stopReason"], json!("max_iterations"), "{reply}");
    assert_eq!(reply["text"], json!("I read the clock; nothing is left to do."));
    let reqs = app.llm.requests();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[1].get("tools").is_none(), "the summary request offers no tools");
    let msgs = sent(&reqs[1]);
    assert!(msgs.iter().all(|m| role(m) != "tool" && m.get("tool_calls").is_none()), "{msgs:?}");
    assert!(str_of(msgs.last().unwrap(), "content").contains("You have used all 1 steps"));
}

/// The scripted app with its model switched to small-model mode and an 8k
/// window from the settings API, the way the dashboard saves it.
async fn small_app(replies: Vec<support::ScriptedReply>) -> support::TestApp {
    let app = spawn_app(replies).await;
    app.disable_reflection().await;
    let saved = app.put("/api/settings", json!({"llm": {"smallModel": true, "contextWindow": 8192}})).await;
    assert_eq!(saved["llm"]["smallModel"], json!(true), "{saved}");
    assert_eq!(saved["llm"]["contextWindow"], json!(8192), "{saved}");
    app
}

#[tokio::test(flavor = "multi_thread")]
async fn a_small_model_gets_cool_sampling_serial_calls_and_fewer_tools() {
    let app = small_app(vec![text("Hello!")]).await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "hello").await["text"], json!("Hello!"));
    let req = app.llm.requests()[0].clone();
    let temperature = req["temperature"].as_f64().expect("a temperature");
    assert!((temperature - 0.3).abs() < 1e-6, "{req}");
    assert_eq!(req["parallel_tool_calls"], json!(false));
    assert_eq!(req["max_tokens"], json!(2048), "a quarter of the window");
    let names = support::tool_names(&req);
    for gone in ["current_time", "list_agents", "list_sessions", "read_session", "setup"] {
        assert!(!names.contains(&gone.to_string()), "{gone}: {names:?}");
    }
    assert!(names.contains(&"memory".to_string()), "{names:?}");
    let sys = support::system_prompt(&req);
    assert!(sys.contains("Every reply either calls a tool or gives the user a final answer"), "{sys}");
    assert!(sys.contains("ask one short question first"), "{sys}");
    assert!(!sys.contains("# Setup"), "{sys}");

    // Talk of setting something up brings the setup tool back.
    app.llm.push(vec![text("Sure.")]);
    app.send_wait(&sid, "please connect my Telegram bot").await;
    let req = app.llm.requests().last().cloned().unwrap();
    assert!(support::tool_names(&req).contains(&"setup".to_string()));
    assert!(support::system_prompt(&req).contains("# Setup"));
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_tools_of_a_small_model_are_found_and_called_through_the_bridge() {
    let mcp = support::McpMock::start(false).await;
    let app = small_app(vec![
        tool_call("tool_search", json!({"query": "echo text"})),
        tool_call("tool_call", json!({"name": "echo", "arguments": {"text": "via bridge"}})),
        text("done"),
    ])
    .await;
    let created = app.post("/api/mcp", json!({"name": "Echo", "url": mcp.endpoint(), "transport": "http"})).await;
    app.post(&format!("/api/mcp/{}/refresh", str_of(&created, "id")), json!({})).await;

    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "echo something").await["text"], json!("done"));
    let first = app.llm.requests()[0].clone();
    let names = support::tool_names(&first);
    assert!(names.contains(&"tool_search".to_string()) && names.contains(&"tool_call".to_string()), "{names:?}");
    assert!(!names.contains(&"mcp__echo__echo".to_string()), "deferred: {names:?}");
    let sys = support::system_prompt(&first);
    assert!(sys.contains("Echo (1 tool)") && sys.contains("`tool_search`"), "{sys}");

    let results = tool_results(&app.messages(&sid).await).into_iter().cloned().collect::<Vec<_>>();
    assert_eq!(results.len(), 2);
    let found = str_of(&results[0], "content");
    assert!(found.contains("- mcp__echo__echo: [Echo] Echo the text back"), "{found}");
    assert!(found.contains("arguments: text (string)"), "{found}");
    // A near-miss name through tool_call reaches the MCP server.
    assert_eq!(results[1]["name"], json!("tool_call"));
    assert_eq!(results[1]["isError"], json!(false), "{}", results[1]);
    assert_eq!(results[1]["content"], json!("echo: via bridge"));
    let call = mcp.calls().into_iter().rfind(|c| c["method"] == json!("tools/call")).expect("a tools/call");
    assert_eq!(call["params"]["name"], json!("echo"));
    assert_eq!(call["params"]["arguments"]["text"], json!("via bridge"));
}

#[tokio::test(flavor = "multi_thread")]
async fn memories_sharing_words_with_the_message_are_recalled() {
    let app = small_app(vec![text("Sunday night."), text("Hi.")]).await;
    let db = &app.state.db;
    for i in 0..10 {
        db.save_memory(None, "fact", &format!("Filler fact number {i}"), &[], 5).await.unwrap();
    }
    db.save_memory(None, "fact", "The NAS backup runs every Sunday night", &["storage".into()], 1).await.unwrap();

    let sid = app.create_session().await;
    app.send_wait(&sid, "When does the NAS backup run?").await;
    let sys = support::system_prompt(&app.llm.requests()[0]);
    assert!(sys.contains("- [fact] The NAS backup runs every Sunday night"), "{sys}");
    // Unrelated messages get the most important memories only (8 for a small model).
    app.send_wait(&sid, "hello").await;
    let sys = support::system_prompt(&app.llm.requests()[1]);
    assert!(!sys.contains("NAS backup"), "{sys}");
    assert_eq!(sys.matches("Filler fact number").count(), 8, "{sys}");
    // The Memory section sits after the stable sections.
    assert!(sys.find("# Memory").unwrap() > sys.find("# Using tools").unwrap(), "{sys}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeated_memory_is_refused_and_can_be_updated_instead() {
    let add = |content: &str| tool_call("memory", json!({"action": "add", "content": content, "kind": "preference"}));
    let app = spawn_app(vec![add("User likes green tea"), add("user likes green tea!"), text("noted")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    app.send_wait(&sid, "I like green tea").await;
    let memories = app.get("/api/memories").await;
    assert_eq!(memories.as_array().unwrap().len(), 1, "{memories}");
    let id = str_of(&memories[0], "id").to_string();
    let results = tool_results(&app.messages(&sid).await).into_iter().cloned().collect::<Vec<_>>();
    let refused = str_of(&results[1], "content");
    assert!(refused.contains("not saved") && refused.contains(&id) && refused.contains("update"), "{refused}");
    assert_eq!(results[1]["isError"], json!(false));

    let update = json!({"action": "update", "id": id, "content": "User likes green tea and jasmine tea"});
    app.llm.push(vec![tool_call("memory", update), text("updated")]);
    app.send_wait(&sid, "jasmine too").await;
    let memories = app.get("/api/memories").await;
    assert_eq!(memories.as_array().unwrap().len(), 1, "{memories}");
    assert_eq!(memories[0]["content"], json!("User likes green tea and jasmine tea"));
    assert_eq!(memories[0]["kind"], json!("preference"));
}

#[tokio::test(flavor = "multi_thread")]
async fn other_recent_conversations_are_listed_in_the_prompt() {
    let app = spawn_app(vec![text("Kyoto it is."), text("Hi!")]).await;
    app.disable_reflection().await;
    let trip = app.create_session().await;
    app.send_wait(&trip, "plan my trip to Kyoto in May").await;
    let first = support::system_prompt(&app.llm.requests()[0]);
    assert!(!first.contains("# Recent conversations"), "no other conversation yet: {first}");

    let sid = app.create_session().await;
    app.send_wait(&sid, "hi").await;
    let sys = support::system_prompt(&app.llm.requests()[1]);
    assert!(sys.contains("# Recent conversations"), "{sys}");
    assert!(sys.contains(&format!("- plan my trip to Kyoto in May (session {trip})")), "{sys}");
    assert!(sys.contains("check these before asking them to repeat themselves"), "{sys}");
    assert!(sys.find("# Recent conversations").unwrap() < sys.find("Current time:").unwrap(), "{sys}");
}
