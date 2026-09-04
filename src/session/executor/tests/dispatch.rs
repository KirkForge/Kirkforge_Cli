// Tool-call dispatching tests. Split out from the former single-file
// `mod.rs` (WO 15.5). Pure refactor: test bodies are moved verbatim.

use super::super::*;
use super::common::*;
use crate::shared::metrics::{read_events, MetricEvent, PlanDecisionKind};
// Only used by `#[cfg(unix)]` tests below; gate the import so the
// Windows build (deny(warnings)) doesn't flag it unused.
#[cfg(unix)]
use crate::shared::test_util::remove_test_file;
use crate::shared::{FinishReason, ModelInfo, StreamEvent, TokenUsage, ToolDef, ToolOutcome};
use crate::tools::{Tool, ToolContext};
use std::sync::Mutex;
#[tokio::test]
async fn test_basic_text_response() {
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("Hello ".to_string()),
            StreamEvent::Text("world!".to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: Some(TokenUsage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    cache_write_tokens: None,
                }),
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();
    let events = exe
        .run_turn_collecting("hello", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let tokens: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tokens, vec!["Hello ", "world!"]);

    let msgs = exe.conversation.all();
    assert_eq!(msgs.len(), 2); // user + assistant
    assert_eq!(msgs[0].role, Role::User);
    assert_eq!(msgs[0].content, "hello");
    assert_eq!(msgs[1].role, Role::Assistant);
    assert_eq!(msgs[1].content, "Hello world!");
    assert_eq!(msgs[1].token_count, Some(5));
}

#[tokio::test]
async fn test_tool_call_dispatch() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "echo",
            description: "echo a value",
            parameters: serde_json::json!({"type": "object", "properties": {"val": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "echoed!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("Calling tool...".to_string()),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"val": "test"}),
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
    let events = exe
        .run_turn_collecting("use echo", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let has_token = events.iter().any(|e| matches!(e, TurnEvent::Token(_)));
    let has_start = events
        .iter()
        .any(|e| matches!(e, TurnEvent::ToolStart { name, .. } if name == "echo"));
    let has_result = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "echo" && output == "echoed!"));

    assert!(has_token, "Should stream text before tool call");
    assert!(has_start, "Should emit ToolStart");
    assert!(has_result, "Should emit ToolResult");

    let called_with = captured.lock().unwrap().take();
    assert!(called_with.is_some(), "Tool should have been called");
    assert_eq!(
        called_with.unwrap().get("val").and_then(|v| v.as_str()),
        Some("test")
    );

    let msgs = exe.conversation.all();
    let tool_msgs: Vec<_> = msgs.iter().filter(|m| m.role == Role::Tool).collect();
    assert_eq!(tool_msgs.len(), 1);
    assert_eq!(tool_msgs[0].content, "echoed!");
}

/// WO 48.31: a parallel same-name batch stamps each event with its own
/// model-assigned call id — ToolStart/ToolResult pairs are exact, so
/// downstream consumers (TUI cards, replay traces) never confuse the
/// two calls.
#[tokio::test]
async fn parallel_same_name_calls_stamp_their_own_call_ids() {
    let tool = MockTool {
        def: ToolDef {
            name: "echo",
            description: "echo a value",
            parameters: serde_json::json!({"type": "object", "properties": {"val": {"type": "string"}}}),
        },
        captured_args: Arc::new(Mutex::new(None)),
        outcome: ToolOutcome::Success {
            content: "echoed!".into(),
        },
    };
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-a".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"val": "a"}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-b".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"val": "b"}),
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
    let events = exe
        .run_turn_collecting("use echo twice", &approval_tx, never_cancelled())
        .await
        .unwrap();

    for expected_id in ["call-a", "call-b"] {
        assert!(
            events.iter().any(|e| matches!(e, TurnEvent::ToolStart { call_id, name, .. } if call_id == expected_id && name == "echo")),
            "ToolStart must carry the model id {expected_id}"
        );
        assert!(
            events.iter().any(|e| matches!(e, TurnEvent::ToolResult { call_id, name, .. } if call_id == expected_id && name == "echo")),
            "ToolResult must carry the model id {expected_id}"
        );
    }
}

#[tokio::test]
async fn test_error_event_forwarded() {
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("Starting...".to_string()),
            StreamEvent::Error("connection lost".to_string()),
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();
    let events = exe
        .run_turn_collecting("do it", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let has_error = events
        .iter()
        .any(|e| matches!(e, TurnEvent::Error(msg) if msg == "connection lost"));
    assert!(has_error, "Error events should be forwarded");
}

#[tokio::test]
async fn test_unknown_tool_reported_as_error() {
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "nonexistent_tool".into(),
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();
    let events = exe
        .run_turn_collecting("use unknown tool", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let has_error = events
        .iter()
        .any(|e| matches!(e, TurnEvent::Error(msg) if msg.contains("Unknown tool")));
    assert!(has_error, "Unknown tools should produce error events");
}

#[tokio::test]
async fn test_tool_call_loop_capped() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "looper",
            description: "keeps being called",
            parameters: serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "loop again".into(),
        },
    };

    struct LoopAdapter {
        info: ModelInfo,
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait]
    impl ModelAdapter for LoopAdapter {
        fn model_info(&self) -> ModelInfo {
            self.info.clone()
        }

        async fn stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
            let (tx, rx) = mpsc::channel(64);
            let count = *self.call_count.lock().unwrap();
            *self.call_count.lock().unwrap() = count + 1;
            tokio::spawn(async move {
                let _ = tx
                    .send(StreamEvent::ToolCall(ToolInvocation {
                        id: format!("call-{count}"),
                        name: "looper".into(),
                        arguments: serde_json::json!({"x": format!("round-{}", count)}),
                    }))
                    .await;
                let _ = tx
                    .send(StreamEvent::Done {
                        finish_reason: FinishReason::ToolCalls,
                        usage: None,
                    })
                    .await;
            });
            Ok(rx)
        }
    }

    let call_count = Arc::new(Mutex::new(0usize));
    let adapter = LoopAdapter {
        info: make_info(),
        call_count: call_count.clone(),
    };

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.max_tool_calls_per_turn = 5;
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config).unwrap();
    let _events = exe
        .run_turn_collecting("loop", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let tool_calls = *call_count.lock().unwrap();
    assert!(
        tool_calls <= 5,
        "Should not exceed configured max_tool_calls_per_turn (was {tool_calls})"
    );
}

