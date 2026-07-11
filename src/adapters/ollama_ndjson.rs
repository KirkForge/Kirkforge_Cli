//! Shared NDJSON parser for adapters that hit Ollama's `/api/chat`.
//!
//! GLM, DeepSeek, and Gemini all stream NDJSON over the same HTTP endpoint.
//! The framing is identical; what differs is the *schema* of each JSON line:
//!
//! | Adapter  | Thinking field       | Tool calls                |
//! |----------|----------------------|---------------------------|
//! | GLM      | `message.thinking`   | batched at `done: true`   |
//! | DeepSeek | `message.reasoning_content` | batched at `done: true` |
//! | Gemini   | (none)               | batched at `done: true`*  |
//!
//! * Gemini's adapter historically emitted tool calls inline per chunk; that
//!   timing difference is invisible to the session loop (which only cares
//!   about the order `Text* ToolCall* Done`), so we normalize on the
//!   buffered-then-flushed behavior.
//!
//! Parameterization is via [`OllamaNdjsonConfig`]: pick a thinking field
//! name (or `None`) and the rest is shared.

use crate::shared::{FinishReason, StreamEvent, TokenUsage, ToolInvocation};
use tokio_stream::StreamExt;

/// Maximum bytes the NDJSON parser will accumulate while waiting for a
/// complete line. A misbehaving server that never emits a newline would
/// otherwise grow the buffer without bound.
const MAX_NDJSON_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Per-adapter knobs for [`parse_ollama_ndjson_stream`].
#[derive(Clone)]
pub struct OllamaNdjsonConfig {
    /// Field path on the message object that holds the model's chain-of-thought
    /// (analogous to OpenAI's `reasoning_content`). `None` means the model has
    /// no thinking channel (e.g. Gemini).
    pub thinking_field: Option<&'static str>,
}

impl OllamaNdjsonConfig {
    /// GLM-5.1:Cloud — uses `message.thinking`.
    pub const GLM: Self = Self {
        thinking_field: Some("thinking"),
    };
    /// DeepSeek-v4-Pro — uses `message.reasoning_content`.
    pub const DEEPSEEK: Self = Self {
        thinking_field: Some("reasoning_content"),
    };
    /// Gemini 3.0 Flash 1M — no thinking field through Ollama.
    pub const GEMINI: Self = Self {
        thinking_field: None,
    };
}

