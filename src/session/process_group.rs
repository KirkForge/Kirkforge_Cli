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
/// descendants.
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
        // Let it exit
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        reap_child(&mut child, std::time::Duration::from_secs(1)).await;
        // If we get here without hanging, the reaping worked.
    }

    #[tokio::test]
    async fn reap_child_times_out_on_slow_process() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("spawn sleep");
        // Reap with a short timeout — the child is still sleeping.
        reap_child(&mut child, std::time::Duration::from_millis(100)).await;
        // Clean up: kill the child so it doesn't linger.
        let _ = child.start_kill();
        let _ = child.wait().await;
        // If we get here without hanging for 10s, the timeout worked.
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

    #[cfg(unix)]
    #[test]
    fn setup_process_group_does_not_panic() {
        let mut cmd = tokio::process::Command::new("true");
        setup_process_group(&mut cmd);
        // The pre_exec hook is set; we can't test the actual setpgid
        // without spawning, but the function should not panic.
    }

    #[cfg(not(unix))]
    #[test]
    fn setup_process_group_is_noop_on_non_unix() {
        let mut cmd = tokio::process::Command::new("cmd");
        setup_process_group(&mut cmd);
        // No-op; should not panic.
    }
}
