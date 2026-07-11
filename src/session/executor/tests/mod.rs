//! Executor unit tests.

use super::helpers::*;
use super::turn::PostTurnHookGuard;
use super::types::PLAN_COMPLETE_MARKER;
use super::*;
use crate::adapters::ModelAdapter;
use crate::shared::permission::PermissionAction;
use crate::shared::test_util::{remove_test_dir, remove_test_file};
use crate::shared::{
    Config, FinishReason, Message, ModelInfo, Role, StreamEvent, TokenUsage, ToolCallStyle,
    ToolDef, ToolInvocation, ToolOutcome,
};
use crate::tools::{Tool, ToolContext};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// RAII guard that removes a temp file when dropped. Used by plan-mode
/// tests that need a real, readable file on disk.
struct CleanupFile(std::path::PathBuf);

impl Drop for CleanupFile {
    fn drop(&mut self) {
        remove_test_file(&self.0);
    }
}

fn never_cancelled() -> &'static AtomicBool {
    static NC: std::sync::LazyLock<AtomicBool> =
        std::sync::LazyLock::new(|| AtomicBool::new(false));
    &NC
}

fn cfg(exe: &Executor) -> std::sync::RwLockReadGuard<'_, Config> {
    crate::shared::read_shared_config(&exe.config)
}

struct MockAdapter {
    first_events: Vec<StreamEvent>,

    followup_events: Vec<StreamEvent>,
    info: ModelInfo,
    call_count: Arc<Mutex<usize>>,
}

impl MockAdapter {
    fn new(events: Vec<StreamEvent>, info: ModelInfo) -> Self {
        Self {
            first_events: events,
            followup_events: vec![
                StreamEvent::Text("Done.".to_string()),
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            ],
            info,
            call_count: Arc::new(Mutex::new(0)),
        }
    }

    fn with_followup_events(mut self, events: Vec<StreamEvent>) -> Self {
        self.followup_events = events;
        self
    }
}

#[async_trait::async_trait]
impl ModelAdapter for MockAdapter {
    fn model_info(&self) -> ModelInfo {
        self.info.clone()
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDef],
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        let mut count = self.call_count.lock().unwrap();
        let is_first = *count == 0;
        *count += 1;
        drop(count);

        let (tx, rx) = mpsc::channel(64);
        let events = if is_first {
            self.first_events.clone()
        } else {
            self.followup_events.clone()
        };
        tokio::spawn(async move {
            for ev in events {
                let _ = tx.send(ev).await;
            }
        });
        Ok(rx)
    }
}

#[derive(Clone)]
struct MockTool {
    def: ToolDef,
    captured_args: Arc<Mutex<Option<serde_json::Value>>>,
    outcome: ToolOutcome,
}

#[async_trait::async_trait]
impl Tool for MockTool {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, args: serde_json::Value) -> ToolOutcome {
        *self.captured_args.lock().unwrap() = Some(args);
        self.outcome.clone()
    }
}

fn make_info() -> ModelInfo {
    ModelInfo {
        name: "test-model".into(),
        supports_thinking: false,
        tool_call_format: ToolCallStyle::Native,
        max_context_tokens: 8192,
        recommended_temperature: 0.7,
        supports_images: false,
        supports_cache: false,
    }
}

