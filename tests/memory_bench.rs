//! Memory and latency benchmark: the release binary against the in-memory Turso (Hrana) mock
//! and an LLM mock that streams like a slow model. Ignored by `cargo test`; run it with
//! `cargo test --release --test memory_bench -- --ignored --nocapture` (Linux only, it reads
//! `/proc`). `.github/workflows/bench.yml` runs it on an arm64 runner and writes the tables
//! to the job summary and the README.
#![cfg(target_os = "linux")]

mod support;

use std::convert::Infallible;
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::TcpListener as StdListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use support::HranaMock;

const BIN: &str = env!("CARGO_BIN_EXE_kongming");
const PASSWORD: &str = "bench";

/// The mock answers with this many content chunks, `CHUNK_DELAY` apart.
const CHUNKS: usize = 8;
const CHUNK_DELAY: Duration = Duration::from_millis(25);
const WARMUP: usize = 10;
const REQUESTS: usize = 300;
const CONCURRENCY: [usize; 3] = [5, 20, 50];

async fn chat(_body: Bytes) -> Response {
    let chunk = |choices: Value| json!({"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 0, "model": "bench-model", "choices": choices});
    let events = futures_util::stream::unfold(0usize, move |i| async move {
        let event = match i {
            0 => chunk(json!([{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}])),
            n if n <= CHUNKS => {
                tokio::time::sleep(CHUNK_DELAY).await;
                chunk(json!([{"index": 0, "delta": {"content": "pong "}, "finish_reason": null}]))
            }
            n if n == CHUNKS + 1 => chunk(json!([{"index": 0, "delta": {}, "finish_reason": "stop"}])),
            n if n == CHUNKS + 2 => {
                let mut c = chunk(json!([]));
                c["usage"] = json!({"prompt_tokens": 4000, "completion_tokens": CHUNKS, "total_tokens": 4000 + CHUNKS});
                c
            }
            n if n == CHUNKS + 3 => return Some((Ok::<_, Infallible>("data: [DONE]\n\n".to_string()), i + 1)),
            _ => return None,
        };
        Some((Ok(format!("data: {event}\n\n")), i + 1))
    });
    ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(events)).into_response()
}

/// A running daemon; killed on drop. Its database is the mock's in-memory SQLite.
struct Instance {
    child: Child,
    base: String,
    _hrana: HranaMock,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start_daemon(llm: &str, workers: &str) -> Instance {
    let hrana = HranaMock::start().await;
    let db = hrana.db().await;
    // What the dashboard saves; the background loops are off so they do not add LLM calls.
    let llm_setting =
        json!({"kind": "openai", "baseUrl": format!("{llm}/v1"), "apiKey": "bench", "model": "bench-model"});
    db.set_setting("llm", &llm_setting).await.unwrap();
    for key in ["reflection", "proactive", "earn"] {
        db.set_setting(key, &json!({"enabled": false})).await.unwrap();
    }

    let port = StdListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut cmd = Command::new(BIN);
    cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--turso-url", &hrana.url])
        .args(["--turso-token", "bench", "--password", PASSWORD, "--secret", "bench-secret-bench-secret"])
        .env("LOG_LEVEL", "warn")
        .env("KONGMING_WORKERS", workers)
        .stdout(Stdio::null());
    for var in [
        "KONGMING_HOST",
        "PORT",
        "KONGMING_PUBLIC_URL",
        "KONGMING_SHELL",
        "KONGMING_CONTEXT_TOKENS",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        cmd.env_remove(var);
    }
    let base = format!("http://127.0.0.1:{port}");
    let mut instance = Instance { child: cmd.spawn().unwrap(), base, _hrana: hrana };

    let http = reqwest::Client::new();
    for _ in 0..1500 {
        if let Some(status) = instance.child.try_wait().unwrap() {
            panic!("kongming exited early: {status}");
        }
        if http.get(format!("{}/health", instance.base)).send().await.is_ok_and(|r| r.status().is_success()) {
            return instance;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("kongming did not become ready on {}", instance.base);
}

/// A client with the dashboard's session cookie.
async fn login(base: &str) -> reqwest::Client {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/auth/login"))
        .json(&json!({"password": PASSWORD}))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let token = body["token"].as_str().expect("token in login reply");
    let mut headers = reqwest::header::HeaderMap::new();
    let cookie = format!("{}={token}", kongming::web::auth::COOKIE);
    headers.insert(reqwest::header::COOKIE, reqwest::header::HeaderValue::from_str(&cookie).unwrap());
    reqwest::Client::builder().default_headers(headers).build().unwrap()
}

/// Resident and peak resident set size (KiB) and thread count, from `/proc/<pid>/status`.
struct Mem {
    rss_kb: u64,
    hwm_kb: u64,
    threads: u64,
}

fn mem(pid: u32) -> Mem {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    Mem { rss_kb: field("VmRSS:"), hwm_kb: field("VmHWM:"), threads: field("Threads:") }
}

/// Polls RSS every 10 ms on its own thread and keeps the highest value seen.
struct Sampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: std::thread::JoinHandle<()>,
}

impl Sampler {
    fn start(pid: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let (s, p) = (stop.clone(), peak.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                p.fetch_max(mem(pid).rss_kb, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Sampler { stop, peak, thread }
    }

    fn finish(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().unwrap();
        self.peak.load(Ordering::Relaxed)
    }
}

/// One chat as the dashboard starts it: a new session, then a message answered in full.
/// Only the turn is timed.
async fn one(http: &reqwest::Client, base: &str) -> Result<Duration, String> {
    let resp = http.post(format!("{base}/api/sessions")).json(&json!({})).send().await.map_err(|e| e.to_string())?;
    let session: Value = resp.json().await.map_err(|e| e.to_string())?;
    let id = session["id"].as_str().ok_or_else(|| format!("no session id: {session}"))?;
    let started = Instant::now();
    let resp = http
        .post(format!("{base}/api/sessions/{id}/messages?wait=true"))
        .json(&json!({"text": "ping"}))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() || !body["text"].as_str().unwrap_or("").contains("pong") {
        return Err(format!("{status}: {}", body.to_string().chars().take(300).collect::<String>()));
    }
    Ok(started.elapsed())
}

struct Run {
    concurrency: usize,
    requests: usize,
    errors: Vec<String>,
    elapsed: Duration,
    latencies: Vec<Duration>,
    peak_kb: u64,
}

impl Run {
    fn percentile(&self, p: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }
        self.latencies[((self.latencies.len() - 1) as f64 * p).round() as usize]
    }
}

async fn load(pid: u32, http: &reqwest::Client, base: &str, concurrency: usize, requests: usize) -> Run {
    let sampler = Sampler::start(pid);
    let started = Instant::now();
    let results: Vec<Result<Duration, String>> =
        futures_util::stream::iter(0..requests).map(|_| one(http, base)).buffer_unordered(concurrency).collect().await;
    let elapsed = started.elapsed();
    let peak_kb = sampler.finish();
    let (mut latencies, mut errors) = (Vec::new(), Vec::new());
    for r in results {
        match r {
            Ok(d) => latencies.push(d),
            Err(e) => errors.push(e),
        }
    }
    latencies.sort();
    Run { concurrency, requests, errors, elapsed, latencies, peak_kb }
}

fn mib(kb: u64) -> String {
    format!("{:.1} MiB", kb as f64 / 1024.0)
}

fn ms(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

/// What the README's summary line quotes from one benchmarked process.
struct Summary {
    workers: String,
    ready: Duration,
    idle_kb: u64,
    concurrency: usize,
    peak_kb: u64,
}

/// Benchmarks one daemon and returns its markdown report, error count and summary.
async fn bench(llm: &str, workers: &str) -> (String, usize, Summary) {
    let started = Instant::now();
    let daemon = start_daemon(llm, workers).await;
    let ready = started.elapsed();
    let pid = daemon.child.id();
    let http = login(&daemon.base).await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    let idle = mem(pid);
    let mut runs = vec![load(pid, &http, &daemon.base, 1, WARMUP).await];
    tokio::time::sleep(Duration::from_secs(2)).await;
    let warm = mem(pid);
    for c in CONCURRENCY {
        runs.push(load(pid, &http, &daemon.base, c, REQUESTS).await);
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    let after = mem(pid);

    let mut md = String::new();
    let _ = writeln!(md, "### {workers} worker thread(s)\n");
    let _ = writeln!(md, "Ready {} after spawn (schema and starter skills applied).\n", ms(ready));
    let _ = writeln!(md, "| State | RSS | Threads |\n|---|---:|---:|");
    let _ = writeln!(md, "| Idle, 2 s after start | {} | {} |", mib(idle.rss_kb), idle.threads);
    let _ = writeln!(md, "| Idle after {WARMUP} sequential chats | {} | {} |", mib(warm.rss_kb), warm.threads);
    let _ = writeln!(md, "| Idle, 5 s after the load runs | {} | {} |", mib(after.rss_kb), after.threads);
    let _ = writeln!(md, "| Peak for the whole run (VmHWM) | {} | |\n", mib(after.hwm_kb));
    let _ = writeln!(md, "| Concurrent chats | Chats | Errors | Chats/s | p50 | p99 | Peak RSS |");
    let _ = writeln!(md, "|---:|---:|---:|---:|---:|---:|---:|");
    for r in &runs {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {:.0} | {} | {} | {} |",
            r.concurrency,
            r.requests,
            r.errors.len(),
            r.latencies.len() as f64 / r.elapsed.as_secs_f64(),
            ms(r.percentile(0.5)),
            ms(r.percentile(0.99)),
            mib(r.peak_kb),
        );
    }
    let errors: Vec<&String> = runs.iter().flat_map(|r| &r.errors).collect();
    if let Some(first) = errors.first() {
        let _ = writeln!(md, "\nFirst error: `{first}`");
    }
    md.push('\n');
    let last = runs.last().unwrap();
    let summary = Summary {
        workers: workers.to_string(),
        ready,
        idle_kb: idle.rss_kb,
        concurrency: last.concurrency,
        peak_kb: last.peak_kb,
    };
    (md, errors.len(), summary)
}

/// Replaces the text between `<!-- bench:{name} -->` and `<!-- /bench:{name} -->`.
fn replace_marked(text: &str, name: &str, body: &str) -> String {
    let (open, close) = (format!("<!-- bench:{name} -->"), format!("<!-- /bench:{name} -->"));
    let start = text.find(&open).unwrap_or_else(|| panic!("{open} is missing from the README")) + open.len();
    let end = start + text[start..].find(&close).unwrap_or_else(|| panic!("{close} is missing from the README"));
    format!("{}{body}{}", &text[..start], &text[end..])
}

#[test]
fn replace_marked_keeps_the_markers() {
    let text = "a <!-- bench:x -->old<!-- /bench:x --> b";
    assert_eq!(replace_marked(text, "x", "new"), "a <!-- bench:x -->new<!-- /bench:x --> b");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; run with --release -- --ignored"]
async fn memory_and_latency() {
    let llm = support::serve(Router::new().route("/v1/chat/completions", post(chat))).await;
    let workers = std::env::var("BENCH_WORKERS").unwrap_or_else(|_| "1".into());
    let size_mib = std::fs::metadata(BIN).map(|m| m.len()).unwrap_or(0) as f64 / (1024.0 * 1024.0);
    let sha = std::env::var("GITHUB_SHA").unwrap_or_default();
    let commit = if sha.is_empty() { String::new() } else { format!(" at commit `{}`", &sha[..sha.len().min(7)]) };

    let mut md = String::new();
    let _ = writeln!(
        md,
        "Measured {} on {}{commit}. Binary {size_mib:.1} MiB. Each chat is a new session and one message answered \
         in full by the default agent (system prompt, skills index and tool schemas included). The mock model \
         streams {CHUNKS} chunks {} ms apart (about {} ms per reply); the Turso mock runs on the same machine, so \
         a real Turso adds its round trips to every turn.\n",
        time::OffsetDateTime::now_utc().date(),
        std::env::consts::ARCH,
        CHUNK_DELAY.as_millis(),
        CHUNK_DELAY.as_millis() * CHUNKS as u128,
    );
    let mut errors = 0;
    let mut summaries = Vec::new();
    for w in workers.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        let (section, e, summary) = bench(&llm, w).await;
        md.push_str(&section);
        errors += e;
        summaries.push(summary);
    }

    println!("{md}");
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY") {
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path).unwrap();
        writeln!(f, "## kongming memory benchmark\n\n{md}").unwrap();
    }
    assert_eq!(errors, 0, "some benchmark chats failed; see the report above");

    // The workflow passes BENCH_README=README.md to refresh the numbers quoted there.
    if let (Ok(path), Some(s)) = (std::env::var("BENCH_README"), summaries.first()) {
        let line = format!(
            "Measured: a {size_mib:.1} MiB binary that is ready {} after start and idles at {} RSS; {} concurrent \
             chats peak at {} ({} worker thread(s); see [Benchmark](#benchmark)).",
            ms(s.ready),
            mib(s.idle_kb),
            s.concurrency,
            mib(s.peak_kb),
            s.workers,
        );
        let readme = std::fs::read_to_string(&path).unwrap();
        let readme = replace_marked(&readme, "summary", &line);
        let readme = replace_marked(&readme, "report", &format!("\n{}\n", md.trim()));
        std::fs::write(&path, readme).unwrap();
    }
}
