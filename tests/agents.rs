//! The team: the user talks only to Kongming, which hires, adjusts, retires and
//! delegates to its reports; the rest of the team is read-only to the user.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{role, spawn_app, str_of, system_prompt, text, tool_call, tool_names};

#[tokio::test(flavor = "multi_thread")]
async fn the_team_is_read_only_to_the_user() {
    let app = spawn_app(vec![]).await;
    let default = app.default_agent().await;
    assert_eq!(default.handle, "kongming");
    assert!(default.is_default);
    let r = app.hire("researcher", "Research analyst", Some(&default.id)).await;

    let list = app.get("/api/agents").await;
    assert_eq!(list.as_array().unwrap().len(), 2, "the user sees the whole team");
    assert_eq!(app.get("/api/agents/researcher").await["id"], json!(r.id));

    let (status, body) = app.post_raw("/api/agents", json!({"handle": "writer"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(str_of(&body, "error").contains("read-only"), "{body}");
    let (status, body) = app.put_raw(&format!("/api/agents/{}", r.id), json!({"role": "Intern"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let resp = support::within(app.client.delete(app.url(&format!("/api/agents/{}", r.id))).send()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(app.state.db.agent(&r.id).await.unwrap().unwrap().role, "Research analyst");

    // Kongming itself stays editable, but it keeps leading.
    let v = app.put(&format!("/api/agents/{}", default.id), json!({"role": "Strategist", "handle": "Kongming"})).await;
    assert_eq!(v["role"], json!("Strategist"));
    assert_eq!(v["handle"], json!("kongming"));
    let (status, _) = app.put_raw(&format!("/api/agents/{}", default.id), json!({"managerId": r.id})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.put_raw(&format!("/api/agents/{}", default.id), json!({"isDefault": false})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.put_raw(&format!("/api/agents/{}", default.id), json!({"handle": "researcher"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "handle taken");
    assert!(app.default_agent().await.is_default);

    // Chats and automations are with Kongming only.
    let (status, body) = app.post_raw("/api/sessions", json!({"agentId": "researcher"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let s = app.post("/api/sessions", json!({"agentId": "kongming"})).await;
    assert_eq!(s["agentId"], json!(default.id));
    let sid = str_of(&s, "id").to_string();
    let patch = app.client.patch(app.url(&format!("/api/sessions/{sid}"))).json(&json!({"agentId": r.id}));
    assert_eq!(support::within(patch.send()).await.unwrap().status(), StatusCode::FORBIDDEN);
    let (status, _) =
        app.post_raw("/api/cron", json!({"name": "x", "schedule": "every:5", "prompt": "x", "agentId": r.id})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn naming_a_teammate_still_reaches_kongming() {
    let app = spawn_app(vec![text("I will ask the researcher")]).await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    let r = app.hire("researcher", "Research analyst", Some(&default.id)).await;

    // A chat left with a report from before stays with Kongming from now on.
    let sid = app.create_session().await;
    app.state.db.set_session_agent(&sid, &r.id).await.unwrap();
    let reply = app.send_wait(&sid, "@researcher hi").await;
    assert_eq!(reply["text"], json!("I will ask the researcher"));
    assert_eq!(reply["agent"]["handle"], json!("kongming"));
    let detail = app.get(&format!("/api/sessions/{sid}")).await;
    assert_eq!(detail["session"]["agentId"], json!(default.id));
    let msgs = detail["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["parts"][0]["text"], json!("@researcher hi"));
    assert_eq!(msgs[1]["agentId"], json!(default.id));

    let reqs = app.llm.requests();
    let last = reqs[0]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"], json!("@researcher hi"), "Kongming sees who was meant");
    let sys = system_prompt(&reqs[0]);
    assert!(sys.contains("You are **Kongming** (@kongming)"), "{sys}");
    assert!(sys.contains("the only one the user talks to"), "{sys}");
    assert!(sys.contains("- @researcher — Research analyst"), "{sys}");
    let names = tool_names(&reqs[0]);
    assert!(names.contains(&"delegate_task".to_string()), "{names:?}");
    assert!(names.contains(&"agent_manage".to_string()), "{names:?}");

    // "@kongming" at the start is just an address.
    app.llm.push(vec![text("here")]);
    app.send_wait(&sid, "@kongming are you there").await;
    let last = app.llm.requests().last().unwrap()["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"], json!("are you there"));
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
    let researcher_id = app.hire("researcher", "Research analyst", Some(&default.id)).await.id;

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

    // The child session is a delegation session, not listed in the sidebar.
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

    // The user can read it but not write in it.
    let (status, body) =
        app.post_raw(&format!("/api/sessions/{child_id}/messages?wait=true"), json!({"text": "do it again"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(str_of(&body, "error").contains("read-only"), "{body}");
    let (status, _) = app.post_raw(&format!("/api/sessions/{child_id}/clear"), json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(app.messages(&child_id).await.len(), 2);

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
    assert!(child_sys.contains("Your manager is **Kongming** (@kongming)"), "{child_sys}");
    assert!(child_sys.contains("the user does not talk to you directly"), "{child_sys}");
    assert!(child_sys.contains("You were given this task by another agent"), "{child_sys}");
    assert!(!tool_names(&reqs[1]).contains(&"delegate_task".to_string()), "a report has no reports");
    let manager_second = reqs[2]["messages"].as_array().unwrap();
    assert!(manager_second.iter().any(|m| m["role"] == json!("tool")));
}

#[tokio::test(flavor = "multi_thread")]
async fn delegation_is_refused_outside_the_structure() {
    let app =
        spawn_app(vec![tool_call("delegate_task", json!({"agent": "stranger", "task": "x"})), text("fine")]).await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    app.hire("researcher", "Research analyst", Some(&default.id)).await;
    app.hire("stranger", "Nobody's report", None).await;

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

#[tokio::test(flavor = "multi_thread")]
async fn kongming_corrects_and_retires_its_reports() {
    let app = spawn_app(vec![
        tool_call("agent_manage", json!({"op": "update", "handle": "researcher", "skills": ["no-such-skill"]})),
        tool_call(
            "agent_manage",
            json!({"op": "update", "handle": "researcher", "system_prompt": "Always cite a source.", "mcp_servers": []}),
        ),
        tool_call("agent_manage", json!({"op": "retire", "handle": "stranger"})),
        tool_call("agent_manage", json!({"op": "retire", "handle": "intern"})),
        text("fixed the researcher and let the intern go"),
    ])
    .await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    let r = app.hire("researcher", "Research analyst", Some(&default.id)).await;
    app.hire("stranger", "Nobody's report", None).await;
    let intern = app.hire("intern", "Intern", Some(&default.id)).await;

    let sid = app.create_session().await;
    let reply = app.send_wait(&sid, "the researcher made things up and we no longer need the intern").await;
    assert_eq!(reply["text"], json!("fixed the researcher and let the intern go"));

    let msgs = app.messages(&sid).await;
    let results: Vec<&Value> = msgs.iter().filter(|m| role(m) == "tool").map(|m| &m["parts"][0]).collect();
    assert_eq!(results.len(), 4);
    assert_eq!(results[0]["isError"], json!(true));
    assert!(str_of(results[0], "content").contains("no skill named 'no-such-skill'"), "{}", results[0]);
    assert_eq!(results[1]["isError"], json!(false), "{}", results[1]);
    assert_eq!(results[2]["isError"], json!(true));
    assert!(str_of(results[2], "content").contains("does not report to you"), "{}", results[2]);
    assert_eq!(results[3]["isError"], json!(false), "{}", results[3]);
    assert!(str_of(results[3], "content").contains("Retired agent @intern"), "{}", results[3]);

    let r = app.state.db.agent(&r.id).await.unwrap().unwrap();
    assert_eq!(r.system_prompt, "Always cite a source.");
    assert!(r.mcp_servers.is_none());
    assert!(r.pinned_skills.is_empty());
    assert!(app.state.db.agent(&intern.id).await.unwrap().is_none());
    assert!(app.state.db.agent_by_handle("stranger").await.unwrap().is_some());
    let imps = app.get("/api/improvements").await;
    assert!(imps.as_array().unwrap().iter().any(|i| i["kind"] == json!("agent_retired")), "{imps}");
}
