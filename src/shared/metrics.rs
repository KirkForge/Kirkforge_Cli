//! Operational metrics — append-only NDJSON event log.
//!
//! Records lightweight, structured events for tool calls, verifier
//! verdicts, turn outcomes, and approval decisions. The log lives at
//! `~/.local/share/kirkforge/metrics.ndjson` and is designed to be
//! human-readable and trivial to query with standard shell tools.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};

/// Maximum size of the active metrics log before rotation. Older logs are
/// moved to `metrics.ndjson.1`; earlier rotations are overwritten.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// A metric event. Serialized as one NDJSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum MetricEvent {
    ToolCall {
        name: String,
        success: bool,
        duration_ms: u64,
        error_kind: Option<String>,
    },
    Verifier {
        name: String,
        verdict: String,
    },
    Turn {
        model: String,
        duration_ms: u64,
        tool_calls: usize,
        finish_reason: String,
    },
    Approval {
        action: String,
    },
}

impl MetricEvent {
    /// Human-readable category used by the summary command.
    pub fn category(&self) -> &'static str {
        match self {
            MetricEvent::ToolCall { .. } => "tool",
            MetricEvent::Verifier { .. } => "verifier",
            MetricEvent::Turn { .. } => "turn",
            MetricEvent::Approval { .. } => "approval",
        }
    }
}

/// Test-time override for the metrics path. Serialised by `TEST_LOCK` so
/// parallel tests cannot cross-contaminate each other's log files.
#[cfg(test)]
static PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the metrics log path inside the platform data directory.
pub fn metrics_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        let guard = PATH_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = guard.as_ref() {
            return Some(path.clone());
        }
    }
    let dirs = directories::ProjectDirs::from("", "KirkForge", "kirkforge")?;
    let data = dirs.data_local_dir();
    std::fs::create_dir_all(data).ok()?;
    Some(data.join("metrics.ndjson"))
}

/// Open (or create) the metrics log at the resolved path.
fn open_metrics_file(path: &PathBuf) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// Rotate the metrics log if it has grown past [`MAX_LOG_BYTES`].
fn rotate_if_needed(path: &PathBuf) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_LOG_BYTES {
        return;
    }
    let backup = path.with_extension("ndjson.1");
    if let Err(e) = std::fs::rename(path, &backup) {
        tracing::warn!(error = %e, "failed to rotate metrics log backup");
    }
}

/// Record a metric event.
///
/// Events are appended synchronously; failures are logged via `tracing`
/// but never propagated, so a metrics write error cannot break a turn.
pub fn record(event: MetricEvent) {
    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize metric event");
            return;
        }
    };

    let Some(path) = metrics_path() else {
        tracing::debug!("no metrics path available; dropping event");
        return;
    };

    rotate_if_needed(&path);

    match open_metrics_file(&path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                tracing::warn!(error = %e, "failed to write metric event");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to open metrics file");
        }
    }
}

/// Summary statistics computed from the metrics log.
#[derive(Debug, Default, Clone)]
pub struct MetricsSummary {
    pub tool_calls: usize,
    pub tool_success: usize,
    pub tool_failure: usize,
    pub verifier_clean: usize,
    pub verifier_fixable: usize,
    pub verifier_unfixable: usize,
    pub verifier_skipped: usize,
    pub turns: usize,
    pub total_turn_duration_ms: u64,
    pub approvals_allow: usize,
    pub approvals_ask: usize,
    pub approvals_deny: usize,
    pub approvals_always: usize,
}

impl MetricsSummary {
    pub fn avg_turn_duration_ms(&self) -> u64 {
        if self.turns == 0 {
            0
        } else {
            self.total_turn_duration_ms / self.turns as u64
        }
    }
}

/// Read all events from the metrics log.
pub fn read_events() -> Vec<MetricEvent> {
    let Some(path) = metrics_path() else {
        return Vec::new();
    };
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "failed to read metrics log");
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read metrics line");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<MetricEvent>(&line) {
            Ok(e) => events.push(e),
            Err(e) => {
                tracing::warn!(error = %e, line = %line, "failed to parse metrics line");
            }
        }
    }
    events
}