/// WO 47.14: the old test wired a `Verifier`-trait capture verifier into
/// `VerifierSlots` to assert the EditEvent carried the real diff. The
/// `Verifier` trait is deleted; the bus path uses `VerifyContext` which
/// does not carry the diff. The diff-carrying contract is now tested at
/// the `BusEvent::Edit` construction site in dispatch.rs directly.
/// This test is rewritten to assert the bus runs without error on an
/// edit_file tool call.
#[tokio::test]
async fn test_edit_event_bus_runs_without_error() {
    let tool = MockTool {
        def: ToolDef {
            name: "edit_file",
            description: "fake edit",
            parameters: serde_json::json!({"type": "object"}),
        },
        captured_args: Arc::new(Mutex::new(None)),
        outcome: ToolOutcome::FileEdit {
            path: std::path::PathBuf::from("/tmp/edit_event_diff_test.txt"),
            diff: "--- a\n+++ b\n-old line\n+new line".to_string(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": "/tmp/edit_event_diff_test.txt",
                    "old_string": "old line",
                    "new_string": "new line",
                }),
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

    // The read-before-edit gate would otherwise deny the edit.
    exe.sandbox
        .mark_read(&std::path::PathBuf::from("/tmp/edit_event_diff_test.txt"));

    let _events = exe
        .run_turn_collecting("edit it", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // The bus should have been set up by init_default_verifiers.
    assert!(
        exe.verifier_bus.is_some(),
        "verifier_bus must be set up by init_default_verifiers"
    );
}

#[tokio::test]
async fn test_read_image_honours_path_guard_size_limit() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_oversized_image_test_{}.png",
        std::process::id()
    ));
    // Write one byte over the default 1 MiB max_file_read_size.
    let oversized = vec![0xFF; 1024 * 1024 + 1];
    std::fs::write(&tmp, oversized).expect("write oversized image");
    let _cleanup = CleanupFile(tmp.clone());

    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "read_image",
            description: "read image",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Image {
            path: tmp.clone(),
            mime: "image/png".into(),
            data_base64: String::new(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "read_image".into(),
                arguments: serde_json::json!({"path": tmp.to_string_lossy()}),
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
    let events = exe
        .run_turn_collecting("read image", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "oversized read_image must be blocked before reaching the tool"
    );
    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "read_image" && output.contains("too large")
        )
    });
    assert!(
        denied,
        "Expected read_image size-denial, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_max_tool_calls_per_turn_respected() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "echo",
            description: "echo a value",
            parameters: serde_json::json!({"type": "object", "properties": {"val": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "echoed!".into(),
        },
    };

    // The adapter always returns the same tool call, so the executor
    // will loop until it hits the configured cap.
    let tool_call_events = vec![
        StreamEvent::ToolCall(ToolInvocation {
            id: "call-1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"val": "loop"}),
        }),
        StreamEvent::Done {
            finish_reason: FinishReason::ToolCalls,
            usage: None,
        },
    ];
    let adapter = MockAdapter::new(tool_call_events.clone(), make_info())
        .with_followup_events(tool_call_events);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.max_tool_calls_per_turn = 3;
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config).unwrap();
    let events = exe
        .run_turn_collecting("loop", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let tool_results = events
        .iter()
        .filter(|e| matches!(e, TurnEvent::ToolResult { name, .. } if name == "echo"))
        .count();
    assert_eq!(tool_results, 3, "should stop at max_tool_calls_per_turn");

    let hit_limit = events
        .iter()
        .any(|e| matches!(e, TurnEvent::Error(e) if e.contains("Tool call loop limit reached")));
    assert!(
        hit_limit,
        "should emit loop-limit error when cap is reached"
    );
}

/// A `[read_file(X), write_file(X)]` batch in input order must pass the
/// read-before-edit gate because the read is recorded before the write is
/// checked. Uses real files and the real read_file/write_file tool bodies.
#[tokio::test]
async fn test_read_then_write_in_same_batch_passes_read_gate() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_read_then_write_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::{read_file::ReadFile, write_file::WriteFile};

    let read_tool: Arc<dyn Tool> = Arc::new(ReadFile::new(
        crate::session::access::PathGuard::default(),
        false,
        4096,
    ));
    let write_tool: Arc<dyn Tool> = Arc::new(WriteFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": tmp.to_string_lossy()}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-write".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "content": "updated"
                }),
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
        vec![read_tool, write_tool],
        make_config(true),
    )
    .unwrap();

    let events = exe
        .run_turn_collecting("read then write", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let write_success = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, success, .. } if name == "write_file" && *success
        )
    });
    assert!(
        write_success,
        "write_file in same batch should succeed after read_file; got events: {events:?}"
    );
    let content = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(content, "updated", "file should have been overwritten");
}

