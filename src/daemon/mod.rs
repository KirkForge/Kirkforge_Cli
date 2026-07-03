//! Session daemon — lightweight background process that tracks the last
//! few sessions and provides a fast resume path.
//!
//! The daemon does **not** run the TUI or the executor. It owns only
//! session metadata: which `*.conv.ndjson` files exist, which are the
//! most recent, and how to resolve a short id/prefix to a full path.
//!
//! Communication is line-delimited JSON over a Unix domain socket.

pub mod client;
pub mod paths;

#[cfg(unix)]
pub mod server;

use crate::session::session_index::SessionEntry;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Maximum number of recent sessions the daemon remembers.
pub const RECENT_SESSIONS_LIMIT: usize = 5;

/// A request sent from a client to the daemon.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op")]
pub enum Request {
    /// Health check.
    #[serde(rename = "ping")]
    Ping,

    /// Return the last `RECENT_SESSIONS_LIMIT` sessions, newest first.
    #[serde(rename = "list")]
    List,

    /// Resolve a session id or prefix to a log path.
    #[serde(rename = "resolve")]
    Resolve { id: String },

    /// Mark a session as recently used.
    #[serde(rename = "touch")]
    Touch { id: String, path: PathBufSerde },

    /// Ask the daemon to shut down gracefully.
    #[serde(rename = "shutdown")]
    Shutdown,
}

/// A response sent from the daemon back to a client.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "status")]
pub enum Response {
    #[serde(rename = "ok")]
    Ok { data: Option<serde_json::Value> },
    #[serde(rename = "error")]
    Error { message: String },
}

impl Response {
    pub fn ok_empty() -> Self {
        Response::Ok { data: None }
    }

    pub fn ok_json(value: serde_json::Value) -> Self {
        Response::Ok { data: Some(value) }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Response::Error {
            message: message.into(),
        }
    }
}

/// Wrapper so `PathBuf` serializes nicely in JSON without needing a custom
/// module on every use.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(transparent)]
pub struct PathBufSerde {
    pub path: std::path::PathBuf,
}

impl From<std::path::PathBuf> for PathBufSerde {
    fn from(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

impl From<PathBufSerde> for std::path::PathBuf {
    fn from(p: PathBufSerde) -> Self {
        p.path
    }
}

/// In-memory state kept by the daemon.
#[derive(Debug, Clone, Default)]
pub struct DaemonState {
    /// Last N sessions, newest at the front.
    pub recent: VecDeque<SessionEntry>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            recent: VecDeque::with_capacity(RECENT_SESSIONS_LIMIT + 1),
        }
    }

    /// Refresh the recent list from disk.
    pub fn refresh(&mut self) {
        match crate::session::session_index::list_sessions() {
            Ok(entries) => {
                self.recent = entries.into_iter().take(RECENT_SESSIONS_LIMIT).collect();
            }
            Err(e) => {
                tracing::warn!(error = %e, "daemon failed to list sessions");
            }
        }
    }

    /// Move the touched session to the front, or refresh from disk if it
    /// isn't already known.
    pub fn touch(&mut self, id: &str, path: std::path::PathBuf) {
        if let Some(idx) = self.recent.iter().position(|e| e.id == id) {
            let mut entry = self.recent.remove(idx).unwrap_or_else(|| SessionEntry {
                id: id.to_string(),
                path: path.clone(),
                started_at: chrono::Local::now().to_rfc3339(),
                message_count: 0,
                size_bytes: 0,
            });
            entry.path = path;
            self.recent.push_front(entry);
        } else {
            // Don't know this session yet — refresh from disk so we keep
            // the existing metadata if it exists.
            self.refresh();
            if !self.recent.iter().any(|e| e.id == id) {
                self.recent.push_front(SessionEntry {
                    id: id.to_string(),
                    path,
                    started_at: chrono::Local::now().to_rfc3339(),
                    message_count: 0,
                    size_bytes: 0,
                });
            }
        }
        while self.recent.len() > RECENT_SESSIONS_LIMIT {
            self.recent.pop_back();
        }
    }

    /// Resolve an id or prefix against the in-memory recent list.
    /// Falls back to a full disk scan if no recent match.
    pub fn resolve(&self, id_or_prefix: &str) -> Option<SessionEntry> {
        // Exact match in recent list.
        for e in &self.recent {
            if e.id == id_or_prefix {
                return Some(e.clone());
            }
        }
        // Prefix match in recent list (newest first already).
        for e in &self.recent {
            if e.id.starts_with(id_or_prefix) {
                return Some(e.clone());
            }
        }
        // Full disk fallback.
        match crate::session::session_index::resolve_session_id(id_or_prefix) {
            Ok(Some(path)) => {
                let id = path
                    .file_stem()
                    .and_then(|f| f.to_str())
                    .unwrap_or(id_or_prefix)
                    .trim_end_matches(".conv")
                    .to_string();
                Some(SessionEntry {
                    id,
                    path,
                    started_at: chrono::Local::now().to_rfc3339(),
                    message_count: 0,
                    size_bytes: 0,
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_string(),
            path: std::path::PathBuf::from(path),
            started_at: chrono::Local::now().to_rfc3339(),
            message_count: 1,
            size_bytes: 100,
        }
    }

    /// Set `KIRKFORGE_DATA_DIR` to an empty temporary directory for the
    /// duration of the test. Returns the temp dir and the previous env
    /// value so it can be restored.
    fn with_empty_data_dir() -> (tempfile::TempDir, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path());
        (dir, previous)
    }

    fn restore_data_dir(previous: Option<String>) {
        match previous {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
        }
    }

    #[test]
    fn touch_moves_entry_to_front() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let (_dir, previous) = with_empty_data_dir();

        let mut state = DaemonState::new();
        state.recent.push_back(entry("a", "/a"));
        state.recent.push_back(entry("b", "/b"));
        state.recent.push_back(entry("c", "/c"));

        state.touch("b", std::path::PathBuf::from("/b2"));
        assert_eq!(state.recent[0].id, "b");
        assert_eq!(state.recent[0].path, std::path::PathBuf::from("/b2"));
        assert_eq!(state.recent.len(), 3);

        restore_data_dir(previous);
    }

    #[test]
    fn touch_adds_unknown_entry() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let (_dir, previous) = with_empty_data_dir();

        let mut state = DaemonState::new();
        state.recent.push_back(entry("a", "/a"));

        state.touch("x", std::path::PathBuf::from("/x"));
        assert_eq!(state.recent[0].id, "x");
        // Unknown entries refresh from disk, so the synthetic in-memory "a"
        // is replaced by whatever is on disk (nothing, in the empty temp dir).
        assert_eq!(state.recent.len(), 1);

        restore_data_dir(previous);
    }

    #[test]
    fn recent_list_is_capped() {
        use std::time::Duration;

        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let (dir, previous) = with_empty_data_dir();

        // Create real session files so the daemon's refresh sees them.
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        for i in 0..10 {
            let path = sessions_dir.join(format!("s{i}.conv.ndjson"));
            std::fs::write(&path, "").unwrap();
            // Stagger mtimes so the listing order is predictable.
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut state = DaemonState::new();
        for i in 0..10 {
            let path = sessions_dir.join(format!("s{i}.conv.ndjson"));
            state.touch(&format!("s{i}"), path);
        }
        assert_eq!(state.recent.len(), RECENT_SESSIONS_LIMIT);
        assert_eq!(state.recent[0].id, "s9");

        restore_data_dir(previous);
    }
}
