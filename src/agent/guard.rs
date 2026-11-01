//! Tool-loop guardrail, checked after every call: small models repeat a call
//! that just failed, keep hammering a broken tool, or re-read what they
//! already have. The notes it returns are appended to the tool result.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use serde_json::Value;

/// Identical failed calls after which the next one is not run.
const BLOCK_AFTER: usize = 3;
/// Failures in a row of one tool (any arguments) before a warning.
const STREAK_WARN: usize = 3;

#[derive(Debug, Default)]
pub struct LoopGuard {
    /// (tool, arguments hash) -> failures with exactly these arguments.
    failed: HashMap<(String, u64), usize>,
    /// Tool -> failures in a row.
    streak: HashMap<String, usize>,
    /// (tool, arguments hash) -> hash of the last successful result.
    results: HashMap<(String, u64), u64>,
}

impl LoopGuard {
    /// A synthetic error instead of running a call that already failed
    /// `BLOCK_AFTER` times with the same arguments.
    pub fn blocked(&self, name: &str, args: &Value) -> Option<String> {
        let n = self.failed.get(&(name.to_string(), hash(&canonical(args)))).copied().unwrap_or(0);
        (n >= BLOCK_AFTER).then(|| {
            format!(
                "error: not run. You already called {name} with these arguments {n} times and it failed every time. \
Do not call it this way again: change approach, or tell the user what is blocking you."
            )
        })
    }

    /// Record a call's outcome; returns a note for the model, if any.
    pub fn record(&mut self, name: &str, args: &Value, content: &str, is_error: bool) -> Option<String> {
        let key = (name.to_string(), hash(&canonical(args)));
        if is_error {
            let same = self.failed.entry(key).or_insert(0);
            *same += 1;
            let same = *same;
            let streak = self.streak.entry(name.to_string()).or_insert(0);
            *streak += 1;
            if same >= 2 {
                let note = "with these arguments and it failed the same way; change approach or tell the user.";
                return Some(format!("You already called {name} {note}"));
            }
            if *streak >= STREAK_WARN {
                let note = "Stop retrying it: tell the user what you tried and what went wrong.";
                return Some(format!("{name} has failed {streak} times in a row. {note}"));
            }
            return None;
        }
        // Success: the tool works again.
        self.streak.remove(name);
        self.failed.retain(|(tool, _), _| tool != name);
        if !read_only(name, args) {
            return None;
        }
        let digest = hash(content);
        (self.results.insert(key, digest) == Some(digest))
            .then(|| "Same result as before: you already have this, use it instead of calling again.".to_string())
    }
}

/// Calls that only look things up, so that the same answer twice means the
/// model is going round in circles.
fn read_only(name: &str, args: &Value) -> bool {
    if name == "memory" {
        return args.get("action").and_then(Value::as_str) == Some("search");
    }
    let tool = name.rsplit("__").next().unwrap_or(name).to_ascii_lowercase();
    tool.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| ["get", "list", "search", "read", "view", "fetch", "find", "query", "show", "lookup"].contains(&w))
}

/// Arguments with object keys sorted, so key order does not make two calls differ.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            let fields: Vec<String> =
                keys.into_iter().map(|k| format!("{}:{}", Value::from(k.as_str()), canonical(&o[k]))).collect();
            format!("{{{}}}", fields.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

fn hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repeated_failures_are_flagged_then_blocked() {
        let mut g = LoopGuard::default();
        let args = json!({"a": 1, "b": 2});
        let reordered = json!({"b": 2, "a": 1});
        assert_eq!(g.record("f", &args, "boom", true), None);
        let note = g.record("f", &reordered, "boom", true).expect("second failure");
        assert!(note.contains("You already called f with these arguments"), "{note}");
        assert!(g.blocked("f", &args).is_none());
        g.record("f", &args, "boom", true);
        let blocked = g.blocked("f", &reordered).expect("fourth call blocked");
        assert!(blocked.contains("not run"), "{blocked}");
        assert!(g.blocked("f", &json!({"a": 2})).is_none(), "other arguments still run");
        // A success clears the tool's record.
        assert_eq!(g.record("f", &json!({"a": 2}), "ok", false), None);
        assert!(g.blocked("f", &args).is_none());
    }

    #[test]
    fn a_tool_failing_with_different_arguments_gets_a_warning() {
        let mut g = LoopGuard::default();
        assert_eq!(g.record("f", &json!({"n": 1}), "x", true), None);
        assert_eq!(g.record("f", &json!({"n": 2}), "x", true), None);
        let note = g.record("f", &json!({"n": 3}), "x", true).expect("third failure in a row");
        assert!(note.contains("failed 3 times in a row"), "{note}");
        // Another tool's failures are counted on their own.
        assert_eq!(g.record("g", &json!({}), "x", true), None);
    }

    #[test]
    fn the_same_lookup_twice_is_pointed_out() {
        let mut g = LoopGuard::default();
        let q = json!({"action": "search", "query": "tea"});
        assert_eq!(g.record("memory", &q, "- tea", false), None);
        assert!(g.record("memory", &q, "- tea", false).expect("same result").contains("Same result"));
        assert_eq!(g.record("memory", &q, "- coffee", false), None, "a changed result is news");
        // Calls that change something are not compared.
        let add = json!({"action": "add", "content": "x"});
        g.record("memory", &add, "saved", false);
        assert_eq!(g.record("memory", &add, "saved", false), None);
        assert!(read_only("mcp__notes__search_notes", &json!({})));
        assert!(read_only("web_fetch", &json!({})));
        assert!(!read_only("current_time", &json!({})));
    }
}
