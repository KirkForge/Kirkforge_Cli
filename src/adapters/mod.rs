pub mod cache;
pub mod caching;
pub mod deepseek;
pub mod gemini;
pub mod glm;
pub mod ollama_ndjson;
pub mod openai_compat;
pub mod tool_call_markup;

use crate::shared::{ContentPart, ModelInfo, Role, StreamEvent};
use std::future::Future;

/// Maximum number of retries for transient model-request failures.
const MODEL_MAX_RETRIES: u32 = 3;

/// Timeout for a single model request. Long-running generation is handled
/// by streaming; this covers the initial HTTP handshake.
const MODEL_REQUEST_TIMEOUT_SECS: u64 = 300;

/// Build a shared `reqwest::Client` for model adapters.
///
/// Falls back to `reqwest::Client::new()` if custom builder configuration
/// fails (e.g. because of an environment-level connector restriction),
/// logging the failure so operators can diagnose it. The fallback client
/// is still fully functional; custom configuration here is only
/// performance tuning (`tcp_nodelay`).
pub fn build_reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_nodelay(true)
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to build custom reqwest client; falling back to default");
            reqwest::Client::new()
        })
}

/// Send a model request with retries for transient failures.
///
/// Retries up to `MODEL_MAX_RETRIES` times on:
/// - connect errors
/// - timeout errors
/// - HTTP 429 / 503
///
/// Uses exponential backoff starting at 1s. Returns the response on the
/// first success, or the final error otherwise. This consolidates the
/// retry logic that was duplicated across `openai_compat`, `deepseek`,
/// `gemini`, and was missing from `glm`.
pub async fn send_with_retry<F, Fut>(
    _client: &reqwest::Client,
    build_request: F,
) -> anyhow::Result<reqwest::Response>
where
    F: Fn() -> Fut,
    Fut: Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match build_request().await {
            Err(e) if attempt < MODEL_MAX_RETRIES && (e.is_connect() || e.is_timeout()) => {
                tracing::warn!(attempt, error = %e, "model request failed, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
            }
            Err(e) => return Err(e.into()),
            Ok(r) => {
                let s = r.status().as_u16();
                if attempt < MODEL_MAX_RETRIES && (s == 429 || s == 503) {
                    tracing::warn!(
                        attempt,
                        status = s,
                        "model returned transient error, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1u64 << (attempt - 1))).await;
                } else {
                    return Ok(r.error_for_status()?);
                }
            }
        }
    }
}

/// Build a message object for OpenAI-compatible requests.
/// When `content_parts` is present and non-empty, emits the vision
/// array shape; otherwise emits a string `content` field.
fn build_content_object(
    role: &Role,
    content: &str,
    parts: Option<&[ContentPart]>,
) -> serde_json::Value {
    match parts {
        Some(parts) if !parts.is_empty() => {
            let mut oai_parts: Vec<serde_json::Value> = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    ContentPart::Text { text } => {
                        oai_parts.push(serde_json::json!({
                            "type": "text",
                            "text": text,
                        }));
                    }
                    ContentPart::Image { data_base64, mime } => {
                        oai_parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime};base64,{data_base64}"),
                            }
                        }));
                    }
                }
            }
            serde_json::json!({"role": role, "content": oai_parts})
        }
        _ => serde_json::json!({"role": role, "content": content}),
    }
}

/// Classification of the runtime protocol a model speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// Native Ollama `/api/chat` protocol (also covers GLM, DeepSeek,
    /// and Gemini when routed through an Ollama host).
    Ollama,
    /// OpenAI-compatible `/v1/chat/completions` protocol.
    OpenAiCompat,
}

/// Classify a model name (and optional type override) into an
/// [`AdapterKind`]. This is the routing decision before we build the
/// concrete adapter.
pub fn adapter_kind_for(model_name: &str, model_type_override: Option<&str>) -> AdapterKind {
    if let Some(override_type) = model_type_override {
        return match override_type {
            "glm" | "deepseek" | "gemini" => AdapterKind::Ollama,
            _ => AdapterKind::OpenAiCompat,
        };
    }

    let lower = model_name.to_lowercase();
    if lower.starts_with("glm")
        || lower.contains("chatglm")
        || lower.starts_with("deepseek")
        || lower.starts_with("gemini")
    {
        AdapterKind::Ollama
    } else {
        AdapterKind::OpenAiCompat
    }
}

