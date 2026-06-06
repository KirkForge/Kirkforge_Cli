//! Slash-command handlers and background-job notifier.
//!
//! The file is split into five submodules, one per concern:
//!
//! - [`fork`]    — `/fork` and `/resume` (session forking + resume)
//! - [`jobs`]    — `/jobs` listing/detail/cancel/clean + completion notifier
//! - [`compact`] — `/compact` (user-driven history compaction trigger)
//! - [`bang`]    — `!` passthrough (run a shell command, no model round trip)
//! - [`mentions`] — `@<path>` expansion and rendering
//!
//! The few items that don't fit any of those concerns stay in this
//! top-level module: the `messages_to_entries` NDJSON→chat-list
//! converter (used by resume + tests) and the `/status` handler
//! (which depends on the rendering helpers that are themselves
//! shared between the status bar widget and the on-demand report).
//!
//! All public items are re-exported at the top level so external
//! callers (`keys.rs`, `mod.rs`) can continue to use
//! `crate::tui::commands::handle_fork_command` etc. unchanged.
//! Unit tests live here in `mod.rs` (not in the submodules) so a
//! single `cargo test` invocation still exercises everything; the
//! `use super::*` reaches the re-exports automatically.

pub mod bang;
pub mod compact;
pub mod fork;
pub mod jobs;
pub mod mentions;

// Re-export every public item from the submodules so existing
// `crate::tui::commands::handle_fork_command`-style import paths
// keep working unchanged. This is the seam that makes the
// extraction invisible to callers.
pub use bang::*;
pub use compact::*;
pub use fork::*;
pub use jobs::*;
pub use mentions::*;

use crate::shared::{Message, Role};
use crate::tui::app::{AppState, ConversationEntry};

/// Convert a slice of persisted `Message`s into display-ready
/// `ConversationEntry`s for the TUI's chat panel.
///
/// This is the inverse of what `ConversationLog::append` does on
/// disk — it rebuilds the in-memory display from the NDJSON log so
/// the user can see the conversation history after a `/resume`.
///
/// `Message::role` is a `Role` enum; `ConversationEntry::role` is a
/// `String` (the chat widget switches on `"user"`, `"assistant"`,
/// `"system"`, `"tool"`). The mapping is mechanical:
///
/// - `Role::User`      → `"user"`
/// - `Role::System`    → `"system"`
/// - `Role::Assistant` → `"assistant"`
/// - `Role::Tool`      → `"tool"` (no sidecar — see note below)
///
/// For `Role::Tool` we do **not** reconstruct the `tool_output`
/// sidecar that the live event loop populates when a `ToolResult`
/// arrives. The on-disk log only has the full content; the
/// "summary vs full" distinction is a UI-time concept. So we use
/// `ConversationEntry::new("tool", content)` and the chat widget
/// will render the full content verbatim — this is the documented
/// behaviour for tool entries without a `tool_output` sidecar and
/// matches the legacy forward-compat path.
///
/// Pure function, no I/O, no async. Unit-tested at the bottom of
/// this file.
pub fn messages_to_entries(messages: &[Message]) -> Vec<ConversationEntry> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::System => "system",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            ConversationEntry::new(role, m.content.clone())
        })
        .collect()
}

/// Handle `/status` command: print a one-shot summary of session metrics
/// to the chat view.
///
/// This is a richer, on-demand view of the same data the status bar
/// shows in real time: model, cumulative cost, tokens sent/received,
/// and the per-turn context pressure as a fraction of the model's
/// max context window. Useful when the user wants to know "should I
/// `/compact` now?" without scanning the status bar.
///
/// Reads the same fields the status widget reads (`last_turn_prompt_tokens`,
/// `cumulative_cost`, `model_info`) — keeping the calculation in
/// `format_status_block` ensures the on-demand report and the
/// always-on status bar can never disagree about what to recommend.
pub async fn handle_status_command(args: &str, state: &mut AppState) -> String {
    let _ = args;
    format_status_block(state)
}

