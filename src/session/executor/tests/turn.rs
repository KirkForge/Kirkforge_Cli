// Single-turn, post-turn guard, and executor direct-method tests. Split
// out from the former single-file `mod.rs` (WO 15.5). Pure refactor: test
// bodies are moved verbatim.

use super::super::turn::PostTurnHookGuard;
use super::super::types::PLAN_COMPLETE_MARKER;
use super::super::*;
use super::common::*;
use crate::shared::test_util::remove_test_file;
use crate::shared::{FinishReason, ModelInfo, StreamEvent, ToolDef};
/// Smoke test for `PostTurnHookGuard`. Constructs a guard with the
/// default `HookRunner` and lets it fall out of scope. The
/// `HookRunner::run` call inside `Drop` is fire-and-forget and
/// (in the absence of a real `~/.local/share/kf-code/hooks/
/// post-turn.sh`) is a no-op, so this test exercises construction
/// and Drop without making any external assumptions.
///
/// The real value is at compile time: if `PostTurnHookGuard` ever
/// stops being `pub`, or `HookRunner` stops being `Clone`, this
/// test fails to build — catching the regression before it
/// silently breaks the post-turn hook fire path.
#[test]
fn post_turn_guard_constructs_and_drops() {
    let _guard = PostTurnHookGuard::new(HookRunner::default(), Config::default());
}

/// `reload_config` rebuilds access control from a new config and
/// reports the changed fields. This exercises the hot-reload path
/// without needing a live TUI or SIGHUP signal.
#[test]
fn reload_config_rebuilds_and_reports_changes() {
    let adapter = MockAdapter::new(vec![], make_info());
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();

    let mut new_config = make_config(false);
    new_config.model.default_model = "qwen2.5:14b".into();
    new_config.model.json_mode = true;
    new_config.session.carryover_enabled = true;

    let summary = exe.reload_config(new_config.clone());

    assert!(
        summary.contains("default_model")
            || summary.contains("json_mode")
            || summary.contains("carryover_enabled"),
        "reload_config should report changed high-impact fields, got: {summary}"
    );

    // The shared lock should hold the new values.
    let cfg = cfg(&exe);
    assert_eq!(cfg.model.default_model, "qwen2.5:14b");
    assert!(cfg.model.json_mode);
    assert!(cfg.session.carryover_enabled);
}

