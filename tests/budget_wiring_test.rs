//! WO 38.8 — budget guard wiring integration tests.
//!
//! Test 1: slicing fires through the PRODUCTION attach path
//! (`attach_session_stores`), not the manually-wired `set_budget_stores` +
//! `register_sliced_listener` that 35.5's context_economics_test uses.
//!
//! Test 2: two concurrent executors with distinct session_ids dispatch
//! slices to their own listeners/stores — the registry fix that replaced
//! the append-only, first-wins, never-unregistered global Vec.

mod common;

use common::{MockOllama, Reply};
use kf_budget_core::TokenBudget;
use kf_code::adapters::{adapter_for_with_provider, ProviderApiKeys};
use kf_code::session::budget::{BudgetSlicedEvent, SharedBudget, SharedStore};
use kf_code::session::conversation::ConversationLog;
use kf_code::session::executor::{Executor, TurnEvent};
use kf_code::session::toolset::CompositeToolset;
use kf_code::shared::Config;
use kf_code::tools::read_file::ReadFile;
use kf_code::tools::Tool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MODEL: &str = "e2e-38-8-model";

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

// 40KB file: unique head marker (survives the slice head), a middle marker
// (lands in the offloaded middle), filler around them.
fn big_fixture_content() -> String {
    let mut content = String::from("BIGFILE_HEAD_MARKER_38_8\n");
    for i in 0..60 {
        content.push_str(&format!("filler line {i} of the big corpus file\n"));
    }
    content.push_str("BIGFILE_MIDDLE_MARKER_38_8\n");
    for i in 60..1000 {
        content.push_str(&format!("filler line {i} of the big corpus file\n"));
    }
    content
}

/// Build a Config with budget+stratum enabled and the sandbox pointed at
/// `fixture_dir`.
fn test_config(fixture_dir: &std::path::Path) -> Config {
    let mut cfg = Config::default();
    cfg.security.sandbox_dir = Some(fixture_dir.to_string_lossy().to_string());
    cfg.security.auto_approve = true;
    cfg.security.bash_sandbox_workdir = false;
    cfg.security.audit_log_path =
        Some(tempfile::NamedTempFile::new().unwrap().path().to_path_buf());
    cfg.model
        .adapter_routing
        .insert("e2e-".to_string(), "Ollama".to_string());
    cfg.model.request_timeout_secs = 30;
    cfg
}

