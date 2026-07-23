/// Config bootstrap — layered config resolution with env var overrides.
///
/// Resolution order (highest to lowest priority):
/// 1. CLI arguments (handled in main.rs)
/// 2. Environment variables (`KIRKFORGE_*`)
/// 3. Config file (`~/.local/share/kirkforge/config.toml`)
/// 4. Built-in defaults
///
/// Environment variable reference:
/// - `KIRKFORGE_MODEL` — default model name
/// - `KIRKFORGE_HOST` — Ollama host URL
/// - `KIRKFORGE_AUTO_APPROVE` — "true" to auto-approve destructive calls
/// - `KIRKFORGE_DRY_RUN` — "true" to make destructive tools report only
/// - `KIRKFORGE_SANDBOX_DIR` — sandbox directory path
/// - `KIRKFORGE_BLOCK_DOTFILES` — "true" to block dotfile writes
/// - `KIRKFORGE_BLOCK_GITIGNORED_DOTFILES` — "true" to block git-ignored dotfile writes
/// - `KIRKFORGE_MAX_READ_SIZE` — max file read size in bytes
/// - `KIRKFORGE_MAX_OVERWRITE_SIZE` — max existing file size that write tools may overwrite
/// - `KIRKFORGE_FOLLOW_SYMLINKS` — "true" to allow following symlinks
/// - `KIRKFORGE_BLOCK_BINARY` — "true" to block binary file reads
/// - `KIRKFORGE_MINIFY_WRITE_SIDE` — "true" to enable minified-envelope write-side expansion
/// - `KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE` — "true" to let scheduled bash jobs skip interactive approval
/// - `KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS` — max concurrent scheduled jobs (clamped to ≥1)
/// - `KIRKFORGE_BASH_SANDBOX_WORKDIR` — "true"/"false" to force bash cwd into the sandbox
/// - `KIRKFORGE_BANG_REQUIRES_APPROVAL` — "true" to route `!` passthrough through approval gate
/// - `KIRKFORGE_JSON_MODE` — "true" to request JSON-formatted model responses
/// - `KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST` — "true" to reject plugins above max trust
/// - `KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION` — "true" to require `.kirkforge.sig`
/// - `KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH` — minisign public key for plugin signatures
/// - `KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS` — comma-separated extra env vars for plugin tools
/// - `KIRKFORGE_PLUGIN_SOURCES` — comma-separated `name=path` workspace plugin sources
/// - `KIRKFORGE_ENABLED_PLUGINS` — comma-separated names from `plugin_sources` to load
/// - `KIRKFORGE_MEMORY_ENABLED` — "true"/"false" to enable or disable memory injection
/// - `KIRKFORGE_MEMORY_MAX_TOKENS` — token budget for injected memory facts
/// - `KIRKFORGE_MEMORY_TOP_N` — maximum number of facts to consider per turn
/// - `KIRKFORGE_REQUEST_TIMEOUT_SECS` — model request timeout (clamped to ≥1 s)
/// - `KIRKFORGE_TOOL_TIMEOUT_SECS` — per-tool hard timeout (clamped to [1, 3600])
/// - `KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES` — write a checkpoint every N messages
/// - `KIRKFORGE_SUMMARIZE_MODEL` — fast model used by `/compact`
/// - `KIRKFORGE_ROUTING_ENABLED` — "true" to enable smart model routing
/// - `KIRKFORGE_ROUTER_MODEL` — model used for routing classification
/// - `KIRKFORGE_COMMIT_MAX_FILE_SIZE` — max file size allowed in `/commit`
/// - `KIRKFORGE_PRESERVE_RECENT_MESSAGES` — number of recent messages kept verbatim on compact
/// - `KIRKFORGE_MAX_TOOL_CALLS_PER_TURN` — cap on model↔tool iterations per turn
/// - `KIRKFORGE_MAX_PERSONA_TURNS` — cap on fork-isolated persona turns
/// - `KIRKFORGE_AUDIT_LOG_PATH` — path for the append-only JSONL audit log (empty disables)
/// - `KIRKFORGE_HOOKS_DIR` — directory containing lifecycle hook scripts
///
/// Boolean env vars accept `true`/`1`/`yes` (case-insensitive) for true and
/// `false`/`0`/`no` for false. Unrecognized values leave the prior layer
/// unchanged.
use crate::shared::Config;
use std::path::PathBuf;

mod env_overrides;

