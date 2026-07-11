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
/// - `KIRKFORGE_SANDBOX_DIR` — sandbox directory path
/// - `KIRKFORGE_BLOCK_DOTFILES` — "true" to block dotfile writes
/// - `KIRKFORGE_MAX_READ_SIZE` — max file read size in bytes
/// - `KIRKFORGE_FOLLOW_SYMLINKS` — "true" to allow following symlinks
/// - `KIRKFORGE_BLOCK_BINARY` — "true" to block binary file reads
/// - `KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST` — "true" to reject plugins above max trust
/// - `KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION` — "true" to require `.kirkforge.sig`
/// - `KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH` — minisign public key for plugin signatures
/// - `KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS` — comma-separated extra env vars for plugin tools
/// - `KIRKFORGE_PLUGIN_SOURCES` — comma-separated `name=path` workspace plugin sources
/// - `KIRKFORGE_ENABLED_PLUGINS` — comma-separated names from `plugin_sources` to load
/// - `KIRKFORGE_MEMORY_ENABLED` — "true"/"false" to enable or disable memory injection
/// - `KIRKFORGE_MEMORY_MAX_TOKENS` — token budget for injected memory facts
/// - `KIRKFORGE_MEMORY_TOP_N` — maximum number of facts to consider per turn
use crate::shared::Config;
use std::path::PathBuf;

