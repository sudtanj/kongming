//! Deferred MCP tools. Many tool schemas crowd a small model's context and
//! its choice, so instead of every MCP tool the model gets two: `tool_search`
//! finds tools by what they do and `tool_call` runs one by name, with the
//! same name repair, type coercion and argument checks as a direct call.

use serde_json::{json, Value};

use crate::llm::{repair, ToolSpec};

pub const SEARCH: &str = "tool_search";
pub const CALL: &str = "tool_call";
/// More MCP tools than this go behind the bridge for every model.
pub const DEFER_ABOVE: usize = 12;
/// Tools one search returns.
pub const SEARCH_LIMIT: usize = 8;

/// Whether `count` MCP tools go behind the bridge: always for a small
/// model, past `DEFER_ABOVE` for the others.
pub fn defer(small: bool, count: usize) -> bool {
    count > 0 && (small || count > DEFER_ABOVE)
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: SEARCH.into(),
            description: "Find tools of the connected services (MCP) by what you want to do, e.g. \"create github \
issue\". Returns up to 8 tools with their arguments; run one with tool_call."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "what the tool should do, in a few words"}},
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: CALL.into(),
            description: "Run a tool found with tool_search: its exact name, and its arguments as an object.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "the tool's name from tool_search"},
                    "arguments": {"type": "object", "description": "the tool's arguments"}
                },
                "required": ["name"]
            }),
        },
    ]
}

/// Words that say nothing about what a tool does.
const STOP_WORDS: &[&str] = &["mcp", "the", "an", "to", "of", "for", "and", "in", "on", "my", "me", "with"];

/// Lowercase words of a text, split at anything but letters and digits.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .collect()
}

/// A query word against a tool word: the same, or one the start of the
/// other ("repo" and "repos").
fn hit(term: &str, word: &str) -> bool {
    term == word || (term.len() > 2 && word.len() > 2 && (word.starts_with(term) || term.starts_with(word)))
}

/// How well a tool fits the query words: a word in the name counts three
/// times one in the description.
fn score(t: &ToolSpec, terms: &[String]) -> usize {
    let name = words(&t.name);
    let desc = words(&t.description);
    terms.iter().map(|q| 3 * usize::from(any_hit(q, &name)) + usize::from(any_hit(q, &desc))).sum()
}

fn any_hit(term: &str, words: &[String]) -> bool {
    words.iter().any(|w| hit(term, w))
}

/// Tools ranked by keyword overlap with `query`, best first, ties by name.
/// Tools that share nothing with the query are left out (an empty query
/// lists the first ones).
pub fn search<'a>(tools: &'a [ToolSpec], query: &str, limit: usize) -> Vec<&'a ToolSpec> {
    let terms = words(query);
    let mut scored: Vec<(usize, &ToolSpec)> =
        tools.iter().map(|t| (score(t, &terms), t)).filter(|(s, _)| terms.is_empty() || *s > 0).collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(limit).map(|(_, t)| t).collect()
}

/// One line per tool: the name, the first line of its description and its
/// arguments (`*` marks the required ones).
pub fn render(found: &[&ToolSpec]) -> String {
    if found.is_empty() {
        return "No tool matches. Search again with other words: what to do and to what (\"list repositories\", \
\"send email\")."
            .into();
    }
    let mut out = String::new();
    for t in found {
        let desc = t.description.lines().next().unwrap_or("").trim();
        out.push_str(&format!("- {}: {}\n  arguments: {}\n", t.name, crate::mcp::truncate(desc, 160), params(t)));
    }
    out.push_str(&format!("Run one with {CALL} {{\"name\": \"<tool>\", \"arguments\": {{...}}}}."));
    out
}

/// A compact argument list: `owner* (string), per_page (integer)`.
fn params(t: &ToolSpec) -> String {
    let Some(props) = t.parameters.get("properties").and_then(Value::as_object) else { return "none".into() };
    if props.is_empty() {
        return "none".into();
    }
    let required: Vec<&str> = t
        .parameters
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let list: Vec<String> = props
        .iter()
        .map(|(k, p)| {
            let star = if required.contains(&k.as_str()) { "*" } else { "" };
            match p.get("type").and_then(Value::as_str) {
                Some(ty) => format!("{k}{star} ({ty})"),
                None => format!("{k}{star}"),
            }
        })
        .collect();
    crate::mcp::truncate(&list.join(", "), 400)
}

