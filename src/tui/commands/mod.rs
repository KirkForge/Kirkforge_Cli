pub mod bang;
pub mod commit;
pub mod compact;
pub mod fork;
pub mod github;
pub mod init;
pub mod jobs;
pub mod memory;
pub mod mentions;
pub mod model;
pub mod persona;
pub mod reload;
pub mod save;
pub mod sessions;
pub mod status;
pub mod test;
pub mod test_parse;
pub mod undo;

pub use bang::*;
pub use commit::*;
pub use compact::*;
pub use fork::*;
pub use github::*;
pub use init::*;
pub use jobs::*;
pub use memory::*;
pub use mentions::*;
pub use model::*;
pub use persona::*;
pub use reload::*;
pub use save::*;
pub use sessions::*;
pub use status::*;
pub use test::*;
pub use undo::*;

/// Map persisted [`Message`]s into TUI [`ConversationEntry`]s.
///
/// Used when reloading a conversation from disk (`/resume`, `/fork`,
/// persona merge) so the chat panel mirrors the persisted history.
/// Tool results are restored with the full output in the sidecar and a
/// generated one-line summary, preserving the collapse/expand behavior
/// from a live session.
pub fn messages_to_entries(
    msgs: &[crate::shared::Message],
) -> Vec<crate::tui::app::ConversationEntry> {
    msgs.iter()
        .map(|m| {
            if m.role == crate::shared::Role::Tool {
                let name = m.tool_name.as_deref().unwrap_or("tool");
                let full = m.content.clone();
                let (lines, bytes) =
                    crate::tui::app::AppState::tool_output_metrics(&full, 80);
                let summary = format!(
                    "🔧 {} (done) — {} lines, {} bytes [Enter or Tab to expand]",
                    name, lines, bytes
                );
                crate::tui::app::ConversationEntry::tool(summary, full)
            } else {
                let role = match m.role {
                    crate::shared::Role::User => "user",
                    crate::shared::Role::Assistant => "assistant",
                    crate::shared::Role::System => "system",
                    crate::shared::Role::Tool => "tool",
                };
                crate::tui::app::ConversationEntry::new(role, m.content.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::bash_jobs::{BashJob, JobStatus};
    use crate::shared::{Message, Role};

    fn user_msg(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn assistant_msg(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: content.to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn tool_msg(content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: content.to_string(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            tool_name: Some("bash".into()),
            token_count: None,
        }
    }

    #[test]
    fn messages_to_entries_reexport_empty_input() {
        let entries = messages_to_entries(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn messages_to_entries_reexport_role_mapping() {
        let msgs = vec![user_msg("hi"), assistant_msg("hello")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");
    }

    #[test]
    fn messages_to_entries_reexport_tool_has_sidecar_and_summary() {
        let entries = messages_to_entries(&[tool_msg("output")]);
        assert_eq!(entries[0].role, "tool");
        assert_eq!(entries[0].tool_output.as_deref(), Some("output"));
        assert!(entries[0].content.contains("bash"));
        assert!(entries[0].content.contains("bytes"));
    }

    #[test]
    fn messages_to_entries_tool_without_name_uses_default() {
        let msg = Message {
            role: Role::Tool,
            content: "result".to_string(),
            tool_name: None,
            ..Default::default()
        };
        let entries = messages_to_entries(&[msg]);
        assert!(entries[0].content.contains("🔧 tool"));
        assert_eq!(entries[0].tool_output.as_deref(), Some("result"));
    }

    fn dummy_job(id: u64, status: JobStatus) -> BashJob {
        BashJob {
            id,
            command: "echo hi".to_string(),
            status,
            stdout: String::new(),
            stderr: String::new(),
            started_at: chrono::Local::now(),
            finished_at: None,
        }
    }

    #[test]
    fn format_job_status_running_uses_hash_form() {
        let j = dummy_job(5, JobStatus::Running);
        let s = format_job_status(&j);
        assert!(s.contains("#5"), "got: {}", s);
        assert!(!s.contains("(id="), "old format leaked: {}", s);
        assert!(s.contains("running"), "got: {}", s);
    }

    #[test]
    fn format_job_status_completed_includes_exit_code() {
        let j = dummy_job(7, JobStatus::Completed(0));
        let s = format_job_status(&j);
        assert!(s.contains("#7"));
        assert!(s.contains("completed"));
        assert!(s.contains("exit 0"));
    }

    #[test]
    fn format_job_status_failed_includes_error_text() {
        let j = dummy_job(9, JobStatus::Failed("killed".into()));
        let s = format_job_status(&j);
        assert!(s.contains("#9"));
        assert!(s.contains("failed"));
        assert!(s.contains("killed"));
    }

    #[test]
    fn format_job_status_cancelled_is_distinct_from_failed() {
        let cancelled = format_job_status(&dummy_job(11, JobStatus::Cancelled));
        let failed = format_job_status(&dummy_job(11, JobStatus::Failed("x".into())));
        assert!(cancelled.contains("cancelled"));
        assert!(!cancelled.contains("failed"));
        assert_ne!(cancelled, failed);
    }

    #[test]
    fn tail_lines_empty_input_returns_empty() {
        let (out, elided) = tail_lines("", 10);
        assert_eq!(out, "");
        assert_eq!(elided, 0);
    }

    #[test]
    fn tail_lines_short_input_unchanged() {
        let (out, elided) = tail_lines("a\nb\nc", 10);
        assert_eq!(out, "a\nb\nc");
        assert_eq!(elided, 0);
    }

    #[test]
    fn tail_lines_long_input_keeps_tail_and_reports_elided() {
        let input = (1..=100)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, elided) = tail_lines(&input, 5);
        assert_eq!(elided, 95);

        assert_eq!(out, "line96\nline97\nline98\nline99\nline100");

        assert!(!out.contains("line1\n"));
        assert!(!out.contains("line50"));
    }

    #[test]
    fn tail_lines_exact_boundary_no_elision() {
        let input = (1..=5)
            .map(|i| format!("L{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, elided) = tail_lines(&input, 5);
        assert_eq!(out, input);
        assert_eq!(elided, 0);
    }

    use std::sync::OnceLock;
    static TEST_REGISTRY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    fn test_registry_lock() -> &'static tokio::sync::Mutex<()> {
        TEST_REGISTRY_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    #[tokio::test]
    async fn handle_jobs_command_list_includes_spawned_job() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_list_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command("").await;
        assert!(out.starts_with("Background jobs:"), "got: {}", out);
        assert!(out.contains(&unique), "unique cmd missing: {}", out);
        assert!(out.contains(&format!("#{}", id)), "job id missing: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_detail_shows_stdout() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_detail_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command(&id.to_string()).await;
        assert!(out.contains(&format!("#{}", id)), "got: {}", out);
        assert!(out.contains("Command:"), "got: {}", out);
        assert!(out.contains("Started:"), "got: {}", out);
        assert!(out.contains("Finished:"), "got: {}", out);
        assert!(out.contains("stdout"), "got: {}", out);
        assert!(out.contains(&unique), "stdout missing unique: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_detail_unknown_id_says_not_found() {
        let out = handle_jobs_command("999999").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("/jobs foo"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_clean_is_idempotent() {
        let _guard = test_registry_lock().lock().await;
        let out = handle_jobs_command("clean").await;
        assert!(
            out.contains("Cleaned") || out.contains("No completed jobs"),
            "got: {}",
            out
        );
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_running_job_succeeds() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();

        let unique = format!("sleep 5  # kf_test_cancel_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let out = handle_jobs_command(&format!("{} cancel", id)).await;
        assert!(out.contains("Cancel"), "got: {}", out);
        assert!(out.contains(&format!("#{}", id)), "got: {}", out);

        let job = registry.get(id).await;
        if let Some(j) = job {
            assert!(
                matches!(j.status, JobStatus::Cancelled | JobStatus::Failed(_)),
                "expected cancelled or failed, got: {:?}",
                j.status
            );
        }
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_finished_job_returns_error() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_cancel_done_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();

        for _ in 0..40 {
            let job = registry.get(id).await.unwrap();
            if !matches!(job.status, JobStatus::Running) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let out = handle_jobs_command(&format!("{} cancel", id)).await;
        assert!(out.contains("not running"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_id_returns_not_found() {
        let out = handle_jobs_command("999999 cancel").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_without_id_returns_usage() {
        let out = handle_jobs_command("cancel").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("5 foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("foo"), "got: {}", out);
    }

    #[tokio::test]
    async fn handle_jobs_command_cancel_extra_token_returns_usage() {
        let out = handle_jobs_command("5 cancel now").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    fn sample_success(cmd: &str, stdout: &str, ms: u64) -> BangResult {
        BangResult {
            cmd: cmd.to_string(),
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
            timed_out: false,
            elapsed_ms: ms,
        }
    }

    #[test]
    fn format_bang_output_success_includes_command_and_banner() {
        let r = sample_success("echo hi", "hi\n", 42);
        let out = format_bang_output(&r);
        assert!(out.starts_with("$ echo hi"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("hi"), "got: {:?}", out);

        assert!(out.contains("42ms"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_failure_includes_exit_code_and_stderr() {
        let r = BangResult {
            cmd: "ls /nonexistent".to_string(),
            exit_code: 2,
            stdout: String::new(),
            stderr: "ls: /nonexistent: No such file or directory\n".to_string(),
            timed_out: false,
            elapsed_ms: 12,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("$ ls /nonexistent"), "got: {:?}", out);
        assert!(out.contains("❌ exit 2"), "got: {:?}", out);
        assert!(out.contains("⚠ stderr:"), "got: {:?}", out);
        assert!(out.contains("No such file or directory"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_timed_out_shows_clock_icon() {
        let r = BangResult {
            cmd: "sleep 99".to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            elapsed_ms: 30_000,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("⏰"), "got: {:?}", out);
        assert!(out.contains("30s"), "got: {:?}", out);
        assert!(!out.contains("✅"), "got: {:?}", out);
        assert!(!out.contains("❌"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_timed_out_includes_partial_output() {
        let r = BangResult {
            cmd: "slow".to_string(),
            exit_code: -1,
            stdout: "line1\nline2\n".to_string(),
            stderr: "warn!\n".to_string(),
            timed_out: true,
            elapsed_ms: 30_000,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("⏰"), "got: {:?}", out);
        assert!(out.contains("line1"), "got: {:?}", out);
        assert!(out.contains("line2"), "got: {:?}", out);
        assert!(out.contains("⚠ stderr:"), "got: {:?}", out);
        assert!(out.contains("warn!"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_timed_out_strips_run_shell_prefix() {
        let prefix = "[timed out after 30 seconds]\n";
        let r = BangResult {
            cmd: "slow".to_string(),
            exit_code: -1,
            stdout: format!("{}partial output", prefix),
            stderr: String::new(),
            timed_out: true,
            elapsed_ms: 30_000,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("⏰"), "got: {:?}", out);
        assert!(
            !out.contains("[timed out after 30 seconds]"),
            "duplicate prefix should be stripped: {:?}",
            out
        );
        assert!(out.contains("partial output"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_success_with_stderr_separates_them() {
        let r = BangResult {
            cmd: "cargo build 2>&1 >/dev/null".to_string(),
            exit_code: 0,
            stdout: String::new(),
            stderr: "Compiling foo v0.1.0\nFinished release in 1.2s\n".to_string(),
            timed_out: false,
            elapsed_ms: 1200,
        };
        let out = format_bang_output(&r);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("⚠ stderr:"), "got: {:?}", out);
        assert!(out.contains("Compiling foo"), "got: {:?}", out);
    }

    #[test]
    fn format_bang_output_empty_output_is_just_banner() {
        let r = sample_success("true", "", 5);
        let out = format_bang_output(&r);
        assert!(out.contains("$ true"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);

        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    #[test]
    fn format_elapsed_subsecond_is_ms() {
        assert_eq!(format_elapsed(0), "0ms");
        assert_eq!(format_elapsed(1), "1ms");
        assert_eq!(format_elapsed(999), "999ms");
    }

    #[test]
    fn format_elapsed_subminute_is_seconds_with_decimals() {
        assert_eq!(format_elapsed(1000), "1.00s");
        assert_eq!(format_elapsed(1420), "1.42s");
        assert_eq!(format_elapsed(12_345), "12.35s"); // banker-ish rounding
        assert_eq!(format_elapsed(59_999), "60.00s");
    }

    #[test]
    fn format_elapsed_minute_plus_uses_minute_second_format() {
        assert_eq!(format_elapsed(60_000), "1m00s");
        assert_eq!(format_elapsed(90_000), "1m30s");
        assert_eq!(format_elapsed(125_000), "2m05s");
    }

    #[test]
    fn bang_result_is_success_only_for_zero_exit_no_timeout() {
        assert!(sample_success("x", "", 0).is_success());
        assert!(!BangResult {
            exit_code: 1,
            timed_out: false,
            ..sample_success("x", "", 0)
        }
        .is_success());
        assert!(!BangResult {
            exit_code: 0,
            timed_out: true,
            ..sample_success("x", "", 0)
        }
        .is_success());
    }

    fn default_config() -> crate::shared::Config {
        crate::shared::Config::default()
    }

    #[tokio::test]
    async fn handle_bang_command_echo_runs() {
        let cfg = default_config();
        let out = handle_bang_command("echo hi", &cfg).await;
        assert!(out.contains("$ echo hi"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("hi"), "got: {:?}", out);
    }

    #[tokio::test]
    async fn handle_bang_command_true_exits_zero_silently() {
        let cfg = default_config();
        let out = handle_bang_command("true", &cfg).await;
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    #[tokio::test]
    async fn handle_bang_command_false_exits_nonzero() {
        let cfg = default_config();
        let out = handle_bang_command("false", &cfg).await;
        assert!(out.contains("❌ exit 1"), "got: {:?}", out);
    }

    #[tokio::test]
    async fn handle_bang_command_empty_returns_usage() {
        let cfg = default_config();
        let out = handle_bang_command("", &cfg).await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    #[tokio::test]
    async fn handle_bang_command_whitespace_only_returns_usage() {
        let cfg = default_config();
        let out = handle_bang_command("   ", &cfg).await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    #[tokio::test]
    async fn handle_bang_command_blocks_dangerous_pattern() {
        let cfg = default_config();
        let out = handle_bang_command("rm -rf /", &cfg).await;
        assert!(
            out.contains("🔒") && out.contains("dangerous"),
            "dangerous bang command should be blocked, got: {:?}",
            out
        );
    }

    #[test]
    fn parse_mentions_empty_for_prose() {
        let m = parse_mentions("hello world, no mentions here");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_mentions_single_token() {
        let m = parse_mentions("please review @src/main.rs carefully");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "src/main.rs");
        assert_eq!(m[0].spec.range, None);
        assert!(!m[0].spec.raw);

        assert_eq!(m[0].start, 14);

        assert_eq!(m[0].end, 14 + "@src/main.rs".len());
    }

    #[test]
    fn parse_mentions_multiple_tokens() {
        let m = parse_mentions("look at @a.rs and @b/c.py:10-20:raw please");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].spec.path, "a.rs");
        assert_eq!(m[1].spec.path, "b/c.py");
        assert_eq!(m[1].spec.range, Some((10, 20)));
        assert!(m[1].spec.raw);
    }

    #[test]
    fn parse_mentions_double_at_is_literal() {
        let m = parse_mentions("send email to @@user");
        assert!(m.is_empty(), "got: {:?}", m);
    }

    #[test]
    fn parse_mentions_trailing_at_ignored() {
        let m = parse_mentions("what about @");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_mentions_at_before_space_ignored() {
        let m = parse_mentions("try @ then run");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_mentions_raw_suffix() {
        let m = parse_mentions("inline @notes.md:raw now");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "notes.md");
        assert!(m[0].spec.raw);
    }

    #[test]
    fn parse_mentions_range() {
        let m = parse_mentions("see @foo.rs:1-10");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.range, Some((1, 10)));
    }

    #[test]
    fn parse_mentions_invalid_range_rejected() {
        let m = parse_mentions("see @foo.rs:10-5");
        assert!(
            m.is_empty(),
            "inverted range should be rejected, got: {:?}",
            m
        );
    }

    #[test]
    fn parse_mentions_non_numeric_range_rejected() {
        let m = parse_mentions("see @foo.rs:abc-def");
        assert!(m.is_empty());
    }

    #[test]
    fn strip_mentions_removes_tokens() {
        let input = "look at @a.rs and @b.py now";
        let mentions = parse_mentions(input);
        let cleaned = strip_mentions(input, &mentions);
        assert_eq!(cleaned, "look at  and  now", "got: {:?}", cleaned);
    }

    #[test]
    fn strip_mentions_no_mentions_unchanged() {
        let input = "nothing to strip here";
        let cleaned = strip_mentions(input, &parse_mentions(input));
        assert_eq!(cleaned, input);
    }

    #[test]
    fn expand_mentions_reads_real_file() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "hello world\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: None,
            raw: true,
        };
        let token = MentionToken {
            spec,
            start: 0,
            end: 0,
        };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert_eq!(expansions.len(), 1);
        assert!(matches!(expansions[0].status, MentionStatus::Ok { .. }));
        assert_eq!(expansions[0].content, "hello world");
    }

    #[test]
    fn expand_mentions_not_found() {
        let spec = MentionSpec {
            path: "/nonexistent/path/that/definitely/does/not/exist.rs".into(),
            range: None,
            raw: true,
        };
        let token = MentionToken {
            spec,
            start: 0,
            end: 0,
        };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert!(matches!(expansions[0].status, MentionStatus::NotFound));
    }

    #[test]
    fn expand_mentions_range_filters_lines() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "line1\nline2\nline3\nline4\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: Some((2, 3)),
            raw: true,
        };
        let token = MentionToken {
            spec,
            start: 0,
            end: 0,
        };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert_eq!(expansions[0].content, "line2\nline3");
    }

    #[test]
    fn expand_mentions_invalid_range_zero_start() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        std::fs::write(tmp.path(), "line1\nline2\n").expect("write");
        let spec = MentionSpec {
            path: tmp.path().to_string_lossy().to_string(),
            range: Some((0, 1)),
            raw: true,
        };
        let token = MentionToken {
            spec,
            start: 0,
            end: 0,
        };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);
        assert!(matches!(
            expansions[0].status,
            MentionStatus::InvalidRange(_)
        ));
    }

    #[test]
    fn render_mentions_block_empty_for_no_expansions() {
        assert_eq!(render_mentions_block(&[]), "");
    }

    #[test]
    fn render_mentions_block_produces_fenced_code() {
        let e = MentionExpansion {
            spec: MentionSpec {
                path: "src/main.rs".into(),
                range: None,
                raw: true,
            },
            content: "fn main() {}".into(),
            status: MentionStatus::Ok {
                bytes: 12,
                minified: false,
                truncated: false,
            },
        };
        let s = render_mentions_block(&[e]);
        assert!(s.contains("```"));
        assert!(s.contains("fn main()"));
        assert!(s.contains("src/main.rs"));
    }

    #[test]
    fn render_mentions_block_handles_not_found() {
        let e = MentionExpansion {
            spec: MentionSpec {
                path: "x.rs".into(),
                range: None,
                raw: true,
            },
            content: String::new(),
            status: MentionStatus::NotFound,
        };
        let s = render_mentions_block(&[e]);
        assert!(s.contains("not found"), "got: {:?}", s);
    }

    #[test]
    fn format_mention_status_empty_for_no_expansions() {
        assert_eq!(format_mention_status(&[]), "");
    }

    #[test]
    fn format_mention_status_lists_with_icons() {
        let expansions = vec![
            MentionExpansion {
                spec: MentionSpec {
                    path: "a.rs".into(),
                    range: None,
                    raw: true,
                },
                content: "x".into(),
                status: MentionStatus::Ok {
                    bytes: 1,
                    minified: false,
                    truncated: false,
                },
            },
            MentionExpansion {
                spec: MentionSpec {
                    path: "missing.rs".into(),
                    range: None,
                    raw: true,
                },
                content: String::new(),
                status: MentionStatus::NotFound,
            },
        ];
        let s = format_mention_status(&expansions);
        assert!(s.contains("✓"));
        assert!(s.contains("✗"));
        assert!(s.contains("a.rs"));
        assert!(s.contains("missing.rs"));
    }

    #[test]
    fn expand_mentions_tilde_expansion() {
        let spec = MentionSpec {
            path: "~/nonexistent_xyzzy_kf_test.rs".into(),
            range: None,
            raw: true,
        };
        let token = MentionToken {
            spec,
            start: 0,
            end: 0,
        };
        let guard = crate::session::access::PathGuard::default();
        let expansions = expand_mentions(&[token], &guard);

        assert!(matches!(
            expansions[0].status,
            MentionStatus::NotFound | MentionStatus::IoError(_)
        ));
    }
}
