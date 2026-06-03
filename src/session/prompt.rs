use crate::shared::{Message, Role};
use std::collections::HashMap;

/// System prompt builder.
/// Uses a Handlebars template with model-type-aware sections.
pub struct PromptBuilder {
    template: String,
    cache: HashMap<String, String>, // keyed by model name
}

impl PromptBuilder {
    pub fn new() -> Self {
        let template = include_str!("../../prompts/system.hbs");
        Self {
            template: template.to_string(),
            cache: HashMap::new(),
        }
    }

    /// Build the system prompt for the given model and tools.
    pub fn build(&mut self, model_name: &str, model_supports_thinking: bool, tool_names: &[&str]) -> Message {
        let reg = handlebars::Handlebars::new();

        let mut data = serde_json::json!({
            "model_name": model_name,
            "tools": tool_names.iter().map(|n| serde_json::json!({"name": n})).collect::<Vec<_>>(),
        });

        if model_supports_thinking {
            data["thinking_available"] = serde_json::Value::Bool(true);
        }

        let rendered = reg.render_template(&self.template, &data)
            .unwrap_or_else(|_| "You are a coding agent.".to_string());

        Message {
            role: Role::System,
            content: rendered,
            thinking: None,
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            token_count: None,
        }
    }

    /// Build the conversation messages array that will be sent to the model.
    /// Applies token-budgeted truncation based on the model's context window.
    pub fn build_messages(
        &mut self,
        system: Message,
        history: &[Message],
        model_max_tokens: usize,
        tool_results: &[Message],
    ) -> Vec<Message> {
        let mut messages = vec![system];

        // Add history messages, newest last
        for msg in history {
            messages.push(msg.clone());
        }

        // Add pending tool results
        for msg in tool_results {
            messages.push(msg.clone());
        }

        // Simple token budget: rough estimate (4 chars ≈ 1 token for code-heavy content)
        let safety_margin = model_max_tokens / 10; // reserve 10% for the response
        let budget = model_max_tokens.saturating_sub(safety_margin);

        let total_est: usize = messages.iter()
            .map(|m| m.content.len() / 4 + m.thinking.as_ref().map(|t| t.len() / 4).unwrap_or(0))
            .sum();

        if total_est <= budget {
            return messages;
        }

        // Need to truncate. Strategy: drop from the middle (oldest non-system, non-tool messages first)
        // Keep system, keep most recent history, drop oldest user/assistant pairs
        let mut truncated = vec![messages[0].clone()]; // keep system

        // Collect index ranges to keep
        let keep_count = (budget * 4) / 20; // rough: keep messages proportional to budget
        let history_to_keep = std::cmp::min(keep_count, messages.len() - 1);

        // Keep the most recent history
        let start = messages.len().saturating_sub(history_to_keep);
        for msg in &messages[start..] {
            truncated.push(msg.clone());
        }

        if truncated.len() < 2 {
            truncated = messages.clone(); // keep everything if we'd empty the conversation
        }

        truncated
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}