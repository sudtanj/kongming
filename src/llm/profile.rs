//! What the configured model can take: whether it is a small model (a
//! tighter prompt, fewer tools, cooler sampling) and how large its context
//! window is. Both come from the provider settings when set, and are guessed
//! from the model name and the server address otherwise.

use super::ProviderConfig;

/// Context window assumed for a small model on a local server when none is
/// set. Ollama's own default is smaller still, so kongming asks for this.
pub const LOCAL_SMALL_WINDOW: usize = 8192;
/// Largest parameter count, in billions, that still counts as small.
const SMALL_MAX_BILLIONS: f64 = 34.0;
/// Families that are small when the name gives no size (`gemma3`, `phi4`,
/// `qwen2.5`, `granite3.1-dense`, ...): a token starts with one of these.
const SMALL_FAMILIES: &[&str] =
    &["gemma", "phi", "qwen2", "qwen3", "ministral", "smollm", "granite", "llama3.1", "llama3.2"];
/// The same, matched as a whole token (`llama3.3` is 70b only).
const SMALL_NAMES: &[&str] = &["llama3", "mistral"];
/// Hosted variants of those families that are large.
const LARGE_MARKERS: &[&str] = &["large", "medium", "max", "plus", "pro", "ultra", "coder"];
/// Open-weight families that tend to describe a tool call instead of making
/// it, or to make tool output up; they get the stricter tool-use rules.
const LOOSE_TOOL_FAMILIES: &[&str] = &["qwen", "glm", "deepseek", "gemma", "llama", "mistral", "phi", "granite"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelProfile {
    pub small: bool,
    /// Context window in tokens; 0 when unknown (the global budget applies).
    pub context_window: usize,
}

/// The profile of the provider's own model.
pub fn profile(cfg: &ProviderConfig) -> ModelProfile {
    profile_for(cfg, &cfg.model)
}

/// The profile of `model` served by the provider (an agent may name its own
/// model). The explicit settings win; otherwise a size of at most 34b in the
/// name, a small family without a size, or a local model server without a
/// size in the name means small.
pub fn profile_for(cfg: &ProviderConfig, model: &str) -> ModelProfile {
    let local = is_local_server(cfg.base_url.as_deref());
    let small = cfg.small_model.unwrap_or_else(|| match param_billions(model) {
        Some(b) => b <= SMALL_MAX_BILLIONS,
        None => small_family(model) || local,
    });
    let context_window = match cfg.context_window.filter(|w| *w > 0) {
        Some(w) => w as usize,
        None if small && local => LOCAL_SMALL_WINDOW,
        None => 0,
    };
    ModelProfile { small, context_window }
}

/// Model families that get Hermes' tool-use enforcement rules.
pub fn needs_tool_rules(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    LOOSE_TOOL_FAMILIES.iter().any(|f| m.contains(f))
}

/// The lowercase tokens of a model name without its vendor prefix:
/// `Qwen/Qwen2.5-7B-Instruct` gives `qwen2.5`, `7b`, `instruct`.
fn tokens(model: &str) -> Vec<String> {
    let m = model.trim().to_ascii_lowercase();
    let name = m.rsplit('/').next().unwrap_or("");
    name.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// A size token in billions: `7b`, `3.8b`, `e4b` (Gemma's effective size),
/// `8x7b` (a mixture of experts, counted whole). `a22b` (active parameters)
/// is not one.
fn size_token(t: &str) -> Option<f64> {
    let t = t.strip_suffix('b')?;
    let t = t.strip_prefix('e').unwrap_or(t);
    let num = |s: &str| -> Option<f64> {
        let digits = s.starts_with(|c: char| c.is_ascii_digit()) && s.chars().all(|c| c.is_ascii_digit() || c == '.');
        if digits {
            s.parse().ok()
        } else {
            None
        }
    };
    match t.split_once('x') {
        Some((n, each)) => Some(num(n)? * num(each)?),
        None => num(t),
    }
}

/// The parameter count a model name gives, in billions: the largest size
/// token, so `qwen3-235b-a22b` is 235.
fn param_billions(model: &str) -> Option<f64> {
    tokens(model).iter().filter_map(|t| size_token(t.as_str())).reduce(f64::max)
}

fn small_family(model: &str) -> bool {
    let tokens = tokens(model);
    let family = tokens
        .iter()
        .any(|t| SMALL_FAMILIES.iter().any(|f| t.starts_with(f)) || SMALL_NAMES.contains(&t.as_str()));
    family && !tokens.iter().any(|t| LARGE_MARKERS.contains(&t.as_str()))
}

fn port_and_host(base_url: Option<&str>) -> Option<(Option<u16>, String, String)> {
    let u = url::Url::parse(base_url?.trim()).ok()?;
    Some((u.port(), u.host_str().unwrap_or("").to_ascii_lowercase(), u.path().to_string()))
}

/// Ollama (port 11434, or a host named for it), llama.cpp's server (8080) or
/// LM Studio (1234): local servers that mostly run small models.
pub fn is_local_server(base_url: Option<&str>) -> bool {
    match port_and_host(base_url) {
        Some((port, host, _)) => matches!(port, Some(11434 | 8080 | 1234)) || host.contains("ollama"),
        None => false,
    }
}

/// An Ollama server, which takes the context window as `num_ctx`: port
/// 11434, a host named for it, or its native `/api` base path.
pub fn is_ollama(base_url: Option<&str>) -> bool {
    match port_and_host(base_url) {
        Some((port, host, path)) => {
            port == Some(11434) || host.contains("ollama") || path.trim_end_matches('/').ends_with("/api")
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(model: &str, base: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            kind: "openai".into(),
            model: model.into(),
            base_url: base.map(str::to_string),
            ..Default::default()
        }
    }

    fn small(model: &str) -> bool {
        profile(&cfg(model, Some("https://openrouter.ai/api/v1"))).small
    }

    #[test]
    fn sizes_in_the_name_decide() {
        for m in ["qwen2.5:7b", "llama3.1:8b", "Qwen/Qwen2.5-32B-Instruct", "phi3:3.8b", "gemma3n:e4b", "gpt-oss-20b"] {
            assert!(small(m), "{m} is small");
        }
        for m in ["llama3.3:70b", "qwen/qwen3-235b-a22b", "mixtral:8x7b", "gpt-oss-120b", "deepseek-r1:671b"] {
            assert!(!small(m), "{m} is large");
        }
        assert_eq!(param_billions("qwen3-235b-a22b"), Some(235.0));
        assert_eq!(param_billions("gemma3n:e2b"), Some(2.0));
        assert_eq!(param_billions("gpt-4o"), None);
    }

    #[test]
    fn small_families_without_a_size() {
        for m in ["gemma3", "phi4", "qwen2.5", "qwen3", "ministral", "smollm2", "granite3.1-dense", "llama3"] {
            assert!(small(m), "{m} is small");
        }
        for m in ["gpt-4o", "claude-sonnet-4-5", "qwen3-max", "mistral-large-latest", "llama3.3", "deepseek-chat"] {
            assert!(!small(m), "{m} is not small");
        }
    }

    #[test]
    fn local_servers_and_explicit_settings() {
        // A local server with no size in the name: small, with an 8k window.
        let ollama = profile(&cfg("my-model", Some("http://localhost:11434/v1")));
        assert_eq!(ollama, ModelProfile { small: true, context_window: LOCAL_SMALL_WINDOW });
        assert!(profile(&cfg("custom", Some("http://192.168.1.5:1234/v1"))).small);
        assert!(profile(&cfg("custom", Some("http://pi:8080/v1"))).small);
        // A port that only starts like one is not one.
        assert!(!profile(&cfg("custom", Some("http://127.0.0.1:12345/v1"))).small);
        // A size in the name still wins on a local server.
        assert!(!profile(&cfg("llama3.3:70b", Some("http://localhost:11434/v1"))).small);
        // Explicit settings win over detection.
        let mut c = cfg("gpt-4o", None);
        c.small_model = Some(true);
        c.context_window = Some(32_000);
        assert_eq!(profile(&c), ModelProfile { small: true, context_window: 32_000 });
        let mut c = cfg("qwen2.5:7b", Some("http://localhost:11434/v1"));
        c.small_model = Some(false);
        assert_eq!(profile(&c), ModelProfile { small: false, context_window: 0 });
        // An agent's own model is profiled on the same provider.
        assert!(profile_for(&cfg("gpt-4o", None), "qwen3:8b").small);
    }

    #[test]
    fn ollama_and_tool_rules_are_recognized() {
        assert!(is_ollama(Some("http://localhost:11434/v1")));
        assert!(is_ollama(Some("http://ollama:8000/v1")));
        assert!(!is_ollama(Some("https://openrouter.ai/api/v1")));
        assert!(!is_ollama(Some("http://localhost:8080/v1")));
        assert!(!is_ollama(None));
        assert!(needs_tool_rules("Qwen3-30B-A3B") && needs_tool_rules("z-ai/glm-4.6"));
        assert!(!needs_tool_rules("gpt-4o") && !needs_tool_rules("claude-sonnet-4-5"));
    }
}