/// Slicing fires through the PRODUCTION attach path: `attach_session_stores`
/// (not the manual `set_budget_stores` + `register_sliced_listener` that
/// 35.5 uses). The executor is constructed the same way `run_line_mode`
/// constructs it, then `attach_session_stores` wires the budget guard.
#[tokio::test]
async fn slicing_fires_through_production_attach_path() {
    let fixture = tempfile::tempdir().expect("tempdir");
    let big_path = fixture.path().join("big_38_8.txt");
    let big_content = big_fixture_content();
    std::fs::write(&big_path, &big_content).unwrap();

    let cfg = test_config(fixture.path());
    let (_deny_list, path_guard, _read_gate) = kf_code::session::access::access_from_config(&cfg);

    let mock = MockOllama::start(
        vec![
            Reply::tool(
                "read_file",
                serde_json::json!({
                    "path": big_path.to_string_lossy(),
                    "offset": 0,
                    "limit": 100_000,
                }),
            ),
            Reply::text("38_8 DONE"),
        ],
        Vec::new(),
    )
    .await;

    let tools =
        vec![Arc::new(ReadFile::new(path_guard.clone(), false, usize::MAX)) as Arc<dyn Tool>];
    let mut composite = CompositeToolset::empty();
    composite.add(Box::new(kf_code::session::toolset::VecToolset::new(
        "chain38_8",
        tools,
    )));

    let log_path = fixture.path().join("conversation_38_8.ndjson");
    let (conversation, _) = ConversationLog::open(log_path).expect("conversation log");

    // Construct the executor the same way run_line_mode does.
    let mut executor = Executor::with_log_and_undo_and_plugins(
        ollama_adapter(&mock.uri()),
        composite,
        Arc::new(std::sync::RwLock::new(cfg.clone())),
        conversation,
        None,
        None,
        None,
    )
    .expect("executor");

    // WO 38.8: set session_id BEFORE attach_session_stores (the stratum
    // listener is keyed by session_id).
    const SESSION: &str = "wo38-8-production-attach";
    executor.set_session_id(SESSION.to_string());

    // Build the SessionStores the same way run_session.rs does, then attach
    // via the PRODUCTION method (not set_budget_stores + manual registration).
    let budget: SharedBudget = Arc::new(Mutex::new(TokenBudget {
        ceiling: 2000,
        approaching_ratio: 0.8,
        used: 1800,
    }));
    let budget_store: SharedStore =
        Arc::new(kf_budget_core::InMemoryOffloadStore::new_with_cap(1000));
    let stratum_store = Arc::new(kf_compress_core::store::InMemoryOffloadStore::new_with_cap(
        1000,
    ));
    let stores = kf_code::session::SessionStores {
        budget: budget.clone(),
        budget_store: budget_store.clone(),
        stratum_store: stratum_store.clone(),
    };
    executor.attach_session_stores(stores);

    // attach_session_stores runs init_from_config which resets the ceiling
    // to the config default (200_000). Force Approaching AFTER attach so the
    // read_file result must be sliced.
    {
        let mut guard = budget.lock().unwrap();
        guard.ceiling = 2000;
        guard.used = 1800;
    }

    // Register a capture listener (keyed by SESSION) to record the slice.
    // Clear the default stratum listener that attach_session_stores
    // registered, then register the capture listener FIRST (returns None
    // → records the event), then re-register the default stratum listener
    // (returns Some → compresses). This mirrors 35.5's pattern.
    kf_code::session::budget::clear_session_sliced_listeners(SESSION);
    let captured: Arc<Mutex<Option<BudgetSlicedEvent>>> = Arc::new(Mutex::new(None));
    let captured_clone = captured.clone();
    kf_code::session::budget::register_sliced_listener(
        SESSION,
        Arc::new(move |event: BudgetSlicedEvent| {
            *captured_clone.lock().unwrap() = Some(event.clone());
            None
        }),
    );
    // Re-register the default stratum compression listener so the sliced
    // display is compressed (same as production).
    kf_code::session::stratum::register_default_budget_listener(SESSION, stratum_store.clone());

    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move { while approval_rx.recv().await.is_some() {} });
    let cancelled = std::sync::atomic::AtomicBool::new(false);

    let events = tokio::time::timeout(Duration::from_secs(30), async {
        executor
            .run_turn_collecting("read the big file", &approval_tx, &cancelled)
            .await
    })
    .await
    .expect("turn must not wedge")
    .expect("turn should complete");

    // The read_file result was sliced (not the 40KB raw content).
    let oversized_result = events
        .iter()
        .rev()
        .find_map(|e| match e {
            TurnEvent::ToolResult { name, output, .. } if name == "read_file" => {
                Some(output.clone())
            }
            _ => None,
        })
        .expect("no read_file tool result event");
    let event = captured
        .lock()
        .unwrap()
        .clone()
        .expect("budget must have sliced the oversized read_file result via production attach");
    assert!(
        event.original_size > oversized_result.len(),
        "sliced result ({}) must be smaller than original ({})",
        oversized_result.len(),
        event.original_size
    );
    assert!(
        oversized_result.len() < 1500,
        "final read_file result must be sliced down ({} bytes)",
        oversized_result.len()
    );
    // The offloaded middle is retrievable from the budget_store.
    let middle = budget_store
        .get(&event.key)
        .expect("offloaded middle must be retrievable by slice key");
    let middle = String::from_utf8_lossy(&middle);
    assert!(
        middle.contains("BIGFILE_MIDDLE_MARKER_38_8"),
        "offloaded middle must carry the corpus middle"
    );
    assert!(
        oversized_result.contains("BIGFILE_HEAD_MARKER_38_8"),
        "sliced head must keep the high-signal head; got: {oversized_result}"
    );
}

