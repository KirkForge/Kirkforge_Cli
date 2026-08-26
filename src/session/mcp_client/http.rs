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
use crate::adapters::MAX_SSE_BUFFER_BYTES;
use crate::shared::McpServerConfig;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::StreamExt;

fn reqwest_to_io(e: reqwest::Error) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Maximum length of a single SSE `data:` payload accepted from the server.
const MAX_SSE_DATA_LEN: usize = 1 << 20;

/// A client that speaks MCP over streamable-HTTP (SSE + POST).
pub(super) struct McpHttpTransport {
    config: McpServerConfig,
    pending: PendingMap,
    next_id: Arc<tokio::sync::Mutex<u64>>,
    alive: Arc<AtomicBool>,
    /// Channel used to inject outbound requests so the SSE reader task can
    /// keep reading while requests are in flight.
    request_tx: mpsc::UnboundedSender<HttpRequestEnvelope>,
    // reason: held for Drop semantics; read only by #[cfg(test)] disconnect()
    // and the production Drop arm in the parent module.
    _reader_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    _poster_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(super) shutdown_tx: Option<oneshot::Sender<()>>,
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

        let client = crate::shared::build_reqwest_client(Some(Duration::from_secs(60)));

        let alive = Arc::new(AtomicBool::new(true));
        let pending: PendingMap =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let next_id = Arc::new(tokio::sync::Mutex::new(1_u64));

        let sse_url = format!("{base_url}/sse");
        let post_url = format!("{base_url}/messages");

