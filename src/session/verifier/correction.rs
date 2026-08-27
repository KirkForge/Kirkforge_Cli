use super::handler::VerifierHandler;
use super::types::BusEvent;
use super::types::{FixSuggestion, Verdict};
use crate::session::executor::types::VerificationOutcome;
use std::sync::Arc;
use std::time::Duration;

/// Hard bound on formatter subprocesses (WO 38.3). Matches the hook
/// runner's 5s: formatters run on a single file and should finish in
/// well under a second; anything longer is a hang, not slowness.
const FORMATTER_TIMEOUT_SECS: u64 = 5;
const FORMATTER_TIMEOUT: Duration = Duration::from_secs(FORMATTER_TIMEOUT_SECS);

// ── Correction Loop ─────────────────────────────────────────────────────

/// Manages the correction loop: after tool execution, check verifiers,
/// apply auto-fixes, and report results back to the conversation.
pub struct CorrectionLoop {
    verifier_handler: Arc<VerifierHandler>,
    max_iterations: usize,
}

impl CorrectionLoop {
    /// Create a new correction loop.
    pub fn new(verifier_handler: Arc<VerifierHandler>) -> Self {
        Self {
            verifier_handler,
            max_iterations: 3,
        }
    }

    /// Access the verifier handler so the executor can mutate slots during
    /// live plugin reload.
    pub fn verifier_handler(&self) -> Arc<VerifierHandler> {
        self.verifier_handler.clone()
    }

    /// Create with a custom iteration limit.
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        Self { ..self }
    }

    /// Run the correction loop after a tool execution event.
    ///
    /// Re-checks after each auto-fix to catch cascading issues.
    /// Returns a list of correction messages that should be appended to
    /// the conversation as tool results.
    pub async fn run(&self, event: &BusEvent) -> Vec<CorrectionResult> {
        let mut results = Vec::new();

        for _iteration in 0..self.max_iterations {
            let (verdict, decisive_name) = self.verifier_handler.verify_event(event).await;
            match verdict {
                Verdict::Clean => break,
                Verdict::Skipped(reason) => {
                    results.push(CorrectionResult {
                        verifier: decisive_name.clone(),
                        outcome: VerificationOutcome::Skipped,
                        message: format!("verification skipped: {reason}"),
                        fix: None,
                        file: None,
                        line: None,
                    });
                    break;
                }
                Verdict::Fixable(fix) => {
                    // A fix with no concrete text replacement but with an
                    // external command is a formatter-style fix (e.g. rustfmt).
                    let (applied, message, is_suggestion) =
                        if fix.original.is_empty() && fix.replacement.is_empty() {
                            if let Some(ref cmd) = fix.command {
                                let ok = apply_command_fix(
                                    cmd,
                                    &fix.file,
                                    &self.verifier_handler.path_guard,
                                )
                                .await;
                                (
                                    ok,
                                    if ok {
                                        format!(
                                            "Auto-formatted: {} — {}",
                                            fix.severity, fix.description
                                        )
                                    } else {
                                        format!(
                                            "Failed to run formatter: {} — {}",
                                            fix.severity, fix.description
                                        )
                                    },
                                    false,
                                )
                            } else {
                                // The verifier knows something is wrong but
                                // cannot provide a deterministic text fix.
                                // Return the suggestion to the model as an
                                // informational tool result.
                                (
                                    true,
                                    format!(
                                        "Verifier suggestion: {} — {} ({})",
                                        fix.severity,
                                        fix.description,
                                        fix.file.display()
                                    ),
                                    true,
                                )
                            }
                        } else {
                            let ok = apply_text_fix(&fix, &self.verifier_handler.path_guard).await;
                            (
                                ok,
                                if ok {
                                    format!("Auto-fixed: {} — {}", fix.severity, fix.description)
                                } else {
                                    format!(
                                        "Failed to auto-fix: {} — {}",
                                        fix.severity, fix.description
                                    )
                                },
                                false,
                            )
                        };

                    let file = fix.file.clone();
                    let line = fix.line;
                    let outcome = if is_suggestion {
                        VerificationOutcome::Suggestion
                    } else if applied {
                        VerificationOutcome::Fixed
                    } else {
                        VerificationOutcome::Failed
                    };
                    results.push(CorrectionResult {
                        verifier: decisive_name.clone(),
                        outcome,
                        message,
                        fix: Some(fix),
                        file: Some(file),
                        line,
                    });
                    if !applied || is_suggestion {
                        break; // can't fix, or suggestion only → stop looping
                    }
                    // A fix mutated disk content — any cached Clean verdict
                    // for this event's path is now stale. Drop it so the next
                    // verify_event re-runs verifiers against the fixed file.
                    if let Some(path) = event_path(event) {
                        self.verifier_handler.invalidate_cache(&path);
                    }
                }
                Verdict::Unfixable(err) => {
                    results.push(CorrectionResult {
                        verifier: decisive_name.clone(),
                        outcome: VerificationOutcome::Failed,
                        message: format!(
                            "Verification failed: {} — {}",
                            err.description, err.details
                        ),
                        fix: None,
                        file: err.file.clone(),
                        line: err.line,
                    });
                    break; // unfixable → stop
                }
            }
        }

        results
    }

    pub fn max_iterations(&self) -> usize {
        self.max_iterations
    }
}

