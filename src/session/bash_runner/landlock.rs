//! Landlock filesystem confinement for the default bash path (WO 21.7-R1).
//!
//! Provides filesystem isolation without Docker overhead. On Linux 5.13+
//! the child shell is restricted: workspace gets full r/w, system dirs get
//! read-only, home and XDG dirs get full r/w (cargo, rustup need to write
//! there). On older kernels or non-Linux, this is a no-op.
//!
//! ponytail: landlock cannot restrict network; use CLONE_NEWNET for that.
//! ceiling: landlock is an allow-list (additive). To deny a path, do not
//! add it. The "everything else is denied" invariant relies on restrict_self
//! removing the pre-landlock access scope.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

use std::os::unix::ffi::OsStrExt;

const LANDLOCK_CREATE_RULESET: libc::c_long = 444;

const LANDLOCK_ADD_RULE: libc::c_long = 445;
const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const RULE_PATH_BENEATH: libc::c_long = 1;

#[repr(C, packed)]
struct landlock_ruleset_attr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct landlock_path_beneath_attr {
    allowed_access: u64,
    parent_fd: libc::c_int,
}

const LANDLOCK_ACCESS_FS_EXECUTE: u64 = 1 << 0;
const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_READ_FILE: u64 = 1 << 2;
const LANDLOCK_ACCESS_FS_READ_DIR: u64 = 1 << 3;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

const ACCESS_FS_READ: u64 = LANDLOCK_ACCESS_FS_EXECUTE
    | LANDLOCK_ACCESS_FS_READ_FILE
    | LANDLOCK_ACCESS_FS_READ_DIR
    | LANDLOCK_ACCESS_FS_REFER;

const ACCESS_FS_WRITE: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_TRUNCATE;

const ACCESS_FS_ALL: u64 = ACCESS_FS_READ | ACCESS_FS_WRITE;

fn landlock_available() -> Option<libc::c_int> {
    let attr = landlock_ruleset_attr {
        handled_access_fs: ACCESS_FS_ALL,
    };
    let fd = unsafe {
        libc::syscall(
            LANDLOCK_CREATE_RULESET,
            &attr as *const _,
            std::mem::size_of::<landlock_ruleset_attr>(),
            0u32,
        )
    };
    if fd < 0 {
        return None;
    }
    Some(fd as libc::c_int)
}

unsafe fn landlock_add_path_rule(
    ruleset_fd: libc::c_int,
    dir_fd: libc::c_int,
    access: u64,
) -> bool {
    let attr = landlock_path_beneath_attr {
        allowed_access: access,
        parent_fd: dir_fd,
    };
    libc::syscall(
        LANDLOCK_ADD_RULE,
        ruleset_fd as libc::c_long,
        RULE_PATH_BENEATH,
        &attr as *const _,
        0u32,
    ) == 0
}

unsafe fn landlock_restrict(ruleset_fd: libc::c_int) -> bool {
    libc::syscall(LANDLOCK_RESTRICT_SELF, ruleset_fd as libc::c_long, 0u32) == 0
}

unsafe fn add_path(ruleset_fd: libc::c_int, path: &CStr, access: u64) -> bool {
    let fd = libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC);
    if fd < 0 {
        return false;
    }
    let ok = landlock_add_path_rule(ruleset_fd, fd, access);
    libc::close(fd);
    ok
}

// &CStr literals (NUL-terminated, 'static, zero-alloc) — libc::open in
// pre_exec needs a NUL-terminated path; building these in the child would
// allocate, which pre_exec forbids.
static SYSTEM_READ_DIRS: &[&CStr] = &[
    c"/usr",
    c"/bin",
    c"/sbin",
    c"/lib",
    c"/lib64",
    c"/etc",
    c"/opt",
    c"/nix",
    c"/snap",
    c"/var/lib",
    c"/tmp",
    c"/dev",
    c"/proc",
];

/// Pre-resolved paths for the landlock allow-list. Built in the parent
/// process (before fork) so the pre_exec closure never allocates.
///
/// Paths are stored as `CString`s: the old version kept `PathBuf`s and
/// passed `as_bytes().as_ptr()` to `libc::open`, which reads a
/// NUL-terminated string — the byte AFTER the path was heap garbage, so
/// short paths like `.` (the default bash workdir) opened `".<junk>"`
/// and failed with ENOENT/EINVAL, breaking every workdir-less bash call
/// (WO 30.0.5 follow-up, 2026-08-13 dogfood).
#[allow(dead_code)]
pub(crate) struct LandlockPaths {
    pub workspace: CString,
    pub home: Option<CString>,
    pub xdg_dirs: Vec<CString>,
    /// Operator-supplied extra allow-list paths (config
    /// `security.landlock_extra_paths` / `KF_CODE_LANDLOCK_EXTRA_PATHS`),
    /// granted full read/write. Operators explicitly trust these.
    pub extra: Vec<CString>,
}