        let (request_tx, mut request_rx) = mpsc::unbounded_channel::<HttpRequestEnvelope>();
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let session_id: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let last_event_id: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));

        // SSE reader task.
        let alive_for_reader = alive.clone();
        let pending_for_reader = pending.clone();
        let client_for_reader = client.clone();
        let session_id_for_reader = session_id.clone();
        let last_event_id_for_reader = last_event_id.clone();
        let reader_task = tokio::spawn(async move {
            let _ = run_sse_reader(
                client_for_reader,
                sse_url,
                pending_for_reader,
                alive_for_reader,
                &mut shutdown_rx,
                session_id_for_reader,
                last_event_id_for_reader,
            )
            .await;
        });

        // Poster task: consumes outbound request envelopes and POSTs them.
        let pending_for_poster = pending.clone();
        let session_id_for_poster = session_id.clone();
        let poster_task = tokio::spawn(async move {
            while let Some(envelope) = request_rx.recv().await {
                let session_id_val = session_id_for_poster.lock().await.clone();
                let resp = post_request(
                    &client,
                    &post_url,
                    &envelope.body,
                    &envelope.id,
                    session_id_val.as_deref(),
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
            pending,
            next_id,
            alive,
            request_tx,
            _reader_task: tokio::sync::Mutex::new(Some(reader_task)),
            _poster_task: tokio::sync::Mutex::new(Some(poster_task)),
            shutdown_tx: Some(shutdown_tx),
        };

        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {"resources": {}, "prompts": {}},
                "clientInfo": {
                    "name": "kf-code",
                    "version": "0.1.0"
                }
            }
        });

        let resp =
            match tokio::time::timeout(super::STARTUP_TIMEOUT, transport.send_request(&init_req))
                .await
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

        super::warn_unsupported_capabilities(&config.name, resp.get("result").unwrap());

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
        tracing::trace!(id = %id, request = %body, "MCP HTTP request");

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

    pub(super) async fn send_notification(&self, notification: &serde_json::Value) {
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
                        message: format!(
                            "MCP tool '{tool_name}' returned a response without a result"
                        ),
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

    pub(super) async fn list_resources(&self) -> Vec<super::McpResource> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "resources/list",
            "params": {}
        });
        let resp = match self.send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(server = %self.config.name, error = %e, "MCP HTTP resources/list failed");
                return vec![];
            }
        };
        let resources = match resp.get("result").and_then(|r| r.get("resources")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };
        resources
            .into_iter()
            .filter_map(|r| {
                let uri = r.get("uri")?.as_str()?.to_string();
                let name = r
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or(&uri)
                    .to_string();
                let description = r
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let mime_type = r
                    .get("mimeType")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string());
                Some(super::McpResource {
                    uri,
                    name,
                    description,
                    mime_type,
                })
            })
            .collect()
    }

    pub(super) async fn read_resource(&self, uri: &str) -> Result<serde_json::Value, McpError> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "resources/read",
            "params": { "uri": uri }
        });
        self.send_request(&req).await
    }

    pub(super) async fn list_prompts(&self) -> Vec<super::McpPrompt> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "prompts/list",
            "params": {}
        });
        let resp = match self.send_request(&req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    server = %self.config.name,
                    error = %e,
                    "MCP HTTP prompts/list failed"
                );
                return vec![];
            }
        };
        let prompts = match resp.get("result").and_then(|r| r.get("prompts")) {
            Some(serde_json::Value::Array(arr)) => arr.clone(),
            _ => return vec![],
        };
        prompts
            .into_iter()
            .filter_map(|p| {
                let name = p.get("name")?.as_str()?.to_string();
                let description = p
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = p
                    .get("arguments")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|arg| {
                                Some(super::McpPromptArg {
                                    name: arg.get("name")?.as_str()?.to_string(),
                                    description: arg
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    required: arg
                                        .get("required")
                                        .and_then(|r| r.as_bool())
                                        .unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(super::McpPrompt {
                    name,
                    description,
                    arguments,
                })
            })
            .collect()
    }

    pub(super) async fn get_prompt(
        &self,
        name: &str,
        args: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let params = match args {
            Some(a) => serde_json::json!({ "name": name, "arguments": a }),
            None => serde_json::json!({ "name": name }),
        };
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "prompts/get",
            "params": params
        });
        self.send_request(&req).await
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
    /// ponytail: production relies on Drop for cleanup; explicit disconnect
    /// is test-only.
    // reason: lifecycle API used by tests; production relies on Drop fallback.
    #[cfg(test)]
    pub(super) async fn disconnect(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        #[allow(unused_must_use)]
        {
            if let Some(t) = self._reader_task.lock().await.take() {
                tokio::time::timeout(Duration::from_secs(2), t).await;
            }
            if let Some(t) = self._poster_task.lock().await.take() {
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
    session_id: Option<&str>,
) -> Result<(), McpError> {
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(body.to_string());
    // WO 10.7: send Mcp-Session-Id on every POST when the server
    // provided one (MCP streamable-HTTP spec, 2025-06-18 §Session
    // Management). Backward-compatible: if the server never sent a
    // session id, the header is omitted (some servers reject unknown
    // headers).
    if let Some(sid) = session_id {
        request = request.header("mcp-session-id", sid);
    }
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
    session_id: Arc<tokio::sync::Mutex<Option<String>>>,
    last_event_id: Arc<tokio::sync::Mutex<Option<String>>>,
) {
    // Reconnect-with-backoff loop (WO 10.7). When the SSE stream drops,
    // reconnect with the session id + Last-Event-ID header so the server
    // can resume. Backoff: 1s, 2s, 5s, 10s, 30s, max 5 retries.
    const BACKOFF_SCHEDULE: &[u64] = &[1, 2, 5, 10, 30];
    const MAX_RETRIES: usize = 5;
    let mut attempt: usize = 0;
    loop {
        let last_eid = last_event_id.lock().await.clone();
        let (stream, header_session_id) = match open_sse_stream(&client, &url, last_eid.as_deref())
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(url = %url, error = %e, attempt, "failed to open MCP SSE stream");
                if attempt >= MAX_RETRIES {
                    McpClient::fail_all_pending(pending.clone()).await;
                    alive.store(false, Ordering::SeqCst);
                    return;
                }
                let delay = BACKOFF_SCHEDULE[attempt.min(BACKOFF_SCHEDULE.len() - 1)];
                tokio::select! {
                    _ = &mut *shutdown => return,
                    _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                }
                attempt += 1;
                continue;
            }
        };

        // The server MAY send Mcp-Session-Id on the initial GET response
        // (new streamable-HTTP transport) or via the `endpoint` event
        // (old HTTP+SSE transport, as a URL query param). Capture the
        // header-derived id here; the `endpoint` event handler below
        // overwrites it if the server provides one in the URL.
        if let Some(hid) = header_session_id {
            let mut guard = session_id.lock().await;
            if guard.is_none() {
                *guard = Some(hid);
            }
        }

        attempt = 0; // reset backoff after a successful connect
        let mut stream = stream;
        let mut buffer: Vec<u8> = Vec::new();
        // SSE event fields accumulate across lines until a blank-line
        // dispatch. The old transport's `endpoint` event carries the
        // POST URL (and optionally a session_id query param); `id:`
        // lines carry the SSE event id for resumability.
        let mut current_event_type: Option<String> = None;
        let mut current_data: Vec<String> = Vec::new();
        let mut current_id: Option<String> = None;

        loop {
            let chunk_result = tokio::select! {
                biased;
                _ = &mut *shutdown => {
                    tracing::trace!("MCP HTTP reader shutting down");
                    McpClient::fail_all_pending(pending.clone()).await;
                    alive.store(false, Ordering::SeqCst);
                    return;
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
                    tracing::trace!(url = %url, "MCP SSE stream closed");
                    break;
                }
            };

            buffer.extend_from_slice(&bytes);
            if buffer.len() > MAX_SSE_BUFFER_BYTES {
                tracing::warn!("MCP SSE buffer exceeded limit; disconnecting");
                break;
            }

            // Parse SSE frames. The SSE spec defines frames as
            // `field: value\n` lines terminated by a blank line. We
            // parse line-by-line so we can capture `event:`, `id:`, and
            // `data:` fields, then dispatch on the blank-line boundary.
            loop {
                let Some(nl) = find_subseq(&buffer, b"\n") else {
                    break; // incomplete line, wait for more bytes
                };
                let line_bytes: Vec<u8> = buffer.drain(..nl + 1).collect();
                let line = String::from_utf8_lossy(&line_bytes).into_owned();
                let line = line.trim_end_matches('\r');

                if line.is_empty() {
                    // Blank line: dispatch the accumulated event.
                    if !current_data.is_empty() || current_event_type.is_some() {
                        let data = current_data.join("\n");
                        current_data.clear();
                        let ev_type = current_event_type.take();
                        let ev_id = current_id.take();

                        // Track the last event id for resumability.
                        if let Some(ref id) = ev_id {
                            *last_event_id.lock().await = Some(id.clone());
                        }

                        // Handle the `endpoint` event (old transport):
                        // extract session_id from the URL query param.
                        if ev_type.as_deref() == Some("endpoint") {
                            if let Some(sid) = parse_session_id_from_url(&data) {
                                *session_id.lock().await = Some(sid);
                            }
                            // The endpoint URL itself is not needed —
                            // we POST to the fixed /messages path.
                            continue;
                        }

                        if data.is_empty() {
                            continue;
                        }
                        if data.len() > MAX_SSE_DATA_LEN {
                            tracing::warn!("MCP SSE data frame exceeded maximum length");
                            continue;
                        }

                        if data == "[DONE]" {
                            McpClient::fail_all_pending(pending.clone()).await;
                            alive.store(false, Ordering::SeqCst);
                            return;
                        }

                        let Ok(resp) = serde_json::from_str::<serde_json::Value>(&data) else {
                            tracing::trace!(line = %data, "MCP SSE non-JSON data line");
                            continue;
                        };

                        let Some(id) = resp.get("id").and_then(json_id_to_string) else {
                            tracing::trace!(response = %resp, "MCP SSE notification without id");
                            continue;
                        };

                        McpClient::dispatch_response(id, resp, &pending, "http").await;
                    }
                    continue;
                }

                // Parse `field: value` (SSE spec). A line starting with
                // `:` is a comment. A line without `:` is a field with
                // an empty value (ignore).
                if line.starts_with(':') {
                    continue;
                }
                let (field, value) = if let Some(colon) = line.find(':') {
                    let f = &line[..colon];
                    let v = line[colon + 1..]
                        .strip_prefix(' ')
                        .unwrap_or(&line[colon + 1..]);
                    (f, v)
                } else {
                    (line, "")
                };

                match field {
                    "event" => current_event_type = Some(value.to_string()),
                    "data" => current_data.push(value.to_string()),
                    "id" => current_id = Some(value.to_string()),
                    _ => {} // retry, etc. — ignored
                }
            }
        }

        // The stream dropped. Reconnect if we haven't been shut down and
        // haven't exhausted retries. The backoff schedule resets to 0 on
        // a successful connect (above), so a long-lived session that
        // occasionally drops reconnects quickly.
        if shutdown.try_recv().is_ok() {
            McpClient::fail_all_pending(pending.clone()).await;
            alive.store(false, Ordering::SeqCst);
            return;
        }
        if attempt >= MAX_RETRIES {
            tracing::warn!(url = %url, attempts = attempt, "MCP SSE reconnect attempts exhausted");
            McpClient::fail_all_pending(pending.clone()).await;
            alive.store(false, Ordering::SeqCst);
            return;
        }
        let delay = BACKOFF_SCHEDULE[attempt.min(BACKOFF_SCHEDULE.len() - 1)];
        tracing::info!(url = %url, attempt, delay_secs = delay, "MCP SSE reconnecting with backoff");
        tokio::select! {
            _ = &mut *shutdown => {
                McpClient::fail_all_pending(pending.clone()).await;
                alive.store(false, Ordering::SeqCst);
                return;
            }
            _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
        }
        attempt += 1;
    }
}

/// Extract a `session_id` query param from a URL string (the `endpoint`
/// SSE event's data payload in the old HTTP+SSE transport). Returns
/// `None` if the URL has no `session_id` param.
fn parse_session_id_from_url(url: &str) -> Option<String> {
    let q = url.split_once('?').map(|(_, q)| q)?;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "session_id" || k == "sessionId" {
            return Some(v.to_string());
        }
    }
    None
}

