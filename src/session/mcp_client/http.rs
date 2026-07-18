//! Streamable-HTTP transport for the MCP client.
//!
//! Implements the Model Context Protocol HTTP transport as described in
//! MCP 2024-11-05 and later drafts:
//!   - GET the SSE endpoint to open an event stream.
//!   - POST JSON-RPC requests to the messages endpoint.
//!   - Route inbound SSE `message` events back to the caller by JSON-RPC id.
//!
//! The public API (`McpHttpTransport`) intentionally mirrors the stdio
//! transport (`McpClient`) so the manager can use either without knowing
//! which transport is underneath.

use super::error::McpError;
use super::{json_id_to_string, McpClient, PendingMap, REQUEST_TIMEOUT};
use crate::shared::McpServerConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;

fn reqwest_to_io(e: reqwest::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Maximum bytes the SSE parser will accumulate while waiting for a complete
/// `data: ...\n\n` frame.
const MAX_SSE_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Maximum length of a single SSE `data:` payload accepted from the server.
const MAX_SSE_DATA_LEN: usize = 1 << 20;

/// A client that speaks MCP over streamable-HTTP (SSE + POST).
pub(super) struct McpHttpTransport {
    config: McpServerConfig,
    client: reqwest::Client,
    base_url: String,
    pending: PendingMap,
    next_id: Arc<tokio::sync::Mutex<u64>>,
    alive: Arc<AtomicBool>,
    /// Channel used to inject outbound requests so the SSE reader task can
    /// keep reading while requests are in flight.
    request_tx: mpsc::UnboundedSender<HttpRequestEnvelope>,
    reader_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    poster_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

struct HttpRequestEnvelope {
    body: String,
    id: String,
}

impl McpHttpTransport {
    /// Open a streamable-HTTP session to `config.url` and perform the MCP
    /// initialize handshake. Returns `None` if the connection or handshake
    /// fails.
    pub(super) async fn connect(config: &McpServerConfig) -> Option<Self> {
        let base_url = config.url.trim_end_matches('/').to_string();
        if base_url.is_empty() {
            tracing::warn!(server = %config.name, "MCP HTTP transport configured without url");
            return None;
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .tcp_nodelay(true)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to build custom MCP HTTP client; falling back");
                reqwest::Client::new()
            });

        let alive = Arc::new(AtomicBool::new(true));
        let pending: PendingMap = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let next_id = Arc::new(tokio::sync::Mutex::new(1_u64));

        let sse_url = format!("{base_url}/sse");
        let post_url = format!("{base_url}/messages");

        let (request_tx, mut request_rx) = mpsc::unbounded_channel::<HttpRequestEnvelope>();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        // SSE reader task.
        let alive_for_reader = alive.clone();
        let pending_for_reader = pending.clone();
        let client_for_reader = client.clone();
        let reader_task = tokio::spawn(async move {
            let _ = run_sse_reader(
                client_for_reader,
                sse_url,
                pending_for_reader,
                alive_for_reader,
                &mut shutdown_rx,
            )
            .await;
        });

        // Poster task: consumes outbound request envelopes and POSTs them.
        let client_for_poster = client.clone();
        let pending_for_poster = pending.clone();
        let poster_task = tokio::spawn(async move {
            while let Some(envelope) = request_rx.recv().await {
                let resp = post_request(
                    &client_for_poster,
                    &post_url,
                    &envelope.body,
                    &envelope.id,
                )
                .await;
                // The SSE reader will route the real response; the POST
                // response itself is only a transport acknowledgment. We
                // still surface POST-level errors immediately so callers
                // don't wait the full REQUEST_TIMEOUT for nothing.
                if let Err(e) = resp {
                    // The pending map holds the response sender for this id.
                    // Remove it and report the transport failure.
                    if let Some(tx) = pending_for_poster.lock().await.remove(&envelope.id) {
                        let _ = tx.send(Err(e));
                    }
                }
            }
        });

        let transport = Self {
            config: config.clone(),
            client,
            base_url,
            pending,
            next_id,
            alive,
            request_tx,
            reader_task: tokio::sync::Mutex::new(Some(reader_task)),
            poster_task: tokio::sync::Mutex::new(Some(poster_task)),
            shutdown_tx: Some(shutdown_tx),
        };

        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "kirkforge",
                    "version": "0.1.0"
                }
            }
        });

        let resp = match tokio::time::timeout(super::STARTUP_TIMEOUT, transport.send_request(&init_req)).await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(server = %config.name, error = %e, "MCP HTTP initialize failed");
                return None;
            }
            Err(_) => {
                tracing::warn!(server = %config.name, "MCP HTTP initialize timed out");
                return None;
            }
        };
        if resp.get("result").is_none() {
            tracing::warn!(server = %config.name, response = %resp, "MCP HTTP initialize response missing result");
            return None;
        }

        transport
            .send_notification(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .await;

        Some(transport)
    }

    pub(super) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub(super) async fn send_request(
        &self,
        req: &serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        if !self.is_alive() {
            return Err(McpError::Disconnected);
        }

        let id_num = {
            let mut guard = self.next_id.lock().await;
            let id = *guard;
            *guard += 1;
            id
        };
        let id = id_num.to_string();

        let mut req_with_id = req.clone();
        if let Some(obj) = req_with_id.as_object_mut() {
            obj.insert("id".to_string(), serde_json::json!(id_num));
        }
        let body = serde_json::to_string(&req_with_id)
            .map_err(|e| McpError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)))?;
        tracing::debug!(id = %id, request = %body, "MCP HTTP request");

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        if self
            .request_tx
            .send(HttpRequestEnvelope {
                body,
                id: id.clone(),
            })
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(McpError::Disconnected);
        }

        // Wait for the SSE reader to route the real response. POST-level
        // failures are surfaced by removing the pending waiter in the poster
        // task, which closes this channel and yields ChannelClosed.
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::ChannelClosed)
            }
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(McpError::Timeout)
            }
        }
    }

    pub(super) async fn send_notification(&self,
        notification: &serde_json::Value,
    ) {
        if !self.is_alive() {
            return;
        }
        let body = match serde_json::to_string(notification) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to serialize MCP HTTP notification");
                return;
            }
        };
        // Notifications have no response; the SSE reader ignores them.
        let id = "notify".to_string();
        let _ = self.request_tx.send(HttpRequestEnvelope { body, id });
    }

    pub(super) async fn list_tools(&self) -> Vec<super::McpToolDef> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/list",
            "params": {}
        });
        let resp = match self.send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %self.config.name, error = %e, "MCP HTTP tools/list failed");
                return vec![];
            }
        };
        let tools = match resp.get("result").and_then(|r| r.get("tools")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };

        tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                let description = t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                Some(super::McpToolDef {
                    name,
                    description,
                    parameters,
                })
            })
            .collect()
    }

    pub(super) async fn call_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> crate::shared::ToolOutcome {
        use crate::shared::{ToolError, ToolOutcome};
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": args,
            }
        });
        match self.send_request(&req).await {
            Ok(resp) => {
                let Some(result) = resp.get("result") else {
                    return ToolOutcome::Failure(ToolError::Internal {
                        message: format!("MCP tool '{tool_name}' returned a response without a result"),
                    });
                };
                Self::tool_result_from_content(result, tool_name)
            }
            Err(e) => match e {
                McpError::Timeout => ToolOutcome::Failure(ToolError::Timeout {
                    after_secs: REQUEST_TIMEOUT.as_secs(),
                }),
                _ => ToolOutcome::Failure(ToolError::Internal {
                    message: format!("MCP tool '{tool_name}' failed: {e}"),
                }),
            },
        }
    }

    fn tool_result_from_content(
        result: &serde_json::Value,
        tool_name: &str,
    ) -> crate::shared::ToolOutcome {
        use crate::shared::ToolOutcome;
        if let Some(content_blocks) = result.get("content").and_then(|c| c.as_array()) {
            let text_parts: Vec<String> = content_blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            if text_parts.is_empty() {
                ToolOutcome::Success {
                    content: serde_json::to_string_pretty(result).unwrap_or_default(),
                }
            } else {
                ToolOutcome::Success {
                    content: text_parts.join(""),
                }
            }
        } else {
            ToolOutcome::Success {
                content: serde_json::to_string_pretty(result)
                    .unwrap_or_else(|_| format!("MCP tool '{tool_name}' returned non-JSON result")),
            }
        }
    }

    /// Gracefully disconnect.
    pub(super) async fn disconnect(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        #[allow(unused_must_use)]
        {
            if let Some(t) = self.reader_task.lock().await.take() {
                tokio::time::timeout(Duration::from_secs(2), t).await;
            }
            if let Some(t) = self.poster_task.lock().await.take() {
                tokio::time::timeout(Duration::from_secs(2), t).await;
            }
        }
    }
}

