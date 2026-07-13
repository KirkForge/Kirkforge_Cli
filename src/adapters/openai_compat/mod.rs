//! OpenAI-compatible fallback adapter.
//!
//! Uses `/v1/chat/completions` (SSE streaming) instead of `/api/chat` (NDJSON).
//! Activated for any model that doesn't match GLM/DeepSeek/Gemini patterns,
//! or explicitly via `--model-type openai`.
//!
//! Parses SSE `data: {...}` lines. Supports tool calls in the
//! OpenAI function-calling format.

use crate::shared::{FinishReason, Message, ModelInfo, StreamEvent, TokenUsage, ToolCallStyle};
use tokio_stream::StreamExt;

use super::ModelAdapter;

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
/// Maximum bytes the SSE parser will accumulate while waiting for a complete
/// `data: ...\n\n` frame. A misbehaving server that never emits a frame
/// terminator would otherwise grow the buffer without bound.
const MAX_SSE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

pub(crate) async fn parse_openai_compat_stream<B, E, S>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: S,
) where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
    S: tokio_stream::Stream<Item = Result<B, E>> + Unpin,
{
    let mut buffer = String::new();
    let mut pending_tool_calls = ToolCallAccumulator::new();
    let mut done_emitted = false;

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                buffer.push_str(&String::from_utf8_lossy(bytes.as_ref()));
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
                // We accumulate bytes into `buffer` until we
                // see a complete `data: ...\n\n` frame, then
                // slice the payload out. We only **drain**
                // consumed bytes after a successful JSON
                // parse — if the parse fails, the model has
                // streamed an event whose JSON body is
                // incomplete (a `tool_calls.arguments`
                // fragment with an unterminated string, in
                // practice), and we need the next chunk to
                // complete it. The outer stream loop will
                // re-read more bytes into the same buffer
                // and we'll retry from this position.
                while let Some(start) = buffer.find("data: ") {
                    let after_data = &buffer[start + 6..];
                    // If the frame isn't complete yet (no
                    // terminating blank line and not at the
                    // end of the buffer), wait for more bytes.
                    //
                    // SSE frames end at a blank line, and the HTML5
                    // spec allows LF (`\n\n`), CRLF (`\r\n\r\n`), and
                    // CR (`\r\r`) line endings. A reverse proxy or
                    // OpenAI-compatible server using CRLF would never
                    // satisfy a `\n\n`-only search, so the frame would
                    // look incomplete forever and the buffer would
                    // grow to the cap and abort the stream. Take the
                    // earliest terminator present.
                    let sep = ["\n\n", "\r\n\r\n", "\r\r"]
                        .into_iter()
                        .filter_map(|t| after_data.find(t).map(|i| (i, t.len())))
                        .min_by_key(|(i, _)| *i);
                    let Some((sep_idx, term_len)) = sep else {
                        // Incomplete frame. Bail out of the
                        // inner loop; the outer stream
                        // loop will read more.
                        break;
                    };
                    let line: String = after_data[..sep_idx].trim().to_string();
                    // Drain only on the happy path — on
                    // parse error we leave the frame in
                    // place for the next read.
                    let drain_to = start + 6 + sep_idx + term_len;

                    if line.is_empty() || line == "[DONE]" {
                        buffer.drain(..drain_to);
                        if line == "[DONE]" {
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

                    match serde_json::from_str::<serde_json::Value>(&line) {
                        Ok(json) => {
                            buffer.drain(..drain_to);
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
                                    // Try OpenAI-style fields first, then
                                    // Ollama-native names. Some
                                    // OpenAI-compat proxies through
                                    // Ollama emit the native names
                                    // (prompt_eval_count / eval_count)
                                    // even though the rest of the
                                    // framing is OpenAI-compat. See
                                    // `parse_token_usage` in
                                    // ollama_ndjson.rs for the
                                    // corresponding fix there.
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
                                    // Cache hit count. OpenAI's
                                    // chat-completions endpoint
                                    // surfaces it under
                                    // `prompt_tokens_details.cached_tokens`;
                                    // Anthropic-style responses
                                    // (routed through some
                                    // OpenAI-compat proxies) use
                                    // the top-level
                                    // `cache_read_input_tokens`.
                                    // Either name is fine — we
                                    // just look up both and
                                    // tolerate absence.
                                    cached_tokens: u
                                        .get("cache_read_input_tokens")
                                        .and_then(|v| v.as_u64())
                                        .or_else(|| {
                                            u.get("prompt_tokens_details")
                                                .and_then(|d| d.get("cached_tokens"))
                                                .and_then(|v| v.as_u64())
                                        })
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
                            // The frame was complete (had
                            // its `\n\n`) but its JSON body
                            // is invalid. This is the
                            // model streaming an event with
                            // an unterminated string (a
                            // `tool_calls.arguments`
                            // fragment). Don't drain — the
                            // next chunk will append the
                            // continuation and we'll retry.
                            // Don't notify the consumer —
                            // it's a transient streaming
                            // artefact, not an error.
                            tracing::debug!(
                                error = %e,
                                line_bytes = line.len(),
                                "openai_compat: incomplete JSON in event, waiting for more bytes"
                            );
                            break;
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
    client: reqwest::Client,
    json_mode: bool,
    timeout_secs: u64,
}

impl OpenAiCompatAdapter {
    pub fn new(ollama_host: &str, model: &str, timeout_secs: u64) -> Self {
        let api_base = ollama_host.trim_end_matches('/').to_string();
        Self {
            model: model.to_string(),
            api_base,
            client: super::build_reqwest_client(),
            json_mode: false,
            timeout_secs,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for OpenAiCompatAdapter {
    fn model_info(&self) -> ModelInfo {
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
            self.json_mode,
        );
        let url = format!("{}/v1/chat/completions", self.api_base);

        let response = super::send_with_retry(&self.client, || async {
            self.client
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_secs(self.timeout_secs))
                .send()
                .await
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

        tokio::spawn(parse_openai_compat_stream(tx, response.bytes_stream()));

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
        let s = r#"{"path":"AGENTS.md"}{"path":"REPORULES.md"}{"path":"README.md"}{"path":"ARCHITECTURE.md"}"#;
        let out = split_concatenated_json(s);
        assert_eq!(out.len(), 4, "expected 4 objects, got: {out:?}");
        assert_eq!(out[0], json!({"path":"AGENTS.md"}));
        assert_eq!(out[1], json!({"path":"REPORULES.md"}));
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
        // at the payload boundary, no `\n\n` yet. Old code:
        //   drain(..=start + 6 + end) — equal to drain(..=L) — panic.
        // New code: drain(..start + 6 + end + 0) — exclusive, in range.
        let mut buffer = String::from("data: {\"x\":1}");
        let start = buffer.find("data: ").unwrap();
        let after_data = &buffer[start + 6..];
        let end = after_data.find("\n\n").unwrap_or(after_data.len());
        let drain_to = start + 6 + end + if after_data.contains("\n\n") { 2 } else { 0 };
        buffer.drain(..drain_to);
        // Buffer is now empty — we correctly drained everything
        // we'd consumed, and the (absent) `\n\n` was NOT drained.
        assert!(buffer.is_empty(), "expected empty buffer, got {buffer:?}");
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
        parse_openai_compat_stream(tx, stream).await;
        drain(rx, 256).await
    }

    /// SSE frames are: `data: <json>\n\n`. Build one from a JSON value.
    fn sse_data(value: serde_json::Value) -> Vec<u8> {
        format!("data: {}\n\n", serde_json::to_string(&value).unwrap()).into_bytes()
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
}
