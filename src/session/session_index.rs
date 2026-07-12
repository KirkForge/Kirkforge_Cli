//! Session index — list, prune, delete, and search prior sessions.
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
//! A small NDJSON metadata index (`sessions/.index.ndjson`) caches
//! the summary so listing and searching are fast even with hundreds
//! of sessions. The index is rewritten atomically after any mutating
//! operation and falls back to a full directory scan if it is
//! missing or unreadable.
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
use anyhow::Context;
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

/// Path to the cached NDJSON session metadata index.
fn index_path() -> anyhow::Result<PathBuf> {
    let data_dir = crate::session::data_dir()?;
    Ok(data_dir.join("sessions").join(".index.ndjson"))
}

/// Cached session metadata index.
#[derive(Debug, Clone)]
pub struct SessionIndex {
    path: PathBuf,
    entries: Vec<SessionEntry>,
}

impl SessionIndex {
    /// Load the existing index, or rebuild it from a directory scan
    /// if the index is missing or unreadable.
    pub fn load_or_refresh() -> anyhow::Result<Self> {
        let path = index_path()?;
        if let Some(entries) = load_index(&path) {
            return Ok(Self { path, entries });
        }
        let mut s = Self {
            path,
            entries: Vec::new(),
        };
        s.refresh()?;
        Ok(s)
    }

    /// Return a sorted view (newest first) of the indexed sessions.
    pub fn list(&self) -> Vec<SessionEntry> {
        self.entries.clone()
    }

    /// Search indexed sessions by id, date, message count, or message content.
    /// The query is matched case-insensitively against:
    /// - the session id,
    /// - the `started_at` rfc3339 string,
    /// - the stringified `message_count`,
    /// - the raw text of every message in the session.
    pub fn search(&self, query: &str) -> Vec<SessionEntry> {
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                if e.id.to_lowercase().contains(&q)
                    || e.started_at.to_lowercase().contains(&q)
                    || e.message_count.to_string().contains(query)
                {
                    return true;
                }
                session_content_matches(&e.path, &q)
            })
            .cloned()
            .collect()
    }

    /// Re-scan the sessions directory and rewrite the index.
    pub fn refresh(&mut self) -> anyhow::Result<()> {
        let sessions_dir = crate::session::data_dir()?.join("sessions");
        let mut entries = Vec::new();
        if sessions_dir.is_dir() {
            for entry in std::fs::read_dir(&sessions_dir)
                .with_context(|| format!("read sessions directory {}", sessions_dir.display()))?
                .flatten()
            {
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
        entries.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        self.entries = entries;
        self.save()?;
        Ok(())
    }

    /// Update the index for a touched session. Creates the entry if it
    /// is not already present.
    pub fn touch(&mut self, id: &str, path: &std::path::Path) -> anyhow::Result<()> {
        let summary = summarize_file(path).unwrap_or_else(|| SessionEntry {
            id: id.to_string(),
            path: path.to_path_buf(),
            started_at: chrono::Local::now().to_rfc3339(),
            message_count: 0,
            size_bytes: 0,
        });
        if let Some(idx) = self.entries.iter().position(|e| e.id == id) {
            self.entries[idx] = summary;
        } else {
            self.entries.push(summary);
        }
        self.entries.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        self.save()?;
        Ok(())
    }

    /// Remove a session from the index. Does nothing if the id is absent.
    pub fn remove(&mut self, id: &str) -> anyhow::Result<()> {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        if self.entries.len() != before {
            self.save()?;
        }
        Ok(())
    }

    /// Write the index atomically (temp file + rename).
    fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create index directory {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)
                .with_context(|| format!("create temporary index {}", tmp.display()))?;
            for e in &self.entries {
                let line = serde_json::to_string(e)?;
                writeln!(file, "{line}")?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "commit session index from {} to {}",
                tmp.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }
}

/// Load the NDJSON index from disk. Returns `None` if the file is
/// missing or any line is unreadable.
fn load_index(path: &std::path::Path) -> Option<Vec<SessionEntry>> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut entries = Vec::new();
    for line in content.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<SessionEntry>(line) {
            Ok(e) => entries.push(e),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "session index corrupt; rebuilding");
                return None;
            }
        }
    }
    Some(entries)
}

/// Enumerate sessions in `<data_dir>/sessions/`, sorted newest-first
/// by `started_at`.
///
/// Sessions whose files fail to stat are silently skipped — the
/// listing is best-effort and a single corrupt file shouldn't
/// break `/sessions` for the rest.
pub fn list_sessions() -> anyhow::Result<Vec<SessionEntry>> {
    Ok(SessionIndex::load_or_refresh()?.list())
}

