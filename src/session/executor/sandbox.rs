//! Sandbox enforcement — groups path, deny-list, and read-gate checks
//! into a single sub-struct owned by [`super::Executor`].
//!
//! ponytail: not a sandbox. This is path-based access control (read/write
//! scoping + deny list). Real sandboxing requires seccomp (syscall
//! allow-list) or landlock (file-level bind mount) — see ADR-054
//! "Future work" section. Upgrade path: add `setup_seccomp` alongside
//! `setup_rlimits` in `bash_runner/mod.rs` when a BPF compiler is
//! available without a C FFI dep.

use crate::session::access::{DenyList, GuardVerdict, PathGuard, ReadGate};
use std::path::PathBuf;

// ponytail: not a sandbox, upgrade path: seccomp/landlock
pub(crate) struct PathGuardTower {
    pub(crate) path_guard: PathGuard,
    pub(crate) deny_list: DenyList,
    pub(crate) read_gate: ReadGate,
}

impl PathGuardTower {
    pub(crate) fn check_read(&self, path: &std::path::Path) -> GuardVerdict {
        self.path_guard.check_read(path)
    }

    pub(crate) async fn check_write(&self, path: &std::path::Path) -> GuardVerdict {
        self.path_guard.check_write(path).await
    }

    pub(crate) fn mark_read(&mut self, path: &std::path::Path) {
        self.read_gate.mark_read(path);
    }

    pub(crate) fn check_edit(
        &self,
        path: &std::path::Path,
        resolved: &std::path::Path,
    ) -> GuardVerdict {
        self.read_gate.check_edit(path, resolved)
    }

    /// Return the top `n` most frequently accessed file paths, sorted by
    /// access count descending. Used by the shared context stem to inject
    /// hot file bodies into the cached prefix so Anthropic's prompt cache
    /// covers the files the model re-reads every turn (WO 17.5).
    pub(crate) fn top_files(&self, n: usize) -> Vec<PathBuf> {
        self.read_gate.top_files(n)
    }
}
