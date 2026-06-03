/// OpenAI-compatible fallback adapter.
///
/// Uses `/v1/chat/completions` (SSE streaming) instead of `/api/chat` (NDJSON).
/// Activated for any model that doesn't match GLM/DeepSeek/Gemini patterns,
/// or explicitly via `--model-type openai`.
///
/// Parses SSE `data: {...}` lines. Supports tool calls in the
/// OpenAI function-calling format.

use crate::shared::{FinishReason, Message, ModelInfo, StreamEvent, ToolCallStyle, ToolInvocation, TokenUsage};
use tokio_stream::StreamExt;

use super::ModelAdapter;

pub struct OpenAiCompatAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
}

impl OpenAiCompatAdapter {
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
impl ModelAdapter for OpenAiCompatAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 32_768, // conservative default
            recommended_temperature: 0.7,
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_openai_compat_body(&self.model, messages, tools);
        let url = format!("{}/v1/chat/completions", self.api_base);

        let response = self.client.post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await?
            .error_for_status()?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(128);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut pending_tool_calls: Vec<ToolInvocation> = Vec::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

                        // SSE: data: {...}\n\n
                        while let Some(start) = buffer.find("data: ") {
                            let after_data = &buffer[start + 6..];
                            let end = after_data.find("\n\n").unwrap_or(after_data.len());
                            let line: String = after_data[..end].trim().to_string();
                            buffer.drain(..=start + 6 + end);

                            if line.is_empty() || line == "[DONE]" {
                                if line == "[DONE]" {
                                    let _ = tx.send(StreamEvent::Done {
                                        finish_reason: FinishReason::Stop,
                                        usage: None,
                                    }).await;
                                }
                                continue;
                            }

                            match serde_json::from_str::<serde_json::Value>(&line) {
                                Ok(json) => {
                                    if let Some(err) = json.get("error") {
                                        let _ = tx.send(StreamEvent::Error(
                                            err.get("message").and_then(|m| m.as_str()).unwrap_or("API error").to_string()
                                        )).await;
                                        continue;
                                    }

                                    let choice = json.get("choices")
                                        .and_then(|c| c.as_array())
                                        .and_then(|c| c.first());

                                    let delta = choice.and_then(|c| c.get("delta"));
                                    let finish = choice.and_then(|c| c.get("finish_reason"));

                                    // Text content
                                    if let Some(content) = delta.and_then(|d| d.get("content")) {
                                        if let Some(c) = content.as_str() {
                                            if !c.is_empty() {
                                                let _ = tx.send(StreamEvent::Text(c.to_string())).await;
                                            }
                                        }
                                    }

                                    // Tool calls in delta
                                    if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")) {
                                        if let Some(calls) = tcs.as_array() {
                                            for tc in calls {
                                                if let (Some(name), Some(args)) = (
                                                    tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()),
                                                    tc.get("function").and_then(|f| f.get("arguments")),
                                                ) {
                                                    // OpenAI streams tool call arguments across multiple deltas.
                                                    // For simplicity, collect the full call when name is present.
                                                    pending_tool_calls.push(ToolInvocation {
                                                        id: tc.get("id").and_then(|id| id.as_str()).unwrap_or("").to_string(),
                                                        name: name.to_string(),
                                                        arguments: args.clone(),
                                                    });
                                                }
                                            }
                                        }
                                    }

                                    // Finish reason signals end
                                    if let Some(reason) = finish.and_then(|r| r.as_str()) {
                                        for tc in pending_tool_calls.drain(..) {
                                            let _ = tx.send(StreamEvent::ToolCall(tc)).await;
                                        }

                                        let finish_reason = match reason {
                                            "length" => FinishReason::Length,
                                            "tool_calls" => FinishReason::ToolCalls,
                                            "error" => FinishReason::Error,
                                            _ => FinishReason::Stop,
                                        };

                                        let usage = json.get("usage").map(|u| {
                                            TokenUsage {
                                                prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).map(|v| v as usize),
                                                completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).map(|v| v as usize),
                                            }
                                        });

                                        let _ = tx.send(StreamEvent::Done { finish_reason, usage }).await;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(StreamEvent::Error(format!("SSE parse: {}", e))).await;
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