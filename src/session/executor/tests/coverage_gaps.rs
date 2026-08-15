// WO 28.9 — coverage gap fillers for the executor loop. Each test pins a
// behavior the spec (R2.1/R2.2/R2.4) names that had no prior executable
// check. Reuses the shared `MockAdapter` / `make_executor` helpers.

use super::super::*;
use super::common::*;
use crate::adapters::ModelAdapter;
use crate::shared::{FinishReason, Message, ModelInfo, Role, StreamEvent, ToolDef, ToolOutcome};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

// ── R2.1 — system-prompt assembly reaches the adapter ──────────────────
//
// The wiremock integration test proves a turn drains; it does NOT prove the
// executor assembles a system prompt and forwards it as the first message.
// This test installs a capturing adapter that records every message passed
// to `stream`, then asserts the conversation delivered to the model starts
// with a `Role::System` message built by the `PromptBuilder`.

struct CapturingAdapter {
    info: ModelInfo,
    seen_messages: Arc<Mutex<Vec<Message>>>,
}

#[async_trait::async_trait]
impl ModelAdapter for CapturingAdapter {
    fn model_info(&self) -> ModelInfo {
        self.info.clone()
    }

    async fn stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDef],
    ) -> anyhow::Result<mpsc::Receiver<StreamEvent>> {
        *self.seen_messages.lock().unwrap() = messages.to_vec();
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let _ = tx.send(StreamEvent::Text("ok".to_string())).await;
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

#[tokio::test]
async fn turn_start_assembles_system_prompt() {
    let seen: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(Vec::new()));
    let adapter = CapturingAdapter {
        info: make_info(),
        seen_messages: seen.clone(),
    };
    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();

    exe.run_turn_collecting("hello", &approval_tx, never_cancelled())
        .await
        .unwrap();

    let messages = seen.lock().unwrap().clone();
    assert!(
        !messages.is_empty(),
        "adapter must receive at least the assembled prompt"
    );
    let first = &messages[0];
    assert_eq!(
        first.role,
        crate::shared::Role::System,
        "first message delivered to the adapter must be the assembled system prompt"
    );
    assert!(
        !first.content.is_empty(),
        "system prompt content must be non-empty"
    );
    // The trailing user turn must be delivered too — proves the executor
    // appends the live input after the stable stem.
    assert!(
        messages
            .iter()
            .any(|m| m.role == crate::shared::Role::User && m.content == "hello"),
        "assembled prompt must include the trailing user turn"
    );
}

// ── R2.2 — schema validation routes through dispatch ───────────────────
//
// `validate_args_against_schema` is unit-tested in `helpers/mod.rs`, but no
// test drives it through `run_turn_collecting`. The pre-run gate must skip a
// tool call whose arguments disagree with the declared schema and emit an
// "Invalid arguments" ToolResult without invoking the tool body.

#[tokio::test]
async fn tool_call_dispatch_validates_params_and_routes() {
    let captured: Arc<Mutex<Option<serde_json::Value>>> = Arc::new(Mutex::new(None));
    let tool = MockTool {
        def: ToolDef {
            name: "echo",
            description: "echo a value",
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"val": {"type": "string"}},
                "required": ["val"],
            }),
        },
        captured_args: captured.clone(),
        outcome: ToolOutcome::Success {
            content: "echoed!".into(),
        },
    };

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::ToolCall(ToolInvocation {
                id: "call-1".into(),
                name: "echo".into(),
                // Wrong type: `val` declared as string, model passed integer.
                arguments: serde_json::json!({"val": 42}),
            }),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                usage: None,
            },
        ],
        make_info(),
    )
    .with_followup_events(vec![
        StreamEvent::Text("done".to_string()),
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: None,
        },
    ]);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe =
        make_executor(Box::new(adapter), vec![Arc::new(tool)], make_config(true)).unwrap();

    let events = exe
        .run_turn_collecting("use echo badly", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // The tool body must NOT have run — captured args stay None.
    assert!(
        captured.lock().unwrap().is_none(),
        "invalid-args tool call must be skipped before the tool body runs"
    );

    // The skip must surface as a failing ToolResult naming the validation error.
    let rejected = events.iter().any(|e| {
        matches!(
            e,
            TurnEvent::ToolResult { name, output, success: false }
                if name == "echo" && output.contains("Invalid arguments")
        )
    });
    assert!(
        rejected,
        "expected an 'Invalid arguments' ToolResult for the bad call, got events: {events:?}"
    );
}

// ── R2.4 — max-continuation cap aborts the turn ────────────────────────
//
// When the model repeatedly returns `FinishReason::Length`, the executor
// appends a "continue" user message and re-streams — up to
// `max_continuation_rounds`. Past the cap, it must emit a single
// `TurnEvent::Error` naming the cap and return. WO 23.9.

