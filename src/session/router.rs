//! Smart model routing — task complexity classification.
//!
//! When enabled, the user's first message each turn is classified into a
//! complexity tier (Simple, Medium, Complex). This can be used to route
//! simple tasks to a fast/cheap model and complex tasks to a more capable
//! one, saving cost and latency.
//!
//! In Phase 4, classification results are wired into the executor's
//! adapter hot-swap (`adapter_swap.rs`) so the model can change mid-session
//! when `routing_enabled` is true.
//!
//! # Classification methods
//!
//! 1. **Local keyword heuristics** — fast, free, ~60% accurate. Weights
//!    words like "fix", "refactor", "implement", "explore" to estimate
//!    complexity.
//! 2. **LLM-based classification** — when a router model is configured,
//!    sends the user message to a small/fast model for classification.
//!    Falls back to local on error or timeout.
//!
//! # Per-tier model configuration
//!
//! The classifier only decides the complexity tier (`simple`, `medium`,
//! `complex`). The actual model used for each tier is read from
//! `Config::routing_model_map`. If a tier has no entry there, the
//! session falls back to `Config::default_model`. No model names are
//! hard-coded; routing is opt-in and requires explicit configuration.

use serde::{Deserialize, Serialize};

/// Task complexity tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    /// Trivial questions, one-line fixes, simple lookups.
    Simple,
    /// Multi-file changes, moderate complexity.
    Medium,
    /// Architectural changes, debugging, refactoring.
    Complex,
}

impl std::fmt::Display for ComplexityTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComplexityTier::Simple => write!(f, "simple"),
            ComplexityTier::Medium => write!(f, "medium"),
            ComplexityTier::Complex => write!(f, "complex"),
        }
    }
}

/// Configuration for the task router.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Whether model routing is enabled.
    pub enabled: bool,
    /// Model for LLM-based routing (fast/cheap). Empty = use local heuristics.
    pub router_model: String,
}

/// The result of classifying a user message.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// The predicted complexity tier.
    pub tier: ComplexityTier,
    /// Confidence score 0.0–1.0.
    pub confidence: f64,
    /// How the classification was made.
    pub method: RouteMethod,
    /// Suggested model for this tier.
    pub suggested_model: String,
}

/// How the classification was performed.
#[derive(Debug, Clone)]
pub enum RouteMethod {
    /// Local keyword-based heuristics.
    Local,
    /// LLM-based classification.
    Llm,
    /// Previous classification reused (cache hit).
    Cached,
}

/// Classify a user message using local keyword heuristics.
///
/// Fast, free, deterministic. Returns lower confidence than LLM-based
/// classification (~0.6 vs ~0.85). The returned `suggested_model` is
/// the tier name (`simple`/`medium`/`complex`) — actual model selection
/// happens in the adapter swap layer via `routing_model_map`.
pub fn classify_local(message: &str) -> RouteResult {
    let lower = message.to_lowercase();
    let mut score: i32 = 0;

    // Simple indicators (reduce complexity score)
    for word in &[
        "what is", "how do i", "explain", "simple", "quick", "just", "one line", "trivial",
        "example", "show me", "list", "help", "status", "check",
    ] {
        if lower.contains(word) {
            score -= 1;
        }
    }

    // Medium indicators
    for word in &[
        "add", "update", "change", "modify", "write", "create", "delete", "remove", "rename",
        "extract", "split",
    ] {
        if lower.contains(word) {
            score += 1;
        }
    }

    // Complex indicators
    for word in &[
        "refactor",
        "redesign",
        "architecture",
        "rewrite",
        "migrate",
        "debug",
        "fix bug",
        "investigate",
        "optimize",
        "performance",
        "security",
        "audit",
        "implement",
        "design",
        "multi",
        "complex",
        "all",
        "every",
        "across",
        "comprehensive",
    ] {
        if lower.contains(word) {
            score += 2;
        }
    }

    // Message length is a weak signal
    if message.len() > 500 {
        score += 1;
    }
    if message.len() > 1000 {
        score += 1;
    }

    let (tier, confidence) = if score <= 0 {
        (ComplexityTier::Simple, 0.6)
    } else if score <= 3 {
        (ComplexityTier::Medium, 0.55)
    } else {
        (ComplexityTier::Complex, 0.55)
    };

    RouteResult {
        tier,
        confidence,
        method: RouteMethod::Local,
        suggested_model: tier.to_string(),
    }
}

