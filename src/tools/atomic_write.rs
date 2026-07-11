//! Atomic file-write helper used by `edit_file` and `write_file`.
//!
//! Writing directly to the target path risks leaving a half-truncated file
//! if the process crashes or the disk fills mid-write. This helper writes
//! to a temporary file in the same directory, fsyncs it, then renames it
//! over the target so the replacement is a single filesystem step.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Write `contents` to `path` atomically.
///
/// The parent directory must already exist. The temporary file is created
/// in the same directory as `path` (so `rename` is atomic within one
/// filesystem), fsynced before rename, and removed automatically if the
/// rename fails.
pub fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let contents = contents.as_ref();
    let parent = path.parent().unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "write".to_string());
    // Unique temp name: pid + nanosecond timestamp + monotonic counter.
    // The timestamp makes the name hard to predict, which blocks a
    // symlink-race attacker from pre-creating the temp path.
    let tmp_name = format!(
        ".kirkforge-{file_name}.{}-{}-{}.tmp",
        std::process::id(),
        unique_timestamp_nanos(),
        unique_counter()
    );
    let tmp_path = parent.join(&tmp_name);

    let result = write_fsync_rename(&tmp_path, path, contents);
    if result.is_err() {
        // Best-effort cleanup; ignore NotFound.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn write_fsync_rename(tmp: &Path, target: &Path, contents: &[u8]) -> std::io::Result<()> {
    // `create_new(true)` is `O_EXCL|O_CREAT`: it fails if `tmp` already
    // exists, preventing a symlink at the temp path from redirecting the
    // write to an arbitrary file.
    let mut file = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, target)
}

/// Process-local monotonic counter for temp-file names.
fn unique_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Nanosecond timestamp for temp-file names. Falls back to 0 if the system
/// clock is before the Unix epoch (should never happen on real hardware).
fn unique_timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
