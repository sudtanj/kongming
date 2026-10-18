//! Agent roles: creation, `@handle` switching, management structure and
//! delegation between a manager and a report.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{role, spawn_app, str_of, system_prompt, text, tool_call, tool_names, TestApp};

async fn create_researcher(app: &TestApp, manager_id: &str) -> Value {
    app.post(
        "/api/agents",
        json!({
            "handle": "Researcher",
            "name": "Researcher",
            "role": "Research analyst",
            "description": "Finds and verifies facts.",
            "managerId": manager_id,
        }),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn create_agents_and_validate_handles() {
    let app = spawn_app(vec![]).await;
    let default = app.default_agent().await;
    assert_eq!(default.handle, "kongming");
    assert!(default.is_default);

    let r = create_researcher(&app, &default.id).await;
    assert_eq!(r["handle"], json!("researcher"), "handles are lowercased");
    assert_eq!(r["managerId"], json!(default.id));
    assert_eq!(r["isDefault"], json!(false));
    assert_eq!(r["selfImprove"], json!(true));

    let (status, body) = app.post_raw("/api/agents", json!({"handle": "researcher"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _) = app.post_raw("/api/agents", json!({"handle": "skill"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "reserved handle");
    let (status, _) = app.post_raw("/api/agents", json!({"name": "no handle"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.post_raw("/api/agents", json!({"handle": "x", "managerId": "ghost"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown manager");
    // Without a managerId a new agent joins the team under the default agent.
    let w = app.post("/api/agents", json!({"handle": "writer"})).await;
    assert_eq!(w["managerId"], json!(default.id));
    app.delete(&format!("/api/agents/{}", str_of(&w, "id"))).await;

    let list = app.get("/api/agents").await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(app.get("/api/agents/researcher").await["id"], r["id"]);
    assert_eq!(app.get(&format!("/api/agents/{}", str_of(&r, "id"))).await["handle"], json!("researcher"));

    // Switch the default agent; the previous default loses the flag.
    let updated = app.put(&format!("/api/agents/{}", str_of(&r, "id")), json!({"isDefault": true})).await;
    assert_eq!(updated["isDefault"], json!(true));
    assert!(!app.get(&format!("/api/agents/{}", default.id)).await["isDefault"].as_bool().unwrap());
    assert_eq!(app.default_agent().await.handle, "researcher");

    // Deleting an agent; the last one cannot go.
    app.delete(&format!("/api/agents/{}", str_of(&r, "id"))).await;
    let list = app.get("/api/agents").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["isDefault"], json!(true), "default moves to the remaining agent");
    let r = support::within(app.client.delete(app.url(&format!("/api/agents/{}", default.id))).send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn mention_switches_the_session_agent() {
    let app = spawn_app(vec![text("yo")]).await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    let researcher = create_researcher(&app, &default.id).await;
    let researcher_id = str_of(&researcher, "id").to_string();

    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "@researcher hi").await;
    assert_eq!(reply["text"], json!("yo"));
    assert_eq!(reply["agent"]["handle"], json!("researcher"));

    let detail = app.get(&format!("/api/sessions/{sid}")).await;
    assert_eq!(detail["session"]["agentId"], json!(researcher_id));
    assert_eq!(detail["agent"]["handle"], json!("researcher"));
    assert_eq!(detail["session"]["title"], json!("researcher hi"));
    let msgs = detail["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["parts"][0]["text"], json!("@researcher hi"), "plain handle mentions stay in the stored text");
    assert_eq!(msgs[1]["agentId"], json!(researcher_id));

    let reqs = app.llm.requests();
    let sys = system_prompt(&reqs[0]);
    assert!(sys.contains("You are **Researcher** (@researcher)"), "{sys}");
    assert!(sys.contains("Role: Research analyst"), "{sys}");
    assert!(sys.contains("# Team"), "{sys}");
    assert!(sys.contains("Your manager is **Kongming** (@kongming)"), "{sys}");
    assert!(!sys.contains("delegate_task"), "a report cannot delegate: {sys}");
    let last = reqs[0]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"], json!("hi"), "the leading handle is stripped for the model");
    assert!(!tool_names(&reqs[0]).contains(&"delegate_task".to_string()));

    // The switch is sticky.
    app.llm.push(vec![text("still me")]);
    let reply = app.send_wait(&sid, "and now?").await;
    assert_eq!(reply["agent"]["handle"], json!("researcher"));
    // Switching back by mention.
    app.llm.push(vec![text("boss here")]);
    let reply = app.send_wait(&sid, "@kongming are you there").await;
    assert_eq!(reply["agent"]["handle"], json!("kongming"));
    assert_eq!(app.get(&format!("/api/sessions/{sid}")).await["session"]["agentId"], json!(default.id));
    // The manager's prompt lists its report.
    let sys = system_prompt(app.llm.requests().last().unwrap());
    assert!(sys.contains("Agents reporting to you"), "{sys}");
    assert!(sys.contains("- @researcher — Research analyst"), "{sys}");
    assert!(tool_names(app.llm.requests().last().unwrap()).contains(&"delegate_task".to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn management_cycles_are_rejected() {
    let app = spawn_app(vec![]).await;
    let default = app.default_agent().await;
    let researcher = create_researcher(&app, &default.id).await;
    let researcher_id = str_of(&researcher, "id").to_string();

    let (status, body) = app.put_raw(&format!("/api/agents/{}", default.id), json!({"managerId": researcher_id})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(str_of(&body, "error").contains("cycle"), "{body}");

    let (status, body) =
        app.put_raw(&format!("/api/agents/{researcher_id}"), json!({"managerId": researcher_id})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(str_of(&body, "error").contains("itself"), "{body}");

    // Three levels: default -> researcher -> intern; default may not report to intern.
    let intern = app.post("/api/agents", json!({"handle": "intern", "managerId": researcher_id})).await;
    let (status, _) =
        app.put_raw(&format!("/api/agents/{}", default.id), json!({"managerId": str_of(&intern, "id")})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Removing the manager works, and the structure is unchanged otherwise.
    let v = app.put(&format!("/api/agents/{researcher_id}"), json!({"managerId": null})).await;
    assert!(v["managerId"].is_null());
    assert_eq!(app.get(&format!("/api/agents/{}", default.id)).await["managerId"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_delegates_to_a_report() {
    let app = spawn_app(vec![
        tool_call("delegate_task", json!({"agent": "researcher", "task": "find x", "context": "x is a number"})),
        text("child result"),
        text("manager done"),
    ])
    .await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    let researcher = create_researcher(&app, &default.id).await;
    let researcher_id = str_of(&researcher, "id").to_string();

    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "please find x").await;
    assert_eq!(reply["text"], json!("manager done"));
    assert_eq!(reply["agent"]["handle"], json!("kongming"));

    // Task bookkeeping.
    let tasks = app.get("/api/tasks").await;
    let tasks = tasks.as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task["status"], json!("done"));
    assert_eq!(task["fromAgentId"], json!(default.id));
    assert_eq!(task["toAgentId"], json!(researcher_id));
    assert_eq!(task["sessionId"], json!(sid));
    assert_eq!(task["instruction"], json!("find x"));
    assert!(str_of(task, "result").contains("child result"), "{task}");
    assert!(task["finishedAt"].is_number());
    let child_id = str_of(task, "childSessionId").to_string();
    assert!(!child_id.is_empty());

    // The child session is a delegation session, not listed in the UI.
    let child = app.state.db.session(&child_id).await.unwrap().expect("child session");
    assert_eq!(child.channel, "delegation");
    assert_eq!(child.agent_id, researcher_id);
    assert_eq!(child.parent_session_id.as_deref(), Some(sid.as_str()));
    assert!(child.title.starts_with("@kongming → @researcher"), "{}", child.title);
    let listed = app.get("/api/sessions").await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let child_msgs = app.messages(&child_id).await;
    assert_eq!(child_msgs.len(), 2);
    assert!(str_of(&child_msgs[0]["parts"][0], "text").contains("Task from @kongming"));
    assert!(str_of(&child_msgs[0]["parts"][0], "text").contains("find x"));
    assert!(str_of(&child_msgs[0]["parts"][0], "text").contains("x is a number"));
    assert_eq!(child_msgs[1]["parts"][0]["text"], json!("child result"));
    assert_eq!(child_msgs[1]["agentId"], json!(researcher_id));

    // The manager's history carries the tool call and its result.
    let msgs = app.messages(&sid).await;
    let roles: Vec<&str> = msgs.iter().map(role).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
    assert_eq!(msgs[1]["parts"][0]["name"], json!("delegate_task"));
    let result = &msgs[2]["parts"][0];
    assert_eq!(result["isError"], json!(false));
    assert!(str_of(result, "content").contains("Result from @researcher"), "{result}");
    assert!(str_of(result, "content").contains("child result"), "{result}");

    // Model requests: manager, child, manager again.
    let reqs = app.llm.requests();
    assert!(reqs.len() >= 3, "requests: {}", reqs.len());
    assert!(tool_names(&reqs[0]).contains(&"delegate_task".to_string()));
    let child_sys = system_prompt(&reqs[1]);
    assert!(child_sys.contains("(@researcher)"), "{child_sys}");
    assert!(child_sys.contains("You were given this task by another agent"), "{child_sys}");
    let manager_second = reqs[2]["messages"].as_array().unwrap();
    assert!(manager_second.iter().any(|m| m["role"] == json!("tool")));
}

#[tokio::test(flavor = "multi_thread")]
async fn delegation_is_refused_outside_the_structure() {
    let app =
        spawn_app(vec![tool_call("delegate_task", json!({"agent": "stranger", "task": "x"})), text("fine")]).await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    create_researcher(&app, &default.id).await;
    // "stranger" has no manager (an explicit null; the default is Kongming).
    app.post("/api/agents", json!({"handle": "stranger", "managerId": null})).await;

    let sid = app.create_session().await;
    assert_eq!(app.send_wait(&sid, "delegate").await["text"], json!("fine"));
    let msgs = app.messages(&sid).await;
    let result = &msgs[2]["parts"][0];
    assert_eq!(result["isError"], json!(true));
    assert!(str_of(result, "content").contains("does not report to you"), "{result}");
    assert!(app.get("/api/tasks").await.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn kongming_hires_a_report_and_delegates_to_it_in_the_same_turn() {
    let hire = json!({"op": "create", "handle": "Analyst", "role": "Data analyst", "description": "Crunches numbers."});
    let app = spawn_app(vec![
        tool_call("agent_manage", hire),
        tool_call("delegate_task", json!({"agent": "analyst", "task": "sum 1 and 2"})),
        text("3"),
        text("the analyst says 3"),
    ])
    .await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    let sid = app.create_session().await;

    assert_eq!(app.send_wait(&sid, "hire an analyst and ask it").await["text"], json!("the analyst says 3"));
    let analyst = app.get("/api/agents/analyst").await;
    assert_eq!(analyst["managerId"], json!(default.id));
    assert_eq!(analyst["role"], json!("Data analyst"));
    let reqs = app.llm.requests();
    let names = tool_names(&reqs[0]);
    assert!(names.contains(&"agent_manage".to_string()), "{names:?}");
    assert!(!names.contains(&"delegate_task".to_string()), "no reports yet: {names:?}");
    assert!(tool_names(&reqs[1]).contains(&"delegate_task".to_string()), "delegation opens up after hiring");
    let tasks = app.get("/api/tasks").await;
    assert_eq!(tasks[0]["status"], json!("done"), "{tasks}");

    // Only the lead agent hires.
    assert!(!tool_names(&reqs[2]).contains(&"agent_manage".to_string()), "the analyst's tools");
}
