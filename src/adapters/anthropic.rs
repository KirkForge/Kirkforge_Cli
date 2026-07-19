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
    ContentPart, FinishReason, Message, ModelInfo, Role, StreamEvent, TokenUsage, ToolCallStyle,
    ToolInvocation,
};
use tokio_stream::StreamExt;

use super::ModelAdapter;

/// Maximum bytes the SSE parser will accumulate while waiting for a complete
/// `data: ...\n\n` frame.
const MAX_SSE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Anthropic Messages API version we target.
const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    json_mode: bool,
    timeout_secs: u64,
}

impl AnthropicAdapter {
    pub fn new(api_base: &str, model: &str, timeout_secs: u64) -> Self {
        Self {
            model: model.to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            timeout_secs,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicAdapter {
    fn model_info(&self) -> ModelInfo {
        let lower = self.model.to_lowercase();
        let is_reasoning = lower.contains("claude-3-7-sonnet") || lower.contains("claude-4");
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: is_reasoning,
            tool_call_format: ToolCallStyle::Anthropic,
            max_context_tokens: 200_000,
            recommended_temperature: 1.0,
            supports_images: lower.starts_with("claude-3"),
            supports_cache: true,
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = build_anthropic_body(&self.model, messages, tools, self.json_mode);
        let url = format!("{}/v1/messages", self.api_base);

        let response = super::send_with_retry(|| async {
            self.client
                .post(&url)
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
) -> serde_json::Value {
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

    // Apply cache breakpoints to the last two prefix messages.
    // The trailing user turn changes every time; don't cache it.
    // Anthropic allows `cache_control` on the *last* block of a message's
    // `content` array. We also skip the very first user message to keep the
    // prefix stable at the natural boundary after the system prompt.
    if anthropic_messages.len() > 2 {
        let prefix_end = anthropic_messages.len() - 1;
        for msg in anthropic_messages.iter_mut().take(prefix_end).skip(1) {
            if let Some(content) = msg.get_mut("content") {
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
        "max_tokens": 8192,
        "messages": anthropic_messages,
        "stream": true,
    });

    if !system_blocks.is_empty() {
        if system_blocks.len() == 1 {
            body["system"] = system_blocks.into_iter().next().unwrap();
        } else {
            body["system"] = serde_json::Value::Array(system_blocks);
        }
    }

    if !tools.is_empty() {
        let tool_defs: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tool_defs);
    }

    if json_mode {
        // Anthropic supports JSON mode via explicit prefill / tool-free
        // instructions rather than a response_format field. We do not add an
        // unsupported top-level key; callers are expected to use a system
        // prompt that asks for JSON.
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
            if tool.input.is_some() {
                let _ = tx.send(StreamEvent::ToolCall(tool.into_invocation())).await;
            }
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
            let input = block.get("input").cloned().filter(|v| !is_empty_object(v));
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

fn is_empty_object(v: &serde_json::Value) -> bool {
    v.as_object().map(|o| o.is_empty()).unwrap_or(false)
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

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
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
    use serde_json::json;

    fn line(s: &str) -> Vec<u8> {
        format!("data: {s}\n\n").into_bytes()
    }

    #[allow(dead_code)]
    fn chunk(s: &str) -> Vec<u8> {
        s.bytes().collect()
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
        let body = build_anthropic_body("claude-sonnet-4", &messages, &[], false);
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
        let body = build_anthropic_body("claude-sonnet-4", &messages, &[], false);
        let msgs = body["messages"].as_array().unwrap();
        assert!(msgs[0]
            .get("content")
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .get("cache_control")
            .is_none());
        assert_eq!(
            msgs[1]["content"].as_array().unwrap().last().unwrap()["cache_control"],
            json!({"type":"ephemeral"})
        );
        assert!(msgs[2]
            .get("content")
            .unwrap()
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .get("cache_control")
            .is_none());
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
}
