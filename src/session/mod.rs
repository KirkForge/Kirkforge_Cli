pub mod config;
pub mod conversation;
pub mod executor;
pub mod prompt;

use crate::shared::{Config, SessionId};
use std::path::PathBuf;

/// Resolve the runtime data directory.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    let project = directories::ProjectDirs::from("", "", "kirkforge")
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data directory"))?;
    Ok(project.data_dir().to_path_buf())
}

/// Resolve the config file path.
pub fn config_path() -> PathBuf {
    let mut path = data_dir().unwrap_or_else(|_| PathBuf::from("."));
    path.push("config.toml");
    path
}

/// Load config from disk, or create default + write it.
pub fn load_or_create_config() -> Config {
    let path = config_path();
    if let Ok(content) = std::fs::read_to_string(&path) {
        toml::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Config parse error ({}), using defaults", e);
            Config::default()
        })
    } else {
        let cfg = Config::default();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, toml::to_string_pretty(&cfg).unwrap_or_default());
        cfg
    }
}

/// Generate a new session ID based on today's date.
pub fn new_session_id() -> SessionId {
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();
    SessionId { date, seq: 1 }
}