// WO 48.16: a read whose body FAILED must not satisfy the
// read-before-edit gate for a later write in the same batch.
#[tokio::test]
async fn failed_read_does_not_satisfy_read_before_edit_gate() {
    let tmp = std::env::temp_dir().join(format!("kf_code_failed_read_{}.txt", std::process::id()));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    let read_tool: Arc<dyn Tool> = Arc::new(MockTool {
        def: ToolDef {
            name: "read_file",
            description: "failing read",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        },
        captured_args: Arc::new(Mutex::new(None)),
        outcome: crate::shared::ToolOutcome::Failure(crate::shared::ToolError::Internal {
            message: "simulated I/O failure".into(),
        }),
    });
    let write_tool: Arc<dyn Tool> = Arc::new(crate::tools::write_file::WriteFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": tmp.to_string_lossy()}),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-write".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "content": "updated"
                }),
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
        vec![read_tool, write_tool],
        make_config(true),
    )
    .unwrap();

    let events = exe
        .run_turn_collecting("failed read then write", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let write_denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, success, .. } if name == "write_file" && !*success
        )
    });
    assert!(
        write_denied,
        "write_file after a FAILED read must be denied by the read-before-edit gate; got events: {events:?}"
    );
    let content = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(content, "original", "file must be untouched");
}

#[tokio::test]
async fn test_plan_reason_emitted_after_tool_call() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "echo",
            description: "echo a value",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"val": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "echoed!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Thinking("I need to echo a value.".to_string()),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({"val": "test"}),
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
    exe.run_turn_collecting("use echo", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let events = read_events();
    let found = events.iter().any(|e| {
        matches!(
            e,
            MetricEvent::PlanReason {
                decision_kind: PlanDecisionKind::ToolSelect,
                ref reason,
                related_id: Some(ref id),
                confidence,
            } if reason == "I need to echo a value." && id == "call-1" && *confidence == 1.0
        )
    });
    assert!(
        found,
        "expected PlanReason ToolSelect after tool call; got: {events:?}"
    );
}

// ── WO 9.6: plugin verifier → unified VerifierBus → CorrectionResult ──
//
// Proves the code-level unification of the Rust verifier bus and the
// plugin verifier path (ADR-0028 / ADR-043). A mock plugin declares a
// `security` verifier; the executor's `emit_tool_event_and_correct`
// must run it through the unified `VerifierBus` and convert the
// `Severity::Error` verdict into a `CorrectionResult` — the same struct
// the correction loop emits — so a single correction path handles
// built-in and plugin verdicts.
#[cfg(unix)]
#[tokio::test]
async fn plugin_verifier_triggers_correction_result() {
    use kf_plugin_host::{PluginRegistry, TrustPolicy};
    use std::os::unix::fs::PermissionsExt;

    // 1. Build a mock plugin whose `security` verifier fails with a
    //    recognizable message on stderr and exits 1.
    let tmp = tempfile::tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins");
    let plugin_dir = plugins_dir.join("sec-plugin");
    let bin_dir = plugin_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let check = bin_dir.join("check.sh");
    std::fs::write(
        &check,
        "#!/bin/sh\necho 'plugin-security: dangerous pattern' >&2\nexit 1\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&check).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&check, perms).unwrap();

    std::fs::write(
        plugin_dir.join("kf-code.toml"),
        r#"
name = "sec-plugin"
version = "0.1.0"
description = "mock security verifier"
trust = "shell"

[[capabilities]]
type = "verifier"
name = "security"
priority = 1
command = "bin/check.sh"
"#,
    )
    .unwrap();

    let mut registry = PluginRegistry::new();
    let warnings = registry
        .load_from_dir(
            &plugins_dir,
            TrustPolicy::up_to(kf_plugin_host::sdk::TrustTier::Shell),
        )
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(registry.active_count(), 1);

    // 2. Construct an Executor with the plugin registry so
    //    `init_default_verifiers` wires the plugin verifier into the
    //    unified `VerifierBus` via `register_plugin_verifiers_into_bus`.
    let adapter = MockAdapter::new(vec![], make_info());
    let log_path = std::env::temp_dir().join(format!(
        "kf-plugin-sdk-verifier-test-{}.ndjson",
        std::process::id()
    ));
    remove_test_file(&log_path);
    let (conversation, _outcome) = ConversationLog::open(log_path.clone()).unwrap();
    let mut composite = crate::session::toolset::CompositeToolset::empty();
    composite.add(Box::new(crate::session::toolset::VecToolset::new(
        "test",
        vec![],
    )));
    let exe = Executor::with_log_and_undo_and_plugins(
        Box::new(adapter),
        composite,
        Arc::new(std::sync::RwLock::new(make_config(true))),
        conversation,
        None,
        None,
        Some(&registry),
    )
    .expect("executor construction");

    // Sanity: the bus registered the plugin verifier.
    {
        let bus_lock = exe.verifier_bus.as_ref().expect("verifier_bus set");
        let bus = bus_lock.lock().unwrap();
        assert!(
            bus.verifier_count() >= 1,
            "plugin verifier should be registered on the bus"
        );
    }

    // 3. Drive the seam: a `write_file` tool call must run the bus and
    //    convert any `Severity::Error` verdict into a `CorrectionResult`.
    let tc = ToolInvocation {
        id: "call-1".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({
            "path": "src/secret.rs",
            "content": "fn main() {}",
        }),
    };
    let outcome = ToolOutcome::Success {
        content: "wrote 1 file".into(),
    };
    let corrections = exe
        .emit_tool_event_and_correct(
            &tc,
            "write_file",
            &tc.arguments,
            &outcome,
            None,
            None,
            None,
            None,
        )
        .await;

    // 4. Assert the plugin verifier's verdict surfaced as a
    //    `CorrectionResult` sourced from `plugin:security` with a Failed
    //    outcome (Severity::Error → Failed; WO 45.36).
    let plugin_correction = corrections.iter().find(|c| {
        c.verifier == "plugin:security"
            && c.outcome == crate::session::executor::types::VerificationOutcome::Failed
            && c.fix.is_none()
    });
    assert!(
        plugin_correction.is_some(),
        "expected a CorrectionResult from plugin:security, got: {corrections:?}"
    );
    let c = plugin_correction.unwrap();
    assert!(
        c.message.contains("plugin-security: dangerous pattern"),
        "CorrectionResult message should carry the plugin verifier's stderr: {}",
        c.message
    );

    remove_test_file(&log_path);
}

/// A deferred file call denied by the read-before-edit gate (Phase 2.5)
/// must produce exactly one "Access denied" tool result — not two. Before
/// WO 15.7 2.8, Phase 3 re-ran `record_tool_result`, which re-checked the
/// path guard + read gate and emitted a second, identical denial message,
/// so the model saw two "Access denied" results for one failed edit.
#[tokio::test]
async fn test_denied_edit_records_single_access_denied_result() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_denied_edit_single_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::edit_file::EditFile;
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "old_string": "original",
                    "new_string": "updated",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![edit_tool], make_config(true)).unwrap();
    // Deliberately do NOT mark_read — the edit must be denied by the
    // read-before-edit gate in Phase 2.5.

    let events = exe
        .run_turn_collecting("edit unread file", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied_results: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, success: false, .. }
                    if name == "edit_file" && output.contains("Access denied")
            )
        })
        .collect();
    assert_eq!(
        denied_results.len(),
        1,
        "denied edit should produce exactly one Access denied ToolResult, got {denied_results:?}; \
         events: {events:?}"
    );

    let msgs = exe.conversation.all();
    let denial_msgs: Vec<_> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("Access denied"))
        .collect();
    assert_eq!(
        denial_msgs.len(),
        1,
        "conversation should contain exactly one Access denied message, got {denial_msgs:?}"
    );
}