/// Drive an Ollama `/api/chat` NDJSON response into a `StreamEvent` channel.
///
/// Reads `response.bytes_stream()` on the current task, line-buffers until
/// `\n`, parses each complete line as JSON, and emits the appropriate
/// `StreamEvent` variants. Tool calls are buffered and flushed at
/// `done: true` — the session loop only cares about the *order* of events
/// within a turn, not when during streaming they arrive.
///
/// Errors (transport, JSON parse, API `error` field) become
/// `StreamEvent::Error`. The task returns when the upstream stream ends or
/// yields an error; the receiver stays open until the caller drops it.
///
/// The stream is taken as an explicit parameter (rather than reconstructed
/// from a `reqwest::Response`) so the function is testable against a
/// fabricated stream and so the caller controls ownership of the response.
///
/// **Dropped-receiver behavior:** every `tx.send` here goes through
/// [`send_or_bail`]. If the consumer is gone (executor cancelled, TUI
/// closed, session aborted), the send fails and we log once at the call
/// site and break out of the loop — there is no point draining the
/// upstream HTTP body when nobody's listening, and silently swallowing
/// the failure hides the cancellation cause.
pub async fn parse_ollama_ndjson_stream<B, E, S>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    config: OllamaNdjsonConfig,
    mut stream: S,
) where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
    S: tokio_stream::Stream<Item = Result<B, E>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut tool_calls_buffer: Vec<ToolInvocation> = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                buffer.extend_from_slice(bytes.as_ref());
                if buffer.len() > MAX_NDJSON_BUFFER_BYTES {
                    let _ = tx
                        .send(StreamEvent::Error(format!(
                            "NDJSON line buffer exceeded {} MiB limit; aborting stream",
                            MAX_NDJSON_BUFFER_BYTES / (1024 * 1024)
                        )))
                        .await;
                    return;
                }

                // Ollama NDJSON: one JSON object per line.  We split on
                // newline bytes and only decode complete lines as UTF-8 so
                // that a chunk boundary that falls in the middle of a
                // multi-byte character never produces replacement
                // characters and corrupts the JSON.
                while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                    // Drain the line *without* the trailing newline.
                    let line_bytes: Vec<u8> = buffer.drain(..newline_pos).collect();
                    // Consume the newline itself.
                    buffer.drain(..1);

                    // Tolerate \r\n by dropping a trailing carriage return.
                    let line_bytes = if line_bytes.ends_with(b"\r") {
                        &line_bytes[..line_bytes.len() - 1]
                    } else {
                        &line_bytes[..]
                    };

                    // Skip empty/whitespace lines.
                    if line_bytes.iter().all(|&b| b.is_ascii_whitespace()) {
                        continue;
                    }

                    let line = match std::str::from_utf8(line_bytes) {
                        Ok(s) => s.trim(),
                        Err(e) => {
                            // A complete line that is not valid UTF-8 is a
                            // server-side encoding error; waiting for more
                            // bytes will not fix it.  Surface it and move
                            // on rather than stalling forever.
                            if !send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("NDJSON line is not valid UTF-8: {e}")),
                                "UTF-8 decode error",
                            )
                            .await
                            {
                                return;
                            }
                            continue;
                        }
                    };

                    if line.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<serde_json::Value>(line) {
                        Ok(json) => {
                            // API-level error
                            if let Some(err) = json.get("error") {
                                let msg = err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .or_else(|| err.as_str())
                                    .map(String::from)
                                    .unwrap_or_else(|| err.to_string());
                                if !send_or_bail(&tx, StreamEvent::Error(msg), "API error event")
                                    .await
                                {
                                    return;
                                }
                                continue;
                            }

                            // Thinking field (GLM/DeepSeek)
                            if let Some(field) = config.thinking_field {
                                if let Some(thinking) =
                                    json.get("message").and_then(|m| m.get(field))
                                {
                                    if let Some(t) = thinking.as_str() {
                                        if !t.is_empty()
                                            && !send_or_bail(
                                                &tx,
                                                StreamEvent::Thinking(t.to_string()),
                                                "thinking chunk",
                                            )
                                            .await
                                        {
                                            return;
                                        }
                                    }
                                }
                            }

                            // Text content
                            if let Some(content) =
                                json.get("message").and_then(|m| m.get("content"))
                            {
                                if let Some(c) = content.as_str() {
                                    if !c.is_empty()
                                        && !send_or_bail(
                                            &tx,
                                            StreamEvent::Text(c.to_string()),
                                            "text chunk",
                                        )
                                        .await
                                    {
                                        return;
                                    }
                                }
                            }

                            // Tool calls — buffer until done
                            if let Some(tcs) = json.get("message").and_then(|m| m.get("tool_calls"))
                            {
                                if let Some(calls) = tcs.as_array() {
                                    let before = tool_calls_buffer.len();
                                    for tc in calls {
                                        if let (Some(name), Some(args)) = (
                                            tc.get("function")
                                                .and_then(|f| f.get("name"))
                                                .and_then(|n| n.as_str()),
                                            tc.get("function").and_then(|f| f.get("arguments")),
                                        ) {
                                            tool_calls_buffer.push(ToolInvocation {
                                                id: tc
                                                    .get("id")
                                                    .and_then(|id| id.as_str())
                                                    .unwrap_or("")
                                                    .to_string(),
                                                name: name.to_string(),
                                                arguments: args.clone(),
                                            });
                                        }
                                    }
                                    let parsed = tool_calls_buffer.len() - before;
                                    if !calls.is_empty() && parsed == 0
                                        && !send_or_bail(
                                            &tx,
                                            StreamEvent::Error(
                                                "Model emitted tool_calls with no parseable entries"
                                                    .to_string(),
                                            ),
                                            "tool-call parse error",
                                        )
                                        .await
                                        {
                                            return;
                                        }
                                }
                            }

                            // Done — flush buffered tool calls + emit Done
                            if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                                for tc in tool_calls_buffer.drain(..) {
                                    if !send_or_bail(
                                        &tx,
                                        StreamEvent::ToolCall(tc),
                                        "buffered tool call",
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }

                                let usage = json.get("usage").map(parse_token_usage);

                                let reason = json
                                    .get("done_reason")
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("stop");

                                let finish_reason = match reason {
                                    "length" => FinishReason::Length,
                                    "tool_calls" => FinishReason::ToolCalls,
                                    "error" => FinishReason::Error,
                                    _ => FinishReason::Stop,
                                };

                                if !send_or_bail(
                                    &tx,
                                    StreamEvent::Done {
                                        finish_reason,
                                        usage,
                                    },
                                    "done",
                                )
                                .await
                                {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            if !send_or_bail(
                                &tx,
                                StreamEvent::Error(format!("JSON parse: {e}")),
                                "JSON parse error",
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
                // Last-ditch error delivery. If the consumer is also
                // gone, log once and exit — same pattern as the
                // per-event sends above, but at the bottom of the
                // loop because we're about to break anyway.
                if !send_or_bail(
                    &tx,
                    StreamEvent::Error(e.to_string()),
                    "Ollama transport error",
                )
                .await
                {
                    return;
                }
                break;
            }
        }
    }

    // `response` is no longer needed; the bytes stream is the only thing
    // we actually drive. (The arg was removed in commit 2 — see git log.)
}
/// message and bail-semantics consistent across both adapters.
pub(crate) async fn send_or_bail(
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
    ev: StreamEvent,
    kind: &'static str,
) -> bool {
    if tx.send(ev).await.is_ok() {
        true
    } else {
        tracing::warn!(
            event_kind = kind,
            "Stream consumer dropped receiver mid-stream; aborting adapter parser"
        );
        false
    }
}

/// Parse an Ollama `usage` object into a [`TokenUsage`].
///
/// Ollama's native `/api/chat` response uses `prompt_eval_count` and
/// `eval_count` for the token counts. Some adapters (notably
/// OpenAI-compat mode through Ollama, or the GLM/DeepSeek cloud
/// proxies) emit the OpenAI-style `prompt_tokens` / `completion_tokens`
/// instead. We try both shapes and prefer whichever is populated, so
/// usage is reported correctly across native and compat adapters.
///
/// This was the bug behind GPT 5.5 review finding #7: the parser
/// previously only looked at the OpenAI-style fields, so a stock
/// `ollama run deepseek-v4` invocation never surfaced token usage even
/// though the `done: true` line always includes the counts.
fn parse_token_usage(u: &serde_json::Value) -> TokenUsage {
    let prompt_tokens = u
        .get("prompt_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| u.get("prompt_eval_count").and_then(|v| v.as_u64()))
        .map(|v| v as usize);
    let completion_tokens = u
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| u.get("eval_count").and_then(|v| v.as_u64()))
        .map(|v| v as usize);
    // Ollama's native /api/chat added cached-prompt reporting in 0.5.x
    // (`prompt_eval_count` is the fresh-eval count; the rest of the
    // prompt is served from KV-cache). Tolerate absence — older
    // Ollama versions simply don't surface the count.
    let cached_tokens = u
        .get("cached_count")
        .or_else(|| u.get("cached_tokens"))
        .or_else(|| {
            u.get("prompt_tokens_details")
                .and_then(|p| p.get("cached_tokens"))
        })
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    TokenUsage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{FinishReason, StreamEvent};
    use serde_json::json;

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

    /// A test stream that yields a fixed sequence of byte chunks, then EOF.
    fn chunks(
        items: Vec<Vec<u8>>,
    ) -> impl tokio_stream::Stream<Item = Result<Vec<u8>, std::convert::Infallible>> {
        tokio_stream::iter(items.into_iter().map(Ok))
    }

    /// Convert a single Ollama NDJSON line into the byte representation
    /// the parser expects (line + trailing `\n`).
    fn line(s: &str) -> Vec<u8> {
        format!("{s}\n").into_bytes()
    }

    /// Drive the parser over a sequence of NDJSON lines and return the events.
    async fn run(lines: &[&str]) -> Vec<StreamEvent> {
        run_config(lines, OllamaNdjsonConfig::GLM).await
    }

    /// Drive the parser with an explicit adapter config.
    async fn run_config(lines: &[&str], config: OllamaNdjsonConfig) -> Vec<StreamEvent> {
        let body: Vec<u8> = lines.iter().flat_map(|l| line(l)).collect();
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        parse_ollama_ndjson_stream(tx, config, chunks(vec![body])).await;
        drain(rx, 1024).await
    }

    #[tokio::test]
    async fn glm_thinking_and_text_are_emitted() {
        let events = run(&[
            r#"{"message":{"thinking":"let me think","content":""},"done":false}"#,
            r#"{"message":{"thinking":"","content":"Hello "},"done":false}"#,
            r#"{"message":{"thinking":"","content":"world"},"done":true,"done_reason":"stop","usage":{"prompt_tokens":3,"completion_tokens":5}}"#,
        ])
        .await;

        // Thinking comes first, then text fragments in order, then Done.
        assert!(matches!(events.first(), Some(StreamEvent::Thinking(t)) if t == "let me think"));
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["Hello ", "world"]);

        // The terminal event must be Done with Stop + the right token counts.
        match events.last() {
            Some(StreamEvent::Done {
                finish_reason,
                usage,
            }) => {
                assert!(matches!(finish_reason, FinishReason::Stop));
                let u = usage.as_ref().expect("usage should be present");
                assert_eq!(u.prompt_tokens, Some(3));
                assert_eq!(u.completion_tokens, Some(5));
            }
            other => panic!("expected Done as last event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gemini_config_skips_thinking_field() {
        // Build a line that names "thinking" but config has None → no Thinking event.
        let body: Vec<u8> = line(
            r#"{"message":{"thinking":"ignored","content":"hi"},"done":true,"done_reason":"stop"}"#,
        );
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GEMINI, chunks(vec![body])).await;
        let events = drain(rx, 16).await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Thinking(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    #[tokio::test]
    async fn tool_calls_buffered_and_flushed_at_done() {
        let events = run(&[
            r#"{"message":{"content":"calling tool","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"/etc/hosts"}}}]},"done":false}"#,
            r#"{"message":{"content":"","tool_calls":[{"function":{"name":"ls","arguments":{}}}]},"done":true,"done_reason":"tool_calls"}"#,
        ])
        .await;
        // Text fragments: the second chunk's empty `content` is intentionally
        // skipped by the parser (the `if !c.is_empty()` guard in the loop),
        // so we only see the first chunk's "calling tool".
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["calling tool"]);
        let tool_names: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCall(tc) => Some(tc.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_names, vec!["read_file", "ls"]);
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
                ..
            })
        ));
    }

    /// Regression: a chunk boundary that falls inside a multi-byte UTF-8
    /// character used to corrupt the line via `from_utf8_lossy`
    /// replacement characters and fail JSON parsing. The byte-buffer parser
    /// must wait for the complete character (and the trailing newline) before
    /// decoding.
    #[tokio::test]
    async fn structured_api_error_surfaces_message() {
        let events = run(&[r#"{"error":{"message":"model 'qwen' not found"}}"#]).await;
        assert!(
            events.iter().any(
                |e| matches!(e, StreamEvent::Error(s) if s.contains("model 'qwen' not found"))
            ),
            "expected structured API error message, got {events:?}"
        );
    }

    #[tokio::test]
    async fn ndjson_multibyte_char_split_across_chunks() {
        // "héllo" contains a two-byte UTF-8 character for é (C3 A9).
        let line = r#"{"message":{"content":"héllo"},"done":true,"done_reason":"stop"}"#;
        let bytes = line.as_bytes();
        // Find the byte index of the first byte of "é" (C3) and split there.
        let split = bytes
            .iter()
            .position(|&b| b == 0xC3)
            .expect("é should start with 0xC3");
        let first = bytes[..split].to_vec();
        let mut second = bytes[split..].to_vec();
        second.push(b'\n');

        let events = run_with_chunks(vec![first, second]).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, StreamEvent::Text(s) if s == "héllo")),
            "expected héllo text event, got {events:?}"
        );
        assert!(
            matches!(events.last(), Some(StreamEvent::Done { .. })),
            "expected Done"
        );
    }

    /// Multiple complete NDJSON lines delivered in a single chunk must all
    /// be parsed.
    #[tokio::test]
    async fn ndjson_multiple_lines_in_one_chunk() {
        let body: Vec<u8> = [
            line(r#"{"message":{"content":"a"},"done":false}"#),
            line(r#"{"message":{"content":"b"},"done":false}"#),
            line(r#"{"message":{"content":"c"},"done":true,"done_reason":"stop"}"#),
        ]
        .into_iter()
        .flatten()
        .collect();

        let events = run_with_chunks(vec![body]).await;
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    /// An incomplete final line at the end of a chunk must be retained and
    /// completed by the next chunk.
    #[tokio::test]
    async fn ndjson_incomplete_line_completes_next_chunk() {
        let first = r#"{"message":{"content":"first"},"done":true}"#;
        // Split the first JSON line in the middle; the second chunk carries
        // the rest plus a trailing newline.
        let split = first.len() / 2;
        let partial = first.as_bytes()[..split].to_vec();
        let mut rest = first.as_bytes()[split..].to_vec();
        rest.push(b'\n');

        let events = run_with_chunks(vec![partial, rest]).await;
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["first"]);
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    /// Windows-style \\r\\n line endings must be tolerated.
    #[tokio::test]
    async fn ndjson_handles_crlf_line_endings() {
        let body = b"{\"message\":{\"content\":\"hi\"},\"done\":true}\r\n".to_vec();
        let events = run_with_chunks(vec![body]).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    /// Helper like `run` but accepts raw byte chunks instead of pre-encoded
    /// strings, so we can simulate arbitrary splits.
    async fn run_with_chunks(items: Vec<Vec<u8>>) -> Vec<StreamEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GLM, chunks(items)).await;
        drain(rx, 1024).await
    }

    #[test]
    fn parse_token_usage_extracts_counts() {
        let u = json!({"prompt_tokens": 12, "completion_tokens": 34});
        let t = parse_token_usage(&u);
        assert_eq!(t.prompt_tokens, Some(12));
        assert_eq!(t.completion_tokens, Some(34));
    }

    /// Ollama's native `/api/chat` `usage` object uses the
    /// `prompt_eval_count` / `eval_count` names, not the OpenAI-style
    /// fields. Previously the parser only looked at the OpenAI names
    /// and silently reported `None` for both counts, so the CostStats
    /// event never fired for native-Ollama models. This is the
    /// regression test for GPT 5.5 review finding #7.
    #[test]
    fn parse_token_usage_falls_back_to_ollama_native_fields() {
        let u = json!({"prompt_eval_count": 7, "eval_count": 11});
        let t = parse_token_usage(&u);
        assert_eq!(t.prompt_tokens, Some(7));
        assert_eq!(t.completion_tokens, Some(11));
    }

    /// Mixed shapes: some proxies emit one set of names, some the
    /// other. We should accept either; the test confirms we don't
    /// accidentally require *both* to be in the same shape.
    #[test]
    fn parse_token_usage_mixed_shapes() {
        let u = json!({"prompt_tokens": 5, "eval_count": 9});
        let t = parse_token_usage(&u);
        assert_eq!(t.prompt_tokens, Some(5));
        assert_eq!(t.completion_tokens, Some(9));
    }

    #[test]
    fn parse_token_usage_handles_missing_fields() {
        let u = json!({});
        let t = parse_token_usage(&u);
        assert_eq!(t.prompt_tokens, None);
        assert_eq!(t.completion_tokens, None);
    }

    #[test]
    fn glm_config_has_thinking_field() {
        assert_eq!(OllamaNdjsonConfig::GLM.thinking_field, Some("thinking"));
    }

    #[test]
    fn deepseek_config_has_reasoning_field() {
        assert_eq!(
            OllamaNdjsonConfig::DEEPSEEK.thinking_field,
            Some("reasoning_content")
        );
    }

    #[test]
    fn gemini_config_has_no_thinking() {
        assert_eq!(OllamaNdjsonConfig::GEMINI.thinking_field, None);
    }

    /// DeepSeek uses `reasoning_content` for its chain-of-thought channel.
    #[tokio::test]
    async fn deepseek_reasoning_content_is_emitted() {
        let events = run_config(
            &[
                r#"{"message":{"reasoning_content":"step one","content":""},"done":false}"#,
                r#"{"message":{"reasoning_content":"","content":"answer"},"done":true,"done_reason":"stop"}"#,
            ],
            OllamaNdjsonConfig::DEEPSEEK,
        )
        .await;
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Thinking(s) if s == "step one")));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "answer")));
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    /// Empty thinking/reasoning strings must not produce Thinking events.
    #[tokio::test]
    async fn empty_thinking_field_is_skipped() {
        let events = run(&[
            r#"{"message":{"thinking":"","content":"hi"},"done":true,"done_reason":"stop"}"#,
        ])
        .await;
        assert!(!events.iter().any(|e| matches!(e, StreamEvent::Thinking(_))));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "hi")));
    }

    /// An API-level `error` field on a JSON line must surface as a
    /// StreamEvent::Error with the message text.
    #[tokio::test]
    async fn api_error_field_surfaces_as_error_event() {
        let events = run(&[
            r#"{"error":"model not found"}"#,
            r#"{"message":{"content":"x"},"done":true,"done_reason":"stop"}"#,
        ])
        .await;
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Error(s) if s == "model not found"
        )));
    }

    /// A malformed JSON line must be reported, and parsing continues on
    /// the next line.
    #[tokio::test]
    async fn malformed_json_line_emits_parse_error_and_continues() {
        let events = run(&[
            r#"{"message":{"content":"a"},"done":false}"#,
            r#"{not json}"#,
            r#"{"message":{"content":"b"},"done":true,"done_reason":"stop"}"#,
        ])
        .await;
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Error(s) if s.starts_with("JSON parse:")
        )));
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b"]);
    }

    /// A complete line that is not valid UTF-8 is an encoding error; the
    /// parser surfaces it and keeps going.
    #[tokio::test]
    async fn invalid_utf8_line_emits_error_and_continues() {
        let mut bad = vec![0xC3]; // lone continuation byte is invalid
        bad.push(b'\n');
        let rest = line(r#"{"message":{"content":"ok"},"done":true}"#);
        let events = run_with_chunks(vec![bad, rest]).await;
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Error(s) if s.starts_with("NDJSON line is not valid UTF-8")
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::Text(s) if s == "ok")));
    }

    /// A transport error on the upstream byte stream becomes the last
    /// event delivered before the parser exits.
    #[tokio::test]
    async fn transport_error_becomes_error_event() {
        let items = vec![Ok::<_, std::io::Error>(line(
            r#"{"message":{"content":"partial"},"done":false}"#,
        ))];
        let stream = tokio_stream::iter(items).chain(tokio_stream::iter(vec![Err(
            std::io::Error::other("connection reset"),
        )]));
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        parse_ollama_ndjson_stream(tx, OllamaNdjsonConfig::GLM, stream).await;
        let events = drain(rx, 16).await;
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Error(s) if s == "connection reset"
        )));
    }

    /// `done_reason: "length"` must map to FinishReason::Length.
    #[tokio::test]
    async fn done_reason_length_maps_to_length_finish() {
        let events =
            run(&[r#"{"message":{"content":"..."},"done":true,"done_reason":"length"}"#]).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Length,
                ..
            })
        ));
    }

    /// `done_reason: "error"` must map to FinishReason::Error.
    #[tokio::test]
    async fn done_reason_error_maps_to_error_finish() {
        let events =
            run(&[r#"{"message":{"content":"!"},"done":true,"done_reason":"error"}"#]).await;
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Done {
                finish_reason: FinishReason::Error,
                ..
            })
        ));
    }

    /// Tool-call entries that are malformed (no parseable function name and
    /// arguments) must trigger an error event when non-empty raw array is
    /// present.
    #[tokio::test]
    async fn malformed_tool_calls_emits_error() {
        let events = run(&[
            r#"{"message":{"content":"","tool_calls":[{"bad":"shape"}]},"done":true,"done_reason":"tool_calls"}"#,
        ])
        .await;
        assert!(events.iter().any(|e| matches!(
            e,
            StreamEvent::Error(s) if s == "Model emitted tool_calls with no parseable entries"
        )));
    }

    #[test]
    fn parse_token_usage_reads_cached_count() {
        let u = json!({"prompt_tokens": 10, "completion_tokens": 20, "cached_count": 5});
        let t = parse_token_usage(&u);
        assert_eq!(t.cached_tokens, Some(5));
    }

    #[test]
    fn parse_token_usage_reads_cached_tokens_alias() {
        let u = json!({"prompt_tokens": 10, "completion_tokens": 20, "cached_tokens": 6});
        let t = parse_token_usage(&u);
        assert_eq!(t.cached_tokens, Some(6));
    }

    #[test]
    fn parse_token_usage_reads_prompt_tokens_details_cached() {
        let u = json!({"prompt_tokens": 10, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 7}});
        let t = parse_token_usage(&u);
        assert_eq!(t.cached_tokens, Some(7));
    }

    /// Empty and whitespace-only lines between NDJSON objects must be
    /// silently skipped.
    #[tokio::test]
    async fn empty_and_whitespace_lines_are_skipped() {
        let body: Vec<u8> = [
            line(r#"{"message":{"content":"a"},"done":false}"#),
            b"\n".to_vec(),
            b"   \n".to_vec(),
            line(r#"{"message":{"content":"b"},"done":true,"done_reason":"stop"}"#),
        ]
        .into_iter()
        .flatten()
        .collect();
        let events = run_with_chunks(vec![body]).await;
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["a", "b"]);
    }
}
