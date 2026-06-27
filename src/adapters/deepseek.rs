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
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "failed to build custom reqwest client; falling back to default");
                    reqwest::Client::new()
                }),
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

        // Channel size: 4096 events. The previous value of 128 was
        // too small for streaming responses from thinking models —
        // a single response can produce 200+ text chunks before the
        // executor drains the receiver, and a full channel blocks
        // `tx.send` which in turn causes the parser to bail with
        // "stream consumer dropped receiver mid-stream" warnings
        // (2026-06-11 incident, see screenshot 1/2/3). 4096 gives
        // ~20x headroom and is still small enough to bound memory.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);

        tokio::spawn(async move {
            let stream = response.bytes_stream();
            ollama_ndjson::parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::DEEPSEEK, stream)
                .await;
        });

        Ok(rx)
    }
}
