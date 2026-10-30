//! The `curl` and `wget` the binary doubles as for the shell tool, against a
//! local HTTP server: the body goes to stdout, statuses become exit codes,
//! and nothing is ever written to a file.

mod support;

use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Redirect;
use axum::routing::{any, get};
use axum::Router;

async fn echo(method: Method, uri: Uri, headers: HeaderMap, body: String) -> String {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("-").to_string();
    let query = uri.query().unwrap_or("-");
    format!("{method} {} {} {query} {body}", header("content-type"), header("x-test"))
}

async fn server() -> String {
    let router = Router::new()
        .route("/hello", get(|| async { "hello world" }))
        .route("/missing", get(|| async { (StatusCode::NOT_FOUND, "nope") }))
        .route("/redirect", get(|| async { Redirect::to("/hello") }))
        .route("/echo", any(echo));
    support::serve(router).await
}

/// Run `prog args...`; returns the exit code, stdout and stderr.
async fn fetch(prog: &str, args: &[&str]) -> (i32, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = args.iter().map(|a| a.to_string()).collect();
    let code = support::within(kongming::fetch::run(prog, args, &mut out, &mut err)).await;
    (code, String::from_utf8_lossy(&out).into_owned(), String::from_utf8_lossy(&err).into_owned())
}

#[tokio::test]
async fn bodies_go_to_stdout() {
    let base = server().await;
    let url = format!("{base}/hello");
    assert_eq!(fetch("curl", &[&url]).await, (0, "hello world".into(), String::new()));
    assert_eq!(fetch("curl", &["-sSL", &url]).await, (0, "hello world".into(), String::new()));
    assert_eq!(fetch("wget", &["-qO-", &url]).await, (0, "hello world".into(), String::new()));
    // No -O: still stdout, since nothing can be saved.
    assert_eq!(fetch("wget", &[&url]).await, (0, "hello world".into(), String::new()));
    // Without a scheme the URL is http.
    let bare = url.trim_start_matches("http://").to_string();
    assert_eq!(fetch("curl", &["-s", &bare]).await.1, "hello world");
}

#[tokio::test]
async fn http_errors_become_exit_codes() {
    let base = server().await;
    let url = format!("{base}/missing");
    let (code, out, err) = fetch("curl", &["-sSf", &url]).await;
    assert_eq!((code, out.as_str()), (22, ""));
    assert!(err.contains("404"), "{err}");
    let (code, out, err) = fetch("curl", &["-sf", &url]).await;
    assert_eq!((code, out.as_str(), err.as_str()), (22, "", ""), "-s without -S stays quiet");
    assert_eq!(fetch("curl", &["-s", &url]).await, (0, "nope".into(), String::new()));
    assert_eq!(fetch("curl", &["-s", "--fail-with-body", &url]).await.0, 22);

    let (code, out, err) = fetch("wget", &["-qO-", &url]).await;
    assert_eq!((code, out.as_str()), (8, ""));
    assert!(err.contains("server returned error: HTTP/1.1 404 Not Found"), "{err}");
    let (code, _, err) = fetch("wget", &["-q", "--spider", &url]).await;
    assert_eq!(code, 8, "{err}");
    assert_eq!(fetch("wget", &["-q", "--spider", &format!("{base}/hello")]).await, (0, String::new(), String::new()));
}

#[tokio::test]
async fn requests_carry_data_and_headers() {
    let base = server().await;
    let url = format!("{base}/echo");
    let (code, out, _) = fetch("curl", &["-sS", "-d", "a=1", "--data-urlencode", "b=x y", &url]).await;
    assert_eq!((code, out.as_str()), (0, "POST application/x-www-form-urlencoded - - a=1&b=x%20y"));
    let (_, out, _) = fetch("curl", &["-sS", "--json", r#"{"k":1}"#, &url]).await;
    assert_eq!(out, r#"POST application/json - - {"k":1}"#);
    let args = ["-sSXPUT", "-HX-Test: yes", "-H", "Content-Type: text/plain", "-d", "hi", url.as_str()];
    let (_, out, _) = fetch("curl", &args).await;
    assert_eq!(out, "PUT text/plain yes - hi");
    let (_, out, _) = fetch("curl", &["-sSG", "-d", "q=1", "--data", "r=2", &url]).await;
    assert_eq!(out, "GET - - q=1&r=2 ");

    let (_, out, _) = fetch("wget", &["-qO-", "--header=X-Test: w", "--post-data=a=1", &url]).await;
    assert_eq!(out, "POST application/x-www-form-urlencoded w - a=1");
    let (_, out, _) = fetch("wget", &["-q", "-O", "-", "--method=DELETE", &url]).await;
    assert_eq!(out, "DELETE - - - ");
}

#[tokio::test]
async fn redirects_and_headers() {
    let base = server().await;
    let url = format!("{base}/redirect");
    // curl stays put unless -L.
    let (code, out, _) = fetch("curl", &["-s", "-w", "%{http_code} %{redirect_url}", &url]).await;
    assert_eq!((code, out), (0, format!("303 {base}/hello")));
    let (_, out, _) = fetch("curl", &["-sL", "-w", r"\n%{http_code} %{url_effective}", &url]).await;
    assert_eq!(out, format!("hello world\n200 {base}/hello"));
    let (code, _, err) = fetch("curl", &["-sSL", "--max-redirs", "0", &url]).await;
    assert_eq!(code, 47, "{err}");
    // wget follows by default.
    assert_eq!(fetch("wget", &["-qO-", &url]).await, (0, "hello world".into(), String::new()));

    let hello = format!("{base}/hello");
    let (_, out, _) = fetch("curl", &["-si", &hello]).await;
    assert!(out.starts_with("HTTP/1.1 200 OK\n"), "{out}");
    assert!(out.contains("\ncontent-type: text/plain"), "{out}");
    assert!(out.ends_with("\n\nhello world"), "{out}");
    let (_, out, _) = fetch("curl", &["-sI", &hello]).await;
    assert!(out.starts_with("HTTP/1.1 200 OK\n") && !out.contains("hello world"), "{out}");
    let (code, out, _) = fetch("curl", &["-s", "-o", "/dev/null", "-w", "%{http_code}", &hello]).await;
    assert_eq!((code, out.as_str()), (0, "200"));
    let (_, out, err) = fetch("wget", &["-qSO-", &hello]).await;
    assert_eq!(out, "hello world");
    assert!(err.contains("  HTTP/1.1 200 OK"), "{err}");
}

#[tokio::test]
async fn files_are_refused_and_failures_reported() {
    let (code, out, err) = fetch("wget", &["-O", "/tmp/page.html", "http://127.0.0.1:1/"]).await;
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("can't write /tmp/page.html") && err.contains("stdout"), "{err}");
    let (code, _, err) = fetch("curl", &["-sS", "-o", "page.html", "http://127.0.0.1:1/"]).await;
    assert_eq!(code, 23, "{err}");
    let (code, _, err) = fetch("curl", &["-sS", "--frobnicate", "http://127.0.0.1:1/"]).await;
    assert_eq!(code, 2, "{err}");

    // Nothing listens on port 1: curl exits 7 and still prints -w; wget exits 4.
    let (code, out, err) = fetch("curl", &["-sS", "-w", "%{http_code}", "http://127.0.0.1:1/"]).await;
    assert_eq!((code, out.as_str()), (7, "000"), "{err}");
    assert!(err.starts_with("curl: (7) "), "{err}");
    assert_eq!(fetch("wget", &["-qO-", "http://127.0.0.1:1/"]).await.0, 4);
}
