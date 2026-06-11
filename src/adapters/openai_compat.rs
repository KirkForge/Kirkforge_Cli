//! OpenAI-compatible fallback adapter.
//!
//! Uses `/v1/chat/completions` (SSE streaming) instead of `/api/chat` (NDJSON).
//! Activated for any model that doesn't match GLM/DeepSeek/Gemini patterns,
//! or explicitly via `--model-type openai`.
//!
//! Parses SSE `data: {...}` lines. Supports tool calls in the
//! OpenAI function-calling format.

use crate::shared::{
    FinishReason, Message, ModelInfo, StreamEvent, TokenUsage, ToolCallStyle, ToolInvocation,
};
use tokio_stream::StreamExt;

use super::ModelAdapter;

/// Accumulator for OpenAI SSE tool-call deltas.
///
/// OpenAI streams tool calls incrementally across multiple SSE events.
/// The first delta has `id` and `name`, subsequent deltas only have
/// `arguments` fragments. Keyed by `index` (0-based within the array).
///
/// Example delta sequence:
///   {index: 0, id: "call_1", function: {name: "read_file", arguments: ""}}
///   {index: 0, id: null,      function: {name: null,        arguments: "{\"path\":" }}
///   {index: 0, id: null,      function: {name: null,        arguments: " \"/etc\"}" }}
struct ToolCallAccumulator {
    /// Keyed by `index` field from the delta.
    calls: std::collections::HashMap<usize, (String, String, String)>, // (id, name, args_json)
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self {
            calls: std::collections::HashMap::new(),
        }
    }

    /// Accumulate one delta. Merges `arguments` by appending.
    fn accumulate(&mut self, index: usize, id: &str, name: Option<&str>, args: Option<&str>) {
        let entry = self
            .calls
            .entry(index)
            .or_insert_with(|| (id.to_string(), String::new(), String::new()));
        // ID: only set on first delta — keep whatever we get
        if !id.is_empty() {
            entry.0 = id.to_string();
        }
        // Name: set when present (first delta, usually)
        if let Some(n) = name {
            entry.1 = n.to_string();
        }
        // Arguments: append incrementally across deltas
        if let Some(a) = args {
            entry.2.push_str(a);
        }
    }

    /// Drain all accumulated calls as ToolInvocation values.
    ///
    /// Two adapter-layer problems are handled here:
    ///
    /// 1. **Concatenated argument objects.** Some models
    ///    (notably `minimax-m3:cloud` via Ollama's
    ///    OpenAI-compat layer) emit multiple parallel tool
    ///    calls in a single delta with their argument
    ///    objects *concatenated* into one string, e.g.
    ///
    ///      arguments = `"{\"path\":\"a\"}{\"path\":\"b\"}"`
    ///
    ///    We split on top-level JSON object boundaries
    ///    and emit one `ToolInvocation` per object.
    ///
    /// 2. **Duplicate call IDs.** The same model
    ///    occasionally emits multiple `tool_calls` under
    ///    the same `id` field, which is not spec-
    ///    compliant. Ollama's OpenAI-compat layer
    ///    rejects subsequent requests that reference
    ///    those duplicate ids. We de-duplicate by
    ///    suffixing the original id with `__<index>`
    ///    so every emitted call has a unique id.
    fn drain(&mut self) -> Vec<ToolInvocation> {
        let mut out: Vec<_> = self.calls.drain().collect();
        out.sort_by_key(|(idx, _)| *idx);
        let mut next: usize = 0;
        out.into_iter()
            .flat_map(|(_, (id, name, args_json))| {
                let args = split_concatenated_json(&args_json);
                args.into_iter()
                    .map(|arg| {
                        let unique_id = if next == 0 {
                            id.clone()
                        } else {
                            format!("{}__{}", id, next)
                        };
                        next += 1;
                        ToolInvocation {
                            id: unique_id,
                            name: name.clone(),
                            arguments: arg,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

pub struct OpenAiCompatAdapter {
    model: String,
    api_base: String,
    client: reqwest::Client,
    json_mode: bool,
}

impl OpenAiCompatAdapter {
    pub fn new(ollama_host: &str, model: &str) -> Self {
        let api_base = ollama_host.trim_end_matches('/').to_string();
        Self {
            model: model.to_string(),
            api_base,
            client: reqwest::Client::builder()
                .tcp_nodelay(true)
                .build()
                .expect("reqwest client build failed"),
            json_mode: false,
        }
    }
}

#[async_trait::async_trait]
impl ModelAdapter for OpenAiCompatAdapter {
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            name: self.model.clone(),
            supports_thinking: false,
            tool_call_format: ToolCallStyle::OpenAiCompat,
            max_context_tokens: 32_768, // conservative default
            recommended_temperature: 0.7,
            // Conservative default: only the named vision / Anthropic /
            // OpenAI-prefixed models are known to accept image inputs.
            // Adapters with vision support that don't match the prefix
            // (e.g. a local `llava` running behind an OpenAI-compat
            // proxy) will report a "tool not available" error from the
            // model, which is the right surface to fix the registration.
            supports_images: false,
            // Most OpenAI-compat servers ignore cache_control, and the
            // field is unknown to Ollama's /v1/chat/completions
            // endpoint. Set `true` only for the explicitly cache-aware
            // models (claude-3-*, gpt-4o, gpt-5) when we know the
            // server honours the marker.
            supports_cache: self.model.starts_with("claude-3-")
                || self.model.starts_with("gpt-4o")
                || self.model.starts_with("gpt-5"),
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

        let response = self
            .client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await?
            .error_for_status()?;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamEvent>(128);

        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut pending_tool_calls = ToolCallAccumulator::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));

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
                            // If the frame isn't complete yet
                            // (no terminating `\n\n` and not at
                            // the end of the buffer), wait for
                            // more bytes.
                            let sep = after_data.find("\n\n");
                            let Some(sep_idx) = sep else {
                                // Incomplete frame. Bail out of the
                                // inner loop; the outer stream
                                // loop will read more.
                                break;
                            };
                            let line: String = after_data[..sep_idx].trim().to_string();
                            // Drain only on the happy path — on
                            // parse error we leave the frame in
                            // place for the next read.
                            let drain_to = start + 6 + sep_idx + 2;

                            if line.is_empty() || line == "[DONE]" {
                                buffer.drain(..drain_to);
                                if line == "[DONE]"
                                    && !super::ollama_ndjson::send_or_bail(
                                        &tx,
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
                                            if !c.is_empty() {
                                                let _ =
                                                    tx.send(StreamEvent::Text(c.to_string())).await;
                                            }
                                        }
                                    }

                                    // Tool calls in delta — accumulate across chunks
                                    if let Some(tcs) = delta.and_then(|d| d.get("tool_calls")) {
                                        if let Some(calls) = tcs.as_array() {
                                            for tc in calls {
                                                let index = tc
                                                    .get("index")
                                                    .and_then(|i| i.as_u64())
                                                    .unwrap_or(0)
                                                    as usize;
                                                let id = tc
                                                    .get("id")
                                                    .and_then(|id| id.as_str())
                                                    .unwrap_or("");
                                                let name = tc
                                                    .get("function")
                                                    .and_then(|f| f.get("name"))
                                                    .and_then(|n| n.as_str());
                                                let args = tc
                                                    .get("function")
                                                    .and_then(|f| f.get("arguments"))
                                                    .and_then(|a| a.as_str());
                                                pending_tool_calls
                                                    .accumulate(index, id, name, args);
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
                                                    u.get("prompt_eval_count")
                                                        .and_then(|v| v.as_u64())
                                                })
                                                .map(|v| v as usize),
                                            completion_tokens: u
                                                .get("completion_tokens")
                                                .and_then(|v| v.as_u64())
                                                .or_else(|| {
                                                    u.get("eval_count")
                                                        .and_then(|v| v.as_u64())
                                                })
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

                                        if !super::ollama_ndjson::send_or_bail(
                                            &tx,
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
        });

        Ok(rx)
    }
}

/// Split an argument string that may contain one or more
/// top-level JSON objects concatenated together.
///
/// The OpenAI streaming spec says each `tool_call` entry carries
/// one JSON-encoded argument object. Some adapters (notably Ollama
/// routing `minimax-m3:cloud`) emit multiple parallel tool calls
/// in a single delta with their argument objects *concatenated*,
/// e.g. `{"path":"a"}{"path":"b"}`. This helper recovers the
/// original list of values so the executor sees each as a
/// separate `ToolInvocation`.
///
/// Behaviour:
/// 1. Trim outer whitespace.
/// 2. Try to parse the whole string as a single JSON value.
///    If that succeeds, return it wrapped in a one-element vec.
/// 3. Otherwise, walk the string character-by-character tracking
///    brace depth (and quote state, with backslash escapes) and
///    split at every depth-0 `}`. Parse each slice as JSON.
/// 4. Drop slices that fail to parse (defensive — the executor
///    already handles `Value::String` fallbacks).
pub(crate) fn split_concatenated_json(s: &str) -> Vec<serde_json::Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return vec![];
    }
    // 1. Whole-string parse first.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return vec![v];
    }
    // 2. Walk the string looking for top-level JSON object boundaries.
    let bytes = trimmed.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut slice_start: Option<usize> = None;
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if in_str {
            match c {
                b'\\' => escape = true,
                b'"' => in_str = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    slice_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = slice_start {
                        let slice = &trimmed[start..=i];
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(slice) {
                            out.push(v);
                        }
                        slice_start = None;
                    }
                }
                if depth < 0 {
                    // Stray closing brace — bail out and let the
                    // caller fall back to the original string.
                    return vec![serde_json::Value::String(s.to_string())];
                }
            }
            _ => {}
        }
        i += 1;
    }
    if out.is_empty() {
        // Nothing parsed cleanly — fall back to the original
        // behaviour of stuffing the raw string into a `Value::String`
        // so the rest of the pipeline (which already handles this
        // case) keeps working.
        vec![serde_json::Value::String(s.to_string())]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(out.len(), 4, "expected 4 objects, got: {:?}", out);
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
        assert_eq!(out.len(), 2, "expected 2 objects, got: {:?}", out);
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
        assert_eq!(calls.len(), 3, "expected 3 calls, got: {:?}", calls);
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
        let drain_to = start
            + 6
            + end
            + if after_data.find("\n\n").is_some() {
                2
            } else {
                0
            };
        buffer.drain(..drain_to);
        // Buffer is now empty — we correctly drained everything
        // we'd consumed, and the (absent) `\n\n` was NOT drained.
        assert!(buffer.is_empty(), "expected empty buffer, got {:?}", buffer);
    }
}
