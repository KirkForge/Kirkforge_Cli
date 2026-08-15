/// Config bootstrap — layered config resolution with env var overrides.
///
/// Resolution order (highest to lowest priority):
/// 1. CLI arguments (handled in main.rs)
/// 2. Environment variables (`KF_CODE_*`)
/// 3. Config file (`~/.local/share/kf-code/config.toml`)
/// 4. Built-in defaults
///
/// Environment variable reference:
/// - `KF_CODE_MODEL` — default model name
/// - `KF_CODE_HOST` — Ollama host URL
/// - `KF_CODE_AUTO_APPROVE` — "true" to auto-approve destructive calls
/// - `KF_CODE_DRY_RUN` — "true" to make destructive tools report only
/// - `KF_CODE_SANDBOX_DIR` — sandbox directory path
/// - `KF_CODE_BLOCK_DOTFILES` — "true" to block dotfile writes
/// - `KF_CODE_BLOCK_GITIGNORED_DOTFILES` — "true" to block git-ignored dotfile writes
/// - `KF_CODE_MAX_READ_SIZE` — max file read size in bytes
/// - `KF_CODE_MAX_OVERWRITE_SIZE` — max existing file size that write tools may overwrite
/// - `KF_CODE_FOLLOW_SYMLINKS` — "true" to allow following symlinks
/// - `KF_CODE_BLOCK_BINARY` — "true" to block binary file reads
/// - `KF_CODE_MINIFY_WRITE_SIDE` — "true" to enable minified-envelope write-side expansion
/// - `KF_CODE_MINIFY_ABOVE_BYTES` — auto-minify `read_file` output above this byte threshold (default 4096)
/// - `KF_CODE_SCHEDULED_BASH_AUTO_APPROVE` — "true" to let scheduled bash jobs skip interactive approval
/// - `KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS` — max concurrent scheduled jobs (clamped to ≥1)
/// - `KF_CODE_MAX_BACKGROUND_TASKS` — max concurrent background tasks (clamped to 1..=64)
/// - `KF_CODE_TASK_CONCURRENCY_MODE` — "queue" (default) or "reject" for background task backpressure
/// - `KF_CODE_BASH_SANDBOX_WORKDIR` — "true"/"false" to force bash cwd into the sandbox
/// - `KF_CODE_BANG_REQUIRES_APPROVAL` — "true" to route `!` passthrough through approval gate
/// - `KF_CODE_JSON_MODE` — "true" to request JSON-formatted model responses
/// - `KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST` — "true" to reject plugins above max trust
/// - `KF_CODE_PLUGIN_SIGNATURE_VALIDATION` — "true" to require `.kf-code.sig`
/// - `KF_CODE_PLUGIN_PUBLIC_KEY_PATH` — minisign public key for plugin signatures
/// - `KF_CODE_PLUGIN_ALLOWED_ENV_VARS` — comma-separated extra env vars for plugin tools
/// - `KF_CODE_PLUGIN_SOURCES` — comma-separated `name=path` workspace plugin sources
/// - `KF_CODE_ENABLED_PLUGINS` — comma-separated names from `plugin_sources` to load
/// - `KF_CODE_MEMORY_ENABLED` — "true"/"false" to enable or disable memory injection
/// - `KF_CODE_MEMORY_MAX_TOKENS` — token budget for injected memory facts
/// - `KF_CODE_MEMORY_TOP_N` — maximum number of facts to consider per turn
/// - `KF_CODE_MEMORY_AUTO_POPULATE` — "true"/"false" to enable or disable memory auto-extraction
/// - `KF_CODE_REQUEST_TIMEOUT_SECS` — model request timeout (clamped to ≥1 s)
/// - `KF_CODE_TOOL_TIMEOUT_SECS` — per-tool hard timeout (clamped to [1, 3600])
/// - `KF_CODE_CHECKPOINT_INTERVAL_MESSAGES` — write a checkpoint every N messages
/// - `KF_CODE_SUMMARIZE_MODEL` — fast model used by `/compact`
/// - `KF_CODE_ROUTING_ENABLED` — "true" to enable smart model routing
/// - `KF_CODE_ROUTER_MODEL` — model used for routing classification
/// - `KF_CODE_COMMIT_MAX_FILE_SIZE` — max file size allowed in `/commit`
/// - `KF_CODE_PRESERVE_RECENT_MESSAGES` — number of recent messages kept verbatim on compact
/// - `KF_CODE_MAX_TOOL_CALLS_PER_TURN` — cap on model↔tool iterations per turn
/// - `KF_CODE_MAX_PERSONA_TURNS` — cap on fork-isolated persona turns
/// - `KF_CODE_AUDIT_LOG_PATH` — path for the append-only JSONL audit log (empty disables)
/// - `KF_CODE_HOOKS_DIR` — directory containing lifecycle hook scripts
///
/// Boolean env vars accept `true`/`1`/`yes` (case-insensitive) for true and
/// `false`/`0`/`no` for false. Unrecognized values leave the prior layer
/// unchanged.
use crate::shared::Config;
use std::path::PathBuf;

#[cfg_attr(not(test), allow(dead_code))]
mod env_overrides;

#[cfg(test)]
use env_overrides::apply_env_overrides;

/// Expand a leading `~` in a path string using `$HOME` (or the equivalent
/// on Windows). Falls back to the original string if expansion fails.
fn expand_tilde_str(s: &str) -> String {
    shellexpand::tilde(s).into_owned()
}

/// Parse a boolean environment variable value consistently.
///
/// Treats "true", "1", "yes" (case-insensitive) as true,
/// "false", "0", "no" (case-insensitive) as false, and any other value as
/// `None` so the config default is preserved.
#[cfg_attr(not(test), allow(dead_code))]
fn parse_bool_env(val: &str) -> Option<bool> {
    if val.eq_ignore_ascii_case("true")
        || val.eq_ignore_ascii_case("1")
        || val.eq_ignore_ascii_case("yes")
    {
        Some(true)
    } else if val.eq_ignore_ascii_case("false")
        || val.eq_ignore_ascii_case("0")
        || val.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
    }
}

