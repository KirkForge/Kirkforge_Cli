use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::shared::ResponseFormat;

fn default_anthropic_provider() -> String {
    "anthropic".to_string()
}

fn default_aws_region() -> String {
    "us-east-1".to_string()
}

fn default_zen_endpoint() -> String {
    "https://opencode.ai/zen/v1/chat/completions".to_string()
}

fn default_gcp_region() -> String {
    "us-central1".to_string()
}

fn default_max_bg_tasks() -> usize {
    4
}

fn default_request_timeout_secs() -> u64 {
    120
}

fn default_streaming_timeout_secs() -> u64 {
    180
}

fn default_true() -> bool {
    true
}

fn default_budget_tokens() -> usize {
    10_000
}

fn default_max_tokens() -> u32 {
    8192
}

fn default_summarize_model() -> String {
    String::new()
}

/// Optional provider override for subagents (WO 30.0.6 brain+brawn).
///
/// Every field is optional; an unset field inherits the parent's value
/// (the corresponding `ModelConfig` field). Setting `model` lets the
/// subagent run on a different adapter; setting `ollama_host` and the
/// per-provider API keys routes it to a different account/endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubagentProvider {
    /// Model name for subagents. When `None`, the parent's
    /// `default_model` is used (or the `task` tool's `model` arg, which
    /// takes precedence over both).
    #[serde(default)]
    pub model: Option<String>,
    /// Ollama host override. When `None` or empty, inherits parent's
    /// `ollama_host`.
    #[serde(default)]
    pub ollama_host: Option<String>,
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub deepseek_api_key: Option<String>,
    #[serde(default)]
    pub gemini_api_key: Option<String>,
    #[serde(default)]
    pub kimi_api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub default_model: String,
    pub ollama_host: String,
    #[serde(default = "default_summarize_model")]
    pub summarize_model: String,
    #[serde(default)]
    pub summarize_enabled: bool,
    #[serde(default)]
    pub routing_enabled: bool,
    #[serde(default)]
    pub router_model: String,
    #[serde(default)]
    pub routing_model_map: HashMap<String, String>,
    #[serde(default = "default_anthropic_provider")]
    pub anthropic_provider: String,
    #[serde(default = "default_aws_region")]
    pub aws_region: String,
    #[serde(default)]
    pub gcp_service_account_path: Option<PathBuf>,
    #[serde(default)]
    pub gcp_project_id: String,
    #[serde(default = "default_gcp_region")]
    pub gcp_region: String,
    #[serde(default)]
    pub subagent_allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub opencode_zen_api_key: Option<String>,
    /// Per-provider API keys, resolved in order: config field →
    /// `<PROVIDER>_API_KEY` env → keychain (Series 18).
    #[serde(default)]
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub openai_api_key: Option<String>,
    #[serde(default)]
    pub deepseek_api_key: Option<String>,
    #[serde(default)]
    pub gemini_api_key: Option<String>,
    #[serde(default)]
    pub kimi_api_key: Option<String>,
    #[serde(default = "default_zen_endpoint")]
    pub opencode_zen_endpoint: String,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Per-adapter streaming idle timeout in seconds. If no byte arrives
    /// from the model within this window, the stream is aborted with a
    /// `StreamEvent::Error`. Default 180 — well above any realistic
    /// inter-token silence, but turns a wedged HTTP body into a clean
    /// error instead of hanging the agent loop forever (WO 32.13).
    #[serde(default = "default_streaming_timeout_secs")]
    pub streaming_timeout_secs: u64,
    #[serde(default)]
    pub cache_enabled: bool,
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub json_mode: bool,
    #[serde(default = "default_max_bg_tasks")]
    pub max_concurrent_background_tasks: usize,
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,
    /// Enable extended thinking for models that support it (e.g. Claude
    /// 3.7 Sonnet, Claude 4). When false, thinking blocks are omitted
    /// even if the model supports them.
    #[serde(default = "default_true")]
    pub extended_thinking: bool,
    /// Budget tokens for extended thinking. Default 10000.
    #[serde(default = "default_budget_tokens")]
    pub budget_tokens: usize,
    /// Maximum tokens for completions (wo/20.2.0). Default 8192.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// User-supplied model→adapter routing overrides. Maps model-name
    /// prefixes (e.g. `"claude-"`) to [`AdapterKind`](super::adapters::AdapterKind)
    /// variant names (e.g. `"Anthropic"`). Checked by
    /// [`adapter_kind_for_routed`] before the hardcoded defaults; longest
    /// prefix match wins. Empty by default, so existing configs with no
    /// `[adapter_routing]` section continue to work identically.
    #[serde(default)]
    pub adapter_routing: HashMap<String, String>,
    /// Optional subagent provider override (WO 30.0.6). When a field is
    /// `None`, the subagent inherits the parent's value.
    #[serde(default)]
    pub subagent_provider: SubagentProvider,
    /// Config-driven pricing overrides (WO 38.5): model-name prefix →
    /// per-Mtok USD rates. Longest matching prefix wins over the
    /// built-in `PRICING_TABLE`, so unmapped/self-hosted models can be
    /// priced without a code change.
    #[serde(default)]
    pub price_overrides: HashMap<String, crate::shared::ModelPrice>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default_model: String::new(),
            ollama_host: "http://localhost:11434".to_string(),
            summarize_model: String::new(),
            summarize_enabled: false,
            routing_enabled: false,
            router_model: String::new(),
            routing_model_map: HashMap::new(),
            anthropic_provider: default_anthropic_provider(),
            aws_region: default_aws_region(),
            gcp_service_account_path: None,
            gcp_project_id: String::new(),
            gcp_region: default_gcp_region(),
            subagent_allowed_models: None,
            opencode_zen_api_key: None,
            anthropic_api_key: None,
            openai_api_key: None,
            deepseek_api_key: None,
            gemini_api_key: None,
            kimi_api_key: None,
            opencode_zen_endpoint: default_zen_endpoint(),
            request_timeout_secs: default_request_timeout_secs(),
            streaming_timeout_secs: default_streaming_timeout_secs(),
            cache_enabled: false,
            cache_dir: None,
            seed: None,
            json_mode: false,
            max_concurrent_background_tasks: default_max_bg_tasks(),
            response_format: None,
            extended_thinking: true,
            budget_tokens: default_budget_tokens(),
            max_tokens: default_max_tokens(),
            adapter_routing: HashMap::new(),
            subagent_provider: SubagentProvider::default(),
            price_overrides: HashMap::new(),
        }
    }
}