/// The tool and arguments a `tool_call` names, checked the way a direct call
/// is; the message for the model when it cannot run. Arguments may also come
/// as a JSON string, under `args` or `parameters`, or spread next to `name`.
pub fn resolve(tools: &[ToolSpec], args: &Value) -> Result<(String, Value), String> {
    let name = args.get("name").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err(format!("give the tool's name in `name`; find it with {SEARCH} first"));
    }
    let given = ["arguments", "args", "parameters"].iter().find_map(|k| args.get(*k));
    let mut call_args = match given {
        Some(Value::Object(o)) => Value::Object(o.clone()),
        Some(Value::String(s)) => repair::parse_args(s),
        Some(Value::Null) => json!({}),
        Some(other) => return Err(format!("`arguments` must be an object, not {other}")),
        None => {
            let mut rest = args.as_object().cloned().unwrap_or_default();
            rest.remove("name");
            Value::Object(rest)
        }
    };
    let Some(spec) = repair::resolve_tool_name(name, tools) else {
        let near: Vec<&str> = search(tools, name, 3).iter().map(|t| t.name.as_str()).collect();
        let hint = if near.is_empty() { String::new() } else { format!(" Close matches: {}.", near.join(", ")) };
        return Err(format!("unknown tool {name}: find the exact name with {SEARCH}.{hint}"));
    };
    repair::coerce_args(spec, &mut call_args);
    match super::check_args(Some(spec), &call_args) {
        Some(problem) => Err(problem),
        None => Ok((spec.name.clone(), call_args)),
    }
}

/// The one-line catalog for the prompt: each server with its tool count.
pub fn catalog(servers: &[(String, usize)]) -> String {
    let list: Vec<String> =
        servers.iter().map(|(name, n)| format!("{name} ({n} tool{})", if *n == 1 { "" } else { "s" })).collect();
    list.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: &str, params: Value) -> ToolSpec {
        ToolSpec { name: name.into(), description: description.into(), parameters: params }
    }

    fn tools() -> Vec<ToolSpec> {
        let repo = json!({"type": "object", "properties": {"owner": {"type": "string"}, "repo": {"type": "string"}, "title": {"type": "string"}}, "required": ["owner", "repo", "title"]});
        let per_page = json!({"type": "object", "properties": {"per_page": {"type": "integer"}}});
        let mail = "[Gmail] Send an email; can open an issue ticket by mail";
        vec![
            tool("mcp__github__create_issue", "[GitHub] Create an issue in a repository", repo),
            tool("mcp__github__list_repos", "[GitHub] List repositories of a user", per_page),
            tool("mcp__gmail__send_email", mail, json!({"type": "object", "properties": {}})),
            tool("mcp__calendar__list_events", "[Calendar] Upcoming events", json!({"type": "object"})),
        ]
    }

    #[test]
    fn search_ranks_by_name_then_description() {
        let t = tools();
        let names = |q: &str| search(&t, q, 8).iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        // A name hit outweighs a description hit.
        assert_eq!(names("create issue"), ["mcp__github__create_issue", "mcp__gmail__send_email"]);
        assert_eq!(names("list my repo")[0], "mcp__github__list_repos");
        assert_eq!(names("email")[0], "mcp__gmail__send_email");
        assert!(names("weather forecast").is_empty());
        assert_eq!(search(&t, "", 2).len(), 2, "an empty query lists the first tools");
        assert_eq!(search(&t, "list", 1).len(), 1);
        let shown = render(&search(&t, "create issue", 1));
        assert!(shown.contains("- mcp__github__create_issue: [GitHub] Create an issue"), "{shown}");
        assert!(shown.contains("owner* (string)") && shown.contains("tool_call"), "{shown}");
        assert!(render(&[]).contains("No tool matches"));
    }

    #[test]
    fn calls_are_resolved_and_checked() {
        let t = tools();
        let call = json!({"name": "mcp__github__create_issue", "arguments": {"owner": "o", "repo": "r", "title": "t"}});
        assert_eq!(resolve(&t, &call).unwrap().0, "mcp__github__create_issue");
        // Near-miss names, stringified or spread-out arguments.
        let loose = json!({"name": "create_issue", "arguments": "{\"owner\": \"o\", \"repo\": \"r\", \"title\": 5}"});
        let (name, args) = resolve(&t, &loose).unwrap();
        assert_eq!(name, "mcp__github__create_issue");
        assert_eq!(args["title"], json!("5"), "coerced to the schema's type");
        let spread = json!({"name": "mcp__github__list_repos", "per_page": "10"});
        assert_eq!(resolve(&t, &spread).unwrap().1, json!({"per_page": 10}));
        // Problems come back as messages the model can act on.
        let missing = resolve(&t, &json!({"name": "mcp__github__create_issue", "arguments": {"owner": "o"}}));
        assert!(missing.unwrap_err().contains("missing required: repo, title"));
        let unknown = resolve(&t, &json!({"name": "open ticket"})).unwrap_err();
        assert!(unknown.contains("unknown tool open ticket") && unknown.contains("tool_search"), "{unknown}");
        assert!(resolve(&t, &json!({})).unwrap_err().contains("`name`"));
    }

    #[test]
    fn small_models_always_defer_others_past_the_threshold() {
        assert!(defer(true, 1) && !defer(true, 0));
        assert!(!defer(false, DEFER_ABOVE) && defer(false, DEFER_ABOVE + 1));
        assert_eq!(catalog(&[("GitHub".into(), 40), ("Echo".into(), 1)]), "GitHub (40 tools), Echo (1 tool)");
    }
}