/// Convert a path to a NUL-terminated CString, dropping paths with
/// interior NULs (impossible for real filesystem paths).
fn to_cstring(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_bytes()).ok()
}

/// Resolve all env vars and path computations in the parent process.
/// Returns None if landlock is not available on this kernel.
/// `extra` are operator-supplied paths added to the allow-list at full r/w.
pub(crate) fn resolve_paths(workspace: &Path, extra: &[PathBuf]) -> Option<LandlockPaths> {
    landlock_available()?;
    let workspace = to_cstring(workspace)?;
    let home = std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .and_then(|h| to_cstring(&h));
    let xdg_vars = [
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ];
    let mut xdg_dirs = Vec::new();
    for var in &xdg_vars {
        if let Ok(val) = std::env::var(var) {
            if let Some(pb) = to_cstring(&PathBuf::from(val)) {
                if !xdg_dirs.contains(&pb) {
                    xdg_dirs.push(pb);
                }
            }
        }
    }
    let extra = extra.iter().filter_map(|p| to_cstring(p)).collect();
    Some(LandlockPaths {
        workspace,
        home,
        xdg_dirs,
        extra,
    })
}

/// Apply landlock confinement. MUST be called from a pre_exec closure
/// (post-fork, pre-exec). All path resolution is already done — this
/// function only opens fds and makes syscalls, no heap allocation.
///
/// Returns Err if workspace cannot be added or restrict_self fails
/// (per WO 21.7-R5 + WO 27.1: refuse to launch unsandboxed on supported
/// kernels; the caller checks `accept_unsandboxed` to decide whether to
/// fall back with a warning or hard-fail the spawn).
pub(crate) fn apply_landlock(paths: &LandlockPaths) -> Result<(), String> {
    let ruleset_fd = match landlock_available() {
        Some(fd) => fd,
        None => return Err("landlock not available".to_string()),
    };

    // workspace: full read+write — fatal if this fails
    unsafe {
        if !add_path(ruleset_fd, &paths.workspace, ACCESS_FS_ALL) {
            libc::close(ruleset_fd);
            return Err(format!(
                "landlock: cannot add workspace rule for {:?}",
                paths.workspace
            ));
        }
    }

    // system dirs: read-only (or full for /dev) — warn on failure, continue
    for dir in SYSTEM_READ_DIRS {
        let access = if *dir == c"/dev" {
            ACCESS_FS_ALL
        } else {
            ACCESS_FS_READ
        };
        unsafe {
            if !add_path(ruleset_fd, dir, access) {
                eprintln!("landlock: warning: cannot add system dir {dir:?}");
            }
        }
    }

    // home dir: full r/w (cargo, rustup, npm need ~/.cargo, ~/.local, etc.)
    if let Some(ref home) = paths.home {
        if home != &paths.workspace {
            unsafe {
                if !add_path(ruleset_fd, home, ACCESS_FS_ALL) {
                    eprintln!("landlock: warning: cannot add home dir {home:?}");
                }
            }
        }
    }

    // XDG dirs: full r/w
    for xdg in &paths.xdg_dirs {
        if xdg != &paths.workspace {
            unsafe {
                if !add_path(ruleset_fd, xdg, ACCESS_FS_ALL) {
                    eprintln!("landlock: warning: cannot add XDG dir {xdg:?}");
                }
            }
        }
    }

    // Operator-supplied extra paths: full r/w (WO 27.1). Operators explicitly
    // trust these (e.g. a monorepo outside the workdir, a non-default cargo cache).
    for path in &paths.extra {
        if path != &paths.workspace {
            unsafe {
                if !add_path(ruleset_fd, path, ACCESS_FS_ALL) {
                    eprintln!("landlock: warning: cannot add extra path {path:?}");
                }
            }
        }
    }

    // restrict_self — fail-closed on real errors, but SKIP on EPERM (missing
    // CAP_SYS_ADMIN — common in CI containers, Docker without --cap-add, or
    // unprivileged namespaces). Distinguishing "no caps" from "real error"
    // prevents the sandbox from locking users out of environments where
    // landlock is available but not granted.
    unsafe {
        if !landlock_restrict(ruleset_fd) {
            let err = std::io::Error::last_os_error();
            libc::close(ruleset_fd);
            if err.raw_os_error() == Some(libc::EPERM) {
                eprintln!(
                    "landlock: restrict_self EPERM — no CAP_SYS_ADMIN in this \
                     environment; continuing WITHOUT filesystem confinement"
                );
                return Ok(());
            }
            return Err(format!("landlock: restrict_self failed: {err}"));
        }
        libc::close(ruleset_fd);
    }
    Ok(())
}

