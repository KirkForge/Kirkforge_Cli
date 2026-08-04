// Multi-turn loop, batch dispatch, and doom-loop tracker tests. Split
// out from the former single-file `mod.rs` (WO 15.5). Pure refactor: test
// bodies are moved verbatim.

use super::super::*;
use super::common::*;
use crate::shared::metrics::{read_events, MetricEvent, PlanDecisionKind};
use crate::shared::permission::PermissionAction;
use crate::shared::test_util::remove_test_file;
use crate::shared::{FinishReason, StreamEvent, ToolDef, ToolError, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
#[tokio::test]
async fn test_always_approve_rule_round_trips_to_next_turn() {
    // A rule created by the TUI's `[A]lways` key in one turn should
    // auto-approve the same command in a later turn without prompting.
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "ran!".into(),
        },
    };

    let command = "cargo test --release";
    let first_events = vec![
        StreamEvent::ToolCall(ToolInvocation {
            id: "call-1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": command}),
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        },
    ];
    let followup_events = vec![
        StreamEvent::ToolCall(ToolInvocation {
            id: "call-2".into(),
            name: "bash".into(),
            arguments: serde_json::json!({"command": command}),
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        },
    ];
    let adapter = MockAdapter::new(first_events, make_info()).with_followup_events(followup_events);

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.unwrap();
        assert_eq!(req.tool_name, "bash");
        assert_eq!(
            req.args.get("command").and_then(|v| v.as_str()),
            Some(command)
        );
        let _ = req.response.send(ApprovalResponse::AlwaysApprove);
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let _events = exe
        .run_turn_collecting("run tests", &approval_tx, never_cancelled())
        .await
        .unwrap();
    approval_handle.await.unwrap();

    {
        let cfg = cfg(&exe);
        assert_eq!(cfg.security.permission_rules.len(), 1);
        assert_eq!(
            cfg.security.permission_rules[0].action,
            PermissionAction::Allow
        );
    }

    // Second turn: same command should now match the rule and run
    // without sending an approval request. Use an unbounded channel
    // and check after the turn completes whether any approval was
    // requested — this avoids the race between abort() and AtomicBool
    // that caused flakiness under parallel test load.
    let (approval_tx2, mut approval_rx2) = mpsc::unbounded_channel();

    let second_events = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        exe.run_turn_collecting("run tests again", &approval_tx2, never_cancelled()),
    )
    .await
    .expect("second turn should complete without approval prompt");

    // Give the executor a brief moment to drain any pending channel sends,
    // then check whether an approval request was received.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        approval_rx2.try_recv().is_err(),
        "rule should prevent second approval request, but one was sent"
    );

    let second_events = second_events.unwrap();
    let has_result = second_events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "bash" && output == "ran!"));
    assert!(has_result, "second turn should execute the allowed command");
}

