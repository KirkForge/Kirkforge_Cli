//! WO 32.15 — Landlock FS-confinement integration test.
//!
//! Verifies a real bash job run through the production
//! `run_shell_with_token` path (rlimits + landlock pre_exec hook +
//! process group + capped drain) is actually confined by landlock:
//!
//! - A file INSIDE the sandbox workspace can be read (positive).
//! - A file OUTSIDE the sandbox (under a path not in the landlock
//!   allow-list) cannot be read — the `cat` fails with EACCES /
//!   permission denied / non-zero exit (negative).
//!
//! Linux-only (`#[cfg(target_os = "linux")]`); skips (not fails) on
//! kernels < 5.13 or containers without CAP_SYS_ADMIN via a runtime
//! landlock-usability probe. No `#[ignore]` — runs in CI on
//! landlock-capable kernels.

#![cfg(target_os = "linux")]

use kf_code::session::bash_runner::run_shell_with_token;
use kf_code::shared::SandboxConfig;
use std::path::PathBuf;

// Landlock syscall numbers (linux/landlock.h). Mirrors the constants in
// src/session/bash_runner/landlock.rs — duplicated here because that
// module is private and only the production `run_shell_with_token` path
// is exercised by this integration test.
const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

#[repr(C, packed)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

// All access_fs flags — requesting the broadest handled set proves the
// kernel supports landlock (create_ruleset) AND that restrict_self
// succeeds in this environment (needs CAP_SYS_ADMIN or unprivileged
// user namespaces).
const ACCESS_FS_ALL: u64 = (1 << 15) - 1;

/// True if the kernel supports landlock AND the current process can
/// actually enforce it (restrict_self succeeds). Mirrors the private
/// `landlock_usable` helper in landlock.rs:305. Tests skip (not fail)
/// when this returns false.
fn landlock_usable() -> bool {
    let attr = LandlockRulesetAttr {
        handled_access_fs: ACCESS_FS_ALL,
    };
    let fd = unsafe {
        libc::syscall(
            LANDLOCK_CREATE_RULESET,
            &attr as *const _,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0u32,
        )
    };
    if fd < 0 {
        return false;
    }
    let ok = unsafe { libc::syscall(LANDLOCK_RESTRICT_SELF, fd, 0u32) == 0 };
    unsafe { libc::close(fd as libc::c_int) };
    ok
}

/// Create a temp dir under `/var/tmp` — a path that is NOT in the
/// landlock allow-list (`SYSTEM_READ_DIRS` covers `/tmp`, `/var/lib`,
/// etc. but NOT `/var/tmp`, which is on a separate mount beneath `/`).
/// Returns `None` if `/var/tmp` is not writable on this host (skip).
fn outside_sandbox_dir() -> Option<tempfile::TempDir> {
    // /var/tmp must exist, be writable, and be distinct from /tmp
    // (landlock allows /tmp read-only, so a file under /tmp would be
    // readable even outside the workspace).
    let var_tmp = PathBuf::from("/var/tmp");
    if !var_tmp.is_dir() {
        return None;
    }
    // Probe writability.
    let probe = var_tmp.join(format!(".kf_wo32_probe_{}", std::process::id()));
    if std::fs::write(&probe, b"x").is_err() {
        return None;
    }
    std::fs::remove_file(&probe).ok();

    // Sanity: /var/tmp must NOT resolve to the same path as the default
    // temp dir (which landlock allows read-only). If they're the same,
    // the "outside" file would be readable and the test would be bogus.
    let default_tmp = std::env::temp_dir();
    if std::fs::canonicalize(&var_tmp).ok() == std::fs::canonicalize(&default_tmp).ok() {
        return None;
    }

    tempfile::Builder::new()
        .prefix("kf_wo32_outside_")
        .tempdir_in(&var_tmp)
        .ok()
}

