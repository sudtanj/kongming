//! Making do with what small models send: arguments that are almost JSON,
//! tool names that are almost right, calls written into the text instead of
//! the API's tool-call field, and values of the wrong JSON type.

use serde_json::{json, Map, Value};

use super::ToolSpec;

/// Tool-call arguments as sent, repaired when they are nearly JSON. What
/// cannot be read comes back as `{"_raw": ...}` for the agent to bounce.
pub fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    match parse_json(raw) {
        // Arguments encoded twice: a JSON string holding the object.
        Some(Value::String(inner)) => match parse_json(&inner) {
            Some(v) if v.is_object() => v,
            _ => json!({"_raw": raw}),
        },
        Some(Value::Null) => json!({}),
        Some(v) => v,
        None => json!({"_raw": raw}),
    }
}

/// Parse JSON, then retry with the usual damage undone: code fences,
/// trailing commas, missing closing brackets, extra closing brackets, and
/// Python-style dicts with single quotes.
pub fn parse_json(raw: &str) -> Option<Value> {
    let text = strip_fences(raw.trim());
    if text.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str(text) {
        return Some(v);
    }
    let fixed = drop_trailing_commas(&balance(text));
    if let Ok(v) = serde_json::from_str(&fixed) {
        return Some(v);
    }
    if text.contains('\'') || ["True", "False", "None"].iter().any(|w| text.contains(*w)) {
        let fixed = drop_trailing_commas(&balance(&pythonic_to_json(text)));
        if let Ok(v) = serde_json::from_str(&fixed) {
            return Some(v);
        }
    }
    None
}

/// The body of a fenced code block (any language tag); other text as it is.
fn strip_fences(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("```") else { return s };
    let body = rest.split_once('\n').map_or("", |(_, b)| b);
    body.trim_end().strip_suffix("```").unwrap_or(body).trim()
}

/// Close what was left open (a string, then brackets, innermost first) and
/// drop closing brackets that match nothing.
fn balance(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut stack: Vec<char> = Vec::new();
    let (mut in_str, mut escaped) = (false, false);
    for c in s.chars() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' => {
                stack.push('}');
                out.push(c);
            }
            '[' => {
                stack.push(']');
                out.push(c);
            }
            '}' | ']' => {
                // A closer for an outer bracket closes the inner ones first.
                if stack.contains(&c) {
                    while let Some(close) = stack.pop() {
                        out.push(close);
                        if close == c {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    if in_str {
        if escaped {
            out.pop();
        }
        out.push('"');
    }
    while let Some(close) = stack.pop() {
        out.push(close);
    }
    out
}

/// Remove commas that directly precede a closing bracket, outside strings.
fn drop_trailing_commas(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut escaped) = (false, false);
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if matches!(next, Some('}' | ']') | None) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// `{'a': 'it\'s', 'b': True, 'c': None}` to JSON.
fn pythonic_to_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    // The quote that opened the current string, if inside one.
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => match c {
                '\\' => match chars.next() {
                    Some('\'') => out.push('\''),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => {}
                },
                '"' if q == '\'' => out.push_str("\\\""),
                _ if c == q => {
                    out.push('"');
                    quote = None;
                }
                _ => out.push(c),
            },
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                out.push('"');
            }
            None if c.is_ascii_alphabetic() => {
                let mut word = String::from(c);
                while let Some(&n) = chars.peek().filter(|n| n.is_ascii_alphanumeric() || **n == '_') {
                    word.push(n);
                    chars.next();
                }
                out.push_str(match word.as_str() {
                    "True" => "true",
                    "False" => "false",
                    "None" => "null",
                    w => w,
                });
            }
            None => out.push(c),
        }
    }
    out
}

/// Tool names compared loosely: no `functions.` prefix, nothing after the
/// first quote, bracket or space, snake_case, no `tool` suffix.
fn normalize_name(name: &str) -> String {
    let name = name.trim();
    let name = name.strip_prefix("functions.").unwrap_or(name);
    let end = name.find(|c: char| "\"'<>".contains(c) || c.is_whitespace()).unwrap_or(name.len());
    let mut out = String::with_capacity(end + 4);
    let mut prev: Option<char> = None;
    for c in name[..end].chars() {
        if c.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            out.push('_');
        }
        match c {
            '-' | ' ' => out.push('_'),
            c => out.extend(c.to_lowercase()),
        }
        prev = Some(c);
    }
    for suffix in ["_tool", "tool"] {
        if let Some(s) = out.strip_suffix(suffix).filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    out
}

