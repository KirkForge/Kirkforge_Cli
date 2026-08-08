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

/// Render a tool error as a `Role::Tool` content string, with a structured
/// `ErrorHint` appended when the classifier recognises the error.
///
/// The hint is appended as a single line prefixed with "Hint:". When no
/// classifier matches, the result is just the original error text (with the
/// `Error: ` prefix that the caller provided). The function is pure: it
/// does not touch the conversation log or any event channel.
fn render_tool_error_with_hint(tool_name: &str, message: &str, args: &serde_json::Value) -> String {
    let mut out = format!("Error: {message}");
    if let Some(hint) = crate::session::error_recovery::classify_for_tool(tool_name, message, args)
    {
        out.push('\n');
        out.push_str(&crate::session::error_recovery::render_hint(&hint));
    }
    out
}

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
            // The rendered diff from the edit_file tool is passed to
            // the correction loop so downstream verifiers see the
            // real unified diff rather than the user's `old_string`.
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
            let output = render_tool_error_with_hint(&tc.name, &message, &tc.arguments);
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: output.clone(),
                        success: false,
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

            // Attempt error recovery — analyze the error and inject a hint
            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = Message {
                    role: Role::User,
                    content: format!(
                        "The previous action failed: {}\n\n{}\n\nPlease correct the issue and try again. \
                         Do NOT repeat the same failing command — use the suggestions above.",
                        hint.error_summary, hint.suggestion
                    ),
                    ..Default::default()
                };
                conversation.append(recovery_msg)?;
            }
        }
        ToolOutcome::Failure(err) => {
            let message = err.to_user_message();
            let output = render_tool_error_with_hint(&tc.name, &message, &tc.arguments);
            crate::send_or_warn!(
                event_tx
                    .send(TurnEvent::ToolResult {
                        name: tc.name.clone(),
                        output: output.clone(),
                        success: false,
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

            if let Some(hint) =
                crate::session::error_recovery::analyze_error(&tc.name, &message, &tc.arguments)
            {
                let recovery_msg = Message {
                    role: Role::User,
                    content: format!(
                        "The previous action failed: {}\n\n{}\n\nPlease correct the issue and try again. \
                         Do NOT repeat the same failing command — use the suggestions above.",
                        hint.error_summary, hint.suggestion
                    ),
                    ..Default::default()
                };
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
                    file: cr.file.clone(),
                    line: cr.line,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_tool_error_with_hint_appends_hint_for_bash_borrow_conflict() {
        let args = serde_json::json!({"command": "cargo build"});
        let out = render_tool_error_with_hint(
            "bash",
            "error: cannot borrow `x` as mutable because it is also borrowed as `y`",
            &args,
        );
        assert!(out.starts_with("Error: "));
        assert!(out.contains("Hint:"));
        assert!(out.contains("borrow conflict"));
    }

    #[test]
    fn render_tool_error_with_hint_appends_hint_for_missing_import() {
        let args = serde_json::json!({"command": "cargo build"});
        let out = render_tool_error_with_hint(
            "bash",
            "error: cannot find value `frobnicate` in this scope",
            &args,
        );
        assert!(out.contains("Hint:"));
        assert!(out.contains("`frobnicate`"));
    }

    #[test]
    fn render_tool_error_with_hint_passthrough_for_unrelated_text() {
        let args = serde_json::json!({"command": "rm"});
        let out = render_tool_error_with_hint("bash", "Permission denied: /etc/foo", &args);
        assert_eq!(out, "Error: Permission denied: /etc/foo");
    }

    #[test]
    fn render_tool_error_with_hint_does_not_classify_for_non_shell_tools() {
        // `classify_for_tool` only runs the classifier for shell-style
        // tools. Other tools (e.g. read_file) get the raw error only.
        let args = serde_json::json!({"path": "src/main.rs"});
        let out =
            render_tool_error_with_hint("read_file", "cannot find value `x` in this scope", &args);
        assert!(!out.contains("Hint:"));
    }

    #[test]
    fn format_grep_output_empty_matches_lists_no_lines() {
        let out = format_grep_output(std::path::Path::new("src/lib.rs"), &[]);
        assert!(out.starts_with("Matches in "));
        assert!(out.contains("src/lib.rs"));
        // Header line + trailing newline only; no match bodies.
        assert_eq!(out.matches('>').count(), 0);
    }

    #[test]
    fn format_grep_output_renders_match_line_and_context() {
        let m = crate::shared::Match {
            line_number: 42,
            line: "let x = 1;".into(),
            context_before: vec!["fn foo() {".into()],
            context_after: vec!["}".into()],
        };
        let out = format_grep_output(std::path::Path::new("src/foo.rs"), &[m]);
        assert!(out.contains("Matches in src/foo.rs"));
        // context-before is indented two spaces; match line is `>42: ...`.
        assert!(out.contains("  fn foo() {"));
        assert!(out.contains(">42: let x = 1;"));
        assert!(out.contains("  }"));
    }

    #[test]
    fn format_grep_output_renders_multiple_matches_separated_by_blank() {
        let matches = vec![
            crate::shared::Match {
                line_number: 1,
                line: "a".into(),
                context_before: vec![],
                context_after: vec![],
            },
            crate::shared::Match {
                line_number: 2,
                line: "b".into(),
                context_before: vec![],
                context_after: vec![],
            },
        ];
        let out = format_grep_output(std::path::Path::new("/x"), &matches);
        assert!(out.contains(">1: a"));
        assert!(out.contains(">2: b"));
        // Two matches → at least one blank-line separator between them.
        assert!(out.contains("\n\n"));
    }
}