fn make_config(auto_approve: bool) -> Config {
    Config {
        default_model: "test".into(),
        ollama_host: "http://localhost:11434".into(),
        auto_approve,
        truncation_strategy: crate::shared::TruncationStrategy::KeepToolOnly,
        max_tool_result_chars: 4000,
        deny_paths: vec![],
        deny_urls: vec![],
        deny_extensions: vec![],
        allowed_write_dirs: vec![],
        sandbox_dir: None,
        block_dotfiles: false,
        block_gitignored_dotfiles: false,
        max_file_read_size: 1024 * 1024,
        max_overwrite_size: 1024 * 1024,
        follow_symlinks: false,
        block_binary_reads: false,
        bash_sandbox_workdir: false,
        carryover_enabled: false,
        permission_rules: vec![],
        summarize_model: String::new(),
        summarize_enabled: false,
        routing_enabled: false,
        router_model: String::new(),
        routing_model_map: std::collections::HashMap::new(),
        mcp_servers: vec![],
        bang_requires_approval: false,
        json_mode: false,
        preserve_recent_messages: 2,
        max_plugin_trust: kirkforge_plugin::TrustTier::Shell,
        reject_on_excess_plugin_trust: true,
        plugin_signature_validation: false,
        plugin_public_key_path: None,
        plugin_allowed_env_vars: vec![],
        max_tool_calls_per_turn: 10,
        max_persona_turns: 10,
        hooks_dir: None,
        commit_max_file_size: 5 * 1024 * 1024,
        tool_timeout_secs: Some(30),
        request_timeout_secs: 300,
        dry_run: false,
        cache_enabled: false,
        cache_dir: None,
        audit_log_path: None,
        memory_enabled: false,
        memory_max_tokens: 0,
        memory_top_n: 0,
        checkpoint_interval_messages: 0,
        plugin_sources: std::collections::HashMap::new(),
        enabled_plugins: vec![],
    }
}

fn make_executor(
    adapter: Box<dyn ModelAdapter>,
    tools: Vec<Arc<dyn Tool>>,
    config: Config,
) -> Executor {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let temp_dir = std::env::temp_dir();
    let log_path = temp_dir.join(format!(
        "kirkforge-test-{}-{}.ndjson",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    remove_test_file(&log_path);
    let (conversation, _outcome) = ConversationLog::open(log_path).unwrap();
    let mut composite = crate::session::toolset::CompositeToolset::empty();
    composite.add(Box::new(crate::session::toolset::VecToolset::new(
        "test", tools,
    )));
    Executor::with_log(adapter, composite, config, conversation, None)
}

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
async fn test_approval_required_for_destructive_bash() {
    // Non-read-only bash (a redirect here) requires approval even
    // when auto_approve is false.
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "echo x > file.txt"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.unwrap();
        assert_eq!(req.tool_name, "bash");
        let _ = req.response.send(ApprovalResponse::Approved);
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    approval_handle.await.unwrap();

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "ran!")));
}

#[tokio::test]
async fn test_read_only_bash_auto_approved() {
    // Read-only bash commands like `ls -la` should run without
    // requiring approval when auto_approve is false.
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls -la"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

    // No approval request should be sent, so the channel stays empty.
    let approval_handle = tokio::spawn(async move {
        let res =
            tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv()).await;
        assert!(
            res.is_err() || res.unwrap().is_none(),
            "read-only bash should not ask for approval"
        );
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    approval_handle.await.unwrap();

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "ran!")));
}

#[tokio::test]
async fn test_approval_denied_for_destructive_tool() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "rm -rf /"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();

    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.unwrap();
        let _ = req.response.send(ApprovalResponse::Denied);
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    approval_handle.await.unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "Tool should not have been called when denied"
    );

    let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "bash" && output.contains("denied")));
    assert!(denied, "Should report that operation was denied");
}

#[tokio::test]
async fn test_always_approve_pushes_permission_rule_not_auto_approve() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "cargo test --release"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.unwrap();

        let _ = req.response.send(ApprovalResponse::AlwaysApprove);
    });

    let config = make_config(false);
    assert!(config.permission_rules.is_empty());
    assert!(!config.auto_approve);

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let _events = exe
        .run_turn_collecting("run tests", &approval_tx, never_cancelled())
        .await
        .unwrap();
    approval_handle.await.unwrap();

    {
        let cfg = cfg(&exe);
        assert_eq!(
            cfg.permission_rules.len(),
            1,
            "AlwaysApprove should have appended exactly one rule, got {:?}",
            cfg.permission_rules
        );
        let r = &cfg.permission_rules[0];
        assert_eq!(r.tool, "bash");
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "cargo test --release");
        assert_eq!(r.action, PermissionAction::Allow);
    }

    assert!(
        !cfg(&exe).auto_approve,
        "AlwaysApprove should NOT flip auto_approve — the new rule is the user's intent"
    );
}

