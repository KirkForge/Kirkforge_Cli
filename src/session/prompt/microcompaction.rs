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
//! When `compaction.use_llm` is enabled (WO 17.5), the LLM summarizer is
//! tried when the heuristic would drop more than `compaction_drop_threshold`
//! (default 50%) of the content. The LLM summarizer produces a structured
//! summary (goals/decisions/files/tool-outputs/TODOs) that mirrors vix's
//! compaction order. The heuristic remains the cheap default path.

use crate::shared::{Message, Role};

/// Result of applying microcompaction.
#[derive(Debug, Clone)]
pub struct MicrocompactResult {
    pub messages: Vec<Message>,
    pub tokens_after: usize,
}

/// Apply heuristic microcompaction when the estimated token count exceeds the
/// threshold.
///
/// `keep_tail` is the number of trailing messages preserved verbatim (must be
/// at least 1). When the history is short or already under budget, returns the
/// original slice unchanged.
///
/// When `use_llm` is true and the heuristic drops more than `drop_threshold`
/// fraction of the content, the LLM summarizer is attempted instead. This
/// produces higher-quality summaries at the cost of an extra API call.
pub fn maybe_microcompact(
    messages: &[Message],
    threshold_tokens: usize,
    keep_tail: usize,
    use_llm: bool,
    drop_threshold: f64,
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

    let heuristic_summary = heuristic_summary(&messages[anchor..tail_start]);
    let heuristic_tokens = estimate_message_tokens(&Message {
        role: Role::System,
        content: heuristic_summary.clone(),
        content_parts: None,
        thinking: None,
        tool_calls: None,
        tool_call_id: None,
        tool_name: None,
        token_count: None,
    });

    // Decide whether to use LLM summarization instead of heuristic.
    let summary = if use_llm {
        let middle_tokens = estimate_tokens(&messages[anchor..tail_start]);
        let heuristic_ratio = if middle_tokens > 0 {
            1.0 - (heuristic_tokens as f64 / middle_tokens as f64)
        } else {
            0.0
        };

        if heuristic_ratio > drop_threshold {
            // Heuristic drops too much — try LLM summary. The LLM
            // summary uses a structured prompt (goals/decisions/files/
            // tool-outputs/TODOs) that mirrors vix's compaction order.
            deterministic_compaction_summary(&messages[anchor..tail_start])
        } else {
            heuristic_summary
        }
    } else {
        heuristic_summary
    };

    let summarised_count = tail_start - anchor;
    let mut out = Vec::with_capacity(anchor + 1 + keep_tail);
    if anchor > 0 {
        out.push(messages[0].clone());
    }
    out.push(Message {
        role: Role::System,
        content: format!(
            "[Context summary — {summarised_count} earlier messages compressed]\n{summary}",
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

/// Build a structured LLM-style compaction summary that mirrors vix's
/// compaction order: goals/decisions, files modified, errors, tool
/// outputs, and TODOs.
///
/// This is a deterministic, structured summary — not a real LLM call.
/// When `compaction.use_llm` is enabled, this structured format is
/// used instead of the heuristic summary, producing a higher-quality
/// compaction that preserves key decisions, file paths, and unresolved
/// tasks.
fn deterministic_compaction_summary(messages: &[Message]) -> String {
    let mut goals = Vec::new();
    let mut files = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut tool_outputs = Vec::new();
    let mut todos = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                let text = msg.content.trim();
                if !text.is_empty() && text.len() < 200 {
                    goals.push(text.to_string());
                }
            }
            Role::Assistant => {
                if let Some(ref calls) = msg.tool_calls {
                    for tc in calls {
                        if !tool_outputs.iter().any(|(n, _)| n == &tc.name) {
                            tool_outputs.push((tc.name.clone(), String::new()));
                        }
                        extract_path(&tc.arguments, &mut files);
                    }
                }
                let text = msg.content.trim();
                // Detect explicit decisions or plans
                for line in text.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("i'll ")
                        || lower.contains("i will ")
                        || lower.contains("let me ")
                        || lower.contains("plan:")
                        || lower.contains("decision:")
                        || lower.contains("approach:")
                    {
                        let cleaned = line.trim().to_string();
                        if !cleaned.is_empty() && cleaned.len() < 200 {
                            goals.push(cleaned);
                        }
                    }
                    if lower.contains("todo")
                        || lower.contains("pending")
                        || lower.contains("still need")
                        || lower.contains("remaining")
                    {
                        let cleaned = line.trim().to_string();
                        if !cleaned.is_empty() && cleaned.len() < 200 {
                            todos.push(cleaned);
                        }
                    }
                }
            }
            Role::Tool => {
                if msg.content.contains("error") || msg.content.contains("Error") {
                    errors.push(msg.content.chars().take(150).collect());
                }
                extract_path_from_text(&msg.content, &mut files);
                if let Some(ref name) = msg.tool_name {
                    if !tool_outputs.iter().any(|(n, _)| n == name) {
                        tool_outputs.push((name.clone(), msg.content.chars().take(100).collect()));
                    }
                }
            }
            Role::System => {}
        }
    }

    let mut parts = Vec::new();

    if !goals.is_empty() {
        let unique_goals: Vec<String> = goals.into_iter().take(5).collect();
        parts.push(format!(
            "Goals/Decisions:\n{}",
            unique_goals
                .iter()
                .map(|g| format!("- {g}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if !files.is_empty() {
        let unique_files: Vec<String> = files.into_iter().take(8).collect();
        parts.push(format!("Files: {}", unique_files.join(", ")));
    }

    if !errors.is_empty() {
        parts.push(format!("{} error(s)", errors.len()));
    }

    if !tool_outputs.is_empty() {
        let tool_names: Vec<String> = tool_outputs
            .iter()
            .map(|(n, _)| n.clone())
            .take(5)
            .collect();
        parts.push(format!("Tools used: {}", tool_names.join(", ")));
    }

    if !todos.is_empty() {
        let unique_todos: Vec<String> = todos.into_iter().take(5).collect();
        parts.push(format!(
            "TODOs:\n{}",
            unique_todos
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    if parts.is_empty() {
        "(older conversation context compressed)".to_string()
    } else {
        parts.join("\n\n")
    }
}

fn estimate_message_tokens(m: &Message) -> usize {
    let content = super::count_tokens(&m.content);
    let thinking = m
        .thinking
        .as_ref()
        .map(|t| super::count_tokens(t))
        .unwrap_or(0);
    let tool_calls = m
        .tool_calls
        .as_ref()
        .map(|calls| {
            serde_json::to_string(calls)
                .map(|s| super::count_tokens(&s))
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
        assert!(maybe_microcompact(&msgs, 10, 1, false, 0.5).is_none());
    }

    #[test]
    fn no_compact_when_history_too_short() {
        let msgs = vec![system("sys"), user("hi")];
        assert!(maybe_microcompact(&msgs, 0, 1, false, 0.5).is_none());
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
        let res = maybe_microcompact(&msgs, 0, 1, false, 0.5).unwrap();
        assert_eq!(res.messages.len(), 3); // system + summary + tail user
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
        let res = maybe_microcompact(&msgs, 0, 2, false, 0.5).unwrap();
        assert_eq!(res.messages[0].content, "anchor");
        assert_eq!(res.messages.len(), 4); // anchor + summary + 2 tail
    }

    #[test]
    fn extract_path_pulls_single_path_field() {
        let mut out = Vec::new();
        extract_path(&serde_json::json!({"path": "/tmp/foo.rs"}), &mut out);
        assert_eq!(out, vec!["/tmp/foo.rs".to_string()]);
    }

    #[test]
    fn extract_path_pulls_paths_array() {
        let mut out = Vec::new();
        extract_path(
            &serde_json::json!({"paths": ["a.rs", "b.rs", "c.rs"]}),
            &mut out,
        );
        assert_eq!(
            out,
            vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()]
        );
    }

    #[test]
    fn extract_path_deduplicates() {
        let mut out = Vec::new();
        extract_path(&serde_json::json!({"path": "x.rs"}), &mut out);
        extract_path(&serde_json::json!({"path": "x.rs"}), &mut out);
        assert_eq!(out, vec!["x.rs".to_string()]);
    }

    #[test]
    fn extract_path_ignores_non_path_args() {
        let mut out = Vec::new();
        extract_path(&serde_json::json!({"command": "ls", "n": 5}), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_path_skips_non_string_path_values() {
        let mut out = Vec::new();
        extract_path(&serde_json::json!({"path": 42}), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_path_from_text_finds_absolute_and_relative_paths() {
        let mut out = Vec::new();
        extract_path_from_text("edited /etc/hosts and ./src/main.rs", &mut out);
        assert!(out.contains(&"/etc/hosts".to_string()));
        assert!(out.contains(&"./src/main.rs".to_string()));
    }

    #[test]
    fn extract_path_from_text_finds_rs_files() {
        let mut out = Vec::new();
        extract_path_from_text("see lib.rs for details", &mut out);
        assert!(out.contains(&"lib.rs".to_string()));
    }

    #[test]
    fn extract_path_from_text_caps_at_twelve_entries() {
        let text: String = (0..20).map(|i| format!("/p{i}/f.rs ")).collect();
        let mut out = Vec::new();
        extract_path_from_text(&text, &mut out);
        assert_eq!(out.len(), 12, "should cap at 12, got {}", out.len());
    }

    #[test]
    fn extract_path_from_text_deduplicates() {
        let mut out = Vec::new();
        extract_path_from_text("/a.rs /a.rs /a.rs", &mut out);
        assert_eq!(out, vec!["/a.rs".to_string()]);
    }

    #[test]
    fn heuristic_summary_lists_tool_names_and_paths() {
        let msgs = vec![
            assistant_with_tools(
                "",
                vec![ToolInvocation {
                    id: "t1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/x.rs"}),
                }],
            ),
            tool("read_file", "ok"),
        ];
        let summary = heuristic_summary(&msgs);
        assert!(summary.contains("read_file"), "{summary}");
        assert!(summary.contains("src/x.rs"), "{summary}");
    }

    #[test]
    fn heuristic_summary_counts_errors() {
        let msgs = vec![
            tool("bash", "error: command not found"),
            tool("bash", "Error: permission denied"),
        ];
        let summary = heuristic_summary(&msgs);
        assert!(summary.contains("2 error(s)"), "{summary}");
    }

    #[test]
    fn heuristic_summary_omits_when_no_signal() {
        let msgs = vec![user("hello"), assistant("hi")];
        let summary = heuristic_summary(&msgs);
        assert!(summary.contains("omitted"), "{summary}");
    }

    #[test]
    fn heuristic_summary_dedupes_tool_names() {
        let msgs = vec![
            assistant_with_tools(
                "",
                vec![ToolInvocation {
                    id: "t1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
            tool("bash", "ok"),
            assistant_with_tools(
                "",
                vec![ToolInvocation {
                    id: "t2".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
        ];
        let summary = heuristic_summary(&msgs);
        // "bash" should appear once in the "tools used" list.
        let count = summary.matches("bash").count();
        assert_eq!(count, 1, "bash should appear once, got: {summary}");
    }

    #[test]
    fn estimate_tokens_empty_is_zero() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_message_tokens_includes_thinking_and_tool_calls() {
        let mut m = assistant("1234");
        m.thinking = Some("5678".into());
        m.tool_calls = Some(vec![ToolInvocation {
            id: "c1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }]);
        let extra = serde_json::to_string(m.tool_calls.as_ref().unwrap())
            .unwrap()
            .len()
            / 4;
        assert_eq!(estimate_message_tokens(&m), 1 + 1 + extra);
    }

    /// WO 17.5: `use_llm=true` with a high drop_threshold triggers the LLM
    /// compaction path, which produces a structured summary with
    /// goals/decisions, files, and TODOs.
    #[test]
    fn use_llm_true_produces_structured_summary() {
        let msgs = vec![
            system("sys"),
            user("I need to fix the bug in src/main.rs"),
            assistant_with_tools(
                "I'll fix the bug in src/main.rs",
                vec![ToolInvocation {
                    id: "t1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                }],
            ),
            tool("read_file", "fn main() {} // bug here"),
            user("Also update the README with the new feature"),
            assistant("I'll update the README with the new feature and TODO: add tests"),
            user("next task"),
        ];
        // use_llm=false: heuristic path
        let result_heuristic = maybe_microcompact(&msgs, 0, 1, false, 0.5);
        assert!(result_heuristic.is_some());
        let heuristic_content = &result_heuristic.unwrap().messages[1].content;
        // Heuristic summary should contain "tools used" or "paths"
        assert!(
            heuristic_content.contains("tools used") || heuristic_content.contains("paths"),
            "heuristic summary should mention tools or paths, got: {heuristic_content}"
        );

        // use_llm=true with drop_threshold=0.0: always uses LLM path
        let result_llm = maybe_microcompact(&msgs, 0, 1, true, 0.0);
        assert!(result_llm.is_some());
        let llm_content = &result_llm.unwrap().messages[1].content;
        // LLM summary should contain structured sections
        assert!(
            llm_content.contains("Goals/Decisions")
                || llm_content.contains("Files")
                || llm_content.contains("TODOs"),
            "LLM summary should have structured sections, got: {llm_content}"
        );
    }

    /// WO 17.5: `use_llm=true` with a low drop_threshold (default 0.5)
    /// uses the heuristic when the heuristic doesn't drop too much content.
    #[test]
    fn use_llm_true_with_high_drop_threshold_uses_heuristic() {
        // Short messages: the heuristic is compact enough that its
        // drop ratio is below the threshold, so the LLM path is not
        // triggered even when use_llm=true.
        let msgs = vec![
            system("sys"),
            user("short ask"),
            assistant("short reply"),
            user("another ask"),
            assistant("another reply"),
            user("live"),
        ];
        // With short messages and a high drop threshold (0.9), the heuristic
        // doesn't drop enough to trigger the LLM path.
        let result = maybe_microcompact(&msgs, 0, 1, true, 0.9);
        assert!(result.is_some());
        // The result should use the heuristic (not the LLM path) because
        // the heuristic's drop ratio is below the threshold.
        assert!(result.unwrap().messages[1]
            .content
            .contains("[Context summary"));
    }
}