async fn post_request(
    client: &reqwest::Client,
    url: &str,
    body: &str,
    id: &str,
) -> Result<(), McpError> {
    let request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(body.to_string());
    // The SSE endpoint returns the endpoint URL via an `endpoint` event and
    // the spec recommends sending an `Mcp-Session-Id` header once it is known.
    // We do not yet track session ids in this minimal implementation; the server
    // is expected to route by the open SSE connection.
    let resp = match tokio::time::timeout(REQUEST_TIMEOUT, request.send()).await {
        Ok(Ok(r)) => r,
                Ok(Err(e)) => return Err(McpError::Io(reqwest_to_io(e))),
        Err(_) => {
            tracing::warn!(id = %id, "MCP HTTP POST timed out");
            return Err(McpError::Timeout);
        }
    };

    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    tracing::warn!(id = %id, status = %status, body = %body, "MCP HTTP POST returned error");
    Err(McpError::JsonRpc {
        code: status.as_u16() as i64,
        message: body,
    })
}

async fn run_sse_reader(
    client: reqwest::Client,
    url: String,
    pending: PendingMap,
    alive: Arc<AtomicBool>,
    shutdown: &mut oneshot::Receiver<()>,
) {
    let mut buffer: Vec<u8> = Vec::new();
    let stream = match open_sse_stream(&client,
        &url,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(url = %url, error = %e, "failed to open MCP SSE stream");
            McpClient::fail_all_pending(pending).await;
            alive.store(false, Ordering::SeqCst);
            return;
        }
    };

    let mut stream = stream;
    loop {
        let chunk_result = tokio::select! {
            biased;
            _ = &mut *shutdown => {
                tracing::debug!("MCP HTTP reader shutting down");
                break;
            }
            result = stream.next() => result,
        };

        let bytes = match chunk_result {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                tracing::warn!(url = %url, error = %e, "MCP SSE stream error");
                break;
            }
            None => {
                tracing::debug!(url = %url, "MCP SSE stream closed");
                break;
            }
        };

        buffer.extend_from_slice(&bytes);
        if buffer.len() > MAX_SSE_BUFFER_BYTES {
            tracing::warn!("MCP SSE buffer exceeded limit; disconnecting");
            break;
        }

        while let Some(start) = find_subseq(&buffer, b"data: ") {
            let after_start = start + 6;
            let after = &buffer[after_start..];
            let sep = [b"\n\n".as_slice(), b"\r\n\r\n".as_slice(), b"\r\r".as_slice()]
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
            if payload.len() > MAX_SSE_DATA_LEN {
                tracing::warn!("MCP SSE data frame exceeded maximum length");
                continue;
            }

            let line = match std::str::from_utf8(&payload) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("MCP SSE frame is not valid UTF-8: {e}");
                    continue;
                }
            };

            if line == "[DONE]" {
                break;
            }

            let Ok(resp) = serde_json::from_str::<serde_json::Value>(line) else {
                tracing::debug!(line = %line, "MCP SSE non-JSON data line");
                continue;
            };

            let Some(id) = resp.get("id").and_then(json_id_to_string) else {
                tracing::debug!(response = %resp, "MCP SSE notification without id");
                continue;
            };

            McpClient::dispatch_response(id, resp, &pending, "http").await;
        }
    }

    McpClient::fail_all_pending(pending).await;
    alive.store(false, Ordering::SeqCst);
}

