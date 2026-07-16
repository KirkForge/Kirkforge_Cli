//! Environment-variable overrides for layered config resolution.
//!
//! Extracted from `mod.rs`: reads `KIRKFORGE_*` env vars and applies
//! them to a `Config` (priority layer 2, above the config file).

use crate::shared::Config;
use std::path::PathBuf;

use super::{expand_tilde_str, parse_bool_env, parse_plugin_sources_env};

/// Apply environment variable overrides to a Config.
pub(super) fn apply_env_overrides(cfg: &mut Config) {
    // KIRKFORGE_MODEL
    if let Ok(val) = std::env::var("KIRKFORGE_MODEL") {
        if !val.is_empty() {
            cfg.default_model = val;
        }
    }

    // KIRKFORGE_HOST
    if let Ok(val) = std::env::var("KIRKFORGE_HOST") {
        if !val.is_empty() {
            cfg.ollama_host = val;
        }
    }

    // KIRKFORGE_AUTO_APPROVE
    if let Ok(val) = std::env::var("KIRKFORGE_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.auto_approve = v;
        }
    }

    // KIRKFORGE_SANDBOX_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_SANDBOX_DIR") {
        cfg.sandbox_dir = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KIRKFORGE_BLOCK_DOTFILES
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.block_dotfiles = v;
        }
    }

    // KIRKFORGE_MAX_READ_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_READ_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_file_read_size = n;
        }
    }

    // KIRKFORGE_FOLLOW_SYMLINKS
    if let Ok(val) = std::env::var("KIRKFORGE_FOLLOW_SYMLINKS") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.follow_symlinks = v;
        }
    }

    // KIRKFORGE_BLOCK_BINARY
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_BINARY") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.block_binary_reads = v;
        }
    }

    // KIRKFORGE_MINIFY_WRITE_SIDE
    if let Ok(val) = std::env::var("KIRKFORGE_MINIFY_WRITE_SIDE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.minify_write_side = v;
        }
    }

    // KIRKFORGE_CARRYOVER_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_CARRYOVER_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.carryover_enabled = v;
        }
    }
    // KIRKFORGE_DRY_RUN
    if let Ok(val) = std::env::var("KIRKFORGE_DRY_RUN") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.dry_run = v;
        }
    }

    // KIRKFORGE_CACHE_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.cache_enabled = v;
        }
    }

    // KIRKFORGE_CACHE_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_DIR") {
        cfg.cache_dir = Some(PathBuf::from(expand_tilde_str(&val)));
    }

    // KIRKFORGE_BANG_REQUIRES_APPROVAL
    if let Ok(val) = std::env::var("KIRKFORGE_BANG_REQUIRES_APPROVAL") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.bang_requires_approval = v;
        }
    }

    // KIRKFORGE_JSON_MODE
    if let Ok(val) = std::env::var("KIRKFORGE_JSON_MODE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.json_mode = v;
        }
    }

    // KIRKFORGE_BASH_SANDBOX_WORKDIR
    if let Ok(val) = std::env::var("KIRKFORGE_BASH_SANDBOX_WORKDIR") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.bash_sandbox_workdir = v;
        }
    }

    // KIRKFORGE_BLOCK_GITIGNORED_DOTFILES
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_GITIGNORED_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.block_gitignored_dotfiles = v;
        }
    }

    // KIRKFORGE_MAX_OVERWRITE_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_OVERWRITE_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_overwrite_size = n;
        }
    }

    // KIRKFORGE_SUMMARIZE_MODEL
    if let Ok(val) = std::env::var("KIRKFORGE_SUMMARIZE_MODEL") {
        if !val.is_empty() {
            cfg.summarize_model = val;
        }
    }

    // KIRKFORGE_ROUTING_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_ROUTING_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.routing_enabled = v;
        }
    }

    // KIRKFORGE_ROUTER_MODEL
    if let Ok(val) = std::env::var("KIRKFORGE_ROUTER_MODEL") {
        if !val.is_empty() {
            cfg.router_model = val;
        }
    }

    // KIRKFORGE_COMMIT_MAX_FILE_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_COMMIT_MAX_FILE_SIZE") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.commit_max_file_size = n;
        }
    }

    // KIRKFORGE_PRESERVE_RECENT_MESSAGES
    if let Ok(val) = std::env::var("KIRKFORGE_PRESERVE_RECENT_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.preserve_recent_messages = n.max(1);
        }
    }

    // KIRKFORGE_MAX_TOOL_CALLS_PER_TURN
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_TOOL_CALLS_PER_TURN") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_tool_calls_per_turn = n.max(1);
        }
    }

    // KIRKFORGE_MAX_PERSONA_TURNS
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_PERSONA_TURNS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_persona_turns = n.max(1);
        }
    }

    // KIRKFORGE_TOOL_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KIRKFORGE_TOOL_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.tool_timeout_secs = Some(n.clamp(1, 3600));
        }
    }

    // KIRKFORGE_AUDIT_LOG_PATH
    if let Ok(val) = std::env::var("KIRKFORGE_AUDIT_LOG_PATH") {
        cfg.audit_log_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KIRKFORGE_HOOKS_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_HOOKS_DIR") {
        cfg.hooks_dir = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST
    if let Ok(val) = std::env::var("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.reject_on_excess_plugin_trust = v;
        }
    }

    // KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.plugin_signature_validation = v;
        }
    }

    // KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH") {
        cfg.plugin_public_key_path = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS") {
        cfg.plugin_allowed_env_vars = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KIRKFORGE_PLUGIN_SOURCES
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_SOURCES") {
        cfg.plugin_sources = parse_plugin_sources_env(&val);
    }

    // KIRKFORGE_ENABLED_PLUGINS
    if let Ok(val) = std::env::var("KIRKFORGE_ENABLED_PLUGINS") {
        cfg.enabled_plugins = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KIRKFORGE_MEMORY_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.memory_enabled = v;
        }
    }

    // KIRKFORGE_MEMORY_MAX_TOKENS
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_MAX_TOKENS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.memory_max_tokens = n.max(1);
        }
    }

    // KIRKFORGE_MEMORY_TOP_N
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_TOP_N") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.memory_top_n = n.max(1);
        }
    }

    // KIRKFORGE_REQUEST_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KIRKFORGE_REQUEST_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.request_timeout_secs = n.max(1);
        }
    }

    // KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES
    if let Ok(val) = std::env::var("KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.checkpoint_interval_messages = n;
        }
    }

    // KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE
    if let Ok(val) = std::env::var("KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.scheduled_bash_auto_approve = v;
        }
    }

    // KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_concurrent_scheduled_jobs = n.max(1);
        }
    }

    // Clamp after all layers so a config file or env override cannot set an
    // unusable zero-second timeout.
    cfg.request_timeout_secs = cfg.request_timeout_secs.max(1);
}
