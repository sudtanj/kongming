//! Recent log lines kept in memory for the debug MCP server (`web/debug_mcp.rs`).
//! Capped by bytes and lines, oldest dropped first; nothing is written to disk.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::OnceLock;

use parking_lot::Mutex;
use time::format_description::well_known::Rfc3339;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const MAX_BYTES: usize = 512 * 1024;
const MAX_LINES: usize = 4000;
const MAX_LINE_CHARS: usize = 2000;

#[derive(Clone)]
pub struct Entry {
    pub at_ms: i64,
    pub level: Level,
    pub target: String,
    pub text: String,
}

impl Entry {
    fn size(&self) -> usize {
        self.target.len() + self.text.len() + 48
    }

    pub fn line(&self) -> String {
        let at = time::OffsetDateTime::from_unix_timestamp_nanos(self.at_ms as i128 * 1_000_000)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .format(&Rfc3339)
            .unwrap_or_default();
        format!("{at} {:<5} {}: {}", self.level, self.target, self.text)
    }
}

pub struct LogBuffer {
    entries: Mutex<(VecDeque<Entry>, usize)>,
    max_bytes: usize,
    max_lines: usize,
}

impl LogBuffer {
    pub fn new(max_bytes: usize, max_lines: usize) -> LogBuffer {
        LogBuffer { entries: Mutex::new((VecDeque::new(), 0)), max_bytes, max_lines }
    }

    pub fn push(&self, e: Entry) {
        let mut g = self.entries.lock();
        g.1 += e.size();
        g.0.push_back(e);
        while g.1 > self.max_bytes || g.0.len() > self.max_lines {
            let Some(old) = g.0.pop_front() else { break };
            g.1 -= old.size();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The newest `limit` entries at `max_level` or more severe, newer than
    /// `since_ms` and containing `contains` (case-insensitive), oldest first.
    pub fn query(&self, max_level: Level, contains: Option<&str>, since_ms: i64, limit: usize) -> Vec<Entry> {
        let needle = contains.map(str::to_lowercase);
        let g = self.entries.lock();
        let mut out: Vec<Entry> = g
            .0
            .iter()
            .rev()
            .filter(|e| e.level <= max_level && e.at_ms >= since_ms)
            .filter(|e| {
                let hit = |s: &str| s.to_lowercase().contains(needle.as_deref().unwrap_or_default());
                needle.is_none() || hit(&e.text) || hit(&e.target)
            })
            .take(limit)
            .cloned()
            .collect();
        out.reverse();
        out
    }
}

pub fn buffer() -> &'static LogBuffer {
    static BUFFER: OnceLock<LogBuffer> = OnceLock::new();
    BUFFER.get_or_init(|| LogBuffer::new(MAX_BYTES, MAX_LINES))
}

/// Tracing layer that copies every event the subscriber lets through into `buffer()`.
pub struct LogLayer;

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut f = Fields::default();
        event.record(&mut f);
        let mut text = f.message + &f.rest;
        if let Some((i, _)) = text.char_indices().nth(MAX_LINE_CHARS) {
            text.truncate(i);
        }
        buffer().push(Entry {
            at_ms: crate::db::now_ms(),
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: Level, text: &str, at_ms: i64) -> Entry {
        Entry { at_ms, level, target: "t".into(), text: text.into() }
    }

    #[test]
    fn drops_oldest_past_the_line_cap() {
        let b = LogBuffer::new(usize::MAX, 3);
        for i in 0..5 {
            b.push(entry(Level::INFO, &format!("m{i}"), i));
        }
        let texts: Vec<String> = b.query(Level::TRACE, None, 0, 10).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["m2", "m3", "m4"]);
    }

    #[test]
    fn drops_oldest_past_the_byte_cap() {
        let b = LogBuffer::new(300, usize::MAX);
        for i in 0..20 {
            b.push(entry(Level::INFO, &format!("{i:0>50}"), i));
        }
        assert!(b.len() < 20 && !b.is_empty());
        assert_eq!(b.query(Level::TRACE, None, 0, 100).last().unwrap().at_ms, 19);
        assert!(b.entries.lock().1 <= 300);
    }

    #[test]
    fn filters_by_level_text_and_age() {
        let b = LogBuffer::new(usize::MAX, 100);
        b.push(entry(Level::ERROR, "disk Failed", 1));
        b.push(entry(Level::WARN, "slow", 2));
        b.push(entry(Level::INFO, "hello", 3));
        b.push(entry(Level::DEBUG, "noise", 4));
        assert_eq!(b.query(Level::WARN, None, 0, 10).len(), 2);
        assert_eq!(b.query(Level::TRACE, Some("FAILED"), 0, 10).len(), 1);
        assert_eq!(b.query(Level::TRACE, None, 3, 10).len(), 2);
        assert_eq!(b.query(Level::TRACE, None, 0, 1)[0].text, "noise");
    }
}