#[tokio::test]
async fn test_plan_complete_marker_emits_event() {
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("Here is the plan.".to_string()),
            StreamEvent::Text(format!("\n{PLAN_COMPLETE_MARKER}\n")),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("plan this", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        events.iter().any(|e| matches!(e, TurnEvent::PlanComplete)),
        "Expected PlanComplete event, got events: {events:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_compact_hooks_fire_pre_and_post() {
    let (_tmp, hooks_dir) = temp_hooks_dir();
    let pre_marker = hooks_dir.join("pre-compact-marker.txt");
    let post_marker = hooks_dir.join("post-compact-marker.txt");

    std::fs::write(
        hooks_dir.join("pre-compact.sh"),
        format!(
            "#!/bin/bash\necho \"$KF_TOOL_ARGS_JSON\" > {}",
            pre_marker.to_string_lossy()
        ),
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("post-compact.sh"),
        format!(
            "#!/bin/bash\necho \"$KF_TOOL_ARGS_JSON\" > {}",
            post_marker.to_string_lossy()
        ),
    )
    .unwrap();

    let mut config = make_config(false);
    config.tools.hooks_dir = Some(hooks_dir);
    let exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        config,
    )
    .unwrap();

    exe.run_compact_hook(
        "pre-compact",
        CompactHookStats {
            message_count: 20,
            preserve_recent: 2,
            original_count: 20,
            result_count: 20,
            dropped_tool_results: 0,
            condensed_assistant_turns: 0,
            summarised_messages: 0,
            strategy: "pending",
        },
    );
    exe.run_compact_hook(
        "post-compact",
        CompactHookStats {
            message_count: 20,
            preserve_recent: 2,
            original_count: 20,
            result_count: 8,
            dropped_tool_results: 5,
            condensed_assistant_turns: 3,
            summarised_messages: 0,
            strategy: "naive",
        },
    );

    // Poll for both hook marker files. Bounded to 15s (5s hook timeout +
    // scheduling slop) with a 10ms interval — replaces a bare 50ms-paced loop.
    let poll = async {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let (mut pre_content, mut post_content) = (String::new(), String::new());
        while std::time::Instant::now() < deadline {
            if pre_content.is_empty() {
                if let Ok(c) = std::fs::read_to_string(&pre_marker) {
                    pre_content = c;
                }
            }
            if post_content.is_empty() {
                if let Ok(c) = std::fs::read_to_string(&post_marker) {
                    post_content = c;
                }
            }
            if !pre_content.is_empty() && !post_content.is_empty() {
                return (pre_content, post_content);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        (pre_content, post_content)
    };
    let (pre_content, post_content) = poll.await;

    assert!(
        !pre_content.is_empty(),
        "pre-compact hook should have written its marker"
    );
    assert!(
        !post_content.is_empty(),
        "post-compact hook should have written its marker"
    );

    let pre_json: serde_json::Value =
        serde_json::from_str(&pre_content).expect("pre-compact hook wrote valid JSON");
    let post_json: serde_json::Value =
        serde_json::from_str(&post_content).expect("post-compact hook wrote valid JSON");

    assert_eq!(pre_json["strategy"], "pending");
    assert_eq!(pre_json["message_count"], 20);

    assert_eq!(post_json["strategy"], "naive");
    assert_eq!(post_json["original_count"], 20);
    assert_eq!(post_json["result_count"], 8);
    assert_eq!(post_json["dropped_tool_results"], 5);
    assert_eq!(post_json["condensed_assistant_turns"], 3);
}

// ── executor/mod.rs direct method tests (WO 12-series coverage) ────────

#[tokio::test]
async fn exit_plan_mode_appends_system_message() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    exe.set_plan_mode(true);
    let msg = exe.exit_plan_mode().await.expect("exit plan mode");
    assert!(msg.contains("Plan mode exited"));
    let all = exe.conversation_log().all();
    let last = all.last().expect("at least one message");
    assert_eq!(last.role, Role::System);
    assert!(last.content.contains("implement the plan"));
}

#[tokio::test]
async fn replace_conversation_swaps_log() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    // Add a message to the old log so we can verify the swap.
    exe.conversation
        .append_async(Message {
            role: Role::User,
            content: "hello".into(),
            content_parts: None,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        })
        .await
        .unwrap();
    let old_count = exe.conversation_log().all().len();
    assert_eq!(old_count, 1, "old log should have 1 message");
    let temp_dir = std::env::temp_dir();
    let new_path = temp_dir.join(format!(
        "kf-code-test-replace-{}-{}.ndjson",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    remove_test_file(&new_path);
    let (new_log, _) = ConversationLog::open(new_path.clone()).unwrap();
    exe.replace_conversation(new_log);
    assert_eq!(
        exe.conversation_log().all().len(),
        0,
        "new log should be empty"
    );
    assert_ne!(
        exe.conversation_log().all().len(),
        old_count,
        "log should have been swapped"
    );
    remove_test_file(&new_path);
}

#[tokio::test]
async fn set_system_override_stores_and_clears() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    exe.set_system_override(Some("custom prompt".into()));
    assert_eq!(exe.system_override(), Some("custom prompt"));
    exe.set_system_override(None);
    assert_eq!(exe.system_override(), None);
}

// WO 36.3: an executor with an attached root cancel token must abort an
// in-flight (stalled) model stream at cancel time, not at the next stream
// event or the adapter timeout. Pre-WO 36.3 the loop awaited `rx.recv()`
// inline, so a stream that never produced another event kept the turn
// alive until `request_timeout_secs`. The stall is a `pending()` future —
// the cancel is event-driven, no sleep-races (interval-overlap standard).
struct StalledStreamAdapter;

#[async_trait::async_trait]
impl ModelAdapter for StalledStreamAdapter {
    fn model_info(&self) -> ModelInfo {
        make_info()
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            // One token, then the stream stalls forever: the turn loop is
            // parked in `rx.recv().await` until the cancel token fires.
            let _ = tx.send(StreamEvent::Text("partial".to_string())).await;
            std::future::pending::<()>().await;
        });
        Ok(rx)
    }
}

#[tokio::test]
async fn cancel_token_aborts_stalled_model_stream() {
    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe =
        make_executor(Box::new(StalledStreamAdapter), vec![], make_config(false)).unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    exe.set_cancel_token(Some(token.clone()));
    let exe = std::sync::Arc::new(tokio::sync::Mutex::new(exe));

    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let (event_tx, mut event_rx) = mpsc::channel::<TurnEvent>(64);
    let turn_exe = std::sync::Arc::clone(&exe);
    let turn = tokio::spawn(async move {
        let mut guard = turn_exe.lock().await;
        guard
            .run_turn("hello", &approval_tx, &cancelled, &event_tx)
            .await
            .expect("cancelled turn returns Ok, not Err")
    });

    // Wait for the single streamed token — proves the turn is parked in
    // the stalled stream before we cancel.
    loop {
        let ev = event_rx.recv().await.expect("event stream alive");
        if matches!(ev, TurnEvent::Token(ref t) if t == "partial") {
            break;
        }
    }
    token.cancel();

    tokio::time::timeout(std::time::Duration::from_secs(2), turn)
        .await
        .expect("turn must return within 2s of cancel, not adapter timeout")
        .expect("turn task must not panic");

    // Cooperative-cancel semantics: TurnComplete was emitted (it is sent
    // before run_turn returns) and the partial assistant message was
    // flushed into the conversation.
    let mut saw_complete = false;
    while let Ok(ev) = event_rx.try_recv() {
        if matches!(ev, TurnEvent::TurnComplete) {
            saw_complete = true;
        }
    }
    assert!(
        saw_complete,
        "TurnComplete must fire after mid-stream cancel"
    );
    let exe = std::sync::Arc::try_unwrap(exe)
        .ok()
        .expect("turn task dropped its executor ref");
    let msgs = exe.into_inner().conversation_log().all().to_vec();
    assert!(
        msgs.iter()
            .any(|m| matches!(m.role, Role::Assistant) && m.content.contains("partial")),
        "cancelled stream must flush the partial assistant message; got {msgs:?}"
    );
}