#[tokio::test]
async fn max_continuation_cap_aborts_after_limit() {
    // Both the first and followup streams end with Length so the loop is
    // forced into the continuation path every iteration.
    let length_stream = vec![
        StreamEvent::Text("truncated...".to_string()),
        StreamEvent::Done {
            finish_reason: FinishReason::Length,
            usage: None,
        },
    ];
    let adapter =
        MockAdapter::new(length_stream.clone(), make_info()).with_followup_events(length_stream);

    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut config = make_config(true);
    config.tools.max_continuation_rounds = 1;
    let mut exe = make_executor(Box::new(adapter), vec![], config).unwrap();

    let events = exe
        .run_turn_collecting("long answer please", &approval_tx, never_cancelled())
        .await
        .unwrap();

    // ContinuationRound telemetry must fire as the cap is approached.
    let continuation_events: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ContinuationRound { round, .. } => Some(*round),
            _ => None,
        })
        .collect();
    assert!(
        !continuation_events.is_empty(),
        "expected at least one ContinuationRound event, got: {events:?}"
    );

    // The cap must abort the turn with exactly one terminal error.
    let cap_errors: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Error(msg) if msg.contains("Max continuation rounds reached") => Some(msg),
            _ => None,
        })
        .collect();
    assert_eq!(
        cap_errors.len(),
        1,
        "expected exactly one 'Max continuation rounds reached' error, got: {cap_errors:?}"
    );
    // The cap value must appear in the message — pins the configured limit.
    assert!(
        cap_errors[0].contains("(1)"),
        "cap message should name the configured limit (1), got: {}",
        cap_errors[0]
    );
}

// ── R4.5 — turn resumes correctly after compaction ─────────────────────
//
// After a compaction rewrites the conversation log (via
// `replace_all_async`), the next turn must still drain cleanly and
// append a new assistant turn. This pins that the executor's turn
// path is not poisoned by a mid-session compaction: the conversation
// log is still writable and the adapter still receives a complete
// message list. Uses the shared `MockAdapter` (no live LLM).

#[tokio::test]
async fn turn_resumes_correctly_after_compaction() {
    use crate::session::prompt::compact_to_budget;

    let adapter = MockAdapter::new(
        vec![
            StreamEvent::Text("resumed after compaction".to_string()),
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage: None,
            },
        ],
        make_info(),
    );
    let (approval_tx, _approval_rx) = mpsc::unbounded_channel();
    let mut exe = make_executor(Box::new(adapter), vec![], make_config(false)).unwrap();

    // Seed a long conversation that would trigger compaction.
    exe.conversation
        .append_async(Message {
            role: Role::System,
            content: "anchor".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    for i in 0..20 {
        exe.conversation
            .append_async(Message {
                role: Role::User,
                content: format!("question {i}"),
                ..Default::default()
            })
            .await
            .unwrap();
        exe.conversation
            .append_async(Message {
                role: Role::Assistant,
                content: "x".repeat(2000),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let pre_count = exe.conversation_log().all().len();
    assert!(pre_count > 10, "fixture must be non-trivial: {pre_count}");

    // Run a compaction in-place: compact the current history and swap it.
    let history = exe.conversation_log().all().to_vec();
    let result = compact_to_budget(&history, 2, Some(1000));
    assert!(
        result.tokens_after < result.tokens_before,
        "compaction must reduce tokens before the turn resumes"
    );
    exe.conversation
        .replace_all_async(result.new_messages.clone())
        .await
        .unwrap();
    // Naive compaction replaces content (stub/condense) but does not
    // delete slots, so the count stays equal — verify the token count
    // dropped instead (the real compaction signal).
    let compacted_count = exe.conversation_log().all().len();
    assert_eq!(
        compacted_count, pre_count,
        "naive compaction preserves slot count (replacement, not deletion)"
    );

    // The turn after compaction must drain and append a new assistant turn.
    let events = exe
        .run_turn_collecting("continue the work", &approval_tx, never_cancelled())
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::Token(t) if t.contains("resumed after compaction"))),
        "post-compaction turn must produce the assistant text, got: {events:?}"
    );

    // The conversation log must have grown (user turn + assistant turn).
    let post_count = exe.conversation_log().all().len();
    assert!(
        post_count > compacted_count,
        "post-compaction turn must append to the log: {compacted_count} -> {post_count}"
    );
    // The new user turn must be in the log.
    assert!(
        exe.conversation_log()
            .all()
            .iter()
            .any(|m| m.role == Role::User && m.content == "continue the work"),
        "the new user turn must be persisted after compaction"
    );
}
