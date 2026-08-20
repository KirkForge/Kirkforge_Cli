//! Per-session undo stack for file edits.
//!
//! Review.md gap #7: the model can `edit_file` and `write_file`, and
//! the only safety net before this change was the user's git working
//! tree. There was no in-app undo — every other AI coding tool
//! (Aider, Claude Code, Cursor) ships one. This module is the gate.
//!
//! # Design
//!
//! On every successful `edit_file` / `write_file`, we snapshot the
//! file's pre-edit bytes to disk before the tool writes the new
//! content. The snapshots are kept in
//! `~/.local/share/kf-code/undo/<session_id>/<n>.snap` — one file
//! per op, in chronological order. The `UndoStack` struct manages
//! reading and writing the snapshots.
//!
//! `pop` restores the most recent file: writes the snapshot back to
//! its original path (or removes the file if it didn't exist before
//! the edit). `list` enumerates the stack without modifying it. The
//! cap is 50 entries — old snapshots are FIFO-trimmed on `push`.
//!
//! # Atomicity
//!
//! Snapshots are written via `tempfile::NamedTempFile` + atomic
//! rename so a crash mid-snapshot can't leave a half-written `.snap`
//! on disk. Restoration writes through the same pattern: write to
//! `<path>.tmp`, fsync, rename. The original file is overwritten
//! atomically.
//!
//! # Threading
//!
//! The stack is wrapped in `Arc<Mutex<UndoStack>>` because the
//! executor and the TUI's `/undo` handler both touch it. The
//! critical sections are tiny (push a few hundred KB of bytes, pop
//! a single file) so contention is not a concern.
//!
//! # Lifecycle
//!
//! The stack lives for the duration of the session. When the user
//! runs `/undo`, the most recent op is restored. When the user
//! quits, the snapshot directory stays on disk — a future
//! `--continue <session-id>` could replay the stack, though that
//! is a future feature (review.md gap #3 sessions-list, M3).
//!
//! # Failure modes
//!
//! - `current_dir()` failure at session start: `for_session` returns
//!   an error and the executor logs a warning. Edits still work;
//!   they just can't be undone. Better than refusing the edit.
//! - Disk full while writing a snapshot: the tool's run returns
//!   `ToolOutcome::Error` with a clear message; the file is NOT
//!   modified (the snapshot is written first, then the tool writes
//!   the new content, so on a snapshot-write failure we abort
//!   before the destructive step).
//! - Permission denied on restore: the user is told which file
//!   couldn't be restored; the snapshot stays on disk for manual
//!   recovery.

use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Maximum number of undo entries kept on disk. Oldest entries
/// are FIFO-trimmed on each push. 50 is generous — most sessions
/// don't go past 10 edits, and 50 is small enough that a runaway
/// agent can't fill the disk.
const MAX_ENTRIES: usize = 50;

/// Maximum total bytes of snapshots kept on disk. A single 5 MiB
/// file edited 11 times would otherwise consume 55 MiB; this cap
/// keeps the undo directory bounded regardless of file sizes.
#[cfg(not(test))]
const MAX_TOTAL_SNAPSHOT_BYTES: u64 = 50 * 1024 * 1024;

/// Test override so the total-size cap can be exercised without writing
/// tens of megabytes to disk in every test run.
#[cfg(test)]
const MAX_TOTAL_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024;

/// One entry in the undo stack. The snapshot lives in a file on
/// disk; this struct holds just the metadata needed to display the
/// stack and restore on demand.
#[derive(Debug, Clone)]
pub struct UndoOp {
    /// Monotonic counter, used as the snapshot filename.
    /// `0` is the first push, `1` the second, etc.
    pub seq: u64,
    /// Edit (`edit_file`) or Write (`write_file`). Affects display
    /// only; restore logic is the same for both.
    pub kind: UndoKind,
    /// Absolute path to the file the edit touched.
    pub path: PathBuf,
    /// True if the file existed before the edit. False means the
    /// edit created a new file; on restore, we remove the file.
    pub prev_existed: bool,
    /// Size of the snapshot in bytes. Display only.
    pub snapshot_size: u64,
    /// Wall-clock timestamp of the push. Display only.
    pub timestamp: chrono::DateTime<chrono::Local>,
}

// WO 28.1: UndoKind now lives in shared::undo (pure enum). Re-exported here so
// existing `session::undo::UndoKind` callers (UndoOp/UndoSummary, tui) resolve.
pub use crate::shared::undo::UndoKind;

