// Timeout scenarios for approval and hook gating.
// Split from the former single-file approval.rs (WO 19.3). Pure refactor:
// test bodies are moved verbatim.

#[cfg(unix)]
use super::super::super::*;
#[cfg(unix)]
use super::super::common::*;
#[cfg(unix)]
use crate::shared::{FinishReason, StreamEvent, ToolDef, ToolOutcome};
#[cfg(unix)]
use std::sync::Mutex;

#[cfg(unix)]
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
    config.tools.hooks_dir = Some(hooks_dir);
    let mut exe = make_executor(Box::new(adapter), vec![Arc::new(tool)], config).unwrap();

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