/// Result of a correction attempt.
///
/// `outcome` carries the typed `Verdict` discriminant (WO 45.36) so a
/// consumer can distinguish `Skipped` from `Clean`/`Fixed`/`Suggestion`/
/// `Failed`. The prior `success: bool` flattened `Skipped` into `true`,
/// indistinguishable from a clean verdict.
#[derive(Debug, Clone)]
pub struct CorrectionResult {
    pub verifier: String,
    pub outcome: VerificationOutcome,
    pub message: String,
    pub fix: Option<FixSuggestion>,
    pub file: Option<std::path::PathBuf>,
    pub line: Option<u32>,
}

/// Extract the file path from a `FileWrite`/`Edit` event. Returns `None`
/// for events that don't target a specific file (BashExec, ToolError, …).
fn event_path(event: &BusEvent) -> Option<std::path::PathBuf> {
    match event {
        BusEvent::FileWrite(e) => Some(e.path.clone()),
        BusEvent::Edit(e) => Some(e.path.clone()),
        _ => None,
    }
}

/// Apply a text-based fix suggestion to the filesystem.
/// Replaces only the first occurrence of the original text.
///
/// The target path is checked against the session [`PathGuard`] before
/// any read or write so auto-fixes cannot escape the sandbox.
async fn apply_text_fix(
    fix: &FixSuggestion,
    path_guard: &crate::session::access::PathGuard,
) -> bool {
    let path = &fix.file;

    // Sandbox / deny-list gate. Treat the fix like a write operation.
    match path_guard.check_write(path).await {
        crate::session::access::GuardVerdict::Allowed(_) => {}
        crate::session::access::GuardVerdict::Denied(msg) => {
            tracing::warn!(
                description = %fix.description,
                file = %path.display(),
                reason = %msg,
                "auto-fix refused: path guard denied write"
            );
            return false;
        }
    }

    if fix.original.is_empty() {
        tracing::warn!(
            description = %fix.description,
            file = %path.display(),
            "auto-fix refused: empty original"
        );
        return false;
    }

    if !path.exists() {
        return false;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if !content.contains(&fix.original) {
        return false;
    }
    // WO 47.19: same symlink-swap TOCTOU walk the file tools get
    // (dispatch.rs `symlink_swap_denied`). The verifier's write happens
    // after the probe/read, so a same-batch bash call (or a fix.file that
    // was already a symlink) would make `std::fs::write` follow the link
    // and O_TRUNC the target.
    // ponytail: the walk is not atomic with the write — a swap inside
    // that micro-window still slips through; upgrade path is
    // openat2(RESOLVE_NO_SYMLINKS) at the write site (same ceiling as
    // the dispatch.rs walk).
    if let Some(msg) = crate::session::executor::dispatch::symlink_swap_denied(path) {
        tracing::warn!(
            description = %fix.description,
            file = %path.display(),
            reason = %msg,
            "auto-fix refused: symlink swap detected"
        );
        return false;
    }
    let new_content = content.replacen(&fix.original, &fix.replacement, 1);
    std::fs::write(path, new_content).is_ok()
}

/// Apply a formatter-style fix by running an external command on the file.
///
/// `ceiling:` the command is spawned inheriting the user's environment
/// (no env clearing here, unlike the plugin-verifier subprocess in
/// `plugin.rs`). This is consistent with the threat model: only built-in
/// verifiers and trusted (signed, validated) plugins can emit a
/// `FixSuggestion.command`, so they are trusted to run formatter
/// invocations in the session env. Sanitizing or env-clearing the
/// command would change behaviour (e.g. drop `PATH` lookups for
/// `rustfmt`) without a security gain, since the command author is
/// already trusted. See ADR-054 for the sandbox-rlimit path that still
/// applies when `--harden` is set.
///
/// `ceiling:` the command string is parsed with `split_whitespace`, so
/// only single-word commands and whitespace-separated arg lists are
/// supported (e.g. `rustfmt`, `rustfmt --edition 2021`). Quoted args,
/// shell operators, or args containing whitespace are NOT handled —
/// `apply_command_fix` is intended for zero-arg / simple-arg formatter
/// invocations only. Using `shlex` for full shell-quoting would add a
/// dependency; the binary is size-optimized (`opt-level = "z"`), so the
/// ceiling is documented rather than closed.
async fn apply_command_fix(
    command: &str,
    path: &std::path::Path,
    path_guard: &crate::session::access::PathGuard,
) -> bool {
    // Sandbox / deny-list gate.
    match path_guard.check_write(path).await {
        crate::session::access::GuardVerdict::Allowed(_) => {}
        crate::session::access::GuardVerdict::Denied(msg) => {
            tracing::warn!(
                command = %command,
                file = %path.display(),
                reason = %msg,
                "formatter refused: path guard denied write"
            );
            return false;
        }
    }

    if !path.exists() {
        return false;
    }

    // WO 47.19: the formatter receives the raw path — a swapped symlink
    // makes it rewrite the target (same class as the apply_text_fix write).
    if let Some(msg) = crate::session::executor::dispatch::symlink_swap_denied(path) {
        tracing::warn!(
            command = %command,
            file = %path.display(),
            reason = %msg,
            "formatter refused: symlink swap detected"
        );
        return false;
    }

    // Split the command string on whitespace for simple invocations.
    // This covers `rustfmt`, `rustfmt --edition 2021`, etc.
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    let (cmd, args) = (parts[0], &parts[1..]);
    // WO 38.3: mirror the hooks discipline — own process group, null
    // stdin, kill on drop, and a hard timeout so a hung formatter
    // cannot stall the turn (this wait sits outside the per-tool
    // timeout). Same 5s bound the hook runner uses.
    let mut proc = tokio::process::Command::new(cmd);
    proc.args(args)
        .arg(path.as_os_str())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    crate::session::process_group::setup_process_group(&mut proc);
    let mut child = match proc.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                command = %command,
                file = %path.display(),
                error = %e,
                "formatter command failed to spawn"
            );
            return false;
        }
    };

    let status = match tokio::time::timeout(FORMATTER_TIMEOUT, child.wait()).await {
        Ok(res) => res,
        Err(_) => {
            crate::session::process_group::kill_process_group(&mut child);
            tracing::warn!(
                command = %command,
                file = %path.display(),
                timeout_secs = FORMATTER_TIMEOUT_SECS,
                "formatter timed out; killed process group"
            );
            return false;
        }
    };
    match status {
        Ok(status) => status.success(),
        Err(e) => {
            tracing::warn!(
                command = %command,
                file = %path.display(),
                error = %e,
                "formatter command did not exit cleanly"
            );
            false
        }
    }
}

