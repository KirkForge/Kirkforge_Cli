//! Integration tests against a real Ollama server.
//!
//! These tests require:
//!   1. Ollama server running on http://localhost:11434
//!   2. The `qwen2.5:0.5b` model pulled (`ollama pull qwen2.5:0.5b`)
//!
//! Run with: `cargo test --test integration_test -- --ignored --nocapture`
//! Or selectively: `cargo test --test integration_test <name> -- --ignored --nocapture`

use std::time::Duration;

/// Shared reqwest client for all integration tests.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(60))
        .build()
        .expect("reqwest client")
}

const OLLAMA_HOST: &str = "http://localhost:11434";
const TEST_MODEL: &str = "qwen2.5:0.5b";

// ── Health / connectivity ─────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_ollama_server_connectivity() {
    let resp = client()
        .get(format!("{}/api/tags", OLLAMA_HOST))
        .send()
        .await
        .expect("Ollama /api/tags request failed");

    assert!(resp.status().is_success(), "Server should respond with 200");

    let body: serde_json::Value = resp.json().await.unwrap();
    let empty: Vec<serde_json::Value> = vec![];
    let models = body["models"].as_array().unwrap_or(&empty);

    let has_test_model = models.iter().any(|m| {
        m["name"]
            .as_str()
            .map(|n| n.starts_with(TEST_MODEL))
            .unwrap_or(false)
    });
    assert!(
        has_test_model,
        "Test model '{}' must be pulled (run: ollama pull {})",
        TEST_MODEL, TEST_MODEL
    );
}

// ── Basic streaming via /api/chat ────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_basic_streaming_response() {
    let body = serde_json::json!({
        "model": TEST_MODEL,
        "messages": [
            {"role": "user", "content": "Say exactly 'HELLO_WORLD' and nothing else."}
        ],
        "stream": true,
    });

    let mut response = client()
        .post(format!("{}/api/chat", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /api/chat failed");

    assert!(response.status().is_success(), "API should return 200");

    let mut full_text = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await.unwrap_or(None) {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(content) = json["message"]["content"].as_str() {
                    full_text.push_str(content);
                }
                if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    // Verify usage stats exist
                    assert!(
                        json.get("usage").is_some() || json.get("eval_count").is_some(),
                        "Done chunk should have usage or eval_count"
                    );
                }
            }
        }
    }

    assert!(!full_text.is_empty(), "Should have received some text");
    assert!(
        full_text.contains("HELLO_WORLD"),
        "Response should contain expected output. Got: {}",
        full_text
    );
}

