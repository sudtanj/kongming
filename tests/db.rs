//! Store tests: the typed queries in `kongming::db` against the Hrana mock.

mod support;

use kongming::db::{Agent, CronJob, McpServer, OAuthState, Part};
use serde_json::json;
use support::HranaMock;

fn agent(id: &str, handle: &str, is_default: bool, manager: Option<&str>) -> Agent {
    let now = kongming::db::now_ms();
    Agent {
        id: id.into(),
        handle: handle.into(),
        name: handle.to_uppercase(),
        role: "Role".into(),
        description: "desc".into(),
        system_prompt: String::new(),
        model: None,
        provider: None,
        manager_id: manager.map(str::to_string),
        tools: None,
        mcp_servers: None,
        pinned_skills: vec![],
        self_improve: true,
        is_default,
        created_at: now,
        updated_at: now,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_is_idempotent_and_ping_works() {
    let hrana = HranaMock::start().await;
    let db = hrana.db().await;
    db.migrate().await.expect("second migrate");
    db.turso.ping().await.expect("ping");
    assert_eq!(hrana.query_i64("SELECT COUNT(*) FROM agents"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_round_trip_with_cache_invalidation() {
    let db = HranaMock::start().await.db().await;
    assert!(db.setting("llm").await.unwrap().is_none());
    db.set_setting("llm", &json!({"kind": "openai", "model": "a"})).await.unwrap();
    assert_eq!(db.setting("llm").await.unwrap().unwrap()["model"], json!("a"));
    // Overwrite: the cached map must be refreshed.
    db.set_setting("llm", &json!({"kind": "openai", "model": "b"})).await.unwrap();
    assert_eq!(db.setting("llm").await.unwrap().unwrap()["model"], json!("b"));
    db.set_setting("ui", &json!({"theme": "dark"})).await.unwrap();
    let all = db.settings().await.unwrap();
    assert_eq!(all.len(), 2);
    db.delete_setting("llm").await.unwrap();
    assert!(db.setting("llm").await.unwrap().is_none());
    assert!(db.setting("ui").await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_upsert_lookup_default_and_delete() {
    let db = HranaMock::start().await.db().await;
    assert!(db.default_agent().await.is_err());
    db.upsert_agent(&agent("a1", "alpha", true, None)).await.unwrap();
    db.upsert_agent(&agent("b1", "beta", false, Some("a1"))).await.unwrap();
    assert_eq!(db.agents().await.unwrap().len(), 2);
    assert_eq!(db.default_agent().await.unwrap().id, "a1");
    // Handle lookup ignores the @ and case.
    assert_eq!(db.agent_by_handle("@Alpha").await.unwrap().unwrap().id, "a1");
    assert!(db.agent_by_handle("nobody").await.unwrap().is_none());
    assert_eq!(db.agent("b1").await.unwrap().unwrap().manager_id.as_deref(), Some("a1"));
    // Update in place (same id) changes the name.
    let mut b = db.agent("b1").await.unwrap().unwrap();
    b.name = "Renamed".into();
    db.upsert_agent(&b).await.unwrap();
    assert_eq!(db.agent("b1").await.unwrap().unwrap().name, "Renamed");
    // Switching the default clears the old one.
    let mut b = db.agent("b1").await.unwrap().unwrap();
    b.is_default = true;
    db.upsert_agent(&b).await.unwrap();
    assert_eq!(db.default_agent().await.unwrap().id, "b1");
    assert!(!db.agent("a1").await.unwrap().unwrap().is_default);
    // Deleting a manager detaches its reports.
    db.upsert_agent(&agent("c1", "gamma", false, Some("b1"))).await.unwrap();
    db.delete_agent("b1").await.unwrap();
    assert!(db.agent("b1").await.unwrap().is_none());
    assert!(db.agent("c1").await.unwrap().unwrap().manager_id.is_none());
    assert_eq!(db.agents().await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_and_messages() {
    let db = HranaMock::start().await.db().await;
    db.upsert_agent(&agent("a1", "alpha", true, None)).await.unwrap();
    let s = db.create_session("a1", "web", None, None, "").await.unwrap();
    assert_eq!(s.channel, "web");
    assert!(s.title.is_empty());
    let d = db.create_session("a1", "delegation", None, Some(&s.id), "child").await.unwrap();
    assert_eq!(d.parent_session_id.as_deref(), Some(s.id.as_str()));
    let listed = db.sessions(50).await.unwrap();
    assert_eq!(listed.len(), 1, "delegation sessions are not listed");
    assert_eq!(listed[0].id, s.id);
    assert_eq!(db.session(&d.id).await.unwrap().unwrap().channel, "delegation");
    assert!(db.session("missing").await.unwrap().is_none());

    db.update_session_title(&s.id, "Hello").await.unwrap();
    assert_eq!(db.session(&s.id).await.unwrap().unwrap().title, "Hello");

    let m1 = db.append_message(&s.id, "user", &[Part::Text { text: "hi".into() }], None).await.unwrap();
    let m2 = db.append_message(&s.id, "assistant", &[Part::Text { text: "yo".into() }], Some("a1")).await.unwrap();
    let m3 = db
        .append_message(
            &s.id,
            "tool",
            &[Part::ToolResult { id: "c1".into(), name: "t".into(), content: "ok".into(), is_error: false }],
            Some("a1"),
        )
        .await
        .unwrap();
    assert_eq!((m1.seq, m2.seq, m3.seq), (1, 2, 3));
    assert_eq!(m2.agent_id.as_deref(), Some("a1"));
    let msgs = db.messages(&s.id, 0).await.unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2].parts, m3.parts);
    assert_eq!(db.messages(&s.id, 2).await.unwrap().len(), 2);
    assert_eq!(db.recent_messages(&s.id, 1).await.unwrap()[0].seq, 3);
    assert_eq!(db.message_count(&s.id).await.unwrap(), 3);
    // Other sessions keep their own sequence.
    let other = db.append_message(&d.id, "user", &[Part::Text { text: "x".into() }], None).await.unwrap();
    assert_eq!(other.seq, 1);

    db.set_session_summary(&s.id, "summary", 2).await.unwrap();
    let s2 = db.session(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.summary.as_deref(), Some("summary"));
    assert_eq!(s2.summary_upto, 2);
    db.clear_session(&s.id).await.unwrap();
    assert_eq!(db.message_count(&s.id).await.unwrap(), 0);
    assert!(db.session(&s.id).await.unwrap().unwrap().summary.is_none());

    db.delete_session(&d.id).await.unwrap();
    assert!(db.session(&d.id).await.unwrap().is_none());
    assert_eq!(db.message_count(&d.id).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_versions_and_names() {
    let db = HranaMock::start().await.db().await;
    let k = db.save_skill(" Deploy Notes ", "when deploying", "# v1", &["ops".into()], "user", None).await.unwrap();
    assert_eq!(k.name, "deploy-notes");
    assert_eq!(k.version, 1);
    assert_eq!(k.source, "user");
    assert_eq!(k.tags, vec!["ops".to_string()]);
    assert!(k.enabled);
    assert!(db.save_skill("***", "d", "b", &[], "user", None).await.is_err());

    let k2 = db.save_skill("deploy-notes", "when deploying v2", "# v2", &[], "agent", Some("a1")).await.unwrap();
    assert_eq!(k2.id, k.id);
    assert_eq!(k2.version, 2);
    assert_eq!(k2.body, "# v2");
    // Source and author of the original are kept on update.
    assert_eq!(k2.source, "user");
    let versions = db.skill_versions("deploy-notes").await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["version"], json!(1));
    assert_eq!(versions[0]["body"], json!("# v1"));

    assert_eq!(db.skills().await.unwrap().len(), 1);
    assert!(db.skill_by_name("DEPLOY-NOTES").await.unwrap().is_some());
    db.set_skill_enabled("deploy-notes", false).await.unwrap();
    assert!(!db.skill_by_name("deploy-notes").await.unwrap().unwrap().enabled);
    db.mark_skill_used("deploy-notes").await.unwrap();
    let used = db.skill_by_name("deploy-notes").await.unwrap().unwrap();
    assert_eq!(used.uses, 1);
    assert!(used.last_used_at.is_some());
    assert!(used.to_markdown().starts_with("---\nname: deploy-notes\n"));

    db.delete_skill("deploy-notes").await.unwrap();
    assert!(db.skill_by_name("deploy-notes").await.unwrap().is_none());
    assert!(db.skill_versions("deploy-notes").await.unwrap().is_empty());
    assert!(db.skills().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn memories_save_and_search() {
    let db = HranaMock::start().await.db().await;
    let m = db.save_memory(None, "preference", "  User likes green tea  ", &["tea".into()], 9).await.unwrap();
    assert_eq!(m.content, "User likes green tea");
    assert_eq!(m.importance, 5, "importance is clamped to 1..=5");
    let private = db.save_memory(Some("a1"), "fact", "Server is called orion", &[], 1).await.unwrap();
    assert_eq!(private.agent_id.as_deref(), Some("a1"));
    db.save_memory(None, "fact", "unrelated note", &[], 1).await.unwrap();

    assert_eq!(db.memories(None, 10).await.unwrap().len(), 3);
    // Agent scoping: shared + own, not another agent's.
    assert_eq!(db.memories(Some("a1"), 10).await.unwrap().len(), 3);
    assert_eq!(db.memories(Some("zz"), 10).await.unwrap().len(), 2);

    let found = db.search_memories(None, "green tea", 10).await.unwrap();
    assert!(!found.is_empty());
    assert_eq!(found[0].id, m.id);
    let orion = db.search_memories(Some("a1"), "orion", 10).await.unwrap();
    assert!(orion.iter().any(|x| x.id == private.id));
    assert!(!orion.iter().any(|x| x.content == "unrelated note"), "non-matching low-importance memories are dropped");
    assert!(!db.search_memories(Some("zz"), "orion", 10).await.unwrap().iter().any(|x| x.id == private.id));

    db.delete_memory(&m.id).await.unwrap();
    assert_eq!(db.memories(None, 10).await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_servers_upsert_and_status() {
    let db = HranaMock::start().await.db().await;
    let now = kongming::db::now_ms();
    let s = McpServer {
        id: "m1".into(),
        name: "Echo".into(),
        url: "http://127.0.0.1:1/mcp".into(),
        transport: "http".into(),
        auth_type: "token".into(),
        token: Some("secret".into()),
        oauth: OAuthState::default(),
        headers: json!({"x-a": "b"}),
        enabled: true,
        status: "unknown".into(),
        last_error: None,
        tools: json!([]),
        created_at: now,
        updated_at: now,
    };
    db.upsert_mcp_server(&s).await.unwrap();
    let got = db.mcp_server("m1").await.unwrap().unwrap();
    assert_eq!(got.name, "Echo");
    assert_eq!(got.token.as_deref(), Some("secret"));
    assert_eq!(got.headers, json!({"x-a": "b"}));
    assert!(got.has_token());
    assert!(!got.has_oauth_token());
    // The public view never carries the token.
    let public = kongming::mcp::server_public(&got);
    assert!(public.get("token").is_none());
    assert_eq!(public["hasToken"], json!(true));

    let tools = json!([{"name": "echo", "inputSchema": {"type": "object"}}]);
    db.set_mcp_status("m1", "connected", None, Some(&tools)).await.unwrap();
    let got = db.mcp_server("m1").await.unwrap().unwrap();
    assert_eq!(got.status, "connected");
    assert_eq!(got.tools.as_array().map(Vec::len), Some(1));
    db.set_mcp_status("m1", "error", Some("boom"), None).await.unwrap();
    let got = db.mcp_server("m1").await.unwrap().unwrap();
    assert_eq!(got.status, "error");
    assert_eq!(got.last_error.as_deref(), Some("boom"));
    assert_eq!(got.tools.as_array().map(Vec::len), Some(1), "tools are kept when not provided");

    let oauth = OAuthState { access_token: Some("at".into()), ..Default::default() };
    db.set_mcp_oauth("m1", &oauth).await.unwrap();
    assert!(db.mcp_server("m1").await.unwrap().unwrap().has_oauth_token());

    let mut renamed = db.mcp_server("m1").await.unwrap().unwrap();
    renamed.name = "Echo2".into();
    db.upsert_mcp_server(&renamed).await.unwrap();
    assert_eq!(db.mcp_servers().await.unwrap().len(), 1);
    assert_eq!(db.mcp_servers().await.unwrap()[0].name, "Echo2");
    db.delete_mcp_server("m1").await.unwrap();
    assert!(db.mcp_servers().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn cron_jobs_upsert_and_mark_run() {
    let db = HranaMock::start().await.db().await;
    let job = CronJob {
        id: "j1".into(),
        name: "Daily".into(),
        agent_id: "a1".into(),
        schedule: "every:30".into(),
        prompt: "report".into(),
        enabled: true,
        deliver_to: None,
        last_run_at: None,
        next_run_at: Some(123),
        last_status: None,
        created_at: kongming::db::now_ms(),
    };
    db.upsert_cron_job(&job).await.unwrap();
    let jobs = db.cron_jobs().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].next_run_at, Some(123));
    assert!(jobs[0].deliver_to.is_none());

    let updated = CronJob {
        name: "Hourly".into(),
        schedule: "every:60".into(),
        next_run_at: Some(456),
        deliver_to: Some("42".into()),
        ..job.clone()
    };
    db.upsert_cron_job(&updated).await.unwrap();
    let jobs = db.cron_jobs().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "Hourly");
    assert_eq!(jobs[0].schedule, "every:60");
    assert_eq!(jobs[0].next_run_at, Some(456));
    assert_eq!(jobs[0].deliver_to.as_deref(), Some("42"));

    db.mark_cron_run("j1", Some(789), "ok").await.unwrap();
    let jobs = db.cron_jobs().await.unwrap();
    let j = &jobs[0];
    assert!(j.last_run_at.is_some());
    assert_eq!(j.next_run_at, Some(789));
    assert_eq!(j.last_status.as_deref(), Some("ok"));

    db.delete_cron_job("j1").await.unwrap();
    assert!(db.cron_jobs().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_totals() {
    let db = HranaMock::start().await.db().await;
    let empty = db.usage_since(0).await.unwrap();
    assert_eq!(empty.requests, 0);
    db.record_usage(Some("s1"), Some("a1"), "m", 10, 2).await.unwrap();
    db.record_usage(None, None, "m", 20, 5).await.unwrap();
    let t = db.usage_since(0).await.unwrap();
    assert_eq!(t.prompt_tokens, 30);
    assert_eq!(t.completion_tokens, 7);
    assert_eq!(t.requests, 2);
    let future = db.usage_since(kongming::db::now_ms() + 60_000).await.unwrap();
    assert_eq!(future.requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_and_improvements() {
    let db = HranaMock::start().await.db().await;
    let t = db.create_task("s1", "a1", "b1", "find x").await.unwrap();
    assert_eq!(t.status, "running");
    db.finish_task(&t.id, Some("child"), "done", "result").await.unwrap();
    let tasks = db.tasks(10).await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, "done");
    assert_eq!(tasks[0].child_session_id.as_deref(), Some("child"));
    assert_eq!(tasks[0].result.as_deref(), Some("result"));
    assert!(tasks[0].finished_at.is_some());

    db.log_improvement(Some("a1"), Some("s1"), "skill_created", Some("k1"), "Created skill").await.unwrap();
    db.log_improvement(None, None, "reflection", None, "note").await.unwrap();
    let list = db.improvements(10).await.unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|i| i.kind == "skill_created" && i.ref_id.as_deref() == Some("k1")));
    assert!(list.iter().any(|i| i.kind == "reflection" && i.agent_id.is_none()));
    assert_eq!(db.improvements(1).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn telegram_chat_mapping() {
    let db = HranaMock::start().await.db().await;
    assert!(db.telegram_chat(7).await.unwrap().is_none());
    db.set_telegram_chat(7, "s1", "a1").await.unwrap();
    db.set_telegram_chat(7, "s2", "a2").await.unwrap();
    assert_eq!(db.telegram_chat(7).await.unwrap(), Some(("s2".to_string(), "a2".to_string())));
}
