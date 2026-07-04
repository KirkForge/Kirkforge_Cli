//! Adapter hot-swap — mid-session model switching based on task complexity.
//!
//! When `routing_enabled` is true, the first user message of each turn is
//! classified (via `router::classify_local` by default) to determine the
//! optimal model for the task. If the suggested model differs from the
//! currently active one, the adapter is swapped in-place via `adapter_for()`.
//!
//! # Design
//!
//! - Classification is synchronous (keyword heuristics) — no extra API calls.
//!   An optional `routing_model_map` in Config can override per-tier model
//!   selections.
//! - The swap replaces the `Box<dyn ModelAdapter>` in the Executor, dropping
//!   any in-flight connections from the previous adapter.
//! - A swap event is emitted as a TurnEvent token so the user sees the switch.

use crate::adapters::cache::ResponseCache;
use crate::adapters::{self, caching::CachingAdapter, ModelAdapter};
use crate::session::router;
use crate::shared::Config;

/// Tracks the current active model and handles swapping.
pub struct AdapterSwap {
    /// The model name currently in use.
    pub current_model_name: String,
    /// Ollama host URL (doesn't change across swaps).
    ollama_host: String,
    /// Model-type override hint (GLM/DeepSeek/Gemini), preserved from session
    /// startup.
    model_type_override: Option<String>,
}

impl AdapterSwap {
    /// Create a new swap tracker from the initial adapter's metadata.
    pub fn new(
        model_name: String,
        ollama_host: String,
        model_type_override: Option<String>,
    ) -> Self {
        Self {
            current_model_name: model_name,
            ollama_host,
            model_type_override,
        }
    }

    /// Classify the user input and swap the adapter if the suggested model
    /// differs. Returns `Some(new_model_name)` if a swap occurred, or `None`
    /// if the current model was kept.
    ///
    /// No-op when `config.routing_enabled` is false.
    pub fn maybe_swap(
        &mut self,
        config: &Config,
        adapter: &mut Box<dyn ModelAdapter>,
        user_input: &str,
    ) -> Option<String> {
        if !config.routing_enabled {
            return None;
        }

        let route = router::classify_local(user_input);
        let suggested = self.resolve_model(config, &route.suggested_model);

        if suggested == self.current_model_name {
            return None;
        }

        let new_adapter = Self::wrap_cached(
            config,
            adapters::adapter_for(
                &suggested,
                &self.ollama_host,
                self.model_type_override.as_deref(),
            ),
        );

        let _old = std::mem::replace(adapter, new_adapter);
        // _old is dropped here, releasing any in-flight connections

        self.current_model_name = suggested.clone();
        Some(suggested)
    }

    /// Install a specific model adapter, bypassing routing. Used by
    /// the `/model <name>` slash command when the user wants to pin a
    /// particular model for the remainder of the session (or until
    /// they `/model` again).
    ///
    /// Mirrors the body of `maybe_swap` but skips `classify_local` and
    /// the per-tier `routing_model_map` lookup — the user's explicit
    /// choice wins. Returns the name of the newly-installed adapter
    /// (which is the same as the input, after re-routing through
    /// `adapters::adapter_for` for any model-type inference).
    pub fn force_swap(
        &mut self,
        model_name: &str,
        adapter: &mut Box<dyn ModelAdapter>,
        config: &Config,
    ) -> String {
        let new_adapter = Self::wrap_cached(
            config,
            adapters::adapter_for(
                model_name,
                &self.ollama_host,
                self.model_type_override.as_deref(),
            ),
        );
        let _old = std::mem::replace(adapter, new_adapter);
        // _old is dropped here, releasing any in-flight connections
        self.current_model_name = model_name.to_string();
        model_name.to_string()
    }

    /// Wrap a freshly-constructed adapter in the response cache when
    /// caching is enabled, preserving the config's `json_mode` flag.
    fn wrap_cached(config: &Config, adapter: Box<dyn ModelAdapter>) -> Box<dyn ModelAdapter> {
        if config.cache_enabled {
            let cache = ResponseCache::new(true, config.cache_dir.clone());
            Box::new(CachingAdapter::new(adapter, cache, config.json_mode))
        } else {
            adapter
        }
    }

