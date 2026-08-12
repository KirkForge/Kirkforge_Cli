// WO 28.1: access types now live in `shared::access` (pure data/logic).
// This re-export keeps every existing `crate::session::access::*` path working
// for session-internal and tui/main callers; tools/ repoints to shared directly.
pub use crate::shared::access;
pub mod adapter_swap;
pub mod bash_jobs;
pub mod bash_runner;
pub mod bench;
pub mod carryover;
pub mod config;
pub mod conversation;
pub mod error_recovery;
pub mod executor;
pub mod git_sanitation;
pub mod hooks;
pub mod mcp_client;
pub mod mcp_resource_tools;
pub mod mcp_tools;
pub mod memory;
pub mod plugin_ops;
pub mod plugin_tools;
pub mod process_group;
pub mod prompt;
pub mod replay;
pub mod router;
pub mod session_fork;
pub mod session_index;
pub mod skills;
// WO 28.1: InProcessTaskSpawner moved here from tools::task (it constructs the
// nested Executor — a session-layer concern). The TaskSpawner port stays in tools.
pub mod task_spawner;
pub mod toolset;
pub mod undo;
pub mod verifier;
pub mod worktree;

#[cfg(feature = "budget")]
pub mod budget;

#[cfg(feature = "stratum")]
pub mod stratum;

use crate::shared::SessionId;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Per-session stores for budget and Stratum offload (WO 22.6-R2).
/// Each session gets its own stores with LRU caps, replacing the old
/// process-global OnceLock pattern that accumulated entries forever.
pub struct SessionStores {
    #[cfg(feature = "budget")]
    pub budget: std::sync::Arc<std::sync::Mutex<kf_budget_core::TokenBudget>>,
    #[cfg(feature = "budget")]
    pub budget_store: std::sync::Arc<dyn kf_budget_core::OffloadStore>,
    #[cfg(feature = "stratum")]
    pub stratum_store: std::sync::Arc<kf_compress_core::store::InMemoryOffloadStore>,
}

#[cfg(test)]
pub(crate) fn test_data_dir_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Ensures the canonical data directory exists and is not world-readable.
/// Runs at most once per process to avoid repeated filesystem calls.
fn ensure_private_data_dir(dir: &std::path::Path) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!(error = %e, path = %dir.display(), "failed to create data directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                tracing::warn!(
                    error = %e,
                    path = %dir.display(),
                    "failed to set data directory permissions"
                );
            }
        }
    });
}

pub fn data_dir() -> anyhow::Result<PathBuf> {
    // Allow tests and advanced deployments to override the canonical data
    // directory location without changing XDG variables.
    if let Ok(dir) = std::env::var("KF_CODE_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let project = directories::ProjectDirs::from("", "", "kf-code")
        .ok_or_else(|| anyhow::anyhow!("Cannot determine data directory"))?;
    let dir = project.data_dir().to_path_buf();
    ensure_private_data_dir(&dir);
    Ok(dir)
}

pub fn jobs_dir() -> anyhow::Result<PathBuf> {
    let dir = data_dir()?.join("jobs");
    ensure_private_data_dir(&dir);
    Ok(dir)
}

pub fn config_path() -> PathBuf {
    let mut path = data_dir().unwrap_or_else(|e| {
        tracing::warn!(
            error = %e,
            "Cannot determine kf-code data directory; falling back to current directory for config.toml"
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

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::shared::test_util::remove_test_dir;

    #[tokio::test]
    async fn data_dir_respects_env_override() {
        let _lock = test_data_dir_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "kf-code-data-dir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("KF_CODE_DATA_DIR", &temp);
        let dir = data_dir().expect("data_dir should succeed");
        assert_eq!(dir, temp);
        std::env::remove_var("KF_CODE_DATA_DIR");
        remove_test_dir(&temp);
    }

    #[tokio::test]
    async fn new_session_id_picks_max_seq_plus_one() {
        let _lock = test_data_dir_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "kf-code-session-id-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sessions_dir = temp.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let date = chrono::Local::now().format("%Y-%m-%d").to_string();
        // Create some session files with different seq numbers
        std::fs::write(sessions_dir.join(format!("{date}-session-1.ndjson")), "").unwrap();
        std::fs::write(sessions_dir.join(format!("{date}-session-7.ndjson")), "").unwrap();
        std::fs::write(sessions_dir.join(format!("{date}-session-3.ndjson")), "").unwrap();
        std::env::set_var("KF_CODE_DATA_DIR", &temp);
        let id = new_session_id();
        assert_eq!(id.date, date);
        assert_eq!(id.seq, 8, "should pick max(1,3,7) + 1 = 8");
        std::env::remove_var("KF_CODE_DATA_DIR");
        remove_test_dir(&temp);
    }

    #[tokio::test]
    async fn new_session_id_returns_1_for_empty_dir() {
        let _lock = test_data_dir_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "kf-code-session-empty-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let sessions_dir = temp.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::env::set_var("KF_CODE_DATA_DIR", &temp);
        let id = new_session_id();
        assert_eq!(id.seq, 1, "empty sessions dir should return seq=1");
        std::env::remove_var("KF_CODE_DATA_DIR");
        remove_test_dir(&temp);
    }

    #[tokio::test]
    async fn new_session_id_returns_1_when_no_sessions_dir() {
        let _lock = test_data_dir_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "kf-code-no-sessions-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        // No "sessions" subdirectory
        std::env::set_var("KF_CODE_DATA_DIR", &temp);
        let id = new_session_id();
        assert_eq!(id.seq, 1, "no sessions dir should return seq=1");
        std::env::remove_var("KF_CODE_DATA_DIR");
        remove_test_dir(&temp);
    }

    #[tokio::test]
    async fn jobs_dir_respects_env_override() {
        let _lock = test_data_dir_lock().lock().await;
        let temp = std::env::temp_dir().join(format!(
            "kf-code-jobs-dir-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("KF_CODE_DATA_DIR", &temp);
        let dir = jobs_dir().expect("jobs_dir should succeed");
        assert!(dir.ends_with("jobs"));
        std::env::remove_var("KF_CODE_DATA_DIR");
        remove_test_dir(&temp);
    }
}