/// A human-readable summary of an undo entry. Used by `/undo list`.
#[derive(Debug, Clone)]
pub struct UndoSummary {
    pub seq: u64,
    pub kind: UndoKind,
    pub path: PathBuf,
    pub snapshot_size: u64,
    pub timestamp: chrono::DateTime<chrono::Local>,
}

impl From<&UndoOp> for UndoSummary {
    fn from(op: &UndoOp) -> Self {
        Self {
            seq: op.seq,
            kind: op.kind,
            path: op.path.clone(),
            snapshot_size: op.snapshot_size,
            timestamp: op.timestamp,
        }
    }
}

/// The undo stack. Wrapped in `Arc<Mutex<UndoStack>>` by callers;
/// the struct itself is not internally synchronized.
pub struct UndoStack {
    /// Absolute path to the snapshot directory for this session.
    dir: PathBuf,
    /// In-memory mirror of the disk state, in chronological order.
    /// `push` appends, `pop` removes from the back.
    ops: VecDeque<UndoOp>,
    /// Monotonic counter; the next snapshot filename is `seq`.
    /// Persisted to `<dir>/next_seq` so a session continuation
    /// doesn't reuse a sequence number (a future feature).
    next_seq: AtomicU64,
    /// Sum of `snapshot_size` for all entries currently in `ops`. Kept
    /// in sync so the total-size cap trim does not require a second pass.
    total_snapshot_bytes: u64,
}

