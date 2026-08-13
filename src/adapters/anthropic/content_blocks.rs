//! Content-block state machine + tool-use accumulator for the Anthropic stream.
//!
//! Extracted from the SSE parser: handles `content_block_start` /
//! `content_block_delta` events and accumulates `partial_json` tool-input
//! fragments until `content_block_stop` (or stream EOF).

use crate::adapters::ollama_ndjson::send_or_bail;
use crate::shared::{StreamEvent, ToolInvocation};

pub(super) async fn handle_content_block_start(
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
                    let _ = send_or_bail(
                        tx,
                        StreamEvent::Text(t.to_string()),
                        "Anthropic text block start",
                    )
                    .await;
                }
            }
        }
        _ => {
            // Hosted computer_use beta (WO 28.16): the model returns
            // `computer_tool_result` blocks containing screenshots + coordinate
            // actions. The structured payload is opaque to the executor today
            // (the vision loop R4 is deferred); surface a text placeholder so
            // the turn doesn't silently swallow the block. Compiles out when
            // the `computer_use` feature is off.
            #[cfg(feature = "computer_use")]
            if block_type == "computer_tool_result" {
                let summary = summarize_computer_tool_result(block);
                let _ = send_or_bail(
                    tx,
                    StreamEvent::Text(summary),
                    "Anthropic computer_tool_result block",
                )
                .await;
            }
        }
    }
}

pub(super) async fn handle_content_block_delta(
    delta: &serde_json::Value,
    pending: &mut Option<PendingToolUse>,
    tx: &tokio::sync::mpsc::Sender<StreamEvent>,
) {
    if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
        if !t.is_empty() {
            let _ = send_or_bail(
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
            let _ =
                send_or_bail(tx, StreamEvent::Text(t.to_string()), "Anthropic text delta").await;
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
pub(super) struct PendingToolUse {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) input: Option<serde_json::Value>,
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

    pub(super) fn into_invocation(mut self) -> ToolInvocation {
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

/// Reduce a `computer_tool_result` content block to a short text summary.
/// The hosted vision loop (screenshot capture + coordinate-action execution,
/// WO 28.16 R4) is deferred, so for now the structured payload is surfaced as
/// a text placeholder. Feature-gated: compiles out when `computer_use` is off.
#[cfg(feature = "computer_use")]
fn summarize_computer_tool_result(block: &serde_json::Value) -> String {
    // The block carries the model's coordinate action (e.g. click/type/scroll)
    // in `action` and optional screenshot evidence in `image`/`content`. We
    // serialize the action subtree so the turn record stays inspectable.
    if let Some(action) = block.get("action") {
        format!("[computer_tool_result: action={action}]")
    } else {
        // ponytail: ceiling — unknown shape; emit the raw JSON so nothing is
        // silently dropped. Upgrade path: structured TurnEvent variant in R4.
        format!(
            "[computer_tool_result: {}]",
            serde_json::to_string(block).unwrap_or_else(|_| "<unserializable>".into())
        )
    }
}
