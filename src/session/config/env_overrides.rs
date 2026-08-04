//! Environment-variable overrides for layered config resolution.
//!
//! Extracted from `mod.rs`: reads `KF_CODE_*` env vars and applies
//! them to a `Config` (priority layer 2, above the config file).

use crate::shared::Config;
use std::path::PathBuf;

use super::{expand_tilde_str, parse_bool_env, parse_plugin_sources_env};

/// Apply environment variable overrides to a Config.
pub(super) fn apply_env_overrides(cfg: &mut Config) {
    // KF_CODE_MODEL
    if let Ok(val) = std::env::var("KF_CODE_MODEL") {
        if !val.is_empty() {
            cfg.model.default_model = val;
        }
    }

    // KF_CODE_HOST
    if let Ok(val) = std::env::var("KF_CODE_HOST") {
        if !val.is_empty() {
            cfg.model.ollama_host = val;
        }
    }

    // KF_CODE_AUTO_APPROVE
    if let Ok(val) = std::env::var("KF_CODE_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.auto_approve = v;
        }
    }

    // KF_CODE_SANDBOX_DIR
    if let Ok(val) = std::env::var("KF_CODE_SANDBOX_DIR") {
        cfg.security.sandbox_dir = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KF_CODE_BLOCK_DOTFILES
    if let Ok(val) = std::env::var("KF_CODE_BLOCK_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.block_dotfiles = v;
        }
    }

    // KF_CODE_MAX_READ_SIZE
    if let Ok(val) = std::env::var("KF_CODE_MAX_READ_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.security.max_file_read_size = n;
        }
    }

    // KF_CODE_FOLLOW_SYMLINKS
    if let Ok(val) = std::env::var("KF_CODE_FOLLOW_SYMLINKS") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.follow_symlinks = v;
        }
    }

    // KF_CODE_BLOCK_BINARY
    if let Ok(val) = std::env::var("KF_CODE_BLOCK_BINARY") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.block_binary_reads = v;
        }
    }

    // KF_CODE_MINIFY_WRITE_SIDE
    if let Ok(val) = std::env::var("KF_CODE_MINIFY_WRITE_SIDE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.minify_write_side = v;
        }
    }

    // KF_CODE_MINIFY_ABOVE_BYTES
    if let Ok(val) = std::env::var("KF_CODE_MINIFY_ABOVE_BYTES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.minify_above_bytes = n;
        }
    }

    // KF_CODE_BUDGET_CEILING
    // WO 14.7: the Token Budget Challenge exports this to pin the
    // token budget ceiling for a single run. The bench runner sets
    // it before invoking the model; init_from_config reads it off
    // cfg.tools.budget_ceiling via the standard env-override layer.
    if let Ok(val) = std::env::var("KF_CODE_BUDGET_CEILING") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.budget_ceiling = n;
        }
    }

    // KF_CODE_CARRYOVER_ENABLED
    if let Ok(val) = std::env::var("KF_CODE_CARRYOVER_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.session.carryover_enabled = v;
        }
    }
    // KF_CODE_DRY_RUN
    if let Ok(val) = std::env::var("KF_CODE_DRY_RUN") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.dry_run = v;
        }
    }

    // KF_CODE_CACHE_ENABLED
    if let Ok(val) = std::env::var("KF_CODE_CACHE_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.cache_enabled = v;
        }
    }

    // KF_CODE_CACHE_DIR
    if let Ok(val) = std::env::var("KF_CODE_CACHE_DIR") {
        cfg.model.cache_dir = Some(PathBuf::from(expand_tilde_str(&val)));
    }

    // KF_CODE_BANG_REQUIRES_APPROVAL
    if let Ok(val) = std::env::var("KF_CODE_BANG_REQUIRES_APPROVAL") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.bang_requires_approval = v;
        }
    }

    // KF_CODE_JSON_MODE
    if let Ok(val) = std::env::var("KF_CODE_JSON_MODE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.json_mode = v;
        }
    }

    // KF_CODE_BASH_SANDBOX_WORKDIR
    if let Ok(val) = std::env::var("KF_CODE_BASH_SANDBOX_WORKDIR") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.bash_sandbox_workdir = v;
        }
    }

    // KF_CODE_BLOCK_GITIGNORED_DOTFILES
    if let Ok(val) = std::env::var("KF_CODE_BLOCK_GITIGNORED_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.block_gitignored_dotfiles = v;
        }
    }

    // KF_CODE_MAX_OVERWRITE_SIZE
    if let Ok(val) = std::env::var("KF_CODE_MAX_OVERWRITE_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.security.max_overwrite_size = n;
        }
    }

    // KF_CODE_SUMMARIZE_MODEL
    if let Ok(val) = std::env::var("KF_CODE_SUMMARIZE_MODEL") {
        if !val.is_empty() {
            cfg.model.summarize_model = val;
        }
    }

    // KF_CODE_ROUTING_ENABLED
    if let Ok(val) = std::env::var("KF_CODE_ROUTING_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.routing_enabled = v;
        }
    }

    // KF_CODE_ROUTER_MODEL
    if let Ok(val) = std::env::var("KF_CODE_ROUTER_MODEL") {
        if !val.is_empty() {
            cfg.model.router_model = val;
        }
    }

    // KF_CODE_COMMIT_MAX_FILE_SIZE
    if let Ok(val) = std::env::var("KF_CODE_COMMIT_MAX_FILE_SIZE") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.commit_max_file_size = n;
        }
    }

    // KF_CODE_PRESERVE_RECENT_MESSAGES
    if let Ok(val) = std::env::var("KF_CODE_PRESERVE_RECENT_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.session.preserve_recent_messages = n.max(1);
        }
    }

    // KF_CODE_MAX_TOOL_CALLS_PER_TURN
    if let Ok(val) = std::env::var("KF_CODE_MAX_TOOL_CALLS_PER_TURN") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_tool_calls_per_turn = n.max(1);
        }
    }

    // KF_CODE_MAX_PERSONA_TURNS
    if let Ok(val) = std::env::var("KF_CODE_MAX_PERSONA_TURNS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_persona_turns = n.max(1);
        }
    }

    // KF_CODE_TOOL_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KF_CODE_TOOL_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.tools.tool_timeout_secs = Some(n.clamp(1, 3600));
        }
    }

    // KF_CODE_AUDIT_LOG_PATH
    if let Ok(val) = std::env::var("KF_CODE_AUDIT_LOG_PATH") {
        cfg.security.audit_log_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KF_CODE_HOOKS_DIR
    if let Ok(val) = std::env::var("KF_CODE_HOOKS_DIR") {
        cfg.tools.hooks_dir = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST
    if let Ok(val) = std::env::var("KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.reject_on_excess_plugin_trust = v;
        }
    }

    // KF_CODE_PLUGIN_SIGNATURE_VALIDATION
    if let Ok(val) = std::env::var("KF_CODE_PLUGIN_SIGNATURE_VALIDATION") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.plugin_signature_validation = v;
        }
    }

    // KF_CODE_PLUGIN_PUBLIC_KEY_PATH
    if let Ok(val) = std::env::var("KF_CODE_PLUGIN_PUBLIC_KEY_PATH") {
        cfg.tools.plugin_public_key_path = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KF_CODE_PLUGIN_ALLOWED_ENV_VARS
    if let Ok(val) = std::env::var("KF_CODE_PLUGIN_ALLOWED_ENV_VARS") {
        cfg.tools.plugin_allowed_env_vars = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KF_CODE_PLUGIN_SOURCES
    if let Ok(val) = std::env::var("KF_CODE_PLUGIN_SOURCES") {
        cfg.tools.plugin_sources = parse_plugin_sources_env(&val);
    }

    // KF_CODE_ENABLED_PLUGINS
    if let Ok(val) = std::env::var("KF_CODE_ENABLED_PLUGINS") {
        cfg.tools.enabled_plugins = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KF_CODE_MEMORY_ENABLED
    if let Ok(val) = std::env::var("KF_CODE_MEMORY_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.display.memory_enabled = v;
        }
    }

    // KF_CODE_MEMORY_MAX_TOKENS
    if let Ok(val) = std::env::var("KF_CODE_MEMORY_MAX_TOKENS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.display.memory_max_tokens = n.max(1);
        }
    }

    // KF_CODE_MEMORY_TOP_N
    if let Ok(val) = std::env::var("KF_CODE_MEMORY_TOP_N") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.display.memory_top_n = n.max(1);
        }
    }

    // KF_CODE_REQUEST_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KF_CODE_REQUEST_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.model.request_timeout_secs = n.max(1);
        }
    }

    // KF_CODE_CHECKPOINT_INTERVAL_MESSAGES
    if let Ok(val) = std::env::var("KF_CODE_CHECKPOINT_INTERVAL_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.session.checkpoint_interval_messages = n;
        }
    }

    // KF_CODE_SCHEDULED_BASH_AUTO_APPROVE
    if let Ok(val) = std::env::var("KF_CODE_SCHEDULED_BASH_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.scheduled_bash_auto_approve = v;
        }
    }

    // KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS
    if let Ok(val) = std::env::var("KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_concurrent_scheduled_jobs = n.max(1);
        }
    }

    // Anthropic cloud-provider routing
    if let Ok(val) = std::env::var("KF_CODE_ANTHROPIC_PROVIDER") {
        if !val.is_empty() {
            cfg.model.anthropic_provider = val;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_AWS_REGION") {
        if !val.is_empty() {
            cfg.model.aws_region = val;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_AWS_PROFILE") {
        cfg.model.aws_profile = val;
    }
    if let Ok(val) = std::env::var("KF_CODE_GCP_PROJECT_ID") {
        if !val.is_empty() {
            cfg.model.gcp_project_id = val;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_GCP_REGION") {
        if !val.is_empty() {
            cfg.model.gcp_region = val;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_GCP_SERVICE_ACCOUNT_PATH") {
        cfg.model.gcp_service_account_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // Computer-use tool config
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.computer_use.enabled = v;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_CHROME_PATH") {
        cfg.security.computer_use.chrome_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_HEADFUL") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.computer_use.headful = v;
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_WIDTH") {
        if let Ok(n) = val.parse::<u32>() {
            cfg.security.computer_use.width = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_HEIGHT") {
        if let Ok(n) = val.parse::<u32>() {
            cfg.security.computer_use.height = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_STARTUP_TIMEOUT") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.computer_use.startup_timeout_secs = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_WAIT_TIMEOUT") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.computer_use.wait_timeout_secs = n.max(1);
        }
    }

    // Clamp after all layers so a config file or env override cannot set an
    // unusable zero-second timeout.
    cfg.model.request_timeout_secs = cfg.model.request_timeout_secs.max(1);
}