type SseStream = Box<dyn tokio_stream::Stream<Item = Result<Vec<u8>, McpError>> + Unpin + Send>;

async fn open_sse_stream(
    client: &reqwest::Client,
    url: &str,
) -> Result<SseStream, McpError> {
    let resp = client
        .get(url)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(reqwest_to_io)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(url = %url, status = %status, body = %body, "MCP SSE endpoint returned error");
        return Err(McpError::JsonRpc {
            code: status.as_u16() as i64,
            message: body,
        });
    }

    let stream = resp.bytes_stream().map(|res| match res {
        Ok(b) => Ok(b.to_vec()),
        Err(e) => Err(McpError::Io(reqwest_to_io(e))),
    });
    Ok(Box::new(Box::pin(stream)))
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

    #[test]
    fn tool_result_from_text_blocks() {
        let result = json!({
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "world"},
            ]
        });
        let outcome = McpHttpTransport::tool_result_from_content(&result, "test");
        assert!(matches!(outcome, crate::shared::ToolOutcome::Success { content } if content == "Hello world"));
    }

    #[test]
    fn tool_result_from_empty_text_blocks_serializes_result() {
        let result = json!({"content": [{"type": "image", "mime": "image/png"}]});
        let outcome = McpHttpTransport::tool_result_from_content(&result, "test");
        assert!(
            matches!(outcome, crate::shared::ToolOutcome::Success { content } if content.contains("image")),
            "expected serialized result for non-text block"
        );
    }
}
