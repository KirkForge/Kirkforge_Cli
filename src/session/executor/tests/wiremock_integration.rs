//! In-process integration tests against a wiremock NDJSON server.
//!
//! These exist because the binary-spawning e2e tests in `tests/e2e/` hang
//! in a way that orphaned subprocesses make impossible to debug (the test
//! runner kills the test process at 60s but the spawned `kf-code` binary
//! keeps running as a grandchild). Running the same code path IN-PROCESS
//! here means a hang becomes a named `tokio::time::timeout` failure with a
//! real backtrace, not a silent wedge.
//!
//! Layered so a failure pinpoints the bad layer:
//!   - `adapter_stream_drains_against_wiremock`: adapter.stream() directly
//!   - `executor_turn_drains_against_wiremock`: full run_turn_collecting loop

#![cfg(test)]

use super::common::{make_config, make_executor};
use crate::adapters::ModelAdapter;
use crate::shared::{Message, Role, StreamEvent};
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build the real Ollama NDJSON adapter pointing at `mock_uri`.
/// Uses the routing table to force the generic Ollama path for the
/// `e2e-` prefix (mirrors the e2e fixture's `[adapter_routing]`).
fn real_adapter(mock_uri: &str) -> Box<dyn ModelAdapter> {
    // Force Ollama routing for the e2e model name. The routing table is
    // the public extension point; using it avoids relying on name-prefix
    // guessing that the binary-spawn e2e tests tripped over.
    crate::adapters::adapter_for_with_provider(
        "e2e-test-model",
        mock_uri,
        None,
        "anthropic",
        30,
        "https://opencode.ai/zen/v1/chat/completions",
        None,
        Some(&HashMap::from([("e2e-".to_string(), "Ollama".to_string())])),
        &crate::adapters::ProviderApiKeys::default(),
        None,
        None,
        None,
        None,
        "https://api.anthropic.com",
    )
}

/// Mount the canonical Ollama `/api/chat` NDJSON reply on `server`.
async fn mount_chat_reply(server: &MockServer, body: &str) {
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(header("content-type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/x-ndjson")
                .set_body_string(body),
        )
        .mount(server)
        .await;
}

const TWO_LINE_NDJSON: &str = "{\"model\":\"e2e-test-model\",\"message\":{\"role\":\"assistant\",\"content\":\"Hello from mock!\"},\"done\":false}\n{\"model\":\"e2e-test-model\",\"total_duration\":1000,\"done\":true}\n";

/// Layer 1 — the adapter alone. If this hangs/fails, the bug is in the
/// adapter HTTP request or the NDJSON stream parser.
#[tokio::test]
async fn adapter_stream_drains_against_wiremock() {
    let server = MockServer::start().await;
    mount_chat_reply(&server, TWO_LINE_NDJSON).await;

    let adapter = real_adapter(&server.uri());
    let messages = vec![Message {
        role: Role::User,
        content: "Say hello".into(),
        ..Default::default()
    }];
    let mut rx = adapter
        .stream(&messages, &[])
        .await
        .expect("adapter stream");

    let mut text = String::new();
    let mut saw_done = false;
    // 10s ceiling: the mock responds instantly; if we exceed this the
    // adapter is wedged and the timeout surfaces a named failure.
    let drained = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Text(t) => text.push_str(&t),
                StreamEvent::Done { .. } => {
                    saw_done = true;
                    break;
                }
                StreamEvent::Error(e) => panic!("adapter emitted StreamEvent::Error: {e}"),
                _ => {}
            }
        }
    })
    .await;

    assert!(
        drained.is_ok(),
        "adapter stream did not drain within 10s — adapter layer WEDGED. \
         text-so-far={text:?} saw_done={saw_done}"
    );
    assert!(
        text.contains("Hello from mock!"),
        "no payload; got {text:?}"
    );
    assert!(saw_done, "stream ended without Done event");
}

/// Layer 2 — the full executor turn loop with the real adapter. If Layer 1
/// passes but this hangs/fails, the bug is in the executor turn setup
/// (prompt builder, plugin hooks, channel wiring) rather than the adapter.
#[tokio::test]
async fn executor_turn_drains_against_wiremock() {
    let server = MockServer::start().await;
    mount_chat_reply(&server, TWO_LINE_NDJSON).await;

    let adapter = real_adapter(&server.uri());
    let config = {
        let mut c = make_config(false);
        c.model.ollama_host = server.uri();
        c
    };
    let mut executor = make_executor(adapter, Vec::new(), config).expect("make_executor");

    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
    // Deny any approval that surfaces (none expected for a no-tool turn).
    tokio::spawn(async move { while approval_rx.recv().await.is_some() {} });
    let cancelled = AtomicBool::new(false);

    let events = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        executor
            .run_turn_collecting("Say hello", &approval_tx, &cancelled)
            .await
    })
    .await;
    let events = match events {
        Ok(Ok(ev)) => ev,
        Ok(Err(e)) => panic!("run_turn_collecting returned Err: {e:#}"),
        Err(_) => panic!("run_turn_collecting did not return within 15s — executor layer WEDGED"),
    };

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            crate::session::executor::TurnEvent::Token(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("Hello from mock!"),
        "no payload in turn events; got {text:?} ({} events)",
        events.len()
    );
}
