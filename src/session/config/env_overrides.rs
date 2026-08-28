//! Generic `KF_CODE_*` env-var loader (WO 47.2).
//!
//! Var → config key: strip `KF_CODE_` and lowercase, so a new Config
//! field is env-overridable with zero loader changes (`KF_CODE_FROB_RATE`
//! → `frob_rate`). Irregular names live in `KEY_MAP`. Values are coerced
//! to the field's type using the current config's serialized form as the
//! type guide; a var whose value doesn't parse is ignored, matching the
//! historical per-field behavior. The result is applied through
//! `merge_toml_into_config`'s serde overlay (and its normalize pass:
//! tilde expansion + clamps), so the file and env layers share one code
//! path.
//!
//! KF_CODE_BUDGET_CEILING (WO 14.7, Token Budget Challenge) and every
//! other documented var in the `config` module header route through the
//! same derived path.

use crate::shared::Config;
use toml::Value;

use super::merge::merge_toml_into_config;
use super::{parse_bool_env, parse_plugin_sources_env};

// Env vars whose config key isn't derivable from the var name
// (`KF_CODE_<SUFFIX>` lowercased doesn't match the field).
pub(super) const KEY_MAP: &[(&str, &str)] = &[
    ("KF_CODE_MODEL", "default_model"),
    ("KF_CODE_HOST", "ollama_host"),
    ("KF_CODE_MAX_READ_SIZE", "max_file_read_size"),
    ("KF_CODE_BLOCK_BINARY", "block_binary_reads"),
    // [computer_use] table
    ("KF_CODE_COMPUTER_USE_ENABLED", "computer_use.enabled"),
    (
        "KF_CODE_COMPUTER_USE_CHROME_PATH",
        "computer_use.chrome_path",
    ),
    ("KF_CODE_COMPUTER_USE_HEADFUL", "computer_use.headful"),
    ("KF_CODE_COMPUTER_USE_WIDTH", "computer_use.width"),
    ("KF_CODE_COMPUTER_USE_HEIGHT", "computer_use.height"),
    (
        "KF_CODE_COMPUTER_USE_STARTUP_TIMEOUT",
        "computer_use.startup_timeout_secs",
    ),
    (
        "KF_CODE_COMPUTER_USE_WAIT_TIMEOUT",
        "computer_use.wait_timeout_secs",
    ),
    ("KF_CODE_COMPUTER_USE_HOSTED", "computer_use.hosted"),
    // [subagent_provider] table (WO 30.0.6)
    ("KF_CODE_SUBAGENT_MODEL", "subagent_provider.model"),
    ("KF_CODE_SUBAGENT_HOST", "subagent_provider.ollama_host"),
    (
        "KF_CODE_SUBAGENT_ANTHROPIC_API_KEY",
        "subagent_provider.anthropic_api_key",
    ),
    (
        "KF_CODE_SUBAGENT_OPENAI_API_KEY",
        "subagent_provider.openai_api_key",
    ),
    (
        "KF_CODE_SUBAGENT_DEEPSEEK_API_KEY",
        "subagent_provider.deepseek_api_key",
    ),
    (
        "KF_CODE_SUBAGENT_GEMINI_API_KEY",
        "subagent_provider.gemini_api_key",
    ),
    (
        "KF_CODE_SUBAGENT_KIMI_API_KEY",
        "subagent_provider.kimi_api_key",
    ),
    // Per-provider API keys resolved by `adapters::auth::resolve_api_key`
    // (Series 18): plain `<PROVIDER>_API_KEY`, no KF_CODE_ prefix.
    ("ANTHROPIC_API_KEY", "anthropic_api_key"),
    ("OPENAI_API_KEY", "openai_api_key"),
    ("DEEPSEEK_API_KEY", "deepseek_api_key"),
    ("GEMINI_API_KEY", "gemini_api_key"),
    ("KIMI_API_KEY", "kimi_api_key"),
];

