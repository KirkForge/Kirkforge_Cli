//! Sandbox enforcement — groups path, deny-list, and read-gate checks
//! into a single sub-struct owned by [`super::Executor`].

use crate::session::access::{DenyList, GuardVerdict, PathGuard, ReadGate};

pub(crate) struct SandboxEnforcer {
    pub(crate) path_guard: PathGuard,
    pub(crate) deny_list: DenyList,
    pub(crate) read_gate: ReadGate,
}

impl SandboxEnforcer {
    pub(crate) fn check_read(&self, path: &std::path::Path) -> GuardVerdict {
        self.path_guard.check_read(path)
    }

    pub(crate) async fn check_write(&self, path: &std::path::Path) -> GuardVerdict {
        self.path_guard.check_write(path).await
    }

    pub(crate) fn mark_read(&mut self, path: &std::path::Path) {
        self.read_gate.mark_read(path);
    }

    pub(crate) fn check_edit(&self, path: &std::path::Path, resolved: &std::path::Path) -> GuardVerdict {
        self.read_gate.check_edit(path, resolved)
    }
}