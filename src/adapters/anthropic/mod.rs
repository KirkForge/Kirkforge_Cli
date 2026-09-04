//! First-party Anthropic Messages API adapter.
//!
//! Speaks the real `/v1/messages` SSE streaming protocol:
//!   - `content_block_start` / `content_block_delta` / `content_block_stop`
//!   - `thinking` blocks for extended-reasoning models
//!   - native `tool_use` / `tool_result` content blocks
//!   - prompt caching via `cache_control: {type: "ephemeral"}` on the
//!     last two prefix messages
//!
//! The executor consumes the canonical `StreamEvent` events produced by
//! [`parse_anthropic_stream`]; no other module needs to know the wire format.

mod content_blocks;
mod sse;
mod usage;

pub(crate) use sse::parse_anthropic_stream;

use super::ModelAdapter;
use crate::shared::{ContentPart, Message, ModelInfo, ResponseFormat, Role, StreamEvent};

// Test-only re-exports so the inherited test module (which addresses these
// parent helpers as `super::find_subseq` / `super::trim_ascii_whitespace`)
// keeps resolving after the split without editing test bodies.
#[cfg(test)]
use super::{find_subseq, trim_ascii_whitespace};

/// Anthropic Messages API version we target.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic's cache_control breakpoint limit (system + tools + last user msgs).
const ANTHROPIC_CACHE_BREAKPOINT_LIMIT: usize = 4;

pub struct AnthropicAdapter {
    model: String,
    api_base: String,
    api_key: Option<String>,
    client: reqwest::Client,
    json_mode: bool,
    response_format: Option<ResponseFormat>,
    seed: Option<u64>,
    timeout_secs: u64,
    extended_thinking: bool,
    budget_tokens: usize,
    max_tokens: u32,
    tool_choice: Option<crate::shared::ToolChoice>,
    stream_idle_timeout: std::time::Duration,
    /// Hosted computer_use display dims. `Some((w,h))` activates the hosted
    /// coordinate-vision path (requires the `computer_use` Cargo feature +
    /// `anthropic-beta: computer-use-2025-01-24` header). `None` = off.
    /// Default: off. See WO 28.16.
    computer_use_dims: Option<(u32, u32)>,
}

impl AnthropicAdapter {
    pub fn new(api_base: &str, model: &str, timeout_secs: u64, api_key: Option<String>) -> Self {
        Self {
            model: model.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
            api_key,
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            extended_thinking: true,
            budget_tokens: 10_000,
            max_tokens: 8192,
            tool_choice: None,
            stream_idle_timeout: super::STREAM_IDLE_TIMEOUT,
            computer_use_dims: None,
        }
    }

    pub fn with_extended_thinking(mut self, enabled: bool) -> Self {
        self.extended_thinking = enabled;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Activate the hosted Anthropic computer_use beta path. Pass `Some((w,h))`
    /// to enable (requires the `computer_use` Cargo feature to take effect at
    /// the wire level) or `None` to disable. See WO 28.16.
    pub fn with_computer_use(mut self, dims: Option<(u32, u32)>) -> Self {
        self.computer_use_dims = dims;
        self
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicAdapter {
    fn model_info(&self) -> ModelInfo {
        super::anthropic_model_info(&self.model, "claude-3")
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        if json_mode {
            self.response_format = Some(ResponseFormat::JsonObject);
        } else {
            self.response_format = None;
        }
    }
    fn set_response_format(&mut self, format: crate::shared::ResponseFormat) {
        self.response_format = Some(format);
    }
    fn set_seed(&mut self, seed: Option<u64>) {
        self.seed = seed;
    }

    fn set_extended_thinking(&mut self, enabled: bool) {
        self.extended_thinking = enabled;
    }

    fn set_budget_tokens(&mut self, budget: usize) {
        self.budget_tokens = budget;
    }

    fn set_max_tokens(&mut self, max_tokens: u32) {
        self.max_tokens = max_tokens;
    }

    fn set_streaming_timeout(&mut self, secs: u64) {
        self.stream_idle_timeout = std::time::Duration::from_secs(secs);
    }

    fn set_computer_use_dims(&mut self, dims: Option<(u32, u32)>) {
        self.computer_use_dims = dims;
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let api_key = super::auth::resolve_api_key("anthropic", self.api_key.as_deref())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no Anthropic API key set (ANTHROPIC_API_KEY or [model].anthropic_api_key)"
                )
            })?;
        let body = build_anthropic_body(
            &self.model,
            messages,
            tools,
            self.response_format.as_ref(),
            self.seed,
            self.extended_thinking,
            self.budget_tokens,
            self.max_tokens,
            self.tool_choice.as_ref(),
        );
        let url = format!("{}/v1/messages", self.api_base);

        // Hosted computer_use beta (WO 28.16): rewrite the `computer` tool to
        // the hosted tool type + tag the request with the beta header. The
        // whole path compiles out when the `computer_use` feature is off, so
        // a default build emits zero computer_use wire bytes.
        #[cfg(feature = "computer_use")]
        let beta_header_needed = self.computer_use_dims.is_some();
        #[cfg(not(feature = "computer_use"))]
        let beta_header_needed = false;
        #[cfg(feature = "computer_use")]
        let body = {
            if let Some((w, h)) = self.computer_use_dims {
                let mut body = body;
                apply_computer_use(&mut body, w, h);
                body
            } else {
                body
            }
        };

        let response = super::send_with_retry(|| async {
            let mut req = self
                .client
                .post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs));
            if beta_header_needed {
                req = req.header("anthropic-beta", "computer-use-2025-01-24");
            }
            req.send().await
        })
        .await
        .map_err(super::classify_transport_error)?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        tokio::spawn(parse_anthropic_stream(
            tx,
            response.bytes_stream(),
            self.stream_idle_timeout,
        ));
        Ok(rx)
    }
}