/// Expand a leading `~` in a path string using `$HOME` (or the equivalent
/// on Windows). Falls back to the original string if expansion fails.
fn expand_tilde_str(s: &str) -> String {
    shellexpand::tilde(s).into_owned()
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

/// Resolve the launch-time cwd and assign it to `config.sandbox_dir` if
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
/// `config.sandbox_dir` means "intentionally unsandboxed," and we
/// do not overwrite it. Only the `None` case (operator didn't set
/// the field) is filled in.
pub fn freeze_launch_sandbox(config: &mut Config) -> Option<String> {
    if config.sandbox_dir.is_some() {
        // Operator already set it (via config file, env var, or
        // an earlier `KIRKFORGE_SANDBOX_DIR` override). Respect
        // their choice — even if it's an explicit empty string
        // meaning "unsandboxed."
        return config.sandbox_dir.clone();
    }
    match std::env::current_dir() {
        Ok(cwd) => {
            let path = cwd.to_string_lossy().to_string();
            config.sandbox_dir = Some(path.clone());
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

/// Apply environment variable overrides to a Config.
fn apply_env_overrides(cfg: &mut Config) {
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
        cfg.auto_approve = val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("1")
            || val.eq_ignore_ascii_case("yes");
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
        cfg.block_dotfiles = val.eq_ignore_ascii_case("true");
    }

    // KIRKFORGE_MAX_READ_SIZE
    if let Ok(val) = std::env::var("KIRKFORGE_MAX_READ_SIZE") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.max_file_read_size = n;
        }
    }

    // KIRKFORGE_FOLLOW_SYMLINKS
    if let Ok(val) = std::env::var("KIRKFORGE_FOLLOW_SYMLINKS") {
        cfg.follow_symlinks = val.eq_ignore_ascii_case("true");
    }

    // KIRKFORGE_BLOCK_BINARY
    if let Ok(val) = std::env::var("KIRKFORGE_BLOCK_BINARY") {
        cfg.block_binary_reads = val.eq_ignore_ascii_case("true");
    }

    // KIRKFORGE_CARRYOVER_ENABLED
    if let Ok(val) = std::env::var("KIRKFORGE_CARRYOVER_ENABLED") {
        cfg.carryover_enabled = val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("1")
            || val.eq_ignore_ascii_case("yes");
    }
    if let Ok(val) = std::env::var("KIRKFORGE_DRY_RUN") {
        cfg.dry_run = val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("1")
            || val.eq_ignore_ascii_case("yes");
    }
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_ENABLED") {
        cfg.cache_enabled = val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("1")
            || val.eq_ignore_ascii_case("yes");
    }
    if let Ok(val) = std::env::var("KIRKFORGE_CACHE_DIR") {
        cfg.cache_dir = Some(PathBuf::from(expand_tilde_str(&val)));
    }

    // KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST
    if let Ok(val) = std::env::var("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST") {
        cfg.reject_on_excess_plugin_trust = val.eq_ignore_ascii_case("true");
    }

    // KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION
    if let Ok(val) = std::env::var("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION") {
        cfg.plugin_signature_validation = val.eq_ignore_ascii_case("true");
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
        cfg.memory_enabled = val.eq_ignore_ascii_case("true")
            || val.eq_ignore_ascii_case("1")
            || val.eq_ignore_ascii_case("yes");
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

    // KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES
    if let Ok(val) = std::env::var("KIRKFORGE_CHECKPOINT_INTERVAL_MESSAGES") {
        if let Ok(n) = val.parse::<usize>() {
            cfg.checkpoint_interval_messages = n;
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
        cfg.default_model = v.clone();
    }
    if let Some(Value::String(v)) = table.get("ollama_host") {
        cfg.ollama_host = v.clone();
    }
    if let Some(Value::Boolean(v)) = table.get("auto_approve") {
        cfg.auto_approve = *v;
    }
    if let Some(Value::String(v)) = table.get("sandbox_dir") {
        cfg.sandbox_dir = Some(expand_tilde_str(v));
    }
    if let Some(Value::Boolean(v)) = table.get("block_dotfiles") {
        cfg.block_dotfiles = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_file_read_size") {
        if let Ok(n) = usize::try_from(*v) {
            cfg.max_file_read_size = n;
        }
    }
    if let Some(Value::Integer(v)) = table.get("request_timeout_secs") {
        if let Ok(n) = u64::try_from(*v) {
            cfg.request_timeout_secs = n;
        }
    }
    if let Some(Value::Boolean(v)) = table.get("follow_symlinks") {
        cfg.follow_symlinks = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("block_binary_reads") {
        cfg.block_binary_reads = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("carryover_enabled") {
        cfg.carryover_enabled = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("dry_run") {
        cfg.dry_run = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("cache_enabled") {
        cfg.cache_enabled = *v;
    }
    if let Some(Value::String(v)) = table.get("cache_dir") {
        cfg.cache_dir = Some(PathBuf::from(expand_tilde_str(v)));
    }

    // Plugin trust / sandbox knobs
    if let Some(Value::Boolean(v)) = table.get("reject_on_excess_plugin_trust") {
        cfg.reject_on_excess_plugin_trust = *v;
    }
    if let Some(Value::Boolean(v)) = table.get("plugin_signature_validation") {
        cfg.plugin_signature_validation = *v;
    }
    if let Some(Value::String(v)) = table.get("plugin_public_key_path") {
        cfg.plugin_public_key_path = if v.is_empty() {
            None
        } else {
            Some(expand_tilde_str(v))
        };
    }
    if let Some(Value::Array(v)) = table.get("plugin_allowed_env_vars") {
        cfg.plugin_allowed_env_vars = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Memory knobs
    if let Some(Value::Boolean(v)) = table.get("memory_enabled") {
        cfg.memory_enabled = *v;
    }
    if let Some(Value::Integer(v)) = table.get("memory_max_tokens") {
        cfg.memory_max_tokens = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("memory_top_n") {
        cfg.memory_top_n = (*v).max(1) as usize;
    }
    if let Some(Value::Integer(v)) = table.get("checkpoint_interval_messages") {
        cfg.checkpoint_interval_messages = (*v).max(0) as usize;
    }

    // Workspace plugin sources
    if let Some(Value::Table(v)) = table.get("plugin_sources") {
        cfg.plugin_sources = v
            .iter()
            .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), PathBuf::from(s))))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("enabled_plugins") {
        cfg.enabled_plugins = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    // Arrays
    if let Some(Value::Array(v)) = table.get("deny_paths") {
        cfg.deny_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(expand_tilde_str))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_urls") {
        cfg.deny_urls = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_extensions") {
        cfg.deny_extensions = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(Value::Array(v)) = table.get("allowed_write_dirs") {
        cfg.allowed_write_dirs = v
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
    if before.default_model != after.default_model {
        diffs.push(format!(
            "default_model: {} → {}",
            before.default_model, after.default_model
        ));
    }
    if before.ollama_host != after.ollama_host {
        diffs.push(format!(
            "ollama_host: {} → {}",
            before.ollama_host, after.ollama_host
        ));
    }
    if before.auto_approve != after.auto_approve {
        diffs.push(format!(
            "auto_approve: {} → {}",
            before.auto_approve, after.auto_approve
        ));
    }
    if before.bang_requires_approval != after.bang_requires_approval {
        diffs.push(format!(
            "bang_requires_approval: {} → {}",
            before.bang_requires_approval, after.bang_requires_approval
        ));
    }
    if before.dry_run != after.dry_run {
        diffs.push(format!("dry_run: {} → {}", before.dry_run, after.dry_run));
    }
    if before.cache_enabled != after.cache_enabled {
        diffs.push(format!(
            "cache_enabled: {} → {}",
            before.cache_enabled, after.cache_enabled
        ));
    }
    if before.sandbox_dir != after.sandbox_dir {
        diffs.push(format!(
            "sandbox_dir: {:?} → {:?}",
            before.sandbox_dir, after.sandbox_dir
        ));
    }
    if before.routing_enabled != after.routing_enabled {
        diffs.push(format!(
            "routing_enabled: {} → {}",
            before.routing_enabled, after.routing_enabled
        ));
    }
    if before.summarize_enabled != after.summarize_enabled {
        diffs.push(format!(
            "summarize_enabled: {} → {}",
            before.summarize_enabled, after.summarize_enabled
        ));
    }
    if before.reject_on_excess_plugin_trust != after.reject_on_excess_plugin_trust {
        diffs.push(format!(
            "reject_on_excess_plugin_trust: {} → {}",
            before.reject_on_excess_plugin_trust, after.reject_on_excess_plugin_trust
        ));
    }
    if before.plugin_signature_validation != after.plugin_signature_validation {
        diffs.push(format!(
            "plugin_signature_validation: {} → {}",
            before.plugin_signature_validation, after.plugin_signature_validation
        ));
    }
    if before.plugin_public_key_path != after.plugin_public_key_path {
        diffs.push(format!(
            "plugin_public_key_path: {:?} → {:?}",
            before.plugin_public_key_path, after.plugin_public_key_path
        ));
    }
    if before.memory_enabled != after.memory_enabled {
        diffs.push(format!(
            "memory_enabled: {} → {}",
            before.memory_enabled, after.memory_enabled
        ));
    }
    if before.memory_max_tokens != after.memory_max_tokens {
        diffs.push(format!(
            "memory_max_tokens: {} → {}",
            before.memory_max_tokens, after.memory_max_tokens
        ));
    }
    if before.memory_top_n != after.memory_top_n {
        diffs.push(format!(
            "memory_top_n: {} → {}",
            before.memory_top_n, after.memory_top_n
        ));
    }
    if before.checkpoint_interval_messages != after.checkpoint_interval_messages {
        diffs.push(format!(
            "checkpoint_interval_messages: {} → {}",
            before.checkpoint_interval_messages, after.checkpoint_interval_messages
        ));
    }
    if before.enabled_plugins != after.enabled_plugins {
        diffs.push(format!(
            "enabled_plugins: {:?} → {:?}",
            before.enabled_plugins, after.enabled_plugins
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
        assert_eq!(cfg.default_model, "qwen2.5:7b");

        set_env("KIRKFORGE_MODEL", Some("deepseek-v4:cloud"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.default_model, "deepseek-v4:cloud");
        set_env("KIRKFORGE_MODEL", None);
    }

    #[test]
    fn test_env_auto_approve_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.auto_approve);

        set_env("KIRKFORGE_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_auto_approve_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config {
            auto_approve: true,
            ..Default::default()
        };

        set_env("KIRKFORGE_AUTO_APPROVE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_dry_run_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.dry_run);

        set_env("KIRKFORGE_DRY_RUN", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.dry_run);
        set_env("KIRKFORGE_DRY_RUN", None);
    }

    #[test]
    fn test_env_dry_run_false() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config {
            dry_run: true,
            ..Default::default()
        };

        set_env("KIRKFORGE_DRY_RUN", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.dry_run);
        set_env("KIRKFORGE_DRY_RUN", None);
    }

    #[test]
    fn test_env_block_dotfiles() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_DOTFILES", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.block_dotfiles);
        set_env("KIRKFORGE_BLOCK_DOTFILES", None);
    }

    #[test]
    fn test_env_follow_symlinks() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.follow_symlinks);
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", None);
    }

    #[test]
    fn test_env_block_binary() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_BINARY", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.block_binary_reads);
        set_env("KIRKFORGE_BLOCK_BINARY", None);
    }

    #[test]
    fn test_env_max_read_size() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_READ_SIZE", Some("65536"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.max_file_read_size, 65536);
        set_env("KIRKFORGE_MAX_READ_SIZE", None);
    }

    #[test]
    fn test_env_bad_max_read_size_ignored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_READ_SIZE", Some("not-a-number"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.max_file_read_size, 1024 * 1024);
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

        assert_eq!(cfg.default_model, "custom-model");
        assert_eq!(cfg.max_file_read_size, 512);
        // Unset fields keep defaults
        assert_eq!(cfg.ollama_host, "http://localhost:11434");
        assert!(!cfg.auto_approve);
    }

    #[test]
    fn test_merge_toml_negative_max_read_size_is_ignored() {
        let mut cfg = Config::default();
        let default_size = cfg.max_file_read_size;
        let table: toml::Table = r#"
            max_file_read_size = -1
        "#
        .parse()
        .unwrap();
        merge_toml_into_config(&mut cfg, table);

        assert_eq!(
            cfg.max_file_read_size, default_size,
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

        assert_eq!(cfg.deny_paths.len(), 2);
        assert!(cfg.deny_paths.contains(&"**/.ssh/**".into()));
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
        assert!(cfg.sandbox_dir.is_none());
        let resolved = freeze_launch_sandbox(&mut cfg);
        // The test runner always has a cwd.
        assert!(resolved.is_some(), "test cwd is always present");
        let resolved = resolved.unwrap();
        assert_eq!(cfg.sandbox_dir.as_deref(), Some(resolved.as_str()));
    }

    /// The explicit-escape-hatch contract: if the operator set
    /// `sandbox_dir = Some("")` (or it was loaded from a config
    /// file that way), `freeze_launch_sandbox` must leave it alone.
    /// This is the policy that lets operators opt out of sandboxing.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_empty() {
        let mut cfg = Config {
            sandbox_dir: Some(String::new()),
            ..Config::default()
        };
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some(""));
        assert_eq!(cfg.sandbox_dir.as_deref(), Some(""));
    }

    /// If the operator set a real path (e.g. from a config file's
    /// `sandbox_dir = "/srv/project"`), the helper must not
    /// overwrite it with cwd. Operators win over defaults.
    #[test]
    fn test_freeze_launch_sandbox_does_not_overwrite_explicit_path() {
        let mut cfg = Config {
            sandbox_dir: Some("/srv/project".to_string()),
            ..Config::default()
        };
        let resolved = freeze_launch_sandbox(&mut cfg);
        assert_eq!(resolved.as_deref(), Some("/srv/project"));
        assert_eq!(cfg.sandbox_dir.as_deref(), Some("/srv/project"));
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
        let b = Config {
            default_model: "qwen2.5:3b".into(),
            ..Config::default()
        };
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("default_model"), "got: {s}");
        assert!(s.contains("→ qwen2.5:3b"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_multiple_fields() {
        let a = Config::default();
        let b = Config {
            default_model: "qwen2.5:3b".into(),
            auto_approve: true,
            ollama_host: "http://example.com:11434".into(),
            ..Config::default()
        };
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("default_model"), "got: {s}");
        assert!(s.contains("auto_approve"), "got: {s}");
        assert!(s.contains("ollama_host"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_ignores_internal_fields() {
        let a = Config::default();
        let b = Config {
            deny_paths: vec!["/secret".into()],
            allowed_write_dirs: vec!["/tmp".into()],
            ..Config::default()
        };
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
        assert!(cfg.reject_on_excess_plugin_trust);

        set_env("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.reject_on_excess_plugin_trust);
        set_env("KIRKFORGE_REJECT_ON_EXCESS_PLUGIN_TRUST", None);
    }

    #[test]
    fn test_env_plugin_signature_validation() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(!cfg.plugin_signature_validation);

        set_env("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.plugin_signature_validation);
        set_env("KIRKFORGE_PLUGIN_SIGNATURE_VALIDATION", None);
    }

    #[test]
    fn test_env_plugin_public_key_path() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH", Some("/tmp/key.pub"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.plugin_public_key_path.as_deref(), Some("/tmp/key.pub"));
        set_env("KIRKFORGE_PLUGIN_PUBLIC_KEY_PATH", None);
    }

    #[test]
    fn test_env_plugin_allowed_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_PLUGIN_ALLOWED_ENV_VARS", Some("FOO,BAR"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.plugin_allowed_env_vars, vec!["FOO", "BAR"]);
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

        assert!(!cfg.reject_on_excess_plugin_trust);
        assert!(cfg.plugin_signature_validation);
        assert_eq!(
            cfg.plugin_public_key_path.as_deref(),
            Some("/opt/kirkforge/plugin.pub")
        );
        assert_eq!(cfg.plugin_allowed_env_vars, vec!["CUSTOM_VAR"]);
    }

    #[test]
    fn test_env_memory_enabled() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        assert!(cfg.memory_enabled);

        set_env("KIRKFORGE_MEMORY_ENABLED", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.memory_enabled);
        set_env("KIRKFORGE_MEMORY_ENABLED", None);
    }

    #[test]
    fn test_env_memory_max_tokens() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MEMORY_MAX_TOKENS", Some("250"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.memory_max_tokens, 250);
        set_env("KIRKFORGE_MEMORY_MAX_TOKENS", None);
    }

    #[test]
    fn test_env_memory_top_n() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MEMORY_TOP_N", Some("5"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.memory_top_n, 5);
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

        assert!(!cfg.memory_enabled);
        assert_eq!(cfg.memory_max_tokens, 300);
        assert_eq!(cfg.memory_top_n, 3);
    }

    #[test]
    fn test_config_diff_summary_memory_knobs() {
        let a = Config::default();
        let b = Config {
            memory_enabled: false,
            memory_max_tokens: 250,
            memory_top_n: 5,
            ..Config::default()
        };
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
        assert_eq!(cfg.checkpoint_interval_messages, 20);
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
        assert_eq!(cfg.checkpoint_interval_messages, 15);
    }

    #[test]
    fn test_config_diff_summary_checkpoint_interval_messages() {
        let a = Config::default();
        let b = Config {
            checkpoint_interval_messages: 12,
            ..Config::default()
        };
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("checkpoint_interval_messages"), "got: {s}");
    }

    #[test]
    fn test_config_diff_summary_plugin_trust_knobs() {
        let a = Config::default();
        let b = Config {
            reject_on_excess_plugin_trust: false,
            plugin_signature_validation: true,
            plugin_public_key_path: Some("/tmp/key.pub".into()),
            ..Config::default()
        };
        let s = config_diff_summary(&a, &b);
        assert!(s.contains("reject_on_excess_plugin_trust"), "got: {s}");
        assert!(s.contains("plugin_signature_validation"), "got: {s}");
        assert!(s.contains("plugin_public_key_path"), "got: {s}");
    }
}
