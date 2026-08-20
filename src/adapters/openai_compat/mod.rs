//! OpenAI-compatible fallback adapter.
//!
//! Uses `/v1/chat/completions` (SSE streaming) instead of `/api/chat` (NDJSON).
//! Activated for any model that doesn't match GLM/DeepSeek/Gemini patterns,
//! or explicitly via `--model-type openai`.
//!
//! Parses SSE `data: {...}` lines. Supports tool calls in the
//! OpenAI function-calling format.

use crate::shared::{FinishReason, Message, ModelInfo, StreamEvent, TokenUsage, ToolCallStyle};

use super::{
    find_subseq, next_chunk_or_idle_timeout, trim_ascii_whitespace, ModelAdapter,
    MAX_SSE_BUFFER_BYTES,
};

mod tool_call;
use tool_call::ToolCallAccumulator;

/// Send a `Done` event only if one has not already been emitted.
///
/// OpenAI-compat streams sometimes carry both a `[DONE]` sentinel and a
/// later `finish_reason`. Sending two `Done` events can cause the
/// executor to drop the receiver and produce a spurious warning on the
/// second send. This helper suppresses duplicate `Done` events.
async fn send_done_once(
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    done_emitted: &mut bool,
    ev: StreamEvent,
    kind: &'static str,
) -> bool {
    if *done_emitted {
        return true;
    }
    if super::ollama_ndjson::send_or_bail(tx, ev, kind).await {
        *done_emitted = true;
        true
    } else {
        false
    }
}

