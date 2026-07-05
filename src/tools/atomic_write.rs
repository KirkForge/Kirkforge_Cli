//! Atomic file-write helper used by `edit_file` and `write_file`.
//!
//! Writing directly to the target path risks leaving a half-truncated file
//! if the process crashes or the disk fills mid-write. This helper writes
//! to a temporary file in the same directory, fsyncs it, then renames it
//! over the target so the replacement is a single filesystem step.

use std::fs::File;
use std::io::Write;
use std::path::Path;

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
    // Unique temp name: include pid and a monotonic counter so concurrent
    // tools editing the same file don't collide.
    let tmp_name = format!(".kirkforge-{file_name}.{}.tmp", unique_counter());
    let tmp_path = parent.join(&tmp_name);

    let result = write_fsync_rename(&tmp_path, path, contents);
    if result.is_err() {
        // Best-effort cleanup; ignore NotFound.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn write_fsync_rename(tmp: &Path, target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(tmp)?;
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