type SseStream = Box<dyn tokio_stream::Stream<Item = Result<Vec<u8>, McpError>> + Unpin + Send>;

async fn open_sse_stream(
    client: &reqwest::Client,
    url: &str,
    last_event_id: Option<&str>,
) -> Result<(SseStream, Option<String>), McpError> {
    let mut req = client.get(url).header("accept", "text/event-stream");
    // WO 10.7: send Last-Event-ID on reconnect so the server can replay
    // missed events (SSE spec §resumability; MCP streamable-HTTP spec
    // 2025-06-18 §Resumability and Redelivery).
    if let Some(eid) = last_event_id {
        req = req.header("last-event-id", eid);
    }
    let resp = req.send().await.map_err(reqwest_to_io)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(url = %url, status = %status, body = %body, "MCP SSE endpoint returned error");
        return Err(McpError::JsonRpc {
            code: status.as_u16() as i64,
            message: body,
        });
    }
    // Capture the Mcp-Session-Id header if the server provides one
    // (new streamable-HTTP transport). The old transport provides it
    // via the `endpoint` SSE event's URL query param instead.
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let stream = resp.bytes_stream().map(|res| match res {
        Ok(b) => Ok(b.to_vec()),
        Err(e) => Err(McpError::Io(reqwest_to_io(e))),
    });
    Ok((Box::new(Box::pin(stream)), session_id))
}

