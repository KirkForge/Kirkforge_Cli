//! Plugin-defined verifier wrapper.
//!
//! v1 verifiers are shell scripts invoked with environment variables describing
//! the event being verified. A zero exit code means the check passed; any
//! non-zero exit code fails, with stderr as the failure message.
//!
//! WO 38.3: verifier runs are bounded (unix). A hung script is killed —
//! whole process group — after [`VERIFIER_TIMEOUT`] and the run fails
//! closed, because the caller invokes `run` under the verifier-bus lock
//! and an unbounded wait would hold that lock forever.

use crate::env::curated_env;
use kf_plugin_sdk::Capability;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Hard bound on a verifier script's runtime, mirroring the binary's
/// 5s hook timeout. Verifiers are quick checks (grep-style scripts);
/// anything longer is a hang, not slowness.
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(5);

/// A plugin verifier that can be invoked.
#[derive(Debug, Clone)]
pub struct PluginVerifier {
    pub name: String,
    pub command: PathBuf,
    pub plugin_root: PathBuf,
}

/// Outcome of a verifier invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierVerdict {
    Pass,
    Fail { message: String },
}

/// Errors that can occur when running a verifier.
#[derive(Debug, thiserror::Error)]
pub enum VerifierError {
    #[error("verifier command not found: {0}")]
    NotFound(PathBuf),
    #[error("verifier failed to execute: {0}")]
    Io(#[from] std::io::Error),
    #[error("verifier timed out after {}s (process group killed)", _0.as_secs())]
    TimedOut(Duration),
}

impl PluginVerifier {
    /// Build a `PluginVerifier` from a verifier capability.
    pub fn from_capability(cap: &Capability, plugin_root: &Path) -> Option<Self> {
        match cap {
            Capability::Verifier { name, command, .. } => {
                let command = command.clone()?;
                if !crate::paths::is_command_within_root(plugin_root, &command) {
                    return None;
                }
                Some(Self {
                    name: name.clone(),
                    command,
                    plugin_root: plugin_root.to_path_buf(),
                })
            }
            _ => None,
        }
    }

    /// Run the verifier script with the given environment.
    pub fn run(&self, env: &HashMap<String, String>) -> Result<VerifierVerdict, VerifierError> {
        let cmd_path = self.plugin_root.join(&self.command);
        if !cmd_path.exists() {
            return Err(VerifierError::NotFound(cmd_path));
        }

        let mut attempts = 0;
        let child = loop {
            let mut cmd = Command::new(&cmd_path);
            cmd.env_clear()
                .envs(curated_env(env))
                .current_dir(&self.plugin_root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Own process group so the watchdog's kill reaches any
            // children the script spawned (mirrors the binary's
            // process_group helpers).
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                cmd.process_group(0);
            }
            match cmd.spawn() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 => {
                    std::thread::sleep(Duration::from_millis(10));
                    attempts += 1;
                    continue;
                }
                other => break other?,
            }
        };

        // WO 38.3 (unix): watchdog thread kills the process group at the
        // deadline. `run` is sync (the BusVerifier trait is sync), so a
        // tokio timeout is not an option here; the polling granularity
        // only delays the kill by at most 50ms.
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
                    let deadline = Instant::now() + VERIFIER_TIMEOUT;
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

        let output = child.wait_with_output()?;
        #[cfg(unix)]
        {
            done.store(true, Ordering::Release);
            let _ = watchdog.join();
            if killed.load(Ordering::Acquire) {
                return Err(VerifierError::TimedOut(VERIFIER_TIMEOUT));
            }
        }

        if output.status.success() {
            Ok(VerifierVerdict::Pass)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = if stderr.trim().is_empty() {
                format!("exited with {:?}", output.status.code())
            } else {
                stderr.trim().to_string()
            };
            Ok(VerifierVerdict::Fail { message })
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_verifier(name: &str, body: &str) -> (tempfile::TempDir, PluginVerifier) {
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
        let verifier = PluginVerifier {
            name: "test".into(),
            command,
            plugin_root: root,
        };
        (tmp, verifier)
    }

    #[cfg(unix)]
    #[test]
    fn exit_zero_passes() {
        let (_tmp, v) = make_verifier("pass.sh", "exit 0");
        assert_eq!(v.run(&HashMap::new()).unwrap(), VerifierVerdict::Pass);
    }

    #[cfg(unix)]
    #[test]
    fn non_zero_fails_with_stderr() {
        let (_tmp, v) = make_verifier("fail.sh", "echo 'bad' >&2\nexit 1");
        assert_eq!(
            v.run(&HashMap::new()).unwrap(),
            VerifierVerdict::Fail {
                message: "bad".into()
            }
        );
    }

    // WO 38.3: a hung verifier is killed at the internal deadline and
    // the run fails closed. Structural kill verification: the script
    // writes its own pid, then sleeps; after run() returns we poll
    // kill(pid, 0) until ESRCH — event-driven, no fixed sleeps.
    #[cfg(unix)]
    #[test]
    fn hung_verifier_is_killed_and_times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let pidfile = root.join("verifier.pid");
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
        let verifier = PluginVerifier {
            name: "hang".into(),
            command: PathBuf::from("hang.sh"),
            plugin_root: root.clone(),
        };

        let start = Instant::now();
        let err = verifier
            .run(&HashMap::new())
            .expect_err("hung verifier must err");
        assert!(
            matches!(err, VerifierError::TimedOut(d) if d.as_secs() == 5),
            "got: {err:?}"
        );
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
                "hung verifier pid {pid} still alive after kill"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