/// Summarize the metrics log.
pub fn summarize() -> MetricsSummary {
    let mut summary = MetricsSummary::default();
    for event in read_events() {
        match event {
            MetricEvent::ToolCall { success, .. } => {
                summary.tool_calls += 1;
                if success {
                    summary.tool_success += 1;
                } else {
                    summary.tool_failure += 1;
                }
            }
            MetricEvent::Verifier { verdict, .. } => match verdict.as_str() {
                "clean" => summary.verifier_clean += 1,
                "fixable" => summary.verifier_fixable += 1,
                "unfixable" => summary.verifier_unfixable += 1,
                _ => summary.verifier_skipped += 1,
            },
            MetricEvent::Turn {
                duration_ms,
                tool_calls,
                ..
            } => {
                summary.turns += 1;
                summary.total_turn_duration_ms += duration_ms;
                summary.tool_calls += tool_calls;
            }
            MetricEvent::Approval { action, .. } => match action.as_str() {
                "approved" => summary.approvals_allow += 1,
                "denied" => summary.approvals_deny += 1,
                "always_approved" => summary.approvals_always += 1,
                _ => {}
            },
        }
    }
    summary
}

/// Format a summary as user-facing text.
pub fn format_summary(summary: &MetricsSummary) -> String {
    let mut lines = Vec::new();
    lines.push("Metrics summary".to_string());
    lines.push(format!("  turns:          {}", summary.turns));
    lines.push(format!(
        "  avg turn time:  {} ms",
        summary.avg_turn_duration_ms()
    ));
    lines.push(format!(
        "  tool calls:     {} ({} ok, {} failed)",
        summary.tool_calls, summary.tool_success, summary.tool_failure
    ));
    lines.push(format!(
        "  verifiers:      {} clean / {} fixable / {} unfixable / {} skipped",
        summary.verifier_clean,
        summary.verifier_fixable,
        summary.verifier_unfixable,
        summary.verifier_skipped
    ));
    lines.push(format!(
        "  approvals:      {} approved / {} denied / {} always-approved",
        summary.approvals_allow, summary.approvals_deny, summary.approvals_always
    ));
    lines.join("\n")
}

#[cfg(test)]
pub(crate) fn with_test_path<F, R>(f: F) -> R
where
    F: FnOnce(PathBuf, MutexGuard<'static, ()>) -> R,
{
    let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("kirkforge_metrics_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metrics.ndjson");
    {
        let mut guard = PATH_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(path);
    }
    let result = f(dir.clone(), lock);
    {
        let mut guard = PATH_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
        *guard = None;
    }
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_read_round_trip() {
        with_test_path(|_dir, _lock| {
            record(MetricEvent::ToolCall {
                name: "read_file".into(),
                success: true,
                duration_ms: 12,
                error_kind: None,
            });
            record(MetricEvent::Approval {
                action: "approved".into(),
            });

            let events = read_events();
            assert_eq!(events.len(), 2);
            assert!(matches!(events[0], MetricEvent::ToolCall { .. }));
            assert!(matches!(events[1], MetricEvent::Approval { .. }));
        });
    }

    #[test]
    fn test_summarize_counts() {
        with_test_path(|_dir, _lock| {
            record(MetricEvent::ToolCall {
                name: "bash".into(),
                success: false,
                duration_ms: 100,
                error_kind: Some("execution".into()),
            });
            record(MetricEvent::Verifier {
                name: "lint".into(),
                verdict: "fixable".into(),
            });
            record(MetricEvent::Turn {
                model: "qwen2.5:3b".into(),
                duration_ms: 2500,
                tool_calls: 1,
                finish_reason: "stop".into(),
            });

            let summary = summarize();
            assert_eq!(summary.tool_failure, 1);
            assert_eq!(summary.verifier_fixable, 1);
            assert_eq!(summary.turns, 1);
            assert_eq!(summary.avg_turn_duration_ms(), 2500);
        });
    }

    #[test]
    fn test_rotation_replaces_old_log() {
        with_test_path(|_dir, _lock| {
            let path = metrics_path().unwrap();
            // Pre-seed an oversized log.
            let big = "a".repeat(MAX_LOG_BYTES as usize + 100);
            std::fs::write(&path, big).unwrap();

            record(MetricEvent::Approval {
                action: "denied".into(),
            });

            let events = read_events();
            assert_eq!(events.len(), 1);
            assert!(matches!(
                events[0],
                MetricEvent::Approval { ref action } if action == "denied"
            ));
        });
    }

    #[test]
    fn test_format_summary_output() {
        let summary = MetricsSummary {
            turns: 10,
            total_turn_duration_ms: 5000,
            tool_calls: 20,
            tool_success: 18,
            tool_failure: 2,
            verifier_clean: 5,
            verifier_fixable: 3,
            verifier_unfixable: 1,
            verifier_skipped: 1,
            approvals_allow: 4,
            approvals_ask: 1,
            approvals_deny: 1,
            approvals_always: 2,
        };
        let text = format_summary(&summary);
        assert!(text.contains("turns:          10"));
        assert!(text.contains("tool calls:     20"));
        assert!(text.contains("avg turn time:  500 ms"));
    }
}
