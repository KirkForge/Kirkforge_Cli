pub mod anthropic;
pub mod anthropic_bedrock;
pub mod anthropic_vertex;
pub mod auth;
pub mod bedrock_signing;
pub mod caching;
pub mod oellama;
pub mod ollama_ndjson;
pub mod openai_compat;
pub mod tool_call_markup;
pub mod vertex_auth;

use std::collections::HashMap;
use std::str::FromStr;

use crate::shared::metrics::{record, MetricEvent, PlanDecisionKind};
use crate::shared::{ContentPart, ModelInfo, Role, StreamEvent, ToolCallStyle};
use std::future::Future;

/// Maximum bytes the SSE parser will accumulate while waiting for a complete
/// `data: ...\n\n` frame. Shared across Anthropic, Bedrock, OpenAI-compat,
/// and MCP HTTP transports.
pub(crate) const MAX_SSE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Build `ModelInfo` for any Anthropic-family model (first-party, Bedrock,
/// or Vertex). `image_prefix` is the model-id prefix that signals vision
/// support — `"claude-3"` for first-party/Vertex, `"anthropic.claude-3"` for
/// Bedrock.
pub(crate) fn anthropic_model_info(model_id: &str, image_prefix: &str) -> ModelInfo {
    let lower = model_id.to_lowercase();
    let is_reasoning = lower.contains("claude-3-7-sonnet") || lower.contains("claude-4");
    ModelInfo {
        name: model_id.to_string(),
        supports_thinking: is_reasoning,
        tool_call_format: ToolCallStyle::Anthropic,
        // ceiling: flat 200_000 for every claude model; model-specific
        // context sizing is deferred (WO 15.26 3.22 deferred).
        max_context_tokens: 200_000,
        recommended_temperature: 1.0,
        supports_images: lower.starts_with(image_prefix),
        supports_cache: true,
    }
}

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

pub use crate::shared::retry_backoff;

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

impl std::fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterKind::Ollama => write!(f, "Ollama"),
            AdapterKind::OpenAiCompat => write!(f, "OpenAiCompat"),
            AdapterKind::Anthropic => write!(f, "Anthropic"),
            AdapterKind::AnthropicBedrock => write!(f, "AnthropicBedrock"),
            AdapterKind::AnthropicVertex => write!(f, "AnthropicVertex"),
            AdapterKind::OpenCodeZen => write!(f, "OpenCodeZen"),
        }
    }
}

impl FromStr for AdapterKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Ollama" => Ok(AdapterKind::Ollama),
            "OpenAiCompat" => Ok(AdapterKind::OpenAiCompat),
            "Anthropic" => Ok(AdapterKind::Anthropic),
            "AnthropicBedrock" => Ok(AdapterKind::AnthropicBedrock),
            "AnthropicVertex" => Ok(AdapterKind::AnthropicVertex),
            "OpenCodeZen" => Ok(AdapterKind::OpenCodeZen),
            _ => Err(format!(
                "unknown AdapterKind {s:?}; expected one of Ollama, OpenAiCompat, Anthropic, AnthropicBedrock, AnthropicVertex, OpenCodeZen"
            )),
        }
    }
}

