//! Generic TOML→Config overlay (WO 47.2).
//!
//! The config file layer and the env-var layer both funnel through
//! `merge_toml_into_config`: serialize the current `Config` to its flat
//! TOML form, overlay one key at a time, decode back via serde. The
//! `#[serde(flatten)]` on `Config` does the key→sub-struct routing,
//! typing, defaults, and alias handling, so a new Config field is
//! reachable from both layers with no loader changes. A key whose value
//! doesn't fit the field is skipped (prior value kept) — a bad value can
//! never wipe unrelated fields. Unknown keys decode fine and are
//! dropped by serde, matching the historical soft-merge behavior.

use crate::shared::Config;
use std::path::PathBuf;

use super::expand_tilde_str;

// Legacy TOML/env key aliases: the alias names the same field as the
// primary. Inserting an alias while the serialized base already holds
// the primary would make serde reject a duplicate field, so aliases
// are swapped to the primary before insert.
const KEY_ALIASES: &[(&str, &str)] = &[("compaction_use_llm", "compaction_use_heuristic")];

/// Merge a parsed TOML table into a Config, field by field.
///
/// This handles partial configs gracefully — missing fields keep
/// their current value.
pub(super) fn merge_toml_into_config(cfg: &mut Config, mut table: toml::Table) {
    // Alias precedence: the primary name wins when a source sets both
    // (matches the old hand-parsed if/else-if ordering).
    if table.contains_key("compaction_use_heuristic") {
        table.remove("compaction_use_llm");
    }
    // Flatten collision: memory_auto_populate is declared by BOTH
    // ToolConfig and DisplayConfig. The custom SessionConfig
    // Deserialize between them hides the key from DisplayConfig, and
    // serialization writes display's copy (last writer wins) — the
    // only round-trip-stable state is both fields equal, so apply the
    // incoming value to both directly instead of through the overlay.
    let memory_auto_populate = table
        .remove("memory_auto_populate")
        .and_then(|v| v.as_bool());
    for (key, value) in table {
        let key = KEY_ALIASES
            .iter()
            .find(|(from, _)| *from == key)
            .map(|(_, to)| to.to_string())
            .unwrap_or(key);
        let mut trial = match toml::Value::try_from(&*cfg) {
            Ok(toml::Value::Table(t)) => t,
            _ => return,
        };
        insert_path(&mut trial, &key, value);
        if let Ok(merged) = toml::Value::Table(trial).try_into() {
            *cfg = merged;
        }
    }
    if let Some(v) = memory_auto_populate {
        cfg.tools.memory_auto_populate = v;
        cfg.display.memory_auto_populate = v;
    }
    normalize(cfg);
}

