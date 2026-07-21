//! GLM-5.1:Cloud adapter.
//!
//! GLM emits a `thinking` field alongside `content` in `/api/chat` responses.
//! This adapter splits them into separate StreamEvent variants so the TUI
//! can show thinking in a collapsible panel and the session never feeds it
//! back as input.
//!
//! All NDJSON framing logic lives in [`super::ollama_ndjson`]; this file
//! is just the HTTP glue and the per-adapter config selection.

use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

use super::ollama_ndjson::{self, OllamaNdjsonConfig};
use super::ModelAdapter;

pub struct GlmAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    /// JSON-mode flag, set by the executor at construction time from
    /// `Config::json_mode`. Default `false`. The body builder reads it
    /// to add `"format": "json"` at the top level of the request.
    json_mode: bool,
    seed: Option<u64>,
    timeout_secs: u64,
}

impl GlmAdapter {
    pub fn new(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        Self {
            model: model.to_string(),
            api_base: ollama_host.trim_end_matches('/').to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            seed: None,
            timeout_secs,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for GlmAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: true,
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: 128_000,
            recommended_temperature: 0.7,
            supports_images: false, // GLM 5.1 cloud has no vision variant
            supports_cache: false,  // Ollama's /api/chat has no cache_control
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
    }

    fn set_seed(&mut self, seed: Option<u64>) {
        self.seed = seed;
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_ollama_chat_body(
            &self.model,
            &self.model_info(),
            messages,
            tools,
            true,
            self.json_mode,
            self.seed,
        );
        let url = format!("{}/api/chat", self.api_base);

        let response = super::send_with_retry(|| async {
            self.client
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
        })
        .await?;

        // Channel size: 4096 events. See deepseek.rs for the
        // rationale (2026-06-11 incident).
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);

        tokio::spawn(async move {
            let stream = response.bytes_stream();
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GLM, stream).await;
        });

        Ok(rx)
    }
}
