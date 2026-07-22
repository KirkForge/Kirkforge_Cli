//! Execution replay — structured turn traces for time-travel debugging.
//!
//! Persists a `TurnRecord` per turn as NDJSON alongside the conversation log.
//! `kirkforge replay <session-id>` steps through the trace to show exactly
//! what the model saw, what tools it called, and what the results were.
//!
//! ponytail: NDJSON turn traces parallel the conversation log. The upgrade
//! path is interactive TUI replay with diff highlighting.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

// ── Data types ──

/// A single recorded message (what was sent to or received from the model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedMessage {
    pub role: String,
    pub content: String,
}

/// A single recorded tool call within a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedToolCall {
    pub tool: String,
    pub args: serde_json::Value,
    pub result: String,
    pub duration_ms: u64,
}

/// Outcome of a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Success,
    Error(String),
    Cancelled,
    Timeout,
}

/// A single turn's complete trace record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub turn: u32,
    pub timestamp: String,
    pub prompt_messages: Vec<RecordedMessage>,
    pub model_response: String,
    pub tool_calls: Vec<RecordedToolCall>,
    pub outcome: TurnOutcome,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub duration_ms: u64,
}

// ── Trace recorder ──

/// Append-only trace recorder. Each `record` call appends one JSON line.
pub struct TraceRecorder {
    file: std::fs::File,
    turn: u32,
}

impl TraceRecorder {
    /// Open (or create) a trace file at the given path.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create trace directory {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        Ok(Self { file, turn: 0 })
    }

    /// Record a turn. Increments the internal turn counter.
    pub fn record(&mut self, mut record: TurnRecord) -> anyhow::Result<()> {
        self.turn += 1;
        record.turn = self.turn;
        let line = serde_json::to_string(&record)?;
        writeln!(self.file, "{line}").with_context(|| "write trace record")?;
        self.file.sync_all().with_context(|| "sync trace file")?;
        Ok(())
    }

    /// Load all records from a trace file.
    ///
    /// Corrupt lines are skipped so later valid lines are preserved.
    pub fn load(path: &Path) -> anyhow::Result<Vec<TurnRecord>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut records = Vec::new();
        for line in reader.lines() {
            let line = line.with_context(|| "read trace line")?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TurnRecord>(&line) {
                Ok(r) => records.push(r),
                Err(e) => {
                    tracing::warn!(error = %e, line = %line.trim(), "skipping corrupt trace line");
                }
            }
        }
        Ok(records)
    }

    pub fn turn(&self) -> u32 {
        self.turn
    }
}

// ── Replay formatting ──

/// Format a single turn for display.
pub fn format_turn(record: &TurnRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("── Turn {} ─{}─\n", record.turn, "─".repeat(60)));

    // Prompt messages
    for msg in &record.prompt_messages {
        let truncated: String = msg.content.chars().take(200).collect();
        let suffix = if msg.content.len() > 200 { "…" } else { "" };
        out.push_str(&format!("[{}] {}{}\n", msg.role, truncated, suffix));
    }

    // Model response
    let truncated: String = record.model_response.chars().take(300).collect();
    let suffix = if record.model_response.len() > 300 {
        "…"
    } else {
        ""
    };
    out.push_str(&format!("Model: {truncated}{suffix}\n"));

    // Tool calls
    for tc in &record.tool_calls {
        out.push_str(&format!(
            "  → {} ({:.0}ms)\n",
            tc.tool, tc.duration_ms as f64
        ));
    }

    // Outcome + stats
    let outcome_str = match &record.outcome {
        TurnOutcome::Success => "Success".to_string(),
        TurnOutcome::Error(e) => format!("Error: {e}"),
        TurnOutcome::Cancelled => "Cancelled".to_string(),
        TurnOutcome::Timeout => "Timeout".to_string(),
    };
    out.push_str(&format!(
        "Outcome: {} | {} tokens in | {} tokens out | {:.1}s\n",
        outcome_str,
        record.tokens_in,
        record.tokens_out,
        record.duration_ms as f64 / 1000.0
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_recorder_open_and_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.trace.ndjson");
        let mut recorder = TraceRecorder::open(&path).unwrap();

        let r1 = TurnRecord {
            turn: 0,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            model_response: "hi there".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 10,
            tokens_out: 5,
            duration_ms: 120,
        };
        recorder.record(r1).unwrap();

        let r2 = TurnRecord {
            turn: 0,
            timestamp: "2026-07-22T00:00:01Z".to_string(),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: "fix the bug".to_string(),
            }],
            model_response: "I'll fix it".to_string(),
            tool_calls: vec![RecordedToolCall {
                tool: "write_file".to_string(),
                args: serde_json::json!({"path": "src/lib.rs"}),
                result: "ok".to_string(),
                duration_ms: 50,
            }],
            outcome: TurnOutcome::Success,
            tokens_in: 100,
            tokens_out: 80,
            duration_ms: 200,
        };
        recorder.record(r2).unwrap();

        let loaded = TraceRecorder::load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].turn, 1);
        assert_eq!(loaded[1].turn, 2);
        assert_eq!(loaded[0].model_response, "hi there");
        assert_eq!(loaded[1].tool_calls.len(), 1);
        assert_eq!(loaded[1].tool_calls[0].tool, "write_file");
    }

    #[test]
    fn trace_recorder_load_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.trace.ndjson");
        let loaded = TraceRecorder::load(&path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn trace_recorder_load_skips_corrupt_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.trace.ndjson");

        let valid = serde_json::to_string(&TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: "ok".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 10,
            tokens_out: 5,
            duration_ms: 100,
        })
        .unwrap();

        std::fs::write(&path, format!("{valid}\nthis is not json\n")).unwrap();

        let loaded = TraceRecorder::load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].model_response, "ok");
    }

    #[test]
    fn replay_format_turn_contains_key_fields() {
        let record = TurnRecord {
            turn: 3,
            timestamp: "2026-07-22T12:00:00Z".to_string(),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: "add a test".to_string(),
            }],
            model_response: "I'll add a test".to_string(),
            tool_calls: vec![RecordedToolCall {
                tool: "write_file".to_string(),
                args: serde_json::json!({"path": "src/lib.rs"}),
                result: "ok".to_string(),
                duration_ms: 120,
            }],
            outcome: TurnOutcome::Success,
            tokens_in: 450,
            tokens_out: 180,
            duration_ms: 2300,
        };

        let formatted = format_turn(&record);
        assert!(formatted.contains("Turn 3"));
        assert!(formatted.contains("user"));
        assert!(formatted.contains("write_file"));
        assert!(formatted.contains("Success"));
        assert!(formatted.contains("450 tokens in"));
        assert!(formatted.contains("180 tokens out"));
    }
}