    /// Resolve the final model name: consult the user's configured
    /// per-tier mapping first, then fall back to the router's built-in
    /// suggestion.
    fn resolve_model(&self, config: &Config, default: &str) -> String {
        if config.routing_model_map.is_empty() {
            return default.to_string();
        }
        // The map keys are tier names: "simple", "medium", "complex".
        // If the suggested default model matches a known tier, look it up.
        // Otherwise fall through to the default.
        for (tier, model) in &config.routing_model_map {
            let tier_lower = tier.to_lowercase();
            if (tier_lower == "simple" && default.contains("qwen"))
                || (tier_lower == "medium" && default.contains("flash"))
                || (tier_lower == "complex" && default.contains("pro"))
            {
                return model.clone();
            }
        }
        default.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(routing_enabled: bool) -> Config {
        Config {
            routing_enabled,
            ..Default::default()
        }
    }

    #[test]
    fn test_no_swap_when_routing_disabled() {
        let mut swap = AdapterSwap::new(
            "deepseek-v4-pro:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let config = make_config(false);

        // Without a real adapter we can't call adapter_for() with a mock,
        // but we can test the early-return guard.
        let result = swap.maybe_swap(
            &config,
            &mut make_dummy_adapter(),
            "refactor the auth system",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_no_swap_when_same_model_suggested() {
        let mut swap = AdapterSwap::new(
            "deepseek-v4-pro:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let config = make_config(true);

        // Complex query → suggests deepseek-v4-pro:cloud (same as current)
        let result = swap.maybe_swap(
            &config,
            &mut make_dummy_adapter(),
            "refactor the entire authentication system to use OAuth2 with comprehensive error handling",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_swap_on_simple_input_with_pro_model() {
        let mut swap = AdapterSwap::new(
            "deepseek-v4-pro:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let config = make_config(true);

        // Simple query → suggests qwen2.5:3b (different from current pro)
        let result = swap.maybe_swap(&config, &mut make_dummy_adapter(), "what is rust?");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "qwen2.5:3b");
        assert_eq!(swap.current_model_name, "qwen2.5:3b");
    }

    #[test]
    fn test_swap_medium_to_complex() {
        let mut swap = AdapterSwap::new(
            "deepseek-v4-flash:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let config = make_config(true);

        // Complex query → suggests deepseek-v4-pro:cloud
        let result = swap.maybe_swap(
            &config,
            &mut make_dummy_adapter(),
            "refactor the executor to support hot-swap and implement comprehensive tests across all modules",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "deepseek-v4-pro:cloud");
        assert_eq!(swap.current_model_name, "deepseek-v4-pro:cloud");
    }

    #[test]
    fn test_resolve_model_with_empty_map() {
        let swap = AdapterSwap::new(
            "deepseek-v4-flash:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let config = Config::default();
        // Map is empty — should return the default
        assert_eq!(
            swap.resolve_model(&config, "deepseek-v4-pro:cloud"),
            "deepseek-v4-pro:cloud"
        );
    }

    #[test]
    fn test_resolve_model_with_custom_map() {
        let swap = AdapterSwap::new(
            "deepseek-v4-flash:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let mut config = Config::default();
        config
            .routing_model_map
            .insert("complex".into(), "my-custom-model:latest".into());
        // The default is "deepseek-v4-pro:cloud" which matches the "complex"
        // tier heuristic (contains "pro")
        assert_eq!(
            swap.resolve_model(&config, "deepseek-v4-pro:cloud"),
            "my-custom-model:latest"
        );
    }

    /// `force_swap` installs the named adapter regardless of the
    /// current state. Returns the new name and updates
    /// `current_model_name`. Does not consult the routing model map
    /// — the user's choice is authoritative.
    #[test]
    fn test_force_swap_replaces_current_model() {
        let mut swap = AdapterSwap::new(
            "deepseek-v4-pro:cloud".into(),
            "http://localhost:11434".into(),
            None,
        );
        let cfg = Config::default();
        let new = swap.force_swap("qwen2.5:3b", &mut make_dummy_adapter(), &cfg);
        assert_eq!(new, "qwen2.5:3b");
        assert_eq!(swap.current_model_name, "qwen2.5:3b");
    }

    /// `force_swap` with the same name is a no-op (it still returns
    /// the name and updates the field, but the visible effect is
    /// null). The executor treats this as a benign "user re-confirmed
    /// the same model" gesture.
    #[test]
    fn test_force_swap_same_model_is_noop_in_effect() {
        let mut swap = AdapterSwap::new("qwen2.5:3b".into(), "http://localhost:11434".into(), None);
        let cfg = Config::default();
        let new = swap.force_swap("qwen2.5:3b", &mut make_dummy_adapter(), &cfg);
        assert_eq!(new, "qwen2.5:3b");
        assert_eq!(swap.current_model_name, "qwen2.5:3b");
    }

    // --- helpers ---

    /// Minimal adapter for unit tests that don't exercise streaming.
    fn make_dummy_adapter() -> Box<dyn ModelAdapter> {
        use crate::adapters::openai_compat::OpenAiCompatAdapter;
        Box::new(OpenAiCompatAdapter::new(
            "http://localhost:11434",
            "deepseek-v4-pro:cloud",
        ))
    }
}
