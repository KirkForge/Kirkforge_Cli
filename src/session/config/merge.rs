//! TOML→Config field-by-field merge.
//!
//! Extracted from `mod.rs`: handles partial configs gracefully — missing
//! fields keep their current value. `merge_toml_into_config` is the single
//! largest function in the config module; isolating it keeps `mod.rs`
//! focused on bootstrap/load/save.

use crate::shared::Config;
use std::path::PathBuf;

use super::expand_tilde_str;

/// Merge a parsed TOML table into a Config, field by field.
///
/// This handles partial configs gracefully — missing fields keep
/// their current value.
pub(super) fn merge_toml_into_config(cfg: &mut Config, table: toml::Table) {
    use toml::Value;

    if let Some(Value::String(v)) = table.get("default_model") {
        cfg.model.default_model = v.clone();
    }
    if let Some(Value::String(v)) = table.get("ollama_host") {
        cfg.model.ollama_host = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("auto_approve") {
        cfg.security.auto_approve = *v;
    }
    if let Some(Value::String(v)) = table.get("sandbox_dir") {
        cfg.security.sandbox_dir = Some(expand_tilde_str(v));
    }
    if let Some(Value::Boolean(v)) = table.get("block_dotfiles") {
        cfg.security.block_dotfiles = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_file_read_size") {
        if let Ok(n) = usize::try_from(*v) {
            cfg.security.max_file_read_size = n;
        }
    }
    if let Some(Value::Integer(v)) = table.get("request_timeout_secs") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.model.request_timeout_secs = n.max(1);
        }
    }
    if let Some(Value::Integer(v)) = table.get("streaming_timeout_secs") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.model.streaming_timeout_secs = n.max(1);
        }
    }
    if let Some(Value::Boolean(v)) = table.get("follow_symlinks") {
        cfg.tools.follow_symlinks = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("block_binary_reads") {
        cfg.tools.block_binary_reads = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("minify_write_side") {
        cfg.tools.minify_write_side = *v;
    }
    if let Some(Value::Integer(v)) = table.get("minify_above_bytes") {
        cfg.tools.minify_above_bytes = (*v as usize).max(0);
    }
    if let Some(Value::Boolean(v)) = table.get("scheduled_bash_auto_approve") {
        cfg.tools.scheduled_bash_auto_approve = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_concurrent_scheduled_jobs") {
        cfg.tools.max_concurrent_scheduled_jobs = (*v as usize).max(1);
    }
    if let Some(Value::Boolean(v)) = table.get("carryover_enabled") {
        cfg.session.carryover_enabled = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("compaction_use_heuristic") {
        cfg.session.compaction_use_heuristic = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("compaction_use_llm") {
        cfg.session.compaction_use_heuristic = *v;
    }
    if let Some(Value::Float(v)) = table.get("compaction_drop_threshold") {
        cfg.session.compaction_drop_threshold = *v;
    }
    if let Some(Value::Integer(v)) = table.get("stem_file_cap") {
        if let Ok(n) = usize::try_from(*v) {
            cfg.session.stem_file_cap = Some(n);
        }
    }
    if let Some(Value::Integer(v)) = table.get("shutdown_timeout_secs") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.session.shutdown_timeout_secs = Some(n);
        }
    }
    if let Some(Value::Boolean(v)) = table.get("dry_run") {
        cfg.tools.dry_run = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("cache_enabled") {
        cfg.model.cache_enabled = *v;
    }
    if let Some(Value::String(v)) = table.get("cache_dir") {
        cfg.model.cache_dir = Some(PathBuf::from(expand_tilde_str(v)));
    }
    if let Some(Value::Boolean(v)) = table.get("bang_requires_approval") {
        cfg.security.bang_requires_approval = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("json_mode") {
        cfg.model.json_mode = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_tokens") {
        if let Ok(n) = u32::try_from(*v) {
            cfg.model.max_tokens = n.max(1);
        }
    }
    if let Some(Value::Boolean(v)) = table.get("extended_thinking") {
        cfg.model.extended_thinking = *v;
    }
    if let Some(Value::Integer(v)) = table.get("budget_tokens") {
        if let Ok(n) = usize::try_from(*v) {
            cfg.model.budget_tokens = n.max(1);
        }
    }
    if let Some(Value::Boolean(v)) = table.get("bash_sandbox_workdir") {
        cfg.security.bash_sandbox_workdir = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("bash_require_allowlist") {
        cfg.security.bash_require_allowlist = *v;
    }
    if let Some(Value::Array(v)) = table.get("bash_allowlist") {
        cfg.security.bash_allowlist = v
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(Value::Boolean(v)) = table.get("block_gitignored_dotfiles") {
        cfg.security.block_gitignored_dotfiles = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_overwrite_size") {
        if let Ok(n) = usize::try_from(*v) {
            cfg.security.max_overwrite_size = n;
        }
    }
    if let Some(Value::String(v)) = table.get("summarize_model") {
        cfg.model.summarize_model = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("routing_enabled") {
        cfg.model.routing_enabled = *v;
    }
    if let Some(Value::String(v)) = table.get("router_model") {
        cfg.model.router_model = v.clone();
    }
    if let Some(Value::Table(v)) = table.get("routing_model_map") {
        cfg.model.routing_model_map = v
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
    }
    if let Some(Value::Table(v)) = table.get("adapter_routing") {
        cfg.model.adapter_routing = v
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
    }
    if let Some(Value::Integer(v)) = table.get("commit_max_file_size") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.security.commit_max_file_size = n;
        }
    }
    if let Some(Value::Integer(v)) = table.get("preserve_recent_messages") {
        cfg.session.preserve_recent_messages = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("max_tool_calls_per_turn") {
        cfg.tools.max_tool_calls_per_turn = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("max_persona_turns") {
        cfg.tools.max_persona_turns = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("max_continuation_rounds") {
        cfg.tools.max_continuation_rounds = (*v).clamp(0, 50) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("doom_loop_max_hits") {
        cfg.tools.doom_loop_max_hits = (*v).max(0) as usize;
    }
    if let Some(Value::String(v)) = table.get("doom_loop_action") {
        cfg.tools.doom_loop_action = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("load_project_mcp_json") {
        cfg.tools.load_project_mcp_json = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_consent_ledger") {
        cfg.tools.plugin_consent_ledger = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_background_tasks") {
        cfg.tools.max_background_tasks = (*v as usize).clamp(1, 64);
    }
    if let Some(Value::String(v)) = table.get("task_concurrency_mode") {
        let mode = v.to_lowercase();
        if mode == "queue" || mode == "reject" {
            cfg.tools.task_concurrency_mode = mode;
        }
    }
    if let Some(Value::Integer(v)) = table.get("tool_timeout_secs") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.tools.tool_timeout_secs = Some(n.clamp(1, 3600));
        }
    }
    if let Some(Value::String(v)) = table.get("audit_log_path") {
        cfg.security.audit_log_path = if v.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(v)))
        };
    }
    if let Some(Value::Boolean(v)) = table.get("diff_review") {
        cfg.security.diff_review = *v;
    }
    if let Some(Value::String(v)) = table.get("hooks_dir") {
        cfg.tools.hooks_dir = if v.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(v)))
        };
    }

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
    // wiring (compaction_use_llm → compaction_use_heuristic) is flat
    // top-level in merge_toml_into_config; the original test wrapped
    // the key in [session], which the fallback merger doesn't descend
    // into. The primary serde path handles [session] via SessionConfig
    // (with the alias = "compaction_use_llm" annotation); this test
    // exercises the flat fallback path.
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
}