fn find_subseq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
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
        assert!(
            matches!(outcome, crate::shared::ToolOutcome::Success { content } if content == "Hello world")
        );
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

    // ── WO 10.7: session-id parsing ──

    #[test]
    fn parse_session_id_from_url_extracts_query_param() {
        assert_eq!(
            parse_session_id_from_url("https://example.com/messages?session_id=abc123"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_extracts_camel_case_param() {
        assert_eq!(
            parse_session_id_from_url("https://example.com/messages?sessionId=xyz"),
            Some("xyz".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_returns_none_without_param() {
        assert_eq!(
            parse_session_id_from_url("https://example.com/messages"),
            None
        );
        assert_eq!(
            parse_session_id_from_url("https://example.com/messages?foo=bar"),
            None
        );
    }

    // ── WO 10.7: Mcp-Session-Id header sent on POST ──
    //
    // These tests spin up a tiny one-shot HTTP server that captures the
    // request headers and returns 200, so we can assert the poster adds
    // the header when a session id is known (and omits it when not).

    /// A minimal HTTP server that accepts one connection, captures the
    /// `Mcp-Session-Id` header from the POST request, and returns 200.
    async fn mock_post_server() -> (String, tokio::sync::oneshot::Receiver<Option<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/messages");
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);

            // Parse the Mcp-Session-Id header from the request.
            let session_id = request.lines().find_map(|line| {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("mcp-session-id:") {
                    Some(line["mcp-session-id:".len()..].trim().to_string())
                } else {
                    None
                }
            });

            let _ = tx.send(session_id);

            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
            sock.write_all(response.as_bytes()).await.unwrap();
        });

        (url, rx)
    }

    #[tokio::test]
    async fn post_request_sends_session_id_header_when_provided() {
        let (url, rx) = mock_post_server().await;
        let client = reqwest::Client::new();
        post_request(&client, &url, "{}", "1", Some("test-session-42"))
            .await
            .unwrap();
        let captured = rx.await.unwrap();
        assert_eq!(captured, Some("test-session-42".to_string()));
    }

    #[tokio::test]
    async fn post_request_omits_session_id_header_when_none() {
        let (url, rx) = mock_post_server().await;
        let client = reqwest::Client::new();
        post_request(&client, &url, "{}", "1", None).await.unwrap();
        let captured = rx.await.unwrap();
        assert_eq!(
            captured, None,
            "header must be omitted when session id is None"
        );
    }

    // ── WO 10.7: Last-Event-ID header sent on reconnect ──

    /// A minimal SSE server that captures the `Last-Event-ID` header
    /// from the GET request, then closes the stream immediately so the
    /// reader reconnects.
    async fn mock_sse_server_last_event_id(
    ) -> (String, tokio::sync::oneshot::Receiver<Option<String>>) {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/sse");
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);

            let last_event_id = request.lines().find_map(|line| {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("last-event-id:") {
                    Some(line["last-event-id:".len()..].trim().to_string())
                } else {
                    None
                }
            });

            let _ = tx.send(last_event_id);

            // Return a minimal SSE response then close.
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
            sock.write_all(response.as_bytes()).await.unwrap();
            // Drop the socket to close the stream.
        });

        (url, rx)
    }

    #[tokio::test]
    async fn open_sse_stream_sends_last_event_id_header_on_reconnect() {
        let (url, rx) = mock_sse_server_last_event_id().await;
        let client = reqwest::Client::new();
        // Simulate a reconnect: pass a last_event_id.
        let (_stream, _sid) = open_sse_stream(&client, &url, Some("event-99"))
            .await
            .unwrap();
        let captured = rx.await.unwrap();
        assert_eq!(
            captured,
            Some("event-99".to_string()),
            "Last-Event-ID header must be sent when reconnecting"
        );
    }

    #[tokio::test]
    async fn open_sse_stream_omits_last_event_id_header_on_first_connect() {
        let (url, rx) = mock_sse_server_last_event_id().await;
        let client = reqwest::Client::new();
        let (_stream, _sid) = open_sse_stream(&client, &url, None).await.unwrap();
        let captured = rx.await.unwrap();
        assert_eq!(
            captured, None,
            "Last-Event-ID header must be omitted on first connect"
        );
    }

    // ── WO 10.7: Mcp-Session-Id captured from GET response header ──

    /// A minimal SSE server that returns an `Mcp-Session-Id` header on
    /// the initial GET response, then closes the stream.
    async fn mock_sse_server_with_session_id(session_id: &str) -> String {
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/sse");
        let sid = session_id.to_string();

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: {sid}\r\n\r\n"
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            // Hold the socket open briefly so reqwest can read the response
            // headers before the stream closes. On Windows, dropping the
            // socket immediately after write_all races the client's read —
            // the close propagates faster than the headers, causing a
            // connection-reset error. This is a cross-library read race, not
            // a test-sync sleep; 20ms is the empirically-shortest stable
            // duration (down from 100ms). The `sock` drops at end of block.
            // ponytail: a oneshot wired through the mock would be fully
            // deterministic but doubles the helper's surface for a Windows-
            // only race; revisit if this flakes again.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = sock;
        });

        url
    }

    #[tokio::test]
    async fn open_sse_stream_captures_session_id_from_response_header() {
        let url = mock_sse_server_with_session_id("header-session-7").await;
        let client = reqwest::Client::new();
        let (_stream, session_id) = open_sse_stream(&client, &url, None).await.unwrap();
        assert_eq!(
            session_id,
            Some("header-session-7".to_string()),
            "Mcp-Session-Id header must be captured from the GET response"
        );
    }

    #[tokio::test]
    async fn open_sse_stream_returns_none_session_id_when_absent() {
        let (url, _rx) = mock_sse_server_last_event_id().await;
        let client = reqwest::Client::new();
        let (_stream, session_id) = open_sse_stream(&client, &url, None).await.unwrap();
        assert_eq!(session_id, None, "no session id when header is absent");
    }

    #[test]
    fn find_subseq_finds_needle_at_start() {
        assert_eq!(find_subseq(b"hello world", b"hello"), Some(0));
    }

    #[test]
    fn find_subseq_finds_needle_in_middle() {
        assert_eq!(find_subseq(b"hello world", b"lo wo"), Some(3));
    }

    #[test]
    fn find_subseq_finds_needle_at_end() {
        assert_eq!(find_subseq(b"hello world", b"world"), Some(6));
    }

    #[test]
    fn find_subseq_returns_none_for_missing_needle() {
        assert_eq!(find_subseq(b"hello", b"xyz"), None);
    }

    #[test]
    fn find_subseq_returns_none_when_needle_longer_than_haystack() {
        assert_eq!(find_subseq(b"ab", b"abcd"), None);
    }

    #[test]
    fn find_subseq_finds_single_byte_needle() {
        assert_eq!(find_subseq(b"abc", b"b"), Some(1));
    }

    #[test]
    fn find_subseq_finds_exact_match_full_string() {
        assert_eq!(find_subseq(b"abcd", b"abcd"), Some(0));
    }

    #[test]
    fn find_subseq_finds_first_occurrence() {
        assert_eq!(find_subseq(b"ababab", b"ab"), Some(0));
    }

    #[test]
    fn parse_session_id_from_url_handles_multiple_params() {
        assert_eq!(
            parse_session_id_from_url("https://x/m?foo=1&session_id=abc&bar=2"),
            Some("abc".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_handles_trailing_amp() {
        assert_eq!(
            parse_session_id_from_url("https://x/m?session_id=abc&"),
            Some("abc".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_handles_empty_value() {
        assert_eq!(
            parse_session_id_from_url("https://x/m?session_id="),
            Some("".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_returns_none_for_valueless_param() {
        assert_eq!(parse_session_id_from_url("https://x/m?session_id"), None);
    }

    #[test]
    fn parse_session_id_from_url_returns_none_for_empty_url() {
        assert_eq!(parse_session_id_from_url(""), None);
    }

    #[test]
    fn parse_session_id_from_url_picks_first_matching_key() {
        assert_eq!(
            parse_session_id_from_url("https://x/m?session_id=first&session_id=second"),
            Some("first".to_string())
        );
    }

    #[test]
    fn parse_session_id_from_url_handles_question_mark_only() {
        assert_eq!(parse_session_id_from_url("https://x/m?"), None);
    }

    #[test]
    fn tool_result_from_content_returns_serialized_for_no_content_field() {
        let result = json!({"other": "value"});
        let outcome = McpHttpTransport::tool_result_from_content(&result, "tool");
        match outcome {
            crate::shared::ToolOutcome::Success { content } => {
                assert!(content.contains("other"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_from_content_returns_serialized_for_empty_content_array() {
        let result = json!({"content": []});
        let outcome = McpHttpTransport::tool_result_from_content(&result, "tool");
        match outcome {
            crate::shared::ToolOutcome::Success { content } => {
                assert!(content.contains("content"), "got: {content}");
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_from_content_joins_multiple_text_blocks() {
        let result = json!({
            "content": [
                {"type": "text", "text": "a"},
                {"type": "text", "text": "b"},
                {"type": "text", "text": "c"},
            ]
        });
        let outcome = McpHttpTransport::tool_result_from_content(&result, "tool");
        match outcome {
            crate::shared::ToolOutcome::Success { content } => assert_eq!(content, "abc"),
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn tool_result_from_content_falls_back_when_pretty_fails() {
        let result = json!({"content": [{"type": "image"}]});
        let outcome = McpHttpTransport::tool_result_from_content(&result, "tool-name");
        match outcome {
            crate::shared::ToolOutcome::Success { content } => {
                assert!(
                    content.contains("image") || content.contains("tool-name"),
                    "got: {content}"
                );
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[test]
    fn reqwest_to_io_wraps_error_message() {
        let err = reqwest::Client::builder()
            .build()
            .unwrap()
            .get("ht!tp://invalid url with spaces")
            .send();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(err);
        if let Err(e) = result {
            let io_err = reqwest_to_io(e);
            assert!(!io_err.to_string().is_empty());
        }
    }
}
