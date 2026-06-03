/// DeepSeek-v4-Pro adapter.
///
/// DeepSeek sends tool calls as a complete block rather than streaming tokens.
/// Through Ollama's `/api/chat`, tool calls arrive in the final chunk (`done: true`)
/// as a `tool_calls` array on the message object.
///
/// DeepSeek also supports "chain-of-thought" which arrives as a `reasoning_content`
/// field — analogous to GLM's `thinking`.

use crate::shared::{FinishReason, Message, ModelInfo, StreamEvent, ToolCallStyle, ToolInvocation, TokenUsage};
use tokio_stream::StreamExt;

use super::ModelAdapter;

pub struct DeepSeekAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
}

impl DeepSeekAdapter {
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
impl ModelAdapter for DeepSeekAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: true, // DeepSeek has reasoning_content
            tool_call_format: ToolCallStyle::Native,
            max_context_tokens: 64_000,
            recommended_temperature: 0.6,
        }
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_ollama_chat_body(&self.model, messages, tools, true);
        let url = format!("{}/api/chat", self.api_base);

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
            let mut tool_calls_buffer: Vec<ToolInvocation> = Vec::new();

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
                                        let _ = tx.send(StreamEvent::Error(
                                            err.as_str().unwrap_or("unknown error").to_string()
                                        )).await;
                                        continue;
                                    }

                                    // DeepSeek sends reasoning_content for CoT
                                    if let Some(reasoning) = json.get("message")
                                        .and_then(|m| m.get("reasoning_content"))
                                    {
                                        if let Some(r) = reasoning.as_str() {
                                            if !r.is_empty() {
                                                let _ = tx.send(StreamEvent::Thinking(r.to_string())).await;
                                            }
                                        }
                                    }

                                    // Text content
                                    if let Some(content) = json.get("message")
                                        .and_then(|m| m.get("content"))
                                    {
                                        if let Some(c) = content.as_str() {
                                            if !c.is_empty() {
                                                let _ = tx.send(StreamEvent::Text(c.to_string())).await;
                                            }
                                        }
                                    }

                                    // Tool calls come in the final chunk
                                    if let Some(tcs) = json.get("message")
                                        .and_then(|m| m.get("tool_calls"))
                                    {
                                        if let Some(calls) = tcs.as_array() {
                                            for tc in calls {
                                                if let (Some(name), Some(args)) = (
                                                    tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()),
                                                    tc.get("function").and_then(|f| f.get("arguments")),
                                                ) {
                                                    tool_calls_buffer.push(ToolInvocation {
                                                        id: tc.get("id").and_then(|id| id.as_str()).unwrap_or("").to_string(),
                                                        name: name.to_string(),
                                                        arguments: args.clone(),
                                                    });
                                                }
                                            }
                                        }
                                    }

                                    if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                                        for tc in tool_calls_buffer.drain(..) {
                                            let _ = tx.send(StreamEvent::ToolCall(tc)).await;
                                        }

                                        let usage = json.get("usage").map(|u| {
                                            TokenUsage {
                                                prompt_tokens: u.get("prompt_tokens").and_then(|v| v.as_u64()).map(|v| v as usize),
                                                completion_tokens: u.get("completion_tokens").and_then(|v| v.as_u64()).map(|v| v as usize),
                                            }
                                        });

                                        let reason = json.get("done_reason")
                                            .and_then(|r| r.as_str())
                                            .unwrap_or("stop");

                                        let finish_reason = match reason {
                                            "length" => FinishReason::Length,
                                            "tool_calls" => FinishReason::ToolCalls,
                                            "error" => FinishReason::Error,
                                            _ => FinishReason::Stop,
                                        };

                                        let _ = tx.send(StreamEvent::Done { finish_reason, usage }).await;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(StreamEvent::Error(format!("JSON parse: {}", e))).await;
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