/// Two executors with distinct session_ids: a slice in session A dispatches
/// to session A's listener/store, NOT session B's. This is the registry fix
/// — the old append-only Vec would have dispatched session B's slice to
/// session A's listener (first-wins, never unregistered).
#[tokio::test]
async fn multi_executor_isolation_slices_land_in_own_stores() {
    let fixture = tempfile::tempdir().expect("tempdir");
    let cfg = test_config(fixture.path());

    let mock_a = MockOllama::start(vec![Reply::text("session A done")], Vec::new()).await;
    let mock_b = MockOllama::start(vec![Reply::text("session B done")], Vec::new()).await;

    let log_a = fixture.path().join("conv_a.ndjson");
    let log_b = fixture.path().join("conv_b.ndjson");
    let (conv_a, _) = ConversationLog::open(log_a).expect("conv a");
    let (conv_b, _) = ConversationLog::open(log_b).expect("conv b");

    let mut exec_a = Executor::with_log_and_undo_and_plugins(
        ollama_adapter(&mock_a.uri()),
        CompositeToolset::empty(),
        Arc::new(std::sync::RwLock::new(cfg.clone())),
        conv_a,
        None,
        None,
        None,
    )
    .expect("exec a");
    exec_a.set_session_id("iso-A".to_string());

    let mut exec_b = Executor::with_log_and_undo_and_plugins(
        ollama_adapter(&mock_b.uri()),
        CompositeToolset::empty(),
        Arc::new(std::sync::RwLock::new(cfg.clone())),
        conv_b,
        None,
        None,
        None,
    )
    .expect("exec b");
    exec_b.set_session_id("iso-B".to_string());

    // Attach stores to both. attach_session_stores runs init_from_config
    // which resets the ceiling to the config default (200_000), so force
    // Approaching AFTER attach.
    let budget_a: SharedBudget = Arc::new(Mutex::new(TokenBudget {
        ceiling: 2000,
        approaching_ratio: 0.8,
        used: 1800,
    }));
    let store_a: SharedStore = Arc::new(kf_budget_core::InMemoryOffloadStore::new_with_cap(1000));
    let stratum_a = Arc::new(kf_compress_core::store::InMemoryOffloadStore::new_with_cap(
        1000,
    ));
    exec_a.attach_session_stores(kf_code::session::SessionStores {
        budget: budget_a.clone(),
        budget_store: store_a.clone(),
        stratum_store: stratum_a,
    });
    {
        let mut g = budget_a.lock().unwrap();
        g.ceiling = 2000;
        g.used = 1800;
    }

    let budget_b: SharedBudget = Arc::new(Mutex::new(TokenBudget {
        ceiling: 2000,
        approaching_ratio: 0.8,
        used: 1800,
    }));
    let store_b: SharedStore = Arc::new(kf_budget_core::InMemoryOffloadStore::new_with_cap(1000));
    let stratum_b = Arc::new(kf_compress_core::store::InMemoryOffloadStore::new_with_cap(
        1000,
    ));
    exec_b.attach_session_stores(kf_code::session::SessionStores {
        budget: budget_b.clone(),
        budget_store: store_b.clone(),
        stratum_store: stratum_b,
    });
    {
        let mut g = budget_b.lock().unwrap();
        g.ceiling = 2000;
        g.used = 1800;
    }

    // Register distinct capture listeners for each session. Clear the
    // default stratum listener that attach_session_stores registered so
    // our marker listener is the one that wins (returns Some with a
    // session-specific marker).
    kf_code::session::budget::clear_session_sliced_listeners("iso-A");
    kf_code::session::budget::clear_session_sliced_listeners("iso-B");
    let cap_a: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap_a_clone = cap_a.clone();
    kf_code::session::budget::register_sliced_listener(
        "iso-A",
        Arc::new(move |event: BudgetSlicedEvent| {
            *cap_a_clone.lock().unwrap() = Some(format!("A:{}", event.sliced_size));
            Some("A-compressed".to_string())
        }),
    );
    let cap_b: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let cap_b_clone = cap_b.clone();
    kf_code::session::budget::register_sliced_listener(
        "iso-B",
        Arc::new(move |event: BudgetSlicedEvent| {
            *cap_b_clone.lock().unwrap() = Some(format!("B:{}", event.sliced_size));
            Some("B-compressed".to_string())
        }),
    );

    // Dispatch a slice event for session A — it must hit A's listener, not B's.
    let result_a = kf_code::session::budget::apply_budget_slice(
        kf_code::shared::ToolOutcome::Success {
            content: "x".repeat(10_000),
        },
        &budget_a,
        &store_a,
        "iso-A",
    );
    // A's listener returned Some, so the outcome carries A's replacement.
    match &result_a {
        kf_code::shared::ToolOutcome::Success { content } => {
            assert_eq!(
                content, "A-compressed",
                "session A slice must hit A's listener"
            );
        }
        other => panic!("expected Success for session A, got {other:?}"),
    }
    // A's listener fired with the actual sliced_size (head+marker+tail).
    let a_val = cap_a.lock().unwrap().clone().expect("A listener fired");
    assert!(
        a_val.starts_with("A:"),
        "session A listener must have fired, got: {a_val}"
    );
    assert!(
        cap_b.lock().unwrap().is_none(),
        "session B listener must NOT fire for session A's slice"
    );

    // Dispatch a slice event for session B — it must hit B's listener, not A's.
    let result_b = kf_code::session::budget::apply_budget_slice(
        kf_code::shared::ToolOutcome::Success {
            content: "y".repeat(10_000),
        },
        &budget_b,
        &store_b,
        "iso-B",
    );
    match &result_b {
        kf_code::shared::ToolOutcome::Success { content } => {
            assert_eq!(
                content, "B-compressed",
                "session B slice must hit B's listener"
            );
        }
        other => panic!("expected Success for session B, got {other:?}"),
    }
    let b_val = cap_b.lock().unwrap().clone().expect("B listener fired");
    assert!(
        b_val.starts_with("B:"),
        "session B listener must have fired, got: {b_val}"
    );
    assert_eq!(
        cap_a.lock().unwrap().as_deref(),
        Some(&a_val[..]),
        "session A listener must not fire again for session B's slice"
    );
}
