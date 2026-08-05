// Deny, reject, block, and plan-mode-block scenarios.
// Split from the former single-file approval.rs (WO 19.3). Pure refactor:
// test bodies are moved verbatim.

use super::super::super::*;
use super::super::common::*;
use crate::shared::permission::PermissionAction;
use crate::shared::test_util::remove_test_dir;
use crate::shared::{FinishReason, StreamEvent, ToolDef, ToolOutcome};
use std::sync::Mutex;

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
        .security
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
        assert_eq!(cfg.security.permission_rules.len(), 1);
        assert_eq!(
            cfg.security.permission_rules[0].action,
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
        .security
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
    config.security.deny_paths = vec!["secret/**".into()];

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
        .security
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

/// `write_file` overwriting an existing file must respect the
/// read-before-edit gate — without this it could blindly clobber a
/// file the model never inspected (review.md High finding). Here the
/// target exists but was never `read_file`d, so the tool must not run.
#[tokio::test]
async fn test_write_file_overwrite_blocked_without_read() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_write_gate_test_{}.txt",
        std::process::id()
    ));
    std::fs::write(&tmp, "original").expect("seed existing file");
    let _cleanup = CleanupFile(tmp.clone());

    let captured = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "write_file",
            description: "write to a file",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}, "content": {"type": "string"}}
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
                    "path": tmp.to_string_lossy(),
                    "content": "overwritten"
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
    let events = exe
        .run_turn_collecting("overwrite", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_none(),
        "write_file must not run when overwriting an unread existing file"
    );
    let denied = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, .. }
                if name == "write_file" && output.contains("Read-before-edit")
        )
    });
    assert!(
        denied,
        "Expected a read-before-edit refusal, got events: {events:?}"
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

    let mut config = make_config(true);
    config
        .security
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

    // WO 19.9: yield + try_recv instead of timeout.
    tokio::task::yield_now().await;
    assert!(
        approval_rx.try_recv().is_err(),
        "dangerous command should be blocked by the safety gate, not by an approval prompt"
    );

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

#[cfg(unix)]
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
    config.tools.hooks_dir = Some(hooks_dir);
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
    config.security.sandbox_dir = Some(sandbox.to_string_lossy().to_string());
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let events = exe
        .run_turn_collecting("list outside sandbox", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let denied = events.iter().any(|e| matches!(e, TurnEvent::ToolResult { name, output, .. } if name == "glob" && output.contains("Access denied")));
    assert!(denied, "glob outside sandbox should be denied");

    remove_test_dir(&sandbox);
}
