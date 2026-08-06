//! Mock provider server for e2e tests.
//!
//! Serves three provider dialects from one `wiremock` instance:
//!   - Ollama `/api/chat` — NDJSON streaming
//!   - OpenAI-compat `/v1/chat/completions` — SSE streaming
//!   - Anthropic `/v1/messages` — SSE streaming with `event:` lines
//!
//! Each dialect reads scripted replies from a queue and records every
//! request for wire assertions.  An `http_error` injector lets tests
//! exercise the retry classifier.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

// ── Reply types ──────────────────────────────────────────────────────

/// A scripted reply for one model turn.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The text content the model should emit.
    pub content: String,
    /// If set, emit a tool-call block with this name and JSON args.
    pub tool_call: Option<ToolCall>,
    /// If true, mark this chunk as the final done message.
    pub done: bool,
    /// Optional thinking text (for models that support extended thinking).
    pub thinking: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

impl Reply {
    /// Simple text reply, marked as done.
    pub fn text(content: &str) -> Self {
        Self {
            content: content.to_string(),
            tool_call: None,
            done: true,
            thinking: None,
        }
    }

    /// Reply that asks for a tool call, then is done.
    pub fn tool(name: &str, arguments: serde_json::Value) -> Self {
        Self {
            content: String::new(),
            tool_call: Some(ToolCall {
                name: name.to_string(),
                arguments,
            }),
            done: true,
            thinking: None,
        }
    }

    /// Inject a thinking block before content.
    #[allow(dead_code)]
    pub fn with_thinking(mut self, thinking: &str) -> Self {
        self.thinking = Some(thinking.to_string());
        self
    }
}

/// An HTTP error to inject at a specific request index.
#[derive(Debug, Clone)]
pub struct HttpError {
    pub status_code: u16,
}

// ── Recorded request ─────────────────────────────────────────────────

/// A recorded request to the mock server.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub body: serde_json::Value,
}

// ── Mock server state ─────────────────────────────────────────────────

/// Shared state behind the mock server.  Tests push replies and error
/// injectors; the response handler pops them.
#[derive(Debug)]
struct MockState {
    /// Queue of replies per request.  The handler pops the front each
    /// time it serves a request.
    replies: VecDeque<Reply>,
    /// If set, return an HTTP error instead of a normal reply for the
    /// Nth request (0-indexed).
    http_errors: Vec<(usize, HttpError)>,
    /// Every request received, in order.
    request_log: Vec<RecordedRequest>,
}

impl MockState {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: replies.into(),
            http_errors: Vec::new(),
            request_log: Vec::new(),
        }
    }
}

/// The in-test mock provider server.
pub struct MockProvider {
    pub server: MockServer,
    state: Arc<Mutex<MockState>>,
}

impl MockProvider {
    /// Start the mock server with the given scripted replies.
    pub async fn start(replies: Vec<Reply>) -> Self {
        let state = Arc::new(Mutex::new(MockState::new(replies)));
        let server = MockServer::start().await;

        // Mount catch-all responders for each dialect.
        // Ollama /api/chat
        let state_ollama = state.clone();
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(move |req: &wiremock::Request| {
                let state = state_ollama.clone();
                respond_ollama(req, &state)
            })
            .mount(&server)
            .await;

        // OpenAI-compat /v1/chat/completions
        let state_oai = state.clone();
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(move |req: &wiremock::Request| {
                let state = state_oai.clone();
                respond_openai_compat(req, &state)
            })
            .mount(&server)
            .await;

        // Anthropic /v1/messages
        let state_anth = state.clone();
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(move |req: &wiremock::Request| {
                let state = state_anth.clone();
                respond_anthropic(req, &state)
            })
            .mount(&server)
            .await;

        // Ollama /api/tags — the TUI polls this on startup to verify
        // connectivity and list available models.  Return a minimal
        // response so the TUI doesn't show a connection error.
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [{"name": "e2e-test-model", "size": 0}]
            })))
            .mount(&server)
            .await;

        Self { server, state }
    }

    /// The base URL for pointing the binary at this mock.
    pub fn url(&self) -> String {
        self.server.uri()
    }

    /// Record an HTTP error to inject at the Nth request (0-indexed).
    pub fn inject_error(&self, request_index: usize, error: HttpError) {
        let mut state = self.state.lock().expect("mock state lock");
        state.http_errors.push((request_index, error));
    }

    /// Return a snapshot of all recorded requests.
    pub fn request_log(&self) -> Vec<RecordedRequest> {
        self.state
            .lock()
            .expect("mock state lock")
            .request_log
            .clone()
    }
}

