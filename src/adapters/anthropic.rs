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

use crate::shared::{
    ContentPart, FinishReason, Message, ModelInfo, Role, StreamEvent, TokenUsage, ToolInvocation,
};
use tokio_stream::StreamExt;

use super::{find_subseq, trim_ascii_whitespace, ModelAdapter, MAX_SSE_BUFFER_BYTES};

/// Anthropic Messages API version we target.
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicAdapter {
    model: String,
    api_base: String,
    api_key: Option<String>,
    client: reqwest::Client,
    json_mode: bool,
    seed: Option<u64>,
    timeout_secs: u64,
    extended_thinking: bool,
    budget_tokens: usize,
}

impl AnthropicAdapter {
    pub fn new(api_base: &str, model: &str, timeout_secs: u64, api_key: Option<String>) -> Self {
        Self {
            model: model.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
            api_key,
            client: super::build_reqwest_client(),
            json_mode: false,
            seed: None,
            timeout_secs,
            extended_thinking: true,
            budget_tokens: 10_000,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicAdapter {
    fn model_info(&self) -> ModelInfo {
        super::anthropic_model_info(&self.model, "claude-3")
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
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
            self.json_mode,
            self.seed,
            self.extended_thinking,
            self.budget_tokens,
        );
        let url = format!("{}/v1/messages", self.api_base);

        let response = super::send_with_retry(|| async {
            self.client
                .post(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
        })
        .await?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        tokio::spawn(parse_anthropic_stream(tx, response.bytes_stream()));
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
pub(crate) fn build_anthropic_body(
    model: &str,
    messages: &[Message],
    tools: &[crate::shared::ToolDef],
    json_mode: bool,
    seed: Option<u64>,
    extended_thinking: bool,
    budget_tokens: usize,
) -> serde_json::Value {
    let lower = model.to_lowercase();
    let supports_thinking = lower.contains("claude-3-7-sonnet") || lower.contains("claude-4");

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

    // Apply cache breakpoints (Anthropic hard limit: 4 per request).
    // Budget allocation (4 total):
    //   - System block:          1 (set above on last system block)
    //   - Last tool definition:  1 (only when tools present — covers the
    //                            system+tools prefix as one cached unit)
    //   - Tail user message:     1 (set below; grows-with-conversation cache)
    //   - Mid prefix messages:   1 when tools present, 2 when no tools
    // ponytail: cap is 4 breakpoints total. With tools we get
    // system(1) + tool(1) + 1 prefix + tail(1) = 4. Without tools,
    // system(1) + 2 prefix + tail(1) = 4. Either way the cap holds.
    if anthropic_messages.len() > 2 {
        let prefix_end = anthropic_messages.len() - 1;
        let prefix_budget = if tools.is_empty() { 2 } else { 1 };
        let skip_from_start = prefix_end.saturating_sub(prefix_budget + 1);
        for msg in anthropic_messages
            .iter_mut()
            .take(prefix_end)
            .skip(1 + skip_from_start)
        {
            if let Some(content) = msg.get_mut("content") {
                if let Some(arr) = content.as_array_mut() {
                    if let Some(last_block) = arr.last_mut() {
                        last_block["cache_control"] = serde_json::json!({"type": "ephemeral"});
                    }
                }
            }
        }
    }

    // Tail breakpoint: mark the last user message's last content block
    // with cache_control: ephemeral so the conversation tail is cached
    // for the next turn (WO 17.5).
    if !anthropic_messages.is_empty() {
        if let Some(last_msg) = anthropic_messages.last_mut() {
            if let Some(content) = last_msg.get_mut("content") {
                if let Some(arr) = content.as_array_mut() {
                    if let Some(last_block) = arr.last_mut() {
                        // Only add if not already present (prefix markers
                        // may have added it for short conversations).
                        if last_block.get("cache_control").is_none() {
                            last_block["cache_control"] = serde_json::json!({"type": "ephemeral"});
                        }
                    }
                }
            }
        }
    }

    // ceiling: max_tokens hardcoded to 8192; not configurable. Making it a
    // Config field touches every Config site (Default impl, test literals
    // across executor/tests, adapter_for wrappers) — out of polish-batch
    // scope. upgrade path: Config field (WO 15.26 3.23 deferred).
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": 8192,
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
            body["system"] = system_blocks.into_iter().next().unwrap();
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

    if json_mode {
        // Anthropic supports JSON mode via explicit prefill / tool-free
        // instructions rather than a response_format field. We do not add an
        // unsupported top-level key; callers are expected to use a system
        // prompt that asks for JSON.
    }

    // Deterministic mode: pin temperature to 0. Anthropic does not
    // accept a `seed` field, but temperature=0 is the closest we can get.
    if seed.is_some() {
        body["temperature"] = serde_json::json!(0.0);
    }

    body
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

/// Drive an Anthropic Messages API SSE byte stream into `StreamEvent`s.
pub(crate) async fn parse_anthropic_stream<B, E, S>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: S,
) where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
    S: tokio_stream::Stream<Item = Result<B, E>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut pending_tool: Option<PendingToolUse> = None;
    let mut done_emitted = false;
    let mut pending_stop_reason: Option<String> = None;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                buffer.extend_from_slice(bytes.as_ref());
                if buffer.len() > MAX_SSE_BUFFER_BYTES {
                    let _ = tx
                        .send(StreamEvent::Error(format!(
                            "SSE frame buffer exceeded {} MiB limit; aborting stream",
                            MAX_SSE_BUFFER_BYTES / (1024 * 1024)
                        )))
                        .await;
                    return;
                }

                while let Some(start) = find_subseq(&buffer, b"data: ") {
                    let after_start = start + 6;
                    let after = &buffer[after_start..];
                    let sep = [
                        b"\n\n".as_slice(),
                        b"\r\n\r\n".as_slice(),
                        b"\r\r".as_slice(),
                    ]
                    .iter()
                    .filter_map(|t| find_subseq(after, t).map(|i| (i, t.len())))
                    .min_by_key(|(i, _)| *i);
                    let Some((sep_idx, term_len)) = sep else {
                        break;
                    };
                    let payload_end = after_start + sep_idx;
                    let drain_to = payload_end + term_len;
                    let payload = trim_ascii_whitespace(&buffer[after_start..payload_end]).to_vec();
                    buffer.drain(..drain_to);

                    if payload.is_empty() {
                        continue;
                    }

                    // Event type lives on the preceding `event: ...` line. We
                    // only need the data payload for parsing, so sniff it.
                    let line = match std::str::from_utf8(&payload) {
                        Ok(s) => s,
                        Err(e) => {
                            if !super::ollama_ndjson::send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("SSE frame is not valid UTF-8: {e}")),
                                "Anthropic UTF-8 decode error",
                            )
                            .await
                            {
                                return;
                            }
                            continue;
                        }
                    };

                    if line == "[DONE]" {
                        if !send_done(&tx, &mut done_emitted, FinishReason::Stop, None).await {
                            return;
                        }
                        continue;
                    }

                    match serde_json::from_str::<serde_json::Value>(line) {
                        Ok(json) => {
                            if let Some(err) = json.get("error") {
                                let msg = err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or_else(|| err.as_str().unwrap_or("API error"))
                                    .to_string();
                                if !super::ollama_ndjson::send_or_bail(
                                    &tx,
                                    StreamEvent::Error(msg),
                                    "Anthropic API error",
                                )
                                .await
                                {
                                    return;
                                }
                                continue;
                            }

                            let event_type =
                                json.get("type").and_then(|t| t.as_str()).unwrap_or("");

                            match event_type {
                                "message_start" => {}
                                "message_delta" => {
                                    // message_delta carries stop_reason and final usage.
                                    // We merge stop_reason into message_stop by remembering it here.
                                    if let Some(r) = json
                                        .get("delta")
                                        .and_then(|d| d.get("stop_reason"))
                                        .and_then(|r| r.as_str())
                                    {
                                        pending_stop_reason = Some(r.to_string());
                                    }
                                }
                                "content_block_start" => {
                                    if let Some(block) = json.get("content_block") {
                                        handle_content_block_start(block, &mut pending_tool, &tx)
                                            .await;
                                    }
                                }
                                "content_block_delta" => {
                                    if let Some(delta) = json.get("delta") {
                                        handle_content_block_delta(delta, &mut pending_tool, &tx)
                                            .await;
                                    }
                                }
                                "content_block_stop" => {
                                    if let Some(tool) = pending_tool.take() {
                                        if tool.input.is_some()
                                            && !super::ollama_ndjson::send_or_bail(
                                                &tx,
                                                StreamEvent::ToolCall(tool.into_invocation()),
                                                "Anthropic tool_use stop",
                                            )
                                            .await
                                        {
                                            return;
                                        }
                                    }
                                }
                                "message_stop" => {
                                    let reason = json
                                        .get("stop_reason")
                                        .and_then(|r| r.as_str())
                                        .or(pending_stop_reason.as_deref())
                                        .unwrap_or("end_turn");
                                    let finish_reason = match reason {
                                        "max_tokens" => FinishReason::Length,
                                        "tool_use" => FinishReason::ToolCalls,
                                        _ => FinishReason::Stop,
                                    };
                                    // Usage appears on message_delta; if
                                    // message_stop also has it, prefer that.
                                    let usage = json.get("usage").map(parse_usage);
                                    if !send_done(&tx, &mut done_emitted, finish_reason, usage)
                                        .await
                                    {
                                        return;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            if !super::ollama_ndjson::send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("JSON parse: {e}")),
                                "Anthropic JSON parse error",
                            )
                            .await
                            {
                                return;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let _ = super::ollama_ndjson::send_or_bail(
                    &tx,
                    StreamEvent::Error(e.to_string()),
                    "Anthropic transport error",
                )
                .await;
                break;
            }
        }
    }

    if !done_emitted {
        if let Some(tool) = pending_tool.take() {
            // A pending tool with `input.is_some()` is the normal EOF
            // flush (content_block_start + partial_json, no
            // content_block_stop). A pending tool with `input.is_none()`
            // means content_block_start arrived but the connection
            // dropped before any partial_json — the tool was attempted
            // but truncated. Emit a ToolCall with an empty input so the
            // executor knows a tool was attempted rather than seeing an
            // empty turn (WO 15.11).
            let _ = tx.send(StreamEvent::ToolCall(tool.into_invocation())).await;
        }
        let _ = send_done(&tx, &mut done_emitted, FinishReason::Stop, None).await;
    }
}

async fn handle_content_block_start(
    block: &serde_json::Value,
    pending: &mut Option<PendingToolUse>,
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
) {
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match block_type {
        "thinking" => {}
        "tool_use" => {
            let id = block
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = block
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Anthropic streams `input: {}` at block start and then sends the
            // real JSON via `partial_json` deltas. Treat an empty object as no
            // initial input so accumulation starts from a clean string.
            let input = block
                .get("input")
                .cloned()
                .filter(|v| !(v.as_object().map(|o| o.is_empty()).unwrap_or(false)));
            *pending = Some(PendingToolUse { id, name, input });
        }
        "text" => {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                if !t.is_empty() {
                    let _ = super::ollama_ndjson::send_or_bail(
                        tx,
                        StreamEvent::Text(t.to_string()),
                        "Anthropic text block start",
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
}

async fn handle_content_block_delta(
    delta: &serde_json::Value,
    pending: &mut Option<PendingToolUse>,
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
) {
    if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
        if !t.is_empty() {
            let _ = super::ollama_ndjson::send_or_bail(
                tx,
                StreamEvent::Thinking(t.to_string()),
                "Anthropic thinking delta",
            )
            .await;
        }
        return;
    }

    if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
        if !t.is_empty() {
            let _ = super::ollama_ndjson::send_or_bail(
                tx,
                StreamEvent::Text(t.to_string()),
                "Anthropic text delta",
            )
            .await;
        }
        return;
    }

    if let Some(partial) = delta.get("partial_json").and_then(|p| p.as_str()) {
        if let Some(tool) = pending.as_mut() {
            tool.append_json(partial);
        }
    }
}

#[derive(Debug, Default)]
struct PendingToolUse {
    id: String,
    name: String,
    input: Option<serde_json::Value>,
}

impl PendingToolUse {
    fn append_json(&mut self, partial: &str) {
        // Anthropic streams `partial_json` as string fragments. Accumulate the
        // raw JSON string and defer parsing until the block stops.
        let mut buffer = match self.input.take() {
            Some(serde_json::Value::String(s)) => s,
            // Ignore any non-string seed (e.g. an empty object sent at block start).
            Some(_) => String::new(),
            None => String::new(),
        };
        buffer.push_str(partial);
        self.input = Some(serde_json::Value::String(buffer));
    }

    fn into_invocation(mut self) -> ToolInvocation {
        let arguments = match self.input.take() {
            Some(serde_json::Value::String(s)) => {
                serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
            }
            Some(v) => v,
            None => serde_json::Value::Object(serde_json::Map::new()),
        };
        ToolInvocation {
            id: self.id,
            name: self.name,
            arguments,
        }
    }
}

fn parse_usage(u: &serde_json::Value) -> TokenUsage {
    let prompt_tokens = u
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let completion_tokens = u
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let cached_tokens = u
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
    }
}

async fn send_done(
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    done_emitted: &mut bool,
    finish_reason: FinishReason,
    usage: Option<TokenUsage>,
) -> bool {
    if *done_emitted {
        return true;
    }
    if super::ollama_ndjson::send_or_bail(
        tx,
        StreamEvent::Done {
            finish_reason,
            usage,
        },
        "Anthropic done",
    )
    .await
    {
        *done_emitted = true;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolCallStyle;
    use serde_json::json;

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
            false,
            None,
            false,
            10_000,
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
    fn body_marks_last_two_prefix_messages_with_cache_control() {
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
            false,
            None,
            false,
            10_000,
        );
        let msgs = body["messages"].as_array().unwrap();
        // First user message (prefix) is skipped for cache markers.
        assert!(msgs[0]
            .get("content")
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .get("cache_control")
            .is_none());
        // Assistant message (second prefix) gets a cache marker.
        assert_eq!(
            msgs[1]["content"].as_array().unwrap().last().unwrap()["cache_control"],
            json!({"type":"ephemeral"})
        );
        // Last user message gets a tail breakpoint (WO 17.5).
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
        let body = build_anthropic_body("claude-sonnet-4", &messages, tools, false, None);
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
            line(r#"{"type":"message_start","message":{"role":"assistant","content":[]}}"#),
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
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
            parse_anthropic_stream(tx, chunks(events)).await;
        });
        let events = drain(rx, 64).await;
        for (idx, ev) in events.iter().enumerate() {
            eprintln!("event[{idx}]: {ev:?}");
        }
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
            parse_anthropic_stream(tx, chunks(events)).await;
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

    #[test]
    fn set_json_mode_toggles_flag() {
        let mut a = AnthropicAdapter::new("https://api.anthropic.com", "claude-4", 30, None);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
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
        let body = build_anthropic_body("claude-4", &messages, &tools, false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn body_seed_mode_pins_temperature_zero() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body("claude-4", &messages, &[], false, Some(7), false, 10_000);
        assert_eq!(body["temperature"], json!(0.0));
    }

    #[test]
    fn body_no_seed_omits_temperature() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body =
            build_anthropic_body("claude-3-opus", &messages, &[], false, None, false, 10_000);
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
        let body =
            build_anthropic_body("claude-3-opus", &messages, &[], false, None, false, 10_000);
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
        let body =
            build_anthropic_body("claude-3-opus", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
            false,
            None,
            false,
            10_000,
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
            false,
            None,
            false,
            10_000,
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
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
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn body_stream_is_true() {
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            ..Default::default()
        }];
        let body = build_anthropic_body("claude-4", &messages, &[], false, None, false, 10_000);
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn stream_done_sentinel_emits_done() {
        let events: Vec<Vec<u8>> = vec![b"data: [DONE]\n\n".to_vec()];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
    async fn stream_message_stop_with_usage_emits_usage() {
        let events: Vec<Vec<u8>> = vec![line(
            r#"{"type":"message_stop","usage":{"input_tokens":5,"output_tokens":7,"cache_read_input_tokens":2}}"#,
        )];
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            parse_anthropic_stream(tx, chunks(events)).await;
        });
        let events = drain(rx, 64).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().unwrap();
                assert_eq!(u.prompt_tokens, Some(5));
                assert_eq!(u.completion_tokens, Some(7));
                assert_eq!(u.cached_tokens, Some(2));
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(vec![bad])).await;
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
        parse_anthropic_stream(tx, stream).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(vec![frame.into_bytes()])).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
            parse_anthropic_stream(tx, chunks(events)).await;
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
        let u = json!({"input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 5});
        let t = parse_usage(&u);
        assert_eq!(t.prompt_tokens, Some(10));
        assert_eq!(t.completion_tokens, Some(20));
        assert_eq!(t.cached_tokens, Some(5));
    }

    #[test]
    fn parse_usage_handles_missing_fields() {
        let u = json!({});
        let t = parse_usage(&u);
        assert_eq!(t.prompt_tokens, None);
        assert_eq!(t.completion_tokens, None);
        assert_eq!(t.cached_tokens, None);
    }

    #[test]
    fn stream_returns_error_when_no_api_key() {
        // With no key configured and no env var, stream should fail
        // with a clear error message.
        std::env::remove_var("ANTHROPIC_API_KEY");
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
        std::env::remove_var("ANTHROPIC_API_KEY");
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
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let key = super::super::auth::resolve_api_key("anthropic", Some("config-key"));
        assert_eq!(key, Some("config-key".to_string()));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn env_key_used_when_no_config() {
        // When config is None, env key is used.
        std::env::set_var("ANTHROPIC_API_KEY", "env-key");
        let key = super::super::auth::resolve_api_key("anthropic", None);
        assert_eq!(key, Some("env-key".to_string()));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn none_when_both_missing() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        let key = super::super::auth::resolve_api_key("anthropic", None);
        assert!(key.is_none());
    }
}