// WO 43.30: a pre-tool hook returning `deny` (exit 2) for a file tool
// must block the spawn BEFORE the mutation is applied — the file on
// disk must be unchanged. Pre-fix, file tools short-circuited to Spawn
// in pre_run before the hook block, so the hook denial fired after the
// write had already landed (silent security bypass).
#[cfg(unix)]
#[tokio::test]
async fn test_pre_tool_hook_deny_blocks_edit_file_before_mutation() {
    let tmp =
        std::env::temp_dir().join(format!("kf_code_hook_veto_edit_{}.txt", std::process::id()));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::edit_file::EditFile;
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "old_string": "original",
                    "new_string": "MUTATED",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (_tmp, hooks_dir) = temp_hooks_dir();
    // Exit 2 => HookDecision::Deny (see run_pre_tool_hook / hook_runner).
    std::fs::write(
        hooks_dir.join("pre-tool-edit_file.sh"),
        "#!/bin/bash\nexit 2",
    )
    .unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![edit_tool], config).unwrap();
    // Mark read so the read-before-edit gate passes — the hook must be
    // the only thing that blocks the edit.
    exe.sandbox.mark_read(&tmp);

    let events = exe
        .run_turn_collecting("edit with hook deny", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // The file on disk MUST be unchanged — the hook denied before spawn.
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(
        on_disk, "original",
        "pre-tool hook deny must block the edit before mutation; \
         disk now contains {on_disk:?}"
    );

    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false, .. }
                if name == "edit_file" && output.contains("denied")
        )
    });
    assert!(
        denied,
        "Expected a hook-denial ToolResult for edit_file, got events: {events:?}"
    );
}

// WO 48.2: exactly ONE pre-tool-{name} evaluation per file-tool call.
// Pre-fix, Phase 3 (record_tool_result) re-ran the hook after the body had
// already mutated disk — doubled hook side-effects on every file call, and
// a divergent second verdict could deny recording a write that already
// happened (the WO 43.30 contract violation surviving in the second-run
// window). The hook appends one byte per invocation; the counter file must
// contain exactly one byte after a successful edit.
#[cfg(unix)]
#[tokio::test]
async fn pre_tool_hook_runs_exactly_once_per_file_tool_call() {
    let tmp =
        std::env::temp_dir().join(format!("kf_code_hook_once_edit_{}.txt", std::process::id()));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::edit_file::EditFile;
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "old_string": "original",
                    "new_string": "updated",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (hook_tmp, hooks_dir) = temp_hooks_dir();
    let counter = hook_tmp.path().join("invocations");
    std::fs::write(&counter, b"").unwrap();
    let script = format!("#!/bin/bash\nprintf x >> {counter:?}\nexit 0\n");
    std::fs::write(hooks_dir.join("pre-tool-edit_file.sh"), script).unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![edit_tool], config).unwrap();
    // Mark read so the read-before-edit gate passes — the edit must apply.
    exe.sandbox.mark_read(&tmp);

    exe.run_turn_collecting("edit counts one hook run", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let invocations = std::fs::read_to_string(&counter).unwrap().len();
    assert_eq!(
        invocations, 1,
        "pre-tool hook must run exactly once per file-tool call, ran {invocations} times"
    );

    // The edit itself still applied (exit 0 => allow).
    let on_disk = std::fs::read_to_string(&tmp).unwrap();
    assert_eq!(on_disk, "updated");
}