// ── Ollama NDJSON response ───────────────────────────────────────────

fn respond_ollama(req: &wiremock::Request, state: &Arc<Mutex<MockState>>) -> ResponseTemplate {
    let mut state = state.lock().expect("mock state lock");
    let request_index = state.request_log.len();
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
    state.request_log.push(RecordedRequest {
        method: req.method.to_string(),
        path: req.url.path().to_string(),
        body,
    });

    // Check for an injected error first.
    if let Some(pos) = state
        .http_errors
        .iter()
        .position(|(i, _)| *i == request_index)
    {
        let err = state.http_errors.remove(pos);
        return ResponseTemplate::new(err.1.status_code);
    }

    let reply = state.replies.pop_front().unwrap_or(Reply {
        content: "mock: no more replies queued".into(),
        tool_call: None,
        done: true,
        thinking: None,
    });

    let mut lines = Vec::new();

    // Thinking chunk
    if let Some(ref thinking) = reply.thinking {
        lines.push(serde_json::json!({
            "message": {"thinking": thinking, "content": ""},
            "done": false,
        }));
        lines.push(serde_json::json!({
            "message": {"thinking": "", "content": &reply.content},
            "done": false,
        }));
    } else if !reply.content.is_empty() {
        lines.push(serde_json::json!({
            "message": {"content": &reply.content},
            "done": false,
        }));
    }

    // Tool call chunk
    if let Some(ref tc) = reply.tool_call {
        lines.push(serde_json::json!({
            "message": {
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": tc.name,
                        "arguments": tc.arguments,
                    }
                }]
            },
            "done": false,
        }));
    }

    // Done chunk
    if reply.done {
        lines.push(serde_json::json!({
            "message": {"content": ""},
            "done": true,
            "done_reason": if reply.tool_call.is_some() { "tool_calls" } else { "stop" },
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        }));
    }

    let ndjson: String = lines.iter().map(|l| format!("{l}\n")).collect();
    ResponseTemplate::new(200).set_body_raw(ndjson.as_bytes().to_vec(), "application/x-ndjson")
}

// ── OpenAI-compat SSE response ────────────────────────────────────────

fn respond_openai_compat(
    req: &wiremock::Request,
    state: &Arc<Mutex<MockState>>,
) -> ResponseTemplate {
    let mut state = state.lock().expect("mock state lock");
    let request_index = state.request_log.len();
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
    state.request_log.push(RecordedRequest {
        method: req.method.to_string(),
        path: req.url.path().to_string(),
        body,
    });

    if let Some(pos) = state
        .http_errors
        .iter()
        .position(|(i, _)| *i == request_index)
    {
        let err = state.http_errors.remove(pos);
        return ResponseTemplate::new(err.1.status_code);
    }

    let reply = state.replies.pop_front().unwrap_or(Reply {
        content: "mock: no more replies queued".into(),
        tool_call: None,
        done: true,
        thinking: None,
    });

    let mut sse_lines = Vec::new();
    let id = format!("chatcmpl-e2e-{request_index}");

    // Thinking (OpenAI-compat doesn't have a standard thinking field,
    // but we emit it as a delta content chunk for compatibility).
    if let Some(ref thinking) = reply.thinking {
        sse_lines.push(format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": &id,
                "object": "chat.completion.chunk",
                "choices": [{"delta": {"content": thinking}, "index": 0}],
            })
        ));
    }

    if !reply.content.is_empty() {
        sse_lines.push(format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": &id,
                "object": "chat.completion.chunk",
                "choices": [{"delta": {"content": &reply.content}, "index": 0}],
            })
        ));
    }

    if let Some(ref tc) = reply.tool_call {
        sse_lines.push(format!(
            "data: {}\n\n",
            serde_json::json!({
                "id": &id,
                "object": "chat.completion.chunk",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_e2e",
                            "type": "function",
                            "function": {"name": tc.name, "arguments": tc.arguments.to_string()},
                        }]
                    },
                    "index": 0,
                }],
            })
        ));
    }

    // Final chunk with finish_reason
    sse_lines.push(format!(
        "data: {}\n\n",
        serde_json::json!({
            "id": &id,
            "object": "chat.completion.chunk",
            "choices": [{
                "delta": {},
                "finish_reason": if reply.tool_call.is_some() { "tool_calls" } else { "stop" },
                "index": 0,
            }],
        })
    ));
    sse_lines.push("data: [DONE]\n\n".to_string());

    let body = sse_lines.join("");
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/event-stream")
}

