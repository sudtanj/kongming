//! `@` mentions in user text, nanobot-style:
//!
//! * `@handle`          — names a teammate; the lead agent still answers
//! * `@skill:name`      — attach a skill's full body to this turn
//! * `@session:<id>`    — include another session's recent transcript as context
//! * `@mcp:<server>`    — make sure that MCP server's tools are offered this turn
//!
//! Structured mentions are removed from the text; plain `@handle` stays so the
//! lead agent sees who the user meant.

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Mentions {
    pub handles: Vec<String>,
    pub skills: Vec<String>,
    pub sessions: Vec<String>,
    pub mcp: Vec<String>,
    pub text: String,
}

fn is_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

pub fn parse(input: &str) -> Mentions {
    let mut m = Mentions::default();
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at_boundary = i == 0 || chars[i - 1].is_whitespace() || matches!(chars[i - 1], '(' | '[' | ',' | ':' | ';');
        if c == '@' && at_boundary && i + 1 < chars.len() && is_handle_char(chars[i + 1]) {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_handle_char(chars[j]) {
                j += 1;
            }
            let word: String = chars[start..j].iter().collect();
            let lower = word.to_ascii_lowercase();
            // Structured: @kind:value
            if j < chars.len() && chars[j] == ':' && matches!(lower.as_str(), "skill" | "session" | "mcp") {
                let vstart = j + 1;
                let mut k = vstart;
                while k < chars.len() && (is_handle_char(chars[k]) || chars[k] == '.' || chars[k] == '/') {
                    k += 1;
                }
                let value: String = chars[vstart..k].iter().collect();
                if !value.is_empty() {
                    match lower.as_str() {
                        "skill" => m.skills.push(value.to_ascii_lowercase()),
                        "session" => m.sessions.push(value),
                        _ => m.mcp.push(value),
                    }
                    i = k;
                    // Drop one following space so the text doesn't gain doubles.
                    if i < chars.len() && chars[i] == ' ' && (out.is_empty() || out.ends_with(' ')) {
                        i += 1;
                    }
                    continue;
                }
            }
            m.handles.push(lower);
            out.extend(chars[i..j].iter());
            i = j;
            continue;
        }
        out.push(c);
        i += 1;
    }
    m.text = out.trim().to_string();
    m
}

/// Strip a leading `@handle` so the agent isn't shown its own name as the first word.
pub fn strip_leading_handle(text: &str, handle: &str) -> String {
    let needle = format!("@{handle}");
    let t = text.trim_start();
    if t.len() >= needle.len() && t[..needle.len()].eq_ignore_ascii_case(&needle) {
        let rest = &t[needle.len()..];
        if rest.is_empty() || !is_handle_char(rest.chars().next().unwrap()) {
            return rest.trim_start_matches([',', ':', ' ']).to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_kinds() {
        let m = parse(
            "@Researcher please use @skill:deploy-notes and @session:abc12 context, cc @mcp:github and email a@b.com",
        );
        assert_eq!(m.handles, vec!["researcher"]);
        assert_eq!(m.skills, vec!["deploy-notes"]);
        assert_eq!(m.sessions, vec!["abc12"]);
        assert_eq!(m.mcp, vec!["github"]);
        assert_eq!(m.text, "@Researcher please use and context, cc and email a@b.com");
    }

    #[test]
    fn email_is_not_a_mention() {
        let m = parse("mail me at alice@example.com");
        assert!(m.handles.is_empty());
        assert_eq!(m.text, "mail me at alice@example.com");
    }

    #[test]
    fn strips_leading_handle_only() {
        assert_eq!(strip_leading_handle("@ops: restart it", "ops"), "restart it");
        assert_eq!(strip_leading_handle("@opsy restart", "ops"), "@opsy restart");
        assert_eq!(strip_leading_handle("ask @ops", "ops"), "ask @ops");
    }
}
