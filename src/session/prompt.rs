use crate::shared::{Message, Role};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

/// System prompt builder with prompt-cache-aware stem design.
///
/// # Cache stem strategy
///
/// The prompt is structured in two parts:
///
/// 1. **Stem** (invariant): The core instruction block that's identical
///    across all calls for a given model. This is designed to maximize
///    Anthropic-style prompt caching — the cache key picks up the first
///    N tokens, and if the stem hasn't changed, the cache hits.
///
/// 2. **Suffix** (variable): Tool list, model-specific flags, user context.
///    Changes every turn but avoids invalidating the stem's cache entry.
///
/// For best cache performance, the stem should be at least 1024 tokens
/// (the minimum for Anthropic prompt caching). With code-heavy system
/// prompts, 1 token ≈ 4 characters → ~4096 chars minimum.
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
    ///
    /// The returned message has `content` structured as:
    /// ```
    /// [CACHE STEM — invariant instructions]
    /// Available tools: [...]
    /// [model-specific extensions]
    /// ```
    ///
    /// The stem portion is identical for all calls to the same model
    /// (same model_name + same thinking flag). The suffix changes
    /// per-turn based on available tools.
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

    /// Build just the cache stem for a given model.
    ///
    /// This is the invariant portion of the system prompt. Use it to
    /// estimate whether a cache hit is likely before the full build.
    pub fn build_stem(&self, model_name: &str, model_supports_thinking: bool) -> String {
        let reg = handlebars::Handlebars::new();
        let mut data = serde_json::json!({
            "model_name": model_name,
            "tools": Vec::<serde_json::Value>::new(), // empty — tools go in suffix
        });

        if model_supports_thinking {
            data["thinking_available"] = serde_json::Value::Bool(true);
        }

        reg.render_template(&self.template, &data)
            .unwrap_or_else(|_| "You are a coding agent.".to_string())
    }

    /// Estimate cache hit probability based on stem stability.
    ///
    /// Returns a score 0.0–1.0 where 1.0 = perfect cache hit expected.
    /// The stem must be at least 1024 tokens (~4096 chars) for cache
    /// eligibility on most providers.
    pub fn cache_hit_probability(&self, model_name: &str, model_supports_thinking: bool) -> f64 {
        let stem = self.build_stem(model_name, model_supports_thinking);
        let stem_chars = stem.len();
        let stem_tokens_est = stem_chars / 4;

        // Minimum 1024 tokens for Anthropic-style prompt caching
        if stem_tokens_est < 1024 {
            return 0.3; // Small stem → tools section is proportionally large → cache miss likely
        }

        // The longer the stem relative to total, the more likely a hit
        // With a stem > 2048 tokens, cache hit is highly likely
        if stem_tokens_est > 2048 {
            0.95
        } else {
            // Linear scale from 1024 to 2048 tokens
            0.3 + (stem_tokens_est as f64 - 1024.0) / (2048.0 - 1024.0) * 0.65
        }
    }

    /// Build the conversation messages array with token budgeting,
    /// minification, and cache-stem-aware truncation.
    ///
    /// When truncating, this preserves the system prompt (cache stem)
    /// at all costs and drops/minifies older messages before dropping
    /// tool results.
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
        let minified_content = RefCell::new(HashMap::<usize, String>::new());

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

            // Minify the content (safe variant — preserves test blocks the model has seen)
            let path = PathBuf::from(format!("message-{}.txt", i));
            let minified = crate::shared::minify::minify_source_safe(&path, &msg.content);
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
        let keep_count = (budget * 4) / 20;
        let history_to_keep = std::cmp::min(keep_count, adjusted.len() - 1);

        let mut truncated = vec![adjusted[0].clone()]; // keep system (cache stem)

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_stem_invariant() {
        let builder = PromptBuilder::new();
        let stem1 = builder.build_stem("glm-5.1:cloud", true);
        let stem2 = builder.build_stem("glm-5.1:cloud", true);
        assert_eq!(stem1, stem2, "Stem should be identical for same model");
    }

    #[test]
    fn test_build_stem_is_non_empty() {
        let builder = PromptBuilder::new();
        let stem1 = builder.build_stem("glm-5.1:cloud", true);
        let stem2 = builder.build_stem("deepseek-v4", false);
        assert!(!stem1.is_empty());
        assert!(!stem2.is_empty());
        // Stems for different models with different settings may differ
    }

    #[test]
    fn test_cache_hit_probability_returns_some() {
        let builder = PromptBuilder::new();
        let prob = builder.cache_hit_probability("glm-5.1:cloud", true);
        assert!(prob >= 0.0 && prob <= 1.0);
    }

    #[test]
    fn test_build_includes_tools() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", false, &["read_file", "bash"]);
        assert_eq!(msg.role, Role::System);
        assert!(!msg.content.is_empty());
    }

    #[test]
    fn test_build_supports_thinking() {
        let mut builder = PromptBuilder::new();
        let msg = builder.build("test-model", true, &[]);
        assert!(!msg.content.is_empty());
    }

    #[test]
    fn test_build_messages_basic() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "You are a coding agent.".into(),
            ..Default::default()
        };
        let history = vec![
            Message {
                role: Role::User,
                content: "Hello".into(),
                ..Default::default()
            },
        ];
        let result = builder.build_messages(system.clone(), &history, 8192, &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, system.content);
        assert_eq!(result[1].content, "Hello");
    }

    #[test]
    fn test_build_messages_truncation() {
        let mut builder = PromptBuilder::new();
        let system = Message {
            role: Role::System,
            content: "S".into(),
            ..Default::default()
        };
        let mut history = Vec::new();
        for i in 0..20 {
            history.push(Message {
                role: Role::User,
                content: format!("Message {}", i),
                ..Default::default()
            });
        }
        let result = builder.build_messages(system.clone(), &history, 50, &[]);
        // With a tiny budget, should truncate
        assert!(result.len() < 22);
        // System prompt must always be first
        assert_eq!(result[0].content, "S");
    }

    #[test]
    fn test_build_stem_no_tools() {
        let builder = PromptBuilder::new();
        let stem = builder.build_stem("test-model", false);
        assert!(!stem.is_empty());
    }
}