// ── Anthropic Messages SSE response ───────────────────────────────────

fn respond_anthropic(req: &wiremock::Request, state: &Arc<Mutex<MockState>>) -> ResponseTemplate {
    let mut state = state.lock().expect("mock state lock");
    let request_index = state.request_log.len();
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
    state.request_log.push(RecordedRequest {
        method: req.method.to_string(),
        path: req.url.path().to_string(),
        body,
    });

    if let Some(pos) = state
        .http_errors
        .iter()
        .position(|(i, _)| *i == request_index)
    {
        let err = state.http_errors.remove(pos);
        return ResponseTemplate::new(err.1.status_code);
    }

    let reply = state.replies.pop_front().unwrap_or(Reply {
        content: "mock: no more replies queued".into(),
        tool_call: None,
        done: true,
        thinking: None,
    });

    let mut sse_lines = Vec::new();

    // Message_start
    sse_lines.push(format!(
        "event: message_start\ndata: {}\n\n",
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": format!("msg_e2e_{}", request_index),
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-mock",
                "stop_reason": null,
            }
        })
    ));

    // Thinking block
    if let Some(ref thinking) = reply.thinking {
        sse_lines.push(format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "thinking", "thinking": ""},
            })
        ));
        sse_lines.push(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "thinking_delta", "thinking": thinking},
            })
        ));
        sse_lines.push(format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::json!({"type": "content_block_stop", "index": 0})
        ));
    }

    // Text content block
    if !reply.content.is_empty() {
        let block_index = if reply.thinking.is_some() { 1 } else { 0 };
        sse_lines.push(format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {"type": "text", "text": ""},
            })
        ));
        sse_lines.push(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_delta",
                "index": block_index,
                "delta": {"type": "text_delta", "text": &reply.content},
            })
        ));
        sse_lines.push(format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::json!({"type": "content_block_stop", "index": block_index})
        ));
    }

    // Tool use block
    if let Some(ref tc) = reply.tool_call {
        let block_index = if reply.thinking.is_some() { 1 } else { 0 };
        // If we also had text, increment
        let block_index = if !reply.content.is_empty() {
            block_index + 1
        } else {
            block_index
        };
        sse_lines.push(format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_start",
                "index": block_index,
                "content_block": {
                    "type": "tool_use",
                    "id": "toolu_e2e",
                    "name": tc.name,
                    "input": {},
                },
            })
        ));
        sse_lines.push(format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_delta",
                "index": block_index,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": tc.arguments.to_string(),
                },
            })
        ));
        sse_lines.push(format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::json!({"type": "content_block_stop", "index": block_index})
        ));
    }

    // Message delta (stop_reason)
    let stop_reason = if reply.tool_call.is_some() {
        "tool_use"
    } else {
        "end_turn"
    };
    sse_lines.push(format!(
        "event: message_delta\ndata: {}\n\n",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 1},
        })
    ));
    sse_lines.push("event: message_stop\ndata: {}\n\n".to_string());

    let body = sse_lines.join("");
    ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/event-stream")
}