/// Build a request body for the Anthropic Messages API.
///
/// System messages are hoisted into the top-level `system` field ( Anthropic
/// does not allow `role: "system"` inside `messages`). The last two prefix
/// messages receive `cache_control: {type: "ephemeral"}` to enable prompt
/// caching; the trailing user message is excluded.
///
/// This function is `pub(crate)` so the Bedrock and Vertex adapters can reuse
/// the same body construction without duplicating message translation.
// reason: DEFERRED hub — 33 callers (31 in this module's test + stream paths
// plus Bedrock/Vertex); an `AnthropicBodyParams` struct would reduce call-site
// repetition but the blast radius is CRITICAL (34 impacted symbols).
// Refactor-on-touch: introduce the struct when the next model family adds a
// new body param. Tracked in WO 45.54 (too_many_arguments audit).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_anthropic_body(
    model: &str,
    messages: &[Message],
    tools: &[crate::shared::ToolDef],
    response_format: Option<&crate::shared::ResponseFormat>,
    seed: Option<u64>,
    extended_thinking: bool,
    budget_tokens: usize,
    max_tokens: u32,
    tool_choice: Option<&crate::shared::ToolChoice>,
) -> serde_json::Value {
    let supports_thinking = super::anthropic_supports_thinking(model);

    let mut system_blocks: Vec<serde_json::Value> = Vec::new();
    let mut anthropic_messages: Vec<serde_json::Value> = Vec::new();

    for (idx, m) in messages.iter().enumerate() {
        if m.role == Role::System {
            let mut block = match m.content_parts.as_deref() {
                Some(parts) if !parts.is_empty() => content_block_from_parts(parts),
                _ => serde_json::json!({"type": "text", "text": m.content}),
            };
            let is_last_system = messages[idx + 1..].iter().all(|m2| m2.role != Role::System);
            if is_last_system {
                block["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
            system_blocks.push(block);
            continue;
        }

        let content = match m.role {
            Role::Tool => {
                // Anthropic uses role "user" with content block type "tool_result".
                let result_block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                    "content": m.content,
                });
                serde_json::json!({"role": "user", "content": vec![result_block]})
            }
            Role::Assistant => {
                if let Some(tcs) = m.tool_calls.as_ref() {
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(serde_json::json!({"type": "text", "text": m.content}));
                    }
                    for tc in tcs {
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                    serde_json::json!({"role": "assistant", "content": blocks})
                } else {
                    let block = match m.content_parts.as_deref() {
                        Some(parts) if !parts.is_empty() => content_block_from_parts(parts),
                        _ => serde_json::json!({"type": "text", "text": m.content}),
                    };
                    serde_json::json!({"role": "assistant", "content": vec![block]})
                }
            }
            _ => {
                let block = match m.content_parts.as_deref() {
                    Some(parts) if !parts.is_empty() => content_block_from_parts(parts),
                    _ => serde_json::json!({"type": "text", "text": m.content}),
                };
                serde_json::json!({"role": "user", "content": vec![block]})
            }
        };
        anthropic_messages.push(content);
    }

    // Apply cache breakpoints, capped at ANTHROPIC_CACHE_BREAKPOINT_LIMIT (4).
    // Priority: system+tools breakpoint (last system block + last tool def),
    // then last user messages (most recent first, up to remaining budget).
    let has_tools = !tools.is_empty();
    let mut breakpoint_count = 0;

    if system_blocks
        .iter()
        .any(|b| b.get("cache_control").is_some())
    {
        breakpoint_count += 1;
    }
    if has_tools {
        breakpoint_count += 1;
    }

    let remaining = ANTHROPIC_CACHE_BREAKPOINT_LIMIT.saturating_sub(breakpoint_count);

    if remaining > 0 && !anthropic_messages.is_empty() {
        let mut user_indices: Vec<usize> = anthropic_messages
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(i, m)| {
                if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                    Some(i)
                } else {
                    None
                }
            })
            .take(remaining)
            .collect();
        user_indices.sort();

        for idx in user_indices {
            if let Some(content) = anthropic_messages[idx].get_mut("content") {
                if let Some(arr) = content.as_array_mut() {
                    if let Some(last_block) = arr.last_mut() {
                        last_block["cache_control"] = serde_json::json!({"type": "ephemeral"});
                    }
                }
            }
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": anthropic_messages,
        "stream": true,
    });

    if extended_thinking && supports_thinking {
        body["thinking"] = serde_json::json!({
            "type": "enabled",
            "budget_tokens": budget_tokens
        });
    }

    if !system_blocks.is_empty() {
        if system_blocks.len() == 1 {
            body["system"] = system_blocks
                .into_iter()
                .next()
                .expect("len==1 checked above");
        } else {
            body["system"] = serde_json::Value::Array(system_blocks);
        }
    }

    if !tools.is_empty() {
        let mut tool_defs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        // WO 17.5: cache breakpoint on the last tool definition so the
        // entire system+tools prefix is cached as a single unit.
        if let Some(last_tool) = tool_defs.last_mut() {
            last_tool["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    if tool_choice.is_some() {
        body["tool_choice"] = serde_json::json!({"type": "auto"});
    }

    match response_format {
        Some(crate::shared::ResponseFormat::JsonObject) => {
            // ponytail: Anthropic has no response_format field and no prefill
            // is implemented — json_object silently degrades to unenforced
            // output. Upgrade path: system-prompt "respond with JSON only"
            // suffix + assistant prefill `{` when guaranteed JSON is needed
            // (WO 48.6). Warn once instead of per request.
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                tracing::warn!(
                    "json_mode on an Anthropic-family model has no wire effect — \
                     sending without JSON enforcement (no prefill implemented)"
                )
            });
        }
        Some(crate::shared::ResponseFormat::JsonSchema { name, schema }) => {
            let synth = serde_json::json!({
                "name": format!("respond_with_{}", name),
                "description": format!("Respond with JSON conforming to the schema for '{}'.", name),
                "input_schema": schema
            });
            let arr = body.get_mut("tools").and_then(|v| v.as_array_mut());
            if let Some(a) = arr {
                a.push(synth);
            } else {
                body["tools"] = serde_json::json!([synth]);
            }
            body["tool_choice"] =
                serde_json::json!({ "type": "tool", "name": format!("respond_with_{}", name) });
        }
        _ => {}
    }

    // Deterministic mode: pin temperature to 0. Anthropic does not
    // accept a `seed` field, but temperature=0 is the closest we can get.
    if seed.is_some() {
        body["temperature"] = serde_json::json!(0.0);
    }

    body
}

