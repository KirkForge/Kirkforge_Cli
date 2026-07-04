// Public/future API surface for upcoming phases; submodules expose symbols used by later work.
#![allow(dead_code)]

pub mod access;
pub mod adapter_swap;
pub mod bash_jobs;
pub mod bash_runner;
pub mod carryover;
pub mod config;
pub mod conversation;
pub mod error_recovery;
pub mod event_bus;
pub mod executor;
pub mod git_sanitation;
pub mod hooks;
pub mod mcp_client;
pub mod mcp_tools;
pub mod memory;
pub mod plugin_tools;
pub mod process_group;
pub mod prompt;
pub mod router;
pub mod session_fork;
pub mod session_index;
pub mod skills;
pub mod toolset;
pub mod undo;
pub mod verifier;

use crate::shared::SessionId;
use std::path::PathBuf;

#[cfg(test)]
pub(crate) fn test_data_dir_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub fn data_dir() -> anyhow::Result<PathBuf> {
    // Allow tests and advanced deployments to override the canonical data
    // directory location without changing XDG variables.
    if let Ok(dir) = std::env::var("KIRKFORGE_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let project = directories::ProjectDirs::from("", "", "kirkforge")
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data directory"))?;
    Ok(project.data_dir().to_path_buf())
}

pub fn config_path() -> PathBuf {
    let mut path = data_dir().unwrap_or_else(|e| {
        tracing::warn!(
            error = %e,
            "Cannot determine kirkforge data directory; falling back to current directory for config.toml"
        );
        PathBuf::from(".")
    });
    path.push("config.toml");
    path
}

pub fn new_session_id() -> SessionId {
    let now = chrono::Local::now();
    let date = now.format("%Y-%m-%d").to_string();

    let next_seq = if let Ok(data_dir) = data_dir() {
        let sessions_dir = data_dir.join("sessions");
        if sessions_dir.is_dir() {
            let prefix = date.to_string(); // "YYYY-MM-DD"
            let mut max_seq: u32 = 0;
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name();
                    let fname = fname.to_string_lossy();

                    if let Some(rest) = fname.strip_prefix(&format!("{prefix}-session-")) {
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
