//! Anthropic Messages API through Amazon Bedrock.
//!
//! Bedrock exposes Anthropic models via an AWS-signed InvokeModel/InvokeModelWithResponseStream
//! request. The request/response body is identical to Anthropic's Messages API, but the HTTP
//! request must be signed with SigV4. We reuse `anthropic::build_anthropic_body` and
//! `anthropic::parse_anthropic_stream` for the wire format.
//!
//! Reference:
//! - https://docs.aws.amazon.com/bedrock/latest/userguide/inference-invoke.html
//! - https://docs.anthropic.com/en/api/claude-on-amazon-bedrock

use super::next_chunk_or_idle_timeout;
use crate::adapters::anthropic;
use crate::shared::{Message, ModelInfo, StreamEvent};

use super::ModelAdapter;

/// Bedrock inference path for Anthropic models.
///
/// `model_id` is the Bedrock model id, e.g. `anthropic.claude-3-5-sonnet-20240620-v1:0`.
/// The CLI `--model` flag holds this id; the adapter constructs the fully-qualified
/// regional endpoint from `Config::aws_region`.
pub struct AnthropicBedrockAdapter {
    model_id: String,
    region: String,
    client: reqwest::Client,
    json_mode: bool,
    response_format: Option<crate::shared::ResponseFormat>,
    seed: Option<u64>,
    timeout_secs: u64,
    extended_thinking: bool,
    budget_tokens: usize,
    stream_idle_timeout: std::time::Duration,
}

impl AnthropicBedrockAdapter {
    pub fn new(model_id: &str, region: &str, timeout_secs: u64) -> Self {
        Self {
            model_id: model_id.to_string(),
            region: region.to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            extended_thinking: true,
            budget_tokens: 10_000,
            stream_idle_timeout: super::STREAM_IDLE_TIMEOUT,
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/invoke-with-response-stream",
            self.region, self.model_id
        )
    }
}

