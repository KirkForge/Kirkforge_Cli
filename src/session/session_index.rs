// Public/future surface in a binary crate: suppress dead-code warnings for pub items.
#![allow(dead_code)]

//! Session index — list, prune, and delete prior sessions.
//!
//! Review.md gap #3: prior to this, the only way to find an old
//! session was to `ls ~/.local/share/kirkforge/sessions/` and open the
//! NDJSON by hand. There was no `/sessions` command in the TUI and
//! `--continue <id>` was just "open this path verbatim."
//!
//! This module reads the sessions directory and returns a structured
//! summary per file: id, started_at (file mtime), message count, size.
//! The TUI's `/sessions` command consumes the list and formats a
//! table. `/sessions prune N` deletes the oldest N keeping the K
//! most recent; `/sessions delete <id>` removes one. Both prune and
//! delete are local file ops — they don't touch any in-memory state.
//!
//! # Format assumption
//!
//! The session file is NDJSON: one JSON value per line. The
//! `Message` struct in `shared::Message` doesn't carry a `timestamp`
//! field, so we don't try to parse a session start time out of the
//! log content — we use the file's mtime instead. `message_count` is
//! the count of non-empty lines.
//!
//! The id is derived from the filename (`<id>.conv.ndjson` → `<id>`)
//! so the listing matches what the user sees in `kirkforge run
//! --continue <id>`.

use crate::session::conversation::ConversationLog;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One row in the sessions listing. Display-only — the `id` is the
/// filename stem, `path` is the absolute path (for `--continue` or
/// "open in editor"), and the other fields are summary metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub path: PathBuf,
    /// Wall-clock timestamp of the file's mtime, in rfc3339.
    /// Display only.
    pub started_at: String,
    /// Number of non-empty lines in the NDJSON log.
    pub message_count: usize,
    /// File size in bytes.
    pub size_bytes: u64,
}

/// Enumerate sessions in `<data_dir>/sessions/`, sorted newest-first
/// by `started_at`.
///
/// Sessions whose files fail to stat are silently skipped — the
/// listing is best-effort and a single corrupt file shouldn't
/// break `/sessions` for the rest.
pub fn list_sessions() -> anyhow::Result<Vec<SessionEntry>> {
    let data_dir = crate::session::data_dir()?;
    let sessions_dir = data_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(&sessions_dir)?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // Match `*.conv.ndjson` (the convention `main.rs` uses when
        // creating new sessions). Other files in the dir — forks,
        // unrelated state — are ignored.
        let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        if !fname.ends_with(".conv.ndjson") {
            continue;
        }
        if let Some(summary) = summarize_file(&path) {
            out.push(summary);
        }
    }
    // Newest first (lexicographic on the rfc3339 string works for
    // same-timezone timestamps; we don't need strict correctness
    // across timezones for a listing). Tie-break on the session id so
    // files created within the same second (common in tests and on
    // low-resolution filesystems) still have a deterministic order.
    out.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(out)
}

/// Read a single session file and produce a `SessionEntry`.
///
/// `None` is returned for files that can't be stat'd. A file with
/// zero parseable lines still produces a `Some` entry — it just has
/// `message_count = 0` and a fallback `started_at` from mtime.
fn summarize_file(path: &std::path::Path) -> Option<SessionEntry> {
    let id = path
        .file_stem()?
        .to_string_lossy()
        .trim_end_matches(".conv")
        .to_string();
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to stat session file");
            return None;
        }
    };
    let size_bytes = metadata.len();

    // Count non-empty lines. Cheap, no JSON parse needed for the
    // count.
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read session file");
            return None;
        }
    };
    let message_count = content.lines().filter(|l| !l.trim().is_empty()).count();

    // started_at: file mtime in rfc3339 form. If the mtime is
    // unparseable (very rare — epoch on some FSes) we use the
    // literal string "unknown" so the listing still renders.
    let started_at = metadata
        .modified()
        .ok()
        .map(|t| {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_else(|| "unknown".to_string());

    Some(SessionEntry {
        id,
        path: path.to_path_buf(),
        started_at,
        message_count,
        size_bytes,
    })
}

/// Delete a single session by id (the filename stem without the
/// `.conv.ndjson` suffix). Returns `Ok(true)` if a file was
/// removed, `Ok(false)` if no matching file existed.
pub fn delete_session(id: &str) -> anyhow::Result<bool> {
    let data_dir = crate::session::data_dir()?;
    let path = data_dir.join("sessions").join(format!("{id}.conv.ndjson"));
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&path)?;
    Ok(true)
}

/// Prune the oldest N sessions, keeping the K most recent.
///
/// `keep` is the number of sessions to preserve (default 10 in the
/// TUI command). `delete_count` is the number to remove (the oldest
/// ones). Returns the list of deleted ids for the user-facing
/// confirmation message. If there are fewer than `keep + delete_count`
/// sessions, nothing is removed.
pub fn prune_oldest(keep: usize, delete_count: usize) -> anyhow::Result<Vec<String>> {
    let sessions_dir = crate::session::data_dir()?.join("sessions");
    prune_oldest_in_dir(&sessions_dir, keep, delete_count)
}

