// Tool-call dispatching tests. Split out from the former single-file
// `mod.rs` (WO 15.5). Pure refactor: test bodies are moved verbatim.

use super::super::*;
use super::common::*;
use crate::shared::metrics::{read_events, PlanDecisionKind};
use crate::shared::test_util::remove_test_file;
use crate::shared::{FinishReason, ModelInfo, StreamEvent, TokenUsage, ToolDef, ToolOutcome};
use crate::tools::Tool;
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
                }),
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
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

/// Regression test for GPT 5.5 review finding #9: the
/// `BusEvent::Edit` used to carry the user's `old_string` as the
/// `diff` field, which made the event useless to downstream
/// consumers (verifiers, correction loop, log replay). After the
/// fix, it should carry the rendered diff that the tool returned
/// in `ToolOutcome::FileEdit { diff, .. }`. This test wires up a
/// real `edit_file` tool call, returns a `FileEdit` outcome with a
/// distinctive diff string, and asserts the dispatched event
/// matches.
#[tokio::test]
async fn test_edit_event_diff_carries_real_diff_not_old_string() {
    use crate::session::event_bus::{EditEvent, EventHandler, EventKind, HandlerResult};

    struct Capture {
        last: Mutex<Option<String>>,
    }
    #[async_trait::async_trait]
    impl EventHandler for Capture {
        fn id(&self) -> &str {
            "capture"
        }
        fn subscribed_kinds(&self) -> Vec<EventKind> {
            vec![EventKind::Edit]
        }
        async fn handle(&self, event: &BusEvent) -> HandlerResult {
            if let BusEvent::Edit(EditEvent { diff, .. }) = event {
                *self.last.lock().unwrap() = Some(diff.clone());
            }
            HandlerResult {
                handler_id: "capture".into(),
                success: true,
                message: String::new(),
            }
        }
    }

    let captured: Arc<Capture> = Arc::new(Capture {
        last: Mutex::new(None),
    });

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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
    exe.event_bus
        .register(captured.clone() as Arc<dyn EventHandler>)
        .await
        .unwrap();
    // The read-before-edit gate would otherwise deny the edit
    // before the tool runs (and before the EditEvent is emitted).
    // Mark the path as already read so we exercise the diff path.
    exe.read_gate
        .mark_read(&std::path::PathBuf::from("/tmp/edit_event_diff_test.txt"));

    let _events = exe
        .run_turn_collecting("edit it", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let last = captured.last.lock().unwrap().clone();
    let got = last.expect("EditEvent should have been dispatched");
    assert!(
        got.contains("--- a")
            && got.contains("+++ b")
            && got.contains("-old line")
            && got.contains("+new line"),
        "EditEvent.diff should be the rendered diff, got: {got:?}"
    );
    assert!(
        got.starts_with("---") || got.contains("\n---"),
        "diff should start with --- header, got: {got:?}"
    );
}

#[tokio::test]
async fn test_read_image_honours_path_guard_size_limit() {
    let tmp = std::env::temp_dir().join(format!(
        "kirkforge_oversized_image_test_{}.png",
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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
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
        "kirkforge_read_then_write_{}.txt",
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
    );

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
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
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
    use kirkforge_plugin_host::{PluginRegistry, TrustPolicy};
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
        plugin_dir.join("kirkforge.toml"),
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
            TrustPolicy::up_to(kirkforge_plugin::TrustTier::Shell),
        )
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(registry.active_count(), 1);

    // 2. Construct an Executor with the plugin registry so
    //    `init_default_verifiers` wires the plugin verifier into the
    //    unified `VerifierBus` via `register_plugin_verifiers_into_bus`.
    let adapter = MockAdapter::new(vec![], make_info());
    let log_path = std::env::temp_dir().join(format!(
        "kirkforge-plugin-verifier-test-{}.ndjson",
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
    );

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
    //    `CorrectionResult` sourced from `plugin:security`.
    let plugin_correction = corrections
        .iter()
        .find(|c| c.verifier == "plugin:security" && !c.success && c.fix.is_none());
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
        "kirkforge_denied_edit_single_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    use crate::tools::edit_file::EditFile;
    let edit_tool: Arc<dyn Tool> = Arc::new(EditFile::new(
        None,
        crate::session::access::PathGuard::default(),
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
    let mut exe = make_executor(Box::new(adapter), vec![edit_tool], make_config(true));
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
                TurnEvent::ToolResult { name, output, success: false }
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