// ── Tests (private-helper coverage — must live next to apply_text_fix / apply_command_fix) ─

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::test_util::remove_test_file;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_apply_text_fix_basic() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fix_test.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let fix = FixSuggestion {
            description: "unused variable".into(),
            file: path.clone(),
            original: "let x = 1;".into(),
            replacement: "let _x = 1;".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };

        assert!(apply_text_fix(&fix, &crate::session::access::PathGuard::default()).await);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "let _x = 1;");
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_nonexistent_file() {
        let fix = FixSuggestion {
            description: "fix".into(),
            file: PathBuf::from("/tmp/kf_code_nonexistent_fix.txt"),
            original: "old".into(),
            replacement: "new".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
    }

    #[tokio::test]
    async fn test_apply_text_fix_original_not_found() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fix_nomatch.txt");
        std::fs::write(&path, "hello world").unwrap();

        let fix = FixSuggestion {
            description: "fix".into(),
            file: path.clone(),
            original: "not present".into(),
            replacement: "replacement".into(),
            severity: "error".into(),
            command: None,
            line: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default()).await);
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_denied_by_path_guard() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fix_denied.pem");
        std::fs::write(&path, "secret").unwrap();

        let guard = crate::session::access::PathGuard {
            deny_extensions: vec![".pem".into()],
            ..crate::session::access::PathGuard::default()
        };
        let fix = FixSuggestion {
            description: "fix".into(),
            file: path.clone(),
            original: "secret".into(),
            replacement: "public".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        assert!(!apply_text_fix(&fix, &guard).await);
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_empty_replacement_deletes() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fix_delete.txt");
        std::fs::write(&path, "use std::fs;\nfn main() {}\n").unwrap();

        let fix = FixSuggestion {
            description: "remove unused import".into(),
            file: path.clone(),
            original: "use std::fs;\n".into(),
            replacement: "".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        assert!(apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_empty_original_refused() {
        let fix = FixSuggestion {
            description: "fix".into(),
            file: PathBuf::from("/tmp/kf_code_empty_original.txt"),
            original: "".into(),
            replacement: "new".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
    }

    // WO 47.19: a fix.file that is (or was swapped for) a symlink must be
    // refused — std::fs::write follows the link and O_TRUNCs the target.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_apply_text_fix_symlink_target_refused() {
        let dir = std::env::temp_dir();
        let target = dir.join(format!("kf_code_fix_target_{}.txt", std::process::id()));
        let link = dir.join(format!("kf_code_fix_link_{}.txt", std::process::id()));
        std::fs::write(&target, "let x = 1;").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let fix = FixSuggestion {
            description: "unused variable".into(),
            file: link.clone(),
            original: "let x = 1;".into(),
            replacement: "let _x = 1;".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default()).await);
        // The symlink target must be untouched.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "let x = 1;");
        remove_test_file(&link);
        remove_test_file(&target);
    }

    // WO 47.19: the formatter must not run through a (swapped) symlink —
    // denied before spawn, so even a no-op command reports failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_apply_command_fix_symlink_target_refused() {
        let dir = std::env::temp_dir();
        let target = dir.join(format!("kf_code_fmt_target_{}.txt", std::process::id()));
        let link = dir.join(format!("kf_code_fmt_link_{}.txt", std::process::id()));
        std::fs::write(&target, "hello world").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(
            !apply_command_fix("true", &link, &crate::session::access::PathGuard::default()).await,
            "symlinked fix.file must be refused before the formatter spawns"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello world");
        remove_test_file(&link);
        remove_test_file(&target);
    }

    #[tokio::test]
    async fn test_apply_command_fix_runs_formatter() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fmt_test.txt");
        std::fs::write(&path, "hello world").unwrap();

        // `true` is a harmless no-op command that exits successfully.
        assert!(
            apply_command_fix("true", &path, &crate::session::access::PathGuard::default(),).await
        );

        // `false` exits unsuccessfully.
        assert!(
            !apply_command_fix(
                "false",
                &path,
                &crate::session::access::PathGuard::default(),
            )
            .await
        );

        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_command_fix_denied_by_path_guard_returns_false() {
        let dir = std::env::temp_dir();
        let path = dir.join("kf_code_fmt_guarded.pem");
        std::fs::write(&path, "secret").unwrap();

        let guard = crate::session::access::PathGuard {
            deny_extensions: vec![".pem".into()],
            ..crate::session::access::PathGuard::default()
        };
        assert!(
            !apply_command_fix("true", &path, &guard).await,
            "path-guard denial must block command fix"
        );
        remove_test_file(&path);
    }

    // WO 38.3: a hung formatter is killed at the internal deadline and
    // the whole process group (script + its sleep child) is gone.
    // Event-driven: the child writes its own pid first, then sleeps;
    // after apply_command_fix returns we poll kill(pid, 0) until the
    // kernel reports ESRCH — no fixed sleep, bounded window.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_apply_command_fix_kills_hung_formatter() {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("hung-fmt.sh");
        let pidfile = tmp.path().join("fmt.pid");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho $$ > {}\nsleep 60\n", pidfile.display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        let target = tmp.path().join("target.rs");
        std::fs::write(&target, "fn main() {}\n").unwrap();

        let start = std::time::Instant::now();
        let ok = apply_command_fix(
            &script.to_string_lossy(),
            &target,
            &crate::session::access::PathGuard::default(),
        )
        .await;
        assert!(!ok, "hung formatter must report failure");
        // Internal 5s deadline + kill margin; far below the 60s sleep.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(30),
            "kill took {:?}",
            start.elapsed()
        );

        // Structural: the script's pid (group leader) is gone.
        let pid: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let rc = unsafe { kill(pid, 0) };
            let gone = rc == -1 && std::io::Error::last_os_error().raw_os_error() == Some(3); // ESRCH
            if gone {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "hung formatter pid {pid} still alive after kill"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    #[test]
    fn correction_loop_new_uses_default_max_iterations() {
        let slots = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::slots::VerifierSlots::new(),
        ));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler.clone());
        assert_eq!(loop_.max_iterations(), 3);
        assert!(
            std::sync::Arc::ptr_eq(&loop_.verifier_handler(), &handler),
            "verifier_handler must return the same Arc"
        );
    }

    #[test]
    fn correction_loop_with_max_iterations_overrides_default() {
        let slots = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::slots::VerifierSlots::new(),
        ));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(7);
        assert_eq!(loop_.max_iterations(), 7);
    }

    #[test]
    fn correction_loop_with_max_iterations_zero_allows_zero_iterations() {
        let slots = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::slots::VerifierSlots::new(),
        ));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(0);
        assert_eq!(loop_.max_iterations(), 0);
    }

    #[tokio::test]
    async fn correction_loop_run_returns_empty_for_clean_event() {
        let slots = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::slots::VerifierSlots::new(),
        ));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler);
        let event = crate::session::verifier::types::BusEvent::FileRead(
            crate::session::verifier::types::FileReadEvent {
                path: std::path::PathBuf::from("x.rs"),
                size_bytes: 1,
                truncated: false,
            },
        );
        let results = loop_.run(&event).await;
        assert!(results.is_empty(), "empty slots → Clean → no results");
    }

    #[tokio::test]
    async fn correction_loop_run_with_zero_iterations_returns_empty() {
        let slots = std::sync::Arc::new(std::sync::RwLock::new(
            super::super::slots::VerifierSlots::new(),
        ));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(0);
        let event = crate::session::verifier::types::BusEvent::FileRead(
            crate::session::verifier::types::FileReadEvent {
                path: std::path::PathBuf::from("x.rs"),
                size_bytes: 1,
                truncated: false,
            },
        );
        let results = loop_.run(&event).await;
        assert!(
            results.is_empty(),
            "max_iterations=0 must not run any iteration"
        );
    }

    #[test]
    fn correction_result_struct_has_expected_fields() {
        let fix = FixSuggestion {
            description: "d".into(),
            file: PathBuf::from("x.rs"),
            original: "a".into(),
            replacement: "b".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        };
        let cr = CorrectionResult {
            verifier: "v".into(),
            outcome: VerificationOutcome::Fixed,
            message: "ok".into(),
            fix: Some(fix.clone()),
            file: Some(fix.file.clone()),
            line: None,
        };
        assert_eq!(cr.verifier, "v");
        assert_eq!(cr.outcome, VerificationOutcome::Fixed);
        assert!(cr.outcome.is_success());
        assert_eq!(cr.message, "ok");
        assert_eq!(cr.fix.as_ref().unwrap().file, fix.file);
    }

    /// WO 15.8 (2.4): the correction loop must populate
    /// `CorrectionResult.verifier` with the decisive verifier's `name()`,
    /// not the hard-coded `"verifier"` (which produced the useless
    /// `verifier:verifier` tool name the model saw).
    #[tokio::test]
    async fn correction_loop_run_carries_decisive_verifier_name() {
        use super::super::types::{Verdict, Verifier};
        struct StubFixableVerifier;
        #[async_trait::async_trait]
        impl Verifier for StubFixableVerifier {
            fn name(&self) -> &str {
                "lint"
            }
            fn priority(&self) -> u8 {
                1
            }
            async fn verify(&self, _event: &BusEvent) -> Verdict {
                Verdict::Fixable(FixSuggestion {
                    description: "unused import".into(),
                    file: PathBuf::from("/tmp/none.rs"),
                    original: "use foo;".into(),
                    replacement: "".into(),
                    severity: "warning".into(),
                    command: None,
                    line: None,
                })
            }
        }
        let mut slots_inner = super::super::slots::VerifierSlots::new();
        slots_inner
            .register(std::sync::Arc::new(StubFixableVerifier))
            .unwrap();
        let slots = std::sync::Arc::new(std::sync::RwLock::new(slots_inner));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(1);
        let event = crate::session::verifier::types::BusEvent::Edit(
            crate::session::verifier::types::EditEvent {
                path: PathBuf::from("/tmp/none.rs"),
                diff: "@@ -1 +1 @@\n-use foo;\n+".into(),
            },
        );
        let results = loop_.run(&event).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].verifier, "lint",
            "verifier name must be the decisive verifier's name(), not \"verifier\""
        );
    }

    /// WO 22.10-R1: Skipped verdicts must produce a CorrectionResult so the
    /// model can see that verification was skipped. The handler folds
    /// individual-verifier `Skipped` to `Clean` (see
    /// `handler_verify_event_skipped_verdict`); the only aggregate
    /// `Verdict::Skipped` the correction loop ever sees is the handler's
    /// ToolError short-circuit (handler.rs verify_event). So this test drives
    /// the loop with a `BusEvent::ToolError` and asserts the loop's Skipped
    /// branch produces exactly one CorrectionResult.
    #[tokio::test]
    async fn correction_loop_skipped_verdict_produces_result() {
        let slots_inner = super::super::slots::VerifierSlots::new();
        let slots = std::sync::Arc::new(std::sync::RwLock::new(slots_inner));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(1);
        let event = crate::session::verifier::types::BusEvent::ToolError(
            crate::session::verifier::types::ToolErrorEvent {
                tool: "bash".into(),
                error: "exit code 1".into(),
            },
        );
        let results = loop_.run(&event).await;
        assert_eq!(results.len(), 1, "Skipped verdict must produce one result");
        assert_eq!(results[0].verifier, "aggregate");
        assert_eq!(
            results[0].outcome,
            VerificationOutcome::Skipped,
            "Skipped is not a failure"
        );
        assert!(results[0].outcome.is_success());
        assert_eq!(
            results[0].message,
            "verification skipped: tool-error event: no verifiers act on ToolError"
        );
        assert!(results[0].fix.is_none());
        assert!(results[0].file.is_none());
        assert!(results[0].line.is_none());
    }

    /// WO 25.14-R4: line from FixSuggestion propagates into CorrectionResult.
    #[tokio::test]
    async fn correction_loop_propagates_line_from_fix_suggestion() {
        use super::super::types::{Verdict, Verifier};
        struct StubLineVerifier;
        #[async_trait::async_trait]
        impl Verifier for StubLineVerifier {
            fn name(&self) -> &str {
                "lint"
            }
            fn priority(&self) -> u8 {
                1
            }
            async fn verify(&self, _event: &BusEvent) -> Verdict {
                Verdict::Fixable(FixSuggestion {
                    description: "unused import".into(),
                    file: PathBuf::from("/tmp/none.rs"),
                    original: "use foo;".into(),
                    replacement: "".into(),
                    severity: "warning".into(),
                    command: None,
                    line: Some(42),
                })
            }
        }
        let mut slots_inner = super::super::slots::VerifierSlots::new();
        slots_inner
            .register(std::sync::Arc::new(StubLineVerifier))
            .unwrap();
        let slots = std::sync::Arc::new(std::sync::RwLock::new(slots_inner));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(1);
        let event = crate::session::verifier::types::BusEvent::Edit(
            crate::session::verifier::types::EditEvent {
                path: PathBuf::from("/tmp/none.rs"),
                diff: "@@ -1 +1 @@\n-use foo;\n+".into(),
            },
        );
        let results = loop_.run(&event).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].line,
            Some(42),
            "line from FixSuggestion must propagate into CorrectionResult"
        );
    }

    /// WO 25.14-R4: line from VerificationError propagates into CorrectionResult.
    #[tokio::test]
    async fn correction_loop_propagates_line_from_verification_error() {
        use super::super::types::{Verdict, VerificationError, Verifier};
        struct StubErrLineVerifier;
        #[async_trait::async_trait]
        impl Verifier for StubErrLineVerifier {
            fn name(&self) -> &str {
                "build"
            }
            fn priority(&self) -> u8 {
                1
            }
            async fn verify(&self, _event: &BusEvent) -> Verdict {
                Verdict::Unfixable(VerificationError {
                    description: "build error".into(),
                    file: Some(PathBuf::from("/tmp/x.rs")),
                    details: "oops".into(),
                    line: Some(7),
                })
            }
        }
        let mut slots_inner = super::super::slots::VerifierSlots::new();
        slots_inner
            .register(std::sync::Arc::new(StubErrLineVerifier))
            .unwrap();
        let slots = std::sync::Arc::new(std::sync::RwLock::new(slots_inner));
        let handler = std::sync::Arc::new(super::super::handler::VerifierHandler::new(
            slots,
            crate::session::access::PathGuard::default(),
        ));
        let loop_ = CorrectionLoop::new(handler).with_max_iterations(1);
        let event = crate::session::verifier::types::BusEvent::Edit(
            crate::session::verifier::types::EditEvent {
                path: PathBuf::from("/tmp/x.rs"),
                diff: "@@ -1 +1 @@".into(),
            },
        );
        let results = loop_.run(&event).await;
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].line,
            Some(7),
            "line from VerificationError must propagate into CorrectionResult"
        );
    }
}
