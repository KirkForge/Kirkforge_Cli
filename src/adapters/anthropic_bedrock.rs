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

use crate::adapters::anthropic;
use crate::shared::{Message, ModelInfo, StreamEvent, ToolCallStyle};
use futures_util::StreamExt;

use super::ModelAdapter;

/// Bedrock inference path for Anthropic models.
///
/// `model_id` is the Bedrock model id, e.g. `anthropic.claude-3-5-sonnet-20240620-v1:0`.
/// The CLI `--model` flag holds this id; the adapter constructs the fully-qualified
/// regional endpoint from `Config::aws_region`.
pub struct AnthropicBedrockAdapter {
    model_id: String,
    region: String,
    profile: String,
    client: reqwest::Client,
    json_mode: bool,
    seed: Option<u64>,
    timeout_secs: u64,
}

impl AnthropicBedrockAdapter {
    pub fn new(model_id: &str, region: &str, profile: &str, timeout_secs: u64) -> Self {
        Self {
            model_id: model_id.to_string(),
            region: region.to_string(),
            profile: profile.to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            seed: None,
            timeout_secs,
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
        let lower = self.model_id.to_lowercase();
        let is_reasoning = lower.contains("claude-3-7-sonnet") || lower.contains("claude-4");
        ModelInfo {
            name: self.model_id.clone(),
            supports_thinking: is_reasoning,
            tool_call_format: ToolCallStyle::Anthropic,
            max_context_tokens: 200_000,
            recommended_temperature: 1.0,
            supports_images: lower.starts_with("anthropic.claude-3"),
            supports_cache: true,
        }
    }

    fn set_json_mode(&mut self, json_mode: bool) {
        self.json_mode = json_mode;
    }

    fn set_seed(&mut self, seed: Option<u64>) {
        self.seed = seed;
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
            self.json_mode,
            self.seed,
        );
        let body_bytes = serde_json::to_vec(&body)?;
        let url = self.endpoint();

        let signed_request =
            super::bedrock_signing::sign_request(&url, &body_bytes, &self.region, &self.profile)?;

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
        tokio::spawn(async move {
            let bytes_stream = response.bytes_stream();
            parse_bedrock_event_stream(tx, bytes_stream).await;
        });
        Ok(rx)
    }
}

/// Bedrock returns an AWS event-stream (`application/vnd.amazon.eventstream`).
/// Each event payload is a JSON object with the same shape as an Anthropic SSE
/// `data:` payload. We strip the event-stream envelope and feed the inner JSON
/// into the shared Anthropic parser.
async fn parse_bedrock_event_stream<B, E>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: impl tokio_stream::Stream<Item = Result<B, E>> + Unpin,
) where
    B: AsRef<[u8]> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let mut envelope_buffer: Vec<u8> = Vec::new();
    let (inner_tx, inner_rx) =
        tokio::sync::mpsc::channel::<Result<Vec<u8>, std::convert::Infallible>>(4096);

    let parser_handle = tokio::spawn(anthropic::parse_anthropic_stream(
        tx,
        tokio_stream::wrappers::ReceiverStream::new(inner_rx),
    ));

    while let Some(chunk_result) = stream.next().await {
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
    let _ = inner_tx.send(Ok(b"data: [DONE]\n\n".to_vec())).await;
    drop(inner_tx);
    let _ = parser_handle.await;
}

/// Ceiling on the outer Bedrock envelope buffer. Matches the inner
/// `parse_anthropic_stream`'s `MAX_SSE_BUFFER_BYTES` so a runaway stream
/// produces an error event instead of OOM.
const MAX_ENVELOPE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Best-effort extraction of the first JSON object in the AWS event-stream envelope.
/// Returns the raw JSON string (without `data:` prefix) and the byte offset in
/// `envelope` immediately after the parsed object, so the caller can drain the
/// consumed bytes and continue parsing the next frame in the same chunk.
fn extract_payload(envelope: &[u8]) -> Option<(String, usize)> {
    let text = String::from_utf8_lossy(envelope);
    for (start, ch) in text.char_indices() {
        if ch != '{' {
            continue;
        }
        let mut de = serde_json::Deserializer::from_str(&text[start..])
            .into_iter::<serde_json::Value>();
        if let Some(Ok(v)) = de.next() {
            if v.is_object() && v.get("type").is_some() {
                let end = start + de.byte_offset();
                return Some((text[start..end].to_string(), end));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_includes_region_and_model() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet-v1", "us-west-2", "", 30);
        assert_eq!(
            a.endpoint(),
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/anthropic.claude-3-5-sonnet-v1/invoke-with-response-stream"
        );
    }

    #[test]
    fn model_info_reports_image_support_for_claude3() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus-v1", "us-east-1", "", 30);
        assert!(a.model_info().supports_images);
    }

    #[test]
    fn model_info_reports_no_images_for_unknown() {
        let a = AnthropicBedrockAdapter::new("my-model", "us-east-1", "", 30);
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
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "eu-west-1", "", 30);
        assert!(a.endpoint().ends_with("/invoke-with-response-stream"));
    }

    #[test]
    fn endpoint_for_us_east_1_region() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet", "us-east-1", "", 30);
        assert!(a
            .endpoint()
            .contains("bedrock-runtime.us-east-1.amazonaws.com"));
    }

    #[test]
    fn model_info_reasoning_for_claude_3_7() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-7-sonnet", "us-east-1", "", 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_reasoning_for_claude_4() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-4-opus", "us-east-1", "", 30);
        assert!(a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_no_thinking_for_claude_3_5() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-5-sonnet", "us-east-1", "", 30);
        assert!(!a.model_info().supports_thinking);
    }

    #[test]
    fn model_info_anthropic_tool_call_format() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", "", 30);
        assert_eq!(a.model_info().tool_call_format, ToolCallStyle::Anthropic);
    }

    #[test]
    fn model_info_supports_cache() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", "", 30);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn model_info_max_context_tokens() {
        let a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", "", 30);
        assert_eq!(a.model_info().max_context_tokens, 200_000);
    }

    #[test]
    fn set_json_mode_toggles_flag() {
        let mut a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", "", 30);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
    }

    #[test]
    fn set_seed_sets_value() {
        let mut a = AnthropicBedrockAdapter::new("anthropic.claude-3-opus", "us-east-1", "", 30);
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
        parse_bedrock_event_stream(tx, stream).await;

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
        parse_bedrock_event_stream(tx, stream).await;

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
