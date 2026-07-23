pub mod anthropic;
pub mod anthropic_bedrock;
pub mod anthropic_vertex;
pub mod bedrock_signing;
pub mod cache;
pub mod caching;
pub mod deepseek;
pub mod gemini;
pub mod glm;
pub mod kimi;
pub mod ollama_ndjson;
pub mod openai_compat;
pub mod tool_call_markup;
pub mod vertex_auth;

use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{ContentPart, ModelInfo, Role, StreamEvent};
use std::future::Future;

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

/// Maximum number of retries for transient model-request failures.
const MODEL_MAX_RETRIES: u32 = 3;

/// Decide whether an HTTP status code warrants a retry.
///
/// Retry on 429 (rate limit) and the whole 5xx range. Fail fast on any
/// other 4xx — the request is malformed or unauthorized and repeating it
/// will not help.
pub(crate) fn should_retry_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

pub use crate::shared::backoff::retry_backoff;

/// Send a model request with retries for transient failures.
///
/// Retries up to `MODEL_MAX_RETRIES` times on:
/// - connect errors
/// - timeout errors
/// - HTTP 429 / 5xx
///
/// Uses exponential backoff with capped deterministic jitter. Returns the
/// response on the first success, or the final error otherwise. This
/// consolidates the retry logic that was duplicated across `openai_compat`,
/// `deepseek`, `gemini`, and was missing from `glm`.
pub async fn send_with_retry<F, Fut>(build_request: F) -> anyhow::Result<reqwest::Response>
where
    F: Fn() -> Fut,
    Fut: Future<Output = reqwest::Result<reqwest::Response>>,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match build_request().await {
            Err(e) if attempt < MODEL_MAX_RETRIES && (e.is_connect() || e.is_timeout()) => {
                let err_kind = if e.is_connect() { "connect" } else { "timeout" };
                record(MetricEvent::PlanReason {
                    decision_kind: PlanDecisionKind::PromptFailure,
                    reason: format!("{err_kind} error on attempt {attempt}"),
                    related_id: None,
                    confidence: 1.0,
                });
                tracing::warn!(attempt, error = %e, "model request failed, retrying");
                tokio::time::sleep(retry_backoff(attempt)).await;
            }
            Err(e) => return Err(e.into()),
            Ok(r) => {
                let s = r.status().as_u16();
                if attempt < MODEL_MAX_RETRIES && should_retry_status(s) {
                    record(MetricEvent::PlanReason {
                        decision_kind: PlanDecisionKind::PromptFailure,
                        reason: format!("HTTP {s} transient error on attempt {attempt}"),
                        related_id: None,
                        confidence: 1.0,
                    });
                    tracing::warn!(
                        attempt,
                        status = s,
                        "model returned transient error, retrying"
                    );
                    tokio::time::sleep(retry_backoff(attempt)).await;
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
    /// Anthropic Messages API (`/v1/messages`) with native `tool_use`
    /// blocks, prompt caching, and extended thinking.
    Anthropic,
    /// Anthropic Messages API via AWS Bedrock, signed with SigV4.
    AnthropicBedrock,
    /// Anthropic Messages API via Google Cloud Vertex AI, using a
    /// service-account access token.
    AnthropicVertex,
    /// OpenCode Zen gateway (OpenAI-compatible endpoint at
    /// opencode.ai/zen/v1/chat/completions).
    OpenCodeZen,
}

/// Classify a model name (and optional type override) into an
/// [`AdapterKind`]. This is the routing decision before we build the
/// concrete adapter.
pub fn adapter_kind_for(
    model_name: &str,
    model_type_override: Option<&str>,
    provider: &str,
) -> AdapterKind {
    if let Some(override_type) = model_type_override {
        return match override_type {
            "glm" | "deepseek" | "gemini" | "kimi" | "moonshot" => AdapterKind::Ollama,
            "anthropic" => AdapterKind::Anthropic,
            "anthropic-bedrock" | "bedrock" => AdapterKind::AnthropicBedrock,
            "anthropic-vertex" | "vertex" => AdapterKind::AnthropicVertex,
            _ => AdapterKind::OpenAiCompat,
        };
    }

    let provider_lower = provider.to_lowercase();
    let lower = model_name.to_lowercase();
    if lower.starts_with("opencode/") {
        return AdapterKind::OpenCodeZen;
    }
    if lower.starts_with("claude-") || lower.starts_with("claude_") || lower.starts_with("claude") {
        match provider_lower.as_str() {
            "bedrock" => AdapterKind::AnthropicBedrock,
            "vertex" => AdapterKind::AnthropicVertex,
            _ => AdapterKind::Anthropic,
        }
    } else if lower.starts_with("glm")
        || lower.contains("chatglm")
        || lower.starts_with("deepseek")
        || lower.starts_with("gemini")
        || lower.starts_with("kimi")
        || lower.starts_with("moonshot")
    {
        AdapterKind::Ollama
    } else if lower.starts_with("anthropic.claude-") || lower.starts_with("claude-3") {
        match provider_lower.as_str() {
            "bedrock" => AdapterKind::AnthropicBedrock,
            "vertex" => AdapterKind::AnthropicVertex,
            _ => AdapterKind::Anthropic,
        }
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
    /// `config.model.json_mode` — the executor doesn't have a way to push
    /// the flag through the per-request stream() signature without
    /// breaking the trait, and a per-adapter field is the simplest
    /// place to remember the setting for the lifetime of the
    /// session.
    fn set_json_mode(&mut self, _json_mode: bool) {}

    /// Configure deterministic-mode seed. Default no-op; adapters
    /// that support a `seed` field in the request body override this.
    /// When set, the adapter should pin temperature=0 and pass the
    /// seed to the provider. Called once at construction by the
    /// executor with `config.model.seed`.
    fn set_seed(&mut self, _seed: Option<u64>) {}

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
    timeout_secs: u64,
) -> Box<dyn ModelAdapter> {
    adapter_for_with_provider(
        model_name,
        ollama_host,
        model_type_override,
        "anthropic",
        timeout_secs,
        "https://opencode.ai/zen/v1/chat/completions",
        None,
    )
}

/// Build the right adapter from a model name string, taking the Anthropic
/// cloud provider hint into account.
pub fn adapter_for_with_provider(
    model_name: &str,
    ollama_host: &str,
    model_type_override: Option<&str>,
    anthropic_provider: &str,
    timeout_secs: u64,
    opencode_zen_endpoint: &str,
    opencode_zen_api_key: Option<&str>,
) -> Box<dyn ModelAdapter> {
    let override_lower = model_type_override.map(|s| s.to_lowercase());
    match adapter_kind_for(model_name, model_type_override, anthropic_provider) {
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
                Box::new(glm::GlmAdapter::new(ollama_host, model_name, timeout_secs))
            } else if override_lower.as_deref() == Some("deepseek") || lower.starts_with("deepseek")
            {
                Box::new(deepseek::DeepSeekAdapter::new(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else if override_lower.as_deref() == Some("gemini") || lower.starts_with("gemini") {
                Box::new(gemini::GeminiAdapter::new(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else if override_lower.as_deref() == Some("kimi")
                || override_lower.as_deref() == Some("moonshot")
                || lower.starts_with("kimi")
                || lower.starts_with("moonshot")
            {
                Box::new(kimi::KimiAdapter::new(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else {
                // With the current classification this branch is
                // unreachable, but keep the previous permissive
                // fallback so we never panic on unknown input.
                Box::new(openai_compat::OpenAiCompatAdapter::new(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            }
        }
        AdapterKind::OpenAiCompat => Box::new(openai_compat::OpenAiCompatAdapter::new(
            ollama_host,
            model_name,
            timeout_secs,
        )),
        AdapterKind::Anthropic => Box::new(anthropic::AnthropicAdapter::new(
            ollama_host,
            model_name,
            timeout_secs,
        )),
        AdapterKind::AnthropicBedrock => {
            // Bedrock credentials come from env at request time, not here.
            Box::new(anthropic_bedrock::AnthropicBedrockAdapter::new(
                model_name,
                "us-east-1",
                "",
                timeout_secs,
            ))
        }
        AdapterKind::AnthropicVertex => {
            // Vertex project/region/credentials are also resolved from config at request time.
            Box::new(anthropic_vertex::AnthropicVertexAdapter::new(
                model_name,
                "",
                "us-central1",
                None,
                timeout_secs,
            ))
        }
        AdapterKind::OpenCodeZen => {
            // Strip the "opencode/" prefix to get the actual model name.
            let zen_model = model_name.strip_prefix("opencode/").unwrap_or(model_name);
            let api_key = opencode_zen_api_key.unwrap_or("");
            Box::new(openai_compat::OpenAiCompatAdapter::with_base_url_and_key(
                opencode_zen_endpoint,
                zen_model,
                api_key,
                timeout_secs,
            ))
        }
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
    seed: Option<u64>,
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

    // Deterministic mode: pin temperature=0 and set seed via options.
    if let Some(s) = seed {
        body["options"] = serde_json::json!({
            "temperature": 0,
            "seed": s,
        });
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
    seed: Option<u64>,
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

    // Deterministic mode: pin temperature=0 and set seed.
    // OpenAI-compat servers accept `seed` at the top level.
    if let Some(s) = seed {
        body["temperature"] = serde_json::json!(0.0);
        body["seed"] = serde_json::json!(s);
    }

    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_5xx_and_rate_limit_statuses() {
        assert!(should_retry_status(429));
        assert!(should_retry_status(503));
        assert!(should_retry_status(500));
        assert!(should_retry_status(502));
        assert!(should_retry_status(599));
    }

    #[test]
    fn should_not_retry_other_4xx() {
        assert!(!should_retry_status(400));
        assert!(!should_retry_status(401));
        assert!(!should_retry_status(403));
        assert!(!should_retry_status(404));
        assert!(!should_retry_status(422));
    }

    #[test]
    fn backoff_grows_with_capped_jitter() {
        let b1 = retry_backoff(1);
        let b2 = retry_backoff(2);
        let b3 = retry_backoff(3);

        // Base doubles each attempt; jitter is small (≤1 s).
        assert!(b1 >= std::time::Duration::from_secs(1));
        assert!(b1 <= std::time::Duration::from_millis(1250));

        assert!(b2 >= std::time::Duration::from_secs(2));
        assert!(b2 <= std::time::Duration::from_millis(2500));

        assert!(b3 >= std::time::Duration::from_secs(4));
        assert!(b3 <= std::time::Duration::from_millis(5000));

        assert!(b3 > b2 && b2 > b1);
    }

    #[test]
    fn adapter_kind_for_classifies_models() {
        assert_eq!(
            adapter_kind_for("qwen2.5:7b", None, "anthropic"),
            AdapterKind::OpenAiCompat
        );
        assert_eq!(
            adapter_kind_for("glm-5", None, "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("chatglm3", None, "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("deepseek-v4", None, "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("gemini-3", None, "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("kimi-2.7k-coder:cloud", None, "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("moonshot-v1-8k", None, "anthropic"),
            AdapterKind::Ollama
        );
    }

    #[test]
    fn adapter_kind_for_override_wins() {
        assert_eq!(
            adapter_kind_for("my-model", Some("glm"), "anthropic"),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for("my-model", Some("openai"), "anthropic"),
            AdapterKind::OpenAiCompat
        );
        assert_eq!(
            adapter_kind_for("my-model", Some("kimi"), "anthropic"),
            AdapterKind::Ollama
        );
    }

    #[test]
    fn adapter_kind_for_cloud_anthropic_overrides() {
        assert_eq!(
            adapter_kind_for("my-model", Some("anthropic-bedrock"), "anthropic"),
            AdapterKind::AnthropicBedrock
        );
        assert_eq!(
            adapter_kind_for("my-model", Some("bedrock"), "anthropic"),
            AdapterKind::AnthropicBedrock
        );
        assert_eq!(
            adapter_kind_for("my-model", Some("anthropic-vertex"), "anthropic"),
            AdapterKind::AnthropicVertex
        );
        assert_eq!(
            adapter_kind_for("my-model", Some("vertex"), "anthropic"),
            AdapterKind::AnthropicVertex
        );
    }

    #[test]
    fn provider_selects_cloud_adapter_for_claude() {
        assert_eq!(
            adapter_kind_for("claude-3-5-sonnet", None, "bedrock"),
            AdapterKind::AnthropicBedrock
        );
        assert_eq!(
            adapter_kind_for("claude-3-5-sonnet", None, "vertex"),
            AdapterKind::AnthropicVertex
        );
        assert_eq!(
            adapter_kind_for("claude-3-5-sonnet", None, "anthropic"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_for_selects_glm() {
        let adapter = adapter_for("glm-5", "http://gateway.example.com", None, 30);
        let info = adapter.model_info();
        assert_eq!(info.name, "glm-5");
        assert!(info.supports_thinking);
    }

    #[test]
    fn adapter_for_selects_deepseek() {
        let adapter = adapter_for("deepseek-v4", "http://gateway.example.com/", None, 30);
        let info = adapter.model_info();
        assert_eq!(info.name, "deepseek-v4");
        assert!(info.supports_thinking);
    }

    #[test]
    fn adapter_for_selects_gemini() {
        let adapter = adapter_for("gemini-3", "http://host/", None, 30);
        let info = adapter.model_info();
        assert_eq!(info.name, "gemini-3");
        assert!(info.supports_images);
    }

    #[test]
    fn adapter_for_selects_openai_compat() {
        let adapter = adapter_for("qwen2.5:7b", "http://host/", None, 30);
        assert_eq!(adapter.model_info().name, "qwen2.5:7b");
    }

    #[test]
    fn adapter_for_selects_kimi() {
        let adapter = adapter_for("kimi-2.7k-coder:cloud", "http://host/", None, 30);
        let info = adapter.model_info();
        assert_eq!(info.name, "kimi-2.7k-coder:cloud");
        assert!(info.supports_thinking);
        assert!(!info.supports_images);
    }

    #[test]
    fn adapter_for_override_selects_concrete_adapter() {
        // A non-GLM name with override "glm" should still route to GLM.
        let adapter = adapter_for("my-glm", "http://host/", Some("glm"), 30);
        assert!(adapter.model_info().supports_thinking);

        // A non-Kimi name with override "kimi" should route to Kimi.
        let adapter = adapter_for("my-kimi", "http://host/", Some("kimi"), 30);
        assert!(adapter.model_info().supports_thinking);
    }

    #[test]
    fn adapter_for_with_provider_selects_bedrock() {
        let adapter = adapter_for_with_provider(
            "anthropic.claude-3-5-sonnet",
            "",
            Some("anthropic-bedrock"),
            "bedrock",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
        );
        assert_eq!(adapter.model_info().name, "anthropic.claude-3-5-sonnet");
        assert!(adapter.model_info().tool_call_format == crate::shared::ToolCallStyle::Anthropic);
    }

    #[test]
    fn adapter_routes_opencode_prefix_to_zen() {
        let kind = adapter_kind_for("opencode/big-pickle", None, "anthropic");
        assert_eq!(kind, AdapterKind::OpenCodeZen);
    }

    #[test]
    fn subagent_allowed_models_rejects_unlisted() {
        let allowed = Some(vec!["qwen2.5:0.5b".to_string()]);
        let requested = Some("deepseek-v4-flash".to_string());
        let effective = requested
            .as_ref()
            .filter(|m| allowed.as_ref().is_none_or(|a| a.contains(&m.to_string())))
            .cloned();
        assert!(effective.is_none(), "unlisted model should be rejected");
    }

    #[test]
    fn subagent_allowed_models_accepts_listed() {
        let allowed = Some(vec!["qwen2.5:0.5b".to_string()]);
        let requested = Some("qwen2.5:0.5b".to_string());
        let effective = requested
            .as_ref()
            .filter(|m| allowed.as_ref().is_none_or(|a| a.contains(&m.to_string())))
            .cloned();
        assert_eq!(effective, Some("qwen2.5:0.5b".to_string()));
    }
}