// WO 48.15: a BODY-produced AccessDenied (the tool ran, its own guard
// denied — here write_file's extension deny list, same shape as the TOCTOU
// re-check) must flow through `record_tool_result` like any failure:
// post-tool hooks, metrics, and the doom-loop breaker all live there.
// Pre-fix, collect_batch's early-record branch pattern-matched it as a
// gate denial and skipped the recording pipeline entirely.
#[cfg(unix)]
#[tokio::test]
async fn body_denied_write_file_still_runs_post_tool_hook() {
    let tmp = std::env::temp_dir().join(format!("kf_code_body_denied_{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::write_file::WriteFile;
    // The TOOL's own guard denies .txt; the executor sandbox (default
    // config, which does NOT deny .txt) allows — Phase 1 spawns, Phase 2.5
    // gates pass (new file), the body denies. Deterministic stand-in for
    // the guard-state-change window the TOCTOU re-checks cover.
    let guard = crate::session::access::PathGuard {
        deny_extensions: vec![".txt".to_string()],
        deny_list: crate::shared::access::DenyList::new(vec![], vec![]),
        ..Default::default()
    };
    let write_tool: Arc<dyn Tool> = Arc::new(WriteFile::new(None, guard, false, false));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-write".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "content": "body denial probe",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (hook_tmp, hooks_dir) = temp_hooks_dir();
    let counter = hook_tmp.path().join("post_tool_runs");
    std::fs::write(&counter, b"").unwrap();
    let script = format!("#!/bin/bash\nprintf x >> {counter:?}\nexit 0\n");
    std::fs::write(hooks_dir.join("post-tool-write_file.sh"), script).unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![write_tool], config).unwrap();

    let events = exe
        .run_turn_collecting("write denied key", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // The denial itself is recorded exactly once.
    let denied_results: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, success: false, .. }
                    if name == "write_file" && output.contains("denied")
            )
        })
        .collect();
    assert_eq!(
        denied_results.len(),
        1,
        "body-denied write should produce exactly one denied ToolResult, got {denied_results:?}; \
         events: {events:?}"
    );
    let msgs = exe.conversation.all();
    let denial_msgs: Vec<_> = msgs
        .iter()
        .filter(|m| m.role == Role::Tool && m.content.contains("denied"))
        .collect();
    assert_eq!(
        denial_msgs.len(),
        1,
        "conversation should contain exactly one denial message, got {denial_msgs:?}"
    );

    // The post-tool hook FIRED — the body denial went through
    // record_tool_result, not the gate-denial early-record path. Hook
    // scripts are spawned fire-and-forget, so poll briefly for the marker.
    let mut hook_runs = 0;
    for _ in 0..250 {
        hook_runs = std::fs::read_to_string(&counter).unwrap().len();
        if hook_runs == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        hook_runs, 1,
        "post-tool-write_file hook must run once for a body-denied write (record_tool_result \
         path), ran {hook_runs} times"
    );

    assert!(!tmp.exists(), "denied write must not create the file");
}

// WO 48.15 companion: a GATE denial (read-before-edit gate, decided before
// the body ran) keeps the early-record fast path — the tool never executed,
// so the post-tool hook must NOT fire.
#[cfg(unix)]
#[tokio::test]
async fn gate_denied_edit_file_skips_post_tool_hook() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_gate_denied_edit_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::edit_file::EditFile;
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": tmp.to_string_lossy(),
                    "old_string": "original",
                    "new_string": "updated",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (hook_tmp, hooks_dir) = temp_hooks_dir();
    let counter = hook_tmp.path().join("post_tool_runs");
    std::fs::write(&counter, b"").unwrap();
    let script = format!("#!/bin/bash\nprintf x >> {counter:?}\nexit 0\n");
    std::fs::write(hooks_dir.join("post-tool-edit_file.sh"), script).unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![edit_tool], config).unwrap();
    // Deliberately do NOT mark_read — the edit must be denied by the
    // read-before-edit gate in Phase 2.5.

    let events = exe
        .run_turn_collecting("edit unread file", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied_results: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                TurnEvent::ToolResult { name, output, success: false, .. }
                    if name == "edit_file" && output.contains("Access denied")
            )
        })
        .collect();
    assert_eq!(
        denied_results.len(),
        1,
        "gate-denied edit should produce exactly one Access denied ToolResult, got {denied_results:?}"
    );

    let hook_runs = std::fs::read_to_string(&counter).unwrap().len();
    assert_eq!(
        hook_runs, 0,
        "post-tool hook must NOT run for a gate denial (the tool body never ran), ran {hook_runs} times"
    );
}

