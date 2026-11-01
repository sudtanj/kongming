//! Turso (hosted libSQL) over its HTTP pipeline API (`/v2/pipeline`).
//!
//! kongming has no local storage: the container root is read-only and every
//! row lives on Turso. The client is plain `reqwest` + JSON, so the binary
//! carries no C SQLite and no libsql runtime. Statements that must be atomic
//! are sent in one pipeline wrapped in BEGIN/COMMIT; a pipeline runs on one
//! connection in order, and an error aborts the rest and rolls the
//! transaction back on close.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use super::sql::{Exec, Rows, Stmt, Val};

pub fn encode(v: &Val) -> Value {
    match v {
        Val::Null => json!({"type": "null"}),
        Val::Int(i) => json!({"type": "integer", "value": i.to_string()}),
        Val::Real(f) => json!({"type": "float", "value": f}),
        Val::Text(s) => json!({"type": "text", "value": s}),
        Val::Blob(b) => {
            use base64::Engine;
            json!({"type": "blob", "base64": base64::engine::general_purpose::STANDARD.encode(b)})
        }
    }
}

pub fn decode(v: &Value) -> Val {
    match v.get("type").and_then(Value::as_str) {
        Some("integer") => v
            .get("value")
            .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_i64()))
            .map(Val::Int)
            .unwrap_or(Val::Null),
        Some("float") => v.get("value").and_then(Value::as_f64).map(Val::Real).unwrap_or(Val::Null),
        Some("text") => Val::Text(v.get("value").and_then(Value::as_str).unwrap_or("").to_string()),
        Some("blob") => {
            use base64::Engine;
            v.get("base64")
                .and_then(Value::as_str)
                .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                .map(Val::Blob)
                .unwrap_or(Val::Null)
        }
        _ => Val::Null,
    }
}

#[derive(Clone)]
pub struct Turso {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl std::fmt::Debug for Turso {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Turso").field("base", &self.base).finish()
    }
}

impl Turso {
    /// `url` is `libsql://<db>-<org>.turso.io` or an `https://` URL; the token is
    /// a Turso database token.
    pub fn new(url: &str, token: &str) -> Result<Turso> {
        let url = url.trim();
        let token = token.trim();
        if url.is_empty() || token.is_empty() {
            bail!(
                "KONGMING_TURSO_DATABASE_URL and KONGMING_TURSO_AUTH_TOKEN are required (kongming keeps no local data)"
            );
        }
        let base = match url.strip_prefix("libsql://") {
            Some(rest) => format!("https://{rest}"),
            None => url.to_string(),
        };
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .use_rustls_tls()
            .build()
            .context("building http client")?;
        Ok(Turso { http, base: base.trim_end_matches('/').to_string(), token: token.to_string() })
    }

    /// Host of the database (never the token), for status pages.
    pub fn host(&self) -> String {
        let rest = self.base.split_once("://").map_or(self.base.as_str(), |(_, r)| r);
        rest.split('/').next().unwrap_or(rest).to_string()
    }

