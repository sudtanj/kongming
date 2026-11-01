//! Small and local models through the agent loop: calls written as text,
//! inline reasoning, near-miss tool names, repeated failures, nudges and the
//! closing summary at the step limit.

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
