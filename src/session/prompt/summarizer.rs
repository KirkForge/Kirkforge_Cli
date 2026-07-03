//! Semantic context summarization — LLM-powered conversation compaction.
//!
//! When the prompt exceeds the model's token budget, instead of simply
//! truncating old tool results (see `compaction.rs`), we send the oldest
//! N turns to a fast/cheap "summarizer" model and replace them with a
//! single compact `[Context summary]` system message.
//!
//! This preserves key decisions, file paths, errors, and unresolved tasks
//! while dramatically reducing the token footprint of old conversation
//! segments.
//!
//! # Architecture
//!
//! ```
//! conversation (oldest → newest)
//! ┌──────────┬─────────────────────┬──────────────┐
//! │ anchor   │  summarise-able     │  working set │
//! │ (system) │  (oldest N turns)   │  (last 4)    │
//! └──────────┴─────────────────────┴──────────────┘
//!                      │
//!                      ▼
//!            ┌─────────────────┐
//!            │ Summarizer LLM  │  (fast/cheap model)
//!            │ "summarise this │
//!            │  conversation"  │
//!            └─────────────────┘
//!                      │
//!                      ▼
//!            ┌─────────────────┐
//!            │ [Context        │
//!            │  summary]       │  (one compact message)
//!            └─────────────────┘
//! ```
//!
//! The anchor (first system message) is always preserved for cache stem
//! stability. The working set (last 4 user↔assistant turns) is kept
//! verbatim so the model has fresh context.

use crate::shared::{Message, Role};
use serde::{Deserialize, Serialize};

/// Configuration for the semantic summarizer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarizerConfig {
    /// Model name for the summarization API call (e.g., "qwen2.5:3b").
    pub model: String,
    /// Maximum tokens for the summary output.
    pub max_summary_tokens: usize,
    /// Minimum number of turns that must be present before summarization
    /// is attempted (otherwise fall back to truncation).
    pub min_turns_for_summary: usize,
    /// Target compression ratio — summarizer won't run if the estimated
    /// savings are below this threshold (e.g., 0.5 = at least 50% reduction).
    pub min_compression_ratio: f64,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            model: "qwen2.5:3b".into(),
            max_summary_tokens: 500,
            min_turns_for_summary: 6,
            min_compression_ratio: 0.5,
        }
    }
}

/// The result of a summarization attempt.
#[derive(Debug, Clone)]
pub struct SummarizeResult {
    /// The summary text (if successful).
    pub summary: Option<String>,
    /// Number of original messages that were summarised.
    pub summarised_messages: usize,
    /// Estimated token count before summarisation.
    pub tokens_before: usize,
    /// Estimated token count after summarisation.
    pub tokens_after: usize,
    /// Whether we fell back to truncation.
    pub fell_back: bool,
    /// Error message if summarisation failed.
    pub error: Option<String>,
}

/// Build the summarization prompt from the conversations to be summarised.
///
/// Produces a compact prompt that instructs the model to extract:
/// - Key decisions made
/// - Files modified or created
/// - Errors encountered
/// - Unresolved tasks/questions
/// - Any important context for continuing work
fn build_summary_prompt(messages: &[Message]) -> String {
    let mut conversation_text = String::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                conversation_text.push_str(&format!(
                    "User: {}\n",
                    truncate_for_summary(&msg.content, 300)
                ));
            }
            Role::Assistant => {
                let text = if msg.content.is_empty() {
                    if let Some(ref tc) = msg.tool_calls {
                        let names: Vec<&str> = tc.iter().map(|t| t.name.as_str()).collect();
                        format!("[called tools: {}]", names.join(", "))
                    } else {
                        "[no content]".to_string()
                    }
                } else {
                    truncate_for_summary(&msg.content, 200)
                };
                conversation_text.push_str(&format!("Assistant: {text}\n"));
            }
            Role::Tool => {
                let label = msg.tool_name.as_deref().unwrap_or("tool");
                conversation_text.push_str(&format!(
                    "{} result: {}\n",
                    label,
                    truncate_for_summary(&msg.content, 150)
                ));
            }
            Role::System => {
                // Skip system messages in the summarizable region
                // (the anchor is preserved separately)
            }
        }
    }

    format!(
        "Summarize this conversation excerpt. Be concise and factual. \
         Include ONLY:\n\
         1. Key decisions made (with file paths)\n\
         2. Files modified, created, or deleted\n\
         3. Errors encountered and whether they were resolved\n\
         4. Unresolved tasks or questions still pending\n\
         5. Important context for continuing work\n\n\
         Format as bullet points. No preamble, no commentary.\n\n\
         Conversation:\n{}",
        conversation_text.trim()
    )
}

/// Truncate text for inclusion in the summary prompt (character limit).
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated: String = text.chars().take(max_chars).collect();
        truncated.push('…');
        truncated
    }
}

/// Prepare the summarization request payload for Ollama's `/api/chat`.
fn build_summarize_request(model: &str, prompt: &str, max_tokens: usize) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": prompt
        }],
        "stream": false,
        "options": {
            "num_predict": max_tokens,
            "temperature": 0.3,
        }
    })
}