// `landlock_available()` only proves the ruleset syscall exists. Real
// confinement additionally needs `restrict_self`, which requires
// CAP_SYS_ADMIN. Probe both so tests skip cleanly on kernels/caps that
// can't actually confine (e.g. unprivileged CI containers).
#[cfg(test)]
pub(crate) fn landlock_usable() -> bool {
    let Some(fd) = landlock_available() else {
        return false;
    };
    let ok = unsafe { landlock_restrict(fd) };
    unsafe { libc::close(fd) };
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn landlock_probe_does_not_crash() {
        let result = landlock_available();
        if let Some(fd) = result {
            unsafe {
                libc::close(fd);
            }
        }
    }

    #[test]
    fn landlock_struct_sizes() {
        assert_eq!(
            std::mem::size_of::<landlock_path_beneath_attr>(),
            12,
            "landlock_path_beneath_attr must be 12 bytes (packed u64 + i32)"
        );
        assert_eq!(
            std::mem::size_of::<landlock_ruleset_attr>(),
            8,
            "landlock_ruleset_attr must be 8 bytes (packed u64)"
        );
    }

    #[test]
    fn landlock_access_flags_correct() {
        // ACCESS_FS_READ must NOT include WRITE_FILE (1<<1)
        assert_eq!(
            ACCESS_FS_READ & LANDLOCK_ACCESS_FS_WRITE_FILE,
            0,
            "ACCESS_FS_READ must not include WRITE_FILE"
        );
        // ACCESS_FS_WRITE must include WRITE_FILE
        assert_ne!(
            ACCESS_FS_WRITE & LANDLOCK_ACCESS_FS_WRITE_FILE,
            0,
            "ACCESS_FS_WRITE must include WRITE_FILE"
        );
        // ACCESS_FS_ALL must be the union
        assert_eq!(ACCESS_FS_ALL, ACCESS_FS_READ | ACCESS_FS_WRITE);
        // REFER and TRUNCATE must be in the right positions
        assert_eq!(LANDLOCK_ACCESS_FS_REFER, 1u64 << 13);
        assert_eq!(LANDLOCK_ACCESS_FS_TRUNCATE, 1u64 << 14);
    }

    #[test]
    fn landlock_blocks_write_outside_workspace() {
        if !landlock_usable() {
            eprintln!("skipping: landlock not available on this kernel");
            return;
        }

        let ws = tempfile::tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let inside = ws_path.join("inside.txt");

        // outside dir must stay alive until after the assertion
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("outside.txt");

        let ws_for_closure = ws_path.clone();
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            &format!(
                "echo ok > {} && echo bad > {}",
                inside.display(),
                outside.display(),
            ),
        ]);
        cmd.current_dir(&ws_path);
        unsafe {
            cmd.pre_exec(move || {
                let paths = resolve_paths(&ws_for_closure, &[])
                    .expect("landlock available (guarded by landlock_usable above)");
                match apply_landlock(&paths) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(std::io::Error::other(e)),
                }
            });
        }

        let output = cmd.output().expect("spawn child");
        // inside should succeed (or at least the file should exist)
        assert!(inside.exists(), "write inside workspace should succeed");
        // outside should be blocked — the write must have failed
        assert!(
            !outside.exists(),
            "write outside workspace should be blocked by landlock, but file was created"
        );
        // the shell should have exited non-zero due to the failed write
        assert!(
            !output.status.success(),
            "shell should exit non-zero when write outside workspace is blocked"
        );
    }

    #[test]
    fn landlock_allows_read_everywhere() {
        if !landlock_usable() {
            eprintln!("skipping: landlock not available on this kernel");
            return;
        }

        let ws = tempfile::tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();

        let ws_for_closure = ws_path.clone();
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args([
            "-c",
            "cat /dev/null /etc/hostname /tmp 2>/dev/null; echo ok",
        ]);
        cmd.current_dir(&ws_path);
        unsafe {
            cmd.pre_exec(move || {
                let paths = resolve_paths(&ws_for_closure, &[])
                    .expect("landlock available (guarded by landlock_usable above)");
                match apply_landlock(&paths) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(std::io::Error::other(e)),
                }
            });
        }

        let output = cmd.output().expect("spawn child");
        assert!(
            output.status.success(),
            "reading system dirs should succeed under landlock, got exit {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn landlock_allows_write_in_workspace() {
        if !landlock_usable() {
            eprintln!("skipping: landlock not available on this kernel");
            return;
        }

        let ws = tempfile::tempdir().unwrap();
        let ws_path = ws.path().to_path_buf();
        let test_file = ws_path.join("writable.txt");

        let ws_for_closure = ws_path.clone();
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", &format!("echo test > {}", test_file.display())]);
        cmd.current_dir(&ws_path);
        unsafe {
            cmd.pre_exec(move || {
                let paths = resolve_paths(&ws_for_closure, &[])
                    .expect("landlock available (guarded by landlock_usable above)");
                match apply_landlock(&paths) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(std::io::Error::other(e)),
                }
            });
        }

        let output = cmd.output().expect("spawn child");
        assert!(
            output.status.success(),
            "write inside workspace should succeed, got exit {:?}, stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            test_file.exists(),
            "file written inside workspace should exist"
        );
    }
}
