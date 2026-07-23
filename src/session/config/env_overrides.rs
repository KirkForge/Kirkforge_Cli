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
            cfg.model.default_model = val;
        }
    }

    // KIRKFORGE_HOST
    if let Ok(val) = std::env::var("KIRKFORGE_HOST") {
        if !val.is_empty() {
            cfg.model.ollama_host = val;
        }
    }

    // KIRKFORGE_AUTO_APPROVE
    if let Ok(val) = std::env::var("KIRKFORGE_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.auto_approve = v;
        }
    }

    // KIRKFORGE_SANDBOX_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_SANDBOX_DIR") {
        cfg.security.sandbox_dir = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KIRKFORGE_BLOCK_DOTFILES
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.block_dotfiles = v;
        }
    }

    // KIRKFORGE_MAX_READ_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_READ_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.security.max_file_read_size = n;
        }
    }

    // KIRKFORGE_FOLLOW_SYMLINKS
    if let Ok(val) = std::env::var("KIRKFORGE_FOLLOW_SYMLINKS") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.follow_symlinks = v;
        }
    }

    // KIRKFORGE_BLOCK_BINARY
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_BINARY") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.block_binary_reads = v;
        }
    }

    // KIRKFORGE_MINIFY_WRITE_SIDE
    if let Ok(val) = std::env::var("KIRKFORGE_MINIFY_WRITE_SIDE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.minify_write_side = v;
        }
    }

    // KIRKFORGE_CARRYOVER_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_CARRYOVER_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.session.carryover_enabled = v;
        }
    }
    // KIRKFORGE_DRY_RUN
    if let Ok(val) = std::env::var("KIRKFORGE_DRY_RUN") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.dry_run = v;
        }
    }

    // KIRKFORGE_CACHE_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.cache_enabled = v;
        }
    }

    // KIRKFORGE_CACHE_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_DIR") {
        cfg.model.cache_dir = Some(PathBuf::from(expand_tilde_str(&val)));
    }

    // KIRKFORGE_BANG_REQUIRES_APPROVAL
    if let Ok(val) = std::env::var("KIRKFORGE_BANG_REQUIRES_APPROVAL") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.bang_requires_approval = v;
        }
    }

    // KIRKFORGE_JSON_MODE
    if let Ok(val) = std::env::var("KIRKFORGE_JSON_MODE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.json_mode = v;
        }
    }

    // KIRKFORGE_BASH_SANDBOX_WORKDIR
    if let Ok(val) = std::env::var("KIRKFORGE_BASH_SANDBOX_WORKDIR") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.bash_sandbox_workdir = v;
        }
    }

    // KIRKFORGE_BLOCK_GITIGNORED_DOTFILES
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_GITIGNORED_DOTFILES") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.block_gitignored_dotfiles = v;
        }
    }

    // KIRKFORGE_MAX_OVERWRITE_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_OVERWRITE_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.security.max_overwrite_size = n;
        }
    }

    // KIRKFORGE_SUMMARIZE_MODEL
    if let Ok(val) = std::env::var("KIRKFORGE_SUMMARIZE_MODEL") {
        if !val.is_empty() {
            cfg.model.summarize_model = val;
        }
    }

    // KIRKFORGE_ROUTING_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_ROUTING_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.model.routing_enabled = v;
        }
    }

    // KIRKFORGE_ROUTER_MODEL
    if let Ok(val) = std::env::var("KIRKFORGE_ROUTER_MODEL") {
        if !val.is_empty() {
            cfg.model.router_model = val;
        }
    }

    // KIRKFORGE_COMMIT_MAX_FILE_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_COMMIT_MAX_FILE_SIZE") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.commit_max_file_size = n;
        }
    }

    // KIRKFORGE_PRESERVE_RECENT_MESSAGES
    if let Ok(val) = std::env::var("KIRKFORGE_PRESERVE_RECENT_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.session.preserve_recent_messages = n.max(1);
        }
    }

    // KIRKFORGE_MAX_TOOL_CALLS_PER_TURN
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_TOOL_CALLS_PER_TURN") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_tool_calls_per_turn = n.max(1);
        }
    }

    // KIRKFORGE_MAX_PERSONA_TURNS
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_PERSONA_TURNS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_persona_turns = n.max(1);
        }
    }

    // KIRKFORGE_TOOL_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KIRKFORGE_TOOL_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.tools.tool_timeout_secs = Some(n.clamp(1, 3600));
        }
    }

    // KIRKFORGE_AUDIT_LOG_PATH
    if let Ok(val) = std::env::var("KIRKFORGE_AUDIT_LOG_PATH") {
        cfg.security.audit_log_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KIRKFORGE_HOOKS_DIR
    if let Ok(val) = std::env::var("KIRKFORGE_HOOKS_DIR") {
        cfg.tools.hooks_dir = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST
    if let Ok(val) = std::env::var("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.reject_on_excess_plugin_trust = v;
        }
    }

    // KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.plugin_signature_validation = v;
        }
    }

    // KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH") {
        cfg.tools.plugin_public_key_path = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS") {
        cfg.tools.plugin_allowed_env_vars = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KIRKFORGE_PLUGIN_SOURCES
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_SOURCES") {
        cfg.tools.plugin_sources = parse_plugin_sources_env(&val);
    }

    // KIRKFORGE_ENABLED_PLUGINS
    if let Ok(val) = std::env::var("KIRKFORGE_ENABLED_PLUGINS") {
        cfg.tools.enabled_plugins = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KIRKFORGE_MEMORY_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.display.memory_enabled = v;
        }
    }

    // KIRKFORGE_MEMORY_MAX_TOKENS
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_MAX_TOKENS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.display.memory_max_tokens = n.max(1);
        }
    }

    // KIRKFORGE_MEMORY_TOP_N
    if let Ok(val) = std::env::var("KIRKFORGE_MEMORY_TOP_N") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.display.memory_top_n = n.max(1);
        }
    }

    // KIRKFORGE_REQUEST_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KIRKFORGE_REQUEST_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.model.request_timeout_secs = n.max(1);
        }
    }

    // KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES
    if let Ok(val) = std::env::var("KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.session.checkpoint_interval_messages = n;
        }
    }

    // KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE
    if let Ok(val) = std::env::var("KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.tools.scheduled_bash_auto_approve = v;
        }
    }

    // KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_concurrent_scheduled_jobs = n.max(1);
        }
    }

    // Anthropic cloud-provider routing
    if let Ok(val) = std::env::var("KIRKFORGE_ANTHROPIC_PROVIDER") {
        if !val.is_empty() {
            cfg.model.anthropic_provider = val;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_AWS_REGION") {
        if !val.is_empty() {
            cfg.model.aws_region = val;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_AWS_PROFILE") {
        cfg.model.aws_profile = val;
    }
    if let Ok(val) = std::env::var("KIRKFORGE_GCP_PROJECT_ID") {
        if !val.is_empty() {
            cfg.model.gcp_project_id = val;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_GCP_REGION") {
        if !val.is_empty() {
            cfg.model.gcp_region = val;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_GCP_SERVICE_ACCOUNT_PATH") {
        cfg.model.gcp_service_account_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }

    // Computer-use tool config
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_ENABLED") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.computer_use.enabled = v;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_CHROME_PATH") {
        cfg.security.computer_use.chrome_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_HEADFUL") {
        if let Some(v) = parse_bool_env(&val) {
            cfg.security.computer_use.headful = v;
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_WIDTH") {
        if let Ok(n) = val.parse::<u32>() {
            cfg.security.computer_use.width = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_HEIGHT") {
        if let Ok(n) = val.parse::<u32>() {
            cfg.security.computer_use.height = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_STARTUP_TIMEOUT") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.computer_use.startup_timeout_secs = n.max(1);
        }
    }
    if let Ok(val) = std::env::var("KIRKFORGE_COMPUTER_USE_WAIT_TIMEOUT") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.security.computer_use.wait_timeout_secs = n.max(1);
        }
    }

    // Clamp after all layers so a config file or env override cannot set an
    // unusable zero-second timeout.
    cfg.model.request_timeout_secs = cfg.model.request_timeout_secs.max(1);
}
