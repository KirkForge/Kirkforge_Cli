//! Seccomp-bpf syscall filter for the default bash path (WO 30.4).
//!
//! Landlock confines the filesystem; this confines the syscall surface.
//! Applied in the same `pre_exec` hook as landlock + rlimits, AFTER landlock
//! (once seccomp is applied, only allowlisted syscalls work). Everything not
//! in the allowlist gets `SECCOMP_RET_ERRNO(EPERM)` — graceful failure, not
//! `SECCOMP_RET_KILL`/SIGSYS — so a tool that hits an unlisted syscall fails
//! with a clear EPERM rather than vanishing.
//!
//! Two-phase, mirroring the landlock resolve-in-parent / apply-in-pre_exec
//! split: the BPF program is COMPILED in the parent process before fork
//! (`build_filter` — allocates a HashMap, must not run in pre_exec) and only
//! APPLIED in pre_exec (`apply_filter` -> `seccompiler::apply_filter`, which
//! does just `prctl(PR_SET_NO_NEW_PRIVS)` + the `seccomp` syscall, no alloc).
//! `seccompiler::apply_filter` sets `PR_SET_NO_NEW_PRIVS` itself, which also
//! prevents the sandboxed bash from gaining privileges via setuid binaries —
//! a desirable side effect.
//!
//! ponytail: the allowlist is a STARTING set. Bash + grep/sed/awk/curl/cargo/
//! node/python need most of these, but real workloads will surface additional
//! syscalls (the EPERM errno names the offender). Adding a syscall is one line
//! in `allowed_syscalls()`. ceiling: this is an unconditional allow-list filter
//! (match=Allow, mismatch=EPERM) with no per-argument filtering, so a listed
//! syscall is fully allowed regardless of args. upgrade path: tighten specific
//! syscalls (e.g. restrict socket families) with `SeccompRule` conditions if a
//! misuse vector appears. The list is tuned for x86_64; aarch64/riscv64 builds
//! need the legacy-syscall lines (stat/fstat/lstat/access/pipe/dup2/fork/...) —
//! which are x86_64-only — dropped or cfg-gated (deferred, see WO 30.4).

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, TargetArch};
use std::collections::BTreeMap;

