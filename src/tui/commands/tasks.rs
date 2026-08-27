//! `/tasks` slash-command handler — read-only listing of persisted
//! subagent summaries (WO 41.5 Phase 1).
//!
//! The `task` tool persists a [`PersistedTask`] JSON to
//! `<data_dir>/tasks/<id>.json` on terminal state. This command reads
//! that directory and renders a history row per file — id, status,
//! persona, duration, and a truncated summary. It is deliberately
//! read-only: Phase 1 does not rehydrate live handles or wire the
//! `/jobs` integration (that's Phase 2).

use crate::tools::task::load_persisted_tasks;

/// Truncate a summary to a single display line, capping at
/// `SUMMARY_TRUNC` chars and appending an ellipsis when truncated.
const SUMMARY_TRUNC: usize = 80;

fn truncate_summary(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= SUMMARY_TRUNC {
        s.replace('\n', " ")
    } else {
        let head: String = chars[..SUMMARY_TRUNC].iter().collect();
        format!("{}…", head.replace('\n', " "))
    }
}

/// Handle `/tasks` — list persisted subagent summaries.
pub async fn handle_tasks_command(_args: &str) -> String {
    let tasks = load_persisted_tasks();
    if tasks.is_empty() {
        return "No persisted subagent tasks. Completed background tasks are saved to ~/.local/share/kf-code/tasks/.".into();
    }
    let mut out = format!("Persisted subagent tasks ({}):\n", tasks.len());
    for t in &tasks {
        let summary = truncate_summary(t.summary.as_deref().unwrap_or("—"));
        let dur = t
            .duration_ms
            .map(format_duration_ms)
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "  {} [{}] persona={} {} — {}\n",
            t.id, t.status, t.persona, dur, summary,
        ));
    }
    out
}