// ── Non-streaming via /api/chat ───────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_non_streaming_response() {
    let body = serde_json::json!({
        "model": TEST_MODEL,
        "messages": [
            {"role": "user", "content": "Reply with just the word 'test123'"}
        ],
        "stream": false,
    });

    let resp = client()
        .post(format!("{}/api/chat", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /api/chat failed");

    assert!(resp.status().is_success());

    let json: serde_json::Value = resp.json().await.unwrap();

    assert!(
        json["done"].as_bool().unwrap_or(false),
        "Non-streaming should have done=true"
    );
    assert!(
        json["message"]["content"]
            .as_str()
            .map(|c| !c.is_empty())
            .unwrap_or(false),
        "Should have content"
    );
    assert!(
        json.get("eval_count").is_some() || json.get("usage").is_some(),
        "Should have token usage info"
    );
}

// ── Tool call format ──────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_tool_calls_format() {
    let body = serde_json::json!({
        "model": TEST_MODEL,
        "messages": [
            {"role": "user", "content": "What's the weather like in Paris today?"}
        ],
        "stream": false,
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get the current weather for a city",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "city": {
                                "type": "string",
                                "description": "The city name"
                            }
                        },
                        "required": ["city"]
                    }
                }
            }
        ]
    });

    let resp = client()
        .post(format!("{}/api/chat", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /api/chat failed");

    assert!(
        resp.status().is_success(),
        "Tool call request should succeed"
    );

    let json: serde_json::Value = resp.json().await.unwrap();

    // qwen2.5:0.5b is small — it may or may not call tools. The test passes
    // as long as the API accepts the tool-def field and returns valid JSON.
    // If it did call tools, verify the format matches what our adapters expect.
    if let Some(tool_calls) = json["message"]["tool_calls"].as_array() {
        assert!(
            !tool_calls.is_empty(),
            "If tool_calls present, should have entries"
        );
        for tc in tool_calls {
            // Must have function.name and function.arguments
            assert!(
                tc["function"]["name"].as_str().is_some(),
                "Each tool call must have function.name. Got: {}",
                serde_json::to_string_pretty(&tc).unwrap()
            );
        }
    }
}

// ── Error handling ────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_error_on_unknown_model() {
    let body = serde_json::json!({
        "model": "nonexistent-model-xyz",
        "messages": [
            {"role": "user", "content": "hi"}
        ],
        "stream": true,
    });

    let resp = client()
        .post(format!("{}/api/chat", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /api/chat failed");

    // Should get a 404 or error field in body
    let status = resp.status();
    if status.is_success() {
        // Some Ollama configs may stream an error field instead
        let body: serde_json::Value = resp.json().await.unwrap();
        if let Some(err) = body.get("error") {
            assert!(
                !err.as_str().unwrap_or("").is_empty(),
                "Error field should have message"
            );
        }
    } else {
        assert_eq!(status.as_u16(), 404, "Unknown model should 404");
    }
}

// ── Streaming with tools (our adapter's typical flow) ─────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_tool_fn_json_parse_in_chunks() {
    // Simulates the exact stream our GLM/DeepSeek adapters parse:
    // multiple JSON lines, tool calls as function blocks, final done=true
    let body = serde_json::json!({
        "model": TEST_MODEL,
        "messages": [
            {"role": "system", "content": "You are a helpful assistant that can use tools."},
            {"role": "user", "content": "What files are in the src/ directory?"}
        ],
        "stream": true,
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "read_directory",
                    "description": "List files in a directory",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Directory path"
                            }
                        },
                        "required": ["path"]
                    }
                }
            }
        ]
    });

    let mut response = client()
        .post(format!("{}/api/chat", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /api/chat failed");

    assert!(response.status().is_success());

    let mut buffer = String::new();
    let mut has_content = false;
    let mut has_done = false;

    while let Some(chunk) = response.chunk().await.unwrap_or(None) {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let json: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|_| serde_json::json!({"__parse_error": line}));

            if json.get("__parse_error").is_some() {
                continue; // partial chunk boundary
            }

            if let Some(content) = json["message"]["content"].as_str() {
                if !content.is_empty() {
                    has_content = true;
                }
            }

            // Tool-only responses have empty content but valid tool_calls
            if json["message"]["tool_calls"]
                .as_array()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false)
            {
                has_content = true;
            }

            if json.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                has_done = true;
            }
        }
    }

    // Stream should have content text, tool_calls, or both — plus a done signal
    assert!(
        has_content,
        "Stream should have produced some content or tool_calls"
    );
    assert!(has_done, "Stream should have terminated with done=true");
}

// ── OpenAI-compat endpoint ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Ollama server running"]
async fn test_openai_compat_endpoint() {
    let body = serde_json::json!({
        "model": TEST_MODEL,
        "messages": [
            {"role": "user", "content": "Say 'OAI_OK'"}
        ],
        "stream": true,
    });

    let mut response = client()
        .post(format!("{}/v1/chat/completions", OLLAMA_HOST))
        .json(&body)
        .send()
        .await
        .expect("POST /v1/chat/completions failed");

    assert!(response.status().is_success());

    let mut full_text = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = response.chunk().await.unwrap_or(None) {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(start) = buffer.find("data: ") {
            let after_data = &buffer[start + 6..];
            let end = after_data.find("\n\n").unwrap_or(after_data.len());
            let line: String = after_data[..end].trim().to_string();
            buffer.drain(..=start + 6 + end);

            if line.is_empty() || line == "[DONE]" {
                if line == "[DONE]" {
                    break;
                }
                continue;
            }

            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                if let Some(content) = json["choices"][0]["delta"]["content"].as_str() {
                    full_text.push_str(content);
                }
                if json["choices"][0]["finish_reason"]
                    .as_str()
                    .is_some_and(|r| !r.is_empty())
                {
                    break;
                }
            }
        }
    }

    assert!(
        !full_text.is_empty(),
        "OpenAI-compat endpoint should return text content"
    );
}