/// Drive an OpenAI-compatible `/v1/chat/completions` SSE byte stream into
/// `StreamEvent` events.
///
/// This is the testable counterpart to the HTTP setup in
/// [`OpenAiCompatAdapter::stream`]. It handles the same SSE framing,
/// incremental tool-call accumulation, concatenated argument objects,
/// duplicate id de-duplication, and `[DONE]` suppression as the public
/// adapter.
/// Find the first occurrence of `needle` in `haystack`.
pub(crate) async fn parse_openai_compat_stream<B, E, S>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: S,
    idle_timeout: std::time::Duration,
) where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
    S: tokio_stream::Stream<Item = Result<B, E>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut pending_tool_calls = ToolCallAccumulator::new();
    let mut done_emitted = false;

    while let Some(chunk_result) = next_chunk_or_idle_timeout(&mut stream, &tx, idle_timeout).await
    {
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

                // SSE: data: {...}\n\n
                //
                // We accumulate raw bytes until we see a complete
                // `data: ...\n\n` frame. Only the frame payload is
                // decoded as UTF-8, so a chunk boundary that falls
                // inside a multibyte character cannot produce
                // replacement characters and corrupt the JSON.
                // This mirrors the NDJSON parser's byte-buffer
                // approach in `ollama_ndjson.rs`.
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
                        // Incomplete frame. Bail out of the
                        // inner loop; the outer stream
                        // loop will read more.
                        break;
                    };
                    let payload_end = after_start + sep_idx;
                    let drain_to = payload_end + term_len;
                    let payload = trim_ascii_whitespace(&buffer[after_start..payload_end]).to_vec();

                    buffer.drain(..drain_to);

                    if payload.is_empty() || payload == b"[DONE]" {
                        if payload == b"[DONE]" {
                            // Some proxies send [DONE] after the
                            // model has emitted tool_calls deltas
                            // but before a finish_reason. Flush any
                            // accumulated calls before closing the
                            // stream so the executor sees them.
                            for tc in pending_tool_calls.drain() {
                                if !super::ollama_ndjson::send_or_bail(
                                    &tx,
                                    StreamEvent::ToolCall(tc),
                                    "SSE [DONE] buffered tool call",
                                )
                                .await
                                {
                                    return;
                                }
                            }
                            if !send_done_once(
                                &tx,
                                &mut done_emitted,
                                StreamEvent::Done {
                                    finish_reason: FinishReason::Stop,
                                    usage: None,
                                },
                                "SSE [DONE] sentinel",
                            )
                            .await
                            {
                                return;
                            }
                        }
                        continue;
                    }

                    let line = match std::str::from_utf8(&payload) {
                        Ok(s) => s,
                        Err(e) => {
                            if !super::ollama_ndjson::send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("SSE frame is not valid UTF-8: {e}")),
                                "OpenAI-compat UTF-8 decode error",
                            )
                            .await
                            {
                                return;
                            }
                            continue;
                        }
                    };

                    match serde_json::from_str::<serde_json::Value>(line) {
                        Ok(json) => {
                            if let Some(err) = json.get("error") {
                                if !super::ollama_ndjson::send_or_bail(
                                    &tx,
                                    StreamEvent::Error(
                                        err.get("message")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("API error")
                                            .to_string(),
                                    ),
                                    "OpenAI-compat API error",
                                )
                                .await
                                {
                                    return;
                                }
                                continue;
                            }

                            let choice = json
                                .get("choices")
                                .and_then(|c| c.as_array())
                                .and_then(|c| c.first());

                            let delta = choice.and_then(|c| c.get("delta"));
                            let finish = choice.and_then(|c| c.get("finish_reason"));

                            // Text content
                            if let Some(content) = delta.and_then(|d| d.get("content")) {
                                if let Some(c) = content.as_str() {
                                    if !c.is_empty()
                                        && !super::ollama_ndjson::send_or_bail(
                                            &tx,
                                            StreamEvent::Text(c.to_string()),
                                            "OpenAI-compat text chunk",
                                        )
                                        .await
                                    {
                                        return;
                                    }
                                }
                            }

                            // Tool calls in delta — accumulate across chunks
                            if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")) {
                                if let Some(calls) = tcs.as_array() {
                                    for tc in calls {
                                        let index =
                                            tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                                as usize;
                                        let id =
                                            tc.get("id").and_then(|id| id.as_str()).unwrap_or("");
                                        let name = tc
                                            .get("function")
                                            .and_then(|f| f.get("name"))
                                            .and_then(|n| n.as_str());
                                        let args = tc
                                            .get("function")
                                            .and_then(|f| f.get("arguments"))
                                            .and_then(|a| a.as_str());
                                        pending_tool_calls.accumulate(index, id, name, args);
                                    }
                                }
                            }

                            // Finish reason signals end
                            if let Some(reason) = finish.and_then(|r| r.as_str()) {
                                if reason == "tool_calls" && pending_tool_calls.is_empty()
                                    && !super::ollama_ndjson::send_or_bail(
                                        &tx,
                                        StreamEvent::Error(
                                            "Model emitted tool_calls finish_reason but no parseable tool calls".to_string()
                                        ),
                                        "OpenAI-compat tool-call finish with no parseable calls",
                                    )
                                    .await
                                {
                                    return;
                                }
                                for tc in pending_tool_calls.drain() {
                                    if !super::ollama_ndjson::send_or_bail(
                                        &tx,
                                        StreamEvent::ToolCall(tc),
                                        "OpenAI-compat accumulated tool call",
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }

                                let finish_reason = match reason {
                                    "length" => FinishReason::Length,
                                    "tool_calls" => FinishReason::ToolCalls,
                                    "error" => FinishReason::Error,
                                    _ => FinishReason::Stop,
                                };

                                let usage = json.get("usage").map(|u| TokenUsage {
                                    prompt_tokens: u
                                        .get("prompt_tokens")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| {
                                            u.get("prompt_eval_count").and_then(|v| v.as_u64())
                                        })
                                        .map(|v| v as usize),
                                    completion_tokens: u
                                        .get("completion_tokens")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| u.get("eval_count").and_then(|v| v.as_u64()))
                                        .map(|v| v as usize),
                                    cached_tokens: u
                                        .get("cache_read_input_tokens")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| {
                                            u.get("prompt_tokens_details")
                                                .and_then(|d| d.get("cached_tokens"))
                                                .and_then(|v| v.as_u64())
                                        })
                                        .map(|v| v as usize),
                                    // Anthropic-native field surfaced by some
                                    // OpenAI-compat proxies (WO 38.5).
                                    cache_write_tokens: u
                                        .get("cache_creation_input_tokens")
                                        .and_then(|v| v.as_u64())
                                        .map(|v| v as usize),
                                });

                                if !send_done_once(
                                    &tx,
                                    &mut done_emitted,
                                    StreamEvent::Done {
                                        finish_reason,
                                        usage,
                                    },
                                    "OpenAI-compat done",
                                )
                                .await
                                {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            // A complete SSE frame with invalid JSON is a
                            // server-side error, not a transient streaming
                            // artefact. Report it and discard the frame so
                            // the parser cannot buffer the same invalid bytes
                            // forever (review.md C5).
                            if !super::ollama_ndjson::send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("SSE frame contains invalid JSON: {e}")),
                                "OpenAI-compat invalid JSON frame",
                            )
                            .await
                            {
                                return;
                            }
                            continue;
                        }
                    }
                }
            }
            Err(e) => {
                // Same shape as the Ollama adapter's
                // transport-error branch: log if the
                // consumer is also gone, then break.
                if !super::ollama_ndjson::send_or_bail(
                    &tx,
                    StreamEvent::Error(e.to_string()),
                    "OpenAI-compat transport error",
                )
                .await
                {
                    return;
                }
                break;
            }
        }
    }
}