/// Call the Ollama `/api/chat` endpoint to generate a summary.
///
/// Returns the summary text, or `None` if the call failed.
pub async fn summarize_conversation(
    config: &SummarizerConfig,
    messages: &[Message],
    ollama_host: &str,
) -> SummarizeResult {
    let tokens_before = estimate_token_count(messages);
    let msg_count = messages.len();

    if msg_count == 0 {
        return SummarizeResult {
            summary: None,
            summarised_messages: 0,
            tokens_before: 0,
            tokens_after: 0,
            fell_back: true,
            error: Some("No messages to summarize".into()),
        };
    }

    if msg_count < config.min_turns_for_summary {
        return SummarizeResult {
            summary: None,
            summarised_messages: msg_count,
            tokens_before,
            tokens_after: tokens_before,
            fell_back: true,
            error: Some(format!(
                "Not enough turns for summarization ({} < {})",
                msg_count, config.min_turns_for_summary
            )),
        };
    }

    let prompt = build_summary_prompt(messages);
    let body = build_summarize_request(&config.model, &prompt, config.max_summary_tokens);
    let url = format!("{}/api/chat", ollama_host.trim_end_matches('/'));

    match reqwest::Client::new()
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(json) => {
                let summary = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()); // Treat empty string as None

                if let Some(ref s) = summary {
                    let tokens_after = s.len() / 4;
                    let compression = 1.0 - (tokens_after as f64 / tokens_before as f64);

                    if compression < config.min_compression_ratio {
                        return SummarizeResult {
                            summary: None,
                            summarised_messages: msg_count,
                            tokens_before,
                            tokens_after,
                            fell_back: true,
                            error: Some(format!(
                                "Compression ratio {:.0}% below threshold {:.0}%",
                                compression * 100.0,
                                config.min_compression_ratio * 100.0,
                            )),
                        };
                    }
                }

                let tokens_after = summary
                    .as_ref()
                    .map(|s| s.len() / 4)
                    .unwrap_or(tokens_before);
                let fell_back = summary.is_none();

                SummarizeResult {
                    summary,
                    summarised_messages: msg_count,
                    tokens_before,
                    tokens_after,
                    fell_back,
                    error: None,
                }
            }
            Err(e) => SummarizeResult {
                summary: None,
                summarised_messages: msg_count,
                tokens_before,
                tokens_after: tokens_before,
                fell_back: true,
                error: Some(format!("JSON parse error: {e}")),
            },
        },
        Err(e) => SummarizeResult {
            summary: None,
            summarised_messages: msg_count,
            tokens_before,
            tokens_after: tokens_before,
            fell_back: true,
            error: Some(format!("HTTP error: {e}")),
        },
    }
}

/// Estimate the token count of a set of messages.
fn estimate_token_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content = m.content.len() / 4;
            let thinking = m.thinking.as_ref().map(|t| t.len() / 4).unwrap_or(0);
            let tool_calls = m
                .tool_calls
                .as_ref()
                .map(|c| serde_json::to_string(c).map(|s| s.len() / 4).unwrap_or(0))
                .unwrap_or(0);
            content + thinking + tool_calls
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_summary_prompt_includes_content() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "Fix the bug in auth.rs".into(),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: "I found the issue — it's a null pointer.".into(),
                ..Default::default()
            },
            Message {
                role: Role::Tool,
                content: "Compiled successfully".into(),
                tool_name: Some("bash".into()),
                ..Default::default()
            },
        ];

        let prompt = build_summary_prompt(&messages);
        assert!(prompt.contains("Fix the bug"));
        assert!(prompt.contains("null pointer"));
        assert!(prompt.contains("Compiled successfully"));
        assert!(prompt.contains("bash result:"));
    }

    #[test]
    fn test_build_summary_prompt_truncates_long_content() {
        let long = "x".repeat(500);
        let messages = vec![Message {
            role: Role::User,
            content: long.clone(),
            ..Default::default()
        }];

        let prompt = build_summary_prompt(&messages);
        // The prompt header is ~360 chars + ~310 chars of "User: " + truncated content
        assert!(
            prompt.len() < 700,
            "Long content should be truncated, got {} chars",
            prompt.len()
        );
        assert!(prompt.contains('…'), "Should show truncation ellipsis");
    }

    #[test]
    fn test_build_summary_prompt_skips_system() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "You are a coding agent.".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "Hello".into(),
                ..Default::default()
            },
        ];

        let prompt = build_summary_prompt(&messages);
        assert!(!prompt.contains("coding agent"));
        assert!(prompt.contains("Hello"));
    }

    #[test]
    fn test_build_summary_prompt_empty_assistant_content_shows_tools() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![
                crate::shared::ToolInvocation {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
                crate::shared::ToolInvocation {
                    id: "call_2".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({"command": "cargo check"}),
                },
            ]),
            ..Default::default()
        }];

        let prompt = build_summary_prompt(&messages);
        assert!(prompt.contains("read_file, bash"));
    }

    #[test]
    fn test_truncate_for_summary() {
        let short = "hello";
        assert_eq!(truncate_for_summary(short, 100), "hello");

        let long = "x".repeat(100);
        let result = truncate_for_summary(&long, 50);
        assert!(result.ends_with('…'));
        // 50 chars + 1 ellipsis (3 UTF-8 bytes) = up to 53 bytes
        assert!(result.len() <= 53, "got len={}", result.len());
    }

    #[test]
    fn test_estimate_token_count() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "hello world".into(),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".into(),
                ..Default::default()
            },
        ];

        let est = estimate_token_count(&messages);
        assert!(est > 0);
        assert!(est < 50);
    }

    #[test]
    fn test_summarizer_config_defaults() {
        let config = SummarizerConfig::default();
        assert_eq!(config.model, "qwen2.5:3b");
        assert_eq!(config.max_summary_tokens, 500);
        assert_eq!(config.min_turns_for_summary, 6);
    }
}