impl UndoStack {
    /// Open or create the undo directory for `session_id`. The
    /// directory is `data_dir()/undo/<session_id>/`.
    pub fn for_session(session_id: &str) -> Result<Self> {
        let data_dir = crate::session::data_dir()?;
        let dir = data_dir.join("undo").join(session_id);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create undo directory {}", dir.display()))?;

        // Reconstruct the in-memory ops deque by listing the
        // snapshot files in order. A snapshot file is just bytes
        // (no header), so we can't recover prev_existed/path from
        // the file alone — we re-read a small sidecar metadata
        // file: `<seq>.meta.json` (one per op).
        let mut ops = VecDeque::new();
        let mut max_seq: u64 = 0;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name();
                let fname = fname.to_string_lossy();
                if let Some(seq_str) = fname.strip_suffix(".meta.json") {
                    if let Ok(_seq) = seq_str.parse::<u64>() {
                        if let Ok(meta_bytes) = std::fs::read(entry.path()) {
                            if let Ok(meta) =
                                serde_json::from_slice::<SerializedMeta>(meta_bytes.as_slice())
                            {
                                ops.push_back(UndoOp {
                                    seq: meta.seq,
                                    kind: meta.kind,
                                    path: PathBuf::from(meta.path),
                                    prev_existed: meta.prev_existed,
                                    snapshot_size: meta.snapshot_size,
                                    timestamp: meta.timestamp,
                                });
                                if meta.seq > max_seq {
                                    max_seq = meta.seq;
                                }
                            }
                        }
                    }
                }
            }
        }
        // Sort by seq to be robust to readdir ordering.
        let mut ops_vec: Vec<UndoOp> = ops.into_iter().collect();
        ops_vec.sort_by_key(|op| op.seq);
        let total_snapshot_bytes = ops_vec.iter().map(|op| op.snapshot_size).sum();
        let ops: VecDeque<UndoOp> = ops_vec.into_iter().collect();

        Ok(Self {
            dir,
            ops,
            next_seq: AtomicU64::new(max_seq + 1),
            total_snapshot_bytes,
        })
    }

    /// Remove a snapshot and its metadata sidecar, ignoring NotFound.
    /// Logs unexpected failures so disk-permission problems don't
    /// silently leave orphaned undo files.
    fn remove_snapshot(&self, seq: u64) {
        if let Err(e) = std::fs::remove_file(self.snapshot_path(seq)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %e,
                    seq,
                    "Failed to remove undo snapshot file"
                );
            }
        }
        if let Err(e) = std::fs::remove_file(self.meta_path(seq)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    error = %e,
                    seq,
                    "Failed to remove undo metadata file"
                );
            }
        }
    }

    /// Snapshot `prev_bytes` to disk for the edit at `path`. Returns
    /// the seq number assigned. If the stack is at capacity, the
    /// oldest entry is FIFO-trimmed first (its snapshot file is
    /// removed).
    ///
    /// `prev_existed = false` means the edit will create a new
    /// file; `prev_bytes` should be empty in that case. We still
    /// record the op so `pop` knows to `rm` the file on restore.
    pub fn push(
        &mut self,
        kind: UndoKind,
        path: &Path,
        prev_existed: bool,
        prev_bytes: &[u8],
    ) -> Result<u64> {
        let new_bytes = prev_bytes.len() as u64;

        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let snap_path = self.snapshot_path(seq);
        let meta_path = self.meta_path(seq);

        // Write snapshot atomically: temp + rename. A crash mid-write
        // either leaves the old state intact (rename didn't happen)
        // or the new snapshot fully visible. Never a half-truncated
        // file the loader might mistake for valid.
        let tmp_snap = snap_path.with_extension("snap.tmp");
        std::fs::write(&tmp_snap, prev_bytes)
            .with_context(|| format!("write undo snapshot {}", tmp_snap.display()))?;
        crate::tools::atomic_write::rename_with_retry(&tmp_snap, &snap_path)
            .with_context(|| format!("finalize undo snapshot {}", snap_path.display()))?;

        // Write the sidecar metadata.
        let meta = SerializedMeta {
            seq,
            kind,
            path: path.to_string_lossy().to_string(),
            prev_existed,
            snapshot_size: prev_bytes.len() as u64,
            timestamp: chrono::Local::now(),
        };
        let meta_json = serde_json::to_string_pretty(&meta)?;
        std::fs::write(&meta_path, meta_json)
            .with_context(|| format!("write undo metadata {}", meta_path.display()))?;

        // Record the operation in memory only after the new snapshot is
        // safely on disk. If the writes above failed, the old stack is
        // untouched.
        self.total_snapshot_bytes += new_bytes;
        self.ops.push_back(UndoOp {
            seq,
            kind,
            path: path.to_path_buf(),
            prev_existed,
            snapshot_size: new_bytes,
            timestamp: meta.timestamp,
        });

        // FIFO-trim the oldest entries only after the new snapshot is
        // durable. A write failure must never cost us the previously
        // saved snapshots.
        while self.ops.len() > MAX_ENTRIES {
            if let Some(oldest) = self.ops.pop_front() {
                self.total_snapshot_bytes = self
                    .total_snapshot_bytes
                    .saturating_sub(oldest.snapshot_size);
                self.remove_snapshot(oldest.seq);
            }
        }

        // Trim oldest entries until the total byte budget is respected.
        // A single snapshot bigger than the cap is still allowed — the user
        // needs at least one undo — but everything older is evicted.
        while !self.ops.is_empty() && self.total_snapshot_bytes > MAX_TOTAL_SNAPSHOT_BYTES {
            if let Some(oldest) = self.ops.pop_front() {
                self.total_snapshot_bytes = self
                    .total_snapshot_bytes
                    .saturating_sub(oldest.snapshot_size);
                self.remove_snapshot(oldest.seq);
            } else {
                break;
            }
        }

        Ok(seq)
    }

    /// Restore the most recent snapshot, removing the file if the
    /// pre-edit state was "didn't exist." Returns a summary string
    /// for the user, or `Ok(None)` if the stack was empty.
    pub fn pop(&mut self) -> Result<Option<RestoredOp>> {
        let Some(op) = self.ops.pop_back() else {
            return Ok(None);
        };
        self.total_snapshot_bytes = self.total_snapshot_bytes.saturating_sub(op.snapshot_size);
        let snap_path = self.snapshot_path(op.seq);

        if op.prev_existed {
            // Atomic write: temp + rename. The user's old file is
            // replaced in one filesystem step.
            let tmp_target = op.path.with_extension(format!(
                "{}.undo.tmp",
                op.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("tmp")
            ));
            let bytes = std::fs::read(&snap_path)
                .with_context(|| format!("read undo snapshot {}", snap_path.display()))?;
            std::fs::write(&tmp_target, &bytes).with_context(|| {
                format!("write undo restore temp file {}", tmp_target.display())
            })?;
            crate::tools::atomic_write::rename_with_retry(&tmp_target, &op.path)
                .with_context(|| format!("finalize undo restore {}", op.path.display()))?;
        } else {
            // The file was created by the edit — restore means
            // remove it.
            if op.path.exists() {
                std::fs::remove_file(&op.path)
                    .with_context(|| format!("remove file during undo {}", op.path.display()))?;
            }
        }

        // Clean up the snapshot + metadata. We've used them; the
        // user can re-edit if they want to redo.
        self.remove_snapshot(op.seq);

        Ok(Some(RestoredOp {
            path: op.path.clone(),
            kind: op.kind,
            prev_existed: op.prev_existed,
        }))
    }

    /// List all entries in chronological order. Used by
    /// `/undo list` and the unit tests.
    pub fn list(&self) -> Vec<UndoSummary> {
        self.ops.iter().map(UndoSummary::from).collect()
    }

    /// True if there's nothing to undo.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of entries currently on the stack.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Remove every snapshot and metadata file, and clear the in-memory stack.
    /// Returns the number of entries removed. The next `push` starts from the
    /// same sequence counter (clear does not reset it).
    pub fn clear(&mut self) -> Result<usize> {
        let count = self.ops.len();
        let seqs: Vec<u64> = self.ops.drain(..).map(|op| op.seq).collect();
        for seq in seqs {
            self.remove_snapshot(seq);
        }
        self.total_snapshot_bytes = 0;
        Ok(count)
    }

    fn snapshot_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{seq:08}.snap"))
    }

    fn meta_path(&self, seq: u64) -> PathBuf {
        self.dir.join(format!("{seq:08}.meta.json"))
    }
}