/// Classify a user message using an LLM router model.
///
/// Sends the message to the configured router model for classification.
/// Falls back to local heuristics on error or timeout. The returned
/// `suggested_model` is the tier name (`simple`/`medium`/`complex`).
pub async fn classify_with_llm(
    message: &str,
    router_model: &str,
    ollama_host: &str,
) -> RouteResult {
    let prompt = format!(
        "Classify this developer task as simple, medium, or complex. \
         Reply with only one word (simple/medium/complex).\n\n\
         Task: {message}"
    );

    let body = serde_json::json!({
        "model": router_model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "options": {"num_predict": 5, "temperature": 0.1}
    });

    let url = format!("{}/api/chat", ollama_host.trim_end_matches('/'));

    match crate::shared::build_reqwest_client(Some(std::time::Duration::from_secs(10)))
        .post(&url)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(json) => {
                let answer = json
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_lowercase();

                let tier = if answer.contains("simple") {
                    ComplexityTier::Simple
                } else if answer.contains("complex") {
                    ComplexityTier::Complex
                } else {
                    ComplexityTier::Medium
                };

                return RouteResult {
                    tier,
                    confidence: 0.85,
                    method: RouteMethod::Llm,
                    suggested_model: tier.to_string(),
                };
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse LLM routing response; falling back to local heuristics");
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "LLM routing request failed; falling back to local heuristics");
        }
    }

    // Fallback: use local heuristics. The local result already carries
    // the tier name as the suggested model.
    let mut result = classify_local(message);
    result.confidence *= 0.7; // downgrade confidence since LLM failed
    result
}

/// Resolve a tier name to a concrete model using the configured map.
///
/// Returns `None` when neither `routing_model_map` nor
/// `default_model` can supply a model, so callers can no-op instead of
/// trying to build an adapter for an empty name.
pub fn resolve_tier_model(config: &crate::shared::Config, tier: &str) -> Option<String> {
    let tier_lower = tier.to_lowercase();
    let from_map = config
        .model
        .routing_model_map
        .get(&tier_lower)
        .cloned()
        .filter(|m| !m.is_empty());
    if from_map.is_some() {
        return from_map;
    }
    if config.model.default_model.is_empty() {
        None
    } else {
        Some(config.model.default_model.clone())
    }
}