#[async_trait::async_trait]
impl ModelAdapter for AnthropicBedrockAdapter {
    fn model_info(&self) -> ModelInfo {
        super::anthropic_model_info(&self.model_id, "anthropic.claude-3")
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
        if json_mode {
            self.response_format = Some(crate::shared::ResponseFormat::JsonObject);
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

    fn set_streaming_timeout(&mut self, secs: u64) {
        self.stream_idle_timeout = std::time::Duration::from_secs(secs);
    }
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = anthropic::build_anthropic_body(
            &self.model_id,
            messages,
            tools,
            self.response_format.as_ref(),
            self.seed,
            self.extended_thinking,
            self.budget_tokens,
            8192,
            None,
        );
        let body_bytes = serde_json::to_vec(&body)?;
        let url = self.endpoint();

        let signed_request = super::bedrock_signing::sign_request(&url, &body_bytes, &self.region)?;

        let response = super::send_with_retry(|| async {
            self.client
                .request(signed_request.method.clone(), &signed_request.url)
                .headers(signed_request.headers.clone())
                .body(body_bytes.clone())
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
        })
        .await?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        let idle_timeout = self.stream_idle_timeout;
        tokio::spawn(async move {
            let bytes_stream = response.bytes_stream();
            parse_bedrock_event_stream(tx, bytes_stream, idle_timeout).await;
        });
        Ok(rx)
    }
}

/// Bedrock returns an AWS event-stream (`application/vnd.amazon.eventstream`).
/// Each event payload is a JSON object with the same shape as an Anthropic SSE
/// `data:` payload. We strip the event-stream envelope and feed the inner JSON
/// into the shared Anthropic parser.
pub(super) async fn parse_bedrock_event_stream<B, E>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: impl tokio_stream::Stream<Item = Result<B, E>> + Unpin,
    idle_timeout: std::time::Duration,
) where
    B: AsRef<[u8]> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mut envelope_buffer: Vec<u8> = Vec::new();
    // [DONE] is only forwarded when the model actually finished
    // (WO 43.22). Injecting it on every stream end laundered mid-turn
    // transport drops into Done{Stop}; without a terminal frame the
    // parser's EOF path emits Done{Error} like every other adapter.
    let mut saw_message_stop = false;
    let (inner_tx, inner_rx) =
        tokio::sync::mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(4096);

    // Clone tx for the idle-timeout helper; the original is moved into the
    // spawned Anthropic parser below. On timeout the error goes straight to
    // the consumer (bypassing the parser) since this is a transport-level
    // stall, not an envelope-parse issue.
    let tx_for_timeout = tx.clone();

    let parser_handle = tokio::spawn(anthropic::parse_anthropic_stream(
        tx,
        tokio_stream::wrappers::ReceiverStream::new(inner_rx),
        idle_timeout,
    ));

    while let Some(chunk_result) =
        next_chunk_or_idle_timeout(&mut stream, &tx_for_timeout, idle_timeout).await
    {
        match chunk_result {
            Ok(chunk) => {
                envelope_buffer.extend_from_slice(chunk.as_ref());
                if envelope_buffer.len() > MAX_ENVELOPE_BUFFER_BYTES {
                    let _ = inner_tx
                        .send(Ok(format!(
                            "data: {{\"type\":\"error\",\"error\":{{\"message\":\"Bedrock envelope buffer exceeded {} MiB limit; aborting stream\"}}}}\n\n",
                            MAX_ENVELOPE_BUFFER_BYTES / (1024 * 1024)
                        ).into_bytes()))
                        .await;
                    envelope_buffer.clear();
                    continue;
                }
                // Drain every complete frame in the buffer, not just the first.
                // A single chunk may carry multiple event-stream frames; the
                // previous `if let` + `clear()` discarded all but the first.
                while let Some((inner, end)) = extract_payload(&envelope_buffer) {
                    if is_message_stop(&inner) {
                        saw_message_stop = true;
                    }
                    let _ = inner_tx
                        .send(Ok(format!("data: {inner}\n\n").into_bytes()))
                        .await;
                    // Drop the consumed frame (and any prelude/CRC bytes before
                    // it); keep the residual tail for the next chunk.
                    if end >= envelope_buffer.len() {
                        envelope_buffer.clear();
                        break;
                    }
                    envelope_buffer.drain(..end);
                }
            }
            Err(e) => {
                let payload =
                    format!("data: {{\"type\":\"error\",\"error\":{{\"message\":\"{e}\"}}}}\n\n");
                let _ = inner_tx.send(Ok(payload.into_bytes())).await;
            }
        }
    }
    if saw_message_stop {
        let _ = inner_tx.send(Ok(b"data: [DONE]\n\n".to_vec())).await;
    }
    drop(inner_tx);
    let _ = parser_handle.await;
}

/// True when a payload is the terminal `message_stop` event. The cheap
/// substring pre-check avoids re-parsing large delta frames; the JSON
/// verify stops prose containing the literal `"message_stop"` from
/// counting as a terminal frame.
fn is_message_stop(payload: &str) -> bool {
    if !payload.contains("\"message_stop\"") {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(v) => v.get("type").and_then(|t| t.as_str()) == Some("message_stop"),
        Err(_) => false,
    }
}