#[tokio::test]
async fn test_always_approve_dedups_repeated_calls() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    let approval_handle = tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let _ = req.response.send(ApprovalResponse::AlwaysApprove);
        }
    });

    let config = make_config(false);

    let mut config = config;
    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "ls".into(),
            action: PermissionAction::Allow,
        });
    assert_eq!(config.permission_rules.len(), 1);

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let _events = exe
        .run_turn_collecting("list", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    assert_eq!(
        cfg(&exe).permission_rules.len(),
        1,
        "AlwaysApprove should dedup against an existing identical rule"
    );
}

#[tokio::test]
async fn test_always_approve_does_not_overwrite_existing_deny() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "rm -rf build"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    let approval_handle = tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let _ = req.response.send(ApprovalResponse::AlwaysApprove);
        }
    });

    let mut config = make_config(false);

    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "rm -rf build".into(),
            action: PermissionAction::Deny,
        });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let _events = exe
        .run_turn_collecting("clean", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    {
        let cfg = cfg(&exe);
        assert_eq!(cfg.permission_rules.len(), 1);
        assert_eq!(
            cfg.permission_rules[0].action,
            PermissionAction::Deny,
            "Existing Deny should not be overwritten by a new Allow on the same pattern"
        );
    }
}

#[tokio::test]
async fn test_deny_rule_blocks_bash_even_with_auto_approve() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),

                arguments: serde_json::json!({"command": "rm -rf /home/user/build"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    let approval_handle = tokio::spawn(async move {
        while let Some(req) = approval_rx.recv().await {
            let _ = req.response.send(ApprovalResponse::Approved);
        }
    });

    let mut config = make_config(true);
    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "rm -rf **".into(),
            action: PermissionAction::Deny,
        });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("clean build", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "Deny rule should prevent the tool from being called even with auto_approve"
    );

    let denied_msg = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } if name == "bash" => Some(output.as_str()),
        _ => None,
    });
    assert!(
        denied_msg.is_some_and(|m| m.contains("Permission rule denied")),
        "Expected a permission-rule denial message, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_deny_paths_blocks_write_file_even_with_auto_approve() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "write_file",
            description: "write to a file",
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "wrote".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": "secret/credentials.json",
                    "content": "{\"leaked\": true}"
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

    let mut config = make_config(true);
    config.deny_paths = vec!["secret/**".into()];

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("save creds", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "write_file must be blocked by the path deny-list before the tool runs"
    );

    let denied = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "write_file" && output.contains("denied")
        ));
    assert!(
        denied,
        "Expected a deny-list refusal ToolResult, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_dangerous_shell_blocked_even_with_allow_all_rule() {
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

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "rm -rf /"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    // No approval request should be sent: the allow-all rule permits the
    // call, but the dangerous-pattern guard blocks it before the tool runs.
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    let approval_handle = tokio::spawn(async move {
        let res =
            tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv()).await;
        assert!(
            res.is_err() || res.unwrap().is_none(),
            "dangerous command should be blocked by the safety gate, not by an approval prompt"
        );
    });

    let mut config = make_config(true);
    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "*".into(),
            key: "*".into(),
            pattern: String::new(),
            action: PermissionAction::Allow,
        });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("wipe disk", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "dangerous shell command must be blocked even when all permission rules allow it"
    );

    let blocked = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "bash" && output.contains("dangerous")
        ));
    assert!(
        blocked,
        "Expected a dangerous-pattern refusal, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_auto_approve_does_not_skip_approval_for_non_read_only_bash() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "compiled".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "cargo build"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.expect("approval request");
        assert_eq!(req.tool_name, "bash");
        let _ = req.response.send(ApprovalResponse::Approved);
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true));
    let _events = exe
        .run_turn_collecting("build", &approval_tx, never_cancelled())
        .await
        .unwrap();
    approval_handle.await.unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "Tool should have run after the user approved the non-read-only command"
    );
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
    config.max_tool_calls_per_turn = 5;
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

