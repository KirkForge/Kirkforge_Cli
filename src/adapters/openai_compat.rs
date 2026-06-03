//! OpenAI-compatible fallback adapter.
//!
//! Uses `/v1/chat/completions` (SSE streaming) instead of `/api/chat` (NDJSON).
//! Activated for any model that doesn't match GLM/DeepSeek/Gemini patterns,
//! or explicitly via `--model-type openai`.
//!
//! Parses SSE `data: {...}` lines. Supports tool calls in the
//! OpenAI function-calling format.

use crate::shared::{
    FinishReason, Message, ModelInfo, StreamEvent, TokenUsage, ToolCallStyle, ToolInvocation,
};
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
            /// Accumulator for OpenAI SSE tool-call deltas.
            ///
            /// OpenAI streams tool calls incrementally across multiple SSE events.
            /// The first delta has `id` and `name`, subsequent deltas only have
            /// `arguments` fragments. Keyed by `index` (0-based within the array).
            ///
            /// Example delta sequence:
            ///   {index: 0, id: "call_1", function: {name: "read_file", arguments: ""}}
            ///   {index: 0, id: null,      function: {name: null,        arguments: "{\"path\":" }}
            ///   {index: 0, id: null,      function: {name: null,        arguments: " \"/etc\"}" }}
            struct ToolCallAccumulator {
                /// Keyed by `index` field from the delta.
                calls: std::collections::HashMap<usize, (String, String, String)>, // (id, name, args_json)
            }

            impl ToolCallAccumulator {
                fn new() -> Self {
                    Self {
                        calls: std::collections::HashMap::new(),
                    }
                }

                /// Accumulate one delta. Merges `arguments` by appending.
                fn accumulate(
                    &mut self,
                    index: usize,
                    id: &str,
                    name: Option<&str>,
                    args: Option<&str>,
                ) {
                    let entry = self
                        .calls
                        .entry(index)
                        .or_insert_with(|| (id.to_string(), String::new(), String::new()));
                    // ID: only set on first delta — keep whatever we get
                    if !id.is_empty() {
                        entry.0 = id.to_string();
                    }
                    // Name: set when present (first delta, usually)
                    if let Some(n) = name {
                        entry.1 = n.to_string();
                    }
                    // Arguments: append incrementally across deltas
                    if let Some(a) = args {
                        entry.2.push_str(a);
                    }
                }

                /// Drain all accumulated calls as ToolInvocation values.
                fn drain(&mut self) -> Vec<ToolInvocation> {
                    let mut out: Vec<_> = self.calls.drain().collect();
                    out.sort_by_key(|(idx, _)| *idx);
                    out.into_iter()
                        .map(|(_, (id, name, args_json))| {
                            // Try to parse the accumulated arguments as JSON
                            let arguments =
                                match serde_json::from_str::<serde_json::Value>(&args_json) {
                                    Ok(v) => v,
                                    Err(_) => serde_json::Value::String(args_json),
                                };
                            ToolInvocation {
                                id,
                                name,
                                arguments,
                            }
                        })
                        .collect()
                }

                fn is_empty(&self) -> bool {
                    self.calls.is_empty()
                }
            }

            let mut pending_tool_calls = ToolCallAccumulator::new();

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
                                    let _ = tx
                                        .send(StreamEvent::Done {
                                            finish_reason: FinishReason::Stop,
                                            usage: None,
                                        })
                                        .await;
                                }
                                continue;
                            }

                            match serde_json::from_str::<serde_json::Value>(&line) {
                                Ok(json) => {
                                    if let Some(err) = json.get("error") {
                                        let _ = tx
                                            .send(StreamEvent::Error(
                                                err.get("message")
                                                    .and_then(|m| m.as_str())
                                                    .unwrap_or("API error")
                                                    .to_string(),
                                            ))
                                            .await;
                                        continue;
                                    }

                                    let choice = json
                                        .get("choices")
                                        .and_then(|c| c.as_array())
                                        .and_then(|c| c.first());

                                    let delta = choice.and_then(|c| c.get("delta"));
                                    let finish = choice.and_then(|c| c.get("finish_reason"));

                                    // Text content
                                    if let Some(content) = delta.and_then(|d| d.get("content")) {
                                        if let Some(c) = content.as_str() {
                                            if !c.is_empty() {
                                                let _ =
                                                    tx.send(StreamEvent::Text(c.to_string())).await;
                                            }
                                        }
                                    }

                                    // Tool calls in delta — accumulate across chunks
                                    if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")) {
                                        if let Some(calls) = tcs.as_array() {
                                            for tc in calls {
                                                let index = tc
                                                    .get("index")
                                                    .and_then(|i| i.as_u64())
                                                    .unwrap_or(0)
                                                    as usize;
                                                let id = tc
                                                    .get("id")
                                                    .and_then(|id| id.as_str())
                                                    .unwrap_or("");
                                                let name = tc
                                                    .get("function")
                                                    .and_then(|f| f.get("name"))
                                                    .and_then(|n| n.as_str());
                                                let args = tc
                                                    .get("function")
                                                    .and_then(|f| f.get("arguments"))
                                                    .and_then(|a| a.as_str());
                                                pending_tool_calls
                                                    .accumulate(index, id, name, args);
                                            }
                                        }
                                    }

                                    // Finish reason signals end
                                    if let Some(reason) = finish.and_then(|r| r.as_str()) {
                                        if reason == "tool_calls" && pending_tool_calls.is_empty() {
                                            let _ = tx.send(StreamEvent::Error(
                                                "Model emitted tool_calls finish_reason but no parseable tool calls".to_string()
                                            )).await;
                                        }
                                        for tc in pending_tool_calls.drain() {
                                            let _ = tx.send(StreamEvent::ToolCall(tc)).await;
                                        }

                                        let finish_reason = match reason {
                                            "length" => FinishReason::Length,
                                            "tool_calls" => FinishReason::ToolCalls,
                                            "error" => FinishReason::Error,
                                            _ => FinishReason::Stop,
                                        };

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
                                        .send(StreamEvent::Error(format!("SSE parse: {}", e)))
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
