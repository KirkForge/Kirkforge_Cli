//! DeepSeek-v4-Pro adapter.
//!
//! DeepSeek sends tool calls as a complete block rather than streaming tokens.
//! Through Ollama's `/api/chat`, tool calls arrive in the final chunk
//! (`done: true`) as a `tool_calls` array on the message object.
//!
//! DeepSeek also supports "chain-of-thought" which arrives as a
//! `reasoning_content` field — analogous to GLM's `thinking`.
//!
//! All NDJSON framing logic lives in [`super::ollama_ndjson`]; this file
//! is just the HTTP glue and the per-adapter config selection.

use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

use super::ollama_ndjson::{self, OllamaNdjsonConfig};
use super::ModelAdapter;

pub struct DeepSeekAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    json_mode: bool,
}

impl DeepSeekAdapter {
    pub fn new(ollama_host: &str, model: &str) -> Self {
        Self {
            model: model.to_string(),
            api_base: ollama_host.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .expect("reqwest client build failed"),
            json_mode: false,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for DeepSeekAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: true, // reasoning_content
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: 64_000,
            recommended_temperature: 0.6,
            supports_images: false, // DeepSeek-V4 cloud has no vision variant
            supports_cache: false,  // Ollama's /api/chat has no cache_control
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
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
        );
        let url = format!("{}/api/chat", self.api_base);

        let response = self
            .client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await?
            .error_for_status()?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(128);

        tokio::spawn(async move {
            let stream = response.bytes_stream();
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::DEEPSEEK, stream)
                .await;
        });

        Ok(rx)
    }
}
