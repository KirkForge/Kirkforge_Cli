/// Session forking — conversation branching and checkpoint management.
///
/// A fork is a copy of the conversation at a specific point in time,
/// allowing the user or model to explore alternative paths without
/// losing the original conversation.
///
/// Forks are stored on disk as separate NDJSON files alongside the
/// parent session, and can be resumed independently.
use crate::session::conversation::ConversationLog;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Metadata about a session fork.
#[derive(Debug, Clone, Serialize)]
pub struct Fork {
    /// Fork identifier (e.g. "fork-01").
    pub id: String,
    /// Human-readable label.
    pub label: String,
    /// Path to the fork's NDJSON log file.
    pub path: PathBuf,
    /// Index in the parent conversation where this fork diverges.
    pub fork_point: usize,
    /// Timestamp when the fork was created.
    pub created_at: String,
    /// Parent session ID.
    pub parent_session: String,
}

/// Manages session forks for a given conversation log.
pub struct ForkManager {
    session_id: String,
    base_path: PathBuf,
    forks: Vec<Fork>,
}

impl ForkManager {
    /// Create a new fork manager for a session.
    ///
    /// `session_id` — display name like "2026-06-03-session-01"
    /// `log_path` — path to the session's NDJSON log file
    pub fn new(session_id: &str, log_path: &Path) -> Self {
        let forks_dir = log_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("forks");
        Self {
            session_id: session_id.to_string(),
            base_path: forks_dir,
            forks: Vec::new(),
        }
    }

    /// Create a fork at the current state of the conversation.
    ///
    /// `label` — human-readable description of the fork
    /// `parent_conversation` — the current conversation to fork from
    /// `fork_point` — message index where this fork diverges (-1 = end)
    pub fn create_fork(
        &mut self,
        label: &str,
        parent_conversation: &ConversationLog,
        fork_point: i64,
    ) -> anyhow::Result<Fork> {
        let fork_num = self.forks.len() + 1;
        let fork_id = format!("fork-{:02}", fork_num);
        let fork_dir = self.base_path.join(&fork_id);
        std::fs::create_dir_all(&fork_dir)?;

        let fork_path = fork_dir.join("conversation.ndjson");
        let mut fork_log = ConversationLog::open(fork_path.clone())?;

        // Copy messages up to the fork point
        let all_msgs = parent_conversation.all();
        let end_idx = if fork_point < 0 || fork_point as usize >= all_msgs.len() {
            all_msgs.len()
        } else {
            fork_point as usize
        };

        for msg in all_msgs.iter().take(end_idx) {
            fork_log.append(msg.clone())?;
        }

        let fork = Fork {
            id: fork_id,
            label: label.to_string(),
            path: fork_path,
            fork_point: end_idx,
            created_at: chrono::Local::now().to_rfc3339(),
            parent_session: self.session_id.clone(),
        };

        self.forks.push(fork.clone());
        // Persist fork metadata
        let meta_path = fork_dir.join("fork.json");
        if let Ok(json) = serde_json::to_string_pretty(&fork) {
            let _ = std::fs::write(&meta_path, json);
        }

        Ok(fork)
    }

    /// List all forks for this session.
    pub fn list(&self) -> &[Fork] {
        &self.forks
    }

    /// Get a specific fork by ID.
    pub fn get(&self, id: &str) -> Option<&Fork> {
        self.forks.iter().find(|f| f.id == id)
    }

    /// Open a fork's conversation log for reading/resuming.
    pub fn open_fork(&self, id: &str) -> anyhow::Result<ConversationLog> {
        let fork = self
            .forks
            .iter()
            .find(|f| f.id == id)
            .ok_or_else(|| anyhow::anyhow!("Fork '{}' not found", id))?;
        ConversationLog::open(fork.path.clone())
    }

    /// Delete a fork and its files.
    pub fn delete_fork(&mut self, id: &str) -> anyhow::Result<bool> {
        let idx = self
            .forks
            .iter()
            .position(|f| f.id == id)
            .ok_or_else(|| anyhow::anyhow!("Fork '{}' not found", id))?;

        let fork = self.forks.remove(idx);
        let fork_dir = fork.path.parent().unwrap_or(Path::new("."));
        let _ = std::fs::remove_dir_all(fork_dir);
        Ok(true)
    }

    /// Number of forks.
    pub fn len(&self) -> usize {
        self.forks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.forks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fork_manager_creation() {
        let mgr = ForkManager::new("test-session", Path::new("/tmp/test-log.ndjson"));
        assert!(mgr.is_empty());
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn test_create_and_list_fork() {
        let dir = std::env::temp_dir().join("kirkforge_fork_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("session.conv.ndjson");
        let mut log = ConversationLog::open(log_path.clone()).unwrap();

        // Add some messages
        use crate::shared::{Message, Role};
        log.append(Message {
            role: Role::User,
            content: "hello".into(),
            ..Default::default()
        })
        .unwrap();
        log.append(Message {
            role: Role::Assistant,
            content: "hi there".into(),
            ..Default::default()
        })
        .unwrap();

        let mut mgr = ForkManager::new("test-session", &log_path);
        assert_eq!(mgr.len(), 0);

        let fork = mgr
            .create_fork("test-fork", &log, -1)
            .expect("should create fork");
        assert_eq!(fork.label, "test-fork");
        assert!(fork.id.starts_with("fork-"));

        assert_eq!(mgr.len(), 1);
        assert_eq!(mgr.list().len(), 1);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_get_fork_by_id() {
        let dir = std::env::temp_dir().join("kirkforge_fork_get_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("session.conv.ndjson");
        let log = ConversationLog::open(log_path.clone()).unwrap();
        let mut mgr = ForkManager::new("test", &log_path);
        let fork = mgr.create_fork("get-test", &log, -1).unwrap();

        let found = mgr.get(&fork.id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, fork.id);

        let not_found = mgr.get("nonexistent");
        assert!(not_found.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_delete_fork() {
        let dir = std::env::temp_dir().join("kirkforge_fork_del_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let log_path = dir.join("session.conv.ndjson");
        let log = ConversationLog::open(log_path.clone()).unwrap();
        let mut mgr = ForkManager::new("test", &log_path);
        let fork = mgr.create_fork("del-me", &log, -1).unwrap();

        assert_eq!(mgr.len(), 1);
        assert!(mgr.delete_fork(&fork.id).is_ok());
        assert_eq!(mgr.len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}