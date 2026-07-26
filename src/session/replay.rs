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

// ── Interactive stepper ──
//
// Pure state holder for stepping through a loaded trace. No TUI dependency —
// the TUI app in `src/tui/replay.rs` drives this struct; unit tests exercise
// it directly. `render_current` produces FULL detail (no 200/300-char
// truncation like `format_turn`) so the interactive view can show the
// complete prompt messages, model response, tool args, and tool results.

/// Interactive replay stepper. Holds a loaded trace + a cursor; lets the
/// caller step forward/backward, jump to an index, and render the current
/// turn at full fidelity.
pub struct ReplayStepper {
    records: Vec<TurnRecord>,
    current_index: usize,
}

impl ReplayStepper {
    /// Build a stepper over the given records. Cursor starts at the first
    /// record (index 0) if any, else 0 on an empty trace.
    pub fn new(records: Vec<TurnRecord>) -> Self {
        Self {
            records,
            current_index: 0,
        }
    }

    /// Number of turns in the trace.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True when the trace has no turns.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Current cursor position (0-based). Clamped to `len() - 1` when
    /// non-empty, or 0 when empty.
    pub fn index(&self) -> usize {
        self.current_index
    }

    /// Borrow the current turn, or `None` on an empty trace.
    pub fn current(&self) -> Option<&TurnRecord> {
        self.records.get(self.current_index)
    }

    /// Advance one turn. Stops at the last turn (does not wrap).
    /// Returns `true` if the cursor actually moved.
    pub fn step_forward(&mut self) -> bool {
        if self.current_index + 1 < self.records.len() {
            self.current_index += 1;
            true
        } else {
            false
        }
    }

    /// Step back one turn. Stops at the first turn (does not wrap).
    /// Returns `true` if the cursor actually moved.
    pub fn step_back(&mut self) -> bool {
        if self.current_index == 0 {
            false
        } else {
            self.current_index -= 1;
            true
        }
    }

    /// Jump to a 0-based index. Out-of-range indices clamp to the nearest
    /// valid bound. Returns the resulting index. On an empty trace this is
    /// a no-op returning 0.
    pub fn jump_to(&mut self, n: usize) -> usize {
        if self.records.is_empty() {
            self.current_index = 0;
            return 0;
        }
        let max = self.records.len() - 1;
        self.current_index = n.min(max);
        self.current_index
    }

    /// Render the current turn at full fidelity — no 200/300-char truncation.
    /// Shows full prompt messages, full model response, full tool call args
    /// (pretty-printed JSON) and full tool results. Returns an empty string
    /// when the trace is empty.
    pub fn render_current(&self) -> String {
        let Some(record) = self.current() else {
            return String::new();
        };
        render_turn_full(record)
    }
}