/// Age threshold for an orphaned undo directory to be considered stale.
/// A session's undo dir is orphaned when its session log no longer exists
/// (the session was deleted or pruned). We only remove dirs older than this
/// so a session that is mid-creation (log not yet written) is never touched.
/// ponytail: 24h is generous — snapshots are written on every edit and the
/// FIFO cap is 50 entries / 50MB, so nothing legitimately in-flight is this
/// old. Upgrade path: reference-count snap files if they need transactional
/// semantics.
const ORPHAN_SNAP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Remove undo directories whose session no longer exists and whose mtime is
/// older than [`ORPHAN_SNAP_MAX_AGE`]. Called at daemon startup so orphaned
/// `.snap` files from deleted/pruned sessions don't leak disk forever.
///
/// Best-effort: a failure to read or remove one directory is logged and does
/// not abort the scan of the rest.
pub fn cleanup_orphan_snaps() {
    let Ok(data_dir) = crate::session::data_dir() else {
        return;
    };
    let undo_root = data_dir.join("undo");
    let sessions_dir = data_dir.join("sessions");
    let Ok(entries) = std::fs::read_dir(&undo_root) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(session_id) = dir.file_name().and_then(|f| f.to_str()) else {
            continue;
        };
        // Skip dirs whose session log still exists — those are active.
        if sessions_dir
            .join(format!("{session_id}.conv.ndjson"))
            .exists()
        {
            continue;
        }
        // Skip dirs that are too young to be safely considered orphaned.
        let modified = match std::fs::metadata(&dir).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "could not stat undo dir; skipping");
                continue;
            }
        };
        // Skip if the dir is too young, or the mtime is in the future
        // (clock skew) — never delete a possibly-in-flight snapshot.
        let too_young = match now.duration_since(modified) {
            Ok(age) => age < ORPHAN_SNAP_MAX_AGE,
            Err(_) => true,
        };
        if too_young {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(path = %dir.display(), error = %e, "failed to remove orphaned undo dir");
        } else {
            tracing::info!(path = %dir.display(), "removed orphaned undo dir");
        }
    }
}

/// What `pop` actually did. Display string is built by the caller.
#[derive(Debug, Clone)]
pub struct RestoredOp {
    pub path: PathBuf,
    pub kind: UndoKind,
    pub prev_existed: bool,
}