/// Character edit distance (two rows; names are short).
fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn similarity(a: &str, b: &str) -> f64 {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 1.0;
    }
    1.0 - levenshtein(a, b) as f64 / longest as f64
}

/// The offered tool a model meant: the exact name, then the same name
/// normalized, then an MCP tool by its own name (`search` for
/// `mcp__server__search`) when only one matches, then the closest name if it
/// is close enough.
pub fn resolve_tool_name<'a>(name: &str, tools: &'a [ToolSpec]) -> Option<&'a ToolSpec> {
    if let Some(t) = tools.iter().find(|t| t.name == name) {
        return Some(t);
    }
    let n = normalize_name(name);
    if n.is_empty() || n.len() > 128 {
        return None;
    }
    let normalized: Vec<String> = tools.iter().map(|t| normalize_name(&t.name)).collect();
    if let Some(i) = normalized.iter().position(|t| *t == n) {
        return Some(&tools[i]);
    }
    let suffix = format!("__{n}");
    let ends: Vec<usize> = (0..tools.len()).filter(|&i| normalized[i].ends_with(&suffix)).collect();
    if let [i] = ends.as_slice() {
        return Some(&tools[*i]);
    }
    let mut best: Option<(f64, usize)> = None;
    let mut tie = false;
    for (i, t) in normalized.iter().enumerate() {
        let own = t.rsplit("__").next().unwrap_or(t);
        let score = similarity(&n, t).max(similarity(&n, own));
        match best {
            Some((b, _)) if score < b => {}
            Some((b, _)) if (score - b).abs() < f64::EPSILON => tie = true,
            _ => {
                best = Some((score, i));
                tie = false;
            }
        }
    }
    match best {
        Some((score, i)) if score >= 0.7 && !tie => Some(&tools[i]),
        _ => None,
    }
}

/// Calls written into the reply text by models without native tool calling:
/// `<tool_call>{...}</tool_call>` blocks first; otherwise fenced code blocks
/// or a bare JSON object (or list) shaped `{"name", "arguments"}`. Only calls
/// to offered tools count. Returns the text without the matched spans and
/// the calls with their resolved names.
pub fn extract_text_calls(text: &str, tools: &[ToolSpec]) -> Option<(String, Vec<(String, Value)>)> {
    if tools.is_empty() || !text.contains('{') {
        return None;
    }
    let mut spans = tagged_spans(text);
    if spans.is_empty() {
        let fenced = fenced_spans(text);
        // A bare object inside a fence is that fence's call.
        let bare = bare_spans(text).into_iter().filter(|b| !fenced.iter().any(|f| b.0 < f.1 && f.0 < b.1));
        spans = fenced.iter().copied().chain(bare).collect();
    }
    let mut calls = Vec::new();
    let mut matched: Vec<(usize, usize)> = Vec::new();
    for (start, end, body) in spans {
        let found = parse_json(body).map(|v| calls_in(&v, tools)).unwrap_or_default();
        if !found.is_empty() {
            calls.extend(found);
            matched.push((start, end));
        }
    }
    if calls.is_empty() {
        return None;
    }
    matched.sort_unstable();
    let mut rest = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in matched {
        if start >= at {
            rest.push_str(&text[at..start]);
            at = end;
        }
    }
    rest.push_str(&text[at..]);
    Some((rest.trim().to_string(), calls))
}

/// `<tool_call>…</tool_call>` blocks as (start, end, body); a last block may
/// be left unclosed.
fn tagged_spans(text: &str) -> Vec<(usize, usize, &str)> {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find(OPEN) {
        let start = from + i;
        let body_start = start + OPEN.len();
        let (body_end, end) = match text[body_start..].find(CLOSE) {
            Some(j) => (body_start + j, body_start + j + CLOSE.len()),
            None => (text.len(), text.len()),
        };
        out.push((start, end, &text[body_start..body_end]));
        from = end;
    }
    out
}