// WO 44.28 regression: a same-batch bash call swaps the edit target for a
// symlink after Phase-1 canonicalization but before the Phase 2.5 body.
// The file is pre-marked read so the read-before-edit gate ALLOWS — pre-fix
// the symlink walk only ran inside the gate's Denied arm, so the body would
// have opened the attacker-controlled symlink target. The hoisted walk must
// deny before `run_prepared_call` and the symlink target must be untouched.
#[cfg(unix)]
#[tokio::test]
async fn symlink_swap_blocks_edit_file_when_read_gate_allows() {
    use crate::tools::bash::Bash;
    use crate::tools::edit_file::EditFile;

    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("kf_code_symlink_swap_edit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("victim.txt");
    let secret = dir.join("secret.txt");
    std::fs::write(&target, "original").unwrap();
    std::fs::write(&secret, "EXFIL_SECRET").unwrap();

    let bash = Arc::new(Bash::new(
        crate::shared::access::DenyList::default(),
        crate::shared::access::PathGuard::default(),
        false,
        None,
        crate::shared::SandboxConfig::default(),
    ));
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
        false,
        false,
    ));

    let swap_cmd = format!(
        "rm -f {tgt} && ln -s {sec} {tgt}",
        tgt = target.to_string_lossy(),
        sec = secret.to_string_lossy(),
    );

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-bash-swap".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": swap_cmd }),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-edit".into(),
                name: "edit_file".into(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy(),
                    "old_string": "original",
                    "new_string": "MUTATED",
                }),
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
        make_executor(Box::new(adapter), vec![bash, edit_tool], make_config(true)).unwrap();
    // Pre-mark read so the read-before-edit gate ALLOWS — this is the
    // attack precondition. The symlink walk must still deny.
    exe.sandbox.mark_read(&target.canonicalize().unwrap());

    let events = exe
        .run_turn_collecting("swap then edit", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false, .. }
                if name == "edit_file" && output.contains("symlink")
        )
    });
    assert!(
        denied,
        "edit_file after a same-batch symlink swap must be denied, got events: {events:?}"
    );

    // The symlink target must be untouched — the edit body never ran.
    let on_disk = std::fs::read_to_string(&secret).unwrap();
    assert_eq!(
        on_disk, "EXFIL_SECRET",
        "symlink target must be unchanged; edit body ran and wrote through the symlink"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// WO 48.17: notebook_edit is a file tool — it must get the same Phase-2.5
// symlink walk its siblings get. Same attack shape as the edit_file case
// above: a same-batch bash call swaps the notebook for a symlink after
// Phase-1 canonicalization; the walk must deny before the body runs and
// the symlink target must be untouched.
#[cfg(unix)]
#[tokio::test]
async fn symlink_swap_blocks_notebook_edit_when_read_gate_allows() {
    use crate::tools::bash::Bash;
    use crate::tools::notebook_edit::NotebookEdit;

    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("kf_code_symlink_swap_nb_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("nb.ipynb");
    let secret = dir.join("secret.txt");
    std::fs::write(
        &target,
        r#"{"cells":[{"cell_type":"code","source":["a"]}],"nbformat":4}"#,
    )
    .unwrap();
    std::fs::write(&secret, "EXFIL_SECRET").unwrap();

    let bash = Arc::new(Bash::new(
        crate::shared::access::DenyList::default(),
        crate::shared::access::PathGuard::default(),
        false,
        None,
        crate::shared::SandboxConfig::default(),
    ));
    let nb_tool: Arc<dyn Tool> = Arc::new(NotebookEdit::new(
        None,
        crate::session::access::PathGuard::default(),
    ));

    let swap_cmd = format!(
        "rm -f {tgt} && ln -s {sec} {tgt}",
        tgt = target.to_string_lossy(),
        sec = secret.to_string_lossy(),
    );

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-bash-swap".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": swap_cmd }),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-nb".into(),
                name: "notebook_edit".into(),
                arguments: serde_json::json!({
                    "path": target.to_string_lossy(),
                    "index": 0,
                    "source": "MUTATED",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![bash, nb_tool], make_config(true)).unwrap();
    // Pre-mark read so the read-before-edit gate ALLOWS — this is the
    // attack precondition. The symlink walk must still deny.
    exe.sandbox.mark_read(&target.canonicalize().unwrap());

    let events = exe
        .run_turn_collecting("swap then notebook_edit", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false, .. }
                if name == "notebook_edit" && output.contains("symlink")
        )
    });
    assert!(
        denied,
        "notebook_edit after a same-batch symlink swap must be denied, got events: {events:?}"
    );

    // The symlink target must be untouched — the notebook_edit body never ran.
    let on_disk = std::fs::read_to_string(&secret).unwrap();
    assert_eq!(
        on_disk, "EXFIL_SECRET",
        "symlink target must be unchanged; notebook_edit body ran and wrote through the symlink"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// WO 48.17: the pre-tool hook for notebook_edit must receive the Phase-1
// resolved path in its args (KF_TOOL_ARGS_JSON). Note: for write-class
// tools `check_write` returns the raw literal verbatim (access/mod.rs
// `GuardVerdict::Allowed(path.to_path_buf())`), so "resolved" == the
// model-supplied path here — the assertion pins that the substitution
// code path feeds the hook the file-tool path arg at all.
#[cfg(unix)]
#[tokio::test]
async fn pre_tool_hook_receives_resolved_path_for_notebook_edit() {
    use crate::tools::notebook_edit::NotebookEdit;

    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("kf_code_hook_resolved_nb_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let nb = dir.join("nb.ipynb");
    std::fs::write(
        &nb,
        r#"{"cells":[{"cell_type":"code","source":["a"]}],"nbformat":4}"#,
    )
    .unwrap();

    let nb_tool: Arc<dyn Tool> = Arc::new(NotebookEdit::new(
        None,
        crate::session::access::PathGuard::default(),
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-nb".into(),
                name: "notebook_edit".into(),
                arguments: serde_json::json!({
                    "path": nb.to_string_lossy(),
                    "index": 0,
                    "source": "updated",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (hook_tmp, hooks_dir) = temp_hooks_dir();
    let captured = hook_tmp.path().join("hook_args");
    std::fs::write(&captured, b"").unwrap();
    let script =
        format!("#!/bin/bash\nprintf '%s' \"$KF_TOOL_ARGS_JSON\" > {captured:?}\nexit 0\n");
    std::fs::write(hooks_dir.join("pre-tool-notebook_edit.sh"), script).unwrap();

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![nb_tool], config).unwrap();
    exe.sandbox.mark_read(&nb);

    let events = exe
        .run_turn_collecting(
            "notebook_edit with args-capturing hook",
            &approval_tx,
            never_cancelled(),
        )
        .await
        .unwrap();

    let ok = events
        .iter()
        .any(|e| matches!(e, TurnEvent::ToolResult { name, success: true, .. } if name == "notebook_edit"));
    assert!(ok, "notebook_edit should succeed, got events: {events:?}");

    let hook_args = std::fs::read_to_string(&captured).unwrap();
    let nb_str = nb.to_string_lossy().into_owned();
    assert!(
        hook_args.contains(&nb_str),
        "hook must receive the notebook path in KF_TOOL_ARGS_JSON, got: {hook_args}"
    );

    // The body wrote through the resolved path — the file changed.
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&nb).unwrap()).unwrap();
    assert_eq!(
        on_disk["cells"][0]["source"][0], "updated",
        "body must edit the file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// WO 48.17: notebook_edit only ever modifies an existing notebook, so the
// read-before-edit gate applies (same as edit_file): a cold call on a file
// not read this session must be denied with the Read-before-edit reason.
#[cfg(unix)]
#[tokio::test]
async fn notebook_edit_cold_call_denied_by_read_gate() {
    use crate::tools::notebook_edit::NotebookEdit;

    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("kf_code_cold_nb_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let nb = dir.join("nb.ipynb");
    std::fs::write(
        &nb,
        r#"{"cells":[{"cell_type":"code","source":["a"]}],"nbformat":4}"#,
    )
    .unwrap();

    let nb_tool: Arc<dyn Tool> = Arc::new(NotebookEdit::new(
        None,
        crate::session::access::PathGuard::default(),
    ));

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-nb".into(),
                name: "notebook_edit".into(),
                arguments: serde_json::json!({
                    "path": nb.to_string_lossy(),
                    "index": 0,
                    "source": "MUTATED",
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    // Deliberately do NOT mark_read — the read-before-edit gate must deny.
    let mut exe = make_executor(Box::new(adapter), vec![nb_tool], make_config(true)).unwrap();

    let events = exe
        .run_turn_collecting("cold notebook_edit", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false, .. }
                if name == "notebook_edit" && output.contains("Read-before-edit")
        )
    });
    assert!(
        denied,
        "cold notebook_edit must be denied by the read gate, got events: {events:?}"
    );

    // The notebook is untouched — the body never ran.
    let on_disk = std::fs::read_to_string(&nb).unwrap();
    assert!(on_disk.contains("\"a\""), "notebook must be unchanged");
    let _ = std::fs::remove_dir_all(&dir);
}

// WO 44.28 regression (read side): a same-batch bash call swaps the read
// target's final component for a symlink before the read_file body runs.
// Pre-fix the symlink walk never ran for reads (needs_read_gate is false
// for read_file), so the body would have followed the symlink and read the
// attacker's secret. The hoisted walk must deny before `run_prepared_call`.
#[cfg(unix)]
#[tokio::test]
async fn symlink_swap_blocks_read_file_on_swapped_final_component() {
    use crate::tools::bash::Bash;
    use crate::tools::read_file::ReadFile;

    let tmp = std::env::temp_dir();
    let dir = tmp.join(format!("kf_code_symlink_swap_read_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("victim.txt");
    let secret = dir.join("secret.txt");
    std::fs::write(&target, "harmless").unwrap();
    std::fs::write(&secret, "EXFIL_SECRET").unwrap();

    let bash = Arc::new(Bash::new(
        crate::shared::access::DenyList::default(),
        crate::shared::access::PathGuard::default(),
        false,
        None,
        crate::shared::SandboxConfig::default(),
    ));
    let read_tool: Arc<dyn Tool> = Arc::new(ReadFile::new(
        crate::session::access::PathGuard::default(),
        false,
        4096,
    ));

    let swap_cmd = format!(
        "rm -f {tgt} && ln -s {sec} {tgt}",
        tgt = target.to_string_lossy(),
        sec = secret.to_string_lossy(),
    );

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-bash-swap".into(),
                name: "bash".into(),
                arguments: serde_json::json!({ "command": swap_cmd }),
            }),
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({ "path": target.to_string_lossy() }),
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
        make_executor(Box::new(adapter), vec![bash, read_tool], make_config(true)).unwrap();

    let events = exe
        .run_turn_collecting("swap then read", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false, .. }
                if name == "read_file" && output.contains("symlink")
        )
    });
    assert!(
        denied,
        "read_file after a same-batch symlink swap must be denied, got events: {events:?}"
    );

    // No read result should leak the secret content.
    let leaked = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { output, .. } if output.contains("EXFIL_SECRET")
        )
    });
    assert!(
        !leaked,
        "read_file must not return the symlink target's secret content, got events: {events:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// WO 35.3: an executor with an attached root cancel token must kill an
// in-flight bash child when the token fires + the flag is set — the
// subagent cancellation path. Without the live child token (pre-WO 35.3
// snapshot semantics) the sleep would run to its 60s tool timeout.
#[cfg(unix)]
#[tokio::test]
async fn attached_cancel_token_kills_inflight_bash_promptly() {
    use crate::tools::bash::Bash;

    let tmp = std::env::temp_dir();
    let marker = tmp.join(format!("kf_code_exec_cancel_marker_{}", std::process::id()));
    let ready = tmp.join(format!("kf_code_exec_cancel_ready_{}", std::process::id()));
    let marker_str = marker.to_string_lossy().to_string();
    let ready_str = ready.to_string_lossy().to_string();
    remove_test_file(&marker);
    remove_test_file(&ready);

    let bash = Arc::new(Bash::new(
        crate::shared::access::DenyList::default(),
        crate::shared::access::PathGuard::default(),
        false,
        None,
        crate::shared::SandboxConfig::default(),
    ));
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-cancel-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({
                    "command": format!("touch {ready_str}; sleep 30; touch {marker_str}"),
                    "timeout": 60,
                }),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![bash], make_config(true)).unwrap();

    // The subagent cancel pair: flag (turn-loop machinery) + root token
    // (per-call child tokens derive from it).
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let token = tokio_util::sync::CancellationToken::new();
    exe.set_cancel_token(Some(token.clone()));

    // WO 38.12: gated-start — poll for the readiness file before firing
    // cancel. The shell touches `ready` immediately on start, so we know
    // the child is running when the file appears. No production readiness
    // signal exists; the readiness file is a test-only technique.
    // Replaces the fixed 300ms sleep-then-cancel race. The readiness poll
    // runs in a spawned task so the main task can drive the turn.
    {
        let ready_clone = ready.clone();
        let flag_clone = Arc::clone(&flag);
        let token_clone = token.clone();
        tokio::spawn(async move {
            // Readiness deadline is generous on purpose: under parallel-
            // worktree load the bash spawn itself can take seconds
            // (state.md "Known flakes") — the file normally appears in ms.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while std::time::Instant::now() < deadline {
                if ready_clone.exists() {
                    flag_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    token_clone.cancel();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
    }

    let start = std::time::Instant::now();
    let events = exe
        .run_turn_collecting("run the sleep", &approval_tx, &flag)
        .await
        .expect("turn should end cooperatively, not error");

    let elapsed = start.elapsed();
    // Bound must stay below the child's 30s sleep so a no-kill regression
    // (bash running to completion) still fails it. 25s: observed 12-18s
    // under parallel-worktree load on an 8-core box (state.md "Known
    // flakes") — the old 10s bound flagged healthy runs.
    assert!(
        elapsed < std::time::Duration::from_secs(25),
        "bash must die on cancel within a bounded window, took {elapsed:?}; events: {events:?}"
    );

    // The process group really died: no descendant survives to touch the
    // marker. Poll for the marker's absence with a 1s ceiling (mirrors the
    // bash tool's own cancel test) — fails fast if a descendant touches it,
    // bounds the wait otherwise.
    let grace_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < grace_deadline {
        assert!(
            !marker.exists(),
            "cancelled bash left a surviving descendant that touched the marker"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !marker.exists(),
        "cancelled bash left a surviving descendant that touched the marker"
    );
    remove_test_file(&marker);
    remove_test_file(&ready);
}

/// WO 38.1 TOCTOU: the tool body must receive the Phase-1 RESOLVED
/// (canonical) path, not the raw model argument — the body's open must
/// target exactly what the path guard checked.
#[tokio::test]
async fn test_file_tool_receives_resolved_path() {
    let dir = std::env::temp_dir().join(format!("kf_wo38_resolved_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let file = dir.join("note.txt");
    std::fs::write(&file, "contents").unwrap();

    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "read_file",
            description: "read a file",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "contents".into(),
        },
    };

    // Non-canonical argument (`sub/../note.txt`) — Phase 1 canonicalizes it.
    let raw_arg = dir
        .join("sub")
        .join("..")
        .join("note.txt")
        .to_string_lossy()
        .into_owned();
    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": raw_arg}),
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

    let _events = exe
        .run_turn_collecting("read it", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let canonical = file.canonicalize().unwrap().to_string_lossy().into_owned();
    let got = captured
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|a| a.get("path"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        got.as_deref(),
        Some(canonical.as_str()),
        "tool body must receive the canonical resolved path"
    );
}

/// WO 43.16 no-throw contract: a registered tool that panics must yield a
/// clean `ToolOutcome::Failure(ToolError::Internal { "tool panicked: …" })`,
/// not unwind through the executor loop. The catch_unwind wrapper in
/// `run_prepared_call` (dispatch.rs:677-698) is the guard; this test pins it.
struct PanickingTool {
    def: ToolDef,
}

#[async_trait::async_trait]
impl Tool for PanickingTool {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        panic!("boom from PanickingTool");
    }
}

#[tokio::test]
async fn test_panicking_tool_yields_failure_internal() {
    let tool = PanickingTool {
        def: ToolDef {
            name: "boom",
            description: "always panics",
            parameters: serde_json::json!({"type": "object"}),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "boom".into(),
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

    // A panic in the tool body would propagate and abort the test task;
    // reaching the assertion itself proves catch_unwind contained it.
    let events = exe
        .run_turn_collecting("call boom", &approval_tx, never_cancelled())
        .await
        .expect("panicking tool must not unwind the executor");

    let failure = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult {
            name,
            output,
            success: false,
            call_id: _,
        } if name == "boom" => Some(output),
        _ => None,
    });
    let msg = failure.expect("boom tool must emit a failed ToolResult");
    assert!(
        msg.contains("tool panicked: boom from PanickingTool"),
        "expected Internal panic message, got: {msg}"
    );

    // The conversation must carry the Internal error text for the model.
    let has_panic_msg = exe
        .conversation
        .all()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .any(|m| m.content.contains("tool panicked: boom from PanickingTool"));
    assert!(
        has_panic_msg,
        "conversation must carry the panic text in a tool-role message"
    );
}
