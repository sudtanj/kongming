//! Scheduled prompts: CRUD validation and a manual run that produces a session.

mod support;

use reqwest::StatusCode;
use serde_json::{json, Value};
use support::{poll_until, spawn_app, str_of, text};

#[tokio::test(flavor = "multi_thread")]
async fn create_validate_update_and_delete_jobs() {
    let app = spawn_app(vec![]).await;
    let before = kongming::db::now_ms();
    let job = app.post("/api/cron", json!({"name": "Daily", "schedule": "every:30", "prompt": "report please"})).await;
    let id = str_of(&job, "id").to_string();
    assert!(!id.is_empty());
    assert_eq!(job["enabled"], json!(true));
    assert_eq!(job["agentId"], json!(app.default_agent().await.id), "defaults to the default agent");
    let next = job["nextRunAt"].as_i64().expect("nextRunAt set");
    assert!(next >= before + 30 * 60_000 - 1000, "nextRunAt {next} should be ~30 minutes ahead of {before}");
    assert!(job["lastRunAt"].is_null());

    let (status, body) = app.post_raw("/api/cron", json!({"name": "Bad", "schedule": "nonsense", "prompt": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(str_of(&body, "error").contains("schedule"), "{body}");
    let (status, body) = app.post_raw("/api/cron", json!({"name": "Bad", "schedule": "every:0", "prompt": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = app.post_raw("/api/cron", json!({"name": "NoPrompt", "schedule": "every:5"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = app
        .post_raw("/api/cron", json!({"name": "Ghost", "schedule": "every:5", "prompt": "x", "agentId": "ghost"}))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // A cron expression is accepted and schedules on a minute boundary.
    let hourly = app.post("/api/cron", json!({"name": "Hourly", "schedule": "0 * * * *", "prompt": "tick"})).await;
    let n = hourly["nextRunAt"].as_i64().unwrap();
    assert_eq!(n % 60_000, 0);
    assert!(n > before);

    let list = app.get("/api/cron").await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Update: disabling clears the next run; re-enabling restores it.
    let updated = app.put(&format!("/api/cron/{id}"), json!({"enabled": false})).await;
    assert!(updated["nextRunAt"].is_null());
    assert_eq!(updated["enabled"], json!(false));
    let updated =
        app.put(&format!("/api/cron/{id}"), json!({"enabled": true, "schedule": "every:5", "deliverTo": "42"})).await;
    assert!(updated["nextRunAt"].is_number());
    assert_eq!(updated["schedule"], json!("every:5"));
    assert_eq!(updated["deliverTo"], json!("42"));
    let (status, _) = app.put_raw(&format!("/api/cron/{id}"), json!({"schedule": "bad"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = app.put_raw("/api/cron/missing", json!({"enabled": true})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    app.delete(&format!("/api/cron/{id}")).await;
    let list = app.get("/api/cron").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["name"], json!("Hourly"));
    let (status, _) = app.post_raw("/api/cron/missing/run", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn manual_run_creates_a_cron_session() {
    let app = spawn_app(vec![text("report")]).await;
    app.disable_reflection().await;
    let job = app.post("/api/cron", json!({"name": "Daily", "schedule": "every:30", "prompt": "report please"})).await;
    let id = str_of(&job, "id").to_string();

    let started = app.post(&format!("/api/cron/{id}/run"), json!({})).await;
    assert_eq!(started["started"], json!(true));

    // Cron sessions are listed (unlike delegation sessions); wait for it to appear.
    let app_ref = &app;
    let session: Value = poll_until("cron session", || async move {
        let list = app_ref.get("/api/sessions").await;
        list.as_array().unwrap().iter().find(|s| s["title"] == json!("⏰ Daily")).cloned()
    })
    .await;
    assert_eq!(session["channel"], json!("cron"));
    assert_eq!(session["channelRef"], json!(id));
    assert_eq!(session["agentId"], json!(app.default_agent().await.id));
    let sid = str_of(&session, "id").to_string();

    // The turn finishes and the job records its status.
    let id_ref = id.as_str();
    let job: Value = poll_until("job status", || async move {
        let list = app_ref.get("/api/cron").await;
        let job = list.as_array().unwrap().iter().find(|j| j["id"] == json!(id_ref)).cloned().unwrap();
        if job["lastStatus"] == json!("ok") {
            Some(job)
        } else {
            None
        }
    })
    .await;
    assert!(job["lastRunAt"].is_number());
    assert!(job["nextRunAt"].is_number());

    let msgs = app.messages(&sid).await;
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["parts"][0]["text"], json!("report please"));
    assert_eq!(msgs[1]["parts"][0]["text"], json!("report"));
    let sys = support::system_prompt(&app.llm.requests()[0]);
    assert!(sys.contains("scheduled run without a human present"), "{sys}");
}