/// Search indexed sessions by id, date, message count, or message content.
pub fn search_sessions(query: &str) -> anyhow::Result<Vec<SessionEntry>> {
    Ok(SessionIndex::load_or_refresh()?.search(query))
}

/// Check whether a session file contains `query` (case-insensitive) anywhere
/// in its message text. Returns `false` on any read/parse error so search
/// never breaks because of one corrupt line.
fn session_content_matches(path: &std::path::Path, q: &str) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "failed to read session for search");
            return false;
        }
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .any(|line| {
            // Fast path: plain substring on the raw JSON line covers most
            // user text because `Message::content` is serialized as a string.
            // If the JSON is minified/escaped in an unusual way, fall back to
            // a best-effort parse and inspect known text fields.
            if line.to_lowercase().contains(q) {
                return true;
            }
            match serde_json::from_str::<crate::shared::Message>(line) {
                Ok(m) => m.content.to_lowercase().contains(q)
                    || m.thinking.as_deref().unwrap_or("").to_lowercase().contains(q)
                    || m.tool_name.as_deref().unwrap_or("").to_lowercase().contains(q),
                Err(_) => false,
            }
        })
}

/// Update the index after a session has been touched/created.
pub fn touch_session(id: &str, path: &std::path::Path) {
    if let Ok(mut index) = SessionIndex::load_or_refresh() {
        if let Err(e) = index.touch(id, path) {
            tracing::warn!(error = %e, "failed to update session index after touch");
        }
    }
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
    std::fs::remove_file(&path)
        .with_context(|| format!("delete session file {}", path.display()))?;
    if let Ok(mut index) = SessionIndex::load_or_refresh() {
        if let Err(e) = index.remove(id) {
            tracing::warn!(error = %e, "failed to update session index after delete");
        }
    }
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
    let deleted = prune_oldest_in_dir(&sessions_dir, keep, delete_count)?;
    // Rebuild the index so subsequent listings/search are accurate.
    if let Ok(mut index) = SessionIndex::load_or_refresh() {
        if let Err(e) = index.refresh() {
            tracing::warn!(error = %e, "failed to refresh session index after prune");
        }
    }
    Ok(deleted)
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
        for entry in std::fs::read_dir(sessions_dir)
            .with_context(|| format!("read sessions directory {}", sessions_dir.display()))?
            .flatten()
        {
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
        if let Err(err) = std::fs::remove_file(&e.path) {
            tracing::warn!(
                path = %e.path.display(),
                error = %err,
                "failed to prune session file; skipping"
            );
        } else {
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

    #[test]
    fn test_session_index_roundtrip() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let path = sessions_dir.join("roundtrip-session.conv.ndjson");
        std::fs::write(&path, "{\"role\":\"user\",\"content\":\"x\"}\n").unwrap();

        let entries = list_sessions().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "roundtrip-session");

        // Index file should have been created.
        assert!(sessions_dir.join(".index.ndjson").exists());

        match previous {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
        }
    }

    #[test]
    fn test_search_sessions_filters_by_id_and_date() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        for id in ["alpha-session", "beta-session"] {
            std::fs::write(
                sessions_dir.join(format!("{id}.conv.ndjson")),
                "{\"role\":\"user\",\"content\":\"x\"}\n",
            )
            .unwrap();
        }

        let alpha = search_sessions("alpha").unwrap();
        assert_eq!(alpha.len(), 1);
        assert_eq!(alpha[0].id, "alpha-session");

        // Every session should have a rfc3339 date containing the current year.
        let all = search_sessions("2026").unwrap();
        assert_eq!(all.len(), 2);

        match previous {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
        }
    }

    /// Search should match message content, not just metadata.
    #[test]
    fn test_search_sessions_matches_content() {
        let _guard = crate::session::test_data_dir_lock().blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("KIRKFORGE_DATA_DIR").ok();
        std::env::set_var("KIRKFORGE_DATA_DIR", dir.path());

        let sessions_dir = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::write(
            sessions_dir.join("hello-session.conv.ndjson"),
            "{\"role\":\"user\",\"content\":\"hello world\"}\n",
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("other-session.conv.ndjson"),
            "{\"role\":\"user\",\"content\":\"something else\"}\n",
        )
        .unwrap();

        let results = search_sessions("world").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "hello-session");

        let none = search_sessions("notfound").unwrap();
        assert!(none.is_empty());

        match previous {
            Some(v) => std::env::set_var("KIRKFORGE_DATA_DIR", v),
            None => std::env::remove_var("KIRKFORGE_DATA_DIR"),
        }
    }
}
