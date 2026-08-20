//! Shared test utilities for the `kf-code` binary crate.
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

// ── TUI test helpers (WO 19.7) ──────────────────────────────────────
// Deduplicated from 8 inline copies across tui/ and tui/commands/.
// Every `make_state` / `test_state` / `test_state_with_log` in the TUI
// tests was building the same `AppState::new(Arc::new(RwLock::new(Config::default())))`
// pattern. These two helpers capture the shared core; callers customize
// with `.connection`, `.session_id`, etc.

/// Construct a bare `AppState` with default config.
/// Replaces the identical `make_state()` and `test_state()` helpers
/// that were copy-pasted across 5 TUI test modules.
#[cfg(test)]
pub(crate) fn app_state() -> crate::tui::app::AppState {
    use std::sync::{Arc, RwLock};
    crate::tui::app::AppState::new(Arc::new(RwLock::new(crate::shared::Config::default())))
}

/// Construct an `AppState` with a `log_path` set.
/// Replaces the 3 identical `test_state_with_log()` helpers across
/// `tui/mod.rs`, `tui/commands/fork.rs`, and `tui/commands/save.rs`.
/// Callers that need `session_id` or `fork_manager` can add them
/// on top of the returned state.
#[cfg(test)]
pub(crate) fn app_state_with_log(log_path: std::path::PathBuf) -> crate::tui::app::AppState {
    let mut state = app_state();
    state.session.log_path = Some(log_path);
    state
}

#[cfg(test)]
pub(crate) struct EnvGuard {
    key: String,
    old: Option<String>,
}
#[cfg(test)]
impl EnvGuard {
    pub fn set(key: &str, val: impl AsRef<std::ffi::OsStr>) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, val);
        Self {
            key: key.to_string(),
            old,
        }
    }
    pub fn remove(key: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::remove_var(key);
        Self {
            key: key.to_string(),
            old,
        }
    }
}
#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => std::env::set_var(&self.key, v),
            None => std::env::remove_var(&self.key),
        }
    }
}

// ── CwdGuard (WO 40.5) ──────────────────────────────────────────────
// Process-global serializing mutex for tests that mutate the CWD via
// `std::env::set_current_dir`. CWD is process-global, so two CWD-mutating
// tests running in parallel race and flake. The guard holds the shared
// lock for its lifetime and restores the original CWD on drop — including
// when the test panics (RAII). `tokio::sync::Mutex` because some CWD-
// sensitive code paths run inside `.await` calls and must observe the
// test's CWD across the await boundary.

#[cfg(test)]
static CWD_GUARD: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// RAII guard that sets the process CWD to `dir` and restores the
/// previous CWD on drop. Serializes all CWD-mutating tests via a shared
/// process-global mutex so they cannot run concurrently.
///
/// Acquire the lock *before* mutating the CWD so concurrent tests block
/// until the holder drops. The guard restores the original CWD even if
/// the test panics.
#[cfg(test)]
pub struct CwdGuard {
    old: std::path::PathBuf,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl CwdGuard {
    /// Set the CWD to `dir` and return a guard that restores it on drop.
    /// Holds the shared CWD serializing lock for the guard's lifetime.
    pub async fn set(dir: impl AsRef<std::path::Path>) -> Self {
        let lock = CWD_GUARD
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let old = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        std::env::set_current_dir(dir).expect("CwdGuard: set_current_dir failed");
        Self { old, _lock: lock }
    }
}

#[cfg(test)]
impl Drop for CwdGuard {
    fn drop(&mut self) {
        if let Err(e) = std::env::set_current_dir(&self.old) {
            tracing::warn!(error = %e, path = %self.old.display(), "CwdGuard: failed to restore CWD");
        }
    }
}
