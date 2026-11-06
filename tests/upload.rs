//! `POST /api/upload`: text comes back, nothing is kept, odd files are refused.

mod support;

use reqwest::StatusCode;
use serde_json::Value;
use support::{spawn_app, within};

#[tokio::test(flavor = "multi_thread")]
async fn a_text_file_is_read_and_a_binary_refused() {
    let app = spawn_app(vec![]).await;

    let r = within(app.client.post(app.url("/api/upload?name=notes.TXT")).body("hello\nworld").send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["name"], "notes.TXT");
    assert_eq!(body["text"], "hello\nworld");
    assert_eq!(body["chars"], 11);

    for name in ["tool.exe", "notes.txt.exe", "archive.zip", "old.doc"] {
        let url = app.url(&format!("/api/upload?name={name}"));
        let r = within(app.client.post(url).body("hello").send()).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{name}");
        let body: Value = r.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().contains("not accepted"), "{body}");
    }

    // The extension says PDF, the content does not.
    let fake = app.client.post(app.url("/api/upload?name=fake.pdf")).body(b"MZ\x90\x00".to_vec());
    let r = within(fake.send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_needs_a_session() {
    let app = spawn_app(vec![]).await;
    let r = within(app.plain.post(app.url("/api/upload?name=notes.txt")).body("hello").send()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}