/// Compact `<n>ms` / `<n>.<dd>s` / `<n>m<dd>s` formatter — mirrors the
/// private `format_duration_ms` in `tools/task.rs` (kept private there
/// for the in-memory view; duplicated here to avoid widening the API
/// for a read-only display).
fn format_duration_ms(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", ms as f64 / 1_000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::DataDirGuard;
    use crate::tools::task::{PersistedTask, TaskMetadata};

    fn persisted(
        id: &str,
        status: &str,
        summary: Option<&str>,
        persona: &str,
        duration_ms: u64,
    ) -> PersistedTask {
        PersistedTask {
            id: id.to_string(),
            status: status.to_string(),
            summary: summary.map(|s| s.to_string()),
            model: Some("qwen".to_string()),
            persona: persona.to_string(),
            prompt_summary: "scan the repo".to_string(),
            started_at: chrono::Local::now().to_rfc3339(),
            duration_ms: Some(duration_ms),
            parent_task_id: None,
            parent_run_id: None,
        }
    }

    fn write_task_to_disk(pt: &PersistedTask) {
        let dir = crate::session::tasks_dir().unwrap();
        let path = dir.join(format!("{}.json", pt.id));
        std::fs::write(path, serde_json::to_string_pretty(pt).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn tasks_command_empty_when_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        let out = handle_tasks_command("").await;
        assert!(out.contains("No persisted"), "got: {out}");
    }

    #[tokio::test]
    async fn tasks_command_lists_persisted_completed_task() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        write_task_to_disk(&persisted(
            "task-42",
            "completed",
            Some("did the work"),
            "explore",
            1_500,
        ));

        let out = handle_tasks_command("").await;
        assert!(out.contains("task-42"), "got: {out}");
        assert!(out.contains("completed"), "got: {out}");
        assert!(out.contains("explore"), "got: {out}");
        assert!(out.contains("1.50s"), "got: {out}");
        assert!(out.contains("did the work"), "got: {out}");
    }

    #[tokio::test]
    async fn tasks_command_lists_cancelled_task_with_status() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        write_task_to_disk(&persisted(
            "task-7",
            "cancelled",
            Some("partial work"),
            "coder",
            500,
        ));

        let out = handle_tasks_command("").await;
        assert!(out.contains("task-7"), "got: {out}");
        assert!(out.contains("cancelled"), "got: {out}");
        assert!(out.contains("partial work"), "got: {out}");
    }

    #[tokio::test]
    async fn tasks_command_lists_failed_task() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        write_task_to_disk(&persisted("task-9", "failed", Some("boom"), "explore", 200));

        let out = handle_tasks_command("").await;
        assert!(out.contains("task-9"), "got: {out}");
        assert!(out.contains("failed"), "got: {out}");
        assert!(out.contains("boom"), "got: {out}");
    }

    #[test]
    fn truncate_summary_short_unchanged() {
        assert_eq!(truncate_summary("hello"), "hello");
    }

    #[test]
    fn truncate_summary_long_capped_with_ellipsis() {
        let s = "x".repeat(120);
        let t = truncate_summary(&s);
        assert!(t.ends_with('…'), "got: {t}");
        assert!(t.chars().count() <= SUMMARY_TRUNC + 1);
    }

    #[test]
    fn truncate_summary_collapses_newlines() {
        assert_eq!(truncate_summary("line1\nline2"), "line1 line2");
    }

    #[test]
    fn format_duration_ms_thresholds() {
        assert_eq!(format_duration_ms(0), "0ms");
        assert_eq!(format_duration_ms(999), "999ms");
        assert_eq!(format_duration_ms(1_000), "1.00s");
        assert_eq!(format_duration_ms(60_000), "1m00s");
    }

    #[test]
    fn load_persisted_tasks_sorted_by_numeric_id() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        write_task_to_disk(&persisted("task-10", "completed", Some("a"), "x", 1));
        write_task_to_disk(&persisted("task-2", "completed", Some("b"), "x", 1));
        write_task_to_disk(&persisted("task-1", "completed", Some("c"), "x", 1));

        let loaded = load_persisted_tasks();
        let ids: Vec<&str> = loaded.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["task-1", "task-2", "task-10"]);
    }

    #[test]
    fn load_persisted_tasks_skips_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        let tasks_dir = crate::session::tasks_dir().unwrap();
        std::fs::write(tasks_dir.join("garbage.json"), "{not json").unwrap();
        write_task_to_disk(&persisted("task-5", "completed", Some("ok"), "x", 1));

        let loaded = load_persisted_tasks();
        assert_eq!(loaded.len(), 1, "malformed file should be skipped");
        assert_eq!(loaded[0].id, "task-5");
    }

    #[test]
    fn persisted_task_serialize_deserialize_round_trip() {
        let pt = persisted("task-1", "completed", Some("done"), "explore", 100);
        let json = serde_json::to_string(&pt).unwrap();
        let back: PersistedTask = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, pt.id);
        assert_eq!(back.status, pt.status);
        assert_eq!(back.summary, pt.summary);
    }

    // ── session::tasks_dir smoke ──
    // WO 47.21: now uses the thread-local DataDirGuard (env-var mutation
    // raced parallel tests); the KF_CODE_DATA_DIR env path itself is
    // covered by session::data_dir_respects_env_override under the
    // shared test lock.
    #[test]
    fn tasks_dir_respects_data_dir_override() {
        let dir = tempfile::tempdir().unwrap();
        let _dd = DataDirGuard::set(dir.path().to_path_buf());
        let tasks = crate::session::tasks_dir().unwrap();
        assert!(tasks.ends_with("tasks"));
    }

    // ── TaskMetadata fields used by PersistedTask::from_handle ──
    // Compile-only check that the struct shape matches (catches a field
    // rename that would break serialization).
    #[test]
    fn task_metadata_has_expected_fields() {
        let m = TaskMetadata {
            model: None,
            persona: "x".into(),
            prompt_summary: "y".into(),
            started_at: chrono::Local::now(),
            duration_ms: None,
            token_estimate: None,
            parent_task_id: None,
            parent_run_id: None,
        };
        assert_eq!(m.persona, "x");
    }
}