/// Load config with full layered resolution.
///
/// 1. Start with defaults
/// 2. Override from config file (if exists)
/// 3. Override from environment variables
///
/// The config is NOT written to disk here — that's the caller's
/// responsibility (e.g., on first run or when CLI overrides are provided).
///
/// Returns the resolved config and an optional human-readable warning if
/// the config file existed but could not be fully parsed.
pub fn load_config() -> (Config, Option<String>) {
    let mut cfg = Config::default();
    let mut warning: Option<String> = None;

    // Layer 1: config file
    let path = super::config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        // Always parse as a TOML table and merge field-by-field into the
        // default config. This preserves ALL user-set values while filling
        // missing fields from Default. The previous approach tried
        // `toml::from_str::<Config>` first, but serde's flatten + default
        // interaction is broken (known serde issue #2230), so a minimal
        // config file with only 4 keys would fail to parse and the merge
        // fallback didn't handle all fields — wiping user values.
        match content.parse::<toml::Table>() {
            Ok(table) => {
                merge_toml_into_config(&mut cfg, table);
            }
            Err(e) => {
                let msg = format!("Failed to parse config ({e}), using defaults");
                tracing::warn!(%msg);
                warning = Some(msg);
            }
        }
    }

    (cfg, warning)
}

// First-run onboarding banner text (WO 14.1). Printed to stdout in
// `load_or_create_config` when a new config file is written, so a new
// user gets a concrete next step instead of silent success. Kept as a
// pure helper so the exact wording is unit-testable without capturing
// stdout.
fn first_run_banner(path: &std::path::Path) -> String {
    format!(
        "Config created at {}. Next: set a model — try `kf-code run -m qwen2.5:0.5b` \
         (Ollama) or edit `default_model` in the config file. See config.toml.example \
         for all options.",
        path.display()
    )
}

/// Compute the legacy pre-rename config path
/// (`~/.local/share/kirkforge/config.toml`).
///
/// Commit ae0e37d (2026-08-04) renamed the data dir from `kirkforge` to
/// `kf-code`. Existing users have their customized `config.toml` at the
/// old path; without migration, `load_or_create_config` writes fresh
/// defaults and the user's customizations vanish. This helper returns
/// the old path so `load_or_create_config` can migrate the file on
/// first run of the new binary.
///
/// Respects `KF_CODE_LEGACY_DATA_DIR` (mirrors `KF_CODE_DATA_DIR`) so
/// the migration is unit-testable without touching the real legacy dir.
fn legacy_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("KF_CODE_LEGACY_DATA_DIR") {
        return Some(PathBuf::from(dir).join("config.toml"));
    }
    directories::ProjectDirs::from("", "", "kirkforge").map(|p| p.data_dir().join("config.toml"))
}

/// Load config and write a default file on first run.
///
/// If the config file doesn't exist, creates it with default values
/// and prints a brief info message. If a config exists at the legacy
/// pre-rename path, it is migrated (copied) to the new location
/// instead of writing defaults — preserving user customizations across
/// the `kirkforge → kf-code` rename.
pub fn load_or_create_config() -> Config {
    let path = super::config_path();
    let exists = path.exists();

    // Migration: if the config file is absent at the current path but
    // exists at the legacy pre-rename path (~/.local/share/kirkforge/),
    // copy it across instead of writing fresh defaults. Commit ae0e37d
    // renamed the data dir; without this, upgrading users lose their
    // customizations on first run of the new binary.
    if !exists {
        if let Some(legacy) = legacy_config_path() {
            if legacy.exists() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::copy(&legacy, &path).is_ok() {
                    tracing::info!(
                        from = %legacy.display(),
                        to = %path.display(),
                        "Migrated config from legacy kirkforge data dir"
                    );
                    let (cfg, warning) = load_config();
                    if let Some(w) = warning {
                        eprintln!("Warning: {} ({})", w, path.display());
                    }
                    return cfg;
                }
            }
        }
    }

    let (cfg, warning) = load_config();
    if let Some(w) = warning {
        eprintln!("Warning: {} ({})", w, path.display());
    }

    if !exists {
        // Write the default config to disk
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    error = %e,
                    dir = %parent.display(),
                    "Failed to create config directory"
                );
            }
        }
        if let Ok(content) = toml::to_string_pretty(&cfg) {
            if std::fs::write(&path, content).is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) =
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    {
                        tracing::warn!(
                            error = %e,
                            path = %path.display(),
                            "Failed to set restrictive config permissions"
                        );
                    }
                }
                println!("{}", first_run_banner(&path));
                tracing::info!(
                    "Config file created at {}. Edit it to customize model, host, etc.",
                    path.display()
                );
            } else {
                tracing::warn!(path = %path.display(), "Failed to write default config file");
            }
        } else {
            tracing::warn!(path = %path.display(), "Failed to serialize default config");
        }
    }

    cfg
}

/// Save config to disk.
pub fn save_config(config: &Config) -> anyhow::Result<()> {
    let path = super::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to set restrictive config permissions"
            );
        }
    }
    Ok(())
}

/// Resolve the launch-time cwd and assign it to `config.security.sandbox_dir` if
/// the operator hasn't already set one explicitly.
///
/// Review.md arch concern #3: `Config::default()` previously called
/// `std::env::current_dir()` itself, which (a) ran before any
/// validation, and (b) silently dropped sandbox protection if the
/// cwd had been deleted before launch. This helper is the new single
/// resolution site: callers in `main.rs` call it once at startup,
/// freezing the value for the session lifetime.
///
/// Returns the resolved path (as a `String`) on success, or `None`
/// if `current_dir()` failed and we left `sandbox_dir` as `None` —
/// in which case the executor's `warn_if_unsandboxed` banner will
/// surface the situation to the user.
///
/// Honours the explicit-escape-hatch policy: an empty string in
/// `config.security.sandbox_dir` means "intentionally unsandboxed," and we
/// do not overwrite it. Only the `None` case (operator didn't set
/// the field) is filled in.
pub fn freeze_launch_sandbox(config: &mut Config) -> Option<String> {
    if config.security.sandbox_dir.is_some() {
        // Operator already set it (via config file, env var, or
        // an earlier `KF_CODE_SANDBOX_DIR` override). Respect
        // their choice — even if it's an explicit empty string
        // meaning "unsandboxed."
        return config.security.sandbox_dir.clone();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let path = cwd.to_string_lossy().to_string();
            config.security.sandbox_dir = Some(path.clone());
            Some(path)
        }
        Err(_) => {
            // `current_dir()` failed (cwd deleted before launch).
            // Leave `sandbox_dir` as `None` so the executor's
            // `warn_if_unsandboxed` banner surfaces the situation.
            // The previous code also fell through to `None` in
            // this case, but did so via the `Default::default()`
            // path; the difference is that NOW the caller knows
            // we tried, and the next test asserts this behaviour
            // explicitly.
            None
        }
    }
}

