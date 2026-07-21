//! Kimi/Moonshot adapter.
//!
//! Kimi-2.7k-Coder and related Moonshot models, when routed through an
//! Ollama gateway, stream NDJSON over `/api/chat` and emit chain-of-thought
//! in a `reasoning_content` field (the same shape DeepSeek uses). This
//! adapter separates thinking from content and normalizes tool calls into
//! `StreamEvent` values, just like the GLM/DeepSeek adapters.
//!
//! All NDJSON framing logic lives in [`super::ollama_ndjson`]; this file
//! is only the HTTP glue and per-adapter metadata.

use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

use super::ollama_ndjson::{self, OllamaNdjsonConfig};
use super::ModelAdapter;

pub struct KimiAdapter {
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

impl KimiAdapter {
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
impl ModelAdapter for KimiAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: true,
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: 256_000,
            recommended_temperature: 0.6,
            supports_images: false,
            supports_cache: false,
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
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::KIMI, stream).await;
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_info_reports_thinking_and_native_tools() {
        let adapter = KimiAdapter::new("http://ollama.example", "kimi-2.7k-coder:cloud", 120);
        let info = adapter.model_info();
        assert_eq!(info.name, "kimi-2.7k-coder:cloud");
        assert!(info.supports_thinking);
        assert_eq!(info.tool_call_format, ToolCallStyle::Native);
        assert_eq!(info.max_context_tokens, 256_000);
        assert!(!info.supports_images);
        assert!(!info.supports_cache);
    }
}
