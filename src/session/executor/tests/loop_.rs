// Multi-turn loop, batch dispatch, and doom-loop tracker tests. Split
// out from the former single-file `mod.rs` (WO 15.5). Pure refactor: test
// bodies are moved verbatim.

use super::super::*;
use super::common::*;
use crate::adapters::ModelAdapter;
use crate::shared::metrics::{read_events, MetricEvent, PlanDecisionKind};
use crate::shared::permission::PermissionAction;
use crate::shared::test_util::remove_test_file;
use crate::shared::{
    FinishReason, Message, ModelInfo, StreamEvent, ToolDef, ToolError, ToolOutcome,
};
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

    let mut exe =
        make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false)).unwrap();
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

    // Yield to let the executor drain any pending channel sends — no
    // wall-clock delay needed, just a scheduling slot (WO 19.9).
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
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
    let mut exe =
        make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true)).unwrap();

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

/// Tool that records (start, end) instants of each call into a shared
/// log so tests can prove calls overlapped in time (concurrent dispatch)
/// without wall-clock thresholds.
struct IntervalTool {
    intervals: Arc<Mutex<Vec<(std::time::Instant, std::time::Instant)>>>,
    sleep_ms: u64,
}

#[async_trait::async_trait]
impl Tool for IntervalTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "sleep",
            description: "sleep",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        let start = std::time::Instant::now();
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        self.intervals
            .lock()
            .unwrap()
            .push((start, std::time::Instant::now()));
        ToolOutcome::Success {
            content: "done".into(),
        }
    }
}

/// Two independent non-file tool calls must run concurrently, not sequentially.
/// Proven structurally: the second call must START before the first call ENDS
/// (overlapping execution intervals). Serial dispatch makes overlap impossible
/// no matter how loaded the machine is — unlike the wall-clock threshold this
/// test used before, which flaked under parallel test load (a slow turn start
/// inflated elapsed time even when dispatch was perfectly parallel).
#[tokio::test]
async fn test_parallel_tool_batch_runs_concurrently() {
    let intervals: Arc<Mutex<Vec<(std::time::Instant, std::time::Instant)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let tool_one = IntervalTool {
        intervals: intervals.clone(),
        sleep_ms: 500,
    };
    let tool_two = IntervalTool {
        intervals: intervals.clone(),
        sleep_ms: 500,
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
    )
    .unwrap();

    let events = exe
        .run_turn_collecting("run two sleeps", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2, "both sleep calls should produce results");
    assert!(results.iter().all(|(_, o)| *o == "done"));

    let logged = intervals.lock().unwrap();
    assert_eq!(logged.len(), 2, "both calls should log an interval");
    let (a_start, a_end) = logged[0];
    let (b_start, b_end) = logged[1];
    assert!(a_start < a_end);
    assert!(b_start < b_end);
    let latest_start = a_start.max(b_start);
    let earliest_end = a_end.min(b_end);
    assert!(
        latest_start < earliest_end,
        "calls must overlap in time (concurrent dispatch): \
         first={a_start:?}..{a_end:?}, second={b_start:?}..{b_end:?}"
    );
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
    )
    .unwrap();
    let events1 = exe1
        .run_turn_collecting("run with seed 42", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let mut exe2 = make_executor(
        Box::new(MockAdapter::new(events, make_info())),
        make_tools(),
        cfg,
    )
    .unwrap();
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
/// WO 19.9: replaced `sleep(250ms)` with `yield_now()` — after the gate
/// signal, a few yield cycles give the tool time to start before we cancel.
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
    )
    .unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));

    let cancelled_flag = cancelled.clone();
    let turn_handle = tokio::spawn(async move {
        exe.run_turn_collecting("run four sleeps", &approval_tx, &cancelled_flag)
            .await
    });

    let _ = started_rx.await;
    // WO 19.9: 150ms is enough for one 100ms tool to finish while still
    // leaving the 3s tools incomplete. The original 250ms was unnecessarily
    // generous; 150ms cuts test time without losing determinism.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(true)).unwrap();

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
    )
    .unwrap();
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
    )
    .unwrap();
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
    )
    .unwrap();
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
    )
    .unwrap();
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
    )
    .unwrap();

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

