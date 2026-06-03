pub mod glm;
pub mod deepseek;
pub mod gemini;
pub mod openai_compat;

use crate::shared::{ModelInfo, StreamEvent};

/// Every model adapter implements this.
/// `stream()` returns a channel receiver the session drains.
/// The session layer never sees raw JSON — only events.
#[async_trait::async_trait]
pub trait ModelAdapter: Send + Sync {
    fn model_info(&self) -> ModelInfo;

    async fn stream(
        &self,
        messages: &[crate::shared::Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>>;
}

/// Build the right adapter from a model name string.
pub fn adapter_for(
    model_name: &str,
    ollama_host: &str,
    model_type_override: Option<&str>,
) -> Box<dyn ModelAdapter> {
    if let Some(override_type) = model_type_override {
        return match override_type {
            "glm" => Box::new(glm::GlmAdapter::new(ollama_host, model_name)),
            "deepseek" => Box::new(deepseek::DeepSeekAdapter::new(ollama_host, model_name)),
            "gemini" => Box::new(gemini::GeminiAdapter::new(ollama_host, model_name)),
            _ => Box::new(openai_compat::OpenAiCompatAdapter::new(ollama_host, model_name)),
        };
    }

    let lower = model_name.to_lowercase();
    if lower.starts_with("glm") || lower.contains("chatglm") {
        Box::new(glm::GlmAdapter::new(ollama_host, model_name))
    } else if lower.starts_with("deepseek") {
        Box::new(deepseek::DeepSeekAdapter::new(ollama_host, model_name))
    } else if lower.starts_with("gemini") {
        Box::new(gemini::GeminiAdapter::new(ollama_host, model_name))
    } else {
        Box::new(openai_compat::OpenAiCompatAdapter::new(ollama_host, model_name))
    }
}

/// Shared: build the JSON body for `/api/chat`.
fn build_ollama_chat_body(
    model: &str,
    messages: &[crate::shared::Message],
    tools: &[crate::shared::ToolDef],
    stream: bool,
) -> serde_json::Value {
    let ollama_messages: Vec<serde_json::Value> = messages.iter().map(|m| {
        let mut obj = serde_json::json!({
            "role": m.role,
            "content": m.content,
        });
        // GLM puts thinking in its own field at the message level
        if let Some(ref t) = m.thinking {
            obj["thinking"] = serde_json::Value::String(t.clone());
        }
        // Tool results
        if let Some(ref id) = m.tool_call_id {
            obj["tool_call_id"] = serde_json::Value::String(id.clone());
        }
        obj
    }).collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": ollama_messages,
        "stream": stream,
    });

    // Expose tool definitions when they exist
    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools.iter().map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        }).collect();
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    body
}

/// Shared: build the JSON body for `/v1/chat/completions` (OpenAI-compat).
fn build_openai_compat_body(
    model: &str,
    messages: &[crate::shared::Message],
    tools: &[crate::shared::ToolDef],
) -> serde_json::Value {
    let oai_messages: Vec<serde_json::Value> = messages.iter().map(|m| {
        match m.role {
            crate::shared::Role::Tool => {
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id,
                    "content": m.content,
                })
            }
            crate::shared::Role::Assistant if m.tool_calls.is_some() => {
                let tcs: Vec<serde_json::Value> = m.tool_calls.as_ref().unwrap().iter().map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments.to_string(),
                        }
                    })
                }).collect();
                serde_json::json!({
                    "role": "assistant",
                    "content": m.content,
                    "tool_calls": tcs,
                })
            }
            _ => {
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            }
        }
    }).collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": oai_messages,
        "stream": true,
    });

    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools.iter().map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            })
        }).collect();
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    body
}