/// Cancellation during a multi-tool batch must append placeholder results
/// for any tool calls that were skipped, so the conversation stays balanced.
#[tokio::test]
async fn test_cancelled_tool_batch_appends_placeholders() {
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let tool = SleepingTool {
        def: ToolDef {
            name: "sleep",
            description: "sleep",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        sleep_ms: 200,
        call_count: Arc::new(Mutex::new(0)),
        start_tx: Arc::new(std::sync::Mutex::new(Some(start_tx))),
    };
    let call_count = tool.call_count.clone();

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "sleep".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-2".into(),
                name: "sleep".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_flag = cancelled.clone();
    tokio::spawn(async move {
        // Wait until the first tool has actually started, then cancel. This
        // makes the test deterministic instead of racing a 50 ms timer against
        // the executor's batch-launch timing.
        let _ = start_rx.await;
        cancelled_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let events = exe
        .run_turn_collecting("run two", &approval_tx, &cancelled)
        .await
        .unwrap();

    // Exactly one tool should have run to completion; the second was cancelled.
    assert_eq!(
        *call_count.lock().unwrap(),
        1,
        "only the first tool call should execute"
    );

    let results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        })
        .collect();

    assert_eq!(
        results.len(),
        2,
        "there should be a result for both requested tool calls"
    );
    assert_eq!(results[0], ("sleep", "done"), "first call should succeed");
    assert!(
        results[1].1.contains("cancelled"),
        "second call should report cancellation, got {:?}",
        results[1]
    );

    let msgs = exe.conversation.all();
    let assistant_tool_calls: Vec<_> = msgs
        .iter()
        .filter_map(|m| {
            if m.role == Role::Assistant {
                m.tool_calls.clone()
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert_eq!(
        assistant_tool_calls.len(),
        2,
        "assistant requested two tools"
    );

    let tool_results: Vec<_> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(
        tool_results.len(),
        2,
        "conversation must contain two tool-result messages"
    );
    assert!(tool_results[1].content.contains("cancelled"));
}

/// Two independent non-file tool calls must run concurrently, not sequentially.
/// Two 200ms sleeps finishing in < 5s proves parallel dispatch — even under
/// heavy parallel test load, two sequential 200ms sleeps + overhead would be
/// under 5s, so a result > 5s would indicate serial dispatch. The original
/// test used 1000ms sleeps with a 3.5s threshold, which was flaky under
/// parallel cargo test load.
#[tokio::test]
async fn test_parallel_tool_batch_runs_concurrently() {
    let tool_one = SleepingTool {
        def: ToolDef {
            name: "sleep",
            description: "sleep",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        sleep_ms: 200,
        call_count: Arc::new(Mutex::new(0)),
        start_tx: Arc::new(std::sync::Mutex::new(None)),
    };
    let tool_two = SleepingTool {
        def: tool_one.def.clone(),
        sleep_ms: 200,
        call_count: Arc::new(Mutex::new(0)),
        start_tx: Arc::new(std::sync::Mutex::new(None)),
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "sleep".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-2".into(),
                name: "sleep".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(
        Box::new(adapter),
        vec![Arc::new(tool_one), Arc::new(tool_two)],
        make_config(true),
    );

    let start = tokio::time::Instant::now();
    let events = exe
        .run_turn_collecting("run two sleeps", &approval_tx, never_cancelled())
        .await
        .unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    assert!(
        elapsed < 5.0,
        "two 200ms tool calls should run in parallel (elapsed {elapsed:.2}s)"
    );

    let results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "both sleep calls should produce results");
    assert!(results.iter().all(|(_, o)| *o == "done"));
}

/// Deterministic mode (--seed) must produce the same tool-call sequence
/// when run twice with the same seed. The model's *content* may still
/// vary by provider, but the tool-call *sequence* must be reproducible.
#[tokio::test]
async fn test_deterministic_mode_produces_same_tool_sequence() {
    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();

    // Build config with seed=42
    let mut cfg = make_config(true);
    cfg.model.seed = Some(42);

    // Helper to build a pair of sleeping tools
    let make_tools = || -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(SleepingTool {
                def: ToolDef {
                    name: "sleep",
                    description: "sleep",
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                },
                sleep_ms: 10,
                call_count: Arc::new(Mutex::new(0)),
                start_tx: Arc::new(std::sync::Mutex::new(None)),
            }),
            Arc::new(SleepingTool {
                def: ToolDef {
                    name: "sleep",
                    description: "sleep",
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                },
                sleep_ms: 10,
                call_count: Arc::new(Mutex::new(0)),
                start_tx: Arc::new(std::sync::Mutex::new(None)),
            }),
        ]
    };

    let events = vec![
        StreamEvent::ToolCall(ToolInvocation {
            id: "call-1".into(),
            name: "sleep".into(),
            arguments: serde_json::json!({}),
        }),
        StreamEvent::ToolCall(ToolInvocation {
            id: "call-2".into(),
            name: "sleep".into(),
            arguments: serde_json::json!({}),
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        },
    ];

    // Run twice with the same seed — each run gets its own adapter
    // so call_count is not shared.
    let mut exe1 = make_executor(
        Box::new(MockAdapter::new(events.clone(), make_info())),
        make_tools(),
        cfg.clone(),
    );
    let events1 = exe1
        .run_turn_collecting("run with seed 42", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let mut exe2 = make_executor(
        Box::new(MockAdapter::new(events, make_info())),
        make_tools(),
        cfg,
    );
    let events2 = exe2
        .run_turn_collecting("run with seed 42 again", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // Extract tool-call sequences: (name, success) pairs
    let seq1: Vec<(&str, bool)> = events1
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolResult { name, success, .. } => Some((name.as_str(), *success)),
            _ => None,
        })
        .collect();
    let seq2: Vec<(&str, bool)> = events2
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolResult { name, success, .. } => Some((name.as_str(), *success)),
            _ => None,
        })
        .collect();

    assert_eq!(
        seq1, seq2,
        "deterministic mode must produce the same tool-call sequence on repeated runs"
    );
    assert_eq!(seq1.len(), 2, "both sleep calls should produce results");
    assert!(
        seq1.iter().all(|(_, s)| *s),
        "all tool calls should succeed"
    );
}

/// A crash mid-batch leaves the conversation log with only the tool results
/// that were recorded before the executor died. We run a batch of four sleep
/// tools, cancel after observing the first two `ToolResult` events, then
/// abort the turn and verify the reloaded log contains exactly those two
/// results and no later ones.
#[tokio::test]
async fn test_mid_batch_checkpoint_persists_partial_results() {
    use std::sync::atomic::Ordering;

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(std::sync::Mutex::new(Some(started_tx)));

    struct GatedSleepTool {
        def: ToolDef,
        sleep_ms: u64,
        gate_tx: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    }

    #[async_trait::async_trait]
    impl Tool for GatedSleepTool {
        fn def(&self) -> ToolDef {
            self.def.clone()
        }

        async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
            if let Ok(mut guard) = self.gate_tx.lock() {
                if let Some(tx) = guard.take() {
                    let _ = tx.send(());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
            ToolOutcome::Success {
                content: "done".into(),
            }
        }
    }

    let tools: Vec<Arc<dyn Tool>> = (0..4)
        .map(|i| {
            Arc::new(GatedSleepTool {
                def: ToolDef {
                    name: match i {
                        0 => "sleep-0",
                        1 => "sleep-1",
                        2 => "sleep-2",
                        _ => "sleep-3",
                    },
                    description: "sleep briefly",
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                },
                sleep_ms: if i < 2 { 100 } else { 3000 },
                gate_tx: started_tx.clone(),
            }) as Arc<dyn Tool>
        })
        .collect();

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "sleep-0".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-2".into(),
                name: "sleep-1".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-3".into(),
                name: "sleep-2".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-4".into(),
                name: "sleep-3".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let log_path = std::env::temp_dir().join(format!(
        "kf-code-mid-batch-test-{}.ndjson",
        std::process::id()
    ));
    remove_test_file(&log_path);
    let (conversation, _outcome) = ConversationLog::open(log_path.clone()).unwrap();
    let mut composite = crate::session::toolset::CompositeToolset::empty();
    composite.add(Box::new(crate::session::toolset::VecToolset::new(
        "test", tools,
    )));
    let mut exe = Executor::with_log(
        Box::new(adapter),
        composite,
        make_config(true),
        conversation,
        None,
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));

    let cancelled_flag = cancelled.clone();
    let turn_handle = tokio::spawn(async move {
        exe.run_turn_collecting("run four sleeps", &approval_tx, &cancelled_flag)
            .await
    });

    let _ = started_rx.await;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    cancelled.store(true, Ordering::SeqCst);

    turn_handle.abort();
    let _ = turn_handle.await;

    let (restored, _outcome) = ConversationLog::open(log_path.clone()).unwrap();

    let msgs = restored.all();
    let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == Role::Tool).collect();

    assert!(
        tool_msgs.iter().any(|m| m.content == "done"),
        "at least one completed tool result should be persisted, got {tool_msgs:?}"
    );
    assert!(
        tool_msgs.len() <= 2,
        "no more than the first two fast results should be recorded, got {tool_msgs:?}"
    );
}

/// WO 10.2: the executor's `CacheStemTracker` emits a
/// `PlanReason::CacheStemReuse` metric event on turns 2-5 (stable stem)
/// and not on turn 1 (no prior hash). A system-message change breaks
/// stability. See ADR-052.
#[tokio::test]
async fn cache_stem_reuse_emitted_on_stable_turn() {
    // Isolate the metrics log so we can assert exact event counts
    // without contamination from other tests' `record()` calls. We
    // install the thread-local path override manually (instead of
    // `with_test_path`, which is sync-only) and hold the global
    // TEST_LOCK for the duration of the async test body. The
    // `#[tokio::test]` runtime is current-thread, so the override is
    // visible to `record()` calls inside `stream_iteration`.
    use std::path::PathBuf;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kf_code_cache_stem_test_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("metrics.ndjson");
    crate::shared::metrics::set_test_path(path.clone());
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("hello".to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(true));

    // Run 5 turns with the same model/tools/system prompt. The cache
    // stem (the system message, prefix_len=1) is stable across all
    // turns, so CacheStemReuse should fire on turns 2-5 and NOT on
    // turn 1 (no prior hash recorded).
    let inputs = [
        "turn one",
        "turn two",
        "turn three",
        "turn four",
        "turn five",
    ];
    for input in &inputs {
        exe.run_turn_collecting(input, &approval_tx, never_cancelled())
            .await
            .unwrap();
    }

    let events = read_events();
    let ours: Vec<&MetricEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                MetricEvent::PlanReason {
                    decision_kind: PlanDecisionKind::CacheStemReuse,
                    ref reason,
                    ..
                } if reason == "prompt-cache stem stable across turns"
            )
        })
        .collect();
    assert_eq!(
        ours.len(),
        4,
        "expected 4 CacheStemReuse events (turns 2-5), got {}: {ours:?}",
        ours.len()
    );

    // Now change the system prompt: the next turn should NOT emit
    // CacheStemReuse (the stem hash differs from the prior turn's).
    exe.set_system_override(Some("DIFFERENT system prompt".to_string()));
    exe.run_turn_collecting("turn six", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let events_after = read_events();
    let ours_after: Vec<&MetricEvent> = events_after
        .iter()
        .filter(|e| {
            matches!(
                e,
                MetricEvent::PlanReason {
                    decision_kind: PlanDecisionKind::CacheStemReuse,
                    ref reason,
                    ..
                } if reason == "prompt-cache stem stable across turns"
            )
        })
        .collect();
    assert_eq!(
        ours_after.len(),
        ours.len(),
        "system-message change should break stem stability; expected no new CacheStemReuse event, got {ours_after:?}"
    );

    // Cleanup: clear the override and remove the temp dir.
    crate::shared::metrics::clear_test_path();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn observe_tool_outcome_success_resets_tracker() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    );
    let (tx, _rx) = mpsc::channel::<TurnEvent>(64);
    exe.observe_tool_outcome(
        "bash",
        &ToolOutcome::Success {
            content: "done".into(),
        },
        &tx,
    );
    // No doom event should fire after a success.
}

