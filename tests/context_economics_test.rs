//! WO 35.5 chain 2 — context economics integration test.
//!
//! retrieval (context index) → compression (stratum listener) → budget
//! (slice + offload) → provider (wiremock NDJSON with usage) →
//! verification (default verifier bus on the turn's file write), in one
//! real executor turn against the mock provider.

// Exercises budget slicing + stratum listeners end-to-end; the modules these
// tests import only exist under those features (WO 48.24).
#![cfg(all(feature = "budget", feature = "stratum"))]

mod common;

use common::{MockOllama, Reply};
use kf_budget_core::TokenBudget;
use kf_code::adapters::{adapter_for_with_provider, ProviderApiKeys};
use kf_code::session::budget::{
    register_sliced_listener, BudgetSlicedEvent, SharedBudget, SharedStore,
};
use kf_code::session::conversation::ConversationLog;
use kf_code::session::executor::{Executor, TurnEvent};
use kf_code::session::stratum::register_default_budget_listener;
use kf_code::session::toolset::CompositeToolset;
use kf_code::shared::Config;
use kf_code::tools::read_file::ReadFile;
use kf_code::tools::write_file::WriteFile;
use kf_code::tools::Tool;
use kf_compress_core::store::InMemoryOffloadStore;
use kf_context_index::ContextIndex;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MODEL: &str = "e2e-35-5-model";
const ANCHOR_FN: &str = "chain_two_retrieval_anchor";

fn ollama_adapter(mock_uri: &str) -> Box<dyn kf_code::adapters::ModelAdapter> {
    let routing = HashMap::from([("e2e-".to_string(), "Ollama".to_string())]);
    adapter_for_with_provider(
        MODEL,
        mock_uri,
        None,
        "anthropic",
        30,
        "https://opencode.ai/zen/v1/chat/completions",
        None,
        Some(&routing),
        &ProviderApiKeys::default(),
        None,
        None,
        None,
        None,
        "https://api.anthropic.com",
    )
}

// 40KB corpus file: unique head marker (must survive the slice head), a
// middle marker deep in the file (must land in the offloaded middle —
// the slice keeps only ~100 head/tail bytes), filler around it.
fn big_fixture_content() -> String {
    let mut content = String::from("BIGFILE_HEAD_MARKER\n");
    for i in 0..60 {
        content.push_str(&format!("filler line {i} of the big corpus file\n"));
    }
    content.push_str("BIGFILE_MIDDLE_MARKER\n");
    for i in 60..1000 {
        content.push_str(&format!("filler line {i} of the big corpus file\n"));
    }
    content
}