impl ModelConfig {
    pub fn effective_response_format(&self) -> ResponseFormat {
        self.response_format.clone().unwrap_or(if self.json_mode {
            ResponseFormat::JsonObject
        } else {
            ResponseFormat::Text
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_response_format_prefers_explicit_over_json_mode() {
        let cfg = ModelConfig {
            json_mode: true,
            response_format: Some(ResponseFormat::Text),
            ..Default::default()
        };
        assert_eq!(cfg.effective_response_format(), ResponseFormat::Text);
    }

    #[test]
    fn effective_response_format_json_mode_true_maps_to_json_object() {
        let cfg = ModelConfig {
            json_mode: true,
            response_format: None,
            ..Default::default()
        };
        assert_eq!(cfg.effective_response_format(), ResponseFormat::JsonObject);
    }

    #[test]
    fn effective_response_format_default_is_text() {
        let cfg = ModelConfig::default();
        assert_eq!(cfg.effective_response_format(), ResponseFormat::Text);
    }

    // WO 30.0.6: subagent_provider defaults to all-None (inherit parent).
    #[test]
    fn subagent_provider_default_inherits_parent() {
        let sub = SubagentProvider::default();
        assert!(sub.model.is_none());
        assert!(sub.ollama_host.is_none());
        assert!(sub.anthropic_api_key.is_none());
        assert!(sub.openai_api_key.is_none());
        assert!(sub.deepseek_api_key.is_none());
        assert!(sub.gemini_api_key.is_none());
        assert!(sub.kimi_api_key.is_none());
        // ModelConfig exposes the sub-struct with a default.
        assert!(ModelConfig::default().subagent_provider.model.is_none());
    }
}