#[tokio::test]
async fn test_explicit_allow_rule_honored_under_auto_approve_bash() {
    // Regression: with auto_approve=true, an explicit allow rule for a
    // non-read-only bash command must be honored, not downgraded back to Ask.
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "built!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "cargo build"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();
    let approval_handle = tokio::spawn(async move {
        let res =
            tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv()).await;
        assert!(
            res.is_err() || res.unwrap().is_none(),
            "Explicit allow rule should be honored under auto_approve; no approval prompt expected"
        );
    });

    let mut config = make_config(true);
    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "cargo build".into(),
            action: PermissionAction::Allow,
        });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("build", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "built!")));
}

#[tokio::test]
async fn test_deny_rule_blocks_read_file() {
    // Regression: deny rules must fire for non-destructive tools too.
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "read_file",
            description: "read a file",
            parameters: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "secret".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/etc/passwd"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel::<ApprovalRequest>();

    let mut config = make_config(false);
    config
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "read_file".into(),
            key: "path".into(),
            pattern: "/etc/**".into(),
            action: PermissionAction::Deny,
        });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("read secrets", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "Deny rule on read_file should prevent the tool from running"
    );

    let denied = events.iter().any(|e| matches!(
            e,
            TurnEvent::ToolResult { name, output, .. } if name == "read_file" && output.contains("Permission rule denied")
        ));
    assert!(denied, "Expected a permission-rule denial for read_file");
}

#[test]
fn test_is_read_only_bash_simple_ls() {
    assert!(is_read_only_bash("ls -la"));
}

#[test]
fn test_is_read_only_bash_pwd() {
    assert!(is_read_only_bash("pwd"));
}

#[test]
fn test_is_read_only_bash_cat() {
    assert!(is_read_only_bash("cat src/main.rs"));
}

#[test]
fn test_is_read_only_bash_grep() {
    assert!(is_read_only_bash("grep -r foo ."));
}

#[test]
fn test_is_read_only_bash_echo() {
    assert!(is_read_only_bash("echo hello world"));
}

#[test]
fn test_is_read_only_bash_find() {
    // Plain find invocations are read-only discovery.
    assert!(is_read_only_bash("find . -name '*.rs'"));
    assert!(is_read_only_bash("find . -type f"));
    assert!(is_read_only_bash("find ."));
}

#[test]
fn test_is_read_only_bash_find_destructive_flags_blocked() {
    // Destructive find flags must still require approval.
    assert!(!is_read_only_bash("find . -delete"));
    assert!(!is_read_only_bash("find . -type f -delete"));
    assert!(!is_read_only_bash("find . -exec rm {} \\;"));
    assert!(!is_read_only_bash("find . -exec sh {} \\;"));
    assert!(!is_read_only_bash("find . -ok rm {} \\;"));
    assert!(!is_read_only_bash("find . -fprint out.txt"));
    assert!(!is_read_only_bash("find . -fls out.txt"));
}

#[test]
fn test_is_read_only_bash_curl_is_not_read_only() {
    assert!(!is_read_only_bash("curl https://example.com"));
}

#[test]
fn test_is_read_only_bash_wget_is_not_read_only() {
    assert!(!is_read_only_bash("wget http://example.com"));
}

#[test]
fn test_is_read_only_bash_pipe_to_sh_blocked() {
    assert!(!is_read_only_bash("cat script | sh"));
    assert!(!is_read_only_bash("cat script | bash"));
}

#[test]
fn test_is_read_only_bash_pipe_to_writer_blocked() {
    // A read-only producer piped into a writing consumer must NOT be
    // auto-approved.
    assert!(!is_read_only_bash("cat list.txt | xargs rm"));
    assert!(!is_read_only_bash("cat data | tee /etc/important"));
    assert!(!is_read_only_bash("cat in | dd of=/dev/sda"));
    assert!(!is_read_only_bash("grep -rl foo . | xargs sed -i 's/a/b/'"));
}

#[test]
fn test_is_read_only_bash_read_only_pipe_allowed() {
    // Pipelines where every stage is read-only stay auto-approved.
    assert!(is_read_only_bash("cat x | grep foo | sort | uniq -c"));
    assert!(is_read_only_bash("ps aux | grep ssh | wc -l"));
}

#[test]
fn test_is_read_only_bash_redirect_blocked() {
    assert!(!is_read_only_bash("ls > out.txt"));
    assert!(!is_read_only_bash("grep foo file >> log.txt"));
}

