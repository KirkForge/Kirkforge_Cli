//! Config diff summary + plugin-sources env parse.
//!
//! Extracted from `mod.rs`: `config_diff_summary` produces a
//! human-readable diff for TUI display (security/internal knobs
//! omitted); `parse_plugin_sources_env` parses the
//! `KF_CODE_PLUGIN_SOURCES` env var. Both are pure functions over
//! `Config` with no dependency on the bootstrap path.

use crate::shared::Config;
use std::path::PathBuf;

use super::expand_tilde_str;

/// Human-readable summary of config changes. Security/internal knobs
/// (deny lists, allowed dirs, etc.) are intentionally omitted so the
/// summary is suitable for display in the TUI.
pub fn config_diff_summary(before: &Config, after: &Config) -> String {
    let mut diffs: Vec<String> = Vec::new();
    if before.model.default_model != after.model.default_model {
        diffs.push(format!(
            "default_model: {} → {}",
            before.model.default_model, after.model.default_model
        ));
    }
    if before.model.ollama_host != after.model.ollama_host {
        diffs.push(format!(
            "ollama_host: {} → {}",
            before.model.ollama_host, after.model.ollama_host
        ));
    }
    if before.security.auto_approve != after.security.auto_approve {
        diffs.push(format!(
            "auto_approve: {} → {}",
            before.security.auto_approve, after.security.auto_approve
        ));
    }
    if before.security.bang_requires_approval != after.security.bang_requires_approval {
        diffs.push(format!(
            "bang_requires_approval: {} → {}",
            before.security.bang_requires_approval, after.security.bang_requires_approval
        ));
    }
    if before.tools.dry_run != after.tools.dry_run {
        diffs.push(format!(
            "dry_run: {} → {}",
            before.tools.dry_run, after.tools.dry_run
        ));
    }
    if before.model.cache_enabled != after.model.cache_enabled {
        diffs.push(format!(
            "cache_enabled: {} → {}",
            before.model.cache_enabled, after.model.cache_enabled
        ));
    }
    if before.security.sandbox_dir != after.security.sandbox_dir {
        diffs.push(format!(
            "sandbox_dir: {:?} → {:?}",
            before.security.sandbox_dir, after.security.sandbox_dir
        ));
    }
    if before.model.routing_enabled != after.model.routing_enabled {
        diffs.push(format!(
            "routing_enabled: {} → {}",
            before.model.routing_enabled, after.model.routing_enabled
        ));
    }
    if before.model.summarize_enabled != after.model.summarize_enabled {
        diffs.push(format!(
            "summarize_enabled: {} → {}",
            before.model.summarize_enabled, after.model.summarize_enabled
        ));
    }
    if before.tools.reject_on_excess_plugin_trust != after.tools.reject_on_excess_plugin_trust {
        diffs.push(format!(
            "reject_on_excess_plugin_trust: {} → {}",
            before.tools.reject_on_excess_plugin_trust, after.tools.reject_on_excess_plugin_trust
        ));
    }
    if before.tools.plugin_signature_validation != after.tools.plugin_signature_validation {
        diffs.push(format!(
            "plugin_signature_validation: {} → {}",
            before.tools.plugin_signature_validation, after.tools.plugin_signature_validation
        ));
    }
    if before.tools.plugin_trust_workspace != after.tools.plugin_trust_workspace {
        diffs.push(format!(
            "plugin_trust_workspace: {} → {}",
            before.tools.plugin_trust_workspace, after.tools.plugin_trust_workspace
        ));
    }
    if before.tools.plugin_public_key_path != after.tools.plugin_public_key_path {
        diffs.push(format!(
            "plugin_public_key_path: {:?} → {:?}",
            before.tools.plugin_public_key_path, after.tools.plugin_public_key_path
        ));
    }
    if before.display.memory_enabled != after.display.memory_enabled {
        diffs.push(format!(
            "memory_enabled: {} → {}",
            before.display.memory_enabled, after.display.memory_enabled
        ));
    }
    if before.display.memory_max_tokens != after.display.memory_max_tokens {
        diffs.push(format!(
            "memory_max_tokens: {} → {}",
            before.display.memory_max_tokens, after.display.memory_max_tokens
        ));
    }
    if before.display.memory_top_n != after.display.memory_top_n {
        diffs.push(format!(
            "memory_top_n: {} → {}",
            before.display.memory_top_n, after.display.memory_top_n
        ));
    }
    if before.display.memory_auto_populate != after.display.memory_auto_populate {
        diffs.push(format!(
            "memory_auto_populate: {} → {}",
            before.display.memory_auto_populate, after.display.memory_auto_populate
        ));
    }
    if before.display.memory_show_in_status != after.display.memory_show_in_status {
        diffs.push(format!(
            "memory_show_in_status: {} → {}",
            before.display.memory_show_in_status, after.display.memory_show_in_status
        ));
    }
    if before.display.theme != after.display.theme {
        diffs.push(format!(
            "theme: {} → {}",
            before.display.theme, after.display.theme
        ));
    }
    if before.session.checkpoint_interval_messages != after.session.checkpoint_interval_messages {
        diffs.push(format!(
            "checkpoint_interval_messages: {} → {}",
            before.session.checkpoint_interval_messages, after.session.checkpoint_interval_messages
        ));
    }
    if before.tools.enabled_plugins != after.tools.enabled_plugins {
        diffs.push(format!(
            "enabled_plugins: {:?} → {:?}",
            before.tools.enabled_plugins, after.tools.enabled_plugins
        ));
    }
    if before.model.anthropic_provider != after.model.anthropic_provider {
        diffs.push(format!(
            "anthropic_provider: {} → {}",
            before.model.anthropic_provider, after.model.anthropic_provider
        ));
    }
    if before.security.computer_use.enabled != after.security.computer_use.enabled {
        diffs.push(format!(
            "computer_use.enabled: {} → {}",
            before.security.computer_use.enabled, after.security.computer_use.enabled
        ));
    }
    diffs.join(", ")
}

