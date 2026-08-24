//! Process group helpers for cleaning up child processes and all of
//! their descendants together.
//!
//! Placing a shell into a new process group before exec lets a later
//! `killpg(..., SIGKILL)` reach every descendant the shell forked. Without
//! this, a timeout that kills only the immediate shell leaves grandchildren
//! alive — they keep stdout/stderr pipes open and can block drain tasks
//! forever, erasing partial output.

use std::time::Duration;
use tokio::process::{Child, Command};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(unix)]
extern "C" {
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn killpg(pgrp: i32, sig: i32) -> i32;
}

#[cfg(unix)]
const SIGKILL: i32 = 9;

/// Put the child into a new process group so a later signal can reach all
/// descendants. On Linux the child also requests `PR_SET_PDEATHSIG`, so it
/// dies with the parent even when the parent aborts or is SIGKILLed.
///
/// On non-Unix targets this is a no-op: there is no process group concept
/// available through `std::process`, so callers fall back to killing the
/// immediate child.
#[cfg(unix)]
pub fn setup_process_group(cmd: &mut Command) {
    unsafe {
        cmd.as_std_mut().pre_exec(|| {
            // In a post-fork pre-exec hook we cannot call logging or
            // allocation; ignore the result and continue exec.
            #[allow(unused_must_use)]
            {
                setpgid(0, 0);
                // WO 43.23: `panic = "abort"` and SIGKILL run no Drop, so
                // PDEATHSIG is the only mechanism that kills children when
                // the parent dies; every subsystem spawning through this
                // helper inherits the coverage.
                #[cfg(target_os = "linux")]
                {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                }
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn setup_process_group(_cmd: &mut Command) {}

/// Kill a child process and, on Unix, its entire process group.
///
/// Use this instead of `Child::start_kill()` when you need to guarantee
/// that grandchildren cannot outlive the parent and keep pipes/resources
/// open. On non-Unix this falls back to `start_kill()`.
#[cfg(unix)]
pub fn kill_process_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe {
            if killpg(pid as i32, SIGKILL) != 0 {
                tracing::warn!(pid, "failed to kill process group");
            }
        }
    }
}

#[cfg(not(unix))]
pub fn kill_process_group(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        tracing::warn!(error = %e, "failed to start killing child process");
    }
}

/// Kill a process group by pid, without holding the `Child` handle.
///
/// Used when the watcher task parks on the child mutex inside
/// `wait().await` for the job's whole lifetime — a lock-based kill would
/// serialize behind the process's natural exit and never fire. Failure is
/// silent: ESRCH (already dead) is the common benign case here.
#[cfg(unix)]
pub fn kill_process_group_by_pid(pid: u32) {
    unsafe {
        killpg(pid as i32, SIGKILL);
    }
}

/// No process-group concept without a `Child` handle on non-Unix; the
/// watcher's `wait()` still returns when the process exits.
#[cfg(not(unix))]
pub fn kill_process_group_by_pid(_pid: u32) {}

// ── Windows Job Object tree-kill (WO 44.44 item 4) ──
//
// On Windows there is no `killpg`. `tokio::process` + `kill_on_drop` kills
// only the direct `sh` child; `sh -c "a; b"` grandchildren survive and keep
// the stdout/stderr pipes open, deadlocking the caller's drain tasks past
// their own timeout (the WO 43.26 regression). A Job Object with
// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` kills the entire process tree when
// the job handle is closed, so dropping the `JobGuard` reaps the tree.
#[cfg(windows)]
mod job_object {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// RAII guard: dropping it closes the job handle, which kills every
    /// process assigned to the job (the `KILL_ON_JOB_CLOSE` flag). Holds
    /// the only ref to the job so the kill fires exactly once.
    pub struct JobGuard {
        handle: HANDLE,
    }

    // SAFETY: JobGuard owns its HANDLE exclusively (created by
    // CreateJobObjectW, never shared/aliased). The handle is a kernel
    // object reference, not a pointer into another thread's data — moving
    // it across threads is safe. This unlocks `Send` for futures holding
    // a JobGuard across `.await` (tokio::spawn requires `Send`).
    unsafe impl Send for JobGuard {}

    impl JobGuard {
        /// Create a job object that kills its process tree when closed.
        pub fn new() -> std::io::Result<Self> {
            // SAFETY: CreateJobObjectW with null args creates an unnamed
            // job with default security. Runs in the parent (no async-
            // signal-safety constraint).
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: zeroed produces a valid all-zero POD struct; the
            // fields we need (BasicLimitInformation.LimitFlags) are set
            // immediately after.
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: handle is a valid job object; info is a POD struct.
            let ok = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                unsafe { CloseHandle(handle) };
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        /// Assign a child process (by pid) to the job. Must be called right
        /// after spawn — Windows applies the job atomically, so any
        /// descendant the child later spawns inherits the job.
        pub fn assign(&self, pid: u32) -> std::io::Result<()> {
            // SAFETY: OpenProcess with PROCESS_SET_QUOTA | PROCESS_TERMINATE
            // is the documented access for AssignProcessToJobObject; the
            // handle is closed here.
            let access = PROCESS_SET_QUOTA | PROCESS_TERMINATE;
            let proc = unsafe { OpenProcess(access, 0, pid) };
            if proc.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let ok = unsafe { AssignProcessToJobObject(self.handle, proc) };
            unsafe { CloseHandle(proc) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for JobGuard {
        fn drop(&mut self) {
            // SAFETY: we hold the only ref; closing the handle fires the
            // KILL_ON_JOB_CLOSE limit and reaps the tree.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// On Windows, create a Job Object guard and assign the child to it so a
/// later drop kills the whole process tree. On non-Windows this is a no-op
/// (Unix uses process groups via `setup_process_group`/`kill_process_group`).
/// Returns `None` if the job could not be created (best-effort: the caller
/// falls back to `kill_on_drop` on the immediate child).
#[cfg(windows)]
pub fn assign_child_to_job(child: &tokio::process::Child) -> Option<job_object::JobGuard> {
    let pid = child.id()?;
    match job_object::JobGuard::new() {
        Ok(g) => match g.assign(pid) {
            Ok(()) => Some(g),
            Err(e) => {
                tracing::warn!(error = %e, "failed to assign child to job object");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to create job object");
            None
        }
    }
}

/// Wait for a child to exit, bounded by a timeout.
///
/// This is best-effort reaping: if the child does not exit in time it
/// may become a zombie. The timeout prevents a stuck child from wedging
/// the caller indefinitely.
pub async fn reap_child(child: &mut Child, timeout: Duration) {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(_status)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "failed to reap child process"),
        Err(_) => tracing::warn!("timed out waiting for child process to exit"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reap_child_returns_on_quick_exit() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        // `true` exits near-instantly; reap_child waits for it directly
        // instead of a blind sleep that races the process under load.
        reap_child(&mut child, std::time::Duration::from_secs(1)).await;
        // Verify the child is truly gone by checking its exit status.
        let status = child.try_wait().expect("child should be waitable");
        assert!(status.is_some(), "child should have exited after reap");
    }

    #[tokio::test]
    async fn reap_child_times_out_on_slow_process() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn sleep");
        // Reap with a short timeout — the child is still sleeping.
        let start = std::time::Instant::now();
        reap_child(&mut child, std::time::Duration::from_millis(100)).await;
        let elapsed = start.elapsed();
        // Clean up: kill the child so it doesn't linger.
        let _ = child.start_kill();
        let _ = child.wait().await;
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "reap_child should time out quickly, took {elapsed:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_process_group_kills_child() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("10");
        setup_process_group(&mut cmd);
        let mut child = cmd.spawn().expect("spawn sleep");
        kill_process_group(&mut child);
        // The child should be killed; wait should return quickly.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
        assert!(result.is_ok(), "child should have been killed within 2s");
    }

    #[cfg(not(unix))]
    #[test]
    fn setup_process_group_is_noop_on_non_unix() {
        let mut cmd = tokio::process::Command::new("cmd");
        setup_process_group(&mut cmd);
        // No-op; should not panic.
    }

    // ── WO 43.23: PR_SET_PDEATHSIG ──
    //
    // The "parent" is a re-exec of this test binary running only the
    // ignored helper test below; it spawns a sleep through
    // setup_process_group, publishes the child pid, then aborts — the
    // no-Drop death path PDEATHSIG must cover. Polls kill(pid, 0) until
    // ESRCH: event-driven, no fixed sleeps.
    #[cfg(all(unix, target_os = "linux"))]
    #[test]
    fn child_dies_when_parent_aborts() {
        let pidfile = std::env::temp_dir().join(format!(
            "kf-pdeathsig-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exe = std::env::current_exe().expect("current exe");
        let out = std::process::Command::new(exe)
            .arg("--ignored")
            // Substring filter, not --exact: module_path!() carries the
            // crate prefix, which libtest's exact matcher rejects.
            .arg("pdeathsig_helper")
            .env("KF_PDEATHSIG_PIDFILE", &pidfile)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("helper should run");
        assert!(
            matches!(out.status.code(), None | Some(134)),
            "helper should have aborted (SIGABRT), got {:?}; stdout: {} stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("helper should have written the pidfile")
            .trim()
            .parse()
            .expect("pidfile should contain a pid");
        std::fs::remove_file(&pidfile).ok();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let rc = unsafe { libc::kill(pid, 0) };
            let gone = rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(3); // ESRCH
            if gone {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child {pid} survived parent abort (PDEATHSIG not applied?)"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    // Helper half of child_dies_when_parent_aborts. Ignored in normal
    // runs (ci-fast skips #[ignore]); driven with --exact --ignored by
    // the test above.
    #[cfg(all(unix, target_os = "linux"))]
    #[tokio::test]
    #[ignore = "driven by child_dies_when_parent_aborts"]
    async fn pdeathsig_helper() {
        let pidfile = std::env::var("KF_PDEATHSIG_PIDFILE").expect("pidfile env");
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        setup_process_group(&mut cmd);
        let child = cmd.spawn().expect("spawn sleep");
        let pid = child.id().expect("child pid");
        std::fs::write(&pidfile, pid.to_string()).expect("write pidfile");
        std::process::abort();
    }
}
