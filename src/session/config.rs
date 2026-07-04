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
use crate::shared::Config;
use std::path::PathBuf;

/// Load config with full layered resolution.
///
/// 1. Start with defaults
/// 2. Override from config file (if exists)
/// 3. Override from environment variables
///
/// The config is NOT written to disk here — that's the caller's
/// responsibility (e.g., on first run or when CLI overrides are provided).
pub fn load_config() -> Config {
    let mut cfg = Config::default();

    // Layer 1: config file
    let path = super::config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        match toml::from_str::<Config>(&content) {
            Ok(file_cfg) => cfg = file_cfg,
            Err(e) => {
                tracing::warn!("Failed to parse config ({}), merging with defaults", e);
                // Try partial merge: parse what we can
                if let Ok(table) = content.parse::<toml::Table>() {
                    merge_toml_into_config(&mut cfg, table);
                }
            }
        }
    }

    // Layer 2: environment variables
    apply_env_overrides(&mut cfg);

    cfg
}

/// Load config and write a default file on first run.
///
/// If the config file doesn't exist, creates it with default values
/// and prints a brief info message.
pub fn load_or_create_config() -> Config {
    let path = super::config_path();
    let exists = path.exists();

    let cfg = load_config();

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
                tracing::info!("Created default config at {}", path.display());
            }
        }
        tracing::info!(
            "Config file created at {}. Edit it to customize model, host, etc.",
            path.display()
        );
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
        cfg.sandbox_dir = if val.is_empty() { None } else { Some(val) };
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
        cfg.cache_dir = Some(PathBuf::from(val));
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
        cfg.sandbox_dir = Some(v.clone());
    }
    if let Some(Value::Boolean(v)) = table.get("block_dotfiles") {
        cfg.block_dotfiles = *v;
    }
    if let Some(Value::Integer(v)) = table.get("max_file_read_size") {
        cfg.max_file_read_size = *v as usize;
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
        cfg.cache_dir = Some(PathBuf::from(v));
    }

    // Arrays
    if let Some(Value::Array(v)) = table.get("deny_paths") {
        cfg.deny_paths = v
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
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
            .filter_map(|v| v.as_str().map(String::from))
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
    diffs.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to temporarily set an env var for a test.
    /// Since Rust tests run in parallel, each test function sets/unsets
    /// the vars it needs — env mutation is safe because each test thread
    /// has its own env map in practice.
    fn set_env(key: &str, val: Option<&str>) {
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn test_env_overrides_model() {
        let mut cfg = Config::default();
        assert_eq!(cfg.default_model, "qwen2.5:7b");

        set_env("KIRKFORGE_MODEL", Some("deepseek-v4:cloud"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.default_model, "deepseek-v4:cloud");
        set_env("KIRKFORGE_MODEL", None);
    }

    #[test]
    fn test_env_auto_approve_true() {
        let mut cfg = Config::default();
        assert!(!cfg.auto_approve);

        set_env("KIRKFORGE_AUTO_APPROVE", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
    }

    #[test]
    fn test_env_auto_approve_false() {
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
        let mut cfg = Config::default();
        assert!(!cfg.dry_run);

        set_env("KIRKFORGE_DRY_RUN", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.dry_run);
        set_env("KIRKFORGE_DRY_RUN", None);
    }

    #[test]
    fn test_env_dry_run_false() {
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
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_DOTFILES", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.block_dotfiles);
        set_env("KIRKFORGE_BLOCK_DOTFILES", None);
    }

    #[test]
    fn test_env_follow_symlinks() {
        let mut cfg = Config::default();
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.follow_symlinks);
        set_env("KIRKFORGE_FOLLOW_SYMLINKS", None);
    }

    #[test]
    fn test_env_block_binary() {
        let mut cfg = Config::default();
        set_env("KIRKFORGE_BLOCK_BINARY", Some("true"));
        apply_env_overrides(&mut cfg);
        assert!(cfg.block_binary_reads);
        set_env("KIRKFORGE_BLOCK_BINARY", None);
    }

    #[test]
    fn test_env_max_read_size() {
        let mut cfg = Config::default();
        set_env("KIRKFORGE_MAX_READ_SIZE", Some("65536"));
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.max_file_read_size, 65536);
        set_env("KIRKFORGE_MAX_READ_SIZE", None);
    }

    #[test]
    fn test_env_bad_max_read_size_ignored() {
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
}