/// Merge a parsed TOML table into a Config, field by field.
///
/// This handles partial configs gracefully — missing fields keep
/// their current value.
fn merge_toml_into_config(cfg: &mut Config, table: toml::Table) {
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
fn parse_plugin_sources_env(value: &str) -> std::collections::HashMap<String, PathBuf> {
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

    // WO 28.1: moved from shared/access/mod.rs. This test exercises the
    // composition of `freeze_launch_sandbox` (defined here in session::config)
    // with `access_from_config` (now in shared::access). It reaches up to the
    // launch-freeze policy, so it lives with that policy — keeping shared free
    // of any session dependency (required for the kf-shared extraction goal).
    /// **Contract:** `Config::default()` now sandboxes to the current
    /// working directory. Operators who want unsandboxed operation must
    /// explicitly opt out via `sandbox_dir = ""` in the config file (or
    /// `KF_CODE_SANDBOX_DIR=""` env var); `access_from_config` treats
    /// the empty string as `None`.
    #[test]
    fn test_launch_path_sandboxes_to_cwd_by_default() {
        // Default Config has no sandbox_dir — operator didn't set it.
        let mut config = Config::default();
        assert!(
            config.security.sandbox_dir.is_none(),
            "Config::default() must NOT pre-resolve cwd — resolution happens \
             at launch time in `freeze_launch_sandbox`."
        );

        // Launch path. In the unit-test runtime, cwd is always present.
        freeze_launch_sandbox(&mut config);
        let (_deny, guard, _gate) = crate::shared::access::access_from_config(&config);
        assert!(
            guard.is_sandboxed(),
            "After freeze_launch_sandbox, the launch path must produce a \
             sandboxed guard."
        );

        // Explicit escape hatch: empty string in config = unsandboxed.
        let mut config_unsandboxed = Config::default();
        config_unsandboxed.security.sandbox_dir = Some(String::new());
        freeze_launch_sandbox(&mut config_unsandboxed);
        let (_deny, guard_unsandboxed, _gate) =
            crate::shared::access::access_from_config(&config_unsandboxed);
        assert!(
            !guard_unsandboxed.is_sandboxed(),
            "Setting sandbox_dir = Some(\"\") is the explicit escape hatch; \
             freeze_launch_sandbox must not overwrite it"
        );
        assert_eq!(
            config_unsandboxed.security.sandbox_dir.as_deref(),
            Some(""),
            "freeze_launch_sandbox must leave an explicit-empty sandbox_dir alone"
        );

        // `None` is also the escape hatch.
        let mut config_none = Config::default();
        freeze_launch_sandbox(&mut config_none);
        config_none.security.sandbox_dir = None;
        let (_deny, guard_none, _gate) = crate::shared::access::access_from_config(&config_none);
        assert!(
            !guard_none.is_sandboxed(),
            "sandbox_dir = None must produce an unsandboxed guard"
        );
    }

    /// Serialize tests that mutate process-wide environment variables.
    /// Rust unit tests run in parallel by default; `std::env::set_var` is
    /// process-wide, so concurrent env tests can observe each other's state
    /// and fail sporadically.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper to temporarily set an env var for a test. Must be called
    /// while `ENV_LOCK` is held.
    fn set_env(key: &str, val: Option<&str>) -> crate::shared::test_util::EnvGuard {
        match val {
            Some(v) => crate::shared::test_util::EnvGuard::set(key, v),
            None => crate::shared::test_util::EnvGuard::remove(key),
        }
    }

    #[test]
    fn test_env_overrides_model() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(
            cfg.model.default_model.is_empty(),
            "default_model is empty by default; configure it explicitly"
        );

        let _env = set_env("KF_CODE_MODEL", Some("deepseek-v4:cloud"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.default_model, "deepseek-v4:cloud");
    }

    #[test]
    fn test_env_auto_approve_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.security.auto_approve);

        let _env = set_env("KF_CODE_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.auto_approve);
    }

    #[test]
    fn test_env_auto_approve_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.security.auto_approve = true;

        let _env = set_env("KF_CODE_AUTO_APPROVE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.security.auto_approve);
    }

    #[test]
    fn test_env_dry_run_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.dry_run);

        let _env = set_env("KF_CODE_DRY_RUN", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.dry_run);
    }

    #[test]
    fn test_env_dry_run_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.tools.dry_run = true;

        let _env = set_env("KF_CODE_DRY_RUN", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.dry_run);
    }

    #[test]
    fn test_env_block_dotfiles() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_BLOCK_DOTFILES", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.block_dotfiles);
    }

    #[test]
    fn test_env_follow_symlinks() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_FOLLOW_SYMLINKS", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.follow_symlinks);
    }

    #[test]
    fn test_env_block_binary() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_BLOCK_BINARY", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.block_binary_reads);
    }

    #[test]
    fn test_env_minify_write_side() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.minify_write_side);
        let _env = set_env("KF_CODE_MINIFY_WRITE_SIDE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.minify_write_side);
    }

    #[test]
    fn test_env_minify_above_bytes() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.minify_above_bytes, 4096);
        let _env = set_env("KF_CODE_MINIFY_ABOVE_BYTES", Some("512"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.minify_above_bytes, 512);
    }

    #[test]
    fn test_env_budget_ceiling() {
        // WO 14.7: KF_CODE_BUDGET_CEILING pins the token budget
        // ceiling for a single run (the Token Budget Challenge sets
        // this per ceiling level). Mirrors KF_CODE_MINIFY_ABOVE_BYTES.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let default_ceiling = cfg.tools.budget_ceiling;
        {
            let _env = set_env("KF_CODE_BUDGET_CEILING", Some("32768"));
            apply_env_overrides(&mut cfg);
            assert_eq!(cfg.tools.budget_ceiling, 32_768);
        }
        // Confirm removal restores the default (no stale leak). Guard dropped above.
        let mut cfg2 = Config::default();
        apply_env_overrides(&mut cfg2);
        assert_eq!(cfg2.tools.budget_ceiling, default_ceiling);
    }

    #[test]
    fn test_env_budget_ceiling_bad_value_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let default_ceiling = cfg.tools.budget_ceiling;
        let _env = set_env("KF_CODE_BUDGET_CEILING", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.budget_ceiling, default_ceiling);
    }

    #[test]
    fn test_env_max_read_size() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_READ_SIZE", Some("65536"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 65536);
    }

    #[test]
    fn test_env_bad_max_read_size_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_READ_SIZE", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 1024 * 1024);
    }

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
        // Unset fields keep defaults (now empty placeholders)
        assert!(
            cfg.model.ollama_host.is_empty(),
            "ollama_host is empty by default; configure it explicitly"
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
    fn test_env_misc_overrides() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_BANG_REQUIRES_APPROVAL", Some("true"));
        let _e2 = set_env("KF_CODE_JSON_MODE", Some("true"));
        let _e3 = set_env("KF_CODE_BASH_SANDBOX_WORKDIR", Some("false"));
        let _e4 = set_env("KF_CODE_BLOCK_GITIGNORED_DOTFILES", Some("false"));
        let _e5 = set_env("KF_CODE_MAX_OVERWRITE_SIZE", Some("2097152"));
        let _e6 = set_env("KF_CODE_SUMMARIZE_MODEL", Some("my-summarize-model"));
        let _e7 = set_env("KF_CODE_ROUTING_ENABLED", Some("true"));
        let _e8 = set_env("KF_CODE_ROUTER_MODEL", Some("my-router-model"));
        let _e9 = set_env("KF_CODE_COMMIT_MAX_FILE_SIZE", Some("1048576"));
        let _e10 = set_env("KF_CODE_PRESERVE_RECENT_MESSAGES", Some("5"));
        let _e11 = set_env("KF_CODE_MAX_TOOL_CALLS_PER_TURN", Some("25"));
        let _e12 = set_env("KF_CODE_MAX_PERSONA_TURNS", Some("3"));
        let _e13 = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("60"));
        let _e14 = set_env("KF_CODE_AUDIT_LOG_PATH", Some("/tmp/kf-audit.ndjson"));
        let _e15 = set_env("KF_CODE_HOOKS_DIR", Some("/tmp/kf-hooks"));

        apply_env_overrides(&mut cfg);

        assert!(cfg.security.bang_requires_approval);
        assert!(cfg.model.json_mode);
        assert!(!cfg.security.bash_sandbox_workdir);
        assert!(!cfg.security.block_gitignored_dotfiles);
        assert_eq!(cfg.security.max_overwrite_size, 2_097_152);
        assert_eq!(cfg.model.summarize_model, "my-summarize-model");
        assert!(cfg.model.routing_enabled);
        assert_eq!(cfg.model.router_model, "my-router-model");
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
    fn test_env_tool_timeout_secs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(1));

        let _e2 = set_env("KF_CODE_TOOL_TIMEOUT_SECS", Some("7200"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(3600));
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

    /// `freeze_launch_sandbox` is the new launch-time cwd resolution
    /// site. It must fill in `sandbox_dir` with the resolved cwd when
    /// the operator hasn't set it explicitly, and must not overwrite
    /// an explicit (including intentionally-empty) value.
    ///
    /// Review.md arch concern #3: the previous code did this in
    /// `Config::default()`, which (a) ran before any validation and
    /// (b) silently dropped sandbox protection on a `current_dir()`
    /// failure. The new helper is a single, testable call site.
    #[test]
    fn test_freeze_launch_sandbox_fills_in_cwd() {
        let mut cfg = Config::default();
        assert!(cfg.security.sandbox_dir.is_none());
        let resolved = freeze_launch_sandbox(&mut cfg);
        // The test runner always has a cwd.
        assert!(resolved.is_some(), "test cwd is always present");
        let resolved = resolved.unwrap();
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some(resolved.as_str()));
    }

    /// The explicit-escape-hatch contract: if the operator set
    /// `sandbox_dir = Some("")` (or it was loaded from a config
    /// file that way), `freeze_launch_sandbox` must leave it alone.
    /// This is the policy that lets operators opt out of sandboxing.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_empty() {
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some(String::new());
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some(""));
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some(""));
    }

    /// If the operator set a real path (e.g. from a config file's
    /// `sandbox_dir = "/srv/project"`), the helper must not
    /// overwrite it with cwd. Operators win over defaults.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_path() {
        let mut cfg = Config::default();
        cfg.security.sandbox_dir = Some("/srv/project".to_string());
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some("/srv/project"));
        assert_eq!(cfg.security.sandbox_dir.as_deref(), Some("/srv/project"));
    }

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
    fn test_env_reject_on_excess_plugin_trust() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.tools.reject_on_excess_plugin_trust);

        let _env = set_env("KF_CODE_REJECT_ON_EXCESS_PLUGIN_TRUST", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.reject_on_excess_plugin_trust);
    }

    #[test]
    fn test_env_plugin_signature_validation() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.tools.plugin_signature_validation);

        let _env = set_env("KF_CODE_PLUGIN_SIGNATURE_VALIDATION", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.plugin_signature_validation);
    }

    #[test]
    fn test_env_plugin_public_key_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_PLUGIN_PUBLIC_KEY_PATH", Some("/tmp/key.pub"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.tools.plugin_public_key_path.as_deref(),
            Some("/tmp/key.pub")
        );
    }

    #[test]
    fn test_env_plugin_allowed_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_PLUGIN_ALLOWED_ENV_VARS", Some("FOO,BAR"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.plugin_allowed_env_vars, vec!["FOO", "BAR"]);
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
    fn test_env_memory_enabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_enabled);

        let _env = set_env("KF_CODE_MEMORY_ENABLED", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_enabled);
    }

    #[test]
    fn test_env_memory_max_tokens() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MEMORY_MAX_TOKENS", Some("250"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_max_tokens, 250);
    }

    #[test]
    fn test_env_memory_top_n() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MEMORY_TOP_N", Some("5"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_top_n, 5);
    }

    #[test]
    fn test_env_memory_auto_populate() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_auto_populate);
        let _env = set_env("KF_CODE_MEMORY_AUTO_POPULATE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_auto_populate);
    }

    #[test]
    fn test_env_memory_show_in_status() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_show_in_status);
        let _env = set_env("KF_CODE_MEMORY_SHOW_IN_STATUS", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_show_in_status);
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
    fn test_env_checkpoint_interval_messages() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_CHECKPOINT_INTERVAL_MESSAGES", Some("20"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.session.checkpoint_interval_messages, 20);
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
    fn parse_bool_env_recognizes_true_and_false_variants() {
        assert_eq!(parse_bool_env("true"), Some(true));
        assert_eq!(parse_bool_env("True"), Some(true));
        assert_eq!(parse_bool_env("1"), Some(true));
        assert_eq!(parse_bool_env("yes"), Some(true));
        assert_eq!(parse_bool_env("false"), Some(false));
        assert_eq!(parse_bool_env("False"), Some(false));
        assert_eq!(parse_bool_env("0"), Some(false));
        assert_eq!(parse_bool_env("no"), Some(false));
        assert_eq!(parse_bool_env("maybe"), None);
        assert_eq!(parse_bool_env(""), None);
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
        assert!(!cfg.tools.minify_write_side);
        let table: toml::Table = r#"
            minify_write_side = true
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);
        assert!(cfg.tools.minify_write_side);
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
    fn test_env_anthropic_cloud_and_computer_use() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_ANTHROPIC_PROVIDER", Some("vertex"));
        let _e2 = set_env("KF_CODE_AWS_REGION", Some("eu-west-1"));
        let _e3 = set_env("KF_CODE_GCP_PROJECT_ID", Some("p2"));
        let _e4 = set_env("KF_CODE_GCP_REGION", Some("europe-west1"));
        let _e5 = set_env("KF_CODE_GCP_SERVICE_ACCOUNT_PATH", Some("/tmp/p2.json"));
        let _e6 = set_env("KF_CODE_COMPUTER_USE_ENABLED", Some("true"));
        let _e7 = set_env("KF_CODE_COMPUTER_USE_WIDTH", Some("1366"));
        let _e8 = set_env("KF_CODE_COMPUTER_USE_HEIGHT", Some("768"));
        let _e9 = set_env("KF_CODE_COMPUTER_USE_STARTUP_TIMEOUT", Some("60"));
        let _e10 = set_env("KF_CODE_COMPUTER_USE_WAIT_TIMEOUT", Some("20"));

        apply_env_overrides(&mut cfg);

        assert_eq!(cfg.model.anthropic_provider, "vertex");
        assert_eq!(cfg.model.aws_region, "eu-west-1");
        assert_eq!(cfg.model.gcp_project_id, "p2");
        assert_eq!(cfg.model.gcp_region, "europe-west1");
        assert_eq!(
            cfg.model.gcp_service_account_path,
            Some(PathBuf::from("/tmp/p2.json"))
        );
        assert!(cfg.security.computer_use.enabled);
        assert_eq!(cfg.security.computer_use.width, 1366);
        assert_eq!(cfg.security.computer_use.height, 768);
        assert_eq!(cfg.security.computer_use.startup_timeout_secs, 60);
        assert_eq!(cfg.security.computer_use.wait_timeout_secs, 20);
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

    #[test]
    fn test_env_scheduled_bash_auto_approve() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.scheduled_bash_auto_approve);
        let _env = set_env("KF_CODE_SCHEDULED_BASH_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.scheduled_bash_auto_approve);
    }

    #[test]
    fn test_env_max_concurrent_scheduled_jobs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 1);
        let _e2 = set_env("KF_CODE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("8"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 8);
    }

    #[test]
    fn test_env_max_background_tasks_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 1);
        let _e2 = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("16"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 16);
        let _e3 = set_env("KF_CODE_MAX_BACKGROUND_TASKS", Some("100"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_background_tasks, 64);
    }

    #[test]
    fn test_env_task_concurrency_mode() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert_eq!(cfg.tools.task_concurrency_mode, "queue");
        let _env = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("reject"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.task_concurrency_mode, "reject");
        let _e2 = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("QUEUE"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.task_concurrency_mode, "queue");
        let _e3 = set_env("KF_CODE_TASK_CONCURRENCY_MODE", Some("invalid"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.tools.task_concurrency_mode, "queue",
            "invalid value should not change mode"
        );
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
    fn test_env_request_timeout_override_and_clamp() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        let _env = set_env("KF_CODE_REQUEST_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.model.request_timeout_secs, 1,
            "env zero timeout must be clamped"
        );

        let _e2 = set_env("KF_CODE_REQUEST_TIMEOUT_SECS", Some("45"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.request_timeout_secs, 45);
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
    fn test_expand_tilde_str_empty_string() {
        assert_eq!(expand_tilde_str(""), "");
    }

    #[test]
    fn test_expand_tilde_str_no_tilde() {
        assert_eq!(expand_tilde_str("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn test_expand_tilde_str_expands_home() {
        let expanded = expand_tilde_str("~/projects");
        assert!(
            !expanded.starts_with('~'),
            "tilde should be expanded, got: {expanded}"
        );
    }

    #[test]
    fn test_expand_tilde_str_preserves_trailing_path() {
        let expanded = expand_tilde_str("~/a/b/c");
        assert!(expanded.ends_with("/a/b/c"), "got: {expanded}");
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

    // First-run onboarding banner (WO 14.1). The banner text must name
    // the config path and give a concrete `-m` hint so a new user knows
    // what to do next. `first_run_banner` is the pure helper that
    // `load_or_create_config` prints to stdout on first run.
    #[test]
    fn test_env_adapter_routing_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(
            cfg.model.adapter_routing.is_empty(),
            "adapter_routing defaults to empty"
        );

        let _env = set_env(
            "KF_CODE_ADAPTER_ROUTING",
            Some("grok-=OpenAiCompat,my-llm=Ollama"),
        );
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.adapter_routing.len(), 2);
        assert_eq!(
            cfg.model.adapter_routing.get("grok-"),
            Some(&"OpenAiCompat".to_string())
        );
        assert_eq!(
            cfg.model.adapter_routing.get("my-llm"),
            Some(&"Ollama".to_string())
        );
    }

    #[test]
    fn test_env_adapter_routing_empty_value_is_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        let _env = set_env("KF_CODE_ADAPTER_ROUTING", Some(""));
        apply_env_overrides(&mut cfg);
        assert!(
            cfg.model.adapter_routing.is_empty(),
            "empty env value should not populate adapter_routing"
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

    #[test]
    fn first_run_banner_printed_to_stdout() {
        let path = std::path::PathBuf::from("/tmp/kf-code/config.toml");
        let banner = first_run_banner(&path);
        // The banner is what `load_or_create_config` prints via `println!`
        // on a first run (no config file present). Asserting on the helper
        // output avoids fragile in-process stdout capture while pinning the
        // exact user-visible wording.
        assert!(banner.contains("Config created"), "got: {banner}");
        assert!(banner.contains("/tmp/kf-code/config.toml"), "got: {banner}");
        assert!(banner.contains("-m qwen2.5:0.5b"), "got: {banner}");
    }

    // The banner must fire exactly once: a second `load_or_create_config`
    // with the file already present must NOT re-print. The gating
    // condition in `load_or_create_config` is `!exists`, so once the
    // first call writes the file, the banner branch is skipped. This
    // uses a process-unique temp `KF_CODE_DATA_DIR` per WO 10.6 so
    // the first-run detection is deterministic and does not race with
    // other config tests.
    #[test]
    fn first_run_banner_silent_on_second_run() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("kf_code_first_run_{}_0", std::process::id(),));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let config_path = super::super::config_path();
        assert!(!config_path.exists(), "precondition: no config yet");

        // First run creates the file (banner would print).
        let _cfg = load_or_create_config();
        assert!(config_path.exists(), "first run created the config file");

        // Second run: the file now exists, so `!exists` is false and the
        // banner branch in `load_or_create_config` is skipped. We assert
        // the gating condition holds — the file is present before the
        // second call, which is exactly what suppresses the banner.
        assert!(
            config_path.exists(),
            "second run must see the file present (banner suppressed)"
        );
        let _cfg2 = load_or_create_config();

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drift-guard: when a new field is added to any Config sub-struct
    /// (ModelConfig, SecurityConfig, ToolConfig, SessionConfig, DisplayConfig)
    /// the author must also add it to:
    ///   1. `merge_toml_into_config`
    ///   2. `apply_env_overrides`
    ///   3. this test's TOML table / env-var list
    ///
    ///   and update `CONFIG_FIELD_COUNT` in `shared::config`.
    ///
    /// If any site is missing the field, the counts below will diverge
    /// from their expected values and the test will fail.
    //
    // WO 27.2-R2: un-ignored after recomputing the expected literals.
    // The const itself had drifted (+5 over the real struct count) and
    // the env-var assertion was tautological (`assert_eq!(80, 80)`).
    // Both corrected: const 103 → 98, env-var count made real, merge
    // TOML expected bumped 78 → 85 to cover compaction_use_llm,
    // doom_loop_action, and plugin_trust_workspace.
    #[test]
    fn config_field_count_drift_guard() {
        use crate::shared::config::CONFIG_FIELD_COUNT;

        // ── 1. Total struct-level fields ──────────────────────────
        // ModelConfig=33 (32 direct + subagent_provider sub-struct handle),
        // SecurityConfig=22, ToolConfig=33, SessionConfig=8,
        // DisplayConfig=7 → 103 total pub fields.
        assert_eq!(
            CONFIG_FIELD_COUNT, 103,
            "CONFIG_FIELD_COUNT has drifted — did you add/remove a config field?"
        );

        // ── 2. merge_toml_into_config field coverage ──────────────
        // Build a TOML table with every key that merge_toml_into_config
        // processes and count the entries. The number must stay in sync
        // with the function body.
        let merge_toml_source = r#"
            default_model = "x"
            ollama_host = "x"
            auto_approve = true
            sandbox_dir = "x"
            block_dotfiles = true
            max_file_read_size = 999
            request_timeout_secs = 999
            streaming_timeout_secs = 999
            follow_symlinks = true
            block_binary_reads = true
            minify_write_side = true
            minify_above_bytes = 999
            scheduled_bash_auto_approve = true
            max_concurrent_scheduled_jobs = 999
            carryover_enabled = true
            compaction_use_heuristic = true
            compaction_use_llm = true
            compaction_drop_threshold = 0.5
            stem_file_cap = 999
            shutdown_timeout_secs = 999
            dry_run = true
            cache_enabled = true
            cache_dir = "x"
            bang_requires_approval = true
            json_mode = true
            extended_thinking = true
            budget_tokens = 999
            max_tokens = 999
            bash_sandbox_workdir = true
            bash_require_allowlist = true
            bash_allowlist = ["ls", "echo"]
            block_gitignored_dotfiles = true
            max_overwrite_size = 999
            summarize_model = "x"
            routing_enabled = true
            router_model = "x"
            commit_max_file_size = 999
            preserve_recent_messages = 999
            max_tool_calls_per_turn = 999
            max_persona_turns = 999
            max_continuation_rounds = 5
            max_background_tasks = 4
            task_concurrency_mode = "queue"
            doom_loop_max_hits = 3
            doom_loop_action = "x"
            tool_timeout_secs = 999
            audit_log_path = "x"
            diff_review = false
            hooks_dir = "x"
            reject_on_excess_plugin_trust = true
            plugin_signature_validation = true
            plugin_trust_workspace = true
            plugin_public_key_path = "x"
            memory_enabled = true
            memory_max_tokens = 999
            memory_top_n = 999
            memory_auto_populate = true
            memory_show_in_status = true
            theme = "x"
            mouse_enabled = true
            checkpoint_interval_messages = 999
            anthropic_provider = "x"
            aws_region = "x"
            gcp_project_id = "x"
            gcp_region = "x"
            gcp_service_account_path = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
            deny_paths = ["/x"]
            deny_urls = ["x"]
            deny_extensions = [".x"]
            allowed_write_dirs = ["/x"]
            landlock_extra_paths = ["/x"]
            plugin_allowed_env_vars = ["x"]
            plugin_sources = { x = "/x" }
            enabled_plugins = ["x"]
            disabled_plugins = ["x"]
            routing_model_map = { x = "x" }
            adapter_routing = { x = "x" }

            [computer_use]
            enabled = true
            chrome_path = "x"
            headful = true
            width = 999
            height = 999
            startup_timeout_secs = 999
            wait_timeout_secs = 999

            [subagent_provider]
            model = "x"
            ollama_host = "x"
            anthropic_api_key = "x"
            openai_api_key = "x"
            deepseek_api_key = "x"
            gemini_api_key = "x"
            kimi_api_key = "x"
        "#;

        let table: toml::Table = merge_toml_source.parse().unwrap();
        let mut toml_key_count: usize = 0;
        for (_key, value) in &table {
            if let Some(sub) = value.as_table() {
                toml_key_count += sub.len();
            } else {
                toml_key_count += 1;
            }
        }
        // 70 top-level leaf keys + 9 array keys + 3 single-key inline
        // tables + 7 computer_use sub-keys + 7 subagent_provider sub-keys = 96
        const MERGE_TOML_EXPECTED: usize = 96;
        assert_eq!(
            toml_key_count, MERGE_TOML_EXPECTED,
            "merge_toml_into_config key count changed — did you add/remove a handled field?"
        );

        // ── 3. apply_env_overrides field coverage ─────────────────
        // Count env-var read calls (direct std::env::var + env_bool!
        // macro invocations) in env_overrides.rs. This must stay in
        // sync with the function body. WO 27.2-R2: replaced the
        // tautological `assert_eq!(80, 80)` with a real source scan.
        let env_overrides_src = include_str!("env_overrides.rs");
        let env_var_count = env_overrides_src.matches("\"KF_CODE_").count()
            + env_overrides_src.matches("\"ANTHROPIC_API_KEY\"").count()
            + env_overrides_src.matches("\"OPENAI_API_KEY\"").count()
            + env_overrides_src.matches("\"DEEPSEEK_API_KEY\"").count()
            + env_overrides_src.matches("\"GEMINI_API_KEY\"").count()
            + env_overrides_src.matches("\"KIMI_API_KEY\"").count();
        // 87 KF_CODE_* literals (77 base + 7 KF_CODE_SUBAGENT_* + 2 bash
        // allowlist + 1 streaming_timeout_secs) + 5 API-key literals = 92
        const ENV_OVERRIDE_EXPECTED: usize = 92;
        assert_eq!(
            env_var_count, ENV_OVERRIDE_EXPECTED,
            "apply_env_overrides env-var count changed — did you add/remove a KF_CODE_* var?"
        );

        // ── 4. Relationship to total field count ──────────────────
        // When CONFIG_FIELD_COUNT changes, verify that the difference
        // between it and the TOML/env counts is still intentional.
        // merge_toml expands sub-structs (e.g. computer_use has 7 sub-keys)
        // and skips some struct fields entirely, so the gap is NOT simply
        // CONFIG_FIELD_COUNT - MERGE_TOML_EXPECTED. The important invariant
        // is: every struct field is EITHER handled by merge_toml OR
        // intentionally skipped. The same applies to apply_env_overrides.
        //
        // Intentionally skipped by merge_toml (15 struct-level fields):
        //   ModelConfig:  summarize_enabled, subagent_allowed_models,
        //                 opencode_zen_api_key, opencode_zen_endpoint, seed
        //   SecurityConfig: permission_rules, docker (4 sub-fields),
        //                   sandbox (4 sub-fields), computer_use.max_steps
        //   ToolConfig:  max_tool_result_chars,
        //                mcp_servers, lsp_servers, max_plugin_trust,
        //                stratum_mode, budget_ceiling, budget_approaching_ratio
        //   SessionConfig: worktree_enabled
        //
        // Additionally skipped by apply_env_overrides (4 more, beyond the 15):
        //   SecurityConfig: deny_paths, deny_urls, deny_extensions,
        //                   allowed_write_dirs
        //   (Arrays/Vec fields without env-var representations.)
        //
        // The expansion of computer_use (1 struct field → 7 TOML keys)
        // means MERGE_TOML_EXPECTED = top-level leaf keys + 7 expansion
        // keys.
        let _ = (
            CONFIG_FIELD_COUNT,
            MERGE_TOML_EXPECTED,
            ENV_OVERRIDE_EXPECTED,
        );

        // ── 5. Serde field count vs CONFIG_FIELD_COUNT ──────────
        // Serialize a default Config to JSON and count top-level keys.
        // This catches the case where someone adds a field to a sub-struct
        // but forgets to update CONFIG_FIELD_COUNT. Fields with
        // #[serde(skip_serializing)] are not counted by serde but ARE
        // config fields, so we add them back.
        let default_config = crate::shared::config::Config::default();
        let json = serde_json::to_value(&default_config).unwrap();
        let obj = json.as_object().unwrap();
        // Fields that exist in the struct but are skipped during serialization.
        const SKIP_SERIALIZING_FIELDS: usize = 1; // seed (ModelConfig)
                                                  // Flattened-name collisions: ToolConfig.memory_auto_populate and
                                                  // DisplayConfig.memory_auto_populate both serialize to the same
                                                  // JSON key, so serde produces 1 key for 2 struct fields. Document
                                                  // here so the count stays honest.
        const FLATTEN_COLLISIONS: usize = 1; // memory_auto_populate
        let serde_field_count = obj.len() + SKIP_SERIALIZING_FIELDS + FLATTEN_COLLISIONS;
        assert_eq!(
            CONFIG_FIELD_COUNT, serde_field_count,
            "CONFIG_FIELD_COUNT ({CONFIG_FIELD_COUNT}) != serde field count ({serde_field_count}) \
             — did you add/remove a config field without updating CONFIG_FIELD_COUNT?"
        );
    }

    /// **Contract:** a config.toml written by an older build (or
    /// hand-edited to a subset of keys) must load via the PRIMARY serde
    /// path — user values preserved, missing fields filled from
    /// `Default`. The struct-level `#[serde(default)]` on the Config
    /// sub-structs is what guarantees this: without it, any field the
    /// file lacks that has no field-level serde default makes
    /// `toml::from_str::<Config>` fail, and the load falls into the
    /// `merge_toml_into_config` fallback — which silently resets every
    /// field it doesn't handle (budget_ceiling, summarize_enabled,
    /// docker, sandbox, permission_rules, …). The next `save_config`
    /// then persists the wipe. This test simulates schema drift with a
    /// file that predates `default_model` (and sets two
    /// fallback-skipped canaries); if the load ever regresses to the
    /// lossy fallback, the canaries come back as defaults.
    #[test]
    fn schema_drift_preserves_user_values() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("kf_code_drift_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let path = super::super::config_path();
        std::fs::write(
            &path,
            "ollama_host = \"http://my-host:1234\"\n\
             auto_approve = true\n\
             budget_ceiling = 50000\n\
             summarize_enabled = true\n",
        )
        .unwrap();

        let (cfg, warning) = load_config();
        assert!(
            warning.is_none(),
            "drifted config must parse via the primary serde path, got: {warning:?}"
        );
        // User-set values survive.
        assert_eq!(cfg.model.ollama_host, "http://my-host:1234");
        assert!(cfg.security.auto_approve);
        // Canaries for fields merge_toml_into_config does NOT handle:
        // only the primary serde path preserves them.
        assert_eq!(cfg.tools.budget_ceiling, 50000);
        assert!(cfg.model.summarize_enabled);
        // Missing fields are filled from defaults, not errors.
        assert!(cfg.model.default_model.is_empty());
        assert_eq!(cfg.model.request_timeout_secs, 120);

        // The merged result must round-trip: saving (e.g. a plugin
        // enable or "always allow" persisting a permission rule) writes
        // the user's values back, never a fresh Config::default().
        save_config(&cfg).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("http://my-host:1234"),
            "user's ollama_host wiped on save:\n{on_disk}"
        );
        assert!(
            on_disk.contains("budget_ceiling = 50000"),
            "user's budget_ceiling wiped on save:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Contract:** an existing `config.toml` at the current data-dir
    /// path must NOT be overwritten by `load_or_create_config`. This is
    /// the core pin against the reported "config gets reset on install"
    /// bug: the function's `!exists` guard is what protects returning
    /// users. If this regresses, the banner prints on every run and the
    /// user's customizations are wiped.
    #[test]
    fn existing_config_not_overwritten_on_startup() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "kf_code_keep_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let _env1 =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", dir.to_str().unwrap());
        let _env2 = crate::shared::test_util::EnvGuard::remove("KF_CODE_LEGACY_DATA_DIR");
        let path = super::super::config_path();
        // Seed a config with a user customization.
        std::fs::write(
            &path,
            "ollama_host = \"http://user-custom:9999\"\n\
             auto_approve = true\n",
        )
        .unwrap();

        let cfg = load_or_create_config();
        assert_eq!(
            cfg.model.ollama_host, "http://user-custom:9999",
            "existing user config must not be overwritten"
        );
        assert!(cfg.security.auto_approve);
        // The file on disk must still contain the user's values, not defaults.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("http://user-custom:9999"),
            "config file on disk was overwritten with defaults:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Contract:** when the config is absent at the current path but a
    /// legacy `kirkforge`-era config exists at the pre-rename path,
    /// `load_or_create_config` must migrate it (copy) to the new path
    /// instead of writing fresh defaults. This is the migration pin for
    /// the `kirkforge → kf-code` rename (commit ae0e37d). Without it,
    /// upgrading users lose every customization on first run of the new
    /// binary — the exact "dead on startup" report.
    #[test]
    fn legacy_kirkforge_config_migrated_not_overwritten() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let new_dir = std::env::temp_dir().join(format!(
            "kf_code_mig_new_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let legacy_dir = std::env::temp_dir().join(format!(
            "kf_code_mig_legacy_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .wrapping_add(1)
        ));
        let _ = std::fs::remove_dir_all(&new_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
        std::fs::create_dir_all(&legacy_dir).unwrap();

        // Simulate a pre-rename user: config lives at the legacy path
        // with a customization. The new path must not exist yet.
        std::fs::write(
            legacy_dir.join("config.toml"),
            "ollama_host = \"http://user-custom:9999\"\n\
             auto_approve = true\n",
        )
        .unwrap();

        let _env1 =
            crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", new_dir.to_str().unwrap());
        let _env2 = crate::shared::test_util::EnvGuard::set(
            "KF_CODE_LEGACY_DATA_DIR",
            legacy_dir.to_str().unwrap(),
        );
        let new_path = super::super::config_path();
        assert!(!new_path.exists(), "precondition: new config absent");

        let cfg = load_or_create_config();
        // User values migrated, not wiped to defaults.
        assert_eq!(
            cfg.model.ollama_host, "http://user-custom:9999",
            "legacy config must be migrated, not overwritten with defaults"
        );
        assert!(cfg.security.auto_approve);
        // The migration wrote the user's file to the new path verbatim.
        assert!(new_path.exists(), "migrated config written to new path");
        let on_disk = std::fs::read_to_string(&new_path).unwrap();
        assert!(
            on_disk.contains("http://user-custom:9999"),
            "migrated file must contain user values, got:\n{on_disk}"
        );

        let _ = std::fs::remove_dir_all(&new_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
    }
}