/// Apply environment variable overrides to a Config.
pub(super) fn apply_env_overrides(cfg: &mut Config) {
    let base = match toml::Value::try_from(&*cfg) {
        Ok(toml::Value::Table(t)) => t,
        _ => return,
    };

    let mut overrides = toml::Table::new();
    for (var, val) in std::env::vars_os() {
        let (Ok(var), Ok(val)) = (var.into_string(), val.into_string()) else {
            continue;
        };
        let Some(key) = config_key(&var) else {
            continue;
        };
        let value = match custom_value(&var, &val) {
            Some(v) => v,
            None => match coerce(&val, guide_value(&base, &key)) {
                Some(v) => v,
                None => continue,
            },
        };
        overrides.insert(key, value);
    }

    // Legacy alias precedence: KF_CODE_COMPACTION_USE_HEURISTIC wins
    // over the old KF_CODE_COMPACTION_USE_LLM when both are set —
    // re-insert it last so overlay order can't decide.
    if let Some(v) = std::env::var("KF_CODE_COMPACTION_USE_HEURISTIC")
        .ok()
        .and_then(|v| parse_bool_env(&v))
        .map(Value::Boolean)
    {
        overrides.insert("compaction_use_heuristic".to_string(), v);
    }

    merge_toml_into_config(cfg, overrides);

    // Whole-section / validated env overrides — semantics the generic
    // overlay can't express (documented to replace the TOML section
    // entirely, or to ignore invalid values rather than coerce them).
    if let Ok(val) = std::env::var("KF_CODE_ADAPTER_ROUTING") {
        if !val.is_empty() {
            cfg.model.adapter_routing = parse_adapter_routing(&val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_PLUGIN_SOURCES") {
        cfg.tools.plugin_sources = parse_plugin_sources_env(&val);
    }
    if let Ok(val) = std::env::var("KF_CODE_TASK_CONCURRENCY_MODE") {
        let mode = val.to_lowercase();
        if mode == "queue" || mode == "reject" {
            cfg.tools.task_concurrency_mode = mode;
        }
    }
}

// Map an env-var name to its (possibly dotted) config key. `None`
// means the var isn't a config override at all — including the vars
// applied by the post-overlay block, which carry validated or
// whole-section semantics the generic path can't express.
fn config_key(var: &str) -> Option<String> {
    if matches!(
        var,
        "KF_CODE_ADAPTER_ROUTING" | "KF_CODE_PLUGIN_SOURCES" | "KF_CODE_TASK_CONCURRENCY_MODE"
    ) {
        return None;
    }
    if let Some((_, key)) = KEY_MAP.iter().find(|(v, _)| *v == var) {
        return Some(key.to_string());
    }
    var.strip_prefix("KF_CODE_")
        .filter(|suffix| !suffix.is_empty())
        .map(|suffix| suffix.to_lowercase())
}

// The current config's serialized value at `key`, used as the type
// guide for coercing the env string. Absent keys are Option fields
// that are currently None (they serialize to nothing).
fn guide_value<'a>(base: &'a toml::Table, key: &str) -> Option<&'a Value> {
    let mut node = base;
    let mut segments = key.split('.');
    let last = segments.next_back()?;
    for seg in segments {
        node = node.get(seg)?.as_table()?;
    }
    node.get(last)
}

// Coerce an env string to the type the guide value indicates. Vec
// fields (string-element) take a comma-separated list. Returns None
// for unparseable/empty values so the prior layer is kept.
fn coerce(val: &str, guide: Option<&Value>) -> Option<Value> {
    match guide {
        Some(Value::Boolean(_)) => parse_bool_env(val).map(Value::Boolean),
        Some(Value::Integer(_)) => val.parse::<i64>().ok().map(Value::Integer),
        Some(Value::Float(_)) => val.parse::<f64>().ok().map(Value::Float),
        Some(Value::String(_)) => {
            if val.is_empty() {
                None
            } else {
                Some(Value::String(val.to_string()))
            }
        }
        Some(Value::Array(_)) => Some(Value::Array(split_list(val, ','))),
        _ => guess(val),
    }
}

// Shape-guessing for keys absent from the serialized config (an
// Option field that is currently None, e.g. stem_file_cap).
fn guess(val: &str) -> Option<Value> {
    if let Ok(n) = val.parse::<i64>() {
        Some(Value::Integer(n))
    } else if let Ok(f) = val.parse::<f64>() {
        Some(Value::Float(f))
    } else if !val.is_empty() {
        Some(Value::String(val.to_string()))
    } else {
        None
    }
}

fn split_list(val: &str, sep: char) -> Vec<Value> {
    val.split(sep)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Value::String(s.to_string()))
        .collect()
}

// Vars whose value semantics can't be derived from the field type.
// Returns None for values that must be ignored.
fn custom_value(var: &str, val: &str) -> Option<Value> {
    match var {
        // Path-valued fields where an empty env value is meaningful
        // (it must flow through, not be skipped): sandbox_dir/cache_dir
        // keep Some("") — the unsandboxed/unset escape hatch; the rest
        // clear to None via normalize's empty→None rule ("empty
        // disables" per the module doc). Tildes expand in normalize.
        "KF_CODE_SANDBOX_DIR"
        | "KF_CODE_CACHE_DIR"
        | "KF_CODE_AUDIT_LOG_PATH"
        | "KF_CODE_HOOKS_DIR"
        | "KF_CODE_PLUGIN_PUBLIC_KEY_PATH"
        | "KF_CODE_GCP_SERVICE_ACCOUNT_PATH"
        | "KF_CODE_COMPUTER_USE_CHROME_PATH" => Some(Value::String(val.to_string())),
        // Colon-separated (PATH-style) lists.
        "KF_CODE_BASH_ALLOWLIST" | "KF_CODE_LANDLOCK_EXTRA_PATHS" => {
            Some(Value::Array(split_list(val, ':')))
        }
        // Legacy name for compaction_use_heuristic (WO 21.6-R5).
        "KF_CODE_COMPACTION_USE_LLM" => parse_bool_env(val).map(Value::Boolean),
        _ => None,
    }
}

// Comma-separated prefix=Kind pairs, e.g.
// "grok-=OpenAiCompat,my-llm=Ollama". Pairs without '=' are ignored.
fn parse_adapter_routing(val: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for entry in val.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((prefix, kind)) = entry.split_once('=') else {
            continue;
        };
        let prefix = prefix.trim();
        let kind = kind.trim();
        if !prefix.is_empty() && !kind.is_empty() {
            map.insert(prefix.to_string(), kind.to_string());
        }
    }
    map
}
