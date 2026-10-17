//! Minimal Server-Sent Events parser over a byte stream. Handles multi-line
//! `data:` fields, `event:` names, comments and CRLF, and yields one event per
//! blank line. Used for LLM streaming and MCP transports alike.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
    pub id: Option<String>,
}

#[derive(Default)]
pub struct SseParser {
    buf: String,
    cur: SseEvent,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every complete event found so far.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=pos).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if self.has_data || !self.cur.event.is_empty() {
                    let mut ev = std::mem::take(&mut self.cur);
                    if ev.data.ends_with('\n') {
                        ev.data.pop();
                    }
                    out.push(ev);
                }
                self.cur = SseEvent::default();
                self.has_data = false;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => self.cur.event = value.to_string(),
                "data" => {
                    self.cur.data.push_str(value);
                    self.cur.data.push('\n');
                    self.has_data = true;
                }
                "id" => self.cur.id = Some(value.to_string()),
                _ => {}
            }
        }
        out
    }

    /// Flush a trailing event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if self.has_data {
            let mut ev = std::mem::take(&mut self.cur);
            if ev.data.ends_with('\n') {
                ev.data.pop();
            }
            self.has_data = false;
            Some(ev)
        } else {
            None
        }
    }
}

/// Turn a reqwest byte stream into a stream of SSE events.
pub fn events<S>(bytes: S) -> impl Stream<Item = anyhow::Result<SseEvent>> + Send
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    let parser = SseParser::new();
    futures_util::stream::unfold((bytes.boxed(), parser, std::collections::VecDeque::<SseEvent>::new(), false), |(mut src, mut parser, mut pending, mut done)| async move {
        loop {
            if let Some(ev) = pending.pop_front() {
                return Some((Ok(ev), (src, parser, pending, done)));
            }
            if done {
                return None;
            }
            match src.next().await {
                Some(Ok(chunk)) => pending.extend(parser.feed(&chunk)),
                Some(Err(e)) => {
                    done = true;
                    return Some((Err(anyhow::anyhow!(e).context("stream read failed")), (src, parser, pending, done)));
                }
                None => {
                    done = true;
                    if let Some(ev) = parser.finish() {
                        pending.push_back(ev);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks() {
        let mut p = SseParser::new();
        let mut evs = p.feed(b"event: message_start\ndata: {\"a\":1}\n\ndata: part1\nda");
        evs.extend(p.feed(b"ta: part2\r\n\r\n: comment\n"));
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event, "message_start");
        assert_eq!(evs[0].data, "{\"a\":1}");
        assert_eq!(evs[1].event, "");
        assert_eq!(evs[1].data, "part1\npart2");
        assert!(p.finish().is_none());
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: [DONE]").is_empty());
        assert_eq!(p.finish().unwrap().data, "[DONE]");
    }
}
