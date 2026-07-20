//! Automatic per-turn context compaction (microcompaction).
//!
//! Unlike the user-driven `/compact` command (`session/prompt/compaction.rs`)
//! which rewrites the conversation log, microcompaction runs silently inside
//! `PromptBuilder::build_messages` when the estimated token count exceeds a
//! configurable threshold. It keeps the system anchor and the last `keep_tail`
//! messages verbatim, and replaces the oldest messages in the middle with a
//! single compact `[context summary]` system message.
//!
//! The summarization is deterministic/heuristic by default: it extracts tool
//! names, file paths, and error markers from the oldest middle messages. This
//! avoids an extra LLM call on every turn while still compressing the middle
//! more aggressively than simple stubbing.
//!
//! ponytail: this is not full semantic summarization. The LLM-based
//! summarizer in `summarizer.rs` remains the higher-quality path triggered by
//! `/compact`.
//!
//! ceiling: the heuristic summary drops prose from user and assistant turns,
//! keeping only structural signals (files, errors, tool names). Upgrade path:
//! wire the LLM summarizer here behind a config flag for automatic semantic
//! microcompaction.

use crate::shared::{Message, Role};

/// Result of applying microcompaction.
#[derive(Debug, Clone)]
pub struct MicrocompactResult {
    pub messages: Vec<Message>,
    pub summarised_messages: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
}

/// Apply heuristic microcompaction when the estimated token count exceeds the
/// threshold.
///
/// `keep_tail` is the number of trailing messages preserved verbatim (must be
/// at least 1). When the history is short or already under budget, returns the
/// original slice unchanged.
pub fn maybe_microcompact(
    messages: &[Message],
    threshold_tokens: usize,
    keep_tail: usize,
) -> Option<MicrocompactResult> {
    let keep_tail = keep_tail.max(1);
    if messages.len() <= keep_tail + 1 {
        return None;
    }

    let tokens_before = estimate_tokens(messages);
    if tokens_before <= threshold_tokens {
        return None;
    }

    let anchor = if !messages.is_empty() && matches!(messages[0].role, Role::System) {
        1
    } else {
        0
    };

    // We must keep the anchor plus keep_tail trailing messages. Everything
    // in between is eligible for summarization.
    let tail_start = messages.len().saturating_sub(keep_tail);
    if tail_start <= anchor {
        // No room in the middle to compress.
        return None;
    }

    let summary = heuristic_summary(&messages[anchor..tail_start]);
    let summarised_messages = tail_start - anchor;

    let mut out = Vec::with_capacity(anchor + 1 + keep_tail);
    if anchor > 0 {
        out.push(messages[0].clone());
    }
    out.push(Message {
        role: Role::System,
        content: format!(
            "[Context summary — {summarised_messages} earlier messages compressed]\n{summary}",
        ),
        content_parts: None,
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    });
    for msg in &messages[tail_start..] {
        out.push(msg.clone());
    }

    let tokens_after = estimate_tokens(&out);
    Some(MicrocompactResult {
        messages: out,
        summarised_messages,
        tokens_before,
        tokens_after,
    })
}

/// Build a deterministic, low-token summary of a set of old messages.
///
/// Captures:
/// - Tool calls made (by name)
/// - File paths mentioned in tool calls or results
/// - Error/failure markers
fn heuristic_summary(messages: &[Message]) -> String {
    let mut tool_names = Vec::new();
    let mut paths = Vec::new();
    let mut errors = 0usize;

    for msg in messages {
        match msg.role {
            Role::Assistant => {
                if let Some(ref calls) = msg.tool_calls {
                    for tc in calls {
                        if !tool_names.contains(&tc.name) {
                            tool_names.push(tc.name.clone());
                        }
                        extract_path(&tc.arguments, &mut paths);
                    }
                }
            }
            Role::Tool => {
                if let Some(ref name) = msg.tool_name {
                    if !tool_names.contains(name) {
                        tool_names.push(name.clone());
                    }
                }
                if msg.content.contains("error") || msg.content.contains("Error") {
                    errors += 1;
                }
                extract_path_from_text(&msg.content, &mut paths);
            }
            _ => {}
        }
    }

    let mut parts = Vec::new();
    if !tool_names.is_empty() {
        parts.push(format!("tools used: {}", tool_names.join(", ")));
    }
    if !paths.is_empty() {
        let unique: Vec<String> = paths.into_iter().take(8).collect();
        parts.push(format!("paths: {}", unique.join(", ")));
    }
    if errors > 0 {
        parts.push(format!("{errors} error(s) encountered"));
    }

    if parts.is_empty() {
        "(older conversation context omitted for token budget)".to_string()
    } else {
        parts.join("; ")
    }
}

