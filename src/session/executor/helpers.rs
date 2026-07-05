//! Free helper functions used by the executor.

use crate::session::access::{DenyList, GuardVerdict, PathGuard};
use crate::session::conversation::ConversationLog;
use crate::session::verifier::CorrectionResult;
use crate::shared::metrics::{record, MetricEvent};
use crate::shared::{Message, Role, ToolInvocation, ToolOutcome};
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::mpsc;

use super::TurnEvent;

pub(crate) fn tool_outcome_success(outcome: &ToolOutcome) -> bool {
    !matches!(
        outcome,
        ToolOutcome::Error { .. } | ToolOutcome::Failure { .. }
    )
}

pub(crate) fn tool_error_kind(outcome: &ToolOutcome) -> Option<&'static str> {
    match outcome {
        ToolOutcome::Error { .. } => Some("error"),
        ToolOutcome::Failure(err) => match err {
            crate::shared::ToolError::InvalidArgs { .. } => Some("invalid_args"),
            crate::shared::ToolError::AccessDenied { .. } => Some("access_denied"),
            crate::shared::ToolError::Execution { .. } => Some("execution"),
            crate::shared::ToolError::Timeout { .. } => Some("timeout"),
            crate::shared::ToolError::Cancelled => Some("cancelled"),
            crate::shared::ToolError::Internal { .. } => Some("internal"),
        },
        _ => None,
    }
}

pub(crate) fn record_turn_metric(
    model: &str,
    start: Instant,
    tool_calls: usize,
    finish_reason: &crate::shared::FinishReason,
) {
    record(MetricEvent::Turn {
        model: model.to_string(),
        duration_ms: start.elapsed().as_millis() as u64,
        tool_calls,
        finish_reason: format!("{finish_reason:?}").to_lowercase(),
    });
}
pub(crate) fn tool_cancel_token(
    cancelled: &std::sync::atomic::AtomicBool,
) -> tokio_util::sync::CancellationToken {
    let token = tokio_util::sync::CancellationToken::new();
    if cancelled.load(Ordering::SeqCst) {
        token.cancel();
    }
    token
}

const READ_ONLY_COMMANDS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "which", "type", "file", "stat", "du",
    "df", "env", "printenv", "true", "false", "dirname", "basename", "realpath", "readlink",
    "grep", "rg", "sort", "wc", "cut", "tr", "uniq", "fold", "nl", "diff", "cmp", "comm", "jq",
    "date", "cal", "whoami", "id", "uname", "hostname", "uptime", "ps", "free", "lscpu", "lsblk",
    "lsof", "dmesg", "nproc", "arch", "tty", "jobs", "help", "find",
];

pub(crate) fn is_read_only_bash(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }

    let trimmed_stripped = trimmed.trim_start();
    let first = match trimmed_stripped.find(|c: char| c.is_whitespace()) {
        Some(pos) => &trimmed_stripped[..pos],
        None => trimmed_stripped,
    };

    if !READ_ONLY_COMMANDS.contains(&first) {
        return false;
    }

    // `find` is read-only for discovery, but several flags mutate the
    // filesystem. Require approval for any find command that looks
    // destructive.
    if first == "find" {
        let lowered = trimmed.to_lowercase();
        for flag in [" -delete", " -exec", " -ok", " -fprint", " -fls"] {
            if lowered.contains(flag) {
                return false;
            }
        }
    }

    let rest = &trimmed[first.len()..];

    if rest.contains('>') {
        return false;
    }

    // Every pipe segment must itself be a read-only command. The first
    // segment's command is already validated above; this catches a
    // read-only producer piped into a writing consumer — e.g.
    // `cat list | xargs rm`, `… | tee /etc/file`, `… | sh`. Without this,
    // such a pipeline would be auto-approved despite mutating state.
    for segment in trimmed.split('|') {
        let seg = segment.trim();
        if let Some(word) = seg.split_whitespace().next() {
            if !READ_ONLY_COMMANDS.contains(&word) {
                return false;
            }
        }
    }

    if rest.contains(';') || rest.contains("&&") || rest.contains("||") {
        return false;
    }

    if rest.contains("$(") || rest.contains('`') {
        return false;
    }

    true
}

pub(crate) fn truncate_tool_output(outcome: ToolOutcome, max_chars: usize) -> ToolOutcome {
    if max_chars == 0 {
        return outcome;
    }
    match outcome {
        ToolOutcome::Success { content } => {
            if content.len() > max_chars {
                let mut boundary = max_chars;
                while !content.is_char_boundary(boundary) {
                    boundary -= 1;
                }
                let truncated = format!(
                    "{}...\n[output truncated to {} chars]",
                    &content[..boundary],
                    max_chars
                );
                ToolOutcome::Success { content: truncated }
            } else {
                ToolOutcome::Success { content }
            }
        }
        ToolOutcome::Error { message } => ToolOutcome::Error { message },
        ToolOutcome::Failure(err) => ToolOutcome::Failure(err.clone()),
        other => other,
    }
}

