//! Usage/token parser for the Anthropic Messages API.

use crate::shared::TokenUsage;

pub(super) fn parse_usage(u: &serde_json::Value) -> TokenUsage {
    let prompt_tokens = u
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let completion_tokens = u
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let cached_tokens = u
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
    }
}
