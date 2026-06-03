//! Gemini 3.0 Flash 1M adapter.
//!
//! Through Ollama, Gemini uses the OpenAI-compatible `/v1/chat/completions` path.
//! It doesn't have a thinking field or native tool calls — Ollama translates
//! function calling into Gemini's tool format.
//!
//! Gemini streams token-by-token with SSE (not NDJSON like `/api/chat`),
//! and chunk boundaries differ from the other models. We use Ollama's
//! `/api/chat` endpoint which normalizes the format, but Gemini still has
//! distinct behavior: no thinking field, different tool call batching.

use crate::shared::{
    FinishReason, Message, ModelInfo, StreamEvent, TokenUsage, ToolCallStyle, ToolInvocation,
};
use tokio_stream::StreamExt;

use super::ModelAdapter;

pub struct GeminiAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
}

impl GeminiAdapter {
    pub fn new(ollama_host: &str, model: &str) -> Self {
        let api_base = ollama_host.trim_end_matches('/').to_string();
        Self {
            model: model.to_string(),
            api_base,
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .expect("reqwest client build failed"),
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
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        while let Some(newline_pos) = buffer.find('\n') {
                            let line: String = buffer.drain(..=newline_pos).collect();
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }

                            match serde_json::from_str::<serde_json::Value>(line) {
                                Ok(json) => {
                                    if let Some(err) = json.get("error") {
                                        let _ = tx
                                            .send(StreamEvent::Error(
                                                err.as_str().unwrap_or("unknown error").to_string(),
                                            ))
                                            .await;
                                        continue;
                                    }

                                    // Gemini has no thinking field, just content
                                    if let Some(content) =
                                        json.get("message").and_then(|m| m.get("content"))
                                    {
                                        if let Some(c) = content.as_str() {
                                            if !c.is_empty() {
                                                let _ =
                                                    tx.send(StreamEvent::Text(c.to_string())).await;
                                            }
                                        }
                                    }

                                    // Tool calls (OpenAI-compat style through Ollama)
                                    if let Some(tcs) =
                                        json.get("message").and_then(|m| m.get("tool_calls"))
                                    {
                                        if let Some(calls) = tcs.as_array() {
                                            let mut parsed_any = false;
                                            for tc in calls {
                                                if let (Some(name), Some(args)) = (
                                                    tc.get("function")
                                                        .and_then(|f| f.get("name"))
                                                        .and_then(|n| n.as_str()),
                                                    tc.get("function")
                                                        .and_then(|f| f.get("arguments")),
                                                ) {
                                                    parsed_any = true;
                                                    let _ = tx
                                                        .send(StreamEvent::ToolCall(
                                                            ToolInvocation {
                                                                id: tc
                                                                    .get("id")
                                                                    .and_then(|id| id.as_str())
                                                                    .unwrap_or("")
                                                                    .to_string(),
                                                                name: name.to_string(),
                                                                arguments: args.clone(),
                                                            },
                                                        ))
                                                        .await;
                                                }
                                            }
                                            if !calls.is_empty() && !parsed_any {
                                                let _ = tx.send(StreamEvent::Error(
                                                    "Model emitted tool_calls with no parseable entries".to_string()
                                                )).await;
                                            }
                                        }
                                    }

                                    if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                                        let usage = json.get("usage").map(|u| TokenUsage {
                                            prompt_tokens: u
                                                .get("prompt_tokens")
                                                .and_then(|v| v.as_u64())
                                                .map(|v| v as usize),
                                            completion_tokens: u
                                                .get("completion_tokens")
                                                .and_then(|v| v.as_u64())
                                                .map(|v| v as usize),
                                        });

                                        let reason = json
                                            .get("done_reason")
                                            .and_then(|r| r.as_str())
                                            .unwrap_or("stop");

                                        let finish_reason = match reason {
                                            "length" => FinishReason::Length,
                                            "tool_calls" => FinishReason::ToolCalls,
                                            "error" => FinishReason::Error,
                                            _ => FinishReason::Stop,
                                        };

                                        let _ = tx
                                            .send(StreamEvent::Done {
                                                finish_reason,
                                                usage,
                                            })
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(StreamEvent::Error(format!("JSON parse: {}", e)))
                                        .await;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(StreamEvent::Error(e.to_string())).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
    }
}