/// A bash job confined by landlock cannot read a file outside the
/// sandbox, while a file inside the sandbox is readable.
///
/// This exercises the real production path: `run_shell_with_token`
/// with `SandboxConfig { harden: true }` installs landlock via the
/// pre_exec hook (the same hook a model-driven bash call uses).
#[tokio::test]
async fn landlock_confines_bash_job_outside_read_blocked() {
    if !landlock_usable() {
        eprintln!("skipping: landlock not usable on this kernel/environment");
        return;
    }

    let Some(outside_dir) = outside_sandbox_dir() else {
        eprintln!("skipping: /var/tmp not writable or same as default temp dir");
        return;
    };

    // Sandbox workspace — a fresh tempdir (gets full r/w under landlock).
    let sandbox = tempfile::tempdir().expect("create sandbox tempdir");
    let inside_file = sandbox.path().join("inside.txt");
    std::fs::write(&inside_file, b"INSIDE_CONTENT").expect("write inside file");

    // Outside file — under /var/tmp, NOT in the landlock allow-list.
    let outside_file = outside_dir.path().join("outside.txt");
    std::fs::write(&outside_file, b"OUTSIDE_SECRET").expect("write outside file");

    let sandbox_cfg = SandboxConfig {
        harden: true,
        ..Default::default()
    };

    // Positive: reading the inside file MUST succeed.
    let inside_path = inside_file.to_string_lossy().to_string();
    let out = run_shell_with_token(
        &format!("cat {inside_path}"),
        sandbox.path(),
        15,
        None,
        Some(&sandbox_cfg),
        &[],
    )
    .await
    .expect("inside-file run should spawn");
    assert!(
        out.status.success(),
        "cat inside-sandbox file should succeed under landlock; \
         exit {:?}, stderr: {}",
        out.status,
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        "INSIDE_CONTENT",
        "inside file contents should match"
    );

    // Negative: reading the outside file MUST fail (landlock blocks it).
    let outside_path = outside_file.to_string_lossy().to_string();
    let out = run_shell_with_token(
        &format!("cat {outside_path}"),
        sandbox.path(),
        15,
        None,
        Some(&sandbox_cfg),
        &[],
    )
    .await
    .expect("outside-file run should spawn (cat fails, not spawn)");

    // The command must NOT have succeeded — landlock should block the read.
    assert!(
        !out.status.success(),
        "cat outside-sandbox file should be BLOCKED by landlock, but it succeeded; \
         stdout: {:?}, stderr: {:?}",
        out.stdout,
        out.stderr
    );

    // Confirm the denial: stderr should mention permission/EACCES, OR
    // the stdout should be empty (cat produced no output because it
    // couldn't open the file). Either is acceptable — the exact error
    // depends on the shell and landlock version.
    let denied = out
        .stderr
        .to_ascii_lowercase()
        .contains("permission denied")
        || out.stderr.to_ascii_lowercase().contains("permission")
        || out.stdout.trim().is_empty();
    assert!(
        denied,
        "expected permission-denied error or empty stdout from blocked read; \
         got stdout: {:?}, stderr: {:?}, exit: {:?}",
        out.stdout, out.stderr, out.status
    );
}

/// Sanity: without `harden`, landlock is NOT applied — so the outside
/// file IS readable. This guards against a false pass where landlock is
/// silently disabled and the negative test above would pass for the
/// wrong reason (i.e. the file was unreadable even WITHOUT landlock).
#[tokio::test]
async fn landlock_unconfined_bash_can_read_outside() {
    if !landlock_usable() {
        eprintln!("skipping: landlock not usable on this kernel/environment");
        return;
    }

    let Some(outside_dir) = outside_sandbox_dir() else {
        eprintln!("skipping: /var/tmp not writable or same as default temp dir");
        return;
    };

    let sandbox = tempfile::tempdir().expect("create sandbox tempdir");
    let outside_file = outside_dir.path().join("outside_unconfined.txt");
    std::fs::write(&outside_file, b"OUTSIDE_READABLE").expect("write outside file");

    // harden=false → no landlock applied → outside file must be readable.
    let sandbox_cfg = SandboxConfig {
        harden: false,
        ..Default::default()
    };

    let outside_path = outside_file.to_string_lossy().to_string();
    let out = run_shell_with_token(
        &format!("cat {outside_path}"),
        sandbox.path(),
        15,
        None,
        Some(&sandbox_cfg),
        &[],
    )
    .await
    .expect("unconfined run should spawn");

    assert!(
        out.status.success(),
        "without harden, cat outside-sandbox file should succeed; \
         exit {:?}, stderr: {}",
        out.status,
        out.stderr
    );
    assert_eq!(
        out.stdout.trim(),
        "OUTSIDE_READABLE",
        "without landlock the outside file should be fully readable"
    );
}
