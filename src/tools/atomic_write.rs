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
///
/// ceiling: the parent directory entry is NOT fsynced after rename. On a
/// hard power loss immediately after `rename` returns, the new directory
/// entry may not yet be durable and the rename can be lost (the temp file
/// was fsynced, so its data survives, but the directory update is not
/// flushed). Cross-platform dir-fsync is non-portable (Unix `fsync(fd)` on
/// the dir vs Windows differs); the tradeoff is accepted here — the data
/// integrity window is limited to the rename durability, not the bytes.
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
        ".kf-code-{file_name}.{}-{}-{}.tmp",
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

    // Preserve the target's existing mode on Unix. The temp file was
    // created with the umask default (typically 0644); without this copy,
    // renaming over the target would silently clobber permissions — editing
    // an executable strips its exec bit (0755→0644), editing a 0600 secret
    // widens it to world-readable. Missing target = first write = keep the
    // umask default, so a stat error is ignored.
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(target) {
        let _ = std::fs::set_permissions(tmp, meta.permissions());
    }

    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    rename_with_retry(tmp, target)
}

/// Rename with a bounded retry on Windows.
///
/// On Windows, `MoveFileEx` fails with ERROR_ACCESS_DENIED (5) or
/// ERROR_SHARING_VIOLATION (32) when another process briefly holds a
/// handle on the source or target — most commonly an antivirus
/// (Defender) scanning a just-closed file. The hold lasts milliseconds;
/// retrying is the standard remedy. Unix `rename` has no such race:
/// it is a single call there.
pub fn rename_with_retry(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        const MAX_ATTEMPTS: usize = 10;
        for attempt in 0..MAX_ATTEMPTS {
            match std::fs::rename(src, dst) {
                Ok(()) => return Ok(()),
                Err(e)
                    if matches!(e.raw_os_error(), Some(5) | Some(32))
                        && attempt + 1 < MAX_ATTEMPTS =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("retry loop returns or errors before exhausting attempts");
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(src, dst)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn atomic_write_creates_file_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(&path, "hello world").unwrap();
        let mut content = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    fn atomic_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.txt");
        std::fs::write(&path, "old content").unwrap();
        atomic_write(&path, "new content").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn atomic_write_handles_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.txt");
        atomic_write(&path, "").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn atomic_write_handles_binary_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binary.dat");
        let data: Vec<u8> = (0..=255).collect();
        atomic_write(&path, &data).unwrap();
        let read_back = std::fs::read(&path).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn atomic_write_fails_for_nonexistent_parent() {
        let path = std::path::PathBuf::from("/nonexistent/dir/file.txt");
        assert!(atomic_write(&path, "content").is_err());
    }

    #[test]
    fn atomic_write_leaves_no_temp_file_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clean.txt");
        atomic_write(&path, "content").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "only the target file should exist, no leftover .tmp files"
        );
        assert_eq!(entries[0], path);
    }

    #[test]
    fn rename_with_retry_replaces_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");
        std::fs::write(&src, "new").unwrap();
        std::fs::write(&dst, "old").unwrap();
        rename_with_retry(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new");
        assert!(!src.exists(), "source is gone after a successful rename");
    }

    #[test]
    fn rename_with_retry_errors_on_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let err = rename_with_retry(&dir.path().join("nope"), &dir.path().join("dst")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    // Permission preservation is Unix-only: the bug is umask-default temp
    // file clobbering the target mode on rename. Windows ACLs don't model
    // this as a mode bits problem.
    #[cfg(unix)]
    fn unix_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode()
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_executable_mode_0755() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(unix_mode(&path) & 0o777, 0o755);

        atomic_write(&path, "#!/bin/sh\necho hi\n").unwrap();

        assert_eq!(
            unix_mode(&path) & 0o777,
            0o755,
            "exec bit must survive overwrite"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_secret_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(&path, "SECRET=old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(unix_mode(&path) & 0o777, 0o600);

        atomic_write(&path, "SECRET=new\n").unwrap();

        assert_eq!(
            unix_mode(&path) & 0o777,
            0o600,
            "0600 secret must not widen on overwrite"
        );
    }

    #[test]
    fn unique_counter_is_monotonic() {
        let a = unique_counter();
        let b = unique_counter();
        assert!(b > a, "counter must be monotonic: {a} -> {b}");
    }

    #[test]
    fn unique_timestamp_nanos_is_nonzero() {
        let ts = unique_timestamp_nanos();
        assert!(ts > 0, "timestamp should be nonzero on real hardware");
    }
}