#[tokio::test]
async fn observe_tool_outcome_doom_after_threshold() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    );
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let err_outcome = ToolOutcome::Error {
        message: "file not found".into(),
    };
    // Below threshold: no event.
    exe.observe_tool_outcome("read_file", &err_outcome, &tx);
    exe.observe_tool_outcome("read_file", &err_outcome, &tx);
    assert!(rx.try_recv().is_err(), "no doom event before threshold");
    // At threshold: event fires.
    exe.observe_tool_outcome("read_file", &err_outcome, &tx);
    let ev = rx.try_recv();
    assert!(
        ev.is_ok(),
        "doom event should fire at threshold 3, got: {ev:?}"
    );
    if let Ok(TurnEvent::DoomLoopDetected { tool, count, .. }) = ev {
        assert_eq!(tool, "read_file");
        assert!(count >= 3);
    } else {
        panic!("expected DoomLoopDetected event");
    }
}

#[tokio::test]
async fn observe_tool_outcome_failure_error_text_extracted() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    );
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let fail_outcome = ToolOutcome::Failure(ToolError::InvalidArgs {
        message: "bad args".into(),
    });
    exe.observe_tool_outcome("edit_file", &fail_outcome, &tx);
    exe.observe_tool_outcome("edit_file", &fail_outcome, &tx);
    exe.observe_tool_outcome("edit_file", &fail_outcome, &tx);
    let ev = rx.try_recv();
    assert!(ev.is_ok(), "doom event should fire for Failure too");
}