/// Convenience: classify with LLM if router_model is set, otherwise local.
pub async fn classify(message: &str, config: &RouterConfig, ollama_host: &str) -> RouteResult {
    if config.enabled && !config.router_model.is_empty() {
        classify_with_llm(message, &config.router_model, ollama_host).await
    } else {
        classify_local(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_local_simple() {
        let result = classify_local("What is the current time?");
        assert_eq!(result.tier, ComplexityTier::Simple);
        assert!(result.confidence > 0.5);
    }

    #[test]
    fn test_classify_local_medium() {
        let result = classify_local("Add a dark mode toggle to the settings");
        assert!(matches!(
            result.tier,
            ComplexityTier::Medium | ComplexityTier::Complex
        ));
    }

    #[test]
    fn test_classify_local_complex() {
        let result = classify_local(
            "Refactor the entire authentication system to use OAuth2 with \
             comprehensive error handling and audit logging across all modules",
        );
        assert_eq!(result.tier, ComplexityTier::Complex);
    }

    #[test]
    fn test_classify_local_complex_debug() {
        let result = classify_local(
            "Debug the race condition and refactor the executor to use a multi-threaded architecture",
        );
        assert_eq!(result.tier, ComplexityTier::Complex);
    }

    #[test]
    fn test_classify_local_empty() {
        let result = classify_local("");
        assert_eq!(result.tier, ComplexityTier::Simple);
    }

    #[test]
    fn test_route_result_display() {
        assert_eq!(ComplexityTier::Simple.to_string(), "simple");
        assert_eq!(ComplexityTier::Medium.to_string(), "medium");
        assert_eq!(ComplexityTier::Complex.to_string(), "complex");
    }

    #[test]
    fn test_complexity_tier_serde() {
        let json = serde_json::to_string(&ComplexityTier::Simple).unwrap();
        assert_eq!(json, "\"simple\"");
        let parsed: ComplexityTier = serde_json::from_str("\"complex\"").unwrap();
        assert_eq!(parsed, ComplexityTier::Complex);
    }

    #[test]
    fn test_classify_local_suggested_model_is_tier_name() {
        let simple = classify_local("what is rust?");
        assert_eq!(simple.suggested_model, "simple");

        let complex = classify_local(
            "refactor the entire authentication system to use OAuth2 with comprehensive audit logging across all modules",
        );
        assert_eq!(complex.suggested_model, "complex");
    }

    #[test]
    fn test_resolve_tier_model_prefers_map_then_default() {
        let mut config = crate::shared::Config::default();
        assert_eq!(resolve_tier_model(&config, "simple"), None);

        config.model.default_model = "fallback-model".into();
        assert_eq!(
            resolve_tier_model(&config, "simple"),
            Some("fallback-model".into())
        );

        config
            .model
            .routing_model_map
            .insert("simple".into(), "cheap-model".into());
        assert_eq!(
            resolve_tier_model(&config, "simple"),
            Some("cheap-model".into())
        );

        assert_eq!(
            resolve_tier_model(&config, "unknown"),
            Some("fallback-model".into())
        );
    }

    #[test]
    fn test_resolve_tier_model_skips_empty_map_entries() {
        let mut config = crate::shared::Config::default();
        config.model.default_model = "default-model".into();
        config
            .model
            .routing_model_map
            .insert("simple".into(), String::new());
        assert_eq!(
            resolve_tier_model(&config, "simple"),
            Some("default-model".into()),
            "empty map entry should fall through to default"
        );
    }

    #[test]
    fn test_resolve_tier_model_is_case_insensitive() {
        let mut config = crate::shared::Config::default();
        config
            .model
            .routing_model_map
            .insert("simple".into(), "cheap-model".into());
        assert_eq!(
            resolve_tier_model(&config, "SIMPLE"),
            Some("cheap-model".into())
        );
        assert_eq!(
            resolve_tier_model(&config, "Simple"),
            Some("cheap-model".into())
        );
    }

    #[test]
    fn test_classify_local_long_message_bumps_complexity() {
        let big = "x".repeat(600);
        let result = classify_local(&big);
        assert!(
            matches!(
                result.tier,
                ComplexityTier::Medium | ComplexityTier::Complex
            ),
            "600-char message should bump score, got {:?}",
            result.tier
        );
        let bigger = "y".repeat(1200);
        let result2 = classify_local(&bigger);
        assert!(
            matches!(
                result2.tier,
                ComplexityTier::Medium | ComplexityTier::Complex
            ),
            "1200-char message should bump score, got {:?}",
            result2.tier
        );
    }

    #[test]
    fn test_classify_local_add_keyword_is_medium_or_higher() {
        let result = classify_local("add a feature");
        assert!(
            matches!(
                result.tier,
                ComplexityTier::Medium | ComplexityTier::Complex
            ),
            "'add' is a medium indicator, got {:?}",
            result.tier
        );
    }

    #[test]
    fn test_classify_local_extract_split_keywords_count() {
        let result = classify_local("extract the helper and split the module");
        assert!(
            matches!(
                result.tier,
                ComplexityTier::Medium | ComplexityTier::Complex
            ),
            "extract+split should be at least medium, got {:?}",
            result.tier
        );
    }

    #[test]
    fn test_classify_local_investigate_optimize_security_audit_push_complex() {
        for word in [
            "refactor the system and investigate the bug",
            "optimize the hot path and audit the security model",
            "comprehensive security audit across all modules",
        ] {
            let result = classify_local(word);
            assert_eq!(
                result.tier,
                ComplexityTier::Complex,
                "{word:?} should be complex, got {:?}",
                result.tier
            );
        }
    }

    #[test]
    fn test_complexity_tier_eq_hash() {
        assert_eq!(ComplexityTier::Simple, ComplexityTier::Simple);
        assert_eq!(ComplexityTier::Medium, ComplexityTier::Medium);
        assert_eq!(ComplexityTier::Complex, ComplexityTier::Complex);
        assert_ne!(ComplexityTier::Simple, ComplexityTier::Complex);
    }

    #[test]
    fn test_router_config_default_is_disabled() {
        let cfg = RouterConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.router_model.is_empty());
    }

    #[tokio::test]
    async fn test_classify_disabled_uses_local() {
        let cfg = RouterConfig {
            enabled: false,
            router_model: "ignored".into(),
        };
        let result = classify("what is rust?", &cfg, "http://localhost:11434").await;
        assert_eq!(result.tier, ComplexityTier::Simple);
        assert!(matches!(result.method, RouteMethod::Local));
    }

    #[tokio::test]
    async fn test_classify_enabled_but_no_router_model_uses_local() {
        let cfg = RouterConfig {
            enabled: true,
            router_model: String::new(),
        };
        let result = classify("what is rust?", &cfg, "http://localhost:11434").await;
        assert_eq!(result.tier, ComplexityTier::Simple);
        assert!(matches!(result.method, RouteMethod::Local));
    }

    #[tokio::test]
    async fn test_classify_with_llm_falls_back_on_unreachable_host() {
        let cfg = RouterConfig {
            enabled: true,
            router_model: "test-router-model".into(),
        };
        let result = classify("what is rust?", &cfg, "http://127.0.0.1:1/").await;
        assert!(
            matches!(result.method, RouteMethod::Local),
            "unreachable host should fall back to Local, got {:?}",
            result.method
        );
        assert_eq!(result.tier, ComplexityTier::Simple);
        assert!(
            result.confidence < 0.6,
            "fallback path should downgrade confidence, got {}",
            result.confidence
        );
    }

    #[test]
    fn test_route_method_debug_format() {
        let local = RouteMethod::Local;
        let llm = RouteMethod::Llm;
        let cached = RouteMethod::Cached;
        let s_local = format!("{local:?}");
        let s_llm = format!("{llm:?}");
        let s_cached = format!("{cached:?}");
        assert!(s_local.contains("Local"));
        assert!(s_llm.contains("Llm"));
        assert!(s_cached.contains("Cached"));
    }

    #[test]
    fn test_route_result_debug_format() {
        let result = RouteResult {
            tier: ComplexityTier::Simple,
            confidence: 0.6,
            method: RouteMethod::Local,
            suggested_model: "simple".into(),
        };
        let s = format!("{result:?}");
        assert!(s.contains("RouteResult"));
        assert!(s.contains("Simple"));
    }

    #[test]
    fn test_router_config_serialization_roundtrip() {
        let cfg = RouterConfig {
            enabled: true,
            router_model: "qwen2.5:0.5b".into(),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: RouterConfig = serde_json::from_str(&json).unwrap();
        assert!(back.enabled);
        assert_eq!(back.router_model, "qwen2.5:0.5b");
    }
}