/// Full-fidelity render of a turn (companion to `format_turn`, but without
/// truncation). Factored out so it can be reused outside the stepper if
/// needed.
pub fn render_turn_full(record: &TurnRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("── Turn {} ─{}─\n", record.turn, "─".repeat(60)));
    out.push_str(&format!("Timestamp: {}\n", record.timestamp));

    out.push_str("\n[Prompt messages]\n");
    if record.prompt_messages.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for (i, msg) in record.prompt_messages.iter().enumerate() {
            out.push_str(&format!("  #{} [{}]\n", i, msg.role));
            for line in msg.content.split('\n') {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    out.push_str("\n[Model response]\n");
    if record.model_response.is_empty() {
        out.push_str("  (empty)\n");
    } else {
        for line in record.model_response.split('\n') {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }

    out.push_str("\n[Tool calls]\n");
    if record.tool_calls.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for (i, tc) in record.tool_calls.iter().enumerate() {
            out.push_str(&format!(
                "  #{} → {} ({:.0}ms)\n",
                i, tc.tool, tc.duration_ms as f64
            ));
            let args =
                serde_json::to_string_pretty(&tc.args).unwrap_or_else(|_| tc.args.to_string());
            out.push_str("    args:\n");
            for line in args.split('\n') {
                out.push_str("      ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("    result:\n");
            for line in tc.result.split('\n') {
                out.push_str("      ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    let outcome_str = match &record.outcome {
        TurnOutcome::Success => "Success".to_string(),
        TurnOutcome::Error(e) => format!("Error: {e}"),
        TurnOutcome::Cancelled => "Cancelled".to_string(),
        TurnOutcome::Timeout => "Timeout".to_string(),
    };
    out.push_str("\n[Outcome]\n");
    out.push_str(&format!(
        "  {} | {} tokens in | {} tokens out | {:.1}s\n",
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

    // ── ReplayStepper tests ──

    fn sample_record(turn: u32, response: &str) -> TurnRecord {
        TurnRecord {
            turn,
            timestamp: format!("2026-07-22T00:00:0{turn}Z"),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: format!("prompt for turn {turn}"),
            }],
            model_response: response.to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 10 * turn as u64,
            tokens_out: 5 * turn as u64,
            duration_ms: 100 * turn as u64,
        }
    }

    #[test]
    fn stepper_empty_trace() {
        let mut stepper = ReplayStepper::new(vec![]);
        assert!(stepper.is_empty());
        assert_eq!(stepper.len(), 0);
        assert_eq!(stepper.index(), 0);
        assert!(stepper.current().is_none());
        assert_eq!(stepper.render_current(), "");
        // Stepping on an empty trace is a no-op.
        assert!(!stepper.step_forward());
        assert!(!stepper.step_back());
        assert_eq!(stepper.jump_to(5), 0);
    }

    #[test]
    fn stepper_single_record() {
        let mut stepper = ReplayStepper::new(vec![sample_record(1, "only")]);
        assert!(!stepper.is_empty());
        assert_eq!(stepper.len(), 1);
        assert_eq!(stepper.index(), 0);
        assert!(stepper.current().is_some());
        assert_eq!(stepper.current().unwrap().turn, 1);
        // No forward to go.
        assert!(!stepper.step_forward());
        assert!(!stepper.step_back());
        assert_eq!(stepper.jump_to(0), 0);
    }

    #[test]
    fn stepper_multi_record_walks_both_directions() {
        let records = vec![
            sample_record(1, "first"),
            sample_record(2, "second"),
            sample_record(3, "third"),
        ];
        let mut stepper = ReplayStepper::new(records);
        assert_eq!(stepper.index(), 0);
        assert_eq!(stepper.current().unwrap().turn, 1);

        assert!(stepper.step_forward());
        assert_eq!(stepper.index(), 1);
        assert_eq!(stepper.current().unwrap().turn, 2);

        assert!(stepper.step_forward());
        assert_eq!(stepper.index(), 2);
        assert_eq!(stepper.current().unwrap().turn, 3);

        // Boundary: clamps at the last record.
        assert!(!stepper.step_forward());
        assert_eq!(stepper.index(), 2);

        // Walk back.
        assert!(stepper.step_back());
        assert_eq!(stepper.index(), 1);
        assert!(stepper.step_back());
        assert_eq!(stepper.index(), 0);

        // Boundary: clamps at the first record.
        assert!(!stepper.step_back());
        assert_eq!(stepper.index(), 0);
    }

    #[test]
    fn stepper_jump_to_clamps_out_of_range() {
        let records = vec![sample_record(1, "a"), sample_record(2, "b")];
        let mut stepper = ReplayStepper::new(records);
        assert_eq!(stepper.jump_to(0), 0);
        assert_eq!(stepper.jump_to(1), 1);
        // Out-of-range clamps to the last valid index.
        assert_eq!(stepper.jump_to(99), 1);
        // And from above back down.
        assert_eq!(stepper.jump_to(0), 0);
    }

    #[test]
    fn stepper_render_current_shows_full_untruncated_content() {
        // Build a record whose prompt + response exceed the 200/300-char
        // truncation thresholds used by `format_turn`. The stepper's
        // `render_current` must emit the FULL content.
        let long_prompt = "X".repeat(500);
        let long_response = "Y".repeat(600);
        let record = TurnRecord {
            turn: 7,
            timestamp: "2026-07-22T12:00:00Z".to_string(),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: long_prompt.clone(),
            }],
            model_response: long_response.clone(),
            tool_calls: vec![RecordedToolCall {
                tool: "shell".to_string(),
                args: serde_json::json!({"cmd": "echo hello", "cwd": "/tmp"}),
                result: "hello\n".to_string(),
                duration_ms: 42,
            }],
            outcome: TurnOutcome::Error("boom".to_string()),
            tokens_in: 1234,
            tokens_out: 5678,
            duration_ms: 9000,
        };
        let stepper = ReplayStepper::new(vec![record]);
        let rendered = stepper.render_current();

        // Full prompt content present (no truncation ellipsis).
        assert!(rendered.contains(&long_prompt));
        // Full model response present.
        assert!(rendered.contains(&long_response));
        // Tool args pretty-printed (multi-line JSON, not single-line).
        assert!(rendered.contains("\"cmd\""));
        assert!(rendered.contains("echo hello"));
        // Tool result present in full.
        assert!(rendered.contains("hello"));
        // Outcome + token stats.
        assert!(rendered.contains("Error: boom"));
        assert!(rendered.contains("1234 tokens in"));
        assert!(rendered.contains("5678 tokens out"));
        // Contrast: `format_turn` would truncate the prompt to 200 chars.
        let truncated = format_turn(stepper.current().unwrap());
        assert!(truncated.contains("…"));
        assert!(!truncated.contains(&long_prompt));
    }
}