/// Hardcoded default routing from model name prefix and provider to
/// [`AdapterKind`]. This is the fallback used when no config-level
/// routing table match is found.
fn adapter_kind_for_default(
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

/// Classify a model name (and optional type override) into an
/// [`AdapterKind`]. Checks the config-provided routing table first;
/// if no prefix matches, falls back to the hardcoded defaults.
///
/// The `adapter_routing` table maps model-name prefixes (e.g. `"claude-"`)
/// to [`AdapterKind`] variant names (e.g. `"Anthropic"`). Longest-prefix
/// match wins. When the table is empty (the default for existing configs
/// with no `[adapter_routing]` section), the hardcoded defaults are used
/// exclusively — preserving backward compatibility.
pub fn adapter_kind_for(
    model_name: &str,
    model_type_override: Option<&str>,
    provider: &str,
) -> AdapterKind {
    adapter_kind_for_routed(model_name, model_type_override, provider, None)
}

/// Data-driven version of [`adapter_kind_for`] that checks a user-supplied
/// routing table before falling back to the hardcoded defaults.
///
/// The `adapter_routing` table maps model-name prefixes to adapter-kind
/// strings (as parsed by [`AdapterKind::from_str`]). Longest-prefix match
/// wins. Entries here override the hardcoded routing, so a user can remap
/// `"deepseek"` from the default `Ollama` to `OpenAiCompat`, or add a
/// new family like `"grok-"` → `OpenAiCompat` without a code change.
pub fn adapter_kind_for_routed(
    model_name: &str,
    model_type_override: Option<&str>,
    provider: &str,
    adapter_routing: Option<&HashMap<String, String>>,
) -> AdapterKind {
    // Config routing table takes priority when present.
    if let Some(table) = adapter_routing {
        if !table.is_empty() {
            // Find the longest matching prefix.
            let mut best_prefix: &str = "";
            let mut best_kind: Option<AdapterKind> = None;
            for (prefix, kind_str) in table {
                if model_name.starts_with(prefix) && prefix.len() > best_prefix.len() {
                    if let Ok(kind) = AdapterKind::from_str(kind_str) {
                        best_prefix = prefix;
                        best_kind = Some(kind);
                    }
                }
            }
            if let Some(kind) = best_kind {
                return kind;
            }
        }
    }
    adapter_kind_for_default(model_name, model_type_override, provider)
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

    /// Enable/disable extended thinking. Default no-op; adapters that
    /// support thinking blocks (e.g. Anthropic) override this.
    fn set_extended_thinking(&mut self, _enabled: bool) {}

    /// Set the budget_tokens for extended thinking. Default no-op.
    fn set_budget_tokens(&mut self, _budget: usize) {}

    async fn stream(
        &self,
        messages: &[crate::shared::Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>>;
}

#[cfg(test)]
mod m5_tests;

/// Per-provider API keys resolved from config, passed to
/// [`adapter_for_with_provider`] so each adapter can authenticate.
#[derive(Debug, Clone, Default)]
pub struct ProviderApiKeys {
    pub anthropic: Option<String>,
    pub openai: Option<String>,
    pub deepseek: Option<String>,
    pub gemini: Option<String>,
    pub kimi: Option<String>,
}

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
        None,
        &ProviderApiKeys::default(),
        None,
        None,
        None,
        None,
    )
}

/// Build the right adapter from a model name string, taking the Anthropic
/// cloud provider hint into account.
#[allow(clippy::too_many_arguments)]
pub fn adapter_for_with_provider(
    model_name: &str,
    ollama_host: &str,
    model_type_override: Option<&str>,
    anthropic_provider: &str,
    timeout_secs: u64,
    opencode_zen_endpoint: &str,
    opencode_zen_api_key: Option<&str>,
    adapter_routing: Option<&HashMap<String, String>>,
    api_keys: &ProviderApiKeys,
    bedrock_region: Option<&str>,
    vertex_project_id: Option<&str>,
    vertex_region: Option<&str>,
    vertex_service_account_path: Option<std::path::PathBuf>,
) -> Box<dyn ModelAdapter> {
    let override_lower = model_type_override.map(|s| s.to_lowercase());
    match adapter_kind_for_routed(
        model_name,
        model_type_override,
        anthropic_provider,
        adapter_routing,
    ) {
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
                Box::new(oellama::OellamaAdapter::glm(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else if override_lower.as_deref() == Some("deepseek") || lower.starts_with("deepseek")
            {
                Box::new(oellama::OellamaAdapter::deepseek(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else if override_lower.as_deref() == Some("gemini") || lower.starts_with("gemini") {
                Box::new(oellama::OellamaAdapter::gemini(
                    ollama_host,
                    model_name,
                    timeout_secs,
                ))
            } else if override_lower.as_deref() == Some("kimi")
                || override_lower.as_deref() == Some("moonshot")
                || lower.starts_with("kimi")
                || lower.starts_with("moonshot")
            {
                Box::new(oellama::OellamaAdapter::kimi(
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
            api_keys.anthropic.clone(),
        )),
        AdapterKind::AnthropicBedrock => Box::new(anthropic_bedrock::AnthropicBedrockAdapter::new(
            model_name,
            bedrock_region.unwrap_or("us-east-1"),
            timeout_secs,
        )),
        AdapterKind::AnthropicVertex => Box::new(anthropic_vertex::AnthropicVertexAdapter::new(
            model_name,
            vertex_project_id.unwrap_or(""),
            vertex_region.unwrap_or("us-central1"),
            vertex_service_account_path,
            timeout_secs,
        )),
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
    //
    // WO 17.5: we also add a tail breakpoint on the last user message
    // so the growing conversation tail is cached for the next turn,
    // and a system+tools breakpoint on the first message (system) if
    // it's a system message.
    let mut cache_marker_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();
    if model_info.supports_cache && messages.len() > 1 {
        // System+tools breakpoint: mark the first message (system prompt)
        // so the static prefix is cached.
        if !messages.is_empty() && matches!(messages[0].role, crate::shared::Role::System) {
            cache_marker_indices.insert(0);
        }
        // Last 2 of the prefix (excluding trailing user turn).
        let prefix_end = messages.len() - 1;
        for i in prefix_end.saturating_sub(2)..prefix_end {
            cache_marker_indices.insert(i);
        }
        // Tail breakpoint: last user message (WO 17.5).
        cache_marker_indices.insert(messages.len() - 1);
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

/// Find the first occurrence of `needle` in `haystack` (byte-level substring search).
pub(crate) fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Trim ASCII whitespace from both ends of a byte slice.
pub(crate) fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(bytes.len());
    &bytes[start..end]
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
            None,
            &ProviderApiKeys::default(),
            Some("us-west-2"),
            Some("test-project"),
            Some("us-east5"),
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

    #[test]
    fn should_retry_status_599_boundary() {
        assert!(should_retry_status(599));
        assert!(!should_retry_status(600));
    }

    #[test]
    fn should_retry_status_429_only_rate_limit() {
        assert!(should_retry_status(429));
        assert!(!should_retry_status(428));
        assert!(!should_retry_status(430));
    }

    #[test]
    fn adapter_kind_for_claude_underscore_prefix() {
        assert_eq!(
            adapter_kind_for("claude_sonnet", None, "anthropic"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_claude_no_dash_prefix() {
        assert_eq!(
            adapter_kind_for("claude", None, "anthropic"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_anthropic_dot_claude_prefix() {
        assert_eq!(
            adapter_kind_for("anthropic.claude-3-sonnet", None, "anthropic"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_anthropic_dot_claude_bedrock() {
        assert_eq!(
            adapter_kind_for("anthropic.claude-3-sonnet", None, "bedrock"),
            AdapterKind::AnthropicBedrock
        );
    }

    #[test]
    fn adapter_kind_for_anthropic_dot_claude_vertex() {
        assert_eq!(
            adapter_kind_for("anthropic.claude-3-sonnet", None, "vertex"),
            AdapterKind::AnthropicVertex
        );
    }

    #[test]
    fn adapter_kind_for_opencode_prefix() {
        assert_eq!(
            adapter_kind_for("opencode/zen-model", None, "anthropic"),
            AdapterKind::OpenCodeZen
        );
    }

    #[test]
    fn adapter_kind_for_unknown_override_defaults_to_openai_compat() {
        assert_eq!(
            adapter_kind_for("my-model", Some("unknown-type"), "anthropic"),
            AdapterKind::OpenAiCompat
        );
    }

    #[test]
    fn adapter_kind_for_override_anthropic_selects_anthropic() {
        assert_eq!(
            adapter_kind_for("my-model", Some("anthropic"), "bedrock"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_for_selects_anthropic_for_claude() {
        let adapter = adapter_for("claude-3-opus", "http://host", None, 30);
        let info = adapter.model_info();
        assert_eq!(info.name, "claude-3-opus");
        assert_eq!(
            info.tool_call_format,
            crate::shared::ToolCallStyle::Anthropic
        );
    }

    #[test]
    fn adapter_for_selects_anthropic_for_claude_underscore() {
        let adapter = adapter_for("claude_sonnet", "http://host", None, 30);
        assert_eq!(adapter.model_info().name, "claude_sonnet");
    }

    #[test]
    fn adapter_for_with_provider_selects_vertex() {
        let adapter = adapter_for_with_provider(
            "claude-3-opus",
            "",
            None,
            "vertex",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            Some("my-gcp-project"),
            Some("europe-west4"),
            None,
        );
        assert_eq!(adapter.model_info().name, "claude-3-opus");
        assert_eq!(
            adapter.model_info().tool_call_format,
            crate::shared::ToolCallStyle::Anthropic
        );
    }

    #[test]
    fn adapter_for_with_provider_selects_anthropic_default() {
        let adapter = adapter_for_with_provider(
            "claude-3-opus",
            "",
            None,
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(adapter.model_info().name, "claude-3-opus");
    }

    #[test]
    fn adapter_for_with_provider_opencode_zen_strips_prefix() {
        let adapter = adapter_for_with_provider(
            "opencode/big-pickle",
            "",
            None,
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            Some("test-key"),
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(adapter.model_info().name, "big-pickle");
    }

    #[test]
    fn adapter_for_with_provider_opencode_zen_no_key() {
        let adapter = adapter_for_with_provider(
            "opencode/big-pickle",
            "",
            None,
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(adapter.model_info().name, "big-pickle");
    }

    #[test]
    fn adapter_for_with_provider_moonshot_override() {
        let adapter = adapter_for_with_provider(
            "my-model",
            "http://host",
            Some("moonshot"),
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert!(adapter.model_info().supports_thinking);
    }

    #[test]
    fn adapter_for_with_provider_deepseek_override() {
        let adapter = adapter_for_with_provider(
            "my-model",
            "http://host",
            Some("deepseek"),
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert!(adapter.model_info().supports_thinking);
    }

    #[test]
    fn adapter_for_with_provider_gemini_override() {
        let adapter = adapter_for_with_provider(
            "my-model",
            "http://host",
            Some("gemini"),
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert!(adapter.model_info().supports_images);
    }

    #[test]
    fn adapter_for_with_provider_unknown_override_falls_to_openai_compat() {
        let adapter = adapter_for_with_provider(
            "my-model",
            "http://host",
            Some("unknown-type"),
            "anthropic",
            30,
            "https://opencode.ai/zen/v1/chat/completions",
            None,
            None,
            &ProviderApiKeys::default(),
            None,
            None,
            None,
            None,
        );
        assert_eq!(adapter.model_info().name, "my-model");
    }

    #[test]
    fn build_content_object_string_content() {
        let v = build_content_object(&Role::User, "hello", None);
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hello");
    }

    #[test]
    fn build_content_object_empty_parts_uses_string() {
        let v = build_content_object(&Role::User, "hello", Some(&[]));
        assert_eq!(v["content"], "hello");
    }

    #[test]
    fn build_content_object_text_part_emits_text() {
        let parts = vec![ContentPart::Text { text: "hi".into() }];
        let v = build_content_object(&Role::User, "", Some(&parts));
        let arr = v["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "hi");
    }

    #[test]
    fn build_content_object_image_part_emits_image_url() {
        let parts = vec![ContentPart::Image {
            data_base64: "BASE64".into(),
            mime: "image/png".into(),
        }];
        let v = build_content_object(&Role::User, "", Some(&parts));
        let arr = v["content"].as_array().unwrap();
        assert_eq!(arr[0]["type"], "image_url");
        assert_eq!(arr[0]["image_url"]["url"], "data:image/png;base64,BASE64");
    }

    #[test]
    fn build_content_object_mixed_parts() {
        let parts = vec![
            ContentPart::Text {
                text: "what?".into(),
            },
            ContentPart::Image {
                data_base64: "B".into(),
                mime: "image/png".into(),
            },
        ];
        let v = build_content_object(&Role::User, "", Some(&parts));
        let arr = v["content"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
    }

    #[test]
    fn build_ollama_chat_body_basic_shape() {
        let msgs = vec![crate::shared::Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_ollama_chat_body("m", &msgs, &[], true, false, None);
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn build_ollama_chat_body_includes_tools() {
        let tools = vec![crate::shared::ToolDef {
            name: "bash",
            description: "run a command",
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = build_ollama_chat_body("m", &[], &tools, true, false, None);
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(tools_arr[0]["type"], "function");
        assert_eq!(tools_arr[0]["function"]["name"], "bash");
    }

    #[test]
    fn build_ollama_chat_body_json_mode_adds_format() {
        let body = build_ollama_chat_body("m", &[], &[], true, true, None);
        assert_eq!(body["format"], "json");
    }

    #[test]
    fn build_ollama_chat_body_seed_sets_options() {
        let body = build_ollama_chat_body("m", &[], &[], true, false, Some(42));
        assert_eq!(body["options"]["temperature"], 0);
        assert_eq!(body["options"]["seed"], 42);
    }

    #[test]
    fn build_ollama_chat_body_thinking_field_included() {
        let msgs = vec![crate::shared::Message {
            role: Role::Assistant,
            content: "answer".into(),
            thinking: Some("reasoning".into()),
            ..Default::default()
        }];
        let body = build_ollama_chat_body("m", &msgs, &[], true, false, None);
        assert_eq!(body["messages"][0]["thinking"], "reasoning");
    }

    #[test]
    fn build_ollama_chat_body_tool_call_id_included() {
        let msgs = vec![crate::shared::Message {
            role: Role::Tool,
            content: "result".into(),
            tool_call_id: Some("call_1".into()),
            ..Default::default()
        }];
        let body = build_ollama_chat_body("m", &msgs, &[], true, false, None);
        assert_eq!(body["messages"][0]["tool_call_id"], "call_1");
    }

    #[test]
    fn build_ollama_chat_body_multimodal_emits_images() {
        let msgs = vec![crate::shared::Message {
            role: Role::User,
            content: String::new(),
            content_parts: Some(vec![
                ContentPart::Text {
                    text: "what?".into(),
                },
                ContentPart::Image {
                    data_base64: "BASE64".into(),
                    mime: "image/png".into(),
                },
            ]),
            ..Default::default()
        }];
        let body = build_ollama_chat_body("m", &msgs, &[], true, false, None);
        assert_eq!(body["messages"][0]["images"][0], "BASE64");
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("[image]"));
    }

    #[test]
    fn build_openai_compat_body_tool_role_emits_tool_call_id() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        };
        let msgs = vec![crate::shared::Message {
            role: Role::Tool,
            content: "result".into(),
            tool_call_id: Some("call_1".into()),
            ..Default::default()
        }];
        let body = build_openai_compat_body("m", &mi, &msgs, &[], false, None);
        assert_eq!(body["messages"][0]["role"], "tool");
        assert_eq!(body["messages"][0]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][0]["content"], "result");
    }

    #[test]
    fn build_openai_compat_body_assistant_with_tool_calls() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        };
        let msgs = vec![crate::shared::Message {
            role: Role::Assistant,
            content: "thinking".into(),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "call_1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"cmd": "ls"}),
            }]),
            ..Default::default()
        }];
        let body = build_openai_compat_body("m", &mi, &msgs, &[], false, None);
        assert_eq!(body["messages"][0]["role"], "assistant");
        let tcs = body["messages"][0]["tool_calls"].as_array().unwrap();
        assert_eq!(tcs[0]["id"], "call_1");
        assert_eq!(tcs[0]["function"]["name"], "bash");
    }

    #[test]
    fn build_openai_compat_body_seed_sets_temperature_and_seed() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        };
        let body = build_openai_compat_body(
            "m",
            &mi,
            &[crate::shared::Message {
                role: Role::User,
                content: "hi".into(),
                ..Default::default()
            }],
            &[],
            false,
            Some(7),
        );
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["seed"], 7);
    }

    #[test]
    fn build_openai_compat_body_json_mode_with_tools_sets_tool_choice() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        };
        let tools = vec![crate::shared::ToolDef {
            name: "bash",
            description: "x",
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = build_openai_compat_body(
            "m",
            &mi,
            &[crate::shared::Message {
                role: Role::User,
                content: "hi".into(),
                ..Default::default()
            }],
            &tools,
            true,
            None,
        );
        assert_eq!(
            body["response_format"],
            serde_json::json!({"type": "json_object"})
        );
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn build_openai_compat_body_json_mode_without_tools_omits_tool_choice() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: false,
        };
        let body = build_openai_compat_body(
            "m",
            &mi,
            &[crate::shared::Message {
                role: Role::User,
                content: "hi".into(),
                ..Default::default()
            }],
            &[],
            true,
            None,
        );
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn build_openai_compat_body_single_message_no_cache_markers() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: true,
        };
        let body = build_openai_compat_body(
            "m",
            &mi,
            &[crate::shared::Message {
                role: Role::User,
                content: "hi".into(),
                ..Default::default()
            }],
            &[],
            false,
            None,
        );
        assert!(body["messages"][0].get("cache_control").is_none());
    }

    /// WO 17.5: OpenAI compat adapter adds a tail breakpoint on the
    /// last user message and a system breakpoint on the first message.
    #[test]
    fn build_openai_compat_body_system_and_tail_breakpoints() {
        let mi = crate::shared::ModelInfo {
            name: "m".into(),
            supports_thinking: false,
            tool_call_format: crate::shared::ToolCallStyle::OpenAiCompat,
            max_context_tokens: 4096,
            recommended_temperature: 0.7,
            supports_images: false,
            supports_cache: true,
        };
        let messages = vec![
            crate::shared::Message {
                role: Role::System,
                content: "sys".into(),
                ..Default::default()
            },
            crate::shared::Message {
                role: Role::User,
                content: "ask".into(),
                ..Default::default()
            },
            crate::shared::Message {
                role: Role::Assistant,
                content: "reply".into(),
                ..Default::default()
            },
            crate::shared::Message {
                role: Role::User,
                content: "followup".into(),
                ..Default::default()
            },
        ];
        let body = build_openai_compat_body("m", &mi, &messages, &[], false, None);
        let msgs = body["messages"].as_array().unwrap();
        // System message (index 0) should have cache_control (system+tools breakpoint).
        assert_eq!(
            msgs[0]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "system message must have cache breakpoint (WO 17.5)"
        );
        // Last user message (index 3) should have cache_control (tail breakpoint).
        assert_eq!(
            msgs[3]["cache_control"],
            serde_json::json!({"type": "ephemeral"}),
            "last user message must have tail breakpoint (WO 17.5)"
        );
    }

    #[test]
    fn build_reqwest_client_returns_a_client() {
        let _client = build_reqwest_client();
    }

    #[test]
    fn adapter_kind_for_chatglm_selects_ollama() {
        assert_eq!(
            adapter_kind_for("chatglm3-something", None, "anthropic"),
            AdapterKind::Ollama
        );
    }

    #[test]
    fn adapter_kind_for_claude_3_prefix_anthropic_provider() {
        assert_eq!(
            adapter_kind_for("claude-3-opus", None, "anthropic"),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_claude_3_prefix_bedrock_provider() {
        assert_eq!(
            adapter_kind_for("claude-3-opus", None, "bedrock"),
            AdapterKind::AnthropicBedrock
        );
    }

    #[test]
    fn adapter_kind_for_claude_3_prefix_vertex_provider() {
        assert_eq!(
            adapter_kind_for("claude-3-opus", None, "vertex"),
            AdapterKind::AnthropicVertex
        );
    }

    #[test]
    fn adapter_kind_for_routed_custom_prefix_overrides_default() {
        let routing: HashMap<String, String> = [
            ("grok-".to_string(), "OpenAiCompat".to_string()),
            ("deepseek".to_string(), "OpenAiCompat".to_string()),
        ]
        .into_iter()
        .collect();

        // "deepseek-v4" normally routes to Ollama, but the config override
        // maps the "deepseek" prefix to OpenAiCompat.
        assert_eq!(
            adapter_kind_for_routed("deepseek-v4", None, "anthropic", Some(&routing)),
            AdapterKind::OpenAiCompat
        );

        // Unknown prefix in routing table → grok- maps to OpenAiCompat.
        assert_eq!(
            adapter_kind_for_routed("grok-2", None, "anthropic", Some(&routing)),
            AdapterKind::OpenAiCompat
        );

        // A model not matching any config prefix falls back to defaults.
        assert_eq!(
            adapter_kind_for_routed("claude-3-opus", None, "anthropic", Some(&routing)),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_routed_longest_prefix_wins() {
        let routing: HashMap<String, String> = [
            ("claude-".to_string(), "Anthropic".to_string()),
            ("claude-3-".to_string(), "AnthropicBedrock".to_string()),
        ]
        .into_iter()
        .collect();

        // "claude-3-opus" matches both prefixes; longest ("claude-3-") wins.
        assert_eq!(
            adapter_kind_for_routed("claude-3-opus", None, "anthropic", Some(&routing)),
            AdapterKind::AnthropicBedrock
        );

        // "claude-sonnet" matches only the shorter prefix.
        assert_eq!(
            adapter_kind_for_routed("claude-sonnet", None, "anthropic", Some(&routing)),
            AdapterKind::Anthropic
        );
    }

    #[test]
    fn adapter_kind_for_routed_empty_table_falls_back() {
        let routing: HashMap<String, String> = HashMap::new();
        // With an empty routing table, hardcoded defaults apply.
        assert_eq!(
            adapter_kind_for_routed("glm-5", None, "anthropic", Some(&routing)),
            AdapterKind::Ollama
        );
        assert_eq!(
            adapter_kind_for_routed("qwen2.5:7b", None, "anthropic", Some(&routing)),
            AdapterKind::OpenAiCompat
        );
    }

    #[test]
    fn adapter_kind_for_routed_none_table_falls_back() {
        // With no routing table at all, hardcoded defaults apply.
        assert_eq!(
            adapter_kind_for_routed("deepseek-v4", None, "anthropic", None),
            AdapterKind::Ollama
        );
    }

    #[test]
    fn adapter_kind_for_routed_unknown_kind_string_ignored() {
        let routing: HashMap<String, String> = [("qwen".to_string(), "NoSuchAdapter".to_string())]
            .into_iter()
            .collect();
        // Invalid kind string → no match → falls back to default.
        assert_eq!(
            adapter_kind_for_routed("qwen2.5:7b", None, "anthropic", Some(&routing)),
            AdapterKind::OpenAiCompat
        );
    }
}