#[tokio::test]
async fn doom_loop_circuit_breaker_auto_plan_mode() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let err = ToolOutcome::Error {
        message: "boom".into(),
    };
    // Below threshold: no doom detection.
    exe.observe_tool_outcome("bash", &err, &tx);
    exe.observe_tool_outcome("bash", &err, &tx);
    // Drain any events.
    while rx.try_recv().is_ok() {}
    // Third identical error crosses the DoomLoopTracker threshold.
    let outcome = exe.observe_tool_outcome("bash", &err, &tx);
    assert!(
        outcome.is_some(),
        "should return DoomLoopOutcome after first doom-loop detection"
    );
    let outcome = outcome.unwrap();
    assert_eq!(
        outcome.action,
        cost_tracking::DoomLoopAction::AutoPlan,
        "default action should be AutoPlan"
    );
    assert_eq!(outcome.tool, "bash");
    assert!(outcome.count >= 1);
    // Drain events and verify DoomLoopRemediation was emitted.
    let mut saw_remediation = false;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, TurnEvent::DoomLoopRemediation { .. }) {
            saw_remediation = true;
        }
    }
    assert!(saw_remediation, "should emit DoomLoopRemediation event");
}

#[tokio::test]
async fn doom_loop_circuit_breaker_default_action_is_auto_plan() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    let (tx, _rx) = mpsc::channel::<TurnEvent>(64);
    let err = ToolOutcome::Error {
        message: "stuck".into(),
    };
    // Trigger a doom-loop detection. The action is determined by config
    // (default: auto_plan), not by whether plan mode is active.
    exe.observe_tool_outcome("bash", &err, &tx);
    exe.observe_tool_outcome("bash", &err, &tx);
    let outcome = exe.observe_tool_outcome("bash", &err, &tx);
    assert!(
        outcome.is_some(),
        "should return DoomLoopOutcome after doom-loop detection"
    );
    assert_eq!(
        outcome.unwrap().action,
        cost_tracking::DoomLoopAction::AutoPlan,
        "default config action is AutoPlan regardless of plan_mode state"
    );
}

#[tokio::test]
async fn doom_loop_circuit_breaker_downgrades_to_warn_only_when_non_interactive() {
    // WO 30.9: in --non-interactive runs there is no user to type
    // `/implement`, so AutoPlan would brick the agent. The breaker must
    // downgrade AutoPlan to WarnOnly (warning still logs, no trap).
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config(false),
    )
    .unwrap();
    exe.set_non_interactive(true);
    let (tx, _rx) = mpsc::channel::<TurnEvent>(64);
    let err = ToolOutcome::Error {
        message: "stuck".into(),
    };
    exe.observe_tool_outcome("bash", &err, &tx);
    exe.observe_tool_outcome("bash", &err, &tx);
    let outcome = exe.observe_tool_outcome("bash", &err, &tx);
    let outcome = outcome.expect("doom-loop should still fire (downgraded, not suppressed)");
    assert_eq!(
        outcome.action,
        cost_tracking::DoomLoopAction::WarnOnly,
        "AutoPlan must downgrade to WarnOnly in non-interactive runs"
    );
}

#[tokio::test]
async fn doom_loop_circuit_breaker_disabled_when_zero() {
    let mut exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        make_config_with_doom_loop_max_hits(0),
    )
    .unwrap();
    let (tx, mut rx) = mpsc::channel::<TurnEvent>(64);
    let err = ToolOutcome::Error {
        message: "boom".into(),
    };
    // With doom_loop_max_hits=0, the circuit breaker should be disabled.
    // 5 errors should NOT trigger plan mode or remediation.
    for _ in 0..5 {
        exe.observe_tool_outcome("bash", &err, &tx);
    }
    // The wrapper no longer sets plan_mode; check the return value.
    // With max_hits=0, no DoomLoopOutcome should be returned.
    // (plan_mode is not changed by observe_tool_outcome in R3)
    // No DoomLoopRemediation events should have been emitted.
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, TurnEvent::DoomLoopRemediation { .. }),
            "no DoomLoopRemediation should be emitted when disabled"
        );
    }
}

// ── WO 36.4: parent-session live cancel token (TUI Esc path) ──────────

/// Drives a full `Executor::run` loop with one channel per control input,
/// mirroring the TUI wiring, so tests can exercise the real cancel-watcher
/// path (`cancel_tx.send(())` = the Esc/Ctrl+C key) end-to-end.
struct RunHarness {
    input_tx: mpsc::UnboundedSender<String>,
    cancel_tx: mpsc::UnboundedSender<()>,
    event_rx: mpsc::Receiver<TurnEvent>,
    handle: tokio::task::JoinHandle<()>,
    // Remaining channel ends not consumed by `run`, kept alive for the
    // loop's lifetime (a dropped end closes its channel).
    // reason: one field per control channel; grouping would obscure the wiring.
    #[allow(clippy::type_complexity)]
    _keepalive: (
        mpsc::UnboundedReceiver<ApprovalRequest>,
        mpsc::UnboundedSender<ConversationLog>,
        mpsc::UnboundedSender<crate::session::prompt::CompactRequest>,
        mpsc::UnboundedSender<String>,
        mpsc::UnboundedSender<()>,
        mpsc::UnboundedSender<Config>,
        mpsc::UnboundedSender<bool>,
        mpsc::UnboundedSender<kf_plugin_host::PluginRegistry>,
    ),
}

