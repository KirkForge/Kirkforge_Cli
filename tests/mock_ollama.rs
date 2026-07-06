//! Mock Ollama server integration tests.
//!
//! These tests spin up a local HTTP mock server (via `wiremock`) that
//! replies with canned Ollama `/api/chat` NDJSON streams. They verify the
//! client-side parsing path without requiring a live Ollama instance, so
//! they run in CI and provide a stable harness for adapter regression tests.

use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Build a minimal NDJSON `/api/chat` response: a partial content chunk,
/// a thinking chunk, a tool-call batch, and a terminal `done: true` chunk
/// with usage statistics.
fn chat_response_body() -> Vec<u8> {
    let lines = [
        r#"{"message":{"thinking":"let me think","content":""},"done":false}"#,
        r#"{"message":{"thinking":"","content":"Hello "},"done":false}"#,
        r#"{"message":{"content":"world"},"done":false}"#,
        r#"{"message":{"content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"/tmp/x.txt"}}}]},"done":false}"#,
        r#"{"message":{"content":""},"done":true,"done_reason":"tool_calls","usage":{"prompt_tokens":3,"completion_tokens":5}}"#,
    ];
    format!("{}\n", lines.join("\n")).into_bytes()
}

/// Spin up a mock Ollama server that responds to POST `/api/chat` with
/// a streaming NDJSON body.
async fn start_mock_ollama() -> MockServer {
    let server = MockServer::start().await;
    let response =
        ResponseTemplate::new(200).set_body_raw(chat_response_body(), "application/x-ndjson");
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn mock_ollama_chat_streams_ndjson_events() {
    let server = start_mock_ollama().await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "model": "qwen2.5:0.5b",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
    });

    let mut response = client
        .post(format!("{}/api/chat", server.uri()))
        .json(&body)
        .send()
        .await
        .expect("mock /api/chat request failed");

    assert!(
        response.status().is_success(),
        "mock server should return 200"
    );

    let mut full_text = String::new();
    let mut saw_done = false;
    let mut saw_tool_call = false;
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await.unwrap_or(None) {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let json: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("mock returned invalid JSON: {e}\nline: {line}"));

            if let Some(content) = json["message"]["content"].as_str() {
                full_text.push_str(content);
            }
            if let Some(tcs) = json["message"]["tool_calls"].as_array() {
                if !tcs.is_empty() {
                    saw_tool_call = true;
                }
            }
            if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                saw_done = true;
                assert_eq!(
                    json["done_reason"].as_str(),
                    Some("tool_calls"),
                    "terminal chunk should carry done_reason"
                );
                assert!(
                    json.get("usage").is_some(),
                    "terminal chunk should carry usage"
                );
            }
        }
    }

    assert_eq!(full_text, "Hello world", "streamed text mismatch");
    assert!(saw_tool_call, "tool_calls chunk should be observed");
    assert!(saw_done, "done chunk should be observed");
}

#[tokio::test]
async fn mock_ollama_server_returns_error_for_missing_model() {
    let server = MockServer::start().await;
    let error_body = r#"{"error":"model 'missing' not found"}"#;
    let response =
        ResponseTemplate::new(200).set_body_raw(error_body.as_bytes().to_vec(), "application/json");
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(response)
        .mount(&server)
        .await;

    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "missing",
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true,
    });

    let response = client
        .post(format!("{}/api/chat", server.uri()))
        .json(&body)
        .send()
        .await
        .expect("mock /api/chat request failed");

    assert!(response.status().is_success());
    let text = response.text().await.expect("read body");
    assert!(text.contains("model 'missing' not found"));
}
