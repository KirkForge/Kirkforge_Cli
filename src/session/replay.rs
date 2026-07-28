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
///
/// `sync_all` (an `fsync`) is batched: it runs every `sync_interval` turns
/// rather than on every turn, so a long session does not block the
/// executor's turn loop with a per-turn fsync. The final partial batch is
/// flushed by `Drop` so a dropped recorder does not lose un-sync'd turns.
/// Set `sync_interval = 1` to restore the old per-turn fsync (e.g. for use
/// cases that need per-turn crash-safety).
pub struct TraceRecorder {
    file: std::fs::File,
    turn: u32,
    turns_since_sync: u32,
    sync_interval: u32,
    // Counts `sync_all` calls made by `record` (not by `Drop`); test-only
    // hook so the batching test can assert the call count without a mock.
    sync_count: u32,
}

impl TraceRecorder {
    /// Open (or create) a trace file at the given path with the default
    /// `sync_interval` of 10 turns.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::with_sync_interval(path, 10)
    }

    /// Open (or create) a trace file, fsync-ing every `sync_interval`
    /// turns. `sync_interval = 1` gives the old per-turn fsync; `0` is
    /// clamped to 1 to avoid never syncing.
    pub fn with_sync_interval(path: &Path, sync_interval: u32) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create trace directory {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        Ok(Self {
            file,
            turn: 0,
            turns_since_sync: 0,
            sync_interval: sync_interval.max(1),
            sync_count: 0,
        })
    }

    /// Record a turn. Increments the internal turn counter.
    pub fn record(&mut self, mut record: TurnRecord) -> anyhow::Result<()> {
        self.turn += 1;
        record.turn = self.turn;
        let line = serde_json::to_string(&record)?;
        writeln!(self.file, "{line}").with_context(|| "write trace record")?;
        self.turns_since_sync += 1;
        if self.turns_since_sync >= self.sync_interval {
            self.file.sync_all().with_context(|| "sync trace file")?;
            self.turns_since_sync = 0;
            self.sync_count += 1;
        }
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

    /// Number of `sync_all` (fsync) calls made by `record` so far. Does
    /// not count the final flush in `Drop`. Test-only.
    #[cfg(test)]
    pub(crate) fn sync_count(&self) -> u32 {
        self.sync_count
    }
}

impl Drop for TraceRecorder {
    fn drop(&mut self) {
        // Flush the final partial batch so a dropped recorder does not
        // lose the last `< sync_interval` turns. The trace is a debugging
        // aid (the conversation log is the source of truth), but it must
        // still be recoverable after a crash/drop.
        let _ = self.file.sync_all();
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

    /// WO 10.5: `sync_all` (fsync) is batched every `sync_interval` turns,
    /// not every turn. With `sync_interval = 10`, 25 turns must call
    /// `sync_all` exactly twice during `record` (at turn 10 and turn 20).
    /// The final partial batch (turns 21-25) is flushed by `Drop`, which
    /// we verify by reading back all 25 records after the recorder drops.
    #[test]
    fn trace_recorder_batches_sync_all_every_n_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("batched.trace.ndjson");
        let mut recorder = TraceRecorder::with_sync_interval(&path, 10).expect("open recorder");
        for i in 1..=25 {
            recorder
                .record(sample_record(i, &format!("turn {i}")))
                .expect("record turn");
        }
        // During the 25 records, sync_all must fire exactly twice (turn
        // 10 and turn 20). The partial batch (turns 21-25) is NOT synced
        // yet — it is flushed by Drop.
        assert_eq!(
            recorder.sync_count(),
            2,
            "sync_all must be called exactly 2 times during record (at turn 10 and 20), \
             not once per turn; got {}",
            recorder.sync_count()
        );
        assert_eq!(recorder.turn(), 25);
        // Drop the recorder: the final partial batch (turns 21-25) must
        // be flushed so all 25 records survive.
        drop(recorder);
        let loaded = TraceRecorder::load(&path).expect("load after drop");
        assert_eq!(loaded.len(), 25, "Drop must flush the final partial batch");
        assert_eq!(loaded[0].turn, 1);
        assert_eq!(loaded[24].turn, 25);
    }

    /// `sync_interval = 1` restores the old per-turn fsync behaviour.
    #[test]
    fn trace_recorder_sync_interval_one_syncs_every_turn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("every-turn.trace.ndjson");
        let mut recorder = TraceRecorder::with_sync_interval(&path, 1).expect("open recorder");
        for i in 1..=3 {
            recorder
                .record(sample_record(i, &format!("turn {i}")))
                .expect("record turn");
        }
        assert_eq!(
            recorder.sync_count(),
            3,
            "sync_interval=1 must fsync on every turn; got {}",
            recorder.sync_count()
        );
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