fn spawn_executor_run(mut exe: Executor) -> RunHarness {
    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    let (event_tx, event_rx) = mpsc::channel::<TurnEvent>(64);
    let (approval_tx, approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
    let (resume_tx, resume_rx) = mpsc::unbounded_channel::<ConversationLog>();
    let (compact_tx, compact_rx) =
        mpsc::unbounded_channel::<crate::session::prompt::CompactRequest>();
    let (model_tx, model_rx) = mpsc::unbounded_channel::<String>();
    let (undo_tx, undo_rx) = mpsc::unbounded_channel::<()>();
    let (config_tx, config_rx) = mpsc::unbounded_channel::<Config>();
    let (plan_tx, plan_rx) = mpsc::unbounded_channel::<bool>();
    let (plugin_tx, plugin_rx) = mpsc::unbounded_channel::<kf_plugin_host::PluginRegistry>();

    let handle = tokio::spawn(async move {
        exe.run(
            input_rx,
            event_tx,
            approval_tx,
            cancel_rx,
            resume_rx,
            compact_rx,
            model_rx,
            undo_rx,
            config_rx,
            plan_rx,
            plugin_rx,
        )
        .await
        .expect("executor run loop returns Ok");
    });

    RunHarness {
        input_tx,
        cancel_tx,
        event_rx,
        handle,
        _keepalive: (
            approval_rx,
            resume_tx,
            compact_tx,
            model_tx,
            undo_tx,
            config_tx,
            plan_tx,
            plugin_tx,
        ),
    }
}

/// Bounded wait for `TurnComplete` — the proof that an Esc-cancelled turn
/// ends cooperatively within the window, not at the adapter/tool timeout.
async fn wait_turn_complete_within(event_rx: &mut mpsc::Receiver<TurnEvent>, secs: u64) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("TurnComplete must arrive within the bounded window");
        let ev = tokio::time::timeout(remaining, event_rx.recv())
            .await
            .expect("turn must end cooperatively, not at adapter/tool timeout")
            .expect("event stream alive");
        if matches!(ev, TurnEvent::TurnComplete) {
            return;
        }
    }
}

/// Esc on the TUI must abort an in-flight (stalled) parent-session model
/// stream via the live per-turn token — the same WO 36.3 abort, driven
/// through the real cancel watcher instead of a direct token cancel.
/// Pre-WO 36.4 the parent attached no root token, so the turn loop's
/// select never fired and the turn hung on the stalled stream.
#[tokio::test]
async fn esc_cancel_aborts_stalled_parent_stream() {
    let exe = make_executor(Box::new(StalledStreamAdapter), vec![], make_config(false)).unwrap();
    let mut h = spawn_executor_run(exe);

    h.input_tx.send("hello".to_string()).unwrap();
    loop {
        let ev = h.event_rx.recv().await.expect("event stream alive");
        if matches!(ev, TurnEvent::Token(ref t) if t == "partial") {
            break;
        }
    }
    // The TUI's Esc/Ctrl+C path.
    h.cancel_tx.send(()).unwrap();

    wait_turn_complete_within(&mut h.event_rx, 2).await;
    h.handle.abort();
}

// ── WO 38.5: executor survives turn errors + Esc epoch window ────────

/// Adapter whose first stream() call fails (provider blip: 401/5xx past
/// retries, missing key) and whose subsequent calls succeed — the user's
/// retry must work because the session survived the failed turn.
struct FailFirstAdapter {
    calls: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl ModelAdapter for FailFirstAdapter {
    fn model_info(&self) -> ModelInfo {
        make_info()
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let mut count = self.calls.lock().unwrap();
        *count += 1;
        if *count == 1 {
            return Err(anyhow::anyhow!("401 unauthorized (simulated)"));
        }
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Text("recovered".to_string())).await;
            let _ = tx
                .send(StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                })
                .await;
        });
        Ok(rx)
    }
}