fn extract_path(args: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
        if !out.contains(&path.to_string()) {
            out.push(path.to_string());
        }
    }
    if let Some(paths) = args.get("paths").and_then(|v| v.as_array()) {
        for p in paths {
            if let Some(s) = p.as_str() {
                if !out.contains(&s.to_string()) {
                    out.push(s.to_string());
                }
            }
        }
    }
}

fn extract_path_from_text(text: &str, out: &mut Vec<String>) {
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.');
        if !trimmed.is_empty()
            && !out.contains(&trimmed.to_string())
            && out.len() < 12
            && (trimmed.starts_with('/') || trimmed.starts_with("./") || trimmed.ends_with(".rs"))
        {
            out.push(trimmed.to_string());
        }
    }
}

fn estimate_message_tokens(m: &Message) -> usize {
    let content = m.content.len() / 4;
    let thinking = m.thinking.as_ref().map(|t| t.len() / 4).unwrap_or(0);
    let tool_calls = m
        .tool_calls
        .as_ref()
        .map(|calls| {
            serde_json::to_string(calls)
                .map(|s| s.len() / 4)
                .unwrap_or(0)
        })
        .unwrap_or(0);
    content + thinking + tool_calls
}

fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{Message, Role, ToolInvocation};

    fn system(text: &str) -> Message {
        Message {
            role: Role::System,
            content: text.to_string(),
            ..Default::default()
        }
    }

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: text.to_string(),
            ..Default::default()
        }
    }

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: text.to_string(),
            ..Default::default()
        }
    }

    fn assistant_with_tools(text: &str, tools: Vec<ToolInvocation>) -> Message {
        Message {
            role: Role::Assistant,
            content: text.to_string(),
            tool_calls: Some(tools),
            ..Default::default()
        }
    }

    fn tool(name: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            tool_name: Some(name.to_string()),
            content: content.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn no_compact_when_under_threshold() {
        let msgs = vec![system("sys"), user("hi")];
        assert!(maybe_microcompact(&msgs, 10, 1).is_none());
    }

    #[test]
    fn no_compact_when_history_too_short() {
        let msgs = vec![system("sys"), user("hi")];
        assert!(maybe_microcompact(&msgs, 0, 1).is_none());
    }

    #[test]
    fn compacts_middle_and_preserves_tail() {
        let msgs = vec![
            system("sys"),
            user("old ask"),
            assistant_with_tools(
                "",
                vec![ToolInvocation {
                    id: "t1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                }],
            ),
            tool("read_file", "fn main() {}"),
            user("new ask"),
        ];
        let res = maybe_microcompact(&msgs, 0, 1).unwrap();
        assert_eq!(res.messages.len(), 3); // system + summary + tail user
        assert_eq!(res.summarised_messages, 3);
        // With very short inputs the summary can be slightly longer than the
        // originals; the important invariant is that the middle was collapsed
        // and the tail preserved.
        assert!(res.messages[1].content.contains("read_file"));
        assert!(res.messages[1].content.contains("src/main.rs"));
        assert_eq!(res.messages[2].content, "new ask");
    }

    #[test]
    fn preserves_anchor_system() {
        let msgs = vec![
            system("anchor"),
            user("a"),
            assistant("b"),
            user("c"),
            assistant("d"),
            user("live"),
        ];
        let res = maybe_microcompact(&msgs, 0, 2).unwrap();
        assert_eq!(res.messages[0].content, "anchor");
        assert_eq!(res.messages.len(), 4); // anchor + summary + 2 tail
    }
}
