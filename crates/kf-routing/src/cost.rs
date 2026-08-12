//! R4 — port of `orchestrator/src/cost.ts`.
//!
//! Simple cost estimation for model calls based on provider and token counts.

const RATES: &[(&str, f64, f64)] = &[
    ("local-ollama", 0.0, 0.0),
    ("openrouter-free", 0.0, 0.0),
    ("nvidia-free", 0.0, 0.0),
    ("openai", 0.00015, 0.0006),
    ("anthropic", 0.0008, 0.004),
    ("deepseek", 0.000014, 0.000028),
    ("google", 0.0000375, 0.00015),
    ("xai", 0.0002, 0.0008),
    ("groq", 0.000059, 0.000079),
    ("mistral", 0.0002, 0.0006),
    ("cohere", 0.0003, 0.0015),
];

/// Resolve the cost provider key from a provider-resolved string. Maps
/// sub-provider keys (e.g. "openai/gpt-4o" → "openai") to their cost-rate key.
/// Unknown → "local-ollama" (free; matches TS default).
pub fn resolve_cost_provider_key(provider_resolved: &str) -> &'static str {
    for (k, _, _) in RATES {
        if *k == provider_resolved {
            return k;
        }
    }
    let lower = provider_resolved.to_lowercase();
    for (k, _, _) in RATES {
        if lower.starts_with(k) {
            return k;
        }
    }
    "local-ollama"
}

/// Estimate the cost of a model call in USD based on provider and token
/// counts. Uses per-1K-token rates.
pub fn estimate_simple_cost(provider: &str, prompt_tokens: i64, completion_tokens: i64) -> f64 {
    let key = resolve_cost_provider_key(provider);
    let rate = RATES
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, i, o)| (*i, *o))
        .unwrap_or((0.0, 0.0));
    (prompt_tokens as f64 * rate.0 + completion_tokens as f64 * rate.1) / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ollama_is_free() {
        assert_eq!(estimate_simple_cost("local-ollama", 1000, 1000), 0.0);
    }

    #[test]
    fn openai_rate_per_1k() {
        let cost = estimate_simple_cost("openai", 1000, 500);
        // rates are per-1K-tokens; formula divides by 1000.
        let expected = (1000.0 * 0.00015 + 500.0 * 0.0006) / 1000.0;
        assert!((cost - expected).abs() < 1e-12);
    }

    #[test]
    fn resolves_sub_provider_prefix() {
        assert_eq!(resolve_cost_provider_key("openai/gpt-4o"), "openai");
        assert_eq!(resolve_cost_provider_key("anthropic/claude-3"), "anthropic");
    }

    #[test]
    fn unknown_provider_defaults_to_free() {
        assert_eq!(resolve_cost_provider_key("klingon-7"), "local-ollama");
        assert_eq!(estimate_simple_cost("klingon-7", 100, 100), 0.0);
    }

    #[test]
    fn zero_tokens_zero_cost() {
        assert_eq!(estimate_simple_cost("anthropic", 0, 0), 0.0);
    }
}