/// Rewrite the `computer` tool entry to Anthropic's hosted computer_use tool
/// type (WO 28.16). The standard `{name, description, input_schema}` shape is
/// replaced with `{"type":"computer_20250124","name":"computer",
/// "display_width_px":W,"display_height_px":H}`. No-op if no `computer` tool
/// is present. Feature-gated: compiles out entirely when `computer_use` is off.
#[cfg(feature = "computer_use")]
fn apply_computer_use(body: &mut serde_json::Value, width: u32, height: u32) {
    let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) else {
        return;
    };
    for tool in tools.iter_mut() {
        if tool.get("name").and_then(|n| n.as_str()) == Some("computer") {
            let preserved_cache_control = tool.get("cache_control").cloned();
            *tool = serde_json::json!({
                "type": "computer_20250124",
                "name": "computer",
                "display_width_px": width,
                "display_height_px": height,
            });
            if let Some(cc) = preserved_cache_control {
                tool["cache_control"] = cc;
            }
        }
    }
}

fn content_block_from_parts(parts: &[ContentPart]) -> serde_json::Value {
    if parts.len() == 1 {
        match &parts[0] {
            ContentPart::Text { text } => serde_json::json!({"type": "text", "text": text}),
            ContentPart::Image { data_base64, mime } => serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": mime,
                    "data": data_base64,
                }
            }),
        }
    } else {
        // Anthropic content blocks are single blocks; collapse mixed parts
        // into a single text block with image placeholders.
        let mut text = String::new();
        for p in parts {
            match p {
                ContentPart::Text { text: t } => text.push_str(t),
                ContentPart::Image { .. } => text.push_str("[image]"),
            }
        }
        serde_json::json!({"type": "text", "text": text})
    }
}

#[cfg(test)]
mod tests {
    use super::usage::parse_usage;
    use super::*;
    use crate::shared::{FinishReason, ToolCallStyle};
    use serde_json::json;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn line(s: &str) -> Vec<u8> {
        format!("data: {s}\n\n").into_bytes()
    }

    fn chunks(
        items: Vec<Vec<u8>>,
    ) -> impl tokio_stream::Stream<Item = Result<Vec<u8>, std::convert::Infallible>> {
        tokio_stream::iter(items.into_iter().map(Ok))
    }

