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
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEntry {
    /// A destructive tool call (write_file, edit_file, bash).
    Tool {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// Tool name (e.g. `write_file`, `bash`).
        tool: String,
        /// Redacted tool arguments.
        args: serde_json::Value,
        /// Whether the tool completed successfully.
        success: bool,
        /// Reason the call was denied, if applicable.
        #[serde(skip_serializing_if = "Option::is_none")]
        denial_reason: Option<String>,
        /// Optional session identifier.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// A hook verdict — denial or fail-open failure (WO 11.6, ADR-061).
    Hook {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// The event name (e.g. `pre-tool-bash`, `post-turn`).
        event: String,
        /// The plugin name if it's a plugin hook, else `None` for built-in.
        #[serde(skip_serializing_if = "Option::is_none")]
        plugin: Option<String>,
        /// The verdict: `allow`, `deny`, or `allow_fail_open`.
        verdict: String,
        /// For `deny`: the reason. For `allow_fail_open`: the error.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Optional session identifier.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// A plugin tool invocation (H4).
    PluginTool {
        /// RFC 3339 UTC timestamp.
        timestamp: String,
        /// Plugin tool name.
        name: String,
        /// First 200 chars of the tool arguments.
        args_summary: String,
        /// Exit code of the subprocess, if available.
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Wall-clock duration in milliseconds.
        duration_ms: u64,
    },
}

impl AuditEntry {
    /// Construct a `Tool` variant with a redacted args snapshot.
    pub fn tool(
        tool: &str,
        args: &serde_json::Value,
        success: bool,
        denial_reason: Option<&str>,
    ) -> Self {
        Self::Tool {
            timestamp: chrono::Utc::now().to_rfc3339(),
            tool: tool.to_string(),
            args: redact_args(tool, args),
            success,
            denial_reason: denial_reason.map(|s| s.to_string()),
            session_id: None,
        }
    }

    /// Construct a `Hook` variant recording a verdict (WO 11.6).
    pub fn hook(event: &str, plugin: Option<&str>, verdict: &str, reason: Option<&str>) -> Self {
        Self::Hook {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event: event.to_string(),
            plugin: plugin.map(|s| s.to_string()),
            verdict: verdict.to_string(),
            reason: reason.map(|s| s.to_string()),
            session_id: None,
        }
    }

    /// Construct a `PluginTool` variant (H4).
    pub fn plugin_tool(
        name: &str,
        args_summary: &str,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) -> Self {
        Self::PluginTool {
            timestamp: chrono::Utc::now().to_rfc3339(),
            name: name.to_string(),
            args_summary: args_summary.to_string(),
            exit_code,
            duration_ms,
        }
    }
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
        let entry = AuditEntry::tool(tool, args, success, denial_reason);
        self.write_entry(&entry);
    }

    /// Record a hook verdict (denial or fail-open failure). WO 11.6.
    ///
    /// Best-effort: I/O failures are logged but never surfaced.
    pub fn log_hook(&self, event: &str, plugin: Option<&str>, verdict: &str, reason: Option<&str>) {
        let entry = AuditEntry::hook(event, plugin, verdict, reason);
        self.write_entry(&entry);
    }

    /// Record a plugin tool invocation (H4). Best-effort.
    pub fn log_plugin_tool(
        &self,
        name: &str,
        args_summary: &str,
        exit_code: Option<i32>,
        duration_ms: u64,
    ) {
        let entry = AuditEntry::plugin_tool(name, args_summary, exit_code, duration_ms);
        self.write_entry(&entry);
    }

    /// Serialize and append a single entry line.
    fn write_entry(&self, entry: &AuditEntry) {
        let line = match serde_json::to_string(entry) {
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
            "kf_code_audit_lines_test_{}_{}",
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
            let AuditEntry::Tool { tool, args, .. } = entry else {
                panic!("expected Tool variant, got {entry:?}");
            };
            assert_eq!(tool, "write_file");
            assert!(args.get("content").is_none(), "content must be redacted");
            assert_eq!(
                args.get("path").and_then(|v| v.as_str()),
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
            "kf_code_audit_bash_test_{}_{}",
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
        let AuditEntry::Tool { args, .. } = entry else {
            panic!("expected Tool variant, got {entry:?}");
        };
        let logged_cmd = args.get("command").and_then(|v| v.as_str()).unwrap();
        assert!(logged_cmd.len() <= 1100, "bash command should be truncated");
        assert!(logged_cmd.ends_with("...[truncated]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_audit_log_records_hook_verdict() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kf_code_audit_hook_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("audit.ndjson");

        let log = AuditLog::new(Some(path.clone()));
        log.log_hook("pre-tool-bash", Some("sec-plugin"), "deny", Some("blocked"));
        log.log_hook(
            "pre-tool-bash",
            None,
            "allow_fail_open",
            Some("hook timed out"),
        );
        drop(log);

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 2, "expected two hook entries, got: {contents}");
        let e0: AuditEntry = serde_json::from_str(lines[0]).unwrap();
        match e0 {
            AuditEntry::Hook {
                event,
                plugin,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(plugin.as_deref(), Some("sec-plugin"));
                assert_eq!(verdict, "deny");
                assert_eq!(reason.as_deref(), Some("blocked"));
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }
        let e1: AuditEntry = serde_json::from_str(lines[1]).unwrap();
        match e1 {
            AuditEntry::Hook {
                event,
                plugin,
                verdict,
                reason,
                ..
            } => {
                assert_eq!(event, "pre-tool-bash");
                assert_eq!(plugin, None);
                assert_eq!(verdict, "allow_fail_open");
                assert_eq!(reason.as_deref(), Some("hook timed out"));
            }
            other => panic!("expected Hook variant, got {other:?}"),
        }

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