#[tokio::test]
async fn turn_threads_retrieval_compression_budget_provider_verification() {
    let fixture = tempfile::tempdir().expect("tempdir");
    let corpus = "pub fn chain_two_retrieval_anchor() -> u32 { 42 }\n";
    std::fs::write(fixture.path().join("corpus.rs"), corpus).unwrap();
    let big_path = fixture.path().join("big.txt");
    let big_content = big_fixture_content();
    std::fs::write(&big_path, &big_content).unwrap();
    let secret_path = fixture.path().join("secret_note.txt");

    let mut cfg = Config::default();
    cfg.security.sandbox_dir = Some(fixture.path().to_string_lossy().to_string());
    cfg.security.auto_approve = true;
    cfg.security.bash_sandbox_workdir = false;
    cfg.security.audit_log_path =
        Some(tempfile::NamedTempFile::new().unwrap().path().to_path_buf());
    cfg.model
        .adapter_routing
        .insert("e2e-".to_string(), "Ollama".to_string());
    cfg.model.request_timeout_secs = 30;

    // Retrieval leg: index the corpus, attach to the executor's prompt
    // builder — relevant symbols must reach the provider request.
    let mut index = ContextIndex::new();
    index
        .index_file(&fixture.path().join("corpus.rs"), corpus)
        .expect("index corpus");

    let mock = MockOllama::start(
        vec![
            Reply::tool(
                "read_file",
                serde_json::json!({
                    "path": big_path.to_string_lossy(),
                    // whole-file read: offset=0 + limit>=total lines returns
                    // the RAW content (no pagination header) — same shape
                    // the bash `cat` leg used, but cross-platform.
                    "offset": 0,
                    "limit": 100_000,
                }),
            ),
            Reply::tool(
                "write_file",
                serde_json::json!({
                    "path": secret_path.to_string_lossy().to_string(),
                    "content": "-----BEGIN PRIVATE KEY-----\nnot a real key\n",
                }),
            ),
            Reply::text("CHAIN TWO DONE").with_usage(7, 11),
        ],
        Vec::new(),
    )
    .await;

    let (_deny_list, path_guard, _read_gate) = kf_code::session::access::access_from_config(&cfg);
    // read_file (not bash) produces the oversized tool result so the chain
    // runs identically on Windows — `cat` is POSIX-only and a failed bash
    // call yields no oversized result to slice. minify_above_bytes is set
    // huge so minification does not shrink the corpus before the budget
    // slicer sees it.
    let tools = vec![
        Arc::new(ReadFile::new(path_guard.clone(), false, usize::MAX)) as Arc<dyn Tool>,
        Arc::new(WriteFile::new(None, path_guard, false, false)) as Arc<dyn Tool>,
    ];
    let mut composite = CompositeToolset::empty();
    composite.add(Box::new(kf_code::session::toolset::VecToolset::new(
        "chain2", tools,
    )));

    let log_path = fixture.path().join("conversation.ndjson");
    let (conversation, _) = ConversationLog::open(log_path).expect("conversation log");
    let mut executor = Executor::with_log(
        ollama_adapter(&mock.uri()),
        composite,
        cfg,
        conversation,
        None,
    )
    .expect("executor");
    executor.set_session_id("ctx-econ-35-5".to_string());
    executor.set_context_index(index);

    // Budget leg: pre-load to Approaching so the read_file result must be
    // sliced; the offload store keeps the middle. The capture listener
    // (registered first, returns None) records the slice event; the
    // default stratum listener then compresses the sliced display.
    // WO 38.8: listeners are keyed by session_id, so they must match the
    // executor's session_id.
    const SESSION: &str = "ctx-econ-35-5";
    let budget: SharedBudget = Arc::new(Mutex::new(TokenBudget {
        ceiling: 2000,
        approaching_ratio: 0.8,
        used: 1800,
    }));
    let store: SharedStore = kf_code::session::budget::new_session_store();
    executor.set_budget_stores(budget, store.clone());
    let stratum_store = Arc::new(InMemoryOffloadStore::new());
    executor.set_stratum_store(stratum_store.clone());
    let captured: Arc<Mutex<Option<BudgetSlicedEvent>>> = Arc::new(Mutex::new(None));
    let captured_clone = captured.clone();
    register_sliced_listener(
        SESSION,
        Arc::new(move |event: BudgetSlicedEvent| {
            *captured_clone.lock().unwrap() = Some(event);
            None
        }),
    );
    register_default_budget_listener(SESSION, stratum_store.clone());

    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move { while approval_rx.recv().await.is_some() {} });
    let cancelled = std::sync::atomic::AtomicBool::new(false);

    let events = tokio::time::timeout(Duration::from_secs(30), async {
        executor
            .run_turn_collecting(
                &format!("process the corpus with {ANCHOR_FN}"),
                &approval_tx,
                &cancelled,
            )
            .await
    })
    .await
    .expect("turn must not wedge")
    .expect("turn should complete");

    // ── Provider leg: three requests, all with non-empty context. ──
    let bodies = mock.request_bodies();
    assert_eq!(bodies.len(), 3, "expected 3 model requests: {bodies:?}");
    for (i, body) in bodies.iter().enumerate() {
        let non_empty = body["messages"].as_array().is_some_and(|m| !m.is_empty());
        assert!(non_empty, "request {i} must carry a non-empty context");
    }

    // ── Retrieval leg: the indexed symbol reached the first request. ──
    let first = serde_json::to_string(&bodies[0]).unwrap();
    assert!(
        first.contains(ANCHOR_FN),
        "context index symbol must reach the provider: {first}"
    );

    // ── Budget + compression legs: the 40KB read_file result was sliced and
    // the offloaded middle is retrievable from the store. ──
    let oversized_result = events
        .iter()
        .rev()
        .find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } if name == "read_file" => {
                Some(output.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "no read_file tool result event; events: {events:?}; requests: {:?}",
                mock.request_bodies().len()
            )
        });
    let event = captured
        .lock()
        .unwrap()
        .clone()
        .expect("budget must have sliced the oversized read_file result");
    assert!(
        event.original_size > oversized_result.len(),
        "sliced result ({}) must be far smaller than the original ({})",
        oversized_result.len(),
        event.original_size
    );
    assert!(
        oversized_result.len() < 1500,
        "final read_file result must be sliced down ({} bytes): {oversized_result}",
        oversized_result.len()
    );
    assert!(
        oversized_result.len() <= event.sliced_size,
        "stratum compression must not grow the display: {} vs {}",
        oversized_result.len(),
        event.sliced_size
    );
    let middle = store
        .get(&event.key)
        .expect("offloaded middle must be retrievable by slice key");
    let middle = String::from_utf8_lossy(&middle);
    assert!(
        middle.contains("BIGFILE_MIDDLE_MARKER"),
        "offloaded middle must carry the corpus middle"
    );
    assert!(
        oversized_result.contains("BIGFILE_HEAD_MARKER"),
        "sliced head must keep the high-signal head; got: {oversized_result}"
    );

    // The sliced context (not the 40KB raw output) reached the provider.
    let second = serde_json::to_string(&bodies[1]).unwrap();
    assert!(
        second.contains("BIGFILE_HEAD_MARKER"),
        "provider must see the sliced head in the follow-up request"
    );
    assert!(
        second.len() < big_content.len(),
        "follow-up context must stay far below the raw corpus size"
    );

    // ── Provider accounting: CostStats matches the mock's usage exactly,
    // and usage-less turns now report ESTIMATED counts (WO 43.22: providers
    // that omit usage must not read zero-cost). The mock only stamps usage
    // on the final text reply; the two tool turns before it fall back to
    // token_count-cache estimates. Estimates are > 0 and grow with the
    // context (turn 2 carries the sliced corpus). The estimated entries
    // carry no flag yet — the `estimated: bool` field is a disclosed
    // deferral in state.md pending; when it lands, tighten this to check
    // it. ──
    let cost_stats: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::CostStats {
                prompt_tokens,
                completion_tokens,
                ..
            } => Some((*prompt_tokens, *completion_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(
        cost_stats.len(),
        3,
        "two estimated turns + one real-usage turn: {cost_stats:?}"
    );
    assert!(
        cost_stats[0].0 > 0 && cost_stats[1].0 > cost_stats[0].0,
        "estimated prompt tokens must be positive and grow with context: {cost_stats:?}"
    );
    assert_eq!(
        cost_stats[2],
        (7, 11),
        "the real-usage turn must match the mock's emitted usage exactly"
    );

    // ── Verification leg: a verifier ran on the turn's file write and
    // flagged the PEM header. ──
    let flagged = events.iter().any(|e| match e {
        TurnEvent::Verification {
            message, outcome, ..
        } => {
            matches!(
                outcome,
                kf_code::session::executor::VerificationOutcome::Failed
            ) && message.to_lowercase().contains("secret")
        }
        _ => false,
    });
    assert!(
        flagged,
        "security verifier must flag the PEM write; events: {events:?}"
    );
    assert!(secret_path.exists(), "the approved write must have landed");

    // The turn's final text made it through the loop.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::Token(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(text.contains("CHAIN TWO DONE"), "got: {text}");
}