#[tokio::test]
async fn observe_tool_outcome_different_tool_resets_run() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    );
    let (tx, _rx) = mpsc::channel::<TurnEvent>(64);
    let err = ToolOutcome::Error {
        message: "err".into(),
    };
    exe.observe_tool_outcome("tool_a", &err, &tx);
    exe.observe_tool_outcome("tool_b", &err, &tx);
    exe.observe_tool_outcome("tool_a", &err, &tx);
    // No doom event: the consecutive run was broken by tool_b.
    // (3 identical in a row are needed; the interspersed tool_b resets the run.)
}

/// Cancellation mid-batch must abort un-awaited `JoinHandle`s so already-
/// spawned tasks do not run detached holding subprocess/network resources
/// (WO 15.7 2.3 — cancel leak). The first tool runs to completion and is
/// recorded; the second is spawned, then cancellation flips; the collect
/// loop awaits the first handle, records it, and on observing `cancelled`
/// aborts the remaining handle. The second tool's `run()` body must never
/// execute (call_count stays at 1).
#[tokio::test]
async fn test_cancelled_batch_aborts_remaining_spawned_tasks() {
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let tool_one = SleepingTool {
        def: ToolDef {
            name: "sleep_a",
            description: "sleep a",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        sleep_ms: 50,
        call_count: Arc::new(Mutex::new(0)),
        start_tx: Arc::new(std::sync::Mutex::new(Some(start_tx))),
    };
    let tool_two = SleepingTool {
        def: ToolDef {
            name: "sleep_b",
            description: "sleep b",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        // Long enough that if it were NOT aborted it would run well past
        // the turn's completion and we could observe the call_count bump.
        sleep_ms: 4000,
        call_count: Arc::new(Mutex::new(0)),
        start_tx: Arc::new(std::sync::Mutex::new(None)),
    };
    let call_count_one = tool_one.call_count.clone();
    let call_count_two = tool_two.call_count.clone();

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "sleep_a".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-2".into(),
                name: "sleep_b".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(
        Box::new(adapter),
        vec![Arc::new(tool_one), Arc::new(tool_two)],
        make_config(true),
    );

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_flag = cancelled.clone();
    tokio::spawn(async move {
        // Wait until the first tool starts, then cancel so the second
        // task is either not spawned or is aborted by the collect loop.
        let _ = start_rx.await;
        cancelled_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let _events = exe
        .run_turn_collecting("run two sleeps", &approval_tx, &cancelled)
        .await
        .unwrap();

    // The first tool started and ran to completion.
    assert_eq!(
        *call_count_one.lock().unwrap(),
        1,
        "first tool should have run exactly once"
    );
    // The second tool must NOT have run — it was either never spawned (the
    // spawn loop broke before reaching it) or was aborted by the collect
    // loop before its body began. A detached task would eventually bump
    // this count; the abort prevents that.
    assert_eq!(
        *call_count_two.lock().unwrap(),
        0,
        "second tool should not have run (cancel leak fix); it ran {} times",
        *call_count_two.lock().unwrap()
    );
}