/// Internal variant that works on an explicit directory so tests can
/// stay isolated from the user's real `~/.local/share/kirkforge`.
fn prune_oldest_in_dir(
    sessions_dir: &std::path::Path,
    keep: usize,
    delete_count: usize,
) -> anyhow::Result<Vec<String>> {
    let mut entries = Vec::new();
    if sessions_dir.is_dir() {
        for entry in std::fs::read_dir(sessions_dir)?.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            if !fname.ends_with(".conv.ndjson") {
                continue;
            }
            if let Some(summary) = summarize_file(&path) {
                entries.push(summary);
            }
        }
    }

    // Newest first, matching list_sessions(). Tie-break on id for
    // deterministic behaviour when mtimes are equal.
    entries.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });

    if entries.len() <= keep + delete_count {
        return Ok(Vec::new());
    }
    let to_delete = &entries[keep..keep + delete_count];
    let mut deleted = Vec::with_capacity(to_delete.len());
    for e in to_delete {
        if std::fs::remove_file(&e.path).is_ok() {
            deleted.push(e.id.clone());
        }
    }
    Ok(deleted)
}

/// Resolve a session id (or id prefix) to a full path. Used by
/// `/resume <id>` so the user can type `2026-06-10-session-01` or
/// just `2026-06-10-01`.
///
/// The match is exact first, then prefix. With multiple prefix
/// matches the newest is returned (the listing is sorted
/// newest-first, so the first prefix match is the newest).
/// Returns `None` if no match.
pub fn resolve_session_id(id_or_prefix: &str) -> anyhow::Result<Option<PathBuf>> {
    let entries = list_sessions()?;
    // Exact match wins.
    for e in &entries {
        if e.id == id_or_prefix {
            return Ok(Some(e.path.clone()));
        }
    }
    // Prefix match — pick the first (newest) one.
    for e in &entries {
        if e.id.starts_with(id_or_prefix) {
            return Ok(Some(e.path.clone()));
        }
    }
    Ok(None)
}

/// Open the resolved session as a `ConversationLog`. Used by
/// `/resume <id>` and `--continue <id>`. Returns `Ok(None)` if no
/// session matches the id or prefix.
pub fn open_resolved(id_or_prefix: &str) -> anyhow::Result<Option<ConversationLog>> {
    if let Some(path) = resolve_session_id(id_or_prefix)? {
        let (log, _outcome) = ConversationLog::open(path)?;
        Ok(Some(log))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `list_sessions` should not panic on a missing or empty
    /// sessions dir.
    #[test]
    fn test_list_sessions_no_panic_on_empty() {
        let entries = list_sessions().unwrap_or_default();
        let _ = entries;
    }

    /// `summarize_file` reads the file's mtime and counts non-empty
    /// lines. The id is the file stem with `.conv` stripped.
    #[test]
    fn test_summarize_file_with_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("2026-06-10-session-01.conv.ndjson");
        std::fs::write(&path, "{\"role\":\"user\",\"content\":\"hi\"}\n\n{\"role\":\"assistant\",\"content\":\"hello\"}\n").unwrap();

        let entry = summarize_file(&path).expect("summarize");
        assert_eq!(entry.id, "2026-06-10-session-01");
        assert_eq!(entry.message_count, 2);
        assert!(entry.size_bytes > 0);
        assert!(!entry.started_at.is_empty());
    }

    /// Empty file still produces an entry with `message_count = 0`.
    #[test]
    fn test_summarize_file_empty_has_zero_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.conv.ndjson");
        std::fs::write(&path, b"").unwrap();

        let entry = summarize_file(&path).expect("summarize");
        assert_eq!(entry.id, "empty");
        assert_eq!(entry.message_count, 0);
    }

    /// `summarize_file` on a missing path returns None.
    #[test]
    fn test_summarize_file_missing_returns_none() {
        let path = std::path::Path::new("/nonexistent/path/kf_test_x.conv.ndjson");
        assert!(summarize_file(path).is_none());
    }

    /// `prune_oldest_in_dir` with delete count larger than the actual
    /// surplus is a no-op.
    #[test]
    fn test_prune_oldest_noop_when_few_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        // 0 sessions on disk → nothing to delete.
        let deleted = prune_oldest_in_dir(&sessions_dir, 10, 5).unwrap();
        assert!(deleted.is_empty());
    }

    /// `prune_oldest_in_dir` deletes the oldest sessions beyond `keep`.
    #[test]
    fn test_prune_oldest_deletes_oldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ids = [
            "2026-06-01-session-01",
            "2026-06-02-session-02",
            "2026-06-03-session-03",
        ];
        for id in ids {
            std::fs::write(sessions_dir.join(format!("{id}.conv.ndjson")), b"").unwrap();
        }

        // Keep 1, delete 1 → the oldest of the two beyond keep is removed.
        let deleted = prune_oldest_in_dir(&sessions_dir, 1, 1).unwrap();
        assert_eq!(deleted, vec!["2026-06-02-session-02".to_string()]);
        assert!(!sessions_dir
            .join("2026-06-02-session-02.conv.ndjson")
            .exists());
        assert!(sessions_dir
            .join("2026-06-03-session-03.conv.ndjson")
            .exists());
    }
}
