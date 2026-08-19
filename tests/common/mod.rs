//! Shared harness for the WO 35.5 cross-component integration tests.
//!
//! A scripted Ollama `/api/chat` mock (wiremock): replies are queued per
//! model request, every request body is recorded for wire assertions, and
//! the literal `{WORKTREE}` in a queued tool-call argument is substituted
//! at serve time with the subagent worktree the spawner just created
//! (discovered by scanning the temp dir — the path embeds this process's
//! pid, so only this test process can produce it).

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// One scripted model turn: assistant text and/or a single tool call.
#[derive(Debug, Clone)]
pub struct Reply {
    pub content: String,
    pub tool_call: Option<(String, serde_json::Value)>,
    /// Usage numbers emitted on the terminal done chunk. `None` = no
    /// usage (adapter then reports no CostStats for this request).
    pub usage: Option<(usize, usize)>,
}

impl Reply {
    pub fn text(content: &str) -> Self {
        Self {
            content: content.to_string(),
            tool_call: None,
            usage: None,
        }
    }

    pub fn tool(name: &str, arguments: serde_json::Value) -> Self {
        Self {
            content: String::new(),
            tool_call: Some((name.to_string(), arguments)),
            usage: None,
        }
    }

    pub fn with_usage(mut self, prompt: usize, completion: usize) -> Self {
        self.usage = Some((prompt, completion));
        self
    }
}

#[derive(Debug, Default)]
struct MockState {
    replies: VecDeque<Reply>,
    request_bodies: Vec<serde_json::Value>,
    /// Worktree dirs seen before the run — the serve-time scan picks the
    /// one new `kf-code-session-task-<pid>-*` dir against this snapshot.
    worktree_before: Vec<std::path::PathBuf>,
    found_worktree: Option<std::path::PathBuf>,
}

/// The scripted mock Ollama server.
pub struct MockOllama {
    pub server: MockServer,
    state: Arc<Mutex<MockState>>,
}

fn scan_subagent_worktrees(before: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let prefix = format!("kf-code-session-task-{}-", std::process::id());
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(std::env::temp_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    found.retain(|p| !before.contains(p));
    found.sort();
    found
}

// Replace every `{WORKTREE}` occurrence inside string values of a JSON
// value with `path` (handles both `{WORKTREE}` alone and
// `{WORKTREE}/file.txt`).
fn substitute_worktree(value: &mut serde_json::Value, path: &str) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("{WORKTREE}") {
                *s = s.replace("{WORKTREE}", path);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                substitute_worktree(item, path);
            }
        }
        serde_json::Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                substitute_worktree(item, path);
            }
        }
        _ => {}
    }
}

fn ollama_ndjson(reply: &Reply) -> String {
    let mut lines = Vec::new();
    if !reply.content.is_empty() {
        lines.push(serde_json::json!({
            "message": {"content": &reply.content},
            "done": false,
        }));
    }
    if let Some((name, args)) = &reply.tool_call {
        lines.push(serde_json::json!({
            "message": {
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": name,
                        "arguments": args,
                    }
                }]
            },
            "done": false,
        }));
    }
    let mut done = serde_json::json!({
        "message": {"content": ""},
        "done": true,
        "done_reason": if reply.tool_call.is_some() { "tool_calls" } else { "stop" },
    });
    if let Some((prompt, completion)) = reply.usage {
        done["usage"] = serde_json::json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
        });
    }
    lines.push(done);
    lines.iter().map(|l| format!("{l}\n")).collect()
}

impl MockOllama {
    /// Start the mock with `replies` queued in order. The literal
    /// `{WORKTREE}` in tool-call arguments is resolved at serve time
    /// against the subagent worktree created since `before_worktrees`.
    pub async fn start(replies: Vec<Reply>, before_worktrees: Vec<std::path::PathBuf>) -> Self {
        let state = Arc::new(Mutex::new(MockState {
            replies: replies.into(),
            worktree_before: before_worktrees,
            ..Default::default()
        }));
        let server = MockServer::start().await;

        let respond_state = state.clone();
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(move |req: &wiremock::Request| {
                let mut state = respond_state.lock().expect("mock state lock");
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
                state.request_bodies.push(body);

                let mut reply = state
                    .replies
                    .pop_front()
                    .unwrap_or_else(|| Reply::text("mock: no more replies queued"));
                if let Some((_, args)) = &mut reply.tool_call {
                    if serde_json::to_string(args).is_ok_and(|s| s.contains("{WORKTREE}")) {
                        let candidates = scan_subagent_worktrees(&state.worktree_before);
                        if candidates.len() > 1 {
                            return ResponseTemplate::new(500).set_body_string(format!(
                                "worktree scan ambiguous: {candidates:?}"
                            ));
                        }
                        let Some(worktree) = candidates.first().cloned() else {
                            return ResponseTemplate::new(500).set_body_string(
                                "no subagent worktree found for {WORKTREE} substitution",
                            );
                        };
                        state.found_worktree = Some(worktree.clone());
                        substitute_worktree(args, &worktree.to_string_lossy());
                    }
                }

                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/x-ndjson")
                    .set_body_string(ollama_ndjson(&reply))
            })
            .mount(&server)
            .await;

        Self { server, state }
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    /// Recorded request bodies, in order.
    pub fn request_bodies(&self) -> Vec<serde_json::Value> {
        self.state
            .lock()
            .expect("mock state lock")
            .request_bodies
            .clone()
    }

    /// The subagent worktree the mock wrote into (set once a reply with
    /// `{WORKTREE}` was served).
    pub fn found_worktree(&self) -> Option<std::path::PathBuf> {
        self.state
            .lock()
            .expect("mock state lock")
            .found_worktree
            .clone()
    }
}
