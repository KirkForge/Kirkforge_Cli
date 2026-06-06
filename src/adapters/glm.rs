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
}

impl GlmAdapter {
    pub fn new(ollama_host: &str, model: &str) -> Self {
        Self {
            model: model.to_string(),
            api_base: ollama_host.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .expect("reqwest client build failed"),
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
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_ollama_chat_body(&self.model, messages, tools, true);
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
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GLM, stream).await;
        });

        Ok(rx)
    }
}
