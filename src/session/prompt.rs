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
    /// When truncating, it minifies older messages first to preserve semantic
    /// content while reducing token count.
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

        let estimate_tokens = |m: &Message| -> usize {
            m.content.len() / 4 + m.thinking.as_ref().map(|t| t.len() / 4).unwrap_or(0)
        };

        let total_est: usize = messages.iter().map(estimate_tokens).sum();

        if total_est <= budget {
            return messages;
        }

        // Over budget. Strategy: try minifying older non-system messages first.
        // If that's still over budget, drop older messages (keep the tail).
        let minified_content = std::cell::RefCell::new(std::collections::HashMap::<usize, String>::new());

        // First pass: try minifying user/assistant pairs from the oldest end
        let mut adjusted = messages.clone();
        let mut minified_any = false;

        for (i, msg) in messages.iter().enumerate() {
            if i == 0 {
                continue; // keep system prompt as-is
            }
            if matches!(msg.role, Role::Tool) {
                continue; // keep tool results as-is
            }

            let est = estimate_tokens(msg);
            if est < 10 {
                continue; // too short to bother
            }

            // Minify the content
            let path = std::path::PathBuf::from(format!("message-{}.txt", i));
            let minified = crate::shared::minify::minify_source(&path, &msg.content);
            if minified.len() < msg.content.len() {
                let savings = msg.content.len() - minified.len();
                if savings > 20 {
                    adjusted[i].content = minified.clone();
                    minified_content.borrow_mut().insert(i, minified);
                    minified_any = true;
                }
            }
        }

        if minified_any {
            let new_est: usize = adjusted.iter().map(estimate_tokens).sum();
            if new_est <= budget {
                return adjusted;
            }
        }

        // Still over budget — drop from middle (keep the most recent tail)
        let keep_count = (budget * 4) / 20; // rough proportional estimate
        let history_to_keep = std::cmp::min(keep_count, adjusted.len() - 1);

        let mut truncated = vec![adjusted[0].clone()]; // keep system

        // Keep the most recent tail
        let start = adjusted.len().saturating_sub(history_to_keep);
        for msg in &adjusted[start..] {
            truncated.push(msg.clone());
        }

        if truncated.len() < 2 {
            truncated = adjusted; // keep everything if we'd empty the conversation
        }

        truncated
    }
}

impl Default for PromptBuilder {
    fn default() -> Self {
        Self::new()
    }
}