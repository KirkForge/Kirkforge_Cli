//! Error recovery — smart retry hints after tool failures.
//!
//! When a tool call fails (file not found, build error, permission denied),
//! this module analyzes the error and provides the model with a correction
//! hint as a follow-up user message. This lets the model self-correct instead
//! of spinning on the same failed approach.
//!
//! # Retry limits
//!
//! - Max 2 retries per turn (the third failure stops the loop)
//! - Each retry includes the specific error message and a targeted hint
//! - The retry count is tracked per-turn, not per-tool

use crate::shared::{Message, Role};

/// A recovery hint: what went wrong and how to fix it.
#[derive(Debug, Clone)]
pub struct RecoveryHint {
    /// The error message from the tool (truncated).
    pub error_summary: String,
    /// The suggested corrective action.
    pub suggestion: String,
    /// Whether this error is considered recoverable.
    pub recoverable: bool,
}

/// Analyze a tool error and produce a recovery hint.
///
/// Returns `None` if the error is not something we can give a useful hint for.
pub fn analyze_error(
    tool_name: &str,
    error_message: &str,
    args: &serde_json::Value,
) -> Option<RecoveryHint> {
    let err_lower = error_message.to_lowercase();

    // File not found patterns (after command-not-found check so "not found"
    // in tool output doesn't capture "command not found" errors)
    if err_lower.contains("no such file")
        || (err_lower.contains("not found") && !err_lower.contains("command"))
    {
        let path_hint = args
            .get("path")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("file_path").and_then(|v| v.as_str()))
            .unwrap_or("the file");

        return Some(RecoveryHint {
            error_summary: format!("File not found: {path_hint}"),
            suggestion: format!(
                "The file '{path_hint}' was not found. Try:\n\
                 1. Use `glob` to search for the correct file path\n\
                 2. Use `grep` to search for code that references this file\n\
                 3. If this is a new file you're creating, use `write_file` instead"
            ),
            recoverable: true,
        });
    }

    // Permission denied
    if err_lower.contains("permission denied") || err_lower.contains("access denied") {
        let path_hint = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("the target");

        return Some(RecoveryHint {
            error_summary: format!("Permission denied: {path_hint}"),
            suggestion: format!(
                "Access was denied for '{path_hint}'. Try:\n\
                 1. Check if you have write permissions in this directory\n\
                 2. Consider using a different output path (e.g., /tmp/)\n\
                 3. If this is a system file, the change may need to be applied differently"
            ),
            recoverable: true,
        });
    }

    // Build/compile errors
    if tool_name == "bash"
        && (err_lower.contains("error:")
            || err_lower.contains("failed")
            || err_lower.contains("cannot find"))
    {
        if err_lower.contains("cargo") || err_lower.contains("rustc") {
            return Some(RecoveryHint {
                error_summary: "Build/compile error".to_string(),
                suggestion: "The build failed. Read the error output carefully — \
                    the compiler tells you exactly what line is wrong and often \
                    suggests the fix. Use `read_file` to view the offending file, \
                    then `edit_file` to fix the specific issue."
                    .to_string(),
                recoverable: true,
            });
        }

        if err_lower.contains("npm") || err_lower.contains("node") || err_lower.contains("tsc") {
            return Some(RecoveryHint {
                error_summary: "JavaScript/TypeScript build error".to_string(),
                suggestion: "The build failed. Check the error output for the specific \
                    file and line number. Use `read_file` to inspect the file and \
                    `edit_file` to fix the issue."
                    .to_string(),
                recoverable: true,
            });
        }
    }

    // Command not found (check before generic "not found" to avoid false match)
    if err_lower.contains("command not found") || err_lower.contains("not recognized") {
        return Some(RecoveryHint {
            error_summary: "Command not found".to_string(),
            suggestion: "The command you tried to run doesn't exist. Check:\n\
                     1. Is the tool installed? Try `which <command>`\n\
                     2. Do you need to install it first? Use the package manager\n\
                     3. Is there an alternative tool you can use?"
                .to_string(),
            recoverable: true,
        });
    }

    // Connection/network errors
    if err_lower.contains("connection")
        || err_lower.contains("timeout")
        || err_lower.contains("network")
    {
        return Some(RecoveryHint {
            error_summary: "Network error".to_string(),
            suggestion: "A network operation failed. Retry may succeed — \
                network issues are often transient. If this persists, check \
                connectivity with a simple command like `curl`."
                .to_string(),
            recoverable: true,
        });
    }

    None
}

/// Build a recovery message to append to the conversation.
///
/// This is sent as a user message so the model can read it and adjust.
pub fn build_recovery_message(hint: &RecoveryHint) -> Message {
    Message {
        role: Role::User,
        content: format!(
            "The previous action failed: {}\n\n{}\n\nPlease correct the issue and try again. \
             Do NOT repeat the same failing command — use the suggestions above.",
            hint.error_summary, hint.suggestion
        ),
        content_parts: None,
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    }
}

/// Track retry state within a turn.
#[derive(Debug, Clone, Default)]
pub struct RetryTracker {
    /// Number of error-recovery retries attempted this turn.
    pub retry_count: usize,
    /// Maximum retries allowed per turn.
    pub max_retries: usize,
}

impl RetryTracker {
    pub fn new() -> Self {
        Self {
            retry_count: 0,
            max_retries: 2,
        }
    }

    /// Returns true if we should still attempt recovery.
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries
    }

    /// Record a retry attempt.
    pub fn record_retry(&mut self) {
        self.retry_count += 1;
    }

    /// Reset for a new turn.
    pub fn reset(&mut self) {
        self.retry_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_file_not_found() {
        let args = serde_json::json!({"path": "src/lib.rs"});
        let hint =
            analyze_error("read_file", "No such file or directory: src/lib.rs", &args).unwrap();
        assert!(hint.suggestion.contains("glob"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_permission_denied() {
        let args = serde_json::json!({"path": "/etc/shadow"});
        let hint = analyze_error("read_file", "Permission denied", &args).unwrap();
        assert!(hint.suggestion.contains("permissions"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_build_error() {
        let args = serde_json::json!({"command": "cargo build"});
        let hint = analyze_error(
            "bash",
            "error: failed to compile `foo`\ncargo failed with exit code 101",
            &args,
        )
        .unwrap();
        assert!(hint.suggestion.contains("read_file"));
        assert!(hint.suggestion.contains("edit_file"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_command_not_found() {
        let args = serde_json::json!({"command": "nonexistent-tool"});
        let hint =
            analyze_error("bash", "bash: nonexistent-tool: command not found", &args).unwrap();
        assert!(hint.suggestion.contains("installed"));
        assert!(hint.recoverable);
    }

    #[test]
    fn test_analyze_unknown_error_returns_none() {
        let args = serde_json::json!({"path": "ok.txt"});
        let hint = analyze_error("read_file", "some unparseable error", &args);
        assert!(hint.is_none());
    }

    #[test]
    fn test_retry_tracker() {
        let mut tracker = RetryTracker::new();
        assert!(tracker.can_retry());
        tracker.record_retry();
        assert!(tracker.can_retry());
        tracker.record_retry();
        assert!(!tracker.can_retry(), "should max out at 2 retries");
        tracker.reset();
        assert!(tracker.can_retry(), "reset should clear counter");
    }
}
