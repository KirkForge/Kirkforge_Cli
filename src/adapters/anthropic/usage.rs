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
    let cache_write_tokens = u
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_write_tokens,
    }
}

/// Field-wise merge: `over`'s `Some` fields replace `base`'s. The real
/// wire splits usage across frames — `message_start` carries the input
/// side (input/cache_read/cache_creation), `message_delta` the final
/// output_tokens — so the accumulator fills in fields as they arrive
/// (WO 38.5).
pub(super) fn merge_usage(base: TokenUsage, over: TokenUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: over.prompt_tokens.or(base.prompt_tokens),
        completion_tokens: over.completion_tokens.or(base.completion_tokens),
        cached_tokens: over.cached_tokens.or(base.cached_tokens),
        cache_write_tokens: over.cache_write_tokens.or(base.cache_write_tokens),
    }
}

/// `Some` only when at least one field was observed — an all-`None`
/// accumulator must not masquerade as reported usage.
pub(super) fn finalize_usage(u: TokenUsage) -> Option<TokenUsage> {
    if u.prompt_tokens.is_none()
        && u.completion_tokens.is_none()
        && u.cached_tokens.is_none()
        && u.cache_write_tokens.is_none()
    {
        None
    } else {
        Some(u)
    }
}
