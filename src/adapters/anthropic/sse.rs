//! SSE byte-stream framing loop for the Anthropic Messages API.
//!
//! [`parse_anthropic_stream`] drives the raw `data: {...}` SSE frames into
//! canonical [`StreamEvent`]s. The content-block state machine lives in
//! [`super::content_blocks`] and the usage parser in [`super::usage`].

use crate::adapters::ollama_ndjson::send_or_bail;
use crate::adapters::{
    find_subseq, next_chunk_or_idle_timeout, trim_ascii_whitespace, MAX_SSE_BUFFER_BYTES,
};
use crate::shared::{FinishReason, StreamEvent, TokenUsage};

use super::content_blocks::{
    handle_content_block_delta, handle_content_block_start, PendingToolUse,
};
use super::usage::{finalize_usage, merge_usage, parse_usage};

/// Drive an Anthropic Messages API SSE byte stream into `StreamEvent`s.
pub(crate) async fn parse_anthropic_stream<B, E, S>(
    tx: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stream: S,
    idle_timeout: std::time::Duration,
) where
    B: AsRef<[u8]>,
    E: std::fmt::Display,
    S: tokio_stream::Stream<Item = Result<B, E>> + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut pending_tool: Option<PendingToolUse> = None;
    let mut done_emitted = false;
    let mut pending_stop_reason: Option<String> = None;
    // Usage accumulates across frames (WO 38.5): `message_start` carries
    // the input side, `message_delta` the final output_tokens. The real
    // API never puts usage on `message_stop`; if a (non-conforming)
    // server does, that wins at Done time below.
    let mut usage = TokenUsage {
        prompt_tokens: None,
        completion_tokens: None,
        cached_tokens: None,
        cache_write_tokens: None,
    };

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
                            if !send_or_bail(
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
                                if !send_or_bail(
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
                                "message_start" => {
                                    if let Some(u) =
                                        json.get("message").and_then(|m| m.get("usage"))
                                    {
                                        usage = merge_usage(usage, parse_usage(u));
                                    }
                                }
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
                                    if let Some(u) = json.get("usage") {
                                        usage = merge_usage(usage, parse_usage(u));
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
                                    // Flush unconditionally (WO 38.5): a
                                    // tool_use block with no partial_json
                                    // deltas is a zero-argument call, not a
                                    // dropped one — into_invocation maps the
                                    // missing input to an empty object.
                                    if let Some(tool) = pending_tool.take() {
                                        if !send_or_bail(
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
                                    // Usage arrives on message_start/message_delta
                                    // (merged into `usage`); the real API never
                                    // carries it on message_stop, but a
                                    // non-conforming server's value wins.
                                    let stop_usage = match json.get("usage") {
                                        Some(u) => Some(parse_usage(u)),
                                        None => finalize_usage(usage.clone()),
                                    };
                                    if !send_done(&tx, &mut done_emitted, finish_reason, stop_usage)
                                        .await
                                    {
                                        return;
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(e) => {
                            if !send_or_bail(
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
                let _ = send_or_bail(
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
        // EOF without message_stop/[DONE] is truncation, not success
        // (WO 38.5): a channel-close mid-stream must not be laundered
        // into a successful turn. The executor mirrors the cancel path
        // (persist partial, Done{Error}).
        let _ = send_done(
            &tx,
            &mut done_emitted,
            FinishReason::Error,
            finalize_usage(usage.clone()),
        )
        .await;
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
    if send_or_bail(
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
