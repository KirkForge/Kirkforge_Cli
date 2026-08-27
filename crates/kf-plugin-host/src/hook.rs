//! Plugin-defined lifecycle hook wrapper.
//!
//! v1 hooks are shell scripts invoked with the same environment variables as
//! built-in hooks. Exit codes follow the Kimi-style fail-open convention:
//!
//! - `0` → allow
//! - `2` → deny (meaningful for pre-tool hooks)
//! - any other non-zero / timeout / crash → allow, but log a warning

use crate::env::curated_env;
use crate::sdk::Capability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

/// Hard bound on a plugin hook script's runtime, mirroring the
/// binary's 5s hook timeout (WO 43.23). Hooks are quick checks;
/// anything longer is a hang, not slowness.
#[cfg(unix)]
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// A plugin hook that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginHook {
    pub event: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
}

/// Outcome of a hook invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookVerdict {
    Allow,
    Deny,
}

/// Errors that can occur when running a hook.
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook command not found: {0}")]
    NotFound(PathBuf),
    #[error("hook failed to execute: {0}")]
    Io(#[from] std::io::Error),
}

impl PluginHook {
    /// Build a `PluginHook` from a hook capability.
    pub fn from_capability(cap: &Capability, plugin_root: &Path) -> Option<Self> {
        match cap {
            Capability::Hook { event, command } => {
                if !crate::paths::is_command_within_root(plugin_root, command) {
                    return None;
                }
                Some(Self {
                    event: event.clone(),
                    command: command.clone(),
                    plugin_root: plugin_root.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    /// Run the hook script with the given environment.
    pub fn run(&self, env: &HashMap<String, String>) -> Result<HookVerdict, HookError> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(HookError::NotFound(cmd_path));
        }

        let mut attempts = 0;
        let mut child = loop {
            let mut command = Command::new(&cmd_path);
            command
                .env_clear()
                .envs(curated_env(env))
                .current_dir(&self.plugin_root);
            // WO 43.23: own process group so the timeout watchdog's
            // kill reaches any children the script spawned.
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            match command.spawn() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 => {
                    std::thread::sleep(Duration::from_millis(10));
                    attempts += 1;
                    continue;
                }
                other => break other?,
            }
        };

        // WO 43.23 (unix): watchdog thread kills the process group at
        // the deadline — the verifier pattern (verifier.rs). On
        // timeout the hook fails open per the convention documented
        // at the top of this file.
        // ceiling: non-unix targets have no watchdog — the wait is
        // unbounded there. Upgrade path: a thread + TerminateProcess
        // dance if this crate ever needs first-class Windows support.
        #[cfg(unix)]
        let (done, killed, watchdog) = {
            let done = Arc::new(AtomicBool::new(false));
            let killed = Arc::new(AtomicBool::new(false));
            let pid = child.id();
            let handle = {
                let done = done.clone();
                let killed = killed.clone();
                std::thread::spawn(move || {
                    let deadline = Instant::now() + HOOK_TIMEOUT;
                    while !done.load(Ordering::Acquire) {
                        let now = Instant::now();
                        if now >= deadline {
                            // ESRCH (already dead) is the benign case.
                            unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
                            killed.store(true, Ordering::Release);
                            return;
                        }
                        std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
                    }
                })
            };
            (done, killed, handle)
        };

        let status = child.wait();
        #[cfg(unix)]
        {
            done.store(true, Ordering::Release);
            let _ = watchdog.join();
            if killed.load(Ordering::Acquire) {
                tracing::warn!(
                    event = %self.event,
                    command = %self.command.display(),
                    secs = HOOK_TIMEOUT.as_secs(),
                    "plugin hook timed out; process group killed; fail-open allowing"
                );
                return Ok(HookVerdict::Allow);
            }
        }

        Ok(match status?.code() {
            Some(0) => HookVerdict::Allow,
            Some(2) => HookVerdict::Deny,
            code => {
                tracing::warn!(
                    event = %self.event,
                    command = %self.command.display(),
                    exit_code = ?code,
                    "plugin hook exited non-zero; fail-open allowing"
                );
                HookVerdict::Allow
            }
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[cfg(unix)]
    fn make_hook(name: &str, body: &str) -> (tempfile::TempDir, PluginHook) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let command = std::path::PathBuf::from(name);
        let script = format!("#!/bin/sh\n{body}");
        std::fs::write(root.join(&command), script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(root.join(&command))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(root.join(&command), perms).unwrap();
        }
        let hook = PluginHook {
            event: "pre-tool-bash".into(),
            command,
            plugin_root: root.clone(),
        };
        (tmp, hook)
    }

    #[cfg(unix)]
    #[test]
    fn exit_zero_allows() {
        let (_tmp, hook) = make_hook("allow.sh", "exit 0");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Allow);
    }

    #[cfg(unix)]
    #[test]
    fn exit_two_denies() {
        let (_tmp, hook) = make_hook("deny.sh", "exit 2");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Deny);
    }

    #[cfg(unix)]
    #[test]
    fn other_exit_allows_with_warning() {
        let (_tmp, hook) = make_hook("warn.sh", "exit 1");
        assert_eq!(hook.run(&HashMap::new()).unwrap(), HookVerdict::Allow);
    }

    // WO 43.23: a hung hook is killed at its deadline — whole process
    // group — and the run fails open (Allow), per the convention
    // documented at the top of this file. Structural kill verification
    // mirroring `hung_verifier_is_killed_and_times_out` (verifier.rs):
    // the script writes its own pid, then sleeps; after run() returns
    // we poll kill(pid, 0) until ESRCH.
    #[cfg(unix)]
    #[test]
    fn hung_hook_is_killed_at_deadline_and_fails_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let pidfile = root.join("hook.pid");
        let script_path = root.join("hang.sh");
        std::fs::write(
            &script_path,
            format!("#!/bin/sh\necho $$ > {}\nsleep 60\n", pidfile.display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        let hook = PluginHook {
            event: "pre-tool-bash".into(),
            command: PathBuf::from("hang.sh"),
            plugin_root: root.clone(),
        };

        let start = Instant::now();
        let verdict = hook.run(&HashMap::new()).expect("timeout must fail open");
        assert_eq!(verdict, HookVerdict::Allow);
        // Internal 5s deadline + kill margin; far below the 60s sleep.
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "kill took {:?}",
            start.elapsed()
        );

        // The script's pid (its process-group leader) is gone.
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let rc = unsafe { libc::kill(pid, 0) };
            let gone = rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(3); // ESRCH
            if gone {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "hung hook pid {pid} still alive after kill"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