    async fn drain(
        mut rx: tokio::sync::mpsc::Receiver<StreamEvent>,
        max: usize,
    ) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for _ in 0..max {
            match rx.recv().await {
                Some(e) => out.push(e),
                None => break,
            }
        }
        out
    }

    #[test]
    fn body_hoists_system_messages() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "You are helpful.".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "Hello".into(),
                ..Default::default()
            },
        ];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body
            .get("messages")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|m| { m.get("role").and_then(|r| r.as_str()) != Some("system") }));
        assert_eq!(body["system"]["text"], "You are helpful.");
    }

    #[test]
    fn body_marks_last_user_messages_with_cache_control() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "sys".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "a".into(),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: "b".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "c".into(),
                ..Default::default()
            },
        ];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let msgs = body["messages"].as_array().unwrap();
        // First user message: has cache_control (budget allows it).
        assert_eq!(
            msgs[0]["content"].as_array().unwrap().last().unwrap()["cache_control"],
            json!({"type":"ephemeral"})
        );
        // Assistant message: no cache_control (only user msgs get message-level markers).
        assert!(msgs[1]["content"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .get("cache_control")
            .is_none());
        // Last user message: has tail breakpoint.
        assert_eq!(
            msgs[2]["content"].as_array().unwrap().last().unwrap()["cache_control"],
            json!({"type":"ephemeral"})
        );
    }

    #[test]
    fn cache_breakpoint_cap_30_messages_no_tools() {
        cache_breakpoint_cap_for_tools(&[]);
    }

    #[test]
    fn cache_breakpoint_cap_30_messages_with_tools() {
        // Regression: previously emitted 5 breakpoints when tools present
        // (system + 2 prefix + tail + last tool), tripping Anthropic's
        // hard limit of 4. CRIT-1 from WO 20.11.0 audit.
        let tools = vec![crate::shared::ToolDef {
            name: "echo",
            description: "echo",
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }];
        cache_breakpoint_cap_for_tools(&tools);
    }

    fn cache_breakpoint_cap_for_tools(tools: &[crate::shared::ToolDef]) {
        let mut messages = vec![Message {
            role: Role::System,
            content: "sys".into(),
            ..Default::default()
        }];
        for i in 0..29 {
            messages.push(Message {
                role: if i % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: format!("msg{i}"),
                ..Default::default()
            });
        }
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            tools,
            None,
            None,
            false,
            0,
            8192,
            None,
        );
        let mut count = 0;
        if body["system"]
            .get("cache_control")
            .is_some_and(|v| v.is_object())
        {
            count += 1;
        }
        for msg in body["messages"].as_array().unwrap() {
            if let Some(content) = msg.get("content") {
                if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if block.get("cache_control").is_some() {
                            count += 1;
                        }
                    }
                }
            }
        }
        // Tools breakpoint: last tool definition carries cache_control.
        if let Some(tools_arr) = body.get("tools").and_then(|t| t.as_array()) {
            for tool in tools_arr {
                if tool.get("cache_control").is_some() {
                    count += 1;
                }
            }
        }
        assert!(
            count <= 4,
            "expected at most 4 cache breakpoints, got {count}"
        );
    }

    #[tokio::test]
    async fn stream_emits_text_and_done() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"message_start","message":{"role":"assistant","content":[],"usage":{"input_tokens":25,"output_tokens":1}}}"#,
            ),
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"!"}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":0}"#),
            line(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hi", "!"]);
        match events.last() {
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                usage,
            }) => {
                // Merged across message_start (input) + message_delta (output).
                let u = usage.as_ref().expect("usage must be captured");
                assert_eq!(u.prompt_tokens, Some(25));
                assert_eq!(u.completion_tokens, Some(2));
            }
            other => panic!("expected Done(Stop) with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_emits_thinking_and_tool_use() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me"}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" check"}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":0}"#),
            line(
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"read_file","input":{}}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"A"}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"GENTS.md\"}"}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":1}"#),
            line(
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let thinking: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Thinking(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, vec!["Let me", " check"]);
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call event");
        assert_eq!(tool.id, "tu_1");
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments, json!({"path":"AGENTS.md"}));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_reports_api_error() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad model"}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s.contains("bad model"))));
    }

    #[test]
    fn model_info_reasoning_for_claude_3_7() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-3-7-sonnet", 30, None);
        let info = a.model_info();
        assert!(info.supports_thinking);
        assert_eq!(info.tool_call_format, ToolCallStyle::Anthropic);
        assert_eq!(info.max_context_tokens, 200_000);
        assert!(info.supports_cache);
    }

    #[test]
    fn model_info_reasoning_for_claude_4() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4-opus", 30, None);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_3_5() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-3-5-sonnet", 30, None);
        assert!(!a.model_info().supports_thinking);
    }

    // WO 45.62: current-shipping model families (claude-sonnet-5,
    // claude-opus-4-8) must be detected as thinking-capable; Haiku never is.
    #[test]
    fn model_info_reasoning_for_claude_sonnet_5() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-sonnet-5", 30, None);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_reasoning_for_claude_opus_4_8() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-opus-4-8", 30, None);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_haiku_4_5() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-haiku-4-5", 30, None);
        assert!(!a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_images_for_claude_3() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-3-opus", 30, None);
        assert!(a.model_info().supports_images);
    }

    #[test]
    fn model_info_no_images_for_claude_4() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4-opus", 30, None);
        assert!(!a.model_info().supports_images);
    }

    #[test]
    fn new_strips_trailing_slash_from_api_base() {
        let a = AnthropicAdapter::new("https://api.anthropic.com/", "claude-4", 30, None);
        assert_eq!(a.api_base, "https://api.anthropic.com");
    }

    // WO 44.22: the default Anthropic base URL produces the correct
    // endpoint path {api_base}/v1/messages = https://api.anthropic.com/v1/messages.
    #[test]
    fn default_api_base_produces_correct_messages_endpoint() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-sonnet-4", 30, None);
        let endpoint = format!("{}/v1/messages", a.api_base);
        assert_eq!(endpoint, "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn set_json_mode_toggles_flag() {
        let mut a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4", 30, None);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
        assert!(a.response_format.is_some());
        a.set_json_mode(false);
        assert!(!a.json_mode);
        assert!(a.response_format.is_none());
    }

    #[test]
    fn set_seed_sets_value() {
        let mut a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4", 30, None);
        assert!(a.seed.is_none());
        a.set_seed(Some(42));
        assert_eq!(a.seed, Some(42));
    }

    #[test]
    fn body_includes_tools_when_present() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tools = vec![crate::shared::ToolDef {
            name: "read_file",
            description: "read a file",
            parameters: json!({"type": "object"}),
        }];
        let body = build_anthropic_body(
            "claude-4", &messages, &tools, None, None, false, 10_000, 8192, None,
        );
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(tools_arr.len(), 1);
        assert_eq!(tools_arr[0]["name"], "read_file");
        assert_eq!(tools_arr[0]["input_schema"], json!({"type": "object"}));
    }

    #[test]
    fn body_omits_tools_when_empty() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn body_seed_mode_pins_temperature_zero() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            Some(7),
            false,
            10_000,
            8192,
            None,
        );
        assert_eq!(body["temperature"], json!(0.0));
    }

    #[test]
    fn body_no_seed_omits_temperature() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn body_tool_message_becomes_user_tool_result() {
        let messages = vec![Message {
            role: Role::Tool,
            content: "result data".into(),
            tool_call_id: Some("tu_1".into()),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let msg = &body["messages"][0];
        assert_eq!(msg["role"], "user");
        let block = &msg["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "tu_1");
        assert_eq!(block["content"], "result data");
    }

    #[test]
    fn body_tool_message_without_id_uses_empty_string() {
        let messages = vec![Message {
            role: Role::Tool,
            content: "x".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert_eq!(body["messages"][0]["content"][0]["tool_use_id"], "");
    }

    #[test]
    fn body_assistant_with_tool_calls_emits_blocks() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: "thinking...".into(),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "tu_1".into(),
                name: "read_file".into(),
                arguments: json!({"path": "a.md"}),
            }]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "thinking...");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_1");
        assert_eq!(blocks[1]["name"], "read_file");
        assert_eq!(blocks[1]["input"], json!({"path": "a.md"}));
    }

    #[test]
    fn body_assistant_with_tool_calls_no_content_omits_text_block() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: "".into(),
            tool_calls: Some(vec![crate::shared::ToolInvocation {
                id: "tu_1".into(),
                name: "bash".into(),
                arguments: json!({"command": "ls"}),
            }]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let blocks = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_use");
    }

    #[test]
    fn body_user_with_single_image_part_emits_image_block() {
        let messages = vec![Message {
            role: Role::User,
            content: String::new(),
            content_parts: Some(vec![ContentPart::Image {
                data_base64: "BASE64".into(),
                mime: "image/png".into(),
            }]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-3-opus",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "BASE64");
    }

    #[test]
    fn body_user_with_single_text_part_emits_text_block() {
        let messages = vec![Message {
            role: Role::User,
            content: String::new(),
            content_parts: Some(vec![ContentPart::Text {
                text: "just text".into(),
            }]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-3-opus",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "just text");
    }

    #[test]
    fn body_multiple_parts_collapse_to_text() {
        let messages = vec![Message {
            role: Role::User,
            content: String::new(),
            content_parts: Some(vec![
                ContentPart::Text { text: "a".into() },
                ContentPart::Image {
                    data_base64: "B".into(),
                    mime: "image/png".into(),
                },
                ContentPart::Text { text: "b".into() },
            ]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-3-opus",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let block = &body["messages"][0]["content"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "a[image]b");
    }

    #[test]
    fn body_single_system_uses_object_not_array() {
        let messages = vec![Message {
            role: Role::System,
            content: "sys".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body["system"].is_object());
        assert!(!body["system"].is_array());
        assert_eq!(body["system"]["text"], "sys");
        assert_eq!(
            body["system"]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn body_multiple_system_blocks_use_array() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "sys1".into(),
                ..Default::default()
            },
            Message {
                role: Role::System,
                content: "sys2".into(),
                ..Default::default()
            },
        ];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let arr = body["system"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "sys1");
        assert_eq!(arr[1]["text"], "sys2");
        assert_eq!(arr[1]["cache_control"], json!({"type": "ephemeral"}));
        assert!(arr[0].get("cache_control").is_none());
    }

    #[test]
    fn body_no_system_omits_system_field() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body.get("system").is_none());
    }

    #[test]
    fn body_short_conversation_has_tail_breakpoint() {
        let messages = vec![
            Message {
                role: Role::User,
                content: "a".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "b".into(),
                ..Default::default()
            },
        ];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        // Short conversations: no prefix markers, but the tail breakpoint
        // should still be present on the last user message (WO 17.5).
        let msgs = body["messages"].as_array().unwrap();
        let last_msg = msgs.last().unwrap();
        let last_block = last_msg["content"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last_block["cache_control"],
            json!({"type": "ephemeral"}),
            "tail breakpoint must be present on last user message"
        );
    }

    /// WO 17.5: the last tool definition carries cache_control: ephemeral
    /// so the system+tools block is cached as a single unit.
    #[test]
    fn body_tools_block_has_cache_control_on_last_tool() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tools = vec![
            crate::shared::ToolDef {
                name: "read_file",
                description: "read a file",
                parameters: serde_json::json!({"type": "object"}),
            },
            crate::shared::ToolDef {
                name: "bash",
                description: "run a command",
                parameters: serde_json::json!({"type": "object"}),
            },
        ];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &tools,
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let tools_arr = body["tools"].as_array().unwrap();
        // First tool has no cache_control.
        assert!(
            tools_arr[0].get("cache_control").is_none(),
            "first tool should not have cache_control"
        );
        // Last tool has cache_control: ephemeral (WO 17.5 system+tools breakpoint).
        assert_eq!(
            tools_arr[1]["cache_control"],
            json!({"type": "ephemeral"}),
            "last tool must carry cache_control for system+tools breakpoint"
        );
    }

    /// WO 17.5: the last user message's last content block carries
    /// cache_control: ephemeral (the tail breakpoint for cross-turn cache).
    #[test]
    fn body_tail_breakpoint_on_last_user_message() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "sys".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "ask1".into(),
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: "reply1".into(),
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: "ask2".into(),
                ..Default::default()
            },
        ];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let msgs = body["messages"].as_array().unwrap();
        // The last message is the trailing user turn.
        let last_msg = msgs.last().unwrap();
        let last_block = last_msg["content"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last_block["cache_control"],
            json!({"type": "ephemeral"}),
            "last user message must have tail cache_control (WO 17.5)"
        );
    }

    /// WO 20.2.0 P2: cache breakpoints must not exceed 4, even in long
    /// conversations. With system + 2 user messages, we get: 1 system + 2
    /// user = 3 breakpoints. Adding tools: +1 = 4 total.
    #[test]
    fn body_cache_breakpoints_capped_at_4() {
        let mut messages = vec![Message {
            role: Role::System,
            content: "sys".into(),
            ..Default::default()
        }];
        for i in 0..10 {
            messages.push(Message {
                role: Role::User,
                content: format!("user-{i}"),
                ..Default::default()
            });
            messages.push(Message {
                role: Role::Assistant,
                content: format!("reply-{i}"),
                ..Default::default()
            });
        }
        let tools = vec![crate::shared::ToolDef {
            name: "bash",
            description: "x",
            parameters: serde_json::json!({"type": "object"}),
        }];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &tools,
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );

        let mut count = 0;

        // Count in system blocks
        if let Some(sys) = body.get("system") {
            if sys.is_object() {
                if sys.get("cache_control").is_some() {
                    count += 1;
                }
            } else if let Some(arr) = sys.as_array() {
                count += arr
                    .iter()
                    .filter(|b| b.get("cache_control").is_some())
                    .count();
            }
        }

        // Count in tools
        if let Some(tools_arr) = body.get("tools").and_then(|t| t.as_array()) {
            count += tools_arr
                .iter()
                .filter(|t| t.get("cache_control").is_some())
                .count();
        }

        // Count in messages
        if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in msgs {
                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                    count += content
                        .iter()
                        .filter(|b| b.get("cache_control").is_some())
                        .count();
                }
            }
        }

        assert_eq!(
            count, 4,
            "cache breakpoints must be capped at 4, got {count}"
        );
    }

    #[test]
    fn body_system_with_parts_uses_blocks() {
        let messages = vec![Message {
            role: Role::System,
            content: String::new(),
            content_parts: Some(vec![ContentPart::Text {
                text: "system text".into(),
            }]),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert_eq!(body["system"]["type"], "text");
        assert_eq!(body["system"]["text"], "system text");
    }

    #[test]
    fn body_max_tokens_is_8192() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn body_max_tokens_is_configurable() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            16384,
            None,
        );
        assert_eq!(body["max_tokens"], 16384);
    }

    #[test]
    fn body_extended_thinking_emits_thinking_param() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            true,
            16384,
            8192,
            None,
        );
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 16384);
    }

    #[test]
    fn body_no_extended_thinking_omits_thinking_param() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn body_stream_is_true() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body(
            "claude-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn stream_done_sentinel_emits_done() {
        let events: Vec<Vec<u8>> = vec![b"data: [DONE]\n\n".to_vec()];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_message_stop_max_tokens_maps_to_length() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"message_stop","stop_reason":"max_tokens"}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Length,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_message_stop_tool_use_maps_to_tool_calls() {
        let events: Vec<Vec<u8>> =
            vec![line(r#"{"type":"message_stop","stop_reason":"tool_use"}"#)];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_message_stop_unknown_reason_maps_to_stop() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"message_stop","stop_reason":"weird_reason"}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
    }

    // WO 38.5: this test previously pinned the MOCK's wrong shape —
    // usage riding on `message_stop`. The real API puts the input side
    // (input_tokens / cache_read / cache_creation) on `message_start`'s
    // `message.usage` and the final `output_tokens` on `message_delta`'s
    // `usage`; `message_stop` carries neither. Rewritten to the real wire.
    #[tokio::test]
    async fn stream_message_stop_with_usage_emits_usage() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":1}}}"#,
            ),
            line(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().unwrap();
                assert_eq!(u.prompt_tokens, Some(5));
                assert_eq!(u.completion_tokens, Some(7));
                assert_eq!(u.cached_tokens, Some(2));
                assert_eq!(u.cache_write_tokens, Some(3));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    /// message_delta usage alone (no message_start usage) still surfaces —
    /// e.g. Bedrock frames dropped mid-stream.
    #[tokio::test]
    async fn stream_message_delta_usage_alone_surfaces() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":11,"output_tokens":13}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().unwrap();
                assert_eq!(u.prompt_tokens, Some(11));
                assert_eq!(u.completion_tokens, Some(13));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_message_delta_stop_reason_carried_to_message_stop() {
        let events: Vec<Vec<u8>> = vec![
            line(r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Length,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn stream_text_block_start_with_text_emits_text() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":"preface"}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "preface")));
    }

    #[tokio::test]
    async fn stream_unknown_event_type_is_ignored() {
        let events: Vec<Vec<u8>> = vec![
            line(r#"{"type":"some_unknown_event","foo":"bar"}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Done { .. })));
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Error(_))));
    }

    #[tokio::test]
    async fn stream_invalid_json_emits_error() {
        let events: Vec<Vec<u8>> = vec![b"data: {not valid json\n\n".to_vec()];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s.contains("JSON parse"))));
    }

    #[tokio::test]
    async fn stream_invalid_utf8_emits_error() {
        let bad = b"data: \xC3\n\n".to_vec();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(vec![bad]), crate::adapters::STREAM_IDLE_TIMEOUT)
                .await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s.contains("UTF-8"))));
    }

    #[tokio::test]
    async fn stream_transport_error_emits_error() {
        let items = vec![Err::<Vec<u8>, std::io::Error>(std::io::Error::other(
            "connection reset",
        ))];
        let stream = tokio_stream::iter(items);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        parse_anthropic_stream(tx, stream, crate::adapters::STREAM_IDLE_TIMEOUT).await;
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s == "connection reset")));
    }

    #[tokio::test]
    async fn stream_eof_without_done_emits_done() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    #[tokio::test]
    async fn stream_eof_flushes_pending_tool_call() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_9","name":"bash","input":{}}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":\"ls\"}"}}"#,
            ),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call should be flushed at EOF");
        assert_eq!(tool.id, "tu_9");
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.arguments, json!({"cmd": "ls"}));
    }

    #[tokio::test]
    async fn stream_eof_with_truncated_tool_use_emits_tool_call() {
        // content_block_start arrives, but the connection drops before
        // any partial_json. The pending tool has no input, so the tool
        // was attempted but truncated. The stream must emit a ToolCall
        // with an empty input (not silently drop the tool) followed by
        // Done (WO 15.11).
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_x","name":"bash","input":{}}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("truncated tool call should still be emitted at EOF");
        assert_eq!(tool.id, "tu_x");
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.arguments, json!({}));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    /// WO 38.5: a tool_use block with NO partial_json deltas is a
    /// zero-argument call. The old `input.is_some()` gate at
    /// content_block_stop dropped it entirely.
    #[tokio::test]
    async fn stream_zero_arg_tool_call_flushes_at_content_block_stop() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_z","name":"list_dir","input":{}}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":0}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("zero-arg tool call must be flushed");
        assert_eq!(tool.id, "tu_z");
        assert_eq!(tool.name, "list_dir");
        assert_eq!(tool.arguments, json!({}));
    }

    /// WO 38.5: EOF without message_stop is truncation — Done{Error},
    /// not a synthesized Done(Stop) that launders a half-reply into a
    /// complete turn.
    #[tokio::test]
    async fn stream_eof_without_done_maps_to_error() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"half"}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "half")));
        match events.last() {
            Some(StreamEvent::Done { finish_reason, .. }) => {
                assert_eq!(finish_reason, &FinishReason::Error);
            }
            other => panic!("expected Done{{Error}} at EOF, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_content_block_stop_emits_tool_call() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_2","name":"read_file","input":{}}}"#,
            ),
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":0}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call");
        assert_eq!(tool.id, "tu_2");
        assert_eq!(tool.arguments, json!({"path": "x"}));
    }

    #[tokio::test]
    async fn stream_tool_use_with_initial_input_object() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_3","name":"bash","input":{"preset":"value"}}}"#,
            ),
            line(r#"{"type":"content_block_stop","index":0}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call");
        assert_eq!(tool.arguments, json!({"preset": "value"}));
    }

    #[tokio::test]
    async fn stream_accepts_crlf_line_endings() {
        let payload = serde_json::to_string(&json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}})).unwrap();
        let frame = format!("data: {payload}\r\n\r\n");
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(
                tx,
                chunks(vec![frame.into_bytes()]),
                crate::adapters::STREAM_IDLE_TIMEOUT,
            )
            .await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    #[tokio::test]
    async fn stream_api_error_without_message_field_uses_error_string() {
        let events: Vec<Vec<u8>> = vec![line(r#"{"type":"error","error":"something broke"}"#)];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s == "something broke")));
    }

    #[tokio::test]
    async fn stream_thinking_delta_empty_string_skipped() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Thinking(_))));
    }

    #[tokio::test]
    async fn stream_text_delta_empty_string_skipped() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":""}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
    }

    #[tokio::test]
    async fn stream_content_block_start_unknown_type_ignored() {
        let events: Vec<Vec<u8>> = vec![
            line(
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"weird_block","text":"x"}}"#,
            ),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
    }

    #[tokio::test]
    async fn stream_message_start_is_noop() {
        let events: Vec<Vec<u8>> = vec![
            line(r#"{"type":"message_start","message":{"role":"assistant","content":[]}}"#),
            line(r#"{"type":"message_stop"}"#),
        ];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events), crate::adapters::STREAM_IDLE_TIMEOUT).await;
        });
        let events = drain(rx, 64).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::Done { .. }));
    }

    #[test]
    fn find_subseq_locates_needle() {
        assert_eq!(super::find_subseq(b"hello world", b"world"), Some(6));
        assert_eq!(super::find_subseq(b"hello", b"xyz"), None);
        assert_eq!(super::find_subseq(b"", b"x"), None);
    }

    #[test]
    fn trim_ascii_whitespace_strips_both_ends() {
        assert_eq!(super::trim_ascii_whitespace(b"  hi  "), b"hi");
        assert_eq!(super::trim_ascii_whitespace(b"\n\tdata\r\n"), b"data");
        assert_eq!(trim_ascii_whitespace(b"   "), b"");
    }

    #[test]
    fn parse_usage_extracts_all_token_fields() {
        let u = json!({
            "input_tokens": 10,
            "output_tokens": 20,
            "cache_read_input_tokens": 5,
            "cache_creation_input_tokens": 7
        });
        let t = parse_usage(&u);
        assert_eq!(t.prompt_tokens, Some(10));
        assert_eq!(t.completion_tokens, Some(20));
        assert_eq!(t.cached_tokens, Some(5));
        assert_eq!(t.cache_write_tokens, Some(7));
    }

    #[test]
    fn parse_usage_handles_missing_fields() {
        let u = json!({});
        let t = parse_usage(&u);
        assert_eq!(t.prompt_tokens, None);
        assert_eq!(t.completion_tokens, None);
        assert_eq!(t.cached_tokens, None);
        assert_eq!(t.cache_write_tokens, None);
    }

    #[test]
    fn stream_returns_error_when_no_api_key() {
        // With no key configured and no env var, stream should fail
        // with a clear error message.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = crate::shared::test_util::EnvGuard::remove("ANTHROPIC_API_KEY");
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4", 30, None);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(a.stream(&[], &[]));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no Anthropic API key"),
            "expected 'no Anthropic API key' error, got: {err}"
        );
    }

    #[test]
    fn stream_returns_error_when_empty_api_key() {
        // An empty config key should still fall through to env,
        // and if env is also missing, produce the error.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = crate::shared::test_util::EnvGuard::remove("ANTHROPIC_API_KEY");
        let a = AnthropicAdapter::new(
            "https://api.anthropic.com",
            "claude-4",
            30,
            Some(String::new()),
        );
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(a.stream(&[], &[]));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no Anthropic API key"),
            "expected 'no Anthropic API key' error, got: {err}"
        );
    }

    #[test]
    fn config_key_takes_priority_over_env() {
        // When both config and env are set, config wins.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = crate::shared::test_util::EnvGuard::set("ANTHROPIC_API_KEY", "env-key");
        let key = super::super::auth::resolve_api_key("anthropic", Some("config-key"));
        assert_eq!(key, Some("config-key".to_string()));
    }

    #[test]
    fn none_when_both_missing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = crate::shared::test_util::EnvGuard::remove("ANTHROPIC_API_KEY");
        let key = super::super::auth::resolve_api_key("anthropic", None);
        assert!(key.is_none());
    }

    // ---- WO 28.16: hosted computer_use beta gate tests ----

    /// Critical safety check (WO 28.16 R1 / success criterion #1): with the
    /// `computer_use` Cargo feature OFF, the adapter body MUST NOT emit the
    /// hosted `computer` tool type. Even when the caller registers a tool
    /// literally named "computer" and sets the dims builder, the default
    /// build serializes it as a standard `{name, description, input_schema}`
    /// tool — no `computer_20250124` bytes reach the wire.
    #[test]
    fn feature_off_emits_no_computer_tool_type() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tools = vec![crate::shared::ToolDef {
            name: "computer",
            description: "screen control",
            parameters: json!({"type": "object"}),
        }];
        let body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &tools,
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let wire = body.to_string();
        assert!(
            !wire.contains("computer_20250124"),
            "hosted computer tool type leaked into wire with feature OFF: {wire}"
        );
        assert!(
            !wire.contains("display_width_px"),
            "hosted display dims leaked into wire with feature OFF: {wire}"
        );
        // The tool is still present, but as a standard schema.
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(tools_arr[0]["name"], "computer");
        assert_eq!(tools_arr[0]["input_schema"], json!({"type": "object"}));
    }

    /// With the feature OFF, the dims builder is inert — the field is stored
    /// but no wire-level effect (no header, no tool rewrite). This complements
    /// the compile-time gate: the runtime safety is also asserted.
    #[test]
    fn feature_off_dims_builder_is_inert() {
        let a = AnthropicAdapter::new("https://api.anthropic.com", "claude-sonnet-4", 30, None)
            .with_computer_use(Some((1024, 768)));
        // Field stored regardless of feature so construction code is uniform.
        assert_eq!(a.computer_use_dims, Some((1024, 768)));
        // The apply_computer_use helper does not exist with the feature off,
        // so there is no code path that can rewrite the tool type.
    }

    #[cfg(feature = "computer_use")]
    #[test]
    fn feature_on_apply_computer_use_rewrites_tool() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tools = vec![
            crate::shared::ToolDef {
                name: "read_file",
                description: "read",
                parameters: json!({"type": "object"}),
            },
            crate::shared::ToolDef {
                name: "computer",
                description: "screen control",
                parameters: json!({"type": "object"}),
            },
        ];
        let mut body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &tools,
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        apply_computer_use(&mut body, 1024, 768);
        let tools_arr = body["tools"].as_array().unwrap();
        // Non-computer tool untouched.
        assert_eq!(tools_arr[0]["name"], "read_file");
        assert_eq!(tools_arr[0]["input_schema"], json!({"type": "object"}));
        // Computer tool rewritten to hosted type.
        assert_eq!(tools_arr[1]["type"], "computer_20250124");
        assert_eq!(tools_arr[1]["name"], "computer");
        assert_eq!(tools_arr[1]["display_width_px"], 1024);
        assert_eq!(tools_arr[1]["display_height_px"], 768);
        // No input_schema on the rewritten tool.
        assert!(tools_arr[1].get("input_schema").is_none());
        // The WO 17.5 cache_control breakpoint on the last tool is preserved.
        assert_eq!(tools_arr[1]["cache_control"], json!({"type": "ephemeral"}));
    }

    #[cfg(feature = "computer_use")]
    #[test]
    fn feature_on_apply_computer_use_noop_without_computer_tool() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let tools = vec![crate::shared::ToolDef {
            name: "read_file",
            description: "read",
            parameters: json!({"type": "object"}),
        }];
        let mut body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &tools,
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let before = body.clone();
        apply_computer_use(&mut body, 1024, 768);
        assert_eq!(body, before, "body must be unchanged when no computer tool");
    }

    #[cfg(feature = "computer_use")]
    #[test]
    fn feature_on_apply_computer_use_noop_without_tools() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let mut body = build_anthropic_body(
            "claude-sonnet-4",
            &messages,
            &[],
            None,
            None,
            false,
            10_000,
            8192,
            None,
        );
        let before = body.clone();
        apply_computer_use(&mut body, 1024, 768);
        assert_eq!(body, before);
    }

    #[cfg(feature = "computer_use")]
    #[tokio::test]
    async fn feature_on_parses_computer_tool_result_block() {
        use super::content_blocks;
        // The hosted model returns a computer_tool_result block. The vision
        // loop (R4) is deferred, so the parser surfaces a text placeholder.
        let block = json!({
            "type": "computer_tool_result",
            "action": {"type": "click", "coordinate": [120, 340]},
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(8);
        let mut pending = None;
        content_blocks::handle_content_block_start(&block, &mut pending, &tx).await;
        let ev = rx.recv().await.expect("expected a text placeholder event");
        match ev {
            StreamEvent::Text(s) => {
                assert!(s.contains("computer_tool_result"), "got: {s}");
                assert!(s.contains("click"), "got: {s}");
            }
            other => panic!("expected Text placeholder, got {other:?}"),
        }
        // No tool_use pending was created for a computer_tool_result block.
        assert!(pending.is_none());
    }
}
