//! Gemini 3.0 Flash 1M adapter.
//!
//! Through Ollama, Gemini uses the `/api/chat` endpoint which normalizes the
//! format. Gemini has no thinking field, so its [`OllamaNdjsonConfig`] sets
//! `thinking_field: None` and the shared parser skips that step.
//!
//! All NDJSON framing logic lives in [`super::ollama_ndjson`]; this file
//! is just the HTTP glue and the per-adapter config selection.

use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};

use super::ollama_ndjson::{self, OllamaNdjsonConfig};
use super::ModelAdapter;

pub struct GeminiAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    json_mode: bool,
}

impl GeminiAdapter {
    pub fn new(ollama_host: &str, model: &str) -> Self {
        Self {
            model: model.to_string(),
            api_base: ollama_host.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "failed to build custom reqwest client; falling back to default");
                    reqwest::Client::new()
                }),
            json_mode: false,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for GeminiAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 1_000_000,
            recommended_temperature: 0.8,
            supports_images: true, // Gemini Flash 1M accepts image parts natively
            supports_cache: false, // routed through Ollama; no cache_control
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

        let response = {
            let mut attempt = 0u32;
            loop {
                attempt += 1;
                match self
                    .client
                    .post(&url)
                    .json(&body)
                    .timeout(std::time::Duration::from_secs(300))
                    .send()
                    .await
                {
                    Err(e) if attempt < 3 && (e.is_connect() || e.is_timeout()) => {
                        tracing::warn!(attempt, error = %e, "model request failed, retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1)))
                            .await;
                    }
                    Err(e) => return Err(e.into()),
                    Ok(r) => {
                        let s = r.status().as_u16();
                        if attempt < 3 && (s == 429 || s == 503) {
                            tracing::warn!(attempt, status = s, "model returned transient error, retrying");
                            tokio::time::sleep(
                                std::time::Duration::from_secs(1u64 << (attempt - 1)),
                            )
                            .await;
                        } else {
                            break r.error_for_status()?;
                        }
                    }
                }
            }
        };

        // Channel size: 4096 events. See deepseek.rs for the
        // rationale (2026-06-11 incident — 128 was too small for
        // thinking-model streaming responses, caused "stream
        // consumer dropped receiver mid-stream" warnings).
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);

        tokio::spawn(async move {
            let stream = response.bytes_stream();
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GEMINI, stream).await;
        });

        Ok(rx)
    }
}
