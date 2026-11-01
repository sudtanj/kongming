//! Turns driven one step at a time, the way a Worker drives them from alarms:
//! `begin_turn`, then `step_turn` per invocation with the state stored in
//! between, delegations as nested steps; and the background `tick`.

mod support;

use std::sync::atomic::AtomicBool;

use kongming::agent::{self, Step, TurnOptions, TurnState};
use kongming::background::{self, Due};
use serde_json::json;
use support::{role, spawn_app, str_of, text, tool_call};

/// One step, with the state stored and loaded again in between.
async fn step(app: &support::TestApp, ts: TurnState) -> Step {
    let stored = serde_json::to_string(&ts).expect("serialize");
    let ts: TurnState = serde_json::from_str(&stored).expect("deserialize");
    agent::step_turn(&app.state, ts, &AtomicBool::new(false)).await.expect("step")
}

#[tokio::test(flavor = "multi_thread")]
async fn one_model_round_per_step_and_the_delegate_steps_too() {
    let app = spawn_app(vec![
        tool_call("delegate_task", json!({"agent": "researcher", "task": "find x"})),
        text("child result"),
        text("manager done"),
    ])
    .await;
    app.disable_reflection().await;
    let default = app.default_agent().await;
    app.hire("researcher", "Research analyst", Some(&default.id)).await;
    let sid = app.create_session().await;
    let session = app.state.db.session(&sid).await.unwrap().expect("session");

    let ts =
        agent::begin_turn(&app.state, session, "please find x".into(), TurnOptions::default()).await.expect("begin");
    assert_eq!(app.llm.request_count(), 0, "beginning a turn calls no model");
    let stored = serde_json::to_string(&ts).unwrap();
    assert!(!stored.contains("\"apiKey\""), "the provider key is not kept in the turn state");

    // 1: the manager's round; its delegate_task call opens the delegate's turn.
    let Step::Continue(ts) = step(&app, ts).await else { panic!("finished after one step") };
    assert_eq!(app.llm.request_count(), 1);
    let child_id = ts.waiting_on().expect("waiting on the delegate").to_string();
    assert_ne!(child_id, sid);

    // 2: the delegate's round; its answer becomes the manager's tool result.
    let Step::Continue(ts) = step(&app, *ts).await else { panic!("finished after two steps") };
    assert_eq!(app.llm.request_count(), 2);
    assert!(ts.waiting_on().is_none());

    // 3: the manager answers.
    let Step::Done(r) = step(&app, *ts).await else { panic!("not finished after three steps") };
    assert_eq!(r.text, "manager done");
    assert_eq!(r.stop_reason, "stop");
    assert!(r.error.is_none());
    assert_eq!(app.llm.request_count(), 3);

    let msgs = app.messages(&sid).await;
    let roles: Vec<&str> = msgs.iter().map(role).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool", "assistant"]);
    let result = &msgs[2]["parts"][0];
    assert_eq!(result["isError"], json!(false));
    assert!(str_of(result, "content").contains("Result from @researcher"), "{result}");
    assert!(str_of(result, "content").contains("child result"), "{result}");
    let child_msgs = app.messages(&child_id).await;
    assert_eq!(child_msgs.len(), 2);
    let tasks = app.get("/api/tasks").await;
    assert_eq!(tasks[0]["status"], json!("done"));
    assert_eq!(tasks[0]["childSessionId"], json!(child_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn tick_claims_a_due_cron_job_whose_turn_runs_in_steps() {
    let app = spawn_app(vec![text("cron answer")]).await;
    app.disable_reflection().await;
    let job = app.post("/api/cron", json!({"name": "Report", "schedule": "every:5", "prompt": "report please"})).await;
    let id = str_of(&job, "id").to_string();
    let next = job["nextRunAt"].as_i64().expect("next run");

    let now = kongming::db::now_ms();
    assert!(!background::tick(&app.state, now).await.iter().any(|d| matches!(d, Due::Cron { .. })), "not due yet");
    let due = background::tick(&app.state, next + 1000).await;
    let job = due
        .into_iter()
        .find_map(|d| match d {
            Due::Cron { job } => Some(job),
            _ => None,
        })
        .expect("the job is due");
    assert_eq!(job.id, id);
    assert!(
        !background::tick(&app.state, next + 2000).await.iter().any(|d| matches!(d, Due::Cron { .. })),
        "a claimed job is not handed out twice"
    );

    let started = background::start(&app.state, Due::Cron { job }).await.expect("start").expect("a turn");
    let stored = serde_json::to_string(&started).expect("serialize");
    let started: background::JobTurn = serde_json::from_str(&stored).expect("deserialize");
    let mut ts = started.turn;
    let res = loop {
        match step(&app, ts).await {
            Step::Continue(next) => ts = *next,
            Step::Done(r) => break r,
        }
    };
    assert_eq!(res.text, "cron answer");
    background::finish(&app.state, started.followup, Ok(res)).await.expect("finish");

    let jobs = app.get("/api/cron").await;
    let job = jobs.as_array().unwrap().iter().find(|j| j["id"] == json!(id)).expect("job");
    assert_eq!(job["lastStatus"], json!("ok"), "{job}");
}
