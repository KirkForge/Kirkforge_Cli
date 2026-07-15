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
    /// HTTP request timeout in seconds, read from config at construction.
    timeout_secs: u64,
}

impl AdapterSwap {
    /// Create a new swap tracker from the initial adapter's metadata.
    pub fn new(
        model_name: String,
        ollama_host: String,
        model_type_override: Option<String>,
        timeout_secs: u64,
    ) -> Self {
        Self {
            current_model_name: model_name,
            ollama_host,
            model_type_override,
            timeout_secs,
        }
    }

    /// Classify the user input and swap the adapter if the suggested model
    /// differs. Returns `Some(new_model_name)` if a swap occurred, or `None`
    /// if the current model was kept.
    ///
    /// No-op when `config.routing_enabled` is false, or when no concrete
    /// model can be resolved for the tier.
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
        let suggested = router::resolve_tier_model(config, &route.suggested_model)?;

        if suggested == self.current_model_name {
            return None;
        }

        let new_adapter = Self::wrap_cached(
            config,
            adapters::adapter_for(
                &suggested,
                &self.ollama_host,
                self.model_type_override.as_deref(),
                self.timeout_secs,
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
                self.timeout_secs,
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
            "kimi-2.7k-coder:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
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
            "kimi-2.7k-coder:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let mut config = make_config(true);
        config
            .routing_model_map
            .insert("complex".into(), "kimi-2.7k-coder:cloud".into());

        // Complex query → resolves to the same model already in use.
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
            "kimi-2.7k-coder:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let mut config = make_config(true);
        config
            .routing_model_map
            .insert("simple".into(), "qwen3:32b".into());

        // Simple query → resolves to the configured simple-tier model.
        let result = swap.maybe_swap(&config, &mut make_dummy_adapter(), "what is rust?");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "qwen3:32b");
        assert_eq!(swap.current_model_name, "qwen3:32b");
    }

    #[test]
    fn test_swap_medium_to_complex() {
        let mut swap = AdapterSwap::new(
            "glm-5.1:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let mut config = make_config(true);
        config
            .routing_model_map
            .insert("complex".into(), "deepseek-v4-pro:cloud".into());

        // Complex query → resolves to the configured complex-tier model.
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
    fn test_no_swap_when_no_model_resolved() {
        let mut swap = AdapterSwap::new(
            "kimi-2.7k-coder:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let config = make_config(true); // empty map + empty default_model

        let result = swap.maybe_swap(&config, &mut make_dummy_adapter(), "what is rust?");
        assert!(
            result.is_none(),
            "should no-op when no tier model is configured"
        );
    }

    /// `force_swap` installs the named adapter regardless of the
    /// current state. Returns the new name and updates
    /// `current_model_name`. Does not consult the routing model map
    /// — the user's choice is authoritative.
    #[test]
    fn test_force_swap_replaces_current_model() {
        let mut swap = AdapterSwap::new(
            "kimi-2.7k-coder:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let cfg = Config::default();
        let new = swap.force_swap("glm-5.1:cloud", &mut make_dummy_adapter(), &cfg);
        assert_eq!(new, "glm-5.1:cloud");
        assert_eq!(swap.current_model_name, "glm-5.1:cloud");
    }

    /// `force_swap` with the same name is a no-op in effect (it still returns
    /// the name and updates the field). The executor treats this as a benign
    /// "user re-confirmed the same model" gesture.
    #[test]
    fn test_force_swap_same_model_is_noop_in_effect() {
        let mut swap = AdapterSwap::new(
            "glm-5.1:cloud".into(),
            "http://ollama.example".into(),
            None,
            120,
        );
        let cfg = Config::default();
        let new = swap.force_swap("glm-5.1:cloud", &mut make_dummy_adapter(), &cfg);
        assert_eq!(new, "glm-5.1:cloud");
        assert_eq!(swap.current_model_name, "glm-5.1:cloud");
    }

    // --- helpers ---

    /// Minimal adapter for unit tests that don't exercise streaming.
    fn make_dummy_adapter() -> Box<dyn ModelAdapter> {
        use crate::adapters::openai_compat::OpenAiCompatAdapter;
        Box::new(OpenAiCompatAdapter::new(
            "http://ollama.example",
            "kimi-2.7k-coder:cloud",
            120,
        ))
    }
}