pub(crate) fn extract_bash_metrics(
    outcome: &ToolOutcome,
) -> (Option<i32>, Option<usize>, Option<usize>) {
    match outcome {
        ToolOutcome::Success { content } => (Some(0), Some(content.len()), Some(0)),
        ToolOutcome::Error { message } => {
            let exit_code = if message.contains("exited with code") {
                message
                    .split("exited with code ")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<i32>().ok())
            } else {
                Some(1)
            };
            let (stdout_len, stderr_len) = message
                .find("\nstderr:\n")
                .map(|pos| (pos, message[pos + 9..].len()))
                .unwrap_or((message.len(), 0));
            (exit_code, Some(stdout_len), Some(stderr_len))
        }
        ToolOutcome::Failure(crate::shared::ToolError::Execution {
            exit_code, stderr, ..
        }) => (*exit_code, Some(0), Some(stderr.len())),
        ToolOutcome::Failure(crate::shared::ToolError::Timeout { .. })
        | ToolOutcome::Failure(crate::shared::ToolError::Cancelled) => (Some(1), Some(0), Some(0)),
        ToolOutcome::Failure(_) => (Some(1), Some(0), Some(0)),
        _ => (None, None, None),
    }
}

/// PathGuard-style check for grep/glob search paths.
///
/// `PathGuard::check_read` requires the path to exist, but grep/glob
/// arguments are often glob patterns (`src/**/*.rs`) or directories
/// that may not exist yet. This helper does the deny-list and sandbox
/// containment checks without requiring existence, falling back to
/// the longest existing ancestor for containment.
///
/// This was the source of GPT 5.5's review finding #3 ("PathGuard
/// applied to grep/glob") — without this, a model could enumerate
/// files outside the sandbox via grep/glob even though
/// read/write/edit were guarded.
pub(crate) fn check_search_path(path_guard: &PathGuard, path: &std::path::Path) -> GuardVerdict {
    // 1. Deny list — same as check_read.
    if path_guard.deny_list.is_path_denied(path) {
        return GuardVerdict::Denied(format!("Path denied by deny list: {}", path.display()));
    }

    // 2. Resolve to the longest existing ancestor so glob patterns
    //    still get a containment check. If nothing in the path exists
    //    (e.g. the model is searching a freshly-deleted directory),
    //    fall back to the literal path; the sandbox check below will
    //    deny it because we can't prove containment.
    let check = if path.exists() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let mut cur = path.to_path_buf();
        while !cur.exists() {
            if !cur.pop() {
                break;
            }
        }
        cur.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    };

    // 3. Sandbox containment on the resolved ancestor.
    if let Some(ref sandbox) = path_guard.sandbox_dir {
        let sb = match sandbox.canonicalize() {
            Ok(s) => s,
            Err(e) => {
                return GuardVerdict::Denied(format!(
                    "Cannot resolve sandbox dir '{}': {e}",
                    sandbox.display()
                ));
            }
        };
        if !check.starts_with(&sb) {
            return GuardVerdict::Denied(format!(
                "Search path outside sandbox: {}",
                path.display()
            ));
        }
    }

    GuardVerdict::Allowed(path.to_path_buf())
}

pub(crate) fn check_deny_list(
    deny_list: &DenyList,
    tool_name: &str,
    args: &serde_json::Value,
) -> Option<String> {
    match tool_name {
        "read_file" | "read_image" | "write_file" | "edit_file" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        "bash" => {}
        "grep" | "glob" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                let p = std::path::Path::new(path);
                if deny_list.is_path_denied(p) {
                    return Some(format!("🔒 Path denied by deny list: {path}"));
                }
            }
        }
        _ => {}
    }
    None
}

/// Process a tool outcome: append the conversation log entry, push a
/// `TurnEvent::ToolResult` for downstream consumers, and (on error) try
/// to surface a recovery hint.
///
/// Returns the rendered diff string when the outcome was a `FileEdit`.
/// This is propagated up to `emit_tool_event_and_correct` so the
/// `BusEvent::Edit` carries the *real* diff, not the user's `old_string`
/// (which is what the previous implementation used — see GPT 5.5
/// review finding #9).
pub(crate) fn handle_tool_outcome(
    outcome: ToolOutcome,
    tc: &ToolInvocation,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<Option<String>> {
    match outcome {
        ToolOutcome::Success { content } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: content.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::FileContent { content, .. } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: content.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::FileEdit { diff, .. } => {
            // Hand the rendered diff to the caller so the
            // BusEvent::Edit event downstream carries the real
            // diff text — see the docstring on this fn.
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: diff.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: diff.clone(),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
            return Ok(Some(diff));
        }
        ToolOutcome::GrepMatches {
            path,
            matches,
            total: _,
        } => {
            let output = format_grep_output(&path, &matches);
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: output.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: output,
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
        ToolOutcome::Error { message } => {
            crate::send_or_warn!(
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: format!("Error: {message}"),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {message}"),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;

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
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: format!("Error: {message}"),
                    success: false,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: format!("Error: {message}"),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;

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
                event_tx.send(TurnEvent::ToolResult {
                    name: tc.name.clone(),
                    output: projection.clone(),
                    success: true,
                }),
                "TurnEvent receiver dropped; discarding event"
            );
            conversation.append(Message {
                role: Role::Tool,
                content: projection,
                content_parts: Some(vec![crate::shared::ContentPart::Image {
                    data_base64,
                    mime,
                }]),
                tool_call_id: Some(tc.id.clone()),
                tool_name: Some(tc.name.clone()),
                ..Default::default()
            })?;
        }
    }
    Ok(None)
}

pub(crate) fn format_grep_output(
    path: &std::path::Path,
    matches: &[crate::shared::Match],
) -> String {
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

pub(crate) fn emit_correction_results(
    results: Vec<CorrectionResult>,
    tc: &ToolInvocation,
    event_tx: &mpsc::UnboundedSender<TurnEvent>,
    conversation: &mut ConversationLog,
) -> anyhow::Result<()> {
    for cr in &results {
        crate::send_or_warn!(
            event_tx.send(TurnEvent::Verification {
                message: cr.message.clone(),
                success: cr.success,
            }),
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