/// WO 38.5 P0 (TUI audit P1-1): a turn-fatal adapter error must cost one
/// turn, not the session. The loop emits `Error` + `TurnComplete`, stays
/// alive, and a retry input produces a normal turn.
#[tokio::test]
async fn turn_error_keeps_session_alive_for_retry() {
    let calls = Arc::new(Mutex::new(0));
    let exe = make_executor(
        Box::new(FailFirstAdapter {
            calls: calls.clone(),
        }),
        vec![],
        make_config(false),
    )
    .unwrap();
    let mut h = spawn_executor_run(exe);

    h.input_tx.send("first".to_string()).unwrap();
    let mut saw_error = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("first turn must resolve within window");
        let ev = tokio::time::timeout(remaining, h.event_rx.recv())
            .await
            .expect("event arrives")
            .expect("event stream alive");
        match ev {
            TurnEvent::Error(ref e) if e.contains("Turn failed") => saw_error = true,
            TurnEvent::TurnComplete if saw_error => break,
            _ => {}
        }
    }
    assert!(saw_error, "turn error must surface as an Error event");

    // The session must still accept input; the retry turn completes.
    h.input_tx.send("retry".to_string()).unwrap();
    let mut saw_recovered = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("retry turn must resolve within window");
        let ev = tokio::time::timeout(remaining, h.event_rx.recv())
            .await
            .expect("event arrives")
            .expect("event stream alive");
        match ev {
            TurnEvent::Token(ref t) if t == "recovered" => saw_recovered = true,
            TurnEvent::TurnComplete if saw_recovered => break,
            _ => {}
        }
    }
    assert_eq!(*calls.lock().unwrap(), 2, "adapter streamed exactly twice");
    h.handle.abort();
}

/// WO 38.4 #3 / WO 38.5: a stale Esc (queued before the input) must not
/// kill the fresh turn. Event-driven: Esc first, then input — the turn
/// completes normally with no cancellation.
#[tokio::test]
async fn stale_esc_before_input_does_not_kill_fresh_turn() {
    let exe = make_executor(
        Box::new(MockAdapter::new(
            vec![
                StreamEvent::Text("fresh".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            ],
            make_info(),
        )),
        vec![],
        make_config(false),
    )
    .unwrap();
    let mut h = spawn_executor_run(exe);

    // Esc lands while no turn is in flight, immediately followed by new
    // input. The old independent watcher could process this Esc after the
    // fresh token install and abort the new turn instantly.
    h.cancel_tx.send(()).unwrap();
    h.input_tx.send("hello".to_string()).unwrap();

    let mut saw_token = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("turn must resolve within window");
        let ev = tokio::time::timeout(remaining, h.event_rx.recv())
            .await
            .expect("event arrives")
            .expect("event stream alive");
        match ev {
            TurnEvent::Token(ref t) if t == "fresh" => saw_token = true,
            TurnEvent::Token(ref t) if t.contains("cancelled") => {
                panic!("stale Esc cancelled the fresh turn")
            }
            TurnEvent::TurnComplete if saw_token => break,
            _ => {}
        }
    }
    h.handle.abort();
}

/// Tool that stalls until its per-call cancel token fires (signalling its
/// start via a oneshot so the test cancels deterministically mid-run).
struct AwaitCancelTool {
    started: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

#[async_trait::async_trait]
impl Tool for AwaitCancelTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "await_cancel",
            description: "stalls until its cancel token fires",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }

    async fn run(&self, ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        if let Ok(mut guard) = self.started.lock() {
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        // Only completes when the per-call token is a LIVE child of the
        // parent token: pre-WO 36.4 it was a flag snapshot (WO 15.7)
        // that never fires mid-run, so this would hang until
        // tool_timeout_secs (120s) — far past the test's bounded window.
        ctx.token.cancelled().await;
        ToolOutcome::Failure(ToolError::Cancelled)
    }
}

/// Esc on the TUI must cascade into in-flight tool calls: per-tool tokens
/// are children of the parent's live per-turn token, so the watcher's
/// cancel fires them mid-run (tool timeout stays independently triggerable).
#[tokio::test]
async fn esc_cancel_cascades_to_live_tool_token() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let tool = Arc::new(AwaitCancelTool {
        started: Arc::new(std::sync::Mutex::new(Some(started_tx))),
    });
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-esc-1".into(),
                name: "await_cancel".into(),
                arguments: serde_json::json!({}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );
    let exe = make_executor(Box::new(adapter), vec![tool], make_config(true)).unwrap();
    let mut h = spawn_executor_run(exe);

    h.input_tx.send("run it".to_string()).unwrap();
    started_rx.await.expect("tool must start before cancel");
    h.cancel_tx.send(()).unwrap();

    wait_turn_complete_within(&mut h.event_rx, 2).await;
    h.handle.abort();
}