    async fn pipeline(&self, mut reqs: Vec<Value>) -> Result<Vec<Value>> {
        let n = reqs.len();
        reqs.push(json!({"type": "close"}));
        let mut last_err = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
            }
            let resp = self
                .http
                .post(format!("{}/v2/pipeline", self.base))
                .bearer_auth(&self.token)
                .json(&json!({ "requests": reqs }))
                .send()
                .await;
            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(anyhow!(e).context("turso request failed"));
                    continue;
                }
            };
            let status = resp.status();
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            if status.is_server_error() {
                last_err = Some(anyhow!("turso HTTP {status}: {}", err_text(&body)));
                continue;
            }
            if !status.is_success() {
                bail!("turso HTTP {status}: {}", err_text(&body));
            }
            let results =
                body.get("results").and_then(Value::as_array).ok_or_else(|| anyhow!("turso: malformed reply"))?;
            let mut out = Vec::with_capacity(n);
            for r in results.iter().take(n) {
                if r.get("type").and_then(Value::as_str) != Some("ok") {
                    let m = r.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
                    bail!("turso: {m}");
                }
                out.push(r.pointer("/response/result").cloned().unwrap_or(Value::Null));
            }
            return Ok(out);
        }
        Err(last_err.unwrap_or_else(|| anyhow!("turso: unreachable")))
    }

    fn exec_req(sql: &str, args: &[Val]) -> Value {
        json!({"type": "execute", "stmt": {"sql": sql, "args": args.iter().map(encode).collect::<Vec<_>>()}})
    }

    pub async fn execute(&self, sql: &str, args: &[Val]) -> Result<Exec> {
        let res = self.pipeline(vec![Self::exec_req(sql, args)]).await?;
        Ok(parse_exec(res.first().unwrap_or(&Value::Null)))
    }

    pub async fn query(&self, sql: &str, args: &[Val]) -> Result<Rows> {
        let res = self.pipeline(vec![Self::exec_req(sql, args)]).await?;
        Ok(parse_rows(res.first().unwrap_or(&Value::Null)))
    }

    /// Run several statements atomically (one BEGIN/COMMIT pipeline).
    pub async fn batch(&self, stmts: &[Stmt]) -> Result<()> {
        if stmts.is_empty() {
            return Ok(());
        }
        let mut reqs = Vec::with_capacity(stmts.len() + 2);
        reqs.push(Self::exec_req("BEGIN", &[]));
        for (sql, args) in stmts {
            reqs.push(Self::exec_req(sql, args));
        }
        reqs.push(Self::exec_req("COMMIT", &[]));
        self.pipeline(reqs).await?;
        Ok(())
    }

    /// Run a multi-statement SQL script (schema setup).
    pub async fn sequence(&self, sql: &str) -> Result<()> {
        self.pipeline(vec![json!({"type": "sequence", "sql": sql})]).await?;
        Ok(())
    }

    pub async fn ping(&self) -> Result<u64> {
        let started = std::time::Instant::now();
        self.query("SELECT 1", &[]).await?;
        Ok(started.elapsed().as_millis() as u64)
    }
}

#[async_trait::async_trait]
impl super::sql::SqlExecutor for Turso {
    async fn execute(&self, sql: &str, args: &[Val]) -> Result<Exec> {
        Turso::execute(self, sql, args).await
    }
    async fn query(&self, sql: &str, args: &[Val]) -> Result<Rows> {
        Turso::query(self, sql, args).await
    }
    async fn batch(&self, stmts: &[Stmt]) -> Result<()> {
        Turso::batch(self, stmts).await
    }
    async fn sequence(&self, sql: &str) -> Result<()> {
        Turso::sequence(self, sql).await
    }
    fn host(&self) -> String {
        Turso::host(self)
    }
}

fn err_text(body: &Value) -> String {
    body.get("error")
        .map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()))
        .unwrap_or_else(|| "no detail".into())
}

fn parse_exec(v: &Value) -> Exec {
    Exec {
        affected: v.get("affected_row_count").and_then(Value::as_u64).unwrap_or(0),
        last_insert_rowid: v
            .get("last_insert_rowid")
            .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_i64())),
    }
}

fn parse_rows(v: &Value) -> Rows {
    let cols = v
        .get("cols")
        .and_then(Value::as_array)
        .map(|cs| cs.iter().map(|c| c.get("name").and_then(Value::as_str).unwrap_or("").to_string()).collect())
        .unwrap_or_default();
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .map(|r| r.as_array().map(|cells| cells.iter().map(decode).collect()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    Rows { cols, rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_roundtrip() {
        for v in [Val::Null, Val::Int(-5), Val::Real(1.5), Val::Text("héllo".into()), Val::Blob(vec![1, 2, 3])] {
            assert_eq!(decode(&encode(&v)), v);
        }
    }

    #[test]
    fn integer_as_json_number_is_accepted() {
        assert_eq!(decode(&json!({"type": "integer", "value": 42})), Val::Int(42));
    }

    #[test]
    fn rows_lookup_by_name() {
        let rows = parse_rows(&json!({
            "cols": [{"name": "id"}, {"name": "name"}, {"name": "n"}],
            "rows": [[{"type":"text","value":"a"}, {"type":"text","value":"Alice"}, {"type":"integer","value":"3"}]]
        }));
        let r = rows.first().unwrap();
        assert_eq!(r.str("id"), "a");
        assert_eq!(r.str("name"), "Alice");
        assert_eq!(r.i64("n"), 3);
        assert!(r.opt_str("missing").is_none());
        assert_eq!(r.i64("missing"), 0);
    }

    #[test]
    fn libsql_url_becomes_https() {
        let t = Turso::new("libsql://db-org.turso.io", "tok").unwrap();
        assert_eq!(t.host(), "db-org.turso.io");
        assert!(Turso::new("", "tok").is_err());
    }
}
