//! `recent_outputs.jsonl` — bounded FIFO of slice keys for the
//! `plugin3 budget compact` and `plugin3 store prune` commands.
//! Per ADR-0002 § Crate layout (`crates/plugin3-cli/src/`).

use std::collections::VecDeque;

use plugin3_core::{
    atomic_write_text,
    budget::TokenBudget,
    cost::{emit_usage, UsageKind, UsageRecord},
    Paths,
};
use serde::{Deserialize, Serialize};

// ponytail: the on-disk shape of recent_outputs.jsonl is owned by
// this crate (the writer and reader both live here), so a typed struct
// beats ad-hoc Value digging. Keep it private — nothing outside this
// module needs it.
#[derive(Deserialize, Serialize)]
pub(crate) struct RecentEntry {
    pub(crate) key: String,
    pub(crate) size: usize,
}

pub(crate) fn load_recent_outputs() -> VecDeque<(String, usize)> {
    let path = Paths::resolve().recent_outputs();
    load_recent_outputs_at(&path)
}

// ponytail: path-parameterised so drift tests in this crate can
// point at a tempdir without mutating the process-wide
// `PLUGIN3_*_DIR` env vars (which would race with parallel tests
// that share the same harness process). The public `load_recent_outputs`
// is the production entry point; this is the test-friendly seam.
pub(crate) fn load_recent_outputs_at(path: &std::path::Path) -> VecDeque<(String, usize)> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    s.lines()
        .filter_map(|line| serde_json::from_str::<RecentEntry>(line).ok())
        .map(|e| (e.key, e.size))
        .collect()
}

pub(crate) const RECENT_BOUND: usize = 32;

// ponytail: rewrite the whole file on every append — bounded at 32
// entries, so O(N) is fine. Switch to append-with-rollover when
// the bound grows. Atomic via `atomic_write_text` (NamedTempFile +
// persist); failures eprintln so a host's stderr captures a
// missing-recent-file warning without breaking the slice path.
pub(crate) fn append_recent(key: &str, size: usize) {
    let path = Paths::resolve().recent_outputs();
    append_recent_at(&path, key, size);
}

// ponytail: VecDeque, not Vec. `Vec::remove(0)` shifts every
// surviving element (O(n) per eviction); with a 32-entry bound
// the FIFO rewrite below was O(n²) per append. VecDeque::pop_front
// is O(1). Public types changed (Vec → VecDeque) but the wire
// shape on disk and the function signature are unchanged —
// the drift test in `recent_outputs_tests` and `state_spec_drift`
// pin the JSONL row shape and the `fn append_recent(key: &str,
// size: usize)` signature, not the in-memory container.
pub(crate) fn append_recent_at(path: &std::path::Path, key: &str, size: usize) {
    let mut entries = load_recent_outputs_at(path);
    entries.push_back((key.to_string(), size));
    while entries.len() > RECENT_BOUND {
        entries.pop_front();
    }
    // ponytail: serde-serialise the struct rather than reaching for
    // `serde_json::json!` — one wire format owned by `RecentEntry`.
    let mut body = String::new();
    for (k, s) in &entries {
        let line = match serde_json::to_string(&RecentEntry {
            key: k.clone(),
            size: *s,
        }) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("plugin3: failed to serialise recent entry: {e}");
                continue;
            }
        };
        body.push_str(&line);
        body.push('\n');
    }
    atomic_write_text(path, "recent", &body);
}

pub(crate) fn empty_record() -> UsageRecord {
    UsageRecord {
        ts: chrono::Utc::now(),
        kind: UsageKind::Prompt,
        session_id: String::new(),
        bytes_in: None,
        bytes_out: None,
        tokens_used: None,
        tokens_ceiling: None,
        tool: None,
    }
}

// ponytail: `run_pre_compact` (no session) and `budget_compact`
// (no session either) both emitted a CompactHint record with
// identical fields. Extracted because the eprintln tags aren't
// the only place drift would surface — a future "add model
// column to CompactHint records" change would need to remember
// to update both call sites.
pub(crate) fn emit_compact_hint(b: &TokenBudget) {
    emit_usage(&UsageRecord {
        kind: UsageKind::CompactHint,
        session_id: String::new(),
        tokens_used: Some(b.used),
        tokens_ceiling: Some(b.ceiling),
        ..empty_record()
    });
}