/// Fenced code blocks (any language tag) as (start, end, body).
fn fenced_spans(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("```") {
        let start = from + i;
        let Some(nl) = text[start..].find('\n') else { break };
        let body_start = start + nl + 1;
        let Some(j) = text[body_start..].find("```") else { break };
        let end = body_start + j + 3;
        out.push((start, end, &text[body_start..body_start + j]));
        from = end;
    }
    out
}

/// Balanced `{…}` / `[…]` spans that start a line, as (start, end, body).
fn bare_spans(text: &str) -> Vec<(usize, usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let line_start = i == 0 || bytes[i - 1] == b'\n';
        if line_start && (bytes[i] == b'{' || bytes[i] == b'[') {
            if let Some(end) = matching_close(text, i) {
                out.push((i, end, &text[i..end]));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// End (exclusive) of the bracket opened at `start`, skipping strings.
fn matching_close(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let (mut in_str, mut escaped) = (false, false);
    for (i, c) in text[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Calls in a parsed value: one call object, OpenAI's `{"function": {...}}`
/// shape, or a list of them; only those naming an offered tool.
fn calls_in(v: &Value, tools: &[ToolSpec]) -> Vec<(String, Value)> {
    match v {
        Value::Array(items) => items.iter().flat_map(|i| calls_in(i, tools)).collect(),
        Value::Object(o) => {
            if let Some(f) = o.get("function").filter(|f| f.is_object()) {
                return calls_in(f, tools);
            }
            let Some(name) = o.get("name").and_then(Value::as_str) else { return vec![] };
            let Some(args) = o.get("arguments").or_else(|| o.get("parameters")) else { return vec![] };
            let args = match args {
                Value::String(s) => parse_args(s),
                Value::Null => json!({}),
                other => other.clone(),
            };
            match resolve_tool_name(name, tools) {
                Some(t) if args.is_object() => vec![(t.name.clone(), args)],
                _ => vec![],
            }
        }
        _ => vec![],
    }
}

/// JSON types a schema property allows.
fn types_of(prop: &Value) -> Vec<&str> {
    match prop.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

/// Bring argument values to the types the tool's schema asks for: "10" for
/// an integer, "true" for a boolean, a lone value where a list is expected,
/// a number where a string is expected, an enum value in the wrong case.
pub fn coerce_args(spec: &ToolSpec, args: &mut Value) {
    let Some(props) = spec.parameters.get("properties").and_then(Value::as_object) else { return };
    let Some(obj) = args.as_object_mut() else { return };
    for (key, value) in obj.iter_mut() {
        if let Some(prop) = props.get(key) {
            coerce_value(prop, value);
        }
    }
}

fn coerce_value(prop: &Value, value: &mut Value) {
    if let Some(allowed) = prop.get("enum").and_then(Value::as_array) {
        if let Some(s) = value.as_str() {
            let same = allowed.iter().find(|a| a.as_str().is_some_and(|a| a.eq_ignore_ascii_case(s)));
            if let Some(a) = same.filter(|a| a.as_str() != Some(s)) {
                *value = a.clone();
            }
        }
    }
    let types = types_of(prop);
    let wants = |t: &str| types.contains(&t);
    let numeric = wants("number") || wants("integer");
    let new = match &*value {
        Value::String(s) if !types.is_empty() && !wants("string") => from_string(prop, &types, s),
        Value::Number(n) if wants("string") && !numeric => Some(Value::String(n.to_string())),
        Value::Bool(b) if wants("string") && !wants("boolean") => Some(Value::String(b.to_string())),
        v @ (Value::Number(_) | Value::Bool(_)) if wants("array") => Some(Value::Array(vec![v.clone()])),
        _ => None,
    };
    if let Some(v) = new {
        *value = v;
    }
}

/// A string argument as the type the schema wants, if it reads as one.
fn from_string(prop: &Value, types: &[&str], s: &str) -> Option<Value> {
    let t = s.trim();
    let wants = |k: &str| types.contains(&k);
    if wants("integer") || wants("number") {
        if let Ok(i) = t.parse::<i64>() {
            return Some(Value::from(i));
        }
    }
    if wants("number") {
        if let Some(f) = t.parse::<f64>().ok().filter(|f| f.is_finite()) {
            return Some(Value::from(f));
        }
    }
    if wants("boolean") && (t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false")) {
        return Some(Value::Bool(t.eq_ignore_ascii_case("true")));
    }
    if wants("array") {
        if let Ok(v @ Value::Array(_)) = serde_json::from_str::<Value>(t) {
            return Some(v);
        }
        let of_strings = prop.get("items").is_some_and(|i| types_of(i) == ["string"]);
        if of_strings && t.contains(',') {
            return Some(t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(Value::from).collect());
        }
        return Some(Value::Array(vec![Value::from(s)]));
    }
    if wants("object") {
        return serde_json::from_str::<Value>(t).ok().filter(Value::is_object);
    }
    None
}

/// Values that are not among a property's allowed `enum` values, as
/// "key must be one of: a, b".
pub fn enum_problems(spec: &ToolSpec, args: &Map<String, Value>) -> Vec<String> {
    let Some(props) = spec.parameters.get("properties").and_then(Value::as_object) else { return vec![] };
    let mut out = Vec::new();
    for (key, value) in args {
        let Some(allowed) = props.get(key).and_then(|p| p.get("enum")).and_then(Value::as_array) else { continue };
        // An empty value means "not given": the tool's own default applies.
        let empty = value.is_null() || value.as_str() == Some("");
        if !empty && !allowed.contains(value) {
            let list: Vec<String> = allowed.iter().map(plain).collect();
            out.push(format!("{key} must be one of: {}", list.join(", ")));
        }
    }
    out
}

/// A JSON value for a message: strings without their quotes.
fn plain(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, parameters: Value) -> ToolSpec {
        ToolSpec { name: name.into(), description: String::new(), parameters }
    }

    fn tools() -> Vec<ToolSpec> {
        ["current_time", "skill_view", "web_fetch", "mcp__composio__GMAIL_SEND", "mcp__notes__search"]
            .iter()
            .map(|n| spec(n, json!({"type": "object"})))
            .collect()
    }

    fn resolve(name: &str) -> Option<String> {
        resolve_tool_name(name, &tools()).map(|t| t.name.clone())
    }

    #[test]
    fn arguments_are_repaired() {
        assert_eq!(parse_args(""), json!({}));
        assert_eq!(parse_args("  "), json!({}));
        assert_eq!(parse_args(r#"{"a": 1}"#), json!({"a": 1}));
        assert_eq!(parse_args("```json\n{\"a\": 1}\n```"), json!({"a": 1}));
        assert_eq!(parse_args(r#"{"a": [1, 2,], "b": "x,}",}"#), json!({"a": [1, 2], "b": "x,}"}));
        assert_eq!(parse_args(r#"{"a": {"b": [1, 2"#), json!({"a": {"b": [1, 2]}}));
        assert_eq!(parse_args(r#"{"a": "unfinished"#), json!({"a": "unfinished"}));
        assert_eq!(parse_args(r#"{"a": [1}"#), json!({"a": [1]}));
        assert_eq!(parse_args(r#"{"a": 1}}]"#), json!({"a": 1}));
        assert_eq!(parse_args(r#"{"a": 1,"#), json!({"a": 1}));
        let python = parse_args(r#"{'q': 'it\'s "x"', 'on': True, 'n': None, 'f': False}"#);
        assert_eq!(python, json!({"q": "it's \"x\"", "on": true, "n": null, "f": false}));
        assert_eq!(parse_args(r#""{\"a\": 1}""#), json!({"a": 1}));
        assert_eq!(parse_args("{bad"), json!({"_raw": "{bad"}));
        assert_eq!(parse_args("not json"), json!({"_raw": "not json"}));
    }

    #[test]
    fn tool_names_are_repaired() {
        assert_eq!(resolve("current_time").as_deref(), Some("current_time"));
        assert_eq!(resolve("CurrentTime").as_deref(), Some("current_time"));
        assert_eq!(resolve("current-time").as_deref(), Some("current_time"));
        assert_eq!(resolve("functions.current_time").as_deref(), Some("current_time"));
        assert_eq!(resolve("current_time_tool").as_deref(), Some("current_time"));
        assert_eq!(resolve("skill_view\" args=").as_deref(), Some("skill_view"));
        assert_eq!(resolve("search").as_deref(), Some("mcp__notes__search"));
        assert_eq!(resolve("GMAIL_SEND").as_deref(), Some("mcp__composio__GMAIL_SEND"));
        assert_eq!(resolve("curent_tme").as_deref(), Some("current_time"));
        assert_eq!(resolve("webfetch").as_deref(), Some("web_fetch"));
        assert_eq!(resolve("nonexistent_tool"), None);
        assert_eq!(resolve("shell"), None);
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn edit_distance() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn calls_are_found_in_text() {
        let t = tools();
        let text = "Checking.\n<tool_call>{\"name\": \"current_time\", \"arguments\": {}}</tool_call>\nOne moment.";
        let (rest, calls) = extract_text_calls(text, &t).expect("tagged call");
        assert_eq!(rest, "Checking.\n\nOne moment.");
        assert_eq!(calls, vec![("current_time".to_string(), json!({}))]);

        let text = "```json\n{\"name\": \"web_fetch\", \"parameters\": {\"url\": \"https://x\"}}\n```";
        let (rest, calls) = extract_text_calls(text, &t).expect("fenced call");
        assert_eq!(rest, "");
        assert_eq!(calls, vec![("web_fetch".to_string(), json!({"url": "https://x"}))]);

        let first = r#"{"name": "CurrentTime", "arguments": "{}"}"#;
        let second = r#"{"name": "skill_view", "arguments": {"name": "x"}}"#;
        let text = format!("Sure:\n[{first}, {second}]");
        let (rest, calls) = extract_text_calls(&text, &t).expect("bare list");
        assert_eq!(rest, "Sure:");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "current_time");
        assert_eq!(calls[1], ("skill_view".to_string(), json!({"name": "x"})));

        // An unclosed tag at the end of the reply.
        let (_, calls) = extract_text_calls(r#"<tool_call>{"name": "current_time", "arguments": {}"#, &t).unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn data_that_only_looks_like_a_call_stays_text() {
        let t = tools();
        assert!(extract_text_calls(r#"{"name": "rm_rf", "arguments": {}}"#, &t).is_none());
        assert!(extract_text_calls(r#"{"name": "current_time"}"#, &t).is_none());
        assert!(extract_text_calls(r#"Use {"name": "current_time", "arguments": {}} like so"#, &t).is_none());
        assert!(extract_text_calls("plain text", &t).is_none());
        assert!(extract_text_calls(r#"<tool_call>{"name": "x", "arguments": {}}</tool_call>"#, &[]).is_none());
    }

    #[test]
    fn argument_types_follow_the_schema() {
        let params = json!({"type": "object", "properties": {
            "n": {"type": "integer"}, "f": {"type": "number"}, "b": {"type": "boolean"},
            "tags": {"type": "array", "items": {"type": "string"}}, "ids": {"type": "array"},
            "s": {"type": "string"}, "mode": {"type": "string", "enum": ["fast", "slow"]},
            "o": {"type": "object"}, "maybe": {"type": ["integer", "null"]}
        }});
        let s = spec("t", params);
        let mut args = json!({
            "n": "10", "f": "2.5", "b": "TRUE", "tags": "a, b,c", "ids": "x,y", "s": 42,
            "mode": "FAST", "o": "{\"k\": 1}", "maybe": " 7 ", "extra": "1"
        });
        coerce_args(&s, &mut args);
        let want = json!({
            "n": 10, "f": 2.5, "b": true, "tags": ["a", "b", "c"], "ids": ["x,y"], "s": "42",
            "mode": "fast", "o": {"k": 1}, "maybe": 7, "extra": "1"
        });
        assert_eq!(args, want);
        let mut args = json!({"n": "ten", "tags": "solo", "b": "yes"});
        coerce_args(&s, &mut args);
        assert_eq!(args, json!({"n": "ten", "tags": ["solo"], "b": "yes"}));

        let problems = enum_problems(&s, json!({"mode": "medium", "n": 1}).as_object().unwrap());
        assert_eq!(problems, vec!["mode must be one of: fast, slow".to_string()]);
    }
}
