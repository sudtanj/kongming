//! Reasoning written into the answer. Models served through OpenAI-compatible
//! endpoints (Qwen, DeepSeek-R1 distills, ...) often put it inline in
//! `content` as `<think>…</think>`; this splits it back out while streaming.
//! An opening tag counts only at the start of the content or of a line, and a
//! tag cut by a chunk boundary is held back until the next chunk decides it.

/// Opening and closing tags, tried in order.
const TAGS: [(&str, &str); 3] =
    [("<think>", "</think>"), ("<thinking>", "</thinking>"), ("<reasoning>", "</reasoning>")];

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Thinking(String),
}

#[derive(Debug, Default)]
pub struct ThinkScrubber {
    /// Received but not emitted yet: a possible tag cut at the chunk boundary.
    pending: String,
    /// Inside a think block: the closing tag that ends it.
    close: Option<&'static str>,
    /// Just after a closing tag: the whitespace that follows it is dropped.
    after_close: bool,
    /// The text emitted so far does not end at a line start.
    mid_line: bool,
}

impl ThinkScrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of content; returns what can be emitted so far.
    pub fn feed(&mut self, chunk: &str) -> Vec<Piece> {
        self.pending.push_str(chunk);
        let mut out = Vec::new();
        loop {
            if let Some(close) = self.close {
                if let Some(pos) = self.pending.find(close) {
                    push(&mut out, Piece::Thinking(self.pending[..pos].to_string()));
                    self.pending.drain(..pos + close.len());
                    self.close = None;
                    self.after_close = true;
                    continue;
                }
                let keep = partial_suffix(&self.pending, close);
                let rest = self.pending.split_off(self.pending.len() - keep);
                push(&mut out, Piece::Thinking(std::mem::replace(&mut self.pending, rest)));
                return out;
            }
            if self.after_close {
                let lead = self.pending.len() - self.pending.trim_start().len();
                self.pending.drain(..lead);
                if self.pending.is_empty() {
                    return out;
                }
                self.after_close = false;
                self.mid_line = false;
            }
            match self.find_open() {
                Some((p, Some(i))) => {
                    let before: String = self.pending.drain(..p).collect();
                    self.emit_text(&mut out, before);
                    self.pending.drain(..TAGS[i].0.len());
                    self.close = Some(TAGS[i].1);
                }
                Some((p, None)) => {
                    let before: String = self.pending.drain(..p).collect();
                    self.emit_text(&mut out, before);
                    return out;
                }
                None => {
                    let all = std::mem::take(&mut self.pending);
                    self.emit_text(&mut out, all);
                    return out;
                }
            }
        }
    }

    /// End of the stream: whatever is held back goes out. A think block that
    /// was never closed stays thinking.
    pub fn finish(&mut self) -> Vec<Piece> {
        let rest = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        if self.close.take().is_some() {
            push(&mut out, Piece::Thinking(rest));
        } else {
            self.emit_text(&mut out, rest);
        }
        out
    }

    /// The first `<` at a line start that opens a tag (`Some(index)`) or may
    /// still become one when more arrives (`None`).
    fn find_open(&self) -> Option<(usize, Option<usize>)> {
        let bytes = self.pending.as_bytes();
        for (p, _) in self.pending.match_indices('<') {
            let line_start = if p == 0 { !self.mid_line } else { bytes[p - 1] == b'\n' };
            if !line_start {
                continue;
            }
            let rest = &self.pending[p..];
            if let Some(i) = TAGS.iter().position(|(open, _)| rest.starts_with(open)) {
                return Some((p, Some(i)));
            }
            if TAGS.iter().any(|(open, _)| open.starts_with(rest)) {
                return Some((p, None));
            }
        }
        None
    }

    fn emit_text(&mut self, out: &mut Vec<Piece>, text: String) {
        if !text.is_empty() {
            self.mid_line = !text.ends_with('\n');
            out.push(Piece::Text(text));
        }
    }
}

fn push(out: &mut Vec<Piece>, piece: Piece) {
    let (Piece::Text(t) | Piece::Thinking(t)) = &piece;
    if !t.is_empty() {
        out.push(piece);
    }
}

/// Length of the longest proper prefix of `tag` that `s` ends with.
fn partial_suffix(s: &str, tag: &str) -> usize {
    (1..tag.len()).rev().find(|&k| s.ends_with(&tag[..k])).unwrap_or(0)
}

/// The visible text of a finished reply, think blocks removed.
pub fn strip(text: &str) -> String {
    let mut s = ThinkScrubber::new();
    let mut pieces = s.feed(text);
    pieces.extend(s.finish());
    pieces
        .into_iter()
        .filter_map(|p| match p {
            Piece::Text(t) => Some(t),
            Piece::Thinking(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> (String, String) {
        let mut s = ThinkScrubber::new();
        let mut pieces = Vec::new();
        for c in chunks {
            pieces.extend(s.feed(c));
        }
        pieces.extend(s.finish());
        let (mut text, mut thinking) = (String::new(), String::new());
        for p in pieces {
            match p {
                Piece::Text(t) => text.push_str(&t),
                Piece::Thinking(t) => thinking.push_str(&t),
            }
        }
        (text, thinking)
    }

    #[test]
    fn splits_a_leading_think_block() {
        assert_eq!(run(&["<think>secret plan</think>Hello"]), ("Hello".into(), "secret plan".into()));
        assert_eq!(run(&["<thinking>a</thinking>\n\nb"]), ("b".into(), "a".into()));
        assert_eq!(run(&["<reasoning>r</reasoning> x"]), ("x".into(), "r".into()));
    }

    #[test]
    fn tags_split_across_chunks_are_held_back() {
        assert_eq!(run(&["<thi", "nk>secr", "et plan</th", "ink>Hel", "lo"]), ("Hello".into(), "secret plan".into()));
        assert_eq!(run(&["<", "think", ">a<", "/", "think>b"]), ("b".into(), "a".into()));
        // A partial that never becomes a tag is text after all.
        assert_eq!(run(&["a\n<th", "is is text"]), ("a\n<this is text".into(), String::new()));
        assert_eq!(run(&["a\n<thi"]), ("a\n<thi".into(), String::new()));
    }

    #[test]
    fn opening_tags_count_only_at_a_line_start() {
        assert_eq!(run(&["use <think> tags"]), ("use <think> tags".into(), String::new()));
        assert_eq!(run(&["intro\n<think>t</think>rest"]), ("intro\nrest".into(), "t".into()));
        assert_eq!(run(&["x", "<think>y"]), ("x<think>y".into(), String::new()));
    }

    #[test]
    fn an_unclosed_block_is_thinking() {
        assert_eq!(run(&["<think>still going"]), (String::new(), "still going".into()));
        assert_eq!(run(&["<think>a</thi"]), (String::new(), "a</thi".into()));
    }

    #[test]
    fn strip_keeps_only_the_text() {
        assert_eq!(strip("<think>plan</think>\nDone."), "Done.");
        assert_eq!(strip("plain"), "plain");
    }
}