/// Every model adapter implements this.
/// `stream()` returns a channel receiver the session drains.
/// The session layer never sees raw JSON — only events.
#[async_trait::async_trait]
pub trait ModelAdapter: Send + Sync {
    fn model_info(&self) -> ModelInfo;

    /// Configure JSON-mode output. Default no-op; adapters that
    /// support `response_format` / `format: "json"` override this.
    /// Called once at construction by the executor with
    /// `config.json_mode` — the executor doesn't have a way to push
    /// the flag through the per-request stream() signature without
    /// breaking the trait, and a per-adapter field is the simplest
    /// place to remember the setting for the lifetime of the
    /// session.
    fn set_json_mode(&mut self, _json_mode: bool) {}

    async fn stream(
        &self,
        messages: &[crate::shared::Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>>;
}

#[cfg(test)]
mod m5_tests;

/// Build the right adapter from a model name string.
pub fn adapter_for(
    model_name: &str,
    ollama_host: &str,
    model_type_override: Option<&str>,
) -> Box<dyn ModelAdapter> {
    let override_lower = model_type_override.map(|s| s.to_lowercase());
    match adapter_kind_for(model_name, model_type_override) {
        AdapterKind::Ollama => {
            let lower = model_name.to_lowercase();
            // Respect the model_type_override when selecting the concrete
            // adapter, so a name like "my-glm" with override "glm" still
            // routes to the GLM adapter rather than falling through to
            // the OpenAI-compat fallback.
            if override_lower.as_deref() == Some("glm")
                || lower.starts_with("glm")
                || lower.contains("chatglm")
            {
                Box::new(glm::GlmAdapter::new(ollama_host, model_name))
            } else if override_lower.as_deref() == Some("deepseek") || lower.starts_with("deepseek")
            {
                Box::new(deepseek::DeepSeekAdapter::new(ollama_host, model_name))
            } else if override_lower.as_deref() == Some("gemini") || lower.starts_with("gemini") {
                Box::new(gemini::GeminiAdapter::new(ollama_host, model_name))
            } else {
                // With the current classification this branch is
                // unreachable, but keep the previous permissive
                // fallback so we never panic on unknown input.
                Box::new(openai_compat::OpenAiCompatAdapter::new(
                    ollama_host,
                    model_name,
                ))
            }
        }
        AdapterKind::OpenAiCompat => Box::new(openai_compat::OpenAiCompatAdapter::new(
            ollama_host,
            model_name,
        )),
    }
}

/// Shared: build the JSON body for `/api/chat`.
///
/// `model_info` controls multimodal + cache_breakpoint behaviour
/// (currently: image-only — Ollama's `/api/chat` has no
/// `cache_control` field, so the cache flag is a no-op here).
/// `json_mode` adds `"format": "json"` at the top level so the model
/// is asked to constrain its output to well-formed JSON.
fn build_ollama_chat_body(
    model: &str,
    _model_info: &crate::shared::ModelInfo,
    messages: &[crate::shared::Message],
    tools: &[crate::shared::ToolDef],
    stream: bool,
    json_mode: bool,
) -> serde_json::Value {
    let ollama_messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            // Content projection: prefer the parts list when set
            // (multimodal); fall through to the legacy `content: String`
            // path otherwise. The text-only projection for a parts list
            // is the concatenation of all `Text` parts (with image
            // placeholders in between), so a model that ignores
            // `images` still sees something coherent.
            let (content_value, images_value) = match &m.content_parts {
                Some(parts) if !parts.is_empty() => {
                    let mut text_projection = String::new();
                    let mut images: Vec<String> = Vec::new();
                    for part in parts {
                        match part {
                            crate::shared::ContentPart::Text { text } => {
                                text_projection.push_str(text);
                            }
                            crate::shared::ContentPart::Image { data_base64, .. } => {
                                if !text_projection.is_empty() && !text_projection.ends_with('\n') {
                                    text_projection.push('\n');
                                }
                                text_projection.push_str("[image]");
                                images.push(data_base64.clone());
                            }
                        }
                    }
                    (
                        serde_json::Value::String(text_projection),
                        if images.is_empty() {
                            None
                        } else {
                            Some(images)
                        },
                    )
                }
                _ => (serde_json::Value::String(m.content.clone()), None),
            };

            let mut obj = serde_json::json!({
                "role": m.role,
                "content": content_value,
            });
            if let Some(imgs) = images_value {
                obj["images"] = serde_json::Value::Array(
                    imgs.into_iter().map(serde_json::Value::String).collect(),
                );
            }
            // GLM puts thinking in its own field at the message level
            if let Some(ref t) = m.thinking {
                obj["thinking"] = serde_json::Value::String(t.clone());
            }
            // Tool results
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = serde_json::Value::String(id.clone());
            }
            obj
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": ollama_messages,
        "stream": stream,
    });

    // Expose tool definitions when they exist
    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    // JSON mode — Ollama's native equivalent of
    // OpenAI's `response_format: {type: "json_object"}`. The regex
    // tool-call extractor in the executor still runs in parallel; this
    // only constrains the *content* stream.
    if json_mode {
        body["format"] = serde_json::Value::String("json".into());
    }

    body
}