/// Build the `/status` report as a single string. Pulled out of the
/// async handler so it can be unit-tested synchronously.
///
/// Sections:
///   1. Model identity (name, max context, thinking/tool support
///      from `ModelInfo`)
///   2. Session metrics (elapsed, cumulative + turn cost, tokens in/out)
///   3. Context pressure (per-turn prompt tokens vs. max context,
///      in the same format the status bar shows it)
///   4. Recommendation: when the per-turn prompt is in the
///      "consider /compact" zone, surface that explicitly.
pub fn format_status_block(state: &AppState) -> String {
    use crate::tui::rendering::{budget_pct, format_budget_indicator, format_token_count};

    let mut out = String::new();
    out.push_str("📊 Session status\n\n");

    // 1. Model
    if let Some(m) = &state.model_info {
        out.push_str(&format!("Model:        {}\n", m.name));
        out.push_str(&format!(
            "Max context:  {} tokens\n",
            format_token_count(m.max_context_tokens)
        ));
        out.push_str(&format!(
            "Thinking:     {}\n",
            if m.supports_thinking { "yes" } else { "no" }
        ));
        out.push_str(&format!(
            "Tool calls:   {}\n",
            match m.tool_call_format {
                crate::shared::ToolCallStyle::Native => "native",
                crate::shared::ToolCallStyle::OpenAiCompat => "openai-compat",
                crate::shared::ToolCallStyle::None => "none",
            }
        ));
    } else {
        out.push_str("Model:        (not connected)\n");
    }

    // 2. Session metrics
    let elapsed = state.session_started.elapsed();
    let secs = elapsed.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let elapsed_str = if h > 0 {
        format!("{}h {}m {}s", h, m, s)
    } else if m > 0 {
        format!("{}m {}s", m, s)
    } else {
        format!("{}s", s)
    };
    out.push_str(&format!("\nSession:      {}\n", elapsed_str));
    out.push_str(&format!(
        "Cost:         ${:.4} cumulative, ${:.4} this turn\n",
        state.cumulative_cost, state.turn_cost
    ));
    out.push_str(&format!(
        "Tokens:       {} sent, {} received\n",
        format_token_count(state.tokens_sent),
        format_token_count(state.tokens_received)
    ));

    // 3. Context pressure
    let max_ctx = state
        .model_info
        .as_ref()
        .map(|m| m.max_context_tokens)
        .unwrap_or(0);
    if state.last_turn_prompt_tokens > 0 && max_ctx > 0 {
        let (text, _color) = format_budget_indicator(state.last_turn_prompt_tokens, max_ctx);
        out.push_str(&format!("\nContext:      {} (last turn)\n", text));
    } else {
        out.push_str("\nContext:      (no turn yet)\n");
    }

    // 4. Recommendation
    if let Some(pct) = budget_pct(state.last_turn_prompt_tokens, max_ctx) {
        if pct >= 80 {
            out.push_str("\n⚠️  Context is tight — consider /compact.\n");
        } else if pct >= 50 {
            out.push_str("\n💡 Context is filling up — /compact is a one-shot way to free room.\n");
        } else {
            out.push_str("\n✅ Context is comfortable.\n");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolCallStyle;
    use crate::shared::{Config, Message, ModelInfo, Role};

    fn user_msg(content: &str) -> Message {
        Message {
            role: Role::User,
            content: content.to_string(),
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
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    fn system_msg(content: &str) -> Message {
        Message {
            role: Role::System,
            content: content.to_string(),
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
            thinking: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            tool_name: Some("bash".into()),
            token_count: None,
        }
    }

    // ---------------------------------------------------------------
    // messages_to_entries: pure conversion, exhaustive role coverage
    // ---------------------------------------------------------------

    #[test]
    fn messages_to_entries_empty_input_returns_empty_vec() {
        let entries = messages_to_entries(&[]);
        assert!(entries.is_empty());
    }

    #[test]
    fn messages_to_entries_user_role_maps_to_user_string() {
        let msgs = vec![user_msg("hi")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content, "hi");
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_assistant_role_maps_to_assistant_string() {
        let msgs = vec![assistant_msg("hello back")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "assistant");
        assert_eq!(entries[0].content, "hello back");
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_system_role_maps_to_system_string() {
        let msgs = vec![system_msg("compaction done")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "system");
        assert_eq!(entries[0].content, "compaction done");
    }

    #[test]
    fn messages_to_entries_tool_role_uses_new_constructor_no_sidecar() {
        // The on-disk log only has the full content; there is no
        // separate "summary" field. So we use `::new("tool", content)`
        // (not `::tool(summary, full)`) and the chat widget will
        // render the full content verbatim. This is the documented
        // forward-compat path for entries without a tool_output
        // sidecar.
        let msgs = vec![tool_msg("command output here")];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries[0].role, "tool");
        assert_eq!(entries[0].content, "command output here");
        // Critical: no sidecar. The chat widget's
        // `tool_should_collapse` check is `entry.tool_output.is_some()`.
        assert!(entries[0].tool_output.is_none());
    }

    #[test]
    fn messages_to_entries_preserves_order() {
        let msgs = vec![
            user_msg("u1"),
            assistant_msg("a1"),
            tool_msg("t1"),
            user_msg("u2"),
        ];
        let entries = messages_to_entries(&msgs);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[2].role, "tool");
        assert_eq!(entries[3].role, "user");
        assert_eq!(entries[3].content, "u2");
    }

    #[test]
    fn messages_to_entries_does_not_clone_tool_calls_or_thinking() {
        // The conversion drops optional fields (thinking, tool_calls,
        // tool_call_id, tool_name, token_count) — those are model-API
        // details, not display details. The chat widget only needs
        // role + content. This test pins down that contract: if
        // somebody later decides to surface `thinking` in the UI,
        // they have to add it to ConversationEntry first, not rely
        // on this function preserving it.
        let mut m = tool_msg("output");
        m.thinking = Some("I should call the bash tool".to_string());
        m.tool_calls = Some(vec![crate::shared::ToolInvocation {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": "ls"}),
        }]);
        m.token_count = Some(42);

        let entries = messages_to_entries(&[m]);
        assert_eq!(entries[0].role, "tool");
        assert_eq!(entries[0].content, "output");
        assert!(entries[0].tool_output.is_none());
        // ConversationEntry doesn't have `thinking` / `tool_calls`
        // fields, so there's nothing to assert on them — they are
        // dropped by construction. The test documents the contract.
    }

    #[test]
    fn messages_to_entries_round_trip_through_conversation_log() {
        // Integration smoke test: serialize a few messages to NDJSON
        // (mimicking what ConversationLog does on disk), read them
        // back, and confirm the conversion is lossless on the
        // (role, content) tuple.
        let tmp = std::env::temp_dir().join("kf_test_messages_round_trip.ndjson");
        let _ = std::fs::remove_file(&tmp);

        let original = vec![
            user_msg("hello"),
            assistant_msg("hi there"),
            tool_msg("ls: file.txt"),
        ];

        // Write
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).unwrap();
            for m in &original {
                writeln!(f, "{}", serde_json::to_string(m).unwrap()).unwrap();
            }
        }

        // Read
        let content = std::fs::read_to_string(&tmp).unwrap();
        let parsed: Vec<Message> = content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        let entries = messages_to_entries(&parsed);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].content, "hello");
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[2].role, "tool");
        assert_eq!(entries[2].content, "ls: file.txt");

        let _ = std::fs::remove_file(&tmp);
    }

    // ── format_status_block tests ─────────────────────────────────
    //
    // The helper is pure (no I/O, no async) so we can drive it with
    // a hand-rolled AppState. The fields we set:
    //   - model_info: Option<ModelInfo>  — drives the "Model:" section
    //   - last_turn_prompt_tokens / tokens_sent / tokens_received
    //   - cumulative_cost / turn_cost
    //
    // We avoid touching `session_started` (Instant::now() is fine for
    // the production path; the elapsed string is hard to pin down in
    // a test without flakiness — we don't assert on it).

    fn make_state_with_model(name: &str, max_ctx: usize) -> AppState {
        let mut s = AppState::new(Config::default());
        s.model_info = Some(ModelInfo {
            name: name.to_string(),
            supports_thinking: true,
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: max_ctx,
            recommended_temperature: 0.7,
        });
        s
    }

    /// No model connected: the "Model:" line says so, the "Context:"
    /// line says "no turn yet", and no recommendation is printed
    /// (the helper bails on `max_ctx == 0`).
    #[test]
    fn format_status_block_no_model_shows_placeholder() {
        let s = AppState::new(Config::default());
        let out = format_status_block(&s);
        assert!(out.contains("Session status"));
        assert!(out.contains("(not connected)"));
        assert!(out.contains("Context:"));
        assert!(out.contains("no turn yet"));
        // No "consider /compact" line because we have no max_ctx
        assert!(!out.contains("consider /compact"));
        assert!(!out.contains("Context is comfortable"));
    }

    /// Comfortable context (< 50%): the recommendation is the
    /// ✅ comfortable line, the percentage is in the green band.
    #[test]
    fn format_status_block_comfortable_recommends_nothing() {
        let mut s = make_state_with_model("deepseek-v4-flash:cloud", 100_000);
        s.last_turn_prompt_tokens = 30_000; // 30%
        let out = format_status_block(&s);
        assert!(out.contains("deepseek-v4-flash:cloud"));
        assert!(out.contains("30.0K/100.0K (30%)"));
        assert!(out.contains("Context is comfortable"));
        assert!(!out.contains("consider /compact"));
    }

    /// Tight context (50–80%): the recommendation is the
    /// 💡 filling-up hint. The percentage is in the yellow band.
    #[test]
    fn format_status_block_tight_recommends_considering_compact() {
        let mut s = make_state_with_model("glm-5.1:cloud", 100_000);
        s.last_turn_prompt_tokens = 60_000; // 60%
        let out = format_status_block(&s);
        assert!(out.contains("60.0K/100.0K (60%)"));
        assert!(out.contains("filling up"));
        assert!(!out.contains("consider /compact")); // this is the > 80% line
    }

    /// Critical context (>= 80%): the recommendation is the
    /// ⚠️ tight / consider /compact line. The percentage is in
    /// the red band.
    #[test]
    fn format_status_block_critical_recommends_compact_now() {
        let mut s = make_state_with_model("kimi:cloud", 128_000);
        s.last_turn_prompt_tokens = 110_000; // 85.9%
        let out = format_status_block(&s);
        assert!(out.contains("110.0K/128.0K (85%)"));
        assert!(out.contains("Context is tight"));
        assert!(out.contains("consider /compact"));
    }

    /// Cost and token counters are formatted to the spec.
    #[test]
    fn format_status_block_shows_cost_and_tokens() {
        let mut s = make_state_with_model("test", 100_000);
        s.tokens_sent = 123_456;
        s.tokens_received = 78_901;
        s.cumulative_cost = 0.0042;
        s.turn_cost = 0.0011;
        s.last_turn_prompt_tokens = 0; // no context-pressure line
        let out = format_status_block(&s);
        assert!(out.contains("$0.0042 cumulative"));
        assert!(out.contains("$0.0011 this turn"));
        assert!(out.contains("123.5K sent")); // format_token_count formatting
        assert!(out.contains("78.9K received"));
        // No context line when last_turn_prompt_tokens == 0
        assert!(!out.contains("(last turn)"));
    }

    /// Tool-call style enum is rendered as a human-readable string
    /// (not the Rust Debug form like "Native" — already fine, but
    /// `OpenAiCompat` would render as `OpenAiCompat` in lowercase
    /// form. Test the OpenAI-compat path explicitly to pin it down).
    #[test]
    fn format_status_block_renders_openai_compat_tool_style() {
        let mut s = AppState::new(Config::default());
        s.model_info = Some(ModelInfo {
            name: "qwen2.5:0.5b".to_string(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 32_000,
            recommended_temperature: 0.5,
        });
        let out = format_status_block(&s);
        assert!(out.contains("Tool calls:   openai-compat"));
        assert!(out.contains("Thinking:     no"));
    }

    // ── /jobs helpers: format_job_status + tail_lines ───────────
    //
    // Both are pure (no I/O, no async) so we can drive them with
    // hand-built BashJob values. The full `handle_jobs_command` flow
    // is integration-tested by the bash_jobs module's own tests; the
    // tests here pin down the *format* contract that the list view,
    // the detail view, and the completion notifier all depend on.

    use crate::session::bash_jobs::{BashJob, JobStatus};

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

    /// All four status variants render as `#<id>` (no `(id=...)` form).
    /// The old code had `(id=5)` for Running and `#5` for the others —
    /// this test pins down the consolidated form so the list, detail,
    /// and notifier views stay consistent.
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

    /// `tail_lines("", n)` returns the empty input unchanged with
    /// 0 elided — guards the early-return path in `tail_lines`.
    #[test]
    fn tail_lines_empty_input_returns_empty() {
        let (out, elided) = tail_lines("", 10);
        assert_eq!(out, "");
        assert_eq!(elided, 0);
    }

    /// When the input is shorter than the limit, no elision happens.
    #[test]
    fn tail_lines_short_input_unchanged() {
        let (out, elided) = tail_lines("a\nb\nc", 10);
        assert_eq!(out, "a\nb\nc");
        assert_eq!(elided, 0);
    }

    /// When the input is longer, the LAST `n` lines survive and the
    /// count of dropped leading lines is reported.
    #[test]
    fn tail_lines_long_input_keeps_tail_and_reports_elided() {
        let input = (1..=100)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (out, elided) = tail_lines(&input, 5);
        assert_eq!(elided, 95);
        // Tail should be the last 5 lines joined
        assert_eq!(out, "line96\nline97\nline98\nline99\nline100");
        // The first 95 should be gone
        assert!(!out.contains("line1\n"));
        assert!(!out.contains("line50"));
    }

    /// The boundary at exactly `n` lines: no elision reported.
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

    // ── handle_jobs_command integration tests ───────────────────
    //
    // These tests hit the real `global_registry()`. They run in
    // parallel with the rest of the test suite by default, and
    // cargo's test harness runs them across multiple threads. The
    // global registry is shared, so a `clean()` call from one test
    // would otherwise race with a `spawn()` + `get()` from another.
    //
    // The 5 tests that MUTATE the registry (`spawn` or `clean`)
    // therefore acquire `TEST_REGISTRY_LOCK` for their whole
    // body. The 4 read-only tests (no spawn, no clean) leave it
    // alone — they only assert on "not found" / usage messages
    // and tolerate whatever state the registry is in.
    //
    // We use a `OnceLock<tokio::sync::Mutex<()>>` rather than
    // `std::sync::Mutex` because the tests are async — the
    // `tokio::sync::Mutex` lets us hold the guard across `.await`
    // points. The `OnceLock` initialises the lock lazily on first
    // access; the inner value is the lock itself.
    //
    // Locking order is irrelevant because all 5 mutating tests
    // acquire only this one lock. Holding it for the whole test
    // body means each mutating test runs effectively serialized
    // relative to the others, but the 4 read-only tests can still
    // race in parallel — which is fine because they only observe.

    use std::sync::OnceLock;
    static TEST_REGISTRY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    fn test_registry_lock() -> &'static tokio::sync::Mutex<()> {
        TEST_REGISTRY_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    /// `/jobs` with no args includes the job we just spawned. The
    /// marker we look for is the unique command string, so this test
    /// is robust against other tests' jobs being in the registry.
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

    /// `/jobs <id>` for a finished job shows the full output.
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

    /// `/jobs <id>` for a non-existent id returns a "not found" message
    /// that lists available job ids. Uses a high id (999_999) that
    /// is extremely unlikely to collide.
    #[tokio::test]
    async fn handle_jobs_command_detail_unknown_id_says_not_found() {
        let out = handle_jobs_command("999999").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    /// `/jobs foo` (non-numeric, non-"clean") returns a usage hint.
    #[tokio::test]
    async fn handle_jobs_command_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("/jobs foo"), "got: {}", out);
    }

    /// `/jobs clean` returns either "cleaned N" or "nothing to clean"
    /// depending on what other tests left behind. Both are valid
    /// outcomes for the global registry. The point is that the
    /// command does not panic and does not return an error string.
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

    /// `/jobs <id> cancel` for a running job returns a cancellation
    /// confirmation. Uses a long-running `sleep` so the job is still
    /// in `Running` state when we call cancel.
    #[tokio::test]
    async fn handle_jobs_command_cancel_running_job_succeeds() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        // The `unique` stamp goes in a shell comment so `sleep`
        // gets exactly one argument. Without the `#`, `sleep` would
        // treat `kf_test_cancel_<pid>` as a second time-interval
        // and error out before the 5s elapse.
        let unique = format!("sleep 5  # kf_test_cancel_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        // Give the spawned process a moment to register as Running.
        // No `break` here — we want the job to still be running when
        // we cancel it.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let out = handle_jobs_command(&format!("{} cancel", id)).await;
        assert!(out.contains("Cancel"), "got: {}", out);
        assert!(out.contains(&format!("#{}", id)), "got: {}", out);

        // Verify the job is now in Cancelled status (best-effort:
        // a `sleep 5` should still be running when we cancel, but
        // the kill might race with natural completion in CI).
        let job = registry.get(id).await;
        if let Some(j) = job {
            assert!(
                matches!(j.status, JobStatus::Cancelled | JobStatus::Failed(_)),
                "expected cancelled or failed, got: {:?}",
                j.status
            );
        }
    }

    /// `/jobs <id> cancel` for an already-completed job returns a
    /// "not running" error pointing the user at the actual status.
    #[tokio::test]
    async fn handle_jobs_command_cancel_finished_job_returns_error() {
        let _guard = test_registry_lock().lock().await;
        let registry = crate::session::bash_jobs::global_registry();
        let unique = format!("echo kf_test_cancel_done_{}", std::process::id());
        let id = registry.spawn(&unique, None, None).await.unwrap();
        // Wait for it to finish.
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

    /// `/jobs <id> cancel` for a non-existent id returns a "not found"
    /// message. Uses a high id (999_999) to avoid collision.
    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_id_returns_not_found() {
        let out = handle_jobs_command("999999 cancel").await;
        assert!(out.contains("not found"), "got: {}", out);
    }

    /// `/jobs cancel` (no id) returns a usage hint. Without the id
    /// there's nothing to cancel.
    #[tokio::test]
    async fn handle_jobs_command_cancel_without_id_returns_usage() {
        let out = handle_jobs_command("cancel").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    /// `/jobs <id> foo` (unknown sub-command) returns a usage hint.
    /// We don't want silent typos like `/jobs 5 stop` to cancel
    /// anything — they should fail loudly.
    #[tokio::test]
    async fn handle_jobs_command_cancel_unknown_subcommand_returns_usage() {
        let out = handle_jobs_command("5 foo").await;
        assert!(out.contains("Usage"), "got: {}", out);
        assert!(out.contains("foo"), "got: {}", out);
    }

    /// `/jobs 5 cancel now` (extra token past cancel) returns a usage
    /// hint. Pin this down to prevent silent ignoring of trailing args.
    #[tokio::test]
    async fn handle_jobs_command_cancel_extra_token_returns_usage() {
        let out = handle_jobs_command("5 cancel now").await;
        assert!(out.contains("Usage"), "got: {}", out);
    }

    // ========================================================================
    // v1.2-p14 — `!` bash passthrough tests
    // ========================================================================

    // ----- format_bang_output (pure) -----

    /// A successful `BangResult` with no stderr renders as:
    /// - `$ <cmd>` header
    /// - `✅ exit 0 in <elapsed>` banner
    /// - stdout verbatim below
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
        // sub-second shows ms granularity
        assert!(out.contains("42ms"), "got: {:?}", out);
    }

    /// A non-zero exit code surfaces a `❌` banner and includes the code.
    /// This is critical: a silent success on `cargo build` that actually
    /// exited 1 would be very confusing.
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

    /// A timed-out command shows the `⏰` icon and the timeout duration.
    /// Stdout/stderr are not rendered separately (the process was killed).
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

    /// Success with both stdout and stderr: stderr still gets a `⚠ stderr:`
    /// marker even though the command exited 0. Many real tools (cargo,
    /// npm, gcc) write progress to stderr; this lets the user see it
    /// distinctly from stdout.
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

    /// Empty stdout + empty stderr: just the banner, no extra blank line.
    /// Common case for `!true`, `!cd` (silent), `!export FOO=bar`.
    #[test]
    fn format_bang_output_empty_output_is_just_banner() {
        let r = sample_success("true", "", 5);
        let out = format_bang_output(&r);
        assert!(out.contains("$ true"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        // No stderr marker
        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    // ----- format_elapsed (pure) -----

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

    // ----- is_success (pure) -----

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

    // ----- handle_bang_command (integration, real shell) -----

    /// `!echo hi` actually runs echo and produces "hi" in the output.
    /// This is the minimum end-to-end check: subprocess wiring works.
    #[tokio::test]
    async fn handle_bang_command_echo_runs() {
        let out = handle_bang_command("echo hi").await;
        assert!(out.contains("$ echo hi"), "got: {:?}", out);
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(out.contains("hi"), "got: {:?}", out);
    }

    /// `!true` exits 0 with no output. Banner-only, no stderr marker.
    #[tokio::test]
    async fn handle_bang_command_true_exits_zero_silently() {
        let out = handle_bang_command("true").await;
        assert!(out.contains("✅ exit 0"), "got: {:?}", out);
        assert!(!out.contains("⚠"), "got: {:?}", out);
    }

    /// `!false` exits non-zero. Banner says `❌ exit 1`.
    #[tokio::test]
    async fn handle_bang_command_false_exits_nonzero() {
        let out = handle_bang_command("false").await;
        assert!(out.contains("❌ exit 1"), "got: {:?}", out);
    }

    /// `!` with nothing after it returns a usage hint, not a crash.
    /// Pin this so we never end up running `/bin/sh -c ""` and waiting
    /// for the timeout for no reason.
    #[tokio::test]
    async fn handle_bang_command_empty_returns_usage() {
        let out = handle_bang_command("").await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    /// Whitespace-only `!   ` is also "empty" for our purposes.
    #[tokio::test]
    async fn handle_bang_command_whitespace_only_returns_usage() {
        let out = handle_bang_command("   ").await;
        assert!(out.contains("Usage"), "got: {:?}", out);
    }

    // ── @-mention tests (v1.2-p15) ─────────────────────────────────

    /// `parse_mentions` returns an empty vec for prose with no @-tokens.
    #[test]
    fn parse_mentions_empty_for_prose() {
        let m = parse_mentions("hello world, no mentions here");
        assert!(m.is_empty());
    }

    /// A single @-mention is parsed with the right offsets.
    #[test]
    fn parse_mentions_single_token() {
        let m = parse_mentions("please review @src/main.rs carefully");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "src/main.rs");
        assert_eq!(m[0].spec.range, None);
        assert!(!m[0].spec.raw);
        // "@" is at byte index 14: "please review " is 14 bytes
        // (p=0, l=1, e=2, a=3, s=4, e=5, ' '=6, r=7, e=8, v=9,
        //  i=10, e=11, w=12, ' '=13, @=14).
        assert_eq!(m[0].start, 14);
        // ends right before " carefully"
        assert_eq!(m[0].end, 14 + "@src/main.rs".len());
    }

    /// Multiple @-mentions in one input are all parsed, in order.
    #[test]
    fn parse_mentions_multiple_tokens() {
        let m = parse_mentions("look at @a.rs and @b/c.py:10-20:raw please");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].spec.path, "a.rs");
        assert_eq!(m[1].spec.path, "b/c.py");
        assert_eq!(m[1].spec.range, Some((10, 20)));
        assert!(m[1].spec.raw);
    }

    /// `@@` (double-@) is treated as literal text, not a mention.
    #[test]
    fn parse_mentions_double_at_is_literal() {
        let m = parse_mentions("send email to @@user");
        assert!(m.is_empty(), "got: {:?}", m);
    }

    /// A trailing `@` with nothing after it is not a mention.
    #[test]
    fn parse_mentions_trailing_at_ignored() {
        let m = parse_mentions("what about @");
        assert!(m.is_empty());
    }

    /// `@` followed immediately by whitespace is not a mention.
    #[test]
    fn parse_mentions_at_before_space_ignored() {
        let m = parse_mentions("try @ then run");
        assert!(m.is_empty());
    }

    /// `:raw` suffix is recognised and sets the raw flag.
    #[test]
    fn parse_mentions_raw_suffix() {
        let m = parse_mentions("inline @notes.md:raw now");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.path, "notes.md");
        assert!(m[0].spec.raw);
    }

    /// `START-END` range is parsed and must be 1-indexed.
    #[test]
    fn parse_mentions_range() {
        let m = parse_mentions("see @foo.rs:1-10");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].spec.range, Some((1, 10)));
    }

    /// A range with start > end is rejected (returns literal text).
    #[test]
    fn parse_mentions_invalid_range_rejected() {
        let m = parse_mentions("see @foo.rs:10-5");
        assert!(
            m.is_empty(),
            "inverted range should be rejected, got: {:?}",
            m
        );
    }

    /// A range with non-numeric body is rejected.
    #[test]
    fn parse_mentions_non_numeric_range_rejected() {
        let m = parse_mentions("see @foo.rs:abc-def");
        assert!(m.is_empty());
    }

    /// `strip_mentions` removes the @-tokens and the whitespace after
    /// them, leaving the surrounding text intact.
    #[test]
    fn strip_mentions_removes_tokens() {
        let input = "look at @a.rs and @b.py now";
        let mentions = parse_mentions(input);
        let cleaned = strip_mentions(input, &mentions);
        assert_eq!(cleaned, "look at  and  now", "got: {:?}", cleaned);
    }

    /// `strip_mentions` is a no-op when there are no mentions.
    #[test]
    fn strip_mentions_no_mentions_unchanged() {
        let input = "nothing to strip here";
        let cleaned = strip_mentions(input, &parse_mentions(input));
        assert_eq!(cleaned, input);
    }

    /// `expand_mentions` reads a real file (via tempfile) and returns
    /// an `Ok` status with the byte count.
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
        assert!(expansions[0].is_ok());
        assert_eq!(expansions[0].content, "hello world");
    }

    /// `expand_mentions` returns `NotFound` for a missing path.
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

    /// `expand_mentions` applies a 1-indexed line range.
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

    /// `expand_mentions` returns `InvalidRange` if start is 0.
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

    /// `render_mentions_block` is empty when there are no expansions.
    #[test]
    fn render_mentions_block_empty_for_no_expansions() {
        assert_eq!(render_mentions_block(&[]), "");
    }

    /// `render_mentions_block` produces a fenced block per expansion.
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

    /// `render_mentions_block` renders a `not found` placeholder for
    /// failed expansions so the model can react.
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

    /// `format_mention_status` is empty when there are no expansions.
    #[test]
    fn format_mention_status_empty_for_no_expansions() {
        assert_eq!(format_mention_status(&[]), "");
    }

    /// `format_mention_status` lists each mention with a status icon.
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

    /// Tilde expansion works in the path component.
    #[test]
    fn expand_mentions_tilde_expansion() {
        // We can't reliably create a file at $HOME in CI, so just
        // verify that the expansion function does NOT panic on a tilde
        // path and returns a NotFound or IoError (not a parse error).
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
        // Either NotFound (if the home dir exists but the file doesn't)
        // or IoError (if the home dir is missing). Both are fine — the
        // important property is that tilde expansion didn't crash.
        assert!(matches!(
            expansions[0].status,
            MentionStatus::NotFound | MentionStatus::IoError(_)
        ));
    }
}