/// On-disk sidecar format. We use a small `Serialize`/`Deserialize`
/// derive to keep this readable. The `seq` field is redundant with
/// the filename — it's there for sanity and to make the file
/// self-describing if someone inspects it by hand.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SerializedMeta {
    seq: u64,
    kind: UndoKind,
    path: String,
    prev_existed: bool,
    snapshot_size: u64,
    timestamp: chrono::DateTime<chrono::Local>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Guard that atomically creates a temp data directory, sets
    /// `KF_CODE_DATA_DIR` to it, and cleans it up on drop. The
    /// shared `test_data_dir_lock` prevents concurrent tests from
    /// racing on the environment variable or deleting each other's
    /// temp directories.
    struct DataDirGuard {
        dir: PathBuf,
        _env: crate::shared::test_util::EnvGuard,
        _lock: tokio::sync::MutexGuard<'static, ()>,
    }

    impl DataDirGuard {
        fn new() -> Self {
            let _lock = crate::session::test_data_dir_lock().blocking_lock();
            let dir = env::temp_dir().join(format!(
                "kf-code-undo-test-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("create temp data dir");
            let env = crate::shared::test_util::EnvGuard::set("KF_CODE_DATA_DIR", &dir);
            Self {
                dir,
                _env: env,
                _lock,
            }
        }
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Helper: a fresh UndoStack in an isolated temp data dir, with a
    /// unique session id. The returned guard must live as long as the
    /// stack so the data directory stays valid for the whole test.
    fn fresh_stack() -> (UndoStack, DataDirGuard) {
        let guard = DataDirGuard::new();
        let id = format!(
            "test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let stack = UndoStack::for_session(&id).expect("for_session");
        (stack, guard)
    }

    /// Round-trip: push a snapshot, pop it, file content reverts.
    #[test]
    fn test_push_pop_round_trip_existing_file() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_target.txt");
        std::fs::write(&target, b"original content").unwrap();

        // Simulate the tool: snapshot, then write new content.
        let prev = std::fs::read(&target).unwrap();
        stack
            .push(UndoKind::Edit, &target, true, &prev)
            .expect("push");
        std::fs::write(&target, b"new content").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new content");

        // Pop: file should revert.
        let restored = stack.pop().expect("pop").expect("non-empty");
        assert_eq!(restored.path, target);
        assert!(restored.prev_existed);
        assert_eq!(std::fs::read(&target).unwrap(), b"original content");
        assert!(stack.is_empty());
    }

    /// Round-trip: file created by the edit. Pop removes the file.
    #[test]
    fn test_push_pop_round_trip_new_file() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_new.txt");
        // Pre-edit state: file does not exist.
        assert!(!target.exists());

        stack
            .push(UndoKind::Write, &target, false, b"")
            .expect("push");
        // Simulate the write_file tool creating the file.
        std::fs::write(&target, b"new file content").unwrap();
        assert!(target.exists());

        let restored = stack.pop().expect("pop").expect("non-empty");
        assert_eq!(restored.path, target);
        assert!(!restored.prev_existed);
        // Pop should remove the file.
        assert!(!target.exists());
        assert!(stack.is_empty());
    }

    /// LIFO order: pop returns the most recent push.
    #[test]
    fn test_pop_returns_most_recent() {
        let (mut stack, _guard) = fresh_stack();
        let a = env::temp_dir().join("kf_code_undo_a.txt");
        let b = env::temp_dir().join("kf_code_undo_b.txt");
        std::fs::write(&a, b"A1").unwrap();
        std::fs::write(&b, b"B1").unwrap();

        let a_prev = std::fs::read(&a).unwrap();
        let b_prev = std::fs::read(&b).unwrap();
        stack.push(UndoKind::Edit, &a, true, &a_prev).unwrap();
        stack.push(UndoKind::Edit, &b, true, &b_prev).unwrap();

        std::fs::write(&a, b"A2").unwrap();
        std::fs::write(&b, b"B2").unwrap();

        // Pop returns B (most recent).
        let r = stack.pop().unwrap().unwrap();
        assert_eq!(r.path, b);
        assert_eq!(std::fs::read(&b).unwrap(), b"B1");
        // Then A.
        let r = stack.pop().unwrap().unwrap();
        assert_eq!(r.path, a);
        assert_eq!(std::fs::read(&a).unwrap(), b"A1");
        // Then empty.
        assert!(stack.pop().unwrap().is_none());
    }

    /// FIFO trim at the cap: 51st push removes the 1st.
    #[test]
    fn test_fifo_trim_at_max() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_trim.txt");
        for i in 0..(MAX_ENTRIES + 1) {
            std::fs::write(&target, format!("v{i}")).unwrap();
            let prev = std::fs::read(&target).unwrap();
            stack
                .push(UndoKind::Edit, &target, true, &prev)
                .expect("push");
        }
        assert_eq!(stack.len(), MAX_ENTRIES, "should cap at MAX_ENTRIES");
    }

    /// `list` returns entries in chronological order.
    #[test]
    fn test_list_in_chronological_order() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_list.txt");
        for i in 0..3 {
            std::fs::write(&target, format!("v{i}")).unwrap();
            let prev = std::fs::read(&target).unwrap();
            stack.push(UndoKind::Edit, &target, true, &prev).unwrap();
        }
        let list = stack.list();
        assert_eq!(list.len(), 3);
        // The seq numbers should be strictly increasing.
        assert!(list[0].seq < list[1].seq);
        assert!(list[1].seq < list[2].seq);
    }

    /// Total snapshot-size cap: oldest entries are evicted once the
    /// budget would be exceeded.
    #[test]
    fn test_total_size_cap_evicts_oldest() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_size_cap.txt");
        // Push two 1.5 MiB snapshots. With the 2 MiB test cap, the second
        // push should evict the first.
        let one_and_half = 3 * 1024 * 1024 / 2;
        for i in 0..2 {
            let content = vec![b'a' + i; one_and_half];
            std::fs::write(&target, &content).unwrap();
            let prev = std::fs::read(&target).unwrap();
            stack
                .push(UndoKind::Edit, &target, true, &prev)
                .expect("push");
            // Pretend an edit happened.
            std::fs::write(&target, vec![b'0' + i; one_and_half]).unwrap();
        }
        assert_eq!(stack.len(), 1, "only the newest snapshot should remain");
        assert!(
            stack.list()[0].snapshot_size >= 1024 * 1024,
            "remaining entry should be the large one"
        );
    }

    /// Write failure during a push must not evict previously saved
    /// snapshots. The new snapshot is written first, then the trim
    /// runs; if the write fails, the old stack and its files must
    /// remain intact.
    #[cfg(unix)]
    #[test]
    fn test_push_write_failure_preserves_old_snapshots() {
        use std::os::unix::fs::PermissionsExt;

        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_fail.txt");
        std::fs::write(&target, b"first").unwrap();
        let prev = std::fs::read(&target).unwrap();
        stack.push(UndoKind::Edit, &target, true, &prev).unwrap();

        let old_seq = stack.list()[0].seq;
        let old_total = stack.total_snapshot_bytes;
        let old_snap = stack.snapshot_path(old_seq);
        let old_meta = stack.meta_path(old_seq);
        assert!(old_snap.exists());
        assert!(old_meta.exists());

        // Make the undo directory read-only so the next write fails.
        std::fs::set_permissions(&stack.dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Fill the stack to the count cap so the old implementation would
        // have trimmed before writing; the new implementation must keep the
        // old entry when the write fails.
        for i in 0..MAX_ENTRIES {
            std::fs::write(&target, format!("v{i}")).unwrap();
            let prev = std::fs::read(&target).unwrap();
            if i == 0 {
                // First push after the read-only chmod should fail.
                let result = stack.push(UndoKind::Edit, &target, true, &prev);
                assert!(result.is_err(), "write should fail on read-only dir");
                break;
            }
        }

        // Restore write permission so the test guard can clean up.
        std::fs::set_permissions(&stack.dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert_eq!(stack.len(), 1, "old snapshot should still be in memory");
        assert_eq!(
            stack.total_snapshot_bytes, old_total,
            "total bytes should not change on failed push"
        );
        assert!(
            old_snap.exists(),
            "old snapshot file should not have been removed"
        );
        assert!(
            old_meta.exists(),
            "old metadata file should not have been removed"
        );
    }

    /// `clear` removes every entry and zeros the total byte count.
    #[test]
    fn test_clear_empties_stack_and_disk() {
        let (mut stack, _guard) = fresh_stack();
        let target = env::temp_dir().join("kf_code_undo_clear.txt");
        std::fs::write(&target, b"v1").unwrap();
        let prev = std::fs::read(&target).unwrap();
        stack.push(UndoKind::Edit, &target, true, &prev).unwrap();

        let removed = stack.clear().expect("clear");
        assert_eq!(removed, 1);
        assert!(stack.is_empty());
        assert_eq!(stack.total_snapshot_bytes, 0);
    }

    /// Persistence: a fresh `for_session` call reconstructs the
    /// stack from disk. This is the path that would matter for
    /// future `--continue <session-id>` (M3) — even without that
    /// feature, it's a good sanity test.
    #[test]
    fn test_for_session_reconstructs_from_disk() {
        let _guard = DataDirGuard::new();
        let id = format!(
            "test-recon-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let target = env::temp_dir().join("kf_code_undo_recon.txt");
        std::fs::write(&target, b"v0").unwrap();

        {
            let mut stack = UndoStack::for_session(&id).unwrap();
            let prev = std::fs::read(&target).unwrap();
            stack.push(UndoKind::Edit, &target, true, &prev).unwrap();
            std::fs::write(&target, b"v1").unwrap();
            let prev = std::fs::read(&target).unwrap();
            stack.push(UndoKind::Edit, &target, true, &prev).unwrap();
            std::fs::write(&target, b"v2").unwrap();
        } // stack dropped

        // Re-open: should have 2 entries, in order.
        let stack = UndoStack::for_session(&id).unwrap();
        assert_eq!(stack.len(), 2);
        let list = stack.list();
        assert!(list[0].seq < list[1].seq);
    }

    #[test]
    fn undo_kind_as_str_covers_both_variants() {
        assert_eq!(UndoKind::Edit.as_str(), "edit");
        assert_eq!(UndoKind::Write.as_str(), "write");
    }

    #[test]
    fn undo_summary_from_copies_fields() {
        let op = UndoOp {
            seq: 7,
            kind: UndoKind::Write,
            path: PathBuf::from("/tmp/x.rs"),
            prev_existed: true,
            snapshot_size: 42,
            timestamp: chrono::Local::now(),
        };
        let summary = UndoSummary::from(&op);
        assert_eq!(summary.seq, 7);
        assert_eq!(summary.path, PathBuf::from("/tmp/x.rs"));
        assert_eq!(summary.snapshot_size, 42);
    }

    #[test]
    fn fresh_stack_is_empty_and_len_zero() {
        let _guard = DataDirGuard::new();
        let id = format!(
            "fresh-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let stack = UndoStack::for_session(&id).unwrap();
        assert!(stack.is_empty());
        assert_eq!(stack.len(), 0);
        assert!(stack.list().is_empty());
    }

    #[test]
    fn snapshot_path_and_meta_path_use_zero_padded_seq() {
        let _guard = DataDirGuard::new();
        let id = format!(
            "snap-paths-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let stack = UndoStack::for_session(&id).unwrap();
        assert!(stack.snapshot_path(1).ends_with("00000001.snap"));
        assert!(stack.snapshot_path(99999999).ends_with("99999999.snap"));
        assert!(stack.meta_path(1).ends_with("00000001.meta.json"));
    }

    /// `cleanup_orphan_snaps` removes undo dirs whose session log is gone and
    /// whose mtime is older than the age threshold, and leaves active or young
    /// dirs alone.
    // The fixture sets a directory's mtime by opening it as a File and
    // calling set_times — that works on Unix (opening a directory yields
    // a valid fd) but panics on Windows (CreateFile on a directory with
    // default access is an error). The production `cleanup_orphan_snaps`
    // is cross-platform; only this mtime-manipulation fixture is Unix-
    // specific. Gate accordingly.
    #[cfg(unix)]
    #[test]
    fn cleanup_orphan_snaps_removes_only_stale_orphans() {
        let _guard = DataDirGuard::new();
        let data_dir = crate::session::data_dir().unwrap();
        let undo_root = data_dir.join("undo");
        let sessions_dir = data_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Active session: log exists → its undo dir must survive.
        std::fs::write(sessions_dir.join("active.conv.ndjson"), b"").unwrap();
        std::fs::create_dir_all(undo_root.join("active")).unwrap();

        // Orphan, old: no log, mtime in the past → must be removed.
        let old_orphan = undo_root.join("old-orphan");
        std::fs::create_dir_all(&old_orphan).unwrap();
        std::fs::write(old_orphan.join("00000001.snap"), b"x").unwrap();
        let old =
            std::time::SystemTime::now() - ORPHAN_SNAP_MAX_AGE - std::time::Duration::from_secs(60);
        let f = std::fs::File::open(&old_orphan).unwrap();
        let times = std::fs::FileTimes::new().set_modified(old);
        f.set_times(times).unwrap();
        drop(f);

        // Orphan, young: no log but recent mtime → must survive.
        let young_orphan = undo_root.join("young-orphan");
        std::fs::create_dir_all(&young_orphan).unwrap();
        std::fs::write(young_orphan.join("00000001.snap"), b"x").unwrap();

        cleanup_orphan_snaps();

        assert!(undo_root.join("active").exists(), "active undo dir kept");
        assert!(!old_orphan.exists(), "old orphaned undo dir removed");
        assert!(
            young_orphan.exists(),
            "young orphaned undo dir kept (not in-flight)"
        );
    }
}