#[test]
fn test_is_read_only_bash_chaining_blocked() {
    assert!(!is_read_only_bash("ls && rm -rf /"));
    assert!(!is_read_only_bash("cat file; rm file"));
    assert!(!is_read_only_bash("ls || true"));
}

#[test]
fn test_is_read_only_bash_substitution_blocked() {
    assert!(!is_read_only_bash("echo $(rm -rf /)"));
    assert!(!is_read_only_bash("echo `ls`"));
}

#[test]
fn test_is_read_only_bash_unknown_command_not_readonly() {
    assert!(!is_read_only_bash("rm -rf /home/user/temp"));
    assert!(!is_read_only_bash("cargo build"));
    assert!(!is_read_only_bash("python -c 'print(1)'"));
    assert!(!is_read_only_bash("npm install"));
}

#[test]
fn test_is_read_only_bash_word_boundary_no_false_positive() {
    assert!(!is_read_only_bash("scurling is not curl"));

    assert!(is_read_only_bash("cat /etc/hostname"));

    assert!(!is_read_only_bash("cattitude"));
}

#[test]
fn test_is_read_only_bash_empty_is_readonly() {
    assert!(is_read_only_bash(""));
    assert!(is_read_only_bash("   "));
}

#[test]
fn test_is_read_only_bash_ps_and_jobs() {
    assert!(is_read_only_bash("ps aux"));
    assert!(is_read_only_bash("jobs"));
    assert!(is_read_only_bash("help"));
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

/// Smoke test for `PostTurnHookGuard`. Constructs a guard with the
/// default `HookRunner` and lets it fall out of scope. The
/// `HookRunner::run` call inside `Drop` is fire-and-forget and
/// (in the absence of a real `~/.local/share/kirkforge/hooks/
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));

    let mut new_config = make_config(false);
    new_config.default_model = "qwen2.5:14b".into();
    new_config.json_mode = true;
    new_config.carryover_enabled = true;

    let summary = exe.reload_config(new_config.clone());

    assert!(
        summary.contains("default_model")
            || summary.contains("json_mode")
            || summary.contains("carryover_enabled"),
        "reload_config should report changed high-impact fields, got: {summary}"
    );

    // The shared lock should hold the new values.
    let cfg = cfg(&exe);
    assert_eq!(cfg.default_model, "qwen2.5:14b");
    assert!(cfg.json_mode);
    assert!(cfg.carryover_enabled);
}

#[tokio::test]
async fn test_plan_mode_blocks_write_file() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "write_file",
            description: "write a file",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "wrote".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "write_file".into(),
                arguments: serde_json::json!({
                    "path": "/tmp/plan_mode_test.txt",
                    "content": "hello"
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("write something", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "write_file must not run while plan mode is active"
    );
    let blocked = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "write_file" && output.contains("Plan mode blocked")
        )
    });
    assert!(blocked, "Expected plan-mode denial, got events: {events:?}");
}

#[tokio::test]
async fn test_plan_mode_blocks_non_read_only_bash() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "ran".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "cargo build"}),
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("build", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "non-read-only bash must not run while plan mode is active"
    );
    let blocked = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "bash" && output.contains("Plan mode blocked")
        )
    });
    assert!(blocked, "Expected plan-mode denial, got events: {events:?}");
}

#[tokio::test]
async fn test_plan_mode_allows_read_file() {
    let tmp = std::env::temp_dir().join(format!(
        "kirkforge_plan_read_test_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "file contents").expect("write temp file");
    let _cleanup = CleanupFile(tmp.clone());

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
            content: "file contents".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "read_file".into(),
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("read something", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "read_file should run in plan mode"
    );
    let allowed = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "read_file" && output == "file contents"
        )
    });
    assert!(allowed, "Expected read_file result, got events: {events:?}");
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
async fn test_plan_mode_allows_read_only_bash() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "listing".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls -la"}),
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("list files", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "read-only bash should run in plan mode"
    );
    let allowed = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "bash" && output == "listing"
        )
    });
    assert!(allowed, "Expected bash result, got events: {events:?}");
}

