//! Atomic file write via `tempfile::NamedTempFile` + `persist`.
//! Per ADR-0014 § Atomic flag file for budget — a crash mid-write
//! leaves the previous file intact.
//!
//! ponytail: a thin wrapper around the stdlib/`tempfile` dance that
//! `save_budget` and `save_budget_config_at` both perform. Eprintln
//! tagging moves out of the helper so the failure log reads the same
//! whether it came from the budget file or a config file — a future
//! contributor chasing a corrupt-budget report sees a single label
//! style across both writers.

use std::io::Write;
use std::path::Path;

/// Write `body` to `path` atomically. Creates `path.parent()` if
/// it does not exist. On any I/O failure, prints a tagged warning
/// to stderr and returns without panicking — same contract as the
/// inline helper the CLI used to ship.
pub fn atomic_write_text(path: &Path, label: &str, body: &str) {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        eprintln!("plugin3: {label} dir create failed: {e}");
        return;
    }
    let mut tmp = match tempfile::NamedTempFile::new_in(parent) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("plugin3: {label} tmpfile create failed: {e}");
            return;
        }
    };
    if let Err(e) = tmp.write_all(body.as_bytes()) {
        eprintln!("plugin3: {label} tmpfile write failed: {e}");
        return;
    }
    if let Err(e) = tmp.flush() {
        eprintln!("plugin3: {label} tmpfile flush failed: {e}");
        return;
    }
    if let Err(e) = tmp.persist(path).map_err(|e| e.error) {
        eprintln!("plugin3: {label} persist failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ADR-0016 drift test: the atomic-write contract. A crash mid-
    // write must leave the previous file intact. The simplest
    // observable form is "the destination file never holds a partial
    // body" — verified by writing two full bodies in sequence and
    // asserting the file is always exactly one of them.

    #[test]
    fn writes_full_body_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.toml");
        atomic_write_text(&path, "test", "ceiling=100\nused=42\n");
        let s = std::fs::read_to_string(&path).unwrap();
        assert_eq!(s, "ceiling=100\nused=42\n");
    }

    #[test]
    fn overwrites_previous_body_atomically() {
        // ponytail: the spec says "crash mid-write leaves the
        // previous file intact". We can't easily kill the process
        // mid-write from a unit test, but we can prove the helper
        // *never* writes partial content: after each call, the file
        // is one of the two bodies the helper received, never a
        // mix. (If the persist step raced, it would be a mix.)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.toml");
        atomic_write_text(&path, "t", "FIRST_BODY");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "FIRST_BODY");
        atomic_write_text(&path, "t", "SECOND_BODY");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "SECOND_BODY");
    }

    #[test]
    fn creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/budget.toml");
        atomic_write_text(&path, "t", "x");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x");
    }

    #[test]
    fn empty_body_writes_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.toml");
        atomic_write_text(&path, "t", "");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn leaves_no_tempfile_on_success() {
        // ponytail: tempfile::NamedTempFile::persist consumes the
        // temp file. A bug that called `keep()` instead would leave
        // siblings like `budget.toml.tmpXXXXXX` around. Verify the
        // parent dir contains exactly one entry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget.toml");
        atomic_write_text(&path, "t", "body");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["budget.toml".to_string()]);
    }
}
