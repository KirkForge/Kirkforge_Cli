//! Unified sandbox policy intake — the seam for WO 45.11.
//!
//! Today KirkForge has four non-unified enforcement surfaces (see
//! `docs/archive/workorders/45.11-sandbox-policy-unification-audit.md`):
//!
//! 1. `PathGuard` + `DenyList` + `ReadGate` — in-process path access control
//!    consumed by the file tools (`read_file`, `write_file`, `edit_file`,
//!    `glob`, `grep`, ...). NOT an OS sandbox.
//! 2. `SandboxConfig` + `LandlockPaths` + seccomp — OS-level subprocess
//!    sandbox consumed by bash fg/bg and the plugin tool wrapper. Binds in
//!    `pre_exec`, so it applies only to a *child* process.
//! 3. `kf_plugin_host::SandboxPolicy` — capability→trust-tier gating, plugin
//!    system only. The name collides with this trait on purpose: the plugin
//!    struct lives in the lib crate, this trait lives in the bin's shared
//!    module — different paths, no Rust name collision.
//! 4. MCP — no sandbox. `McpToolWrapper` forwards to a remote server.
//!
//! This module defines the *unified policy intake* — a single `SandboxOp`
//! enum every primitive could route through, and a `SandboxPolicy` trait
//! that turns an op into a `GuardVerdict`. The existing surfaces are NOT
//! migrated to consume this trait yet; that is the deferred full migration
//! (a design decision, not a bug — see `ceiling:` below).
//!
//! ponytail: seam only. The trait has one trivial impl (`AllowAll`) used by
//! the self-check test. Real impls compose the existing surfaces behind one
//! `check(&self, op)` entry point — that composition is the migration, not
//! this WO. Adding the trait without consumers is the smallest correct step
//! that makes the unification possible without breaking anything.
//!
//! ceiling: two incompatibilities block a single-impl unification, both
//! documented in the WO audit:
//! (a) File tools are in-process `std::fs` calls — landlock/seccomp bind to
//!     a child process in `pre_exec`, so they cannot apply to file tools.
//!     A unified policy behind one trait has two *enforcement* modes
//!     (in-process path guard vs child-process OS sandbox) behind one
//!     *policy* type. Codex's filesystem server is itself a separate
//!     process, which is why its one-policy model works cleanly; KirkForge's
//!     file tools are not.
//! (b) `kf-plugin-host` lib cannot import the bin's landlock module
//!     (`crates/kf-plugin-host/src/sandbox.rs:15-22`). A unified policy that
//!     carries landlock needs landlock extracted into its own crate first,
//!     or the lib's standalone spawn paths stay rlimits-only forever.
//! upgrade path: (1) extract `src/session/bash_runner/landlock.rs` into a
//! shared crate; (2) add a `LandlockSandboxPolicy` impl of this trait;
//! (3) migrate file tools to a `PathGuardSandboxPolicy` impl; (4) migrate
//! bash/plugin to the landlock impl; (5) MCP gets a no-op or egress impl.
//! Each step is independently shippable; do NOT attempt as one big-bang.

use std::path::{Path, PathBuf};

use super::access::GuardVerdict;

