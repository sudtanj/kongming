//! Built-in tools through the agent loop: tool rounds, skills, memory, skill
//! mentions and the post-turn reflection pass.

mod support;

use serde_json::{json, Value};
use support::{
    part_types, poll_until, role, spawn_app, spawn_app_with, str_of, system_prompt, text, tool_call, tool_names,
};

fn find_tool_message(msgs: &[Value]) -> Option<&Value> {
    msgs.iter().find(|m| m["role"] == json!("tool"))
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_round_is_persisted_and_fed_back() {
    let app = spawn_app(vec![tool_call("current_time", json!({})), text("done")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;

    let reply = app.send_wait(&sid, "what time is it?").await;
    assert_eq!(reply["text"], json!("done"));
    assert_eq!(reply["stopReason"], json!("stop"));

    let msgs = app.messages(&sid).await;
    let roles: Vec<&str> = msgs.iter().map(role).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
    assert_eq!(part_types(&msgs[1]), vec!["tool_call".to_string()]);
    assert_eq!(msgs[1]["parts"][0]["name"], json!("current_time"));
    assert_eq!(msgs[1]["parts"][0]["id"], json!("call_0"));
    assert_eq!(part_types(&msgs[2]), vec!["tool_result".to_string()]);
    let result = &msgs[2]["parts"][0];
    assert_eq!(result["id"], json!("call_0"));
    assert_eq!(result["name"], json!("current_time"));
    assert_eq!(result["isError"], json!(false));
    assert!(str_of(result, "content").contains("UTC (unix "), "content: {}", result["content"]);
    assert_eq!(msgs[3]["parts"][0]["text"], json!("done"));

    let reqs = app.llm.requests();
    assert!(reqs.len() >= 2, "expected two model rounds, got {}", reqs.len());
    let second = reqs[1]["messages"].as_array().unwrap();
    let assistant = second.iter().find(|m| m["role"] == json!("assistant")).expect("assistant message with tool_calls");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], json!("current_time"));
    assert_eq!(assistant["tool_calls"][0]["id"], json!("call_0"));
    let tool_msg = second.iter().find(|m| m["role"] == json!("tool")).expect("role: tool message");
    assert_eq!(tool_msg["tool_call_id"], json!("call_0"));
    assert!(str_of(tool_msg, "content").contains("UTC"));
    // Both rounds recorded usage.
    assert_eq!(app.get("/api/usage").await["day"]["requests"], json!(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_tool_yields_an_error_result() {
    let app = spawn_app(vec![tool_call("nonexistent_tool", json!({"a": 1})), text("sorry")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "do it").await["text"], json!("sorry"));
    let msgs = app.messages(&sid).await;
    let tool_msg = find_tool_message(&msgs).expect("tool message");
    assert_eq!(tool_msg["parts"][0]["isError"], json!(true));
    assert!(str_of(&tool_msg["parts"][0], "content").contains("unknown tool"), "{}", tool_msg["parts"][0]);
}

const STEPS: &str =
    "# Deploy\n\n1. Run `make release` on the build host.\n2. Copy the tarball to the board and restart.";
const STEPS_V2: &str =
    "# Deploy\n\n1. Run `make release` on the build host.\n2. Copy the tarball over.\n3. Restart and check /health.";

#[tokio::test(flavor = "multi_thread")]
async fn a_skill_without_a_real_body_is_sent_back() {
    let ops = json!({"operations": [{"op": "create", "name": "deploy", "description": "Use when deploying", "body": "Use when deploying."}]});
    let app = spawn_app(vec![tool_call("skill_manage", ops), text("could not save")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    app.send_wait(&sid, "save a deploy skill").await;
    let msgs = app.messages(&sid).await;
    let result = &find_tool_message(&msgs).expect("tool message")["parts"][0];
    assert_eq!(result["isError"], json!(true));
    assert!(str_of(result, "content").contains("only repeats the description"), "{result}");
    assert!(app.get("/api/skills").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_manage_creates_a_skill_that_shows_up_in_later_prompts() {
    let ops = json!({"operations": [{"op": "create", "name": "Deploy Notes", "description": "when deploying", "body": STEPS}]});
    let app = spawn_app(vec![tool_call("skill_manage", ops), text("saved")]).await;
    app.disable_reflection().await;
    let agent = app.default_agent().await;
    let sid = app.create_session().await;

    // Before: no skills, and the prompt says so.
    assert!(app.get("/api/skills").await.as_array().unwrap().is_empty());
    assert_eq!(app.send_wait(&sid, "save a deploy skill").await["text"], json!("saved"));
    let first_prompt = system_prompt(&app.llm.requests()[0]);
    assert!(first_prompt.contains("No skills saved yet"), "{first_prompt}");

    let msgs = app.messages(&sid).await;
    let tool_msg = find_tool_message(&msgs).expect("tool message");
    assert_eq!(tool_msg["parts"][0]["isError"], json!(false));
    assert!(
        str_of(&tool_msg["parts"][0], "content").contains("Created skill `deploy-notes` (v1)"),
        "{}",
        tool_msg["parts"][0]
    );

    let skills = app.get("/api/skills").await;
    let list = skills.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], json!("deploy-notes"));
    assert_eq!(list[0]["source"], json!("agent"));
    assert_eq!(list[0]["version"], json!(1));
    assert_eq!(list[0]["authorAgentId"], json!(agent.id));
    assert_eq!(list[0]["enabled"], json!(true));

    let detail = app.get("/api/skills/deploy-notes").await;
    assert_eq!(detail["body"], json!(STEPS));
    assert!(str_of(&detail, "markdown").starts_with("---\nname: deploy-notes\n"));
    assert!(detail["versions"].as_array().unwrap().is_empty());

    let improvements = app.get("/api/improvements").await;
    let created = improvements
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == json!("skill_created"))
        .expect("skill_created improvement");
    assert_eq!(created["agentId"], json!(agent.id));
    assert_eq!(created["sessionId"], json!(sid));
    assert_eq!(created["refId"], list[0]["id"]);

    // Second turn: the skills index now lists it.
    app.llm.push(vec![text("ok")]);
    assert_eq!(app.send_wait(&sid, "thanks").await["text"], json!("ok"));
    let reqs = app.llm.requests();
    assert!(reqs.len() >= 3, "requests: {}", reqs.len());
    let sys = system_prompt(&reqs[2]);
    assert!(sys.contains("<available_skills>"), "{sys}");
    assert!(sys.contains("- deploy-notes: when deploying"), "{sys}");
    assert!(sys.contains("</available_skills>"), "{sys}");

    // Updating through the tool bumps the version and keeps the old body.
    let update = json!({"operations": [{"op": "update", "name": "deploy-notes", "body": STEPS_V2}]});
    app.llm.push(vec![tool_call("skill_manage", update), text("updated")]);
    assert_eq!(app.send_wait(&sid, "update it").await["text"], json!("updated"));
    let detail = app.get("/api/skills/deploy-notes").await;
    assert_eq!(detail["version"], json!(2));
    assert_eq!(detail["body"], json!(STEPS_V2));
    assert_eq!(detail["description"], json!("when deploying"), "description is inherited on update");
    let versions = detail["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["body"], json!(STEPS));
    let kinds: Vec<String> =
        app.get("/api/improvements").await.as_array().unwrap().iter().map(|i| str_of(i, "kind").to_string()).collect();
    assert!(kinds.contains(&"skill_updated".to_string()), "{kinds:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_tool_saves_and_prompts_include_it() {
    let add = json!({"action": "add", "content": "User likes tea", "kind": "preference", "keywords": ["tea"], "importance": 4});
    let app = spawn_app(vec![tool_call("memory", add), text("noted")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "I like tea").await["text"], json!("noted"));

    let memories = app.get("/api/memories").await;
    let list = memories.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["content"], json!("User likes tea"));
    assert_eq!(list[0]["kind"], json!("preference"));
    assert_eq!(list[0]["importance"], json!(4));
    assert!(list[0]["agentId"].is_null(), "shared by default");
    assert_eq!(list[0]["keywords"], json!(["tea"]));
    let improvements = app.get("/api/improvements").await;
    assert!(improvements.as_array().unwrap().iter().any(|i| i["kind"] == json!("memory_saved")));

    // Memory search through the tool, then the prompt of a later turn carries the memory.
    app.llm.push(vec![tool_call("memory", json!({"action": "search", "query": "tea"})), text("found")]);
    assert_eq!(app.send_wait(&sid, "what do I like?").await["text"], json!("found"));
    let msgs = app.messages(&sid).await;
    let last_tool = msgs.iter().rev().find(|m| m["role"] == json!("tool")).expect("tool message");
    assert!(str_of(&last_tool["parts"][0], "content").contains("User likes tea"), "{}", last_tool["parts"][0]);
    let reqs = app.llm.requests();
    let sys = system_prompt(reqs.last().unwrap());
    assert!(sys.contains("# Memory"), "{sys}");
    assert!(sys.contains("- [preference] User likes tea"), "{sys}");

    // Remove it via the REST API.
    let id = str_of(&list[0], "id").to_string();
    app.delete(&format!("/api/memories/{id}")).await;
    assert!(app.get("/api/memories").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_mention_inlines_the_skill_body() {
    let app = spawn_app(vec![text("ship it")]).await;
    app.disable_reflection().await;
    let saved =
        app.put("/api/skills/deploy-notes", json!({"description": "when deploying", "body": "# steps\n1. ship"})).await;
    assert_eq!(saved["name"], json!("deploy-notes"));
    assert_eq!(saved["source"], json!("user"));

    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "@skill:deploy-notes what next").await["text"], json!("ship it"));
    let sys = system_prompt(&app.llm.requests()[0]);
    assert!(sys.contains("<skill name=\"deploy-notes\">"), "{sys}");
    assert!(sys.contains("1. ship"), "{sys}");
    assert!(sys.contains("</skill>"), "{sys}");
    // The structured mention is stripped from the stored text.
    let msgs = app.messages(&sid).await;
    assert_eq!(msgs[0]["parts"][0]["text"], json!("what next"));
    // Inlining counts as a use.
    assert_eq!(app.get("/api/skills/deploy-notes").await["uses"], json!(1));

    // Disabled skills are not inlined and vanish from the index.
    app.put("/api/skills/deploy-notes", json!({"enabled": false})).await;
    app.llm.push(vec![text("no skill")]);
    app.send_wait(&sid, "@skill:deploy-notes again").await;
    let sys = system_prompt(app.llm.requests().last().unwrap());
    assert!(!sys.contains("<skill name=\"deploy-notes\">"), "{sys}");
    assert!(!sys.contains("- deploy-notes:"), "{sys}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reflection_runs_after_a_tool_using_turn() {
    let app = spawn_app(vec![tool_call("current_time", json!({})), text("done")]).await;
    app.enable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "time please").await["text"], json!("done"));

    // Reflection is a background task after the turn: wait for its model request.
    let llm = app.llm.clone();
    let reqs = poll_until("reflection request", || {
        let llm = llm.clone();
        async move {
            let reqs = llm.requests();
            if reqs.len() >= 3 {
                Some(reqs)
            } else {
                None
            }
        }
    })
    .await;
    let reflection = &reqs[2];
    let sys = system_prompt(reflection);
    assert!(sys.contains("reflection step"), "{sys}");
    assert!(sys.contains("@kongming"), "{sys}");
    let names = tool_names(reflection);
    assert!(names.contains(&"skill_manage".to_string()), "{names:?}");
    assert!(names.contains(&"memory".to_string()), "{names:?}");
    assert!(!names.contains(&"current_time".to_string()), "{names:?}");
    let user = reflection["messages"].as_array().unwrap().last().unwrap();
    assert!(str_of(user, "content").starts_with("Conversation to review:"), "{}", user["content"]);

    // The mock answered "(no script)", which is logged as a reflection note.
    let app_ref = &app;
    poll_until("reflection improvement", || async move {
        let list = app_ref.get("/api/improvements").await;
        let hit = list
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == json!("reflection") && i["note"] == json!("(no script)"));
        if hit {
            Some(())
        } else {
            None
        }
    })
    .await;
    // Reflection never writes into the chat history.
    assert_eq!(app.messages(&sid).await.len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn reflection_is_skipped_for_plain_chat_when_disabled() {
    let app = spawn_app(vec![tool_call("current_time", json!({})), text("done")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "time please").await["text"], json!("done"));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(app.llm.request_count(), 2);
    assert!(app.get("/api/improvements").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_wrapped_in_the_wrong_tool_is_bounced_back_to_the_model() {
    // Seen with weaker models: another tool's call put inside the arguments.
    let wrapped = json!({"name": "read session", "parameters": {"session_id": "x", "limit": "10"}});
    let app = spawn_app(vec![tool_call("skill_view", wrapped), text("let me retry")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "read it").await["text"], json!("let me retry"));
    let msgs = app.messages(&sid).await;
    let result = &find_tool_message(&msgs).expect("tool message")["parts"][0];
    assert_eq!(result["isError"], json!(true));
    let content = str_of(result, "content");
    assert!(content.contains("unknown: parameters"), "{content}");
    assert!(content.contains("call it directly"), "{content}");
    assert!(content.contains("\"required\":[\"name\"]"), "the schema is shown: {content}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_reply_after_tools_is_reported_not_silent() {
    let app = spawn_app(vec![tool_call("current_time", json!({})), text(""), text("")]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "what time is it?").await;
    assert_eq!(reply["stopReason"], json!("error"), "{reply}");
    assert!(str_of(&reply, "error").contains("empty reply after the tool results"), "{reply}");
}

fn search(q: &str) -> support::ScriptedReply {
    tool_call("memory", json!({"action": "search", "query": q}))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_continues_by_itself_past_the_step_limit() {
    let replies = vec![search("a"), search("b"), search("c"), text("done")];
    let app = spawn_app_with(replies, &["--max-iterations", "2", "--auto-continue", "1"]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "look it up").await;
    assert_eq!(reply["text"], json!("done"), "{reply}");
    assert_eq!(reply["stopReason"], json!("stop"), "{reply}");
    assert_eq!(app.llm.requests().len(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_repeats_itself_stops_at_the_step_limit() {
    let replies = vec![search("a"), search("a"), search("a"), text("done")];
    let app = spawn_app_with(replies, &["--max-iterations", "2", "--auto-continue", "3"]).await;
    app.disable_reflection().await;
    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "look it up").await;
    assert_eq!(reply["stopReason"], json!("max_iterations"), "{reply}");
    assert!(str_of(&reply, "text").contains("kept repeating"), "{reply}");
    assert_eq!(app.llm.requests().len(), 2);
}
