//! Tool-outcome processing for the executor.
//!
//! Extracted from `mod.rs`: materialises a `ToolOutcome` into a
//! `Role::Tool` conversation message + `TurnEvent`, formats grep
//! output, and emits verifier correction results.

use crate::session::conversation::ConversationLog;
use crate::session::verifier::CorrectionResult;
use crate::shared::{Message, Role, ToolInvocation, ToolOutcome};
use tokio::sync::mpsc;

use crate::session::executor::TurnEvent;

pub(crate) async fn handle_tool_outcome(
    outcome: ToolOutcome,
    tc: &ToolInvocation,
    event_tx: &mpsc::Sender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<Option<String>> {
    match outcome {
        ToolOutcome::Success { content } => {
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: content.clone(),
                        success: true,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
        }
        ToolOutcome::FileContent { content, .. } => {
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: content.clone(),
                        success: true,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
        }
        ToolOutcome::FileEdit { diff, .. } => {
            // Hand the rendered diff to the caller so the
            // BusEvent::Edit event downstream carries the real
            // diff text — see the docstring on this fn.
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: diff.clone(),
                        success: true,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: diff.clone(),
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
            return Ok(Some(diff));
        }
        ToolOutcome::GrepMatches {
            path,
            matches,
            total: _,
        } => {
            let output = format_grep_output(&path, &matches);
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: output.clone(),
                        success: true,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: output,
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
        }
        ToolOutcome::Error { message } => {
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!("Error: {message}"),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: format!("Error: {message}"),
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;

            // Attempt error recovery — analyze the error and inject a hint
            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = crate::session::error_recovery::build_recovery_message(&hint);
                conversation.append(recovery_msg)?;
            }
        }
        ToolOutcome::Failure(err) => {
            let message = err.to_user_message();
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: format!("Error: {message}"),
                        success: false,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: format!("Error: {message}"),
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;

            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = crate::session::error_recovery::build_recovery_message(&hint);
                conversation.append(recovery_msg)?;
            }
        }
        // `read_image` returns an Image outcome. We materialise it as
        // a `Role::Tool` message with `content_parts: [Image{…}]` set
        // and a short `content` projection that keeps the conversation
        // log human-readable. The PromptBuilder's image-attach step
        // (see `src/session/prompt/mod.rs`) splices the image onto the
        // next user turn so the model actually sees it inline.
        ToolOutcome::Image {
            path,
            mime,
            data_base64,
        } => {
            let projection = format!(
                "[image: {} ({}, {} bytes)]",
                path.display(),
                mime,
                data_base64.len()
            );
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: projection.clone(),
                        success: true,
                    })
                    .await,
                "TurnEvent receiver dropped; discarding event"
            );
            conversation
                .append_async(Message {
                    role: Role::Tool,
                    content: projection,
                    content_parts: Some(vec![crate::shared::ContentPart::Image {
                        data_base64,
                        mime,
                    }]),
                    tool_call_id: Some(tc.id.clone()),
                    tool_name: Some(tc.name.clone()),
                    ..Default::default()
                })
                .await?;
        }
    }
    Ok(None)
}

fn format_grep_output(path: &std::path::Path, matches: &[crate::shared::Match]) -> String {
    let mut out = format!("Matches in {}:\n", path.display());
    for m in matches {
        for ctx in &m.context_before {
            out.push_str(&format!("  {ctx}\n"));
        }
        out.push_str(&format!(">{}: {}\n", m.line_number, m.line));
        for ctx in &m.context_after {
            out.push_str(&format!("  {ctx}\n"));
        }
        out.push('\n');
    }
    out
}

pub(crate) async fn emit_correction_results(
    results: Vec<CorrectionResult>,
    tc: &ToolInvocation,
    event_tx: &mpsc::Sender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<()> {
    for cr in &results {
        crate::send_or_warn!(
            event_tx
                .send(TurnEvent::Verification {
                    message: cr.message.clone(),
                    success: cr.success,
                })
                .await,
            "TurnEvent receiver dropped; discarding event"
        );
        conversation.append(Message {
            role: Role::Tool,
            content: cr.message.clone(),
            tool_call_id: Some(tc.id.clone()),
            tool_name: Some(format!("verifier:{}", cr.verifier)),
            ..Default::default()
        })?;
    }
    Ok(())
}
