pub mod access;
pub mod bash_jobs;
pub mod carryover;
pub mod config;
pub mod conversation;
pub mod event_bus;
pub mod executor;
pub mod prompt;
pub mod session_fork;
pub mod skills;
pub mod verifier;

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
///
/// Scans the sessions directory for existing sessions with today's
/// date to find the next available sequence number, avoiding log-file
/// collisions when multiple sessions run on the same day.
pub fn new_session_id() -> SessionId {
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();

    // Scan sessions directory to find the highest existing seq for today
    let next_seq = if let Ok(data_dir) = data_dir() {
        let sessions_dir = data_dir.join("sessions");
        if sessions_dir.is_dir() {
            let prefix = date.to_string(); // "YYYY-MM-DD"
            let mut max_seq: u32 = 0;
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name();
                    let fname = fname.to_string_lossy();
                    // Match pattern: "YYYY-MM-DD-session-NN.conv.ndjson"
                    if let Some(rest) = fname.strip_prefix(&format!("{}-session-", prefix)) {
                        if let Some(seq_str) = rest.split('.').next() {
                            if let Ok(seq) = seq_str.parse::<u32>() {
                                if seq > max_seq {
                                    max_seq = seq;
                                }
                            }
                        }
                    }
                }
            }
            max_seq + 1
        } else {
            1
        }
    } else {
        1
    };

    SessionId {
        date,
        seq: next_seq,
    }
}
