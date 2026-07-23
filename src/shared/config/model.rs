use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

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

fn default_request_timeout_secs() -> u64 {
    120
}

fn default_summarize_model() -> String {
    String::new()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub aws_profile: String,
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
    #[serde(default = "default_zen_endpoint")]
    pub opencode_zen_endpoint: String,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    #[serde(default)]
    pub cache_enabled: bool,
    #[serde(default)]
    pub cache_dir: Option<PathBuf>,
    #[serde(default, skip_serializing)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub json_mode: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default_model: String::new(),
            ollama_host: String::new(),
            summarize_model: String::new(),
            summarize_enabled: false,
            routing_enabled: false,
            router_model: String::new(),
            routing_model_map: HashMap::new(),
            anthropic_provider: default_anthropic_provider(),
            aws_region: default_aws_region(),
            aws_profile: String::new(),
            gcp_service_account_path: None,
            gcp_project_id: String::new(),
            gcp_region: default_gcp_region(),
            subagent_allowed_models: None,
            opencode_zen_api_key: None,
            opencode_zen_endpoint: default_zen_endpoint(),
            request_timeout_secs: default_request_timeout_secs(),
            cache_enabled: false,
            cache_dir: None,
            seed: None,
            json_mode: false,
        }
    }
}