// Insert `value` at `path` (dot-separated for nested fields), merging
// sub-tables so a partial `[section]` doesn't wipe keys set by an
// earlier layer. Non-table values replace whatever was there; a value
// that then fails to decode is dropped by the caller's per-key loop.
fn insert_path(table: &mut toml::Table, path: &str, value: toml::Value) {
    match path.split_once('.') {
        Some((head, rest)) => {
            let entry = table
                .entry(head.to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(sub) = entry {
                insert_path(sub, rest, value);
            }
        }
        None => match value {
            toml::Value::Table(new) => {
                let entry = table
                    .entry(path.to_string())
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let toml::Value::Table(base) = entry {
                    for (k, v) in new {
                        insert_path(base, &k, v);
                    }
                }
            }
            value => {
                table.insert(path.to_string(), value);
            }
        },
    }
}

/// Post-overlay normalization shared by the file and env layers:
/// tilde expansion for path fields and minimum clamps for numeric
/// knobs. Idempotent, so running it after each layer is safe.
fn normalize(cfg: &mut Config) {
    // Path fields: expand `~`, and treat an empty string as "clear"
    // (None) for the Option-path fields. sandbox_dir and cache_dir
    // keep Some("") — the documented "unsandboxed"/unset escape hatch.
    if let Some(dir) = cfg.security.sandbox_dir.clone() {
        cfg.security.sandbox_dir = Some(expand_tilde_str(&dir));
    }
    if let Some(dir) = cfg.model.cache_dir.clone() {
        cfg.model.cache_dir = Some(PathBuf::from(expand_tilde_str(&dir.to_string_lossy())));
    }
    clear_empty_path(&mut cfg.security.audit_log_path);
    clear_empty_path(&mut cfg.tools.hooks_dir);
    clear_empty_path(&mut cfg.model.gcp_service_account_path);
    clear_empty_path(&mut cfg.security.computer_use.chrome_path);
    let pubkey = cfg.tools.plugin_public_key_path.clone();
    if let Some(p) = pubkey {
        cfg.tools.plugin_public_key_path = if p.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&p))
        };
    }
    for p in cfg
        .security
        .deny_paths
        .iter_mut()
        .chain(cfg.security.allowed_write_dirs.iter_mut())
        .chain(cfg.security.landlock_extra_paths.iter_mut())
    {
        *p = expand_tilde_str(p);
    }

    // Numeric knobs: a config file or env override cannot set an
    // unusable zero-second timeout or a zero/negative cap where the
    // consumer divides by or loops on the value.
    cfg.model.request_timeout_secs = cfg.model.request_timeout_secs.max(1);
    cfg.model.streaming_timeout_secs = cfg.model.streaming_timeout_secs.max(1);
    cfg.session.preserve_recent_messages = cfg.session.preserve_recent_messages.max(1);
    cfg.tools.max_tool_calls_per_turn = cfg.tools.max_tool_calls_per_turn.max(1);
    cfg.tools.max_persona_turns = cfg.tools.max_persona_turns.max(1);
    cfg.tools.max_continuation_rounds = cfg.tools.max_continuation_rounds.clamp(0, 50);
    cfg.tools.max_concurrent_scheduled_jobs = cfg.tools.max_concurrent_scheduled_jobs.max(1);
    cfg.tools.max_background_tasks = cfg.tools.max_background_tasks.clamp(1, 64);
    cfg.model.max_tokens = cfg.model.max_tokens.max(1);
    cfg.model.budget_tokens = cfg.model.budget_tokens.max(1);
    if let Some(t) = cfg.tools.tool_timeout_secs {
        cfg.tools.tool_timeout_secs = Some(t.clamp(1, 3600));
    }
    cfg.display.memory_max_tokens = cfg.display.memory_max_tokens.max(1);
    cfg.display.memory_top_n = cfg.display.memory_top_n.max(1);
    let cu = &mut cfg.security.computer_use;
    cu.width = cu.width.max(1);
    cu.height = cu.height.max(1);
    cu.startup_timeout_secs = cu.startup_timeout_secs.max(1);
    cu.wait_timeout_secs = cu.wait_timeout_secs.max(1);
}

// Some("") clears; Some(path) tilde-expands in place.
fn clear_empty_path(p: &mut Option<PathBuf>) {
    if let Some(s) = p.as_ref().and_then(|x| x.to_str()) {
        *p = if s.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(s)))
        };
    }
