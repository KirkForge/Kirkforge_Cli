//! Environment-variable overrides for layered config resolution.
//!
//! Extracted from `mod.rs`: reads `KF_CODE_*` env vars and applies
//! them to a `Config` (priority layer 2, above the config file).

use crate::shared::Config;
use std::path::PathBuf;

use super::{expand_tilde_str, parse_bool_env, parse_plugin_sources_env};

/// Apply environment variable overrides to a Config.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn apply_env_overrides(cfg: &mut Config) {
    // Helper for the repeated bool-override pattern: read a KF_CODE_*
    // env var, parse it as a bool, and write it to a config field.
    macro_rules! env_bool {
        ($var:literal, $field:expr) => {
            if let Ok(val) = std::env::var($var) {
                if let Some(v) = parse_bool_env(&val) {
                    $field = v;
                }
            }
        };
    }
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
    env_bool!("KF_CODE_AUTO_APPROVE", cfg.security.auto_approve);

    // KF_CODE_SANDBOX_DIR
    if let Ok(val) = std::env::var("KF_CODE_SANDBOX_DIR") {
        cfg.security.sandbox_dir = if val.is_empty() {
            None
        } else {
            Some(expand_tilde_str(&val))
        };
    }

    // KF_CODE_BLOCK_DOTFILES
    env_bool!("KF_CODE_BLOCK_DOTFILES", cfg.security.block_dotfiles);

    // KF_CODE_MAX_READ_SIZE
    if let Ok(val) = std::env::var("KF_CODE_MAX_READ_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.security.max_file_read_size = n;
        }
    }

    // KF_CODE_FOLLOW_SYMLINKS
    env_bool!("KF_CODE_FOLLOW_SYMLINKS", cfg.tools.follow_symlinks);

    // KF_CODE_BLOCK_BINARY
    env_bool!("KF_CODE_BLOCK_BINARY", cfg.tools.block_binary_reads);

    // KF_CODE_MINIFY_WRITE_SIDE
    env_bool!("KF_CODE_MINIFY_WRITE_SIDE", cfg.tools.minify_write_side);

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
    env_bool!("KF_CODE_CARRYOVER_ENABLED", cfg.session.carryover_enabled);
    // KF_CODE_COMPACTION_USE_HEURISTIC (backward compat: KF_CODE_COMPACTION_USE_LLM)
    if let Ok(val) = std::env::var("KF_CODE_COMPACTION_USE_HEURISTIC") {
        if let Ok(v) = val.parse::<bool>() {
            cfg.session.compaction_use_heuristic = v;
        }
    } else if let Ok(val) = std::env::var("KF_CODE_COMPACTION_USE_LLM") {
        if let Ok(v) = val.parse::<bool>() {
            cfg.session.compaction_use_heuristic = v;
        }
    }
    // KF_CODE_COMPACTION_DROP_THRESHOLD
    if let Ok(val) = std::env::var("KF_CODE_COMPACTION_DROP_THRESHOLD") {
        if let Ok(v) = val.parse::<f64>() {
            cfg.session.compaction_drop_threshold = v;
        }
    }
    // KF_CODE_STEM_FILE_CAP
    if let Ok(val) = std::env::var("KF_CODE_STEM_FILE_CAP") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.session.stem_file_cap = Some(n);
        }
    }
    // KF_CODE_SHUTDOWN_TIMEOUT_SECS
    if let Ok(val) = std::env::var("KF_CODE_SHUTDOWN_TIMEOUT_SECS") {
        if let Ok(n) = val.parse::<u64>() {
            cfg.session.shutdown_timeout_secs = Some(n);
        }
    }
    // KF_CODE_DRY_RUN
    env_bool!("KF_CODE_DRY_RUN", cfg.tools.dry_run);

    // KF_CODE_CACHE_ENABLED
    env_bool!("KF_CODE_CACHE_ENABLED", cfg.model.cache_enabled);

    // KF_CODE_CACHE_DIR
    if let Ok(val) = std::env::var("KF_CODE_CACHE_DIR") {
        cfg.model.cache_dir = Some(PathBuf::from(expand_tilde_str(&val)));
    }

    // KF_CODE_BANG_REQUIRES_APPROVAL
    env_bool!(
        "KF_CODE_BANG_REQUIRES_APPROVAL",
        cfg.security.bang_requires_approval
    );

    // KF_CODE_JSON_MODE
    env_bool!("KF_CODE_JSON_MODE", cfg.model.json_mode);

    // KF_CODE_MAX_TOKENS
    if let Ok(n) = std::env::var("KF_CODE_MAX_TOKENS") {
        if let Ok(v) = n.parse::<u32>() {
            cfg.model.max_tokens = v.max(1);
        }
    }

    // KF_CODE_EXTENDED_THINKING
    env_bool!("KF_CODE_EXTENDED_THINKING", cfg.model.extended_thinking);

    // KF_CODE_BUDGET_TOKENS
    if let Ok(val) = std::env::var("KF_CODE_BUDGET_TOKENS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.model.budget_tokens = n.max(1);
        }
    }

    // KF_CODE_BASH_SANDBOX_WORKDIR
    env_bool!(
        "KF_CODE_BASH_SANDBOX_WORKDIR",
        cfg.security.bash_sandbox_workdir
    );

    // KF_CODE_LANDLOCK_EXTRA_PATHS — colon-separated extra landlock allow-list
    // paths (WO 27.1). Mirrors PATH-style splitting.
    if let Ok(val) = std::env::var("KF_CODE_LANDLOCK_EXTRA_PATHS") {
        cfg.security.landlock_extra_paths = val
            .split(':')
            .map(|s| expand_tilde_str(s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KF_CODE_BLOCK_GITIGNORED_DOTFILES
    env_bool!(
        "KF_CODE_BLOCK_GITIGNORED_DOTFILES",
        cfg.security.block_gitignored_dotfiles
    );

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
    env_bool!("KF_CODE_ROUTING_ENABLED", cfg.model.routing_enabled);

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

    // KF_CODE_MAX_CONTINUATION_ROUNDS
    if let Ok(val) = std::env::var("KF_CODE_MAX_CONTINUATION_ROUNDS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_continuation_rounds = n.clamp(0, 50);
        }
    }

    // KF_CODE_DOOM_LOOP_MAX_HITS
    if let Ok(val) = std::env::var("KF_CODE_DOOM_LOOP_MAX_HITS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.doom_loop_max_hits = n;
        }
    }

    // KF_CODE_DOOM_LOOP_ACTION
    if let Ok(val) = std::env::var("KF_CODE_DOOM_LOOP_ACTION") {
        if !val.is_empty() {
            cfg.tools.doom_loop_action = val;
        }
    }

    // KF_CODE_MAX_BACKGROUND_TASKS
    if let Ok(val) = std::env::var("KF_CODE_MAX_BACKGROUND_TASKS") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.tools.max_background_tasks = n.clamp(1, 64);
        }
    }

    // KF_CODE_TASK_CONCURRENCY_MODE
    if let Ok(val) = std::env::var("KF_CODE_TASK_CONCURRENCY_MODE") {
        let mode = val.to_lowercase();
        if mode == "queue" || mode == "reject" {
            cfg.tools.task_concurrency_mode = mode;
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
    env_bool!(
        "KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST",
        cfg.tools.reject_on_excess_plugin_trust
    );

    // KF_CODE_PLUGIN_SIGNATURE_VALIDATION
    env_bool!(
        "KF_CODE_PLUGIN_SIGNATURE_VALIDATION",
        cfg.tools.plugin_signature_validation
    );

    // KF_CODE_PLUGIN_TRUST_WORKSPACE
    env_bool!(
        "KF_CODE_PLUGIN_TRUST_WORKSPACE",
        cfg.tools.plugin_trust_workspace
    );

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

    // KF_CODE_DISABLED_PLUGINS
    if let Ok(val) = std::env::var("KF_CODE_DISABLED_PLUGINS") {
        cfg.tools.disabled_plugins = val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // KF_CODE_MEMORY_ENABLED
    env_bool!("KF_CODE_MEMORY_ENABLED", cfg.display.memory_enabled);

    // KF_CODE_MEMORY_AUTO_POPULATE
    env_bool!(
        "KF_CODE_MEMORY_AUTO_POPULATE",
        cfg.tools.memory_auto_populate
    );

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

    // KF_CODE_MEMORY_AUTO_POPULATE
    env_bool!(
        "KF_CODE_MEMORY_AUTO_POPULATE",
        cfg.display.memory_auto_populate
    );

    // KF_CODE_MEMORY_SHOW_IN_STATUS
    env_bool!(
        "KF_CODE_MEMORY_SHOW_IN_STATUS",
        cfg.display.memory_show_in_status
    );

    // KF_CODE_THEME
    if let Ok(val) = std::env::var("KF_CODE_THEME") {
        if !val.is_empty() {
            cfg.display.theme = val;
        }
    }

    // KF_CODE_MOUSE_ENABLED
    env_bool!("KF_CODE_MOUSE_ENABLED", cfg.display.mouse_enabled);

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
    env_bool!(
        "KF_CODE_SCHEDULED_BASH_AUTO_APPROVE",
        cfg.tools.scheduled_bash_auto_approve
    );

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

    // Per-provider API keys (env layer — these supplement the config
    // fields and are resolved by `adapters::auth::resolve_api_key`).
    if let Ok(val) = std::env::var("ANTHROPIC_API_KEY") {
        if !val.is_empty() {
            cfg.model.anthropic_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("OPENAI_API_KEY") {
        if !val.is_empty() {
            cfg.model.openai_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("DEEPSEEK_API_KEY") {
        if !val.is_empty() {
            cfg.model.deepseek_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("GEMINI_API_KEY") {
        if !val.is_empty() {
            cfg.model.gemini_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KIMI_API_KEY") {
        if !val.is_empty() {
            cfg.model.kimi_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_AWS_REGION") {
        if !val.is_empty() {
            cfg.model.aws_region = val;
        }
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
    env_bool!(
        "KF_CODE_COMPUTER_USE_ENABLED",
        cfg.security.computer_use.enabled
    );
    if let Ok(val) = std::env::var("KF_CODE_COMPUTER_USE_CHROME_PATH") {
        cfg.security.computer_use.chrome_path = if val.is_empty() {
            None
        } else {
            Some(PathBuf::from(expand_tilde_str(&val)))
        };
    }
    env_bool!(
        "KF_CODE_COMPUTER_USE_HEADFUL",
        cfg.security.computer_use.headful
    );
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

    // KF_CODE_ADAPTER_ROUTING
    // Format: comma-separated prefix=Kind pairs, e.g.
    //   "grok-=OpenAiCompat,my-llm=Ollama"
    // Pairs without '=' are ignored. Overrides the [adapter_routing] TOML
    // section entirely when set.
    if let Ok(val) = std::env::var("KF_CODE_ADAPTER_ROUTING") {
        if !val.is_empty() {
            let mut map = std::collections::HashMap::new();
            for entry in val.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let Some((prefix, kind)) = entry.split_once('=') else {
                    continue;
                };
                let prefix = prefix.trim().to_string();
                let kind = kind.trim().to_string();
                if !prefix.is_empty() && !kind.is_empty() {
                    map.insert(prefix, kind);
                }
            }
            cfg.model.adapter_routing = map;
        }
    }

    // Subagent provider override (WO 30.0.6 brain+brawn). Each var maps
    // to the matching [subagent_provider] TOML field; an empty value is
    // ignored so the subagent keeps inheriting the parent's value.
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_MODEL") {
        if !val.is_empty() {
            cfg.model.subagent_provider.model = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_HOST") {
        if !val.is_empty() {
            cfg.model.subagent_provider.ollama_host = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_ANTHROPIC_API_KEY") {
        if !val.is_empty() {
            cfg.model.subagent_provider.anthropic_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_OPENAI_API_KEY") {
        if !val.is_empty() {
            cfg.model.subagent_provider.openai_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_DEEPSEEK_API_KEY") {
        if !val.is_empty() {
            cfg.model.subagent_provider.deepseek_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_GEMINI_API_KEY") {
        if !val.is_empty() {
            cfg.model.subagent_provider.gemini_api_key = Some(val);
        }
    }
    if let Ok(val) = std::env::var("KF_CODE_SUBAGENT_KIMI_API_KEY") {
        if !val.is_empty() {
            cfg.model.subagent_provider.kimi_api_key = Some(val);
        }
    }
}