// Re-import so `load_config` and the in-file tests (which use
// `use super::*`) keep seeing `apply_env_overrides` at the same path.
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
        match toml::from_str::<Config>(&content) {
            Ok(file_cfg) => cfg = file_cfg,
            Err(e) => {
                let msg = format!("Failed to parse config ({e}), merging with defaults");
                tracing::warn!(%msg);
                warning = Some(msg);
                // Try partial merge: parse what we can
                if let Ok(table) = content.parse::<toml::Table>() {
                    merge_toml_into_config(&mut cfg, table);
                }
            }
        }
    }

    // Layer 2: environment variables
    apply_env_overrides(&mut cfg);

    (cfg, warning)
}

/// Load config and write a default file on first run.
///
/// If the config file doesn't exist, creates it with default values
/// and prints a brief info message.
pub fn load_or_create_config() -> Config {
    let path = super::config_path();
    let exists = path.exists();

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
        // an earlier `KIRKFORGE_SANDBOX_DIR` override). Respect
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
    if let Some(Value::Boolean(v)) = table.get("follow_symlinks") {
        cfg.tools.follow_symlinks = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("block_binary_reads") {
        cfg.tools.block_binary_reads = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("minify_write_side") {
        cfg.tools.minify_write_side = *v;
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
    if let Some(Value::Boolean(v)) = table.get("bash_sandbox_workdir") {
        cfg.security.bash_sandbox_workdir = *v;
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

    // Anthropic cloud-provider routing
    if let Some(Value::String(v)) = table.get("anthropic_provider") {
        cfg.model.anthropic_provider = v.clone();
    }
    if let Some(Value::String(v)) = table.get("aws_region") {
        cfg.model.aws_region = v.clone();
    }
    if let Some(Value::String(v)) = table.get("aws_profile") {
        cfg.model.aws_profile = v.clone();
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

/// Parse `KIRKFORGE_PLUGIN_SOURCES` env var.
///
/// Format: comma-separated `name=path` entries. Entries without `=` are
/// ignored. Paths are kept exactly as written; the loader canonicalizes
/// them at use time.
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

    /// Serialize tests that mutate process-wide environment variables.
    /// Rust unit tests run in parallel by default; `std::env::set_var` is
    /// process-wide, so concurrent env tests can observe each other's state
    /// and fail sporadically.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Helper to temporarily set an env var for a test. Must be called
    /// while `ENV_LOCK` is held.
    fn set_env(key: &str, val: Option<&str>) {
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
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

        set_env("KIRKFORGE_MODEL", Some("deepseek-v4:cloud"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.default_model, "deepseek-v4:cloud");
        set_env("KIRKFORGE_MODEL", None);
    }

    #[test]
    fn test_env_auto_approve_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.security.auto_approve);

        set_env("KIRKFORGE_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_auto_approve_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.security.auto_approve = true;

        set_env("KIRKFORGE_AUTO_APPROVE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.security.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_dry_run_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.dry_run);

        set_env("KIRKFORGE_DRY_RUN", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.dry_run);
        set_env("KIRKFORGE_DRY_RUN", None);
    }

    #[test]
    fn test_env_dry_run_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        cfg.tools.dry_run = true;

        set_env("KIRKFORGE_DRY_RUN", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.dry_run);
        set_env("KIRKFORGE_DRY_RUN", None);
    }

    #[test]
    fn test_env_block_dotfiles() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_DOTFILES", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.security.block_dotfiles);
        set_env("KIRKFORGE_BLOCK_DOTFILES", None);
    }

    #[test]
    fn test_env_follow_symlinks() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.follow_symlinks);
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", None);
    }

    #[test]
    fn test_env_block_binary() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_BINARY", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.block_binary_reads);
        set_env("KIRKFORGE_BLOCK_BINARY", None);
    }

    #[test]
    fn test_env_minify_write_side() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.minify_write_side);
        set_env("KIRKFORGE_MINIFY_WRITE_SIDE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.minify_write_side);
        set_env("KIRKFORGE_MINIFY_WRITE_SIDE", None);
    }

    #[test]
    fn test_env_max_read_size() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_READ_SIZE", Some("65536"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 65536);
        set_env("KIRKFORGE_MAX_READ_SIZE", None);
    }

    #[test]
    fn test_env_bad_max_read_size_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_READ_SIZE", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.security.max_file_read_size, 1024 * 1024);
        set_env("KIRKFORGE_MAX_READ_SIZE", None);
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

        set_env("KIRKFORGE_BANG_REQUIRES_APPROVAL", Some("true"));
        set_env("KIRKFORGE_JSON_MODE", Some("true"));
        set_env("KIRKFORGE_BASH_SANDBOX_WORKDIR", Some("false"));
        set_env("KIRKFORGE_BLOCK_GITIGNORED_DOTFILES", Some("false"));
        set_env("KIRKFORGE_MAX_OVERWRITE_SIZE", Some("2097152"));
        set_env("KIRKFORGE_SUMMARIZE_MODEL", Some("my-summarize-model"));
        set_env("KIRKFORGE_ROUTING_ENABLED", Some("true"));
        set_env("KIRKFORGE_ROUTER_MODEL", Some("my-router-model"));
        set_env("KIRKFORGE_COMMIT_MAX_FILE_SIZE", Some("1048576"));
        set_env("KIRKFORGE_PRESERVE_RECENT_MESSAGES", Some("5"));
        set_env("KIRKFORGE_MAX_TOOL_CALLS_PER_TURN", Some("25"));
        set_env("KIRKFORGE_MAX_PERSONA_TURNS", Some("3"));
        set_env("KIRKFORGE_TOOL_TIMEOUT_SECS", Some("60"));
        set_env("KIRKFORGE_AUDIT_LOG_PATH", Some("/tmp/kf-audit.ndjson"));
        set_env("KIRKFORGE_HOOKS_DIR", Some("/tmp/kf-hooks"));

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

        set_env("KIRKFORGE_BANG_REQUIRES_APPROVAL", None);
        set_env("KIRKFORGE_JSON_MODE", None);
        set_env("KIRKFORGE_BASH_SANDBOX_WORKDIR", None);
        set_env("KIRKFORGE_BLOCK_GITIGNORED_DOTFILES", None);
        set_env("KIRKFORGE_MAX_OVERWRITE_SIZE", None);
        set_env("KIRKFORGE_SUMMARIZE_MODEL", None);
        set_env("KIRKFORGE_ROUTING_ENABLED", None);
        set_env("KIRKFORGE_ROUTER_MODEL", None);
        set_env("KIRKFORGE_COMMIT_MAX_FILE_SIZE", None);
        set_env("KIRKFORGE_PRESERVE_RECENT_MESSAGES", None);
        set_env("KIRKFORGE_MAX_TOOL_CALLS_PER_TURN", None);
        set_env("KIRKFORGE_MAX_PERSONA_TURNS", None);
        set_env("KIRKFORGE_TOOL_TIMEOUT_SECS", None);
        set_env("KIRKFORGE_AUDIT_LOG_PATH", None);
        set_env("KIRKFORGE_HOOKS_DIR", None);
    }

    #[test]
    fn test_env_tool_timeout_secs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();

        set_env("KIRKFORGE_TOOL_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(1));

        set_env("KIRKFORGE_TOOL_TIMEOUT_SECS", Some("7200"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.tool_timeout_secs, Some(3600));

        set_env("KIRKFORGE_TOOL_TIMEOUT_SECS", None);
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

        set_env("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.tools.reject_on_excess_plugin_trust);
        set_env("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST", None);
    }

    #[test]
    fn test_env_plugin_signature_validation() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.tools.plugin_signature_validation);

        set_env("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.plugin_signature_validation);
        set_env("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION", None);
    }

    #[test]
    fn test_env_plugin_public_key_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH", Some("/tmp/key.pub"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.tools.plugin_public_key_path.as_deref(),
            Some("/tmp/key.pub")
        );
        set_env("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH", None);
    }

    #[test]
    fn test_env_plugin_allowed_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS", Some("FOO,BAR"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.plugin_allowed_env_vars, vec!["FOO", "BAR"]);
        set_env("KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS", None);
    }

    #[test]
    fn test_merge_toml_plugin_trust_knobs() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            reject_on_excess_plugin_trust = false
            plugin_signature_validation = true
            plugin_public_key_path = "/opt/kirkforge/plugin.pub"
            plugin_allowed_env_vars = ["CUSTOM_VAR"]
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert!(!cfg.tools.reject_on_excess_plugin_trust);
        assert!(cfg.tools.plugin_signature_validation);
        assert_eq!(
            cfg.tools.plugin_public_key_path.as_deref(),
            Some("/opt/kirkforge/plugin.pub")
        );
        assert_eq!(cfg.tools.plugin_allowed_env_vars, vec!["CUSTOM_VAR"]);
    }

    #[test]
    fn test_env_memory_enabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.display.memory_enabled);

        set_env("KIRKFORGE_MEMORY_ENABLED", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.display.memory_enabled);
        set_env("KIRKFORGE_MEMORY_ENABLED", None);
    }

    #[test]
    fn test_env_memory_max_tokens() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MEMORY_MAX_TOKENS", Some("250"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_max_tokens, 250);
        set_env("KIRKFORGE_MEMORY_MAX_TOKENS", None);
    }

    #[test]
    fn test_env_memory_top_n() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MEMORY_TOP_N", Some("5"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.display.memory_top_n, 5);
        set_env("KIRKFORGE_MEMORY_TOP_N", None);
    }

    #[test]
    fn test_merge_toml_memory_knobs() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            memory_enabled = false
            memory_max_tokens = 300
            memory_top_n = 3
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert!(!cfg.display.memory_enabled);
        assert_eq!(cfg.display.memory_max_tokens, 300);
        assert_eq!(cfg.display.memory_top_n, 3);
    }

    #[test]
    fn test_config_diff_summary_memory_knobs() {
        let a = Config::default();
        let mut b = Config::default();
        b.display.memory_enabled = false;
        b.display.memory_max_tokens = 250;
        b.display.memory_top_n = 5;
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("memory_enabled"), "got: {s}");
        assert!(s.contains("memory_max_tokens"), "got: {s}");
        assert!(s.contains("memory_top_n"), "got: {s}");
    }

    #[test]
    fn test_env_checkpoint_interval_messages() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES", Some("20"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.session.checkpoint_interval_messages, 20);
        set_env("KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES", None);
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
        b.tools.plugin_signature_validation = true;
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
    fn test_merge_toml_anthropic_cloud_and_computer_use() {
        let mut cfg = Config::default();
        let table: toml::Table = r#"
            anthropic_provider = "bedrock"
            aws_region = "us-west-2"
            aws_profile = "dev"
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
        assert_eq!(cfg.model.aws_profile, "dev");
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

        set_env("KIRKFORGE_ANTHROPIC_PROVIDER", Some("vertex"));
        set_env("KIRKFORGE_AWS_REGION", Some("eu-west-1"));
        set_env("KIRKFORGE_AWS_PROFILE", Some("prod"));
        set_env("KIRKFORGE_GCP_PROJECT_ID", Some("p2"));
        set_env("KIRKFORGE_GCP_REGION", Some("europe-west1"));
        set_env("KIRKFORGE_GCP_SERVICE_ACCOUNT_PATH", Some("/tmp/p2.json"));
        set_env("KIRKFORGE_COMPUTER_USE_ENABLED", Some("true"));
        set_env("KIRKFORGE_COMPUTER_USE_WIDTH", Some("1366"));
        set_env("KIRKFORGE_COMPUTER_USE_HEIGHT", Some("768"));
        set_env("KIRKFORGE_COMPUTER_USE_STARTUP_TIMEOUT", Some("60"));
        set_env("KIRKFORGE_COMPUTER_USE_WAIT_TIMEOUT", Some("20"));

        apply_env_overrides(&mut cfg);

        assert_eq!(cfg.model.anthropic_provider, "vertex");
        assert_eq!(cfg.model.aws_region, "eu-west-1");
        assert_eq!(cfg.model.aws_profile, "prod");
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

        set_env("KIRKFORGE_ANTHROPIC_PROVIDER", None);
        set_env("KIRKFORGE_AWS_REGION", None);
        set_env("KIRKFORGE_AWS_PROFILE", None);
        set_env("KIRKFORGE_GCP_PROJECT_ID", None);
        set_env("KIRKFORGE_GCP_REGION", None);
        set_env("KIRKFORGE_GCP_SERVICE_ACCOUNT_PATH", None);
        set_env("KIRKFORGE_COMPUTER_USE_ENABLED", None);
        set_env("KIRKFORGE_COMPUTER_USE_WIDTH", None);
        set_env("KIRKFORGE_COMPUTER_USE_HEIGHT", None);
        set_env("KIRKFORGE_COMPUTER_USE_STARTUP_TIMEOUT", None);
        set_env("KIRKFORGE_COMPUTER_USE_WAIT_TIMEOUT", None);
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
        set_env("KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.tools.scheduled_bash_auto_approve);
        set_env("KIRKFORGE_SCHEDULED_BASH_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_max_concurrent_scheduled_jobs_is_clamped() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 1);
        set_env("KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS", Some("8"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.tools.max_concurrent_scheduled_jobs, 8);
        set_env("KIRKFORGE_MAX_CONCURRENT_SCHEDULED_JOBS", None);
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
        set_env("KIRKFORGE_REQUEST_TIMEOUT_SECS", Some("0"));
        apply_env_overrides(&mut cfg);
        assert_eq!(
            cfg.model.request_timeout_secs, 1,
            "env zero timeout must be clamped"
        );

        set_env("KIRKFORGE_REQUEST_TIMEOUT_SECS", Some("45"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.model.request_timeout_secs, 45);

        set_env("KIRKFORGE_REQUEST_TIMEOUT_SECS", None);
    }
}
