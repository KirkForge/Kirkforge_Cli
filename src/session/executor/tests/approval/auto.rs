// Auto-approve, read-only classification, and plan-mode-allow scenarios.
// Split from the former single-file approval.rs (WO 19.3). Pure refactor:
// test bodies are moved verbatim.

use super::super::super::helpers::is_read_only_bash;
use super::super::super::*;
use super::super::common::*;
use crate::shared::permission::PermissionAction;
use crate::shared::{FinishReason, StreamEvent, ToolDef, ToolOutcome};
use std::sync::Mutex;

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

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("run command", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // WO 19.9: yield + try_recv instead of timeout — no wall-clock
    // delay needed, the turn is already complete.
    tokio::task::yield_now().await;
    assert!(
        approval_rx.try_recv().is_err(),
        "read-only bash should not ask for approval"
    );

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "ran!")));
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
    assert!(config.security.permission_rules.is_empty());
    assert!(!config.security.auto_approve);

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let _events = exe
        .run_turn_collecting("run tests", &approval_tx, never_cancelled())
        .await
        .unwrap();
    approval_handle.await.unwrap();

    {
        let cfg = cfg(&exe);
        assert_eq!(
            cfg.security.permission_rules.len(),
            1,
            "AlwaysApprove should have appended exactly one rule, got {:?}",
            cfg.security.permission_rules
        );
        let r = &cfg.security.permission_rules[0];
        assert_eq!(r.tool, "bash");
        assert_eq!(r.key, "command");
        assert_eq!(r.pattern, "cargo test --release");
        assert_eq!(r.action, PermissionAction::Allow);
    }

    assert!(
        !cfg(&exe).security.auto_approve,
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
        .security
        .permission_rules
        .push(crate::shared::permission::PermissionRule {
            tool: "bash".into(),
            key: "command".into(),
            pattern: "ls".into(),
            action: PermissionAction::Allow,
        });
    assert_eq!(config.security.permission_rules.len(), 1);

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config);
    let _events = exe
        .run_turn_collecting("list", &approval_tx, never_cancelled())
        .await
        .unwrap();
    drop(approval_tx);
    approval_handle.await.unwrap();

    assert_eq!(
        cfg(&exe).security.permission_rules.len(),
        1,
        "AlwaysApprove should dedup against an existing identical rule"
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

    let mut config = make_config(true);
    config
        .security
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

    // WO 19.9: yield + try_recv instead of timeout.
    tokio::task::yield_now().await;
    assert!(
        approval_rx.try_recv().is_err(),
        "Explicit allow rule should be honored under auto_approve; no approval prompt expected"
    );

    let result = events.iter().find_map(|e| match e {
        TurnEvent::ToolResult { name, output, .. } => Some((name.as_str(), output.as_str())),
        _ => None,
    });
    assert_eq!(result, Some(("bash", "built!")));
}

/// Once the existing file has been read, `write_file` may overwrite it.
#[tokio::test]
async fn test_write_file_overwrite_allowed_after_read() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_write_gate_read_test_{}.txt",
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
    // Mark as read first — the gate now permits the overwrite.
    exe.sandbox.mark_read(&tmp);

    let events = exe
        .run_turn_collecting("overwrite", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "write_file should run when the existing file was read first"
    );
    let ran = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, success, .. } if name == "write_file" && *success
        )
    });
    assert!(
        ran,
        "Expected write_file to succeed, got events: {events:?}"
    );
}