#[tokio::test]
async fn test_plan_mode_allows_bash_status() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash_status",
            description: "check job status",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"id": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "running".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash_status".into(),
                arguments: serde_json::json!({"id": "job-1"}),
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("check job", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "bash_status should run in plan mode"
    );
    let allowed = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "bash_status" && output == "running"
        )
    });
    assert!(
        allowed,
        "Expected bash_status result, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_plan_mode_allows_bash_cancel_for_read_only_query() {
    // bash_cancel is a read-only status query in plan mode (it only
    // asks to cancel a job; we treat it as allowed because it does not
    // mutate the worktree or read new files).
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash_cancel",
            description: "cancel a job",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"id": {"type": "string"}}
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "cancelled".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash_cancel".into(),
                arguments: serde_json::json!({"id": "job-1"}),
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
    exe.set_plan_mode(true);

    let events = exe
        .run_turn_collecting("cancel job", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "bash_cancel should run in plan mode"
    );
    let allowed = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "bash_cancel" && output == "cancelled"
        )
    });
    assert!(
        allowed,
        "Expected bash_cancel result, got events: {events:?}"
    );
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
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false));
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

fn temp_hooks_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let hooks_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    (tmp, hooks_dir)
}

#[tokio::test]
async fn test_pre_tool_hook_exit_two_blocks_bash() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!(
                {"type": "object", "properties": {"command": {"type": "string"}}}
            ),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "ran".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "echo hi"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (_tmp, hooks_dir) = temp_hooks_dir();
    std::fs::write(hooks_dir.join("pre-tool-bash.sh"), "#!/bin/bash\nexit 2").unwrap();

    let mut config = make_config(true);
    config.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "pre-tool hook exit 2 must prevent the bash tool from running"
    );
    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "bash" && output.contains("denied")
        )
    });
    assert!(
        denied,
        "Expected a hook-denial ToolResult, got events: {events:?}"
    );
}

#[tokio::test]
async fn test_pre_tool_hook_exit_one_allows_and_warns() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!(
                {"type": "object", "properties": {"command": {"type": "string"}}}
            ),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "ran".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "echo hi"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (_tmp, hooks_dir) = temp_hooks_dir();
    std::fs::write(
        hooks_dir.join("pre-tool-bash.sh"),
        "#!/bin/bash\necho warning >&2\nexit 1",
    )
    .unwrap();

    let mut config = make_config(true);
    config.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let _events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "pre-tool hook exit 1 must be fail-open and allow the bash tool to run"
    );
}

#[tokio::test]
async fn test_pre_tool_hook_timeout_allows_and_warns() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!(
                {"type": "object", "properties": {"command": {"type": "string"}}}
            ),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "ran".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "echo hi"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (_tmp, hooks_dir) = temp_hooks_dir();
    std::fs::write(hooks_dir.join("pre-tool-bash.sh"), "#!/bin/bash\nsleep 10").unwrap();

    let mut config = make_config(true);
    config.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let _events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "pre-tool hook timeout must be fail-open and allow the bash tool to run"
    );
}

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
    config.hooks_dir = Some(hooks_dir);
    let exe = make_executor(
        Box::new(MockAdapter::new(vec![], make_info())),
        vec![],
        config,
    );

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
            tokens_before: 1000,
            tokens_after: 1000,
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
            tokens_before: 1000,
            tokens_after: 200,
            strategy: "naive",
        },
    );

    let mut pre_content = String::new();
    let mut post_content = String::new();
    for _ in 0..40 {
        if let Ok(c) = std::fs::read_to_string(&pre_marker) {
            pre_content = c;
        }
        if let Ok(c) = std::fs::read_to_string(&post_marker) {
            post_content = c;
        }
        if !pre_content.is_empty() && !post_content.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

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

#[tokio::test]
async fn test_find_without_destructive_flags_auto_approved() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "found!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "find . -name '*.rs' -type f"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
    let approval_handle = tokio::spawn(async move {
        let res =
            tokio::time::timeout(std::time::Duration::from_millis(100), approval_rx.recv()).await;
        assert!(
            res.is_err() || res.unwrap().is_none(),
            "non-destructive find should not ask for approval"
        );
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("search files", &approval_tx, never_cancelled())
        .await
        .unwrap();

    approval_handle.await.unwrap();

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "found!")));
}

