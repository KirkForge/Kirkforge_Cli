//! Append-only JSONL audit log for destructive tool calls.
//!
//! Records one line per denied or successful destructive invocation
//! (`write_file`, `edit_file`, `bash`). Arguments are redacted before
//! serialization: literal file contents, `old_string`/`new_string`, and
//! arbitrary values from other tools are stripped or truncated.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A single audit-log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// RFC 3339 UTC timestamp.
    pub timestamp: String,
    /// Tool name (e.g. `write_file`, `bash`).
    pub tool: String,
    /// Redacted tool arguments.
    pub args: serde_json::Value,
    /// Whether the tool completed successfully.
    pub success: bool,
    /// Reason the call was denied, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    /// Optional session identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Append-only audit log.
///
/// When constructed with `path = None`, logging is a no-op. This is the
/// safe fallback when the data directory cannot be determined.
pub struct AuditLog {
    path: Option<PathBuf>,
    writer: Mutex<Option<BufWriter<std::fs::File>>>,
}

impl AuditLog {
    /// Open (or create) the audit log at `path`.
    ///
    /// If `path` is `None`, every log call is silently dropped.
    pub fn new(path: Option<PathBuf>) -> Self {
        let writer = path.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(
                        error = %e,
                        path = %p.display(),
                        "failed to create audit log directory; disabling audit log"
                    );
                    return None;
                }
            }
            match OpenOptions::new().append(true).create(true).open(p) {
                Ok(f) => Some(BufWriter::new(f)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %p.display(),
                        "failed to open audit log; disabling audit log"
                    );
                    None
                }
            }
        });
        Self {
            path,
            writer: Mutex::new(writer),
        }
    }

    /// Return the configured log path, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Record a destructive tool call.
    ///
    /// `args` is redacted in-place according to [`redact_args`] before being
    /// serialized. The call is best-effort: I/O failures are logged but never
    /// surfaced to the model or user.
    pub fn log_destructive(
        &self,
        tool: &str,
        args: &serde_json::Value,
        success: bool,
        denial_reason: Option<&str>,
    ) {
        let entry = AuditEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            tool: tool.to_string(),
            args: redact_args(tool, args),
            success,
            denial_reason: denial_reason.map(|s| s.to_string()),
            session_id: None,
        };
        let line = match serde_json::to_string(&entry) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize audit entry");
                return;
            }
        };
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(ref mut w) = *guard {
                if let Err(e) = writeln!(w, "{line}") {
                    tracing::warn!(error = %e, "failed to write audit entry");
                }
            }
        }
    }
}

impl Drop for AuditLog {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.writer.lock() {
            if let Some(ref mut w) = *guard {
                if let Err(e) = w.flush() {
                    tracing::warn!(error = %e, "failed to flush audit log on drop");
                }
            }
        }
    }
}

/// Redact sensitive values from tool arguments before they reach the log.
///
/// Policy:
/// * `content`, `old_string`, `new_string` are dropped entirely.
/// * For `bash`, `command` is kept but truncated to 1 KiB.
/// * For file tools, `path` is kept.
/// * For all other keys the value is replaced with `""` so the shape of the
///   call is still visible without leaking secrets.
fn redact_args(tool: &str, args: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = args.as_object() else {
        return serde_json::Value::Null;
    };
    let mut out = serde_json::Map::with_capacity(obj.len());
    for (key, value) in obj {
        match key.as_str() {
            "content" | "old_string" | "new_string" => continue,
            "command" if tool == "bash" => {
                let cmd = value.as_str().unwrap_or("");
                out.insert(
                    key.clone(),
                    serde_json::Value::String(truncate_string(cmd, 1024)),
                );
            }
            "path" => {
                if let Some(s) = value.as_str() {
                    out.insert(key.clone(), serde_json::Value::String(s.to_string()));
                }
            }
            _ => {
                out.insert(key.clone(), serde_json::Value::String(String::new()));
            }
        }
    }
    serde_json::Value::Object(out)
}

fn truncate_string(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Slice only at a character boundary; a naive byte slice can split a
    // multi-byte UTF-8 sequence and panic.
    let idx = s
        .char_indices()
        .take_while(|(i, _)| *i <= max)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    format!("{}...[truncated]", &s[..idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_log_appends_json_lines() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kirkforge_audit_lines_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        let args = serde_json::json!({"path": "/tmp/out.txt", "content": "SECRET"});
        log.log_destructive("write_file", &args, true, None);
        log.log_destructive("write_file", &args, false, Some("outside sandbox"));
        // Ensure buffered writes land on disk before reading.
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "expected two JSON lines, got: {contents}");
        for line in &lines {
            let entry: AuditEntry = serde_json::from_str(line).unwrap();
            assert_eq!(entry.tool, "write_file");
            assert!(
                entry.args.get("content").is_none(),
                "content must be redacted"
            );
            assert_eq!(
                entry.args.get("path").and_then(|v| v.as_str()),
                Some("/tmp/out.txt")
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_audit_log_redacts_bash_command() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kirkforge_audit_bash_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        let long_cmd = "echo ".to_string() + &"x".repeat(2048);
        let args = serde_json::json!({"command": long_cmd});
        log.log_destructive("bash", &args, true, None);
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let entry: AuditEntry = serde_json::from_str(contents.trim()).unwrap();
        let logged_cmd = entry.args.get("command").and_then(|v| v.as_str()).unwrap();
        assert!(logged_cmd.len() <= 1100, "bash command should be truncated");
        assert!(logged_cmd.ends_with("...[truncated]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_truncate_string_respects_utf8_boundaries() {
        // Use a 2-byte UTF-8 character so the 1024-byte boundary falls in the
        // middle of a character. The old byte-slice implementation would panic.
        let two_byte = "é";
        let long_cmd = two_byte.repeat(600);
        let truncated = truncate_string(&long_cmd, 1024);
        assert!(truncated.ends_with("...[truncated]"));
        assert!(
            truncated.len() <= 1024 + "...[truncated]".len(),
            "truncated command should not exceed max plus marker: {truncated}"
        );
        assert!(
            truncated.is_char_boundary(truncated.len() - "...[truncated]".len()),
            "truncate point must be on a character boundary"
        );
    }
}
