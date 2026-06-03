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
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(content) = toml::to_string_pretty(&cfg) {
            let _ = std::fs::write(&path, content);
            tracing::info!(
                "Created default config at {}",
                path.display()
            );
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
    Ok(())
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

    // Arrays
    if let Some(Value::Array(v)) = table.get("deny_paths") {
        cfg.deny_paths = v.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_urls") {
        cfg.deny_urls = v.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
    if let Some(Value::Array(v)) = table.get("deny_extensions") {
        cfg.deny_extensions = v.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
    if let Some(Value::Array(v)) = table.get("allowed_write_dirs") {
        cfg.allowed_write_dirs = v.iter().filter_map(|v| v.as_str().map(String::from)).collect();
    }
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
        assert_eq!(cfg.default_model, "glm-5.1:cloud");

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
        let mut cfg = Config::default();
        cfg.auto_approve = true;

        set_env("KIRKFORGE_AUTO_APPROVE", Some("false"));
        apply_env_overrides(&mut cfg);
        assert!(!cfg.auto_approve);
        set_env("KIRKFORGE_AUTO_APPROVE", None);
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
}