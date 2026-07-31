//! rlimit sandbox hardening for plugin tool subprocesses (ADR-060).
//!
//! Mirrors `bash_runner::setup_rlimits` (WO 9.8, ADR-054) so the
//! host-crate `PluginTool` spawn path can apply the same `RLIMIT_CPU`
//! / `RLIMIT_AS` / `RLIMIT_FSIZE` caps as the bin's
//! `PluginToolWrapper`. The caps come from the plugin manifest's
//! `ResourceLimits` (WO 11.5); a `None` field means "no cap for this
//! resource" (the rlimit is left at the OS default).
//!
//! Unix only: rlimits are a Unix-only concept. On Windows this is a
//! no-op (job objects are a separate API surface, out of scope per
//! ADR-054).

use kirkforge_plugin::ResourceLimits;
use std::process::Command;

/// Apply rlimits to a plugin tool child before exec (Unix only, ADR-060).
///
/// When `limits` is `Some`, the three rlimits (`RLIMIT_CPU`,
/// `RLIMIT_AS`, `RLIMIT_FSIZE`) are installed in a `pre_exec` hook
/// (post-fork, pre-exec — the only safe place to call `setrlimit` for
/// the child without affecting the parent). Each `None` field leaves
/// the resource uncapped. When `limits` is `None`, this is a no-op
/// (the default — no manifest `resource_limits` declared).
///
/// On Windows this is a no-op: rlimits are a Unix-only concept.
#[cfg(unix)]
pub(crate) fn setup_rlimits(cmd: &mut Command, limits: Option<&ResourceLimits>) {
    use std::os::unix::process::CommandExt;

    let Some(limits) = limits else {
        return;
    };

    // Snapshot the caps before entering the pre_exec hook; the hook
    // runs in a post-fork async-signal context where allocation and
    // logging are unsafe. A `u64::MAX` soft+hard pair is the rlimit
    // idiom for "no limit", so an absent field leaves the resource
    // uncapped without a conditional in the hook.
    let cpu_secs = limits.cpu_secs.unwrap_or(u64::MAX);
    let as_bytes = limits
        .memory_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(u64::MAX);
    let fsize_bytes = limits
        .filesize_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(u64::MAX);

    unsafe {
        cmd.pre_exec(move || {
            // In a post-fork pre-exec hook we cannot call logging or
            // allocation; setrlimit is async-signal-safe. Ignore
            // failures: a failed setrlimit is a degraded sandbox, not a
            // crash, and exec should still proceed so the user sees a
            // clear error from the child rather than a silent spawn
            // failure.
            #[allow(unused_must_use)]
            {
                let cpu = libc::rlimit {
                    rlim_cur: cpu_secs,
                    rlim_max: cpu_secs,
                };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu);

                let as_lim = libc::rlimit {
                    rlim_cur: as_bytes,
                    rlim_max: as_bytes,
                };
                libc::setrlimit(libc::RLIMIT_AS, &as_lim);

                let fsize = libc::rlimit {
                    rlim_cur: fsize_bytes,
                    rlim_max: fsize_bytes,
                };
                libc::setrlimit(libc::RLIMIT_FSIZE, &fsize);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub(crate) fn setup_rlimits(_cmd: &mut Command, _limits: Option<&ResourceLimits>) {
    // rlimits are a Unix-only concept; Windows job objects are a
    // separate API surface (out of scope per ADR-054). No-op.
}