/// Ceiling on the outer Bedrock envelope buffer. Matches the inner
/// `parse_anthropic_stream`'s `MAX_SSE_BUFFER_BYTES` so a runaway stream
/// produces an error event instead of OOM.
const MAX_ENVELOPE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Best-effort extraction of the first JSON object in the AWS event-stream envelope.
/// Returns the raw JSON string (without `data:` prefix) and the byte offset in
/// `envelope` immediately after the parsed object, so the caller can drain the
/// consumed bytes and continue parsing the next frame in the same chunk.
///
/// WO 38.5: operates on RAW BYTES. The previous implementation computed
/// offsets on `from_utf8_lossy` output but the caller drained the raw
/// buffer — any non-UTF8 prelude (binary event-stream headers, CRCs)
/// made the replacement characters shift the offsets and corrupt every
/// subsequent frame boundary. JSON payloads are UTF-8 by definition, so
/// only the extracted slice is lossily decoded; offsets stay in byte
/// space.
fn extract_payload(envelope: &[u8]) -> Option<(String, usize)> {
    let mut search_from = 0usize;
    while let Some(rel) = envelope[search_from..].iter().position(|&b| b == b'{') {
        let start = search_from + rel;
        let mut de = serde_json::Deserializer::from_slice(&envelope[start..])
            .into_iter::<serde_json::Value>();
        if let Some(Ok(v)) = de.next() {
            if v.is_object() && v.get("type").is_some() {
                let end = start + de.byte_offset();
                let payload = String::from_utf8_lossy(&envelope[start..end]).into_owned();
                return Some((payload, end));
            }
        }
        search_from = start + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ToolCallStyle;
    use serde_json::json;

    #[test]
    fn endpoint_includes_region_and_model() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet-v1", "us-west-2", 30);
        assert_eq!(
            a.endpoint(),
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/anthropic.claude-3-5-sonnet-v1/invoke-with-response-stream"
        );
    }

    #[test]
    fn model_info_reports_image_support_for_claude3() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus-v1", "us-east-1", 30);
        assert!(a.model_info().supports_images);
    }

    #[test]
    fn model_info_reports_no_images_for_unknown() {
        let a = AnthropicBedrockAdapter::new("my-model", "us-east-1", 30);
        assert!(!a.model_info().supports_images);
    }

    #[test]
    fn extract_payload_pulls_first_json_object() {
        let env = b"prelude{\"type\":\"message_start\",\"message\":{}}crc";
        let (out, end) = extract_payload(env).expect("payload present");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap(),
            json!({"type":"message_start","message":{}})
        );
        // end points just past the closing brace of the parsed object.
        assert_eq!(end, env.len() - b"crc".len());
    }

    #[test]
    fn endpoint_includes_invoke_with_response_stream_path() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "eu-west-1", 30);
        assert!(a.endpoint().ends_with("/invoke-with-response-stream"));
    }

    #[test]
    fn endpoint_for_us_east_1_region() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet", "us-east-1", 30);
        assert!(a
            .endpoint()
            .contains("bedrock-runtime.us-east-1.amazonaws.com"));
    }

    #[test]
    fn model_info_reasoning_for_claude_3_7() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-7-sonnet", "us-east-1", 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_reasoning_for_claude_4() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-4-opus", "us-east-1", 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_3_5() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet", "us-east-1", 30);
        assert!(!a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_anthropic_tool_call_format() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", 30);
        assert_eq!(a.model_info().tool_call_format, ToolCallStyle::Anthropic);
    }

    #[test]
    fn model_info_supports_cache() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", 30);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn model_info_max_context_tokens() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", 30);
        assert_eq!(a.model_info().max_context_tokens, 200_000);
    }

    #[test]
    fn set_json_mode_toggles_flag() {
        let mut a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", 30);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
    }

    #[test]
    fn set_seed_sets_value() {
        let mut a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", 30);
        assert!(a.seed.is_none());
        a.set_seed(Some(99));
        assert_eq!(a.seed, Some(99));
    }

    #[test]
    fn extract_payload_returns_none_for_no_type_key() {
        let env = b"prelude{\"foo\":\"bar\"}crc";
        assert!(extract_payload(env).is_none());
    }

    #[test]
    fn extract_payload_returns_none_for_unclosed_object() {
        let env = b"prelude{\"type\":\"message_start\",\"message\":";
        assert!(extract_payload(env).is_none());
    }

    #[test]
    fn extract_payload_handles_nested_objects() {
        let env = b"x{\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\"}}y";
        let (out, end) = extract_payload(env).expect("payload present");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["type"], "content_block_start");
        assert_eq!(v["content_block"]["type"], "tool_use");
        // end is the byte index immediately after the outer closing brace.
        assert_eq!(end, env.len() - b"y".len());
    }

    #[test]
    fn extract_payload_returns_none_for_empty_input() {
        assert!(extract_payload(b"").is_none());
    }

    #[test]
    fn extract_payload_returns_none_for_plain_text() {
        assert!(extract_payload(b"just some text").is_none());
    }

    #[test]
    fn is_message_stop_matches_terminal_frame() {
        assert!(is_message_stop(r#"{"type":"message_stop"}"#));
    }

    #[test]
    fn is_message_stop_ignores_literal_in_nested_field() {
        // A delta whose payload mentions "message_stop" as a JSON value
        // of another key must not count as the terminal frame.
        assert!(!is_message_stop(
            r#"{"type":"content_block_delta","delta":{"type":"message_stop"}}"#
        ));
        assert!(!is_message_stop(
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"no stop here"}}"#
        ));
    }

    /// WO 43.22: a mid-stream transport drop must surface as Done{Error},
    /// not be laundered into Done{Stop} by an injected [DONE]. The old
    /// wrapper forwarded [DONE] on every stream end.
    #[tokio::test]
    async fn parse_bedrock_event_stream_transport_drop_yields_done_error() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        let frame =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#;
        let stream = tokio_stream::iter(vec![
            Ok::<Vec<u8>, String>(frame.to_vec()),
            Err("connection reset by peer".to_string()),
        ]);

        parse_bedrock_event_stream(tx, stream, super::super::STREAM_IDLE_TIMEOUT).await;

        let mut finish_reason = None;
        let mut saw_error = false;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
        {
            match ev {
                StreamEvent::Done { finish_reason: fr, .. } => {
                    finish_reason = Some(fr);
                    break;
                }
                StreamEvent::Error(_) => saw_error = true,
                _ => {}
            }
        }
        assert!(saw_error, "transport error frame must be forwarded");
        assert_eq!(
            finish_reason,
            Some(crate::shared::FinishReason::Error),
            "mid-stream drop must yield Done{{Error}}, got {finish_reason:?}"
        );
    }

    /// WO 43.22: a complete stream (terminal message_stop seen) still gets
    /// [DONE] forwarded → Done{Stop} with the accumulated text.
    #[tokio::test]
    async fn parse_bedrock_event_stream_complete_stream_yields_done_stop() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        let frames = [
            r#"{"type":"message_start","message":{}}"#.as_bytes(),
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#,
            br#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            br#"{"type":"message_stop"}"#,
        ];
        let chunk: Vec<u8> = frames.concat();
        let stream = tokio_stream::iter(vec![Ok::<Vec<u8>, std::convert::Infallible>(chunk)]);

        parse_bedrock_event_stream(tx, stream, super::super::STREAM_IDLE_TIMEOUT).await;

        let mut texts = Vec::new();
        let mut finish_reason = None;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
        {
            match ev {
                StreamEvent::Done { finish_reason: fr, .. } => {
                    finish_reason = Some(fr);
                    break;
                }
                StreamEvent::Error(e) => panic!("stream error: {e}"),
                StreamEvent::Text(t) => texts.push(t),
                _ => {}
            }
        }
        assert_eq!(texts, vec!["hi".to_string()]);
        assert_eq!(
            finish_reason,
            Some(crate::shared::FinishReason::Stop),
            "complete stream must yield Done{{Stop}}, got {finish_reason:?}"
        );
    }

    #[test]
    fn extract_payload_reports_end_offset_for_drain() {
        // prelude bytes before the object must be included in the end offset
        // so the caller can drain them along with the parsed frame.
        let env = b"hdr{\"type\":\"message_start\"}tail";
        let (_out, end) = extract_payload(env).expect("payload present");
        assert_eq!(end, env.len() - b"tail".len());
    }

    #[test]
    fn extract_payload_tolerates_whitespace_and_key_reordering() {
        let env = b"prelude{  \"message\" : {} , \"type\":\"message_start\" }tail";
        let (out, end) = extract_payload(env).expect("payload present");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["type"], "message_start");
        assert_eq!(end, env.len() - b"tail".len());
    }

    /// WO 38.5: non-UTF8 prelude bytes (binary event-stream headers /
    /// CRCs) must not corrupt the frame boundary. The old lossy-string
    /// implementation misaligned offsets because each invalid byte
    /// became a 3-byte replacement character in the string it measured
    /// but not in the raw buffer it drained.
    #[test]
    fn extract_payload_offsets_stay_in_byte_space_with_non_utf8_prelude() {
        let env = b"\xff\xfe\x00{\"type\":\"message_start\",\"message\":{}}\x01\x02crc";
        let (out, end) = extract_payload(env).expect("payload present");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["type"], "message_start");
        // end must point exactly past the `}` — in RAW byte space, so the
        // caller's drain leaves only the trailing 5 bytes.
        assert_eq!(end, env.len() - b"\x01\x02crc".len());
    }

    /// WO 38.5: consecutive frames split by non-UTF8 separators both
    /// extract with correct byte offsets (regression for the drain loop).
    #[test]
    fn extract_payload_handles_two_frames_with_binary_separator() {
        let frame1 = b"{\"type\":\"content_block_delta\",\"delta\":{\"text\":\"A\"}}";
        let frame2 = b"{\"type\":\"content_block_delta\",\"delta\":{\"text\":\"B\"}}";
        let mut env = Vec::new();
        env.extend_from_slice(b"\xff");
        env.extend_from_slice(frame1);
        env.extend_from_slice(b"\xff\xff");
        env.extend_from_slice(frame2);
        let (out1, end1) = extract_payload(&env).expect("first frame");
        let v1: serde_json::Value = serde_json::from_str(&out1).unwrap();
        assert_eq!(v1["delta"]["text"], "A");
        assert_eq!(&env[end1..end1 + 2], b"\xff\xff");
        let (out2, end2) = extract_payload(&env[end1..]).expect("second frame");
        let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
        assert_eq!(v2["delta"]["text"], "B");
        // end2 is the offset within env[end1..], which starts at the
        // \xff\xff separator — so it covers the 2 separator bytes + frame2.
        assert_eq!(end2, 2 + frame2.len());
    }

    // WO 15.6 / 2.1: a chunk carrying multiple event-stream frames must not
    // drop the second frame. The previous `if let` + `clear()` parsed only the
    // first and discarded the rest.
    #[tokio::test]
    async fn parse_bedrock_event_stream_drains_all_frames_in_one_chunk() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        // Two content_block_delta/text_delta frames in a single chunk. Each
        // produces a StreamEvent::Text, so two forwarded frames yield two Text
        // events. The previous `if let` + `clear()` dropped the second frame.
        let frame1 =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"A"}}"#;
        let frame2 =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"B"}}"#;
        let chunk: Vec<u8> = [frame1.as_slice(), frame2.as_slice()].concat();

        let stream = tokio_stream::iter(vec![Ok::<Vec<u8>, std::convert::Infallible>(chunk)]);
        parse_bedrock_event_stream(tx, stream, super::super::STREAM_IDLE_TIMEOUT).await;

        // Collect Text events; both deltas must be forwarded, not just the first.
        let mut texts: Vec<String> = Vec::new();
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
        {
            match ev {
                StreamEvent::Done { .. } => break,
                StreamEvent::Error(e) => panic!("stream error: {e}"),
                StreamEvent::Text(t) => texts.push(t),
                _ => {}
            }
        }
        assert_eq!(
            texts,
            vec!["A".to_string(), "B".to_string()],
            "both frames in one chunk must be forwarded (got {texts:?})"
        );
    }

    // WO 15.6 / 2.1: an envelope buffer that grows past the cap must emit an
    // error event and clear, not OOM. Feed >8 MiB of non-matching bytes.
    #[tokio::test]
    async fn parse_bedrock_event_stream_caps_envelope_buffer_at_8mib() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);
        // A single chunk larger than the cap with no `{"type"` frame, so
        // extract_payload returns None and the buffer would grow unbounded
        // under the old code.
        let big: Vec<u8> = vec![b'x'; MAX_ENVELOPE_BUFFER_BYTES + 1];
        let stream = tokio_stream::iter(vec![Ok::<Vec<u8>, std::convert::Infallible>(big)]);

        // Must not hang or panic.
        parse_bedrock_event_stream(tx, stream, super::super::STREAM_IDLE_TIMEOUT).await;

        let mut saw_error = false;
        while let Ok(Some(ev)) =
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await
        {
            if let StreamEvent::Error(msg) = ev {
                assert!(
                    msg.contains("envelope buffer exceeded"),
                    "unexpected error message: {msg}"
                );
                saw_error = true;
            }
        }
        assert!(
            saw_error,
            "expected an envelope-buffer-overflow error event"
        );
    }
}
