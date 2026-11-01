//! The store's view of SQL: values, rows and the executor trait the host
//! implements. Native: Turso over its HTTP pipeline (`turso.rs`). A Worker:
//! the Durable Object's own SQLite. The schema is plain SQLite either way.

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;

/// A SQLite value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<&str> for Val {
    fn from(s: &str) -> Self {
        Val::Text(s.to_string())
    }
}
impl From<String> for Val {
    fn from(s: String) -> Self {
        Val::Text(s)
    }
}
impl From<i64> for Val {
    fn from(i: i64) -> Self {
        Val::Int(i)
    }
}
impl From<i32> for Val {
    fn from(i: i32) -> Self {
        Val::Int(i as i64)
    }
}
impl From<bool> for Val {
    fn from(b: bool) -> Self {
        Val::Int(b as i64)
    }
}
impl From<f64> for Val {
    fn from(f: f64) -> Self {
        Val::Real(f)
    }
}
impl<T: Into<Val>> From<Option<T>> for Val {
    fn from(o: Option<T>) -> Self {
        match o {
            Some(v) => v.into(),
            None => Val::Null,
        }
    }
}
impl From<&Value> for Val {
    fn from(v: &Value) -> Self {
        Val::Text(v.to_string())
    }
}

/// Result rows of a query, with column names for lookup by name.
#[derive(Debug, Default, Clone)]
pub struct Rows {
    pub cols: Vec<String>,
    pub rows: Vec<Vec<Val>>,
}

/// One row borrowed from `Rows`, with typed getters by column name.
pub struct Row<'a> {
    cols: &'a [String],
    vals: &'a [Val],
}

impl<'a> Row<'a> {
    fn idx(&self, col: &str) -> Option<usize> {
        self.cols.iter().position(|c| c == col)
    }
    pub fn val(&self, col: &str) -> &Val {
        self.idx(col).and_then(|i| self.vals.get(i)).unwrap_or(&Val::Null)
    }
    pub fn str(&self, col: &str) -> String {
        match self.val(col) {
            Val::Text(s) => s.clone(),
            Val::Int(i) => i.to_string(),
            Val::Real(f) => f.to_string(),
            _ => String::new(),
        }
    }
    pub fn opt_str(&self, col: &str) -> Option<String> {
        match self.val(col) {
            Val::Text(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        }
    }
    pub fn i64(&self, col: &str) -> i64 {
        match self.val(col) {
            Val::Int(i) => *i,
            Val::Real(f) => *f as i64,
            Val::Text(s) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }
    pub fn opt_i64(&self, col: &str) -> Option<i64> {
        match self.val(col) {
            Val::Null => None,
            _ => Some(self.i64(col)),
        }
    }
    pub fn bool(&self, col: &str) -> bool {
        self.i64(col) != 0
    }
    pub fn f64(&self, col: &str) -> f64 {
        match self.val(col) {
            Val::Real(f) => *f,
            Val::Int(i) => *i as f64,
            Val::Text(s) => s.parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }
    pub fn json(&self, col: &str) -> Value {
        match self.val(col) {
            Val::Text(s) => serde_json::from_str(s).unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }
}

impl Rows {
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(move |vals| Row { cols: &self.cols, vals })
    }
    pub fn first(&self) -> Option<Row<'_>> {
        self.iter().next()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Exec {
    pub affected: u64,
    pub last_insert_rowid: Option<i64>,
}

pub type Stmt = (String, Vec<Val>);

/// What the host implements, at the statement and batch level.
#[async_trait::async_trait]
pub trait SqlExecutor: Send + Sync {
    /// Run one statement that changes rows.
    async fn execute(&self, sql: &str, args: &[Val]) -> Result<Exec>;
    /// Run one statement and return its rows.
    async fn query(&self, sql: &str, args: &[Val]) -> Result<Rows>;
    /// Run statements atomically: all of them or none.
    async fn batch(&self, stmts: &[Stmt]) -> Result<()>;
    /// Run a script of several `;`-separated statements (the schema).
    async fn sequence(&self, sql: &str) -> Result<()>;
    /// Where the data lives, for status pages (never a credential).
    fn host(&self) -> String;
}

/// Cheap to clone handle on the host's executor.
#[derive(Clone)]
pub struct Sql(Arc<dyn SqlExecutor>);

impl std::fmt::Debug for Sql {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sql").field("host", &self.0.host()).finish()
    }
}

impl Sql {
    pub fn new(exec: impl SqlExecutor + 'static) -> Sql {
        Sql(Arc::new(exec))
    }

    pub fn from_arc(exec: Arc<dyn SqlExecutor>) -> Sql {
        Sql(exec)
    }

    pub async fn execute(&self, sql: &str, args: &[Val]) -> Result<Exec> {
        self.0.execute(sql, args).await
    }

    pub async fn query(&self, sql: &str, args: &[Val]) -> Result<Rows> {
        self.0.query(sql, args).await
    }

    pub async fn batch(&self, stmts: &[Stmt]) -> Result<()> {
        self.0.batch(stmts).await
    }

    pub async fn sequence(&self, sql: &str) -> Result<()> {
        self.0.sequence(sql).await
    }

    pub fn host(&self) -> String {
        self.0.host()
    }

    /// Round trip time of a trivial query, in milliseconds.
    pub async fn ping(&self) -> Result<u64> {
        let started = crate::platform::now_ms();
        self.query("SELECT 1", &[]).await?;
        Ok((crate::platform::now_ms() - started).max(0) as u64)
    }
}