    #[test]
    fn format_turn_error_outcome() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: "oops".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Error("broken".to_string()),
            tokens_in: 10,
            tokens_out: 5,
            duration_ms: 100,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("Error: broken"), "got: {formatted}");
    }

    #[test]
    fn format_turn_cancelled_outcome() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Cancelled,
            tokens_in: 10,
            tokens_out: 5,
            duration_ms: 100,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("Cancelled"), "got: {formatted}");
    }

    #[test]
    fn format_turn_timeout_outcome() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Timeout,
            tokens_in: 10,
            tokens_out: 5,
            duration_ms: 100,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("Timeout"), "got: {formatted}");
    }

    #[test]
    fn format_turn_empty_prompt_messages() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: "response".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("Turn 1"));
        assert!(formatted.contains("Model: response"));
        assert!(formatted.contains("Success"));
    }

    #[test]
    fn format_turn_truncates_long_prompt_message() {
        let long = "Z".repeat(500);
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![RecordedMessage {
                role: "user".to_string(),
                content: long.clone(),
            }],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("…"), "long prompt should be truncated");
        assert!(!formatted.contains(&long));
    }

    #[test]
    fn format_turn_truncates_long_model_response() {
        let long = "Y".repeat(500);
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: long.clone(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let formatted = format_turn(&record);
        assert!(formatted.contains("…"), "long response should be truncated");
        assert!(!formatted.contains(&long));
    }

    #[test]
    fn render_turn_full_empty_prompt_messages() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: "resp".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(
            rendered.contains("(none)"),
            "empty prompt should show (none): {rendered}"
        );
    }

    #[test]
    fn render_turn_full_empty_model_response() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(
            rendered.contains("(empty)"),
            "empty response should show (empty): {rendered}"
        );
    }

    #[test]
    fn render_turn_full_empty_tool_calls() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: "resp".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(
            rendered.contains("(none)"),
            "empty tool calls should show (none): {rendered}"
        );
    }

    #[test]
    fn render_turn_full_includes_timestamp() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T12:34:56Z".to_string(),
            prompt_messages: vec![],
            model_response: "resp".to_string(),
            tool_calls: vec![],
            outcome: TurnOutcome::Success,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(
            rendered.contains("Timestamp: 2026-07-22T12:34:56Z"),
            "got: {rendered}"
        );
    }

    #[test]
    fn render_turn_full_cancelled_outcome() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Cancelled,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(rendered.contains("Cancelled"), "got: {rendered}");
    }

    #[test]
    fn render_turn_full_timeout_outcome() {
        let record = TurnRecord {
            turn: 1,
            timestamp: "2026-07-22T00:00:00Z".to_string(),
            prompt_messages: vec![],
            model_response: String::new(),
            tool_calls: vec![],
            outcome: TurnOutcome::Timeout,
            tokens_in: 0,
            tokens_out: 0,
            duration_ms: 0,
        };
        let rendered = render_turn_full(&record);
        assert!(rendered.contains("Timeout"), "got: {rendered}");
    }

    #[test]
    fn stepper_render_current_empty_trace_returns_empty_string() {
        let stepper = ReplayStepper::new(vec![]);
        assert_eq!(stepper.render_current(), "");
    }

    #[test]
    fn stepper_jump_to_on_empty_trace_returns_zero() {
        let mut stepper = ReplayStepper::new(vec![]);
        assert_eq!(stepper.jump_to(0), 0);
        assert_eq!(stepper.jump_to(100), 0);
    }

    #[test]
    fn stepper_step_forward_on_empty_trace_returns_false() {
        let mut stepper = ReplayStepper::new(vec![]);
        assert!(!stepper.step_forward());
    }

    #[test]
    fn stepper_len_returns_record_count() {
        let records = vec![
            sample_record(1, "a"),
            sample_record(2, "b"),
            sample_record(3, "c"),
        ];
        let stepper = ReplayStepper::new(records);
        assert_eq!(stepper.len(), 3);
        assert!(!stepper.is_empty());
    }

    #[test]
    fn trace_recorder_load_returns_err_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("is-a-dir.trace.ndjson");
        std::fs::create_dir(&path).unwrap();
        assert!(TraceRecorder::load(&path).is_err());
    }

    #[test]
    fn turn_outcome_success_matches_itself() {
        assert!(matches!(TurnOutcome::Success, TurnOutcome::Success));
    }

    #[test]
    fn turn_outcome_error_carries_message() {
        assert!(matches!(
            TurnOutcome::Error("x".into()),
            TurnOutcome::Error(_)
        ));
        let s = format!("{:?}", TurnOutcome::Error("x".into()));
        assert!(s.contains("x"));
    }

    #[test]
    fn turn_outcome_cancelled_and_timeout_are_distinct_variants() {
        assert!(matches!(TurnOutcome::Cancelled, TurnOutcome::Cancelled));
        assert!(matches!(TurnOutcome::Timeout, TurnOutcome::Timeout));
    }
}
