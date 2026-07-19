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

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = anthropic::build_anthropic_body(&self.model_id, messages, tools, self.json_mode);
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
                if let Some(inner) = extract_payload(&envelope_buffer) {
                    let _ = inner_tx
                        .send(Ok(format!("data: {inner}\n\n").into_bytes()))
                        .await;
                    envelope_buffer.clear();
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

/// Best-effort extraction of the first JSON object in the AWS event-stream envelope.
/// Returns the raw JSON string without `data:` prefix.
fn extract_payload(envelope: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(envelope);
    let start = text.find("{\"type\"")?;
    // Find the matching closing brace by scanning from the start of the
    // candidate object and tracking brace depth. This handles nested objects
    // that the previous naive `find("}")` missed.
    let mut depth = 0i32;
    let mut end = 0;
    for (i, c) in text[start..].char_indices() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                end = start + i + 1;
                break;
            }
        }
    }
    if end == 0 {
        return None;
    }
    let candidate = &text[start..end];
    if serde_json::from_str::<serde_json::Value>(candidate).is_ok() {
        Some(candidate.to_string())
    } else {
        None
    }
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
        let out = extract_payload(env).expect("payload present");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out).unwrap(),
            json!({"type":"message_start","message":{}})
        );
    }
}