/// A brand-new file has never existed, so read-before-edit does not
/// apply — `write_file` creates it without a prior read.
#[tokio::test]
async fn test_write_file_new_file_allowed_without_read() {
    let tmp = std::env::temp_dir().join(format!(
        "kf_code_write_gate_new_test_{}.txt",
        std::process::id()
    ));
    // Ensure it does NOT exist going in; clean up after.
    let _ = std::fs::remove_file(&tmp);
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
                    "content": "brand new"
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
        .run_turn_collecting("create", &approval_tx, never_cancelled())
        .await
        .unwrap();

    assert!(
        captured.lock().unwrap().is_some(),
        "write_file should run for a brand-new file with no prior read"
    );
    let ran = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, success, .. } if name == "write_file" && *success
        )
    });
    assert!(
        ran,
        "Expected write_file to succeed, got events: {events:?}"
    );
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

    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(false));
    let events = exe
        .run_turn_collecting("search files", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // WO 19.9: yield + try_recv instead of timeout.
    tokio::task::yield_now().await;
    assert!(
        approval_rx.try_recv().is_err(),
        "non-destructive find should not ask for approval"
    );

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
    assert!(!is_read_only_bash("find . -execdir rm {} \\;"));
    assert!(!is_read_only_bash("find . -ok rm {} \\;"));
    assert!(!is_read_only_bash("find . -okdir rm {} \\;"));
    assert!(!is_read_only_bash("find . -fprint out.txt"));
    assert!(!is_read_only_bash("find . -fls out.txt"));
    // Destructive find must not slip through as a later pipe segment.
    assert!(!is_read_only_bash("cat list | find . -delete"));
    assert!(!is_read_only_bash("cat list | find . -exec rm {} \\;"));
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
    // Redirections in later pipe segments must also be blocked.
    assert!(!is_read_only_bash("cat file | sort > out.txt"));
    assert!(!is_read_only_bash("cat file | grep foo >> log.txt"));
}

#[test]
fn test_is_read_only_bash_chaining_blocked() {
    assert!(!is_read_only_bash("ls && rm -rf /"));
    assert!(!is_read_only_bash("cat file; rm file"));
    assert!(!is_read_only_bash("ls || true"));
    // Chaining in later pipe segments must also be blocked.
    assert!(!is_read_only_bash("cat file | sort; rm file"));
    assert!(!is_read_only_bash("cat file | sort && rm file"));
    assert!(!is_read_only_bash("cat file | sort || rm file"));
}

#[test]
fn test_is_read_only_bash_substitution_blocked() {
    assert!(!is_read_only_bash("echo $(rm -rf /)"));
    assert!(!is_read_only_bash("echo `ls`"));
    // Command substitution in later pipe segments must also be blocked.
    assert!(!is_read_only_bash("cat file | sort $(rm -rf /)"));
    assert!(!is_read_only_bash("cat file | sort `ls`"));
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

#[test]
fn test_is_read_only_bash_git_read_only_subcommands_allowed() {
    assert!(is_read_only_bash("git status"));
    assert!(is_read_only_bash("git status -s"));
    assert!(is_read_only_bash("git log --oneline -5"));
    assert!(is_read_only_bash("git diff HEAD~1"));
    assert!(is_read_only_bash("git show HEAD:src/main.rs"));
    assert!(is_read_only_bash("git --no-pager -C /some/repo log"));
    assert!(is_read_only_bash("git ls-files | grep foo"));
}

#[test]
fn test_is_read_only_bash_git_mutating_subcommands_blocked() {
    assert!(!is_read_only_bash("git add src/main.rs"));
    assert!(!is_read_only_bash("git commit -m 'wip'"));
    assert!(!is_read_only_bash("git push origin main"));
    assert!(!is_read_only_bash("git checkout -b feature"));
    assert!(!is_read_only_bash("git reset --hard HEAD"));
    assert!(!is_read_only_bash("git merge feature"));
    assert!(!is_read_only_bash("git rebase main"));
    assert!(!is_read_only_bash("git stash"));
    assert!(!is_read_only_bash("git"));
    // Mutating git must not slip through as a later pipe segment.
    assert!(!is_read_only_bash("cat list | git add src/main.rs"));
    assert!(!is_read_only_bash("cat list | git commit -m 'wip'"));
}

#[tokio::test]
async fn test_plan_mode_allows_read_file() {
    let tmp =
        std::env::temp_dir().join(format!("kf_code_plan_read_test_{}.txt", std::process::id()));
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

#[cfg(unix)]
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
    config.tools.hooks_dir = Some(hooks_dir);
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