#[tokio::test]
async fn test_find_delete_requires_approval() {
    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "deleted!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "find . -name '*.tmp' -delete"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
    let approval_handle = tokio::spawn(async move {
        let req: ApprovalRequest = approval_rx.recv().await.unwrap();
        assert_eq!(req.tool_name, "bash");
        let _ = req.response.send(ApprovalResponse::Approved);
    });

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("delete temp files", &approval_tx, never_cancelled())
        .await
        .unwrap();

    approval_handle.await.unwrap();

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "deleted!")));
}

#[tokio::test]
async fn test_glob_base_dir_outside_sandbox_denied() {
    let temp = std::env::temp_dir();
    let sandbox = temp.join(format!("kf-sandbox-{}", std::process::id()));
    std::fs::create_dir_all(&sandbox).unwrap();
    let outside = temp.join(format!("kf-outside-{}", std::process::id()));

    let tool = MockTool {
        def: ToolDef {
            name: "glob",
            description: "list files",
            parameters: serde_json::json!({"type": "object", "properties": {"base_dir": {"type": "string"}, "pattern": {"type": "string"}}}),
        },
        captured_args: Arc::new(Mutex::new(None)),
        outcome: ToolOutcome::Success {
            content: "listed!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "glob".into(),
                arguments: serde_json::json!({"base_dir": outside.to_string_lossy(), "pattern": "*.rs"}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    );

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(false);
    config.sandbox_dir = Some(sandbox.to_string_lossy().to_string());
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("list outside sandbox", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "glob" && output.contains("Access denied")));
    assert!(denied, "glob outside sandbox should be denied");

    remove_test_dir(&sandbox);
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
    config.max_tool_calls_per_turn = 3;
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
        assert_eq!(cfg.permission_rules.len(), 1);
        assert_eq!(cfg.permission_rules[0].action, PermissionAction::Allow);
    }

    // Second turn: same command should now match the rule and run
    // without sending an approval request.
    let (approval_tx2, mut approval_rx2) = mpsc::unbounded_channel();
    let requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let requested_flag = requested.clone();
    let no_approval_handle = tokio::spawn(async move {
        if approval_rx2.recv().await.is_some() {
            requested_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    // Second turn: same command should now match the rule and run
    // without sending an approval request. The timeout is generous
    // because this test suite is heavily parallel and the goal is only
    // to detect an infinite hang caused by a misplaced approval prompt.
    let second_events = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        exe.run_turn_collecting("run tests again", &approval_tx2, never_cancelled()),
    )
    .await
    .expect("second turn should complete without approval prompt");

    no_approval_handle.abort();
    assert!(
        !requested.load(std::sync::atomic::Ordering::SeqCst),
        "rule should prevent second approval request"
    );

    let second_events = second_events.unwrap();
    let has_result = second_events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "bash" && output == "ran!"));
    assert!(has_result, "second turn should execute the allowed command");
}

/// Tool that sleeps for a fixed duration before returning. Used to exercise
/// cancellation mid-batch.
struct SleepingTool {
    def: ToolDef,
    sleep_ms: u64,
    call_count: Arc<Mutex<usize>>,
}

#[async_trait::async_trait]
impl Tool for SleepingTool {
    fn def(&self) -> ToolDef {
        self.def.clone()
    }

    async fn run(&self, _ctx: &ToolContext, _args: serde_json::Value) -> ToolOutcome {
        *self.call_count.lock().unwrap() += 1;
        tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
        ToolOutcome::Success {
            content: "done".into(),
        }
    }
}

/// Cancellation during a multi-tool batch must append placeholder results
/// for any tool calls that were skipped, so the conversation stays balanced.
#[tokio::test]
async fn test_cancelled_tool_batch_appends_placeholders() {
    let tool = SleepingTool {
        def: ToolDef {
            name: "sleep",
            description: "sleep",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        },
        sleep_ms: 200,
        call_count: Arc::new(Mutex::new(0)),
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
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