/// An operation a primitive wants to perform, described generically enough
/// that every enforcement surface can rule on it.
///
/// This is the *intake* — what the primitive wants to do — not the
/// *enforcement* (which differs per surface: path guard for in-process,
/// landlock+rlimits for child processes). A unified policy turns this into
/// a `GuardVerdict` via [`SandboxPolicy::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum SandboxOp<'a> {
    /// In-process read of a filesystem path (file tools: `read_file`,
    /// `read_image`, `grep`, `lsp_query`, `glob` traversal).
    /// Enforced today by `PathGuard::check_read` or `check_traversal`.
    Read { path: &'a Path },
    /// In-process write or edit of a filesystem path (file tools:
    /// `write_file`, `edit_file`, `notebook_edit`). Enforced today by
    /// `PathGuard::check_write` and `ReadGate::check_edit`.
    Write { path: &'a Path },
    /// Spawn a subprocess (bash fg/bg). Enforced today by `SandboxConfig`
    /// rlimits plus `LandlockPaths` plus seccomp in `setup_rlimits`'s
    /// `pre_exec`. The `cmd` field is the command string for the deny-list
    /// gate; `workdir` is the subprocess working directory.
    Spawn { cmd: &'a str, workdir: &'a Path },
    /// Dispatch a plugin tool (subprocess with a manifest). Enforced today
    /// by `kf_plugin_host::SandboxPolicy::required_tier` (capability→tier)
    /// and by `setup_rlimits` on the plugin subprocess. The `cap_name` field
    /// is the plugin tool name; `trust` is the effective tier.
    PluginTool { cap_name: &'a str, trust: &'a str },
    /// Call an MCP tool (remote, no local filesystem). Enforced today by
    /// **nothing** — `McpToolWrapper` forwards unchecked. This arm exists
    /// so a future egress/resource policy has a place to hook.
    /// Tracked separately in WO 45.12.
    McpCall { full_name: &'a str },
}

/// Unified sandbox policy intake — one `check` every primitive could route
/// through before acting.
///
/// Implementations compose the existing surfaces:
/// - `PathGuardSandboxPolicy` (future) — wraps `PathGuard` + `DenyList` +
///   `ReadGate`, handles `Read`/`Write`.
/// - `SubprocessSandboxPolicy` (future) — wraps `SandboxConfig` +
///   `LandlockPaths`, handles `Spawn`/`PluginTool`.
/// - `McpSandboxPolicy` (future) — handles `McpCall` (egress/timeout).
///
/// The trait is sync on purpose: the in-process path checks are sync, and
/// the subprocess spawn path's sandbox *decision* (is this cmd allowed?)
/// is also sync — the actual spawn is async, but the policy check is not.
pub trait SandboxPolicy {
    fn check(&self, op: &SandboxOp<'_>) -> GuardVerdict;
}

/// Trivial allow-all policy used by the self-check test and as the
/// migration's starting point (every surface starts as "allow" and gets
/// narrowed onto the trait one at a time).
pub struct AllowAll;

impl SandboxPolicy for AllowAll {
    fn check(&self, op: &SandboxOp<'_>) -> GuardVerdict {
        let p = match op {
            SandboxOp::Read { path } | SandboxOp::Write { path } => path.to_path_buf(),
            SandboxOp::Spawn { workdir, .. } => workdir.to_path_buf(),
            SandboxOp::PluginTool { cap_name, .. } => PathBuf::from(cap_name),
            SandboxOp::McpCall { full_name } => PathBuf::from(full_name),
        };
        GuardVerdict::Allowed(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn allow_all_permits_every_op() {
        let policy = AllowAll;
        let path = Path::new("/tmp/x");
        assert_eq!(
            policy.check(&SandboxOp::Read { path }),
            GuardVerdict::Allowed(path.to_path_buf())
        );
        assert_eq!(
            policy.check(&SandboxOp::Write { path }),
            GuardVerdict::Allowed(path.to_path_buf())
        );
        assert_eq!(
            policy.check(&SandboxOp::Spawn {
                cmd: "ls",
                workdir: path
            }),
            GuardVerdict::Allowed(path.to_path_buf())
        );
        assert_eq!(
            policy.check(&SandboxOp::PluginTool {
                cap_name: "mytool",
                trust: "Shell"
            }),
            GuardVerdict::Allowed(PathBuf::from("mytool"))
        );
        assert_eq!(
            policy.check(&SandboxOp::McpCall {
                full_name: "mcp/srv/t"
            }),
            GuardVerdict::Allowed(PathBuf::from("mcp/srv/t"))
        );
    }

    #[test]
    fn op_is_must_use() {
        // The #[must_use] attribute is the compile-time guard against
        // silently dropping a verdict. This test pins that the attribute
        // stays on the enum — removing it is a regression.
        let op = SandboxOp::Read {
            path: Path::new("/tmp"),
        };
        // Use it so there's no unused warning.
        let _ = op;
    }
}