<<<<<<< HEAD
||||||| 15ad6877

    // Plugin trust / sandbox knobs
    if let Some(Value::Boolean(v)) = table.get("reject_on_excess_plugin_trust") {
        cfg.tools.reject_on_excess_plugin_trust = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_signature_validation") {
        cfg.tools.plugin_signature_validation = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_trust_workspace") {
        cfg.tools.plugin_trust_workspace = *v;
    }
    if let Some(Value::String(v)) = table.get("plugin_public_key_path") {
        cfg.tools.plugin_public_key_path = if v.is_empty() {
            None
        } else {
            Some(expand_tilde_str(v))
        };
    }
    if let Some(Value::Array(v)) = table.get("plugin_allowed_env_vars") {
        cfg.tools.plugin_allowed_env_vars = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Memory knobs
    if let Some(Value::Boolean(v)) = table.get("memory_enabled") {
        cfg.display.memory_enabled = *v;
    }
    if let Some(Value::Integer(v)) = table.get("memory_max_tokens") {
        cfg.display.memory_max_tokens = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("memory_top_n") {
        cfg.display.memory_top_n = (*v).max(1) as usize;
    }
    if let Some(Value::Boolean(v)) = table.get("memory_auto_populate") {
        cfg.display.memory_auto_populate = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("memory_show_in_status") {
        cfg.display.memory_show_in_status = *v;
    }
    if let Some(Value::String(v)) = table.get("theme") {
        cfg.display.theme = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("mouse_enabled") {
        cfg.display.mouse_enabled = *v;
    }
    if let Some(Value::Integer(v)) = table.get("checkpoint_interval_messages") {
        cfg.session.checkpoint_interval_messages = (*v).max(0) as usize;
    }

    // Workspace plugin sources
    if let Some(Value::Table(v)) = table.get("plugin_sources") {
        cfg.tools.plugin_sources = v
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), PathBuf::from(s))))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("enabled_plugins") {
        cfg.tools.enabled_plugins = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("disabled_plugins") {
        cfg.tools.disabled_plugins = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Anthropic cloud-provider routing
    if let Some(Value::String(v)) = table.get("anthropic_provider") {
        cfg.model.anthropic_provider = v.clone();
    }
    if let Some(Value::String(v)) = table.get("anthropic_api_base") {
        cfg.model.anthropic_api_base = v.clone();
    }
    if let Some(Value::String(v)) = table.get("aws_region") {
        cfg.model.aws_region = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_project_id") {
        cfg.model.gcp_project_id = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_region") {
        cfg.model.gcp_region = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_service_account_path") {
        cfg.model.gcp_service_account_path = if v.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(v)))
        };
    }

    // Per-provider API keys
    if let Some(Value::String(v)) = table.get("anthropic_api_key") {
        cfg.model.anthropic_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("openai_api_key") {
        cfg.model.openai_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("deepseek_api_key") {
        cfg.model.deepseek_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("gemini_api_key") {
        cfg.model.gemini_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("kimi_api_key") {
        cfg.model.kimi_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }

    // Config-driven pricing overrides (WO 38.5):
    // [price_overrides."<prefix>"] with input_per_mtok /
    // output_per_mtok / cache_write_per_mtok / cache_read_per_mtok.
    if let Some(Value::Table(v)) = table.get("price_overrides") {
        for (prefix, entry) in v {
            let Some(t) = entry.as_table() else { continue };
            let rate = |key: &str| t.get(key).and_then(|x| x.as_float()).unwrap_or(0.0);
            cfg.model.price_overrides.insert(
                prefix.clone(),
                crate::shared::ModelPrice {
                    input_per_mtok: rate("input_per_mtok"),
                    output_per_mtok: rate("output_per_mtok"),
                    cache_write_per_mtok: rate("cache_write_per_mtok"),
                    cache_read_per_mtok: rate("cache_read_per_mtok"),
                },
            );
        }
    }

    // Computer-use tool config
    if let Some(Value::Table(v)) = table.get("computer_use") {
        if let Some(Value::Boolean(b)) = v.get("enabled") {
            cfg.security.computer_use.enabled = *b;
        }
        if let Some(Value::String(s)) = v.get("chrome_path") {
            cfg.security.computer_use.chrome_path = if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(expand_tilde_str(s)))
            };
        }
        if let Some(Value::Boolean(b)) = v.get("headful") {
            cfg.security.computer_use.headful = *b;
        }
        if let Some(Value::Integer(n)) = v.get("width") {
            cfg.security.computer_use.width = (*n).max(1) as u32;
        }
        if let Some(Value::Integer(n)) = v.get("height") {
            cfg.security.computer_use.height = (*n).max(1) as u32;
        }
        if let Some(Value::Integer(n)) = v.get("startup_timeout_secs") {
            cfg.security.computer_use.startup_timeout_secs = (*n).max(1) as u64;
        }
        if let Some(Value::Integer(n)) = v.get("wait_timeout_secs") {
            cfg.security.computer_use.wait_timeout_secs = (*n).max(1) as u64;
        }
        if let Some(Value::Boolean(b)) = v.get("hosted") {
            cfg.security.computer_use.hosted = *b;
        }
    }

    // Arrays
    if let Some(Value::Array(v)) = table.get("deny_paths") {
        cfg.security.deny_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_urls") {
        cfg.security.deny_urls = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_extensions") {
        cfg.security.deny_extensions = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("allowed_write_dirs") {
        cfg.security.allowed_write_dirs = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("landlock_extra_paths") {
        cfg.security.landlock_extra_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }

    // Subagent provider override (WO 30.0.6 brain+brawn). Each field is
    // optional; an empty string normalises to None so the subagent falls
    // back to the parent's value.
    if let Some(Value::Table(v)) = table.get("subagent_provider") {
        if let Some(Value::String(s)) = v.get("model") {
            cfg.model.subagent_provider.model = if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("ollama_host") {
            cfg.model.subagent_provider.ollama_host =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("anthropic_api_key") {
            cfg.model.subagent_provider.anthropic_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("openai_api_key") {
            cfg.model.subagent_provider.openai_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("deepseek_api_key") {
            cfg.model.subagent_provider.deepseek_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("gemini_api_key") {
            cfg.model.subagent_provider.gemini_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("kimi_api_key") {
            cfg.model.subagent_provider.kimi_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
    }

    if let Some(Value::Integer(v)) = table.get("budget_ceiling") {
        cfg.tools.budget_ceiling = (*v as usize).max(0);
    }
    if let Some(Value::Boolean(v)) = table.get("summarize_enabled") {
        cfg.model.summarize_enabled = *v;
    }
=======

    // Plugin trust / sandbox knobs
    if let Some(Value::Boolean(v)) = table.get("reject_on_excess_plugin_trust") {
        cfg.tools.reject_on_excess_plugin_trust = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_signature_validation") {
        cfg.tools.plugin_signature_validation = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_trust_workspace") {
        cfg.tools.plugin_trust_workspace = *v;
    }
    if let Some(Value::String(v)) = table.get("plugin_public_key_path") {
        cfg.tools.plugin_public_key_path = if v.is_empty() {
            None
        } else {
            Some(expand_tilde_str(v))
        };
    }
    if let Some(Value::Array(v)) = table.get("plugin_allowed_env_vars") {
        cfg.tools.plugin_allowed_env_vars = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Memory knobs
    if let Some(Value::Boolean(v)) = table.get("memory_enabled") {
        cfg.display.memory_enabled = *v;
    }
    if let Some(Value::Integer(v)) = table.get("memory_max_tokens") {
        cfg.display.memory_max_tokens = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("memory_top_n") {
        cfg.display.memory_top_n = (*v).max(1) as usize;
    }
    if let Some(Value::Boolean(v)) = table.get("memory_auto_populate") {
        cfg.display.memory_auto_populate = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("memory_show_in_status") {
        cfg.display.memory_show_in_status = *v;
    }
    if let Some(Value::String(v)) = table.get("theme") {
        cfg.display.theme = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("mouse_enabled") {
        cfg.display.mouse_enabled = *v;
    }
    if let Some(Value::Array(v)) = table.get("extra_commands") {
        cfg.display.extra_commands = v
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(Value::Integer(v)) = table.get("checkpoint_interval_messages") {
        cfg.session.checkpoint_interval_messages = (*v).max(0) as usize;
    }

    // Workspace plugin sources
    if let Some(Value::Table(v)) = table.get("plugin_sources") {
        cfg.tools.plugin_sources = v
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), PathBuf::from(s))))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("enabled_plugins") {
        cfg.tools.enabled_plugins = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("disabled_plugins") {
        cfg.tools.disabled_plugins = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Anthropic cloud-provider routing
    if let Some(Value::String(v)) = table.get("anthropic_provider") {
        cfg.model.anthropic_provider = v.clone();
    }
    if let Some(Value::String(v)) = table.get("anthropic_api_base") {
        cfg.model.anthropic_api_base = v.clone();
    }
    if let Some(Value::String(v)) = table.get("aws_region") {
        cfg.model.aws_region = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_project_id") {
        cfg.model.gcp_project_id = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_region") {
        cfg.model.gcp_region = v.clone();
    }
    if let Some(Value::String(v)) = table.get("gcp_service_account_path") {
        cfg.model.gcp_service_account_path = if v.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(v)))
        };
    }

    // Per-provider API keys
    if let Some(Value::String(v)) = table.get("anthropic_api_key") {
        cfg.model.anthropic_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("openai_api_key") {
        cfg.model.openai_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("deepseek_api_key") {
        cfg.model.deepseek_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("gemini_api_key") {
        cfg.model.gemini_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(Value::String(v)) = table.get("kimi_api_key") {
        cfg.model.kimi_api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }

    // Config-driven pricing overrides (WO 38.5):
    // [price_overrides."<prefix>"] with input_per_mtok /
    // output_per_mtok / cache_write_per_mtok / cache_read_per_mtok.
    if let Some(Value::Table(v)) = table.get("price_overrides") {
        for (prefix, entry) in v {
            let Some(t) = entry.as_table() else { continue };
            let rate = |key: &str| t.get(key).and_then(|x| x.as_float()).unwrap_or(0.0);
            cfg.model.price_overrides.insert(
                prefix.clone(),
                crate::shared::ModelPrice {
                    input_per_mtok: rate("input_per_mtok"),
                    output_per_mtok: rate("output_per_mtok"),
                    cache_write_per_mtok: rate("cache_write_per_mtok"),
                    cache_read_per_mtok: rate("cache_read_per_mtok"),
                },
            );
        }
    }

    // Computer-use tool config
    if let Some(Value::Table(v)) = table.get("computer_use") {
        if let Some(Value::Boolean(b)) = v.get("enabled") {
            cfg.security.computer_use.enabled = *b;
        }
        if let Some(Value::String(s)) = v.get("chrome_path") {
            cfg.security.computer_use.chrome_path = if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(expand_tilde_str(s)))
            };
        }
        if let Some(Value::Boolean(b)) = v.get("headful") {
            cfg.security.computer_use.headful = *b;
        }
        if let Some(Value::Integer(n)) = v.get("width") {
            cfg.security.computer_use.width = (*n).max(1) as u32;
        }
        if let Some(Value::Integer(n)) = v.get("height") {
            cfg.security.computer_use.height = (*n).max(1) as u32;
        }
        if let Some(Value::Integer(n)) = v.get("startup_timeout_secs") {
            cfg.security.computer_use.startup_timeout_secs = (*n).max(1) as u64;
        }
        if let Some(Value::Integer(n)) = v.get("wait_timeout_secs") {
            cfg.security.computer_use.wait_timeout_secs = (*n).max(1) as u64;
        }
        if let Some(Value::Boolean(b)) = v.get("hosted") {
            cfg.security.computer_use.hosted = *b;
        }
    }

    // Arrays
    if let Some(Value::Array(v)) = table.get("deny_paths") {
        cfg.security.deny_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_urls") {
        cfg.security.deny_urls = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_extensions") {
        cfg.security.deny_extensions = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("allowed_write_dirs") {
        cfg.security.allowed_write_dirs = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("landlock_extra_paths") {
        cfg.security.landlock_extra_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }

    // Subagent provider override (WO 30.0.6 brain+brawn). Each field is
    // optional; an empty string normalises to None so the subagent falls
    // back to the parent's value.
    if let Some(Value::Table(v)) = table.get("subagent_provider") {
        if let Some(Value::String(s)) = v.get("model") {
            cfg.model.subagent_provider.model = if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("ollama_host") {
            cfg.model.subagent_provider.ollama_host =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("anthropic_api_key") {
            cfg.model.subagent_provider.anthropic_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("openai_api_key") {
            cfg.model.subagent_provider.openai_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("deepseek_api_key") {
            cfg.model.subagent_provider.deepseek_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("gemini_api_key") {
            cfg.model.subagent_provider.gemini_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
        if let Some(Value::String(s)) = v.get("kimi_api_key") {
            cfg.model.subagent_provider.kimi_api_key =
                if s.is_empty() { None } else { Some(s.clone()) };
        }
    }

    if let Some(Value::Integer(v)) = table.get("budget_ceiling") {
        cfg.tools.budget_ceiling = (*v as usize).max(0);
    }
    if let Some(Value::Boolean(v)) = table.get("summarize_enabled") {
        cfg.model.summarize_enabled = *v;
    }
>>>>>>> wo/wo47.13
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_toml_partial() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            default_model = "custom-model"
            max_file_read_size = 512
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert_eq!(cfg.model.default_model, "custom-model");
        assert_eq!(cfg.security.max_file_read_size, 512);
        // Unset fields keep defaults (ollama_host defaults to localhost:11434)
        assert_eq!(
            cfg.model.ollama_host, "http://localhost:11434",
            "ollama_host defaults to localhost:11434"
        );
        assert!(!cfg.security.auto_approve);
    }

    #[test]
    fn test_merge_toml_negative_max_read_size_is_ignored() {
        let mut cfg = Config::default();
        let default_size = cfg.security.max_file_read_size;
        let table: toml::Table = r#"
            max_file_read_size = -1
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert_eq!(
            cfg.security.max_file_read_size, default_size,
            "negative max_file_read_size should be ignored, not wrap to usize::MAX"
        );
    }

    #[test]
    fn test_merge_toml_arrays() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            deny_paths = ["**/.ssh/**", "**/secret/**"]
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert_eq!(cfg.security.deny_paths.len(), 2);
        assert!(cfg.security.deny_paths.contains(&"**/.ssh/**".into()));
    }

    #[test]
    fn test_merge_toml_misc_fields() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            bang_requires_approval = true
            json_mode = true
            bash_sandbox_workdir = false
            block_gitignored_dotfiles = false
            max_overwrite_size = 2097152
            summarize_model = "my-summarize-model"
            routing_enabled = true
            router_model = "my-router-model"
            routing_model_map = { simple = "glm-5.2:cloud" }
            commit_max_file_size = 1048576
            preserve_recent_messages = 5
            max_tool_calls_per_turn = 25
            max_persona_turns = 3
            tool_timeout_secs = 60
            audit_log_path = "/tmp/kf-audit.ndjson"
            hooks_dir = "/tmp/kf-hooks"
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert!(cfg.security.bang_requires_approval);
        assert!(cfg.model.json_mode);
        assert!(!cfg.security.bash_sandbox_workdir);
        assert!(!cfg.security.block_gitignored_dotfiles);
        assert_eq!(cfg.security.max_overwrite_size, 2_097_152);
        assert_eq!(cfg.model.summarize_model, "my-summarize-model");
        assert!(cfg.model.routing_enabled);
        assert_eq!(cfg.model.router_model, "my-router-model");
        assert_eq!(
            cfg.model.routing_model_map.get("simple"),
            Some(&"glm-5.2:cloud".to_string())
        );
        assert_eq!(cfg.security.commit_max_file_size, 1_048_576);
        assert_eq!(cfg.session.preserve_recent_messages, 5);
        assert_eq!(cfg.tools.max_tool_calls_per_turn, 25);
        assert_eq!(cfg.tools.max_persona_turns, 3);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(60));
        assert_eq!(
            cfg.security.audit_log_path,
            Some(PathBuf::from("/tmp/kf-audit.ndjson"))
        );
        assert_eq!(cfg.tools.hooks_dir, Some(PathBuf::from("/tmp/kf-hooks")));
    }

    #[test]
    fn test_merge_toml_tool_timeout_secs_is_clamped() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            tool_timeout_secs = 7200
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(3600));
    }

    #[test]
    fn test_merge_toml_plugin_trust_knobs() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            reject_on_excess_plugin_trust = false
            plugin_signature_validation = true
            plugin_public_key_path = "/opt/kf-code/plugin.pub"
            plugin_allowed_env_vars = ["CUSTOM_VAR"]
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert!(!cfg.tools.reject_on_excess_plugin_trust);
        assert!(cfg.tools.plugin_signature_validation);
        assert_eq!(
            cfg.tools.plugin_public_key_path.as_deref(),
            Some("/opt/kf-code/plugin.pub")
        );
        assert_eq!(cfg.tools.plugin_allowed_env_vars, vec!["CUSTOM_VAR"]);
    }

    #[test]
    fn test_merge_toml_empty_path_strings_yield_none() {
        // Empty-string path fields in the config file must map to `None`
        // (clearing a previously-set path) rather than a stray empty PathBuf.
        let mut cfg = Config::default();
        cfg.security.audit_log_path = Some("/prev/audit.log".into());
        cfg.tools.hooks_dir = Some("/prev/hooks".into());
        cfg.tools.plugin_public_key_path = Some("/prev/pub.key".into());
        let table: toml::Table = r#"
            audit_log_path = ""
            hooks_dir = ""
            plugin_public_key_path = ""
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert!(
            cfg.security.audit_log_path.is_none(),
            "empty audit_log_path → None"
        );
        assert!(cfg.tools.hooks_dir.is_none(), "empty hooks_dir → None");
        assert!(
            cfg.tools.plugin_public_key_path.is_none(),
            "empty plugin_public_key_path → None"
        );
    }

    #[test]
    fn test_merge_toml_memory_knobs() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            memory_enabled = false
            memory_max_tokens = 300
            memory_top_n = 3
            memory_auto_populate = false
            memory_show_in_status = false
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert!(!cfg.display.memory_enabled);
        assert_eq!(cfg.display.memory_max_tokens, 300);
        assert_eq!(cfg.display.memory_top_n, 3);
        assert!(!cfg.display.memory_auto_populate);
        assert!(!cfg.display.memory_show_in_status);
    }

    #[test]
    fn test_merge_toml_checkpoint_interval_messages() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            checkpoint_interval_messages = 15
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.session.checkpoint_interval_messages, 15);
    }

    // WO 27.2-R2: un-ignored after fixing the test fixture. The alias
    // wiring (compaction_use_llm → compaction_use_heuristic) rides on
    // SessionConfig's serde alias, exercised here through the flat
    // overlay path.
    #[test]
    fn compaction_use_llm_alias_backward_compat() {
        let toml = "compaction_use_llm = true\n";
        let table: toml::Table = toml.parse().expect("parse toml table");
        let mut cfg = Config::default();
        merge_toml_into_config(&mut cfg, table);
        assert!(
            cfg.session.compaction_use_heuristic,
            "compaction_use_llm (old name) must map to compaction_use_heuristic"
        );
    }

    #[test]
    fn test_merge_toml_minify_write_side() {
        let mut cfg = Config::default();
        assert!(
            cfg.tools.minify_write_side,
            "WO 46.5: serde and Default impl now agree on true"
        );
        let table: toml::Table = r#"
            minify_write_side = false
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert!(!cfg.tools.minify_write_side);
    }

    #[test]
    fn test_merge_toml_minify_above_bytes() {
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.minify_above_bytes, 4096);
        let table: toml::Table = r#"
            minify_above_bytes = 1024
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.tools.minify_above_bytes, 1024);
    }

    #[test]
    fn test_merge_toml_anthropic_cloud_and_computer_use() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            anthropic_provider = "bedrock"
            anthropic_api_base = "https://my-anthropic-proxy.example.com"
            aws_region = "us-west-2"
            gcp_project_id = "my-project"
            gcp_region = "us-east4"
            gcp_service_account_path = "/tmp/sa.json"
            [computer_use]
            enabled = true
            chrome_path = "/usr/bin/chromium"
            headful = true
            width = 1920
            height = 1080
            startup_timeout_secs = 45
            wait_timeout_secs = 15
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert_eq!(cfg.model.anthropic_provider, "bedrock");
        assert_eq!(
            cfg.model.anthropic_api_base,
            "https://my-anthropic-proxy.example.com"
        );
        assert_eq!(cfg.model.aws_region, "us-west-2");
        assert_eq!(cfg.model.gcp_project_id, "my-project");
        assert_eq!(cfg.model.gcp_region, "us-east4");
        assert_eq!(
            cfg.model.gcp_service_account_path,
            Some(PathBuf::from("/tmp/sa.json"))
        );
        assert!(cfg.security.computer_use.enabled);
        assert_eq!(
            cfg.security.computer_use.chrome_path,
            Some(PathBuf::from("/usr/bin/chromium"))
        );
        assert!(cfg.security.computer_use.headful);
        assert_eq!(cfg.security.computer_use.width, 1920);
        assert_eq!(cfg.security.computer_use.height, 1080);
        assert_eq!(cfg.security.computer_use.startup_timeout_secs, 45);
        assert_eq!(cfg.security.computer_use.wait_timeout_secs, 15);
    }

    #[test]
    fn test_merge_toml_scheduled_job_knobs() {
        let mut cfg = Config::default();
        assert!(!cfg.tools.scheduled_bash_auto_approve);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 4);
        let table: toml::Table = r#"
            scheduled_bash_auto_approve = true
            max_concurrent_scheduled_jobs = 0
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert!(cfg.tools.scheduled_bash_auto_approve);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 1);
    }

    #[test]
    fn test_merge_toml_background_task_knobs() {
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.max_background_tasks, 4);
        assert_eq!(cfg.tools.task_concurrency_mode, "queue");
        let table: toml::Table = r#"
            max_background_tasks = 8
            task_concurrency_mode = "reject"
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.tools.max_background_tasks, 8);
        assert_eq!(cfg.tools.task_concurrency_mode, "reject");
    }

    #[test]
    fn test_merge_toml_background_task_clamps_to_range() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            max_background_tasks = 0
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(
            cfg.tools.max_background_tasks, 1,
            "0 should be clamped to 1"
        );
        let table: toml::Table = r#"
            max_background_tasks = 100
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(
            cfg.tools.max_background_tasks, 64,
            "100 should be clamped to 64"
        );
    }

    #[test]
    fn test_merge_toml_zero_request_timeout_is_clamped() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            request_timeout_secs = 0
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(
            cfg.model.request_timeout_secs, 1,
            "zero timeout must be clamped to 1 second"
        );
    }

    #[test]
    fn test_merge_toml_adapter_routing() {
        let mut cfg = Config::default();
        assert!(cfg.model.adapter_routing.is_empty());
        let table: toml::Table = r#"
            [adapter_routing]
            "claude-" = "Anthropic"
            "deepseek" = "OpenAiCompat"
            "glm" = "Ollama"
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.model.adapter_routing.len(), 3);
        assert_eq!(
            cfg.model.adapter_routing.get("claude-"),
            Some(&"Anthropic".to_string())
        );
        assert_eq!(
            cfg.model.adapter_routing.get("deepseek"),
            Some(&"OpenAiCompat".to_string())
        );
        assert_eq!(
            cfg.model.adapter_routing.get("glm"),
            Some(&"Ollama".to_string())
        );
    }

    // WO 38.5: [price_overrides."<prefix>"] parses into
    // ModelConfig.price_overrides.
    #[test]
    fn price_overrides_table_parses() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            [price_overrides."big-pickle"]
            input_per_mtok = 1.0
            output_per_mtok = 2.0
            cache_write_per_mtok = 1.25
            cache_read_per_mtok = 0.1
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        let p = cfg
            .model
            .price_overrides
            .get("big-pickle")
            .expect("override present");
        assert!((p.input_per_mtok - 1.0).abs() < 1e-9);
        assert!((p.output_per_mtok - 2.0).abs() < 1e-9);
        assert!((p.cache_write_per_mtok - 1.25).abs() < 1e-9);
        assert!((p.cache_read_per_mtok - 0.1).abs() < 1e-9);
    }

    // WO 47.2: a bad-typed value must only skip its own key — the
    // hand-merged predecessor guaranteed this per-if-let, and the
    // serde overlay must keep the guarantee (it's what stops one
    // wrong line in a user's config.toml from wiping every other
    // customization).
    #[test]
    fn bad_typed_key_does_not_wipe_sibling_keys() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            default_model = "keep-me"
            max_file_read_size = "not-a-number"
            auto_approve = true
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.model.default_model, "keep-me");
        assert!(cfg.security.auto_approve);
        assert_eq!(
            cfg.security.max_file_read_size,
            Config::default().security.max_file_read_size
        );
    }

    // WO 47.2: unknown keys are soft-merged (ignored), never a load
    // failure — pinned by the strict-loader contract too.
    #[test]
    fn unknown_keys_are_ignored() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            default_model = "x"
            no_such_field = 42
            [no_such_table]
            inner = "y"
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert_eq!(cfg.model.default_model, "x");
    }

    // WO 47.2: a partial [computer_use] from a later layer must merge
    // into the current values, not replace the whole sub-table (the
    // env layer overlays computer_use.width after the file layer may
    // have set computer_use.headful).
    #[test]
    fn partial_sub_table_merges_without_wiping_siblings() {
        let mut cfg = Config::default();
        cfg.security.computer_use.headful = true;
        let table: toml::Table = r#"
            [computer_use]
            width = 999
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert!(cfg.security.computer_use.headful, "sibling key survives");
        assert_eq!(cfg.security.computer_use.width, 999);
    }
}