/// Shared: build the JSON body for `/v1/chat/completions` (OpenAI-compat).
///
/// `model_info` drives three behaviours:
/// 1. Multimodal — when a message has `content_parts`, emit OpenAI's
///    vision-format content array (`text` + `image_url` parts).
/// 2. Cache breakpoints — when `model_info.supports_cache` is true,
///    mark the last 2 messages of the prefix with
///    `cache_control: {type: "ephemeral"}` so the server can reuse
///    its prompt KV-cache. The trailing user message is *not* marked
///    (it changes every turn).
/// 3. `json_mode` adds `response_format: {type: "json_object"}`
///    and (only when tools are present) `tool_choice: "auto"`.
fn build_openai_compat_body(
    model: &str,
    model_info: &crate::shared::ModelInfo,
    messages: &[crate::shared::Message],
    tools: &[crate::shared::ToolDef],
    json_mode: bool,
) -> serde_json::Value {
    // Pre-compute the indices of the prefix messages that get the
    // cache_control marker. The "prefix" is everything except the
    // trailing user message (the user's question changes every turn;
    // it's the part that never benefits from caching). We mark the
    // last 2 of the prefix — Anthropic-style and OpenAI's
    // `gpt-4o`/`gpt-5` series both accept the marker, and a small
    // breakpoint at the tail covers the longest stable stretch.
    let mut cache_marker_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();
    if model_info.supports_cache && messages.len() > 1 {
        // Skip the last message (the user turn) and the system
        // message (index 0 after assembly — but here we just have the
        // `messages` slice as passed in, so "all but the last"). Mark
        // the last 2 of that range.
        let prefix_end = messages.len() - 1;
        for i in prefix_end.saturating_sub(2)..prefix_end {
            cache_marker_indices.insert(i);
        }
    }

    let oai_messages: Vec<serde_json::Value> = messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let mut obj = match m.role {
                crate::shared::Role::Tool => {
                    serde_json::json!({
                        "role": "tool",
                        "tool_call_id": m.tool_call_id,
                        "content": m.content,
                    })
                }
                crate::shared::Role::Assistant => {
                    if let Some(tcs) = m.tool_calls.as_ref() {
                        let tcs: Vec<serde_json::Value> = tcs
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string(),
                                    }
                                })
                            })
                            .collect();
                        return serde_json::json!({
                            "role": "assistant",
                            "content": m.content,
                            "tool_calls": tcs,
                        });
                    }
                    build_content_object(&m.role, &m.content, m.content_parts.as_deref())
                }
                _ => build_content_object(&m.role, &m.content, m.content_parts.as_deref()),
            };

            // Cache breakpoint — only when this index is in the marker
            // set (i.e. last 2 of the prefix).
            if cache_marker_indices.contains(&idx) {
                obj["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }

            obj
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "messages": oai_messages,
        "stream": true,
    });

    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    // JSON mode — OpenAI's `response_format: {type: "json_object"}`
    // constrains the content stream to well-formed JSON. The
    // `tool_choice: "auto"` field is set only when tools are
    // present (it's meaningless without them) and is the default
    // behaviour anyway — we set it explicitly so the server knows
    // the client opted in. Regex tool-call extraction still runs
    // server-side as a fallback; some models emit `<tool_call>`
    // blocks in-band even with `response_format: json_object`.
    if json_mode {
        body["response_format"] = serde_json::json!({"type": "json_object"});
        if !tools.is_empty() {
            body["tool_choice"] = serde_json::Value::String("auto".into());
        }
    }

    body
}
