use crate::shared::Config;

/// Load or create the TOML config file.
/// Config lives at ~/.local/share/kirkforge/config.toml
pub fn load_config() -> Config {
    let path = super::config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        toml::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse config ({}), using defaults", e);
            Config::default()
        })
    } else {
        let cfg = Config::default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(content) = toml::to_string_pretty(&cfg) {
            let _ = std::fs::write(&path, content);
        }
        cfg
    }
}

/// Write config to disk.
pub fn save_config(config: &Config) -> anyhow::Result<()> {
    let path = super::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}