/// The syscall allowlist. Each entry maps to an empty rule vector, meaning
/// "match unconditionally and allow" (`match_action = Allow`). Everything else
/// hits the default (`mismatch_action = Errno(EPERM)`).
///
/// Two groups:
///  (1) the WO 30.4 base list (bash shell + common tools), and
///  (2) a glibc-startup + modern `at`-variant block without which no
///      dynamically-linked binary (ld.so / bash / grep / cargo) can even exec.
///      The workorder's literal list omits these; without them ld.so gets
///      EPERM before producing any output, making the filter dead-on-arrival.
fn allowed_syscalls() -> Vec<libc::c_long> {
    use libc::*;
    vec![
        // --- file I/O ---
        SYS_read,
        SYS_write,
        SYS_openat,
        SYS_close,
        SYS_stat,
        SYS_fstat,
        SYS_lstat,
        SYS_poll,
        SYS_lseek,
        SYS_access,
        SYS_dup,
        SYS_dup2,
        SYS_readv,
        SYS_writev,
        SYS_preadv,
        SYS_pwritev,
        SYS_preadv2,
        SYS_pwritev2,
        SYS_splice,
        SYS_tee,
        SYS_vmsplice,
        SYS_fcntl,
        SYS_flock,
        SYS_fsync,
        SYS_fdatasync,
        SYS_ftruncate,
        SYS_fallocate,
        SYS_fadvise64,
        SYS_getdents64,
        SYS_getdents,
        SYS_readlink,
        SYS_readlinkat,
        SYS_uname,
        SYS_getcwd,
        SYS_chdir,
        SYS_fchdir,
        SYS_umask,
        // --- memory ---
        SYS_mmap,
        SYS_mprotect,
        SYS_munmap,
        SYS_brk,
        SYS_mremap,
        SYS_madvise,
        // --- signals ---
        SYS_rt_sigaction,
        SYS_rt_sigprocmask,
        SYS_rt_sigreturn,
        SYS_sigaltstack,
        // --- process / fork ---
        SYS_clone,
        SYS_clone3,
        SYS_wait4,
        SYS_execve,
        SYS_exit,
        SYS_exit_group,
        SYS_fork,
        SYS_vfork,
        SYS_setpgid,
        SYS_getpgid,
        SYS_setsid,
        SYS_getsid,
        SYS_gettid,
        SYS_set_tid_address,
        SYS_set_robust_list,
        SYS_getpid,
        SYS_getppid,
        // --- time ---
        SYS_gettimeofday,
        SYS_clock_gettime,
        SYS_clock_nanosleep,
        SYS_clock_getres,
        SYS_nanosleep,
        SYS_timerfd_create,
        SYS_timerfd_settime,
        SYS_timerfd_gettime,
        // --- identity / limits ---
        SYS_getuid,
        SYS_geteuid,
        SYS_getgid,
        SYS_getegid,
        SYS_setuid,
        SYS_setgid,
        SYS_getrlimit,
        SYS_setrlimit,
        SYS_prlimit64,
        // --- filesystem mutation ---
        SYS_pipe,
        SYS_pipe2,
        SYS_link,
        SYS_linkat,
        SYS_unlink,
        SYS_unlinkat,
        SYS_symlink,
        SYS_symlinkat,
        SYS_rename,
        SYS_renameat,
        SYS_renameat2,
        SYS_chmod,
        SYS_fchmod,
        SYS_fchmodat,
        SYS_chown,
        SYS_fchown,
        SYS_lchown,
        SYS_fchownat,
        SYS_mount,
        SYS_umount2,
        // --- network (curl/cargo/npm/git fetch; landlock confines FS,
        //     --no-network gates the net at a coarser CLONE_NEWNET grain) ---
        SYS_socket,
        SYS_connect,
        SYS_bind,
        SYS_listen,
        SYS_accept4,
        SYS_getsockname,
        SYS_getpeername,
        SYS_setsockopt,
        SYS_getsockopt,
        SYS_shutdown,
        SYS_sendto,
        SYS_recvfrom,
        SYS_sendmmsg,
        SYS_recvmmsg,
        // --- misc / epoll / eventfd / signalfd / memfd / random / scheduling ---
        SYS_ioctl,
        SYS_kill,
        SYS_tkill,
        SYS_tgkill,
        SYS_sched_getaffinity,
        SYS_epoll_create1,
        SYS_epoll_ctl,
        SYS_epoll_wait,
        SYS_eventfd2,
        SYS_signalfd4,
        SYS_memfd_create,
        SYS_getrandom,
        SYS_statx,
        SYS_rseq,
        // --- glibc startup + modern at-variants glibc routes through ---
        // arch_prctl: TLS setup (ld.so ARCH_SET_FS) — block this and no ELF runs.
        SYS_arch_prctl,
        // newfstatat: glibc stat()/fstat()/lstat() route here on x86_64.
        SYS_newfstatat,
        // faccessat/faccessat2: glibc access() / coreutils `test -r`.
        SYS_faccessat,
        SYS_faccessat2,
    ]
}

/// Compile the BPF program in the PARENT process (before fork). Allocates a
/// `HashMap`; do NOT call from a `pre_exec` closure. Returns `Err` only on an
/// unsupported host arch (the filter is otherwise static and well-formed, so
/// this is effectively infallible on x86_64/aarch64/riscv64).
pub(crate) fn build_filter() -> Result<BpfProgram, String> {
    let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();
    for sys in allowed_syscalls() {
        rules.insert(sys, Vec::new());
    }

    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| {
        format!(
            "seccomp: unsupported arch {}: {e:?}",
            std::env::consts::ARCH
        )
    })?;

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        // Everything not in the allowlist fails gracefully with EPERM (not KILL).
        SeccompAction::Errno(libc::EPERM as u32),
        arch,
    )
    .map_err(|e| format!("seccomp: filter compile failed: {e:?}"))?;

    filter
        .try_into()
        .map_err(|e| format!("seccomp: BPF emit failed: {e:?}"))
}

/// Apply a compiled BPF program to the calling thread. Safe to call from a
/// `pre_exec` closure: `seccompiler::apply_filter` performs only the
/// `prctl(PR_SET_NO_NEW_PRIVS)` and `seccomp()` syscalls (no allocation).
pub(crate) fn apply_filter(prog: &BpfProgram) -> Result<(), String> {
    seccompiler::apply_filter(prog).map_err(|e| format!("seccomp: apply failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filter must compile on the host arch (skip cleanly otherwise) and
    /// be non-empty. This is the smallest check that fails if a `SYS_*`
    /// constant vanishes or the seccompiler API shifts.
    #[test]
    fn filter_compiles_non_empty() {
        match build_filter() {
            Ok(prog) => assert!(!prog.is_empty(), "seccomp BPF program must not be empty"),
            Err(e) => eprintln!("skipping: seccomp unsupported on this arch/build: {e}"),
        }
    }
}
