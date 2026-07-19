use super::handler::VerifierHandler;
use super::types::{FixSuggestion, Verdict};
use crate::session::event_bus::BusEvent;
use std::sync::Arc;

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
            let verdict = self.verifier_handler.verify_event(event).await;
            match verdict {
                Verdict::Clean | Verdict::Skipped(_) => break,
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

                    results.push(CorrectionResult {
                        verifier: "verifier".into(),
                        success: applied,
                        message,
                        fix: Some(fix),
                    });
                    if !applied || is_suggestion {
                        break; // can't fix, or suggestion only → stop looping
                    }
                }
                Verdict::Unfixable(err) => {
                    results.push(CorrectionResult {
                        verifier: "verifier".into(),
                        success: false,
                        message: format!(
                            "Verification failed: {} — {}",
                            err.description, err.details
                        ),
                        fix: None,
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
#[derive(Debug, Clone)]
pub struct CorrectionResult {
    pub verifier: String,
    pub success: bool,
    pub message: String,
    pub fix: Option<FixSuggestion>,
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
    let new_content = content.replacen(&fix.original, &fix.replacement, 1);
    std::fs::write(path, new_content).is_ok()
}

/// Apply a formatter-style fix by running an external command on the file.
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

    // Split the command string on whitespace for simple invocations.
    // This covers `rustfmt`, `rustfmt --edition 2021`, etc.
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return false;
    }
    let (cmd, args) = (parts[0], &parts[1..]);
    let mut child = match tokio::process::Command::new(cmd)
        .args(args)
        .arg(path.as_os_str())
        .spawn()
    {
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

    match child.wait().await {
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
        let path = dir.join("kirkforge_fix_test.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let fix = FixSuggestion {
            description: "unused variable".into(),
            file: path.clone(),
            original: "let x = 1;".into(),
            replacement: "let _x = 1;".into(),
            severity: "warning".into(),
            command: None,
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
            file: PathBuf::from("/tmp/kirkforge_nonexistent_fix.txt"),
            original: "old".into(),
            replacement: "new".into(),
            severity: "warning".into(),
            command: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
    }

    #[tokio::test]
    async fn test_apply_text_fix_original_not_found() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fix_nomatch.txt");
        std::fs::write(&path, "hello world").unwrap();

        let fix = FixSuggestion {
            description: "fix".into(),
            file: path.clone(),
            original: "not present".into(),
            replacement: "replacement".into(),
            severity: "error".into(),
            command: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default()).await);
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_denied_by_path_guard() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fix_denied.pem");
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
        };
        assert!(!apply_text_fix(&fix, &guard).await);
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_empty_replacement_deletes() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fix_delete.txt");
        std::fs::write(&path, "use std::fs;\nfn main() {}\n").unwrap();

        let fix = FixSuggestion {
            description: "remove unused import".into(),
            file: path.clone(),
            original: "use std::fs;\n".into(),
            replacement: "".into(),
            severity: "warning".into(),
            command: None,
        };
        assert!(apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        remove_test_file(&path);
    }

    #[tokio::test]
    async fn test_apply_text_fix_empty_original_refused() {
        let fix = FixSuggestion {
            description: "fix".into(),
            file: PathBuf::from("/tmp/kirkforge_empty_original.txt"),
            original: "".into(),
            replacement: "new".into(),
            severity: "warning".into(),
            command: None,
        };
        assert!(!apply_text_fix(&fix, &crate::session::access::PathGuard::default(),).await);
    }

    #[tokio::test]
    async fn test_apply_command_fix_runs_formatter() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fmt_test.txt");
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
}