pub struct OpenAiCompatAdapter {
    model: String,
    api_base: String,
    api_key: String,
    client: reqwest::Client,
    json_mode: bool,
    response_format: Option<crate::shared::ResponseFormat>,
    seed: Option<u64>,
    timeout_secs: u64,
    tool_choice: Option<crate::shared::ToolChoice>,
    stream_idle_timeout: std::time::Duration,
}

impl OpenAiCompatAdapter {
    pub fn new(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        let api_base = ollama_host.trim_end_matches('/').to_string();
        Self {
            model: model.to_string(),
            api_base,
            api_key: String::new(),
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            tool_choice: None,
            stream_idle_timeout: super::STREAM_IDLE_TIMEOUT,
        }
    }

    /// Create an adapter with an explicit base URL and API key.
    /// Used for OpenAI-compatible providers like OpenCode Zen that
    /// require an Authorization header.
    pub fn with_base_url_and_key(
        base_url: &str,
        model: &str,
        api_key: &str,
        timeout_secs: u64,
    ) -> Self {
        Self {
            model: model.to_string(),
            api_base: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: super::build_reqwest_client(),
            json_mode: false,
            response_format: None,
            seed: None,
            timeout_secs,
            tool_choice: None,
            stream_idle_timeout: super::STREAM_IDLE_TIMEOUT,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for OpenAiCompatAdapter {
    fn model_info(&self) -> ModelInfo {
        // ceiling: vision/cache capability is detected by model-name
        // prefix below. A model not on the allow-list (e.g. gpt-4.5) is
        // reported supports_images=false even when the upstream server
        // accepts images. upgrade path: config-driven capability map per
        // model (WO 15.26 3.21 deferred).
        let lower = self.model.to_lowercase();
        let is_claude3 = lower.starts_with("claude-3")
            || lower.starts_with("claude-3.5")
            || lower.starts_with("claude-3-5");
        let is_gpt4o = lower.starts_with("gpt-4o");
        let is_gpt5 = lower.starts_with("gpt-5");
        let is_gemini = lower.starts_with("gemini");
        let is_llava = lower.starts_with("llava");

        ModelInfo {
            name: self.model.clone(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 32_768, // conservative default
            recommended_temperature: 0.7,
            // Enable image support for the families we know accept
            // vision inputs through an OpenAI-compatible endpoint.
            // Models not on this list will still get a clean "tool not
            // available" error from the server if they don't support
            // images, but this stops us from refusing to send images
            // to e.g. `gpt-4o` or `claude-3-5-sonnet` proxies.
            supports_images: is_claude3 || is_gpt4o || is_gpt5 || is_gemini || is_llava,
            // Most OpenAI-compat servers ignore cache_control, and the
            // field is unknown to Ollama's /v1/chat/completions
            // endpoint. Set `true` only for the explicitly cache-aware
            // Anthropic/OpenAI families when we know the server honours
            // the marker. Include claude-3.5 / claude-3-5 style names.
            supports_cache: is_claude3 || is_gpt4o || is_gpt5,
        }
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

    fn set_streaming_timeout(&mut self, secs: u64) {
        self.stream_idle_timeout = std::time::Duration::from_secs(secs);
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[crate::shared::ToolDef],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
        let body = super::build_openai_compat_body(
            &self.model,
            &self.model_info(),
            messages,
            tools,
            self.response_format.as_ref(),
            self.seed,
            self.tool_choice.as_ref(),
        );
        let url = format!("{}/v1/chat/completions", self.api_base);

        let response = super::send_with_retry(|| async {
            let req = self
                .client
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs));
            let req = if self.api_key.is_empty() {
                req
            } else {
                req.header("Authorization", format!("Bearer {}", self.api_key))
            };
            req.send().await
        })
        .await?;

        // Channel size: 4096 events. The previous value of 128 was
        // the proximate cause of the "stream consumer dropped
        // receiver mid-stream; aborting adapter parser" warnings
        // seen on every turn in the 2026-06-11 incident: when the
        // channel fills, `tx.send().await` blocks the parser; the
        // executor meanwhile sees a `Done` or `ToolCall` event and
        // returns from its iteration loop, dropping `rx`; the
        // parser's next `tx.send` returns `Err`, `send_or_bail`
        // logs the warning, the parser bails, the assistant
        // message is never persisted, and the cost is never
        // recorded. 4096 gives ~20x headroom.
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(4096);

        tokio::spawn(parse_openai_compat_stream(
            tx,
            response.bytes_stream(),
            self.stream_idle_timeout,
        ));

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::tool_call::split_concatenated_json;
    use super::*;
    use serde_json::json;

    #[test]
    fn split_single_object() {
        let s = r#"{"path":"AGENTS.md","limit":1}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out, vec![json!({"path":"AGENTS.md","limit":1})]);
    }

    #[test]
    fn split_concatenated_objects() {
        // The exact shape minimax-m3:cloud produces for parallel
        // tool calls: a single string with no separator, multiple
        // top-level objects, sometimes surrounded by an outer
        // JSON-string layer (the `to_string()` from build_openai_compat_body
        // turns a Value::String into a quoted string, so the
        // accumulator can receive the leading/trailing quotes
        // already stripped to the inner contents).
        let s = r#"{"path":"AGENTS.md"}{"path":"CHANGELOG.md"}{"path":"README.md"}{"path":"ARCHITECTURE.md"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 4, "expected 4 objects, got: {out:?}");
        assert_eq!(out[0], json!({"path":"AGENTS.md"}));
        assert_eq!(out[1], json!({"path":"CHANGELOG.md"}));
        assert_eq!(out[2], json!({"path":"README.md"}));
        assert_eq!(out[3], json!({"path":"ARCHITECTURE.md"}));
    }

    #[test]
    fn split_handles_embedded_braces_in_strings() {
        // A value like `{"path":"weird{path}"}` should NOT be
        // split at the inner braces.
        let s = r#"{"path":"weird{path}"}{"path":"ok"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 2, "expected 2 objects, got: {out:?}");
        assert_eq!(out[0], json!({"path":"weird{path}"}));
        assert_eq!(out[1], json!({"path":"ok"}));
    }

    #[test]
    fn split_handles_escaped_quotes() {
        let s = r#"{"path":"a\"b"}{"path":"c"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], json!({"path":"a\"b"}));
    }

    #[test]
    fn split_falls_back_on_garbage() {
        // Unparseable, not concatenable — return as Value::String
        // so the executor's existing fallback path takes over.
        let s = "not json at all";
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], json!("not json at all"));
    }

    #[test]
    fn split_empty_string() {
        let out = split_concatenated_json("");
        assert!(out.is_empty());
    }

    /// Regression: model emitted multiple parallel tool calls
    /// with their JSON argument objects concatenated into one
    /// string. The accumulator must split into N invocations.
    #[test]
    fn accumulator_splits_concatenated_args() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(
            0,
            "call_x",
            Some("read_file"),
            Some(r#"{"path":"a.md"}{"path":"b.md"}{"path":"c.md"}"#),
        );
        let calls = a.drain();
        assert_eq!(calls.len(), 3, "expected 3 calls, got: {calls:?}");
        assert_eq!(calls[0].arguments, json!({"path":"a.md"}));
        assert_eq!(calls[1].arguments, json!({"path":"b.md"}));
        assert_eq!(calls[2].arguments, json!({"path":"c.md"}));
        assert_eq!(calls[0].name, "read_file");
        // The first call keeps the original id; subsequent calls
        // get suffixed so each has a unique id.
        assert_eq!(calls[0].id, "call_x");
        assert_eq!(calls[1].id, "call_x__1");
        assert_eq!(calls[2].id, "call_x__2");
    }

    /// Regression: model emitted multiple separate `tool_calls`
    /// entries under the same `id` (one per SSE delta). The
    /// accumulator must de-duplicate so the server doesn't reject
    /// subsequent requests that reference those duplicate ids.
    #[test]
    fn accumulator_dedupes_duplicate_ids() {
        let mut a = ToolCallAccumulator::new();
        // Two separate deltas, each a complete JSON object,
        // but with the same id — typical minimax-m3:cloud pattern.
        a.accumulate(0, "same_id", Some("read_file"), Some(r#"{"path":"a.md"}"#));
        a.accumulate(1, "same_id", Some("read_file"), Some(r#"{"path":"b.md"}"#));
        let calls = a.drain();
        assert_eq!(calls.len(), 2);
        // Different ids, despite the model emitting the same one.
        assert_ne!(calls[0].id, calls[1].id, "ids should be unique");
        // First keeps the original.
        assert_eq!(calls[0].id, "same_id");
        assert_eq!(calls[1].id, "same_id__1");
    }

    /// Standard spec-compliant case: one delta per call, each with
    /// a unique id, each with a single JSON object as arguments.
    /// Should pass through unchanged with no id-suffixing.
    #[test]
    fn accumulator_single_call_unchanged() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(
            0,
            "call_unique",
            Some("read_file"),
            Some(r#"{"path":"a.md"}"#),
        );
        let calls = a.drain();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_unique");
        assert_eq!(calls[0].arguments, json!({"path":"a.md"}));
    }

    /// Regression: spec-compliant servers emit unique ids for each
    /// tool call. The accumulator must not add id suffixes when there
    /// are no duplicates.
    #[test]
    fn accumulator_unique_ids_passthrough() {
        let mut a = ToolCallAccumulator::new();
        a.accumulate(0, "call_a", Some("read_file"), Some(r#"{"path":"a.md"}"#));
        a.accumulate(1, "call_b", Some("read_file"), Some(r#"{"path":"b.md"}"#));
        let calls = a.drain();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_a");
        assert_eq!(calls[1].id, "call_b");
    }

    /// Regression: SSE parser panicked on long streams because
    /// `buffer.drain(..=start + 6 + end)` was off-by-two — the
    /// inclusive upper bound hit `buffer.len()` exactly when the
    /// `\n\n` separator was missing (a frame that hadn't fully
    /// arrived yet), which is out of range for `RangeToInclusive`.
    /// The fix uses an exclusive range that only consumes the
    /// `\n\n` when it's actually present, otherwise leaves the
    /// partial frame in the buffer for the next read.
    ///
    /// We exercise the buffer-management logic indirectly by
    /// re-implementing the same drain math in the test, because
    /// the actual logic is in a `tokio::spawn` closure that's
    /// hard to test in isolation. The point of this test is to
    /// keep the off-by-two invariant in mind for any future
    /// refactor.
    #[test]
    fn sse_drain_math_is_exclusive() {
        // Simulate the case from the panic: buffer ends exactly
        // at the payload boundary, no `\n\n` yet. The drain range is
        // exclusive and only consumes the terminator when present.
        let mut buffer = b"data: {\"x\":1}".to_vec();
        let start =
            find_subseq(&buffer, b"data: ").expect("data: prefix must exist in test buffer");
        let after_data = &buffer[start + 6..];
        let end = find_subseq(after_data, b"\n\n").unwrap_or(after_data.len());
        let drain_to = start
            + 6
            + end
            + if find_subseq(after_data, b"\n\n").is_some() {
                2
            } else {
                0
            };
        buffer.drain(..drain_to);
        // Buffer is now empty — we correctly drained everything
        // we'd consumed, and the (absent) `\n\n` was NOT drained.
        assert!(buffer.is_empty(), "expected empty buffer, got {buffer:?}");
    }

    /// A multibyte UTF-8 character split across chunk boundaries must
    /// not be corrupted. We keep raw bytes until the SSE frame is
    /// complete, then decode the payload as UTF-8 in one go.
    #[tokio::test]
    async fn sse_multibyte_char_split_across_chunks() {
        let json = r#"{"choices":[{"delta":{"content":"日本語"},"finish_reason":"stop"}]}"#;
        let frame = format!("data: {json}\n\n").into_bytes();
        // Split inside the three-byte UTF-8 sequence for 日.
        let split = frame.len() / 2;
        let events = run_sse(vec![frame[..split].to_vec(), frame[split..].to_vec()]).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::Text(s) if s == "日本語")),
            "expected Japanese text after split chunk, got {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Done { .. })),
            "expected Done after split frame, got {:?}",
            events.last()
        );
    }

    /// A complete SSE frame that contains invalid JSON must be
    /// discarded and reported, not buffered forever waiting for more
    /// bytes (review.md C5). A subsequent valid frame should still be
    /// parsed.
    #[tokio::test]
    async fn sse_invalid_json_frame_is_reported_and_discarded() {
        let bad = b"data: this is not json\n\n".to_vec();
        let good =
            sse_data(json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]}));
        let events = run_sse(vec![bad, good]).await;

        assert!(
            events.iter().any(|e| matches!(e, StreamEvent::Error(_))),
            "expected error for invalid JSON frame, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::Text(s) if s == "ok")),
            "expected valid frame after invalid one, got {events:?}"
        );
    }

    /// Regression: `[DONE]` sentinel and a later `finish_reason` can
    /// both try to emit `Done`. Only the first should be sent.
    #[tokio::test]
    async fn send_done_once_suppresses_duplicates() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let mut emitted = false;
        assert!(
            send_done_once(
                &tx,
                &mut emitted,
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
                "test done",
            )
            .await
        );
        assert!(emitted);
        assert!(
            send_done_once(
                &tx,
                &mut emitted,
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
                "test done duplicate",
            )
            .await
        );
        // Only one event should have been delivered.
        let first = rx.recv().await;
        assert!(matches!(first, Some(StreamEvent::Done { .. })));
        assert!(rx.is_empty());
    }

    /// Drain the channel into a Vec, up to `max` events or until empty.
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

    /// Drive the public SSE parser over a sequence of byte chunks and
    /// return everything the receiver sees.
    async fn run_sse(chunks: Vec<Vec<u8>>) -> Vec<StreamEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let stream = tokio_stream::iter(chunks.into_iter().map(Ok::<_, std::convert::Infallible>));
        parse_openai_compat_stream(tx, stream, crate::adapters::STREAM_IDLE_TIMEOUT).await;
        drain(rx, 256).await
    }

    /// SSE frames are: `data: <json>\n\n`. Build one from a JSON value.
    fn sse_data(value: serde_json::Value) -> Vec<u8> {
        format!(
            "data: {}\n\n",
            serde_json::to_string(&value).expect("test json must serialize")
        )
        .into_bytes()
    }

    fn sse_done() -> Vec<u8> {
        b"data: [DONE]\n\n".to_vec()
    }

    /// [DONE] can arrive mid-stream after tool-call deltas but before a
    /// finish_reason. The accumulated tool calls must be flushed first.
    #[tokio::test]
    async fn done_sentinel_flushes_buffered_tool_calls() {
        let events = run_sse(vec![
            sse_data(json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": {"name": "read_file", "arguments": ""}
                        }]
                    }
                }]
            })),
            sse_data(json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": null,
                            "function": {"name": null, "arguments": "{\"path\":\"a.md"}
                        }]
                    }
                }]
            })),
            sse_data(json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": null,
                            "function": {"name": null, "arguments": "\"}"}
                        }]
                    }
                }]
            })),
            sse_done(),
        ])
        .await;

        let tool_names: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_names, vec!["read_file"]);
        assert!(
            matches!(events.last(), Some(StreamEvent::Done { .. })),
            "expected Done after [DONE], got {:?}",
            events.last()
        );
    }

    /// Some proxies send a finish_reason first and a trailing [DONE]
    /// afterwards. Only one Done event should reach the consumer.
    #[tokio::test]
    async fn done_after_finish_is_suppressed() {
        let events = run_sse(vec![
            sse_data(json!({
                "choices": [{
                    "delta": {"content": "hi"},
                    "finish_reason": "stop"
                }]
            })),
            sse_done(),
        ])
        .await;

        let dones: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::Done { .. }))
            .collect();
        assert_eq!(dones.len(), 1, "expected exactly one Done, got {dones:?}");
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    /// Regression: some OpenAI-compatible servers and reverse proxies emit
    /// SSE frames with CRLF line endings (`data: ...\r\n\r\n`) instead of
    /// LF. The HTML5 spec permits this. A `\n\n`-only terminator search
    /// would never match a CRLF frame, so it would look incomplete forever
    /// and the buffer would grow to the cap and abort the stream. The
    /// parser accepts `\n\n`, `\r\n\r\n`, and `\r\r`; this feeds it a
    /// CRLF-framed content delta plus a CRLF `[DONE]` and asserts the
    /// content and the Done event both surface.
    #[tokio::test]
    async fn sse_accepts_crlf_line_endings() {
        let content = format!(
            "data: {}\r\n\r\n",
            serde_json::to_string(&json!({
                "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]
            }))
            .unwrap()
        );
        let done = "data: [DONE]\r\n\r\n";
        let events = run_sse(vec![content.into_bytes(), done.as_bytes().to_vec()]).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")),
            "expected content 'hi' from CRLF frame, got {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Done { .. })),
            "expected Done after CRLF [DONE], got {:?}",
            events.last()
        );
    }

    /// A single tool-call delta can be split across multiple SSE data
    /// frames (and therefore multiple byte chunks). The accumulator must
    /// reassemble the arguments object.
    #[tokio::test]
    async fn tool_call_arguments_split_across_sse_frames() {
        let events = run_sse(vec![
            sse_data(json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_2",
                            "function": {"name": "bash", "arguments": "{\"co"}
                        }]
                    }
                }]
            })),
            sse_data(json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": null,
                            "function": {"name": null, "arguments": "mmand\":\"ls\"}"}
                        }]
                    }
                }]
            })),
            sse_data(json!({
                "choices": [{
                    "delta": {},
                    "finish_reason": "tool_calls"
                }]
            })),
        ])
        .await;

        let args: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCall(tc) if tc.name == "bash" => Some(tc.arguments.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(args, vec![json!({"command": "ls"})]);
    }

    #[tokio::test]
    async fn sse_error_field_surfaces_as_error_event() {
        let events = run_sse(vec![sse_data(
            json!({"error": {"message": "rate limited", "type": "rate_limit"}}),
        )])
        .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s == "rate limited")));
    }

    #[tokio::test]
    async fn sse_error_field_without_message_uses_default() {
        let events = run_sse(vec![sse_data(json!({"error": {"type": "unknown"}}))]).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s == "API error")));
    }

    #[tokio::test]
    async fn sse_finish_reason_length_maps_to_length() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "length"}]}),
        )])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Length,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn sse_finish_reason_error_maps_to_error() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "error"}]}),
        )])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Error,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn sse_finish_reason_unknown_maps_to_stop() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "weird"}]}),
        )])
        .await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Stop,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn sse_finish_reason_tool_calls_with_no_calls_emits_error() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
        )])
        .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s.contains("no parseable tool calls"))));
    }

    #[tokio::test]
    async fn sse_usage_with_prompt_tokens_details_cached() {
        let events = run_sse(vec![sse_data(json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 50, "prompt_tokens_details": {"cached_tokens": 30}}}))]).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().expect("usage should be present");
                assert_eq!(u.prompt_tokens, Some(100));
                assert_eq!(u.completion_tokens, Some(50));
                assert_eq!(u.cached_tokens, Some(30));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_usage_with_cache_read_input_tokens() {
        let events = run_sse(vec![sse_data(json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 50, "cache_read_input_tokens": 25}}))]).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().expect("usage should be present");
                assert_eq!(u.cached_tokens, Some(25));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_usage_with_ollama_native_field_names() {
        let events = run_sse(vec![sse_data(json!({"choices": [{"delta": {"content": "x"}, "finish_reason": "stop"}], "usage": {"prompt_eval_count": 8, "eval_count": 12}}))]).await;
        match events.last() {
            Some(StreamEvent::Done { usage, .. }) => {
                let u = usage.as_ref().expect("usage should be present");
                assert_eq!(u.prompt_tokens, Some(8));
                assert_eq!(u.completion_tokens, Some(12));
            }
            other => panic!("expected Done with usage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_content_as_non_string_is_skipped() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {"content": 42}, "finish_reason": "stop"}]}),
        )])
        .await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    #[tokio::test]
    async fn sse_empty_content_string_skipped() {
        let events = run_sse(vec![sse_data(
            json!({"choices": [{"delta": {"content": ""}, "finish_reason": "stop"}]}),
        )])
        .await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Text(_))));
    }

    #[tokio::test]
    async fn sse_tool_calls_with_missing_index_defaults_to_zero() {
        let events = run_sse(vec![
            sse_data(json!({"choices": [{"delta": {"tool_calls": [{"id": "call_x", "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}]}}]})),
            sse_data(json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        ]).await;
        let tool = events
            .iter()
            .find_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc),
                _ => None,
            })
            .expect("tool call");
        assert_eq!(tool.name, "bash");
        assert_eq!(tool.arguments, json!({"cmd": "ls"}));
    }

    #[tokio::test]
    async fn sse_multiple_tool_calls_in_parallel() {
        let events = run_sse(vec![
            sse_data(json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "a", "function": {"name": "read_file", "arguments": "{\"path\":\"x\"}"}}, {"index": 1, "id": "b", "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}}]}}]})),
            sse_data(json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
        ]).await;
        let tools: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc.name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(tools, vec!["read_file", "bash"]);
    }

    #[tokio::test]
    async fn sse_empty_data_frame_skipped() {
        let events = run_sse(vec![
            b"data: \n\n".to_vec(),
            sse_data(json!({"choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]})),
        ])
        .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    #[tokio::test]
    async fn sse_transport_error_emits_error_event() {
        let items = vec![Err::<Vec<u8>, std::io::Error>(std::io::Error::other(
            "network down",
        ))];
        let stream = tokio_stream::iter(items);
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        parse_openai_compat_stream(tx, stream, crate::adapters::STREAM_IDLE_TIMEOUT).await;
        let events = drain(rx, 64).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Error(s) if s == "network down")));
    }

    #[tokio::test]
    async fn sse_empty_choices_array_skipped() {
        let events = run_sse(vec![
            sse_data(json!({"choices": []})),
            sse_data(json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]})),
        ])
        .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "ok")));
    }

    #[tokio::test]
    async fn sse_cr_only_line_endings_accepted() {
        let payload = serde_json::to_string(
            &json!({"choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}]}),
        )
        .unwrap();
        let frame = format!("data: {payload}\r\r");
        let events = run_sse(vec![frame.into_bytes()]).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    #[test]
    fn openai_compat_model_info_default_temperature() {
        let a = OpenAiCompatAdapter::new("http://host", "some-model", 30);
        assert_eq!(a.model_info().recommended_temperature, 0.7);
    }

    #[test]
    fn openai_compat_model_info_default_max_context() {
        let a = OpenAiCompatAdapter::new("http://host", "some-model", 30);
        assert_eq!(a.model_info().max_context_tokens, 32_768);
    }

    #[test]
    fn openai_compat_model_info_no_thinking() {
        let a = OpenAiCompatAdapter::new("http://host", "gpt-4o", 30);
        assert!(!a.model_info().supports_thinking);
    }

    #[test]
    fn openai_compat_model_info_openai_tool_format() {
        let a = OpenAiCompatAdapter::new("http://host", "gpt-4o", 30);
        assert_eq!(a.model_info().tool_call_format, ToolCallStyle::OpenAiCompat);
    }

    #[test]
    fn openai_compat_model_info_gpt4o_supports_images_and_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "gpt-4o", 30);
        assert!(a.model_info().supports_images);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_model_info_gpt5_supports_images_and_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "gpt-5", 30);
        assert!(a.model_info().supports_images);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_model_info_claude_3_5_supports_images_and_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "claude-3-5-sonnet", 30);
        assert!(a.model_info().supports_images);
        assert!(a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_model_info_gemini_supports_images_not_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "gemini-3", 30);
        assert!(a.model_info().supports_images);
        assert!(!a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_model_info_llava_supports_images_not_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "llava-7b", 30);
        assert!(a.model_info().supports_images);
        assert!(!a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_model_info_unknown_model_no_images_no_cache() {
        let a = OpenAiCompatAdapter::new("http://host", "qwen2.5", 30);
        assert!(!a.model_info().supports_images);
        assert!(!a.model_info().supports_cache);
    }

    #[test]
    fn openai_compat_new_strips_trailing_slash() {
        let a = OpenAiCompatAdapter::new("http://host/", "model", 30);
        assert_eq!(a.api_base, "http://host");
    }

    #[test]
    fn openai_compat_with_base_url_and_key_strips_slash() {
        let a = OpenAiCompatAdapter::with_base_url_and_key(
            "https://api.example.com/",
            "model",
            "key",
            30,
        );
        assert_eq!(a.api_base, "https://api.example.com");
        assert_eq!(a.api_key, "key");
    }

    #[test]
    fn openai_compat_set_json_mode_toggles() {
        let mut a = OpenAiCompatAdapter::new("http://host", "model", 30);
        assert!(!a.json_mode);
        a.set_json_mode(true);
        assert!(a.json_mode);
    }

    #[test]
    fn openai_compat_set_seed_sets_value() {
        let mut a = OpenAiCompatAdapter::new("http://host", "model", 30);
        assert!(a.seed.is_none());
        a.set_seed(Some(42));
        assert_eq!(a.seed, Some(42));
    }

    #[test]
    fn openai_compat_with_base_url_and_key_empty_key() {
        let a =
            OpenAiCompatAdapter::with_base_url_and_key("https://api.example.com", "model", "", 30);
        assert_eq!(a.api_key, "");
    }

    #[test]
    fn find_subseq_locates_needle() {
        assert_eq!(find_subseq(b"hello world", b"world"), Some(6));
        assert_eq!(find_subseq(b"hello", b"xyz"), None);
        assert_eq!(find_subseq(b"", b"x"), None);
    }

    #[test]
    fn trim_ascii_whitespace_strips_both_ends() {
        assert_eq!(trim_ascii_whitespace(b"  hi  "), b"hi");
        assert_eq!(trim_ascii_whitespace(b"\n\tdata\r\n"), b"data");
        assert_eq!(trim_ascii_whitespace(b"   "), b"");
    }

    #[tokio::test]
    async fn send_done_once_returns_true_when_already_emitted() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let mut emitted = true;
        assert!(
            send_done_once(
                &tx,
                &mut emitted,
                StreamEvent::Done {
                    finish_reason: FinishReason::Stop,
                    usage: None
                },
                "test"
            )
            .await
        );
        assert!(emitted);
    }
}