/// Parse `KF_CODE_PLUGIN_SOURCES` env var.
///
/// Format: comma-separated `name=path` entries. Entries without `=` are
/// ignored. Paths are kept exactly as written; the loader canonicalizes
/// them at use time.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn parse_plugin_sources_env(value: &str) -> std::collections::HashMap<String, PathBuf> {
    let mut out = std::collections::HashMap::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let Some((name, path)) = entry.split_once('=') else {
            continue;
        };
        let name = name.trim().to_string();
        let path = path.trim().to_string();
        if name.is_empty() || path.is_empty() {
            continue;
        }
        out.insert(name, PathBuf::from(expand_tilde_str(&path)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_diff_summary_empty_for_equal() {
        let a = Config::default();
        let b = Config::default();
        assert!(config_diff_summary(&a, &b).is_empty());
    }

    #[test]
    fn test_config_diff_summary_model_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.default_model = "kimi-2.7k-coder:cloud".into();
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("default_model"), "got: {s}");
        assert!(s.contains("→ kimi-2.7k-coder:cloud"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_multiple_fields() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.default_model = "kimi-2.7k-coder:cloud".into();
        b.security.auto_approve = true;
        b.model.ollama_host = "https://gateway.example.com".into();
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("default_model"), "got: {s}");
        assert!(s.contains("auto_approve"), "got: {s}");
        assert!(s.contains("ollama_host"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_ignores_internal_fields() {
        let a = Config::default();
        let mut b = Config::default();
        b.security.deny_paths = vec!["/secret".into()];
        b.security.allowed_write_dirs = vec!["/tmp".into()];
        let s = config_diff_summary(&a, &b);
        assert!(
            !s.contains("deny_paths") && !s.contains("allowed_write_dirs"),
            "internal fields leaked: {s}"
        );
        assert!(s.is_empty());
    }

    #[test]
    fn test_config_diff_summary_memory_knobs() {
        let a = Config::default();
        let mut b = Config::default();
        b.display.memory_enabled = false;
        b.display.memory_max_tokens = 250;
        b.display.memory_top_n = 5;
        b.display.memory_auto_populate = false;
        b.display.memory_show_in_status = false;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("memory_enabled"), "got: {s}");
        assert!(s.contains("memory_max_tokens"), "got: {s}");
        assert!(s.contains("memory_top_n"), "got: {s}");
        assert!(s.contains("memory_auto_populate"), "got: {s}");
        assert!(s.contains("memory_show_in_status"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_checkpoint_interval_messages() {
        let a = Config::default();
        let mut b = Config::default();
        b.session.checkpoint_interval_messages = 12;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("checkpoint_interval_messages"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_plugin_trust_knobs() {
        let a = Config::default();
        let mut b = Config::default();
        b.tools.reject_on_excess_plugin_trust = false;
        b.tools.plugin_signature_validation = false;
        b.tools.plugin_public_key_path = Some("/tmp/key.pub".into());
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("reject_on_excess_plugin_trust"), "got: {s}");
        assert!(s.contains("plugin_signature_validation"), "got: {s}");
        assert!(s.contains("plugin_public_key_path"), "got: {s}");
    }

    #[test]
    fn test_parse_plugin_sources_env_empty_string() {
        let result = parse_plugin_sources_env("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_plugin_sources_env_single_entry() {
        let result = parse_plugin_sources_env("core=/path/to/plugins");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get("core"),
            Some(&std::path::PathBuf::from("/path/to/plugins"))
        );
    }

    #[test]
    fn test_parse_plugin_sources_env_multiple_entries() {
        let result = parse_plugin_sources_env("a=/p1,b=/p2,c=/p3");
        assert_eq!(result.len(), 3);
        assert_eq!(result.get("a"), Some(&std::path::PathBuf::from("/p1")));
        assert_eq!(result.get("b"), Some(&std::path::PathBuf::from("/p2")));
        assert_eq!(result.get("c"), Some(&std::path::PathBuf::from("/p3")));
    }

    #[test]
    fn test_parse_plugin_sources_env_ignores_entries_without_equals() {
        let result = parse_plugin_sources_env("nokey,/p1=valid,alsonokey");
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("/p1=valid".split('=').next().unwrap()));
    }

    #[test]
    fn test_parse_plugin_sources_env_trims_whitespace() {
        let result = parse_plugin_sources_env("  core  =  /path/to/plugins  ");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result.get("core"),
            Some(&std::path::PathBuf::from("/path/to/plugins"))
        );
    }

    #[test]
    fn test_parse_plugin_sources_env_ignores_empty_name_or_path() {
        let result = parse_plugin_sources_env("=/p, name= , ,");
        assert!(
            result.is_empty(),
            "empty name/path should be ignored, got: {result:?}"
        );
    }

    #[test]
    fn test_parse_plugin_sources_env_ignores_empty_entries_between_commas() {
        let result = parse_plugin_sources_env("a=/p1,,b=/p2,,");
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("a"));
        assert!(result.contains_key("b"));
    }

    #[test]
    fn test_config_diff_summary_bang_requires_approval_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.security.bang_requires_approval = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("bang_requires_approval"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_dry_run_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.tools.dry_run = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("dry_run"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_cache_enabled_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.cache_enabled = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("cache_enabled"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_sandbox_dir_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.security.sandbox_dir = Some("/new/sandbox".into());
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("sandbox_dir"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_routing_enabled_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.routing_enabled = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("routing_enabled"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_summarize_enabled_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.summarize_enabled = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("summarize_enabled"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_enabled_plugins_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.tools.enabled_plugins = vec!["plugin-x".into()];
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("enabled_plugins"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_ollama_host_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.ollama_host = "http://new:11434".into();
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("ollama_host"), "got: {s}");
        assert!(s.contains("http://new:11434"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_auto_approve_change() {
        let a = Config::default();
        let mut b = Config::default();
        b.security.auto_approve = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("auto_approve"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_anthropic_cloud_and_computer_use() {
        let a = Config::default();
        let mut b = Config::default();
        b.model.anthropic_provider = "bedrock".into();
        b.security.computer_use.enabled = true;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("anthropic_provider"), "got: {s}");
        assert!(s.contains("computer_use.enabled"), "got: {s}");
    }
}
