//! Shared test utilities for the `kirkforge` binary crate.
//!
//! Helpers in this module are only compiled under `#[cfg(test)]` and
//! exported through `crate::shared::test_util` so that unit tests across
//! the crate can avoid duplicating the same cleanup boilerplate.

use std::path::Path;

/// Best-effort cleanup of a temp file created by a test.
///
/// Logs unexpected failures but ignores `NotFound`, which is normal for
/// idempotent cleanup.
pub fn remove_test_file(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to remove test temp file"
            );
        }
    }
}

/// Best-effort cleanup of a temp directory created by a test.
///
/// Logs unexpected failures but ignores `NotFound`, which is normal for
/// idempotent cleanup.
pub fn remove_test_dir(path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to remove test temp directory"
            );
        }
    }
}
