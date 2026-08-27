use super::slots::VerifierSlots;
use super::types::{Verdict, Verifier};
use crate::session::verifier::types::{BusEvent, EventKind, FileWriteEvent};
use crate::shared::metrics::{record, MetricEvent};

use futures_util::future::FutureExt;
use futures_util::stream::{self, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// Cap on cached verdicts (WO 47.26). FIFO eviction — only Clean/Skipped
// verdicts are cached, so an evicted entry just costs one re-verify.
pub(crate) const VERDICT_CACHE_CAP: usize = 256;

// Verifiers are independent — run this many concurrently so a slow cargo
// build doesn't serialize the whole panel behind it. 4 keeps subprocess
// fan-out sane (build/clippy/test verifiers each spawn their own cargo).
const VERIFIER_CONCURRENCY: usize = 4;

// Per-verifier wall-clock cap. A wedged verifier (e.g. `cargo build` stuck on
// a broken pipe) would otherwise hang the whole turn. Skipped, not failed, so
// a slow verifier doesn't poison an otherwise-clean result.
//
// In tests the cap is shrunk to 50ms so the timeout path is exercisable
// without waiting 30s of real time (and without pulling in tokio's
// `test-util` mock-clock feature).
#[cfg(not(test))]
const VERIFIER_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const VERIFIER_TIMEOUT: Duration = Duration::from_millis(50);

// ponytail: thin pass-through between CorrectionLoop and VerifierSlots; merge if handler gains no more logic
/// Wraps a [`VerifierSlots`] and runs verification on tool events.
///
/// Called directly by the dispatch layer — no intermediate pub/sub bus.
pub struct VerifierHandler {
    slots: Arc<std::sync::RwLock<VerifierSlots>>,
    /// Path guard used when applying auto-fixes.
    pub(crate) path_guard: crate::session::access::PathGuard,
    /// Verdict cache keyed by `(file_path, content_hash)`. Only `Clean`/`Skipped`
    /// verdicts are cached — `Fixable`/`Unfixable` are not, because the
    /// correction loop re-runs verifiers after applying a fix (disk content
    /// changed, so the cached verdict would be stale). Entries are dropped via
    /// [`invalidate_cache`] after a correction loop applies any fix.
    /// Bounded: FIFO eviction past [`VERDICT_CACHE_CAP`] entries.
    verdict_cache: Arc<std::sync::Mutex<VerdictCache>>,
}

/// HashMap + insertion order, so eviction is FIFO instead of arbitrary
/// (mirrors the kf-budget-core WO 46.34 pattern).
struct VerdictCache {
    map: HashMap<(PathBuf, u64), (Verdict, String)>,
    order: VecDeque<(PathBuf, u64)>,
}

impl VerdictCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, key: &(PathBuf, u64)) -> Option<&(Verdict, String)> {
        self.map.get(key)
    }

    fn insert(&mut self, key: (PathBuf, u64), value: (Verdict, String)) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.map.len() > VERDICT_CACHE_CAP {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
    }

    fn invalidate_path(&mut self, path: &PathBuf) {
        self.map.retain(|key, _| key.0 != *path);
        self.order.retain(|key| key.0 != *path);
    }
}

impl VerifierHandler {
    pub fn new(
        slots: Arc<std::sync::RwLock<VerifierSlots>>,
        path_guard: crate::session::access::PathGuard,
    ) -> Self {
        Self {
            slots,
            path_guard,
            verdict_cache: Arc::new(std::sync::Mutex::new(VerdictCache::new())),
        }
    }

    /// Access the underlying verifier slots.
    pub fn slots(&self) -> Arc<std::sync::RwLock<VerifierSlots>> {
        self.slots.clone()
    }

    /// Drop cached verdicts for `path`. Called by the correction loop after a
    /// fix is applied — disk content changed, so any cached `Clean` for this
    /// path is stale regardless of which content_hash produced it.
    pub fn invalidate_cache(&self, path: &PathBuf) {
        let mut cache = self.verdict_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.invalidate_path(path);
    }

    /// Run verification and return the verdict plus the decisive
    /// verifier's name (used by the correction loop so the model sees
    /// `verifier:lint` instead of the useless `verifier:verifier`).
    pub async fn verify_event(&self, event: &BusEvent) -> (Verdict, String) {
        if event.kind() == EventKind::ToolError {
            record(MetricEvent::Verifier {
                name: "aggregate".to_string(),
                verdict: "skipped".to_string(),
                source: "built-in".to_string(),
            });
            return (
                Verdict::Skipped("tool-error event: no verifiers act on ToolError".into()),
                "aggregate".to_string(),
            );
        }

        // Verdict cache: skip re-running cargo build/clippy/test when the
        // same file+content was already verified clean. Only FileWrite events
        // carry a content_hash; other event kinds always run verifiers.
        if let BusEvent::FileWrite(FileWriteEvent {
            path,
            content_hash: hash,
            ..
        }) = event
        {
            if *hash > 0 {
                let cache = self.verdict_cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(cached) = cache.get(&(path.clone(), *hash)) {
                    record(MetricEvent::Verifier {
                        name: "aggregate".to_string(),
                        verdict: match &cached.0 {
                            Verdict::Clean => "clean",
                            Verdict::Skipped(_) => "skipped",
                            _ => "fixable",
                        }
                        .to_string(),
                        source: "cache-hit".to_string(),
                    });
                    return cached.clone();
                }
            }
        }

        let verifiers: Vec<Arc<dyn Verifier>> = {
            let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
            if slots.is_empty() {
                record(MetricEvent::Verifier {
                    name: "none".to_string(),
                    verdict: "clean".to_string(),
                    source: "built-in".to_string(),
                });
                return (Verdict::Clean, "aggregate".to_string());
            }
            slots.all_verifiers()
        };

        let (verdict, decisive_name) = {
            // Run verifiers concurrently, bounded by VERIFIER_CONCURRENCY
            // (WO 47.26). Futures are built in a plain loop (not a stream
            // closure) — closures in the future's type trip rustc's
            // higher-ranked Send-inference limitation for callers that
            // spawn verify_event. Results are restored to registration
            // order before the decisive pick so "first-seen among equals"
            // tie-breaking stays deterministic.
            let futures: Vec<_> = verifiers
                .into_iter()
                .enumerate()
                .map(|(idx, verifier)| run_verifier(idx, verifier, event))
                .collect();
            let mut results: Vec<(usize, String, Verdict)> = stream::iter(futures)
                .buffer_unordered(VERIFIER_CONCURRENCY)
                .collect()
                .await;
            results.sort_by_key(|(idx, _, _)| *idx);
            // Collect all findings — every verifier runs, none are skipped
            // just because an earlier one flagged something.
            let all_findings: Vec<(String, Verdict)> = results
                .into_iter()
                .filter_map(|(_, name, v)| match v {
                    Verdict::Clean | Verdict::Skipped(_) => None,
                    Verdict::Fixable(_) | Verdict::Unfixable(_) => Some((name, v)),
                })
                .collect();
            // Pick the most severe: Unfixable > Fixable, first-seen among equals.
            let decisive = all_findings
                .iter()
                .find(|(_, v)| matches!(v, Verdict::Unfixable(_)))
                .or_else(|| {
                    all_findings
                        .iter()
                        .find(|(_, v)| matches!(v, Verdict::Fixable(_)))
                });
            match decisive {
                Some((name, v)) => (v.clone(), name.clone()),
                None => (Verdict::Clean, "aggregate".to_string()),
            }
        };

        let verdict_label = match &verdict {
            Verdict::Clean => "clean",
            Verdict::Fixable(_) => "fixable",
            Verdict::Unfixable(_) => "unfixable",
            Verdict::Skipped(_) => "skipped",
        };
        record(MetricEvent::Verifier {
            name: decisive_name.clone(),
            verdict: verdict_label.to_string(),
            source: "built-in".to_string(),
        });

        // Cache only non-actionable verdicts. Fixable/Unfixable are not
        // cached: the correction loop re-runs verifiers after applying a fix,
        // and the disk content has changed by then.
        if let BusEvent::FileWrite(FileWriteEvent {
            path,
            content_hash: hash,
            ..
        }) = event
        {
            if *hash > 0 && matches!(verdict, Verdict::Clean | Verdict::Skipped(_)) {
                let mut cache = self.verdict_cache.lock().unwrap_or_else(|e| e.into_inner());
                cache.insert(
                    (path.clone(), *hash),
                    (verdict.clone(), decisive_name.clone()),
                );
            }
        }

        (verdict, decisive_name)
    }
}

// Runs one verifier under the per-verifier timeout + panic guard, tagging
// the result with its registration index. The panic guard contains a
// panicking verifier only in unwind builds (dev/test); release uses
// panic=abort — the process aborts and the WO 38.2 panic hook restores
// the terminal (WO 47.23 contract). A free async fn (not an async
// block inside the stream closure) so the future stays provably Send for
// callers that spawn verify_event (rustc higher-ranked-closure limitation).
async fn run_verifier(
    idx: usize,
    verifier: Arc<dyn Verifier>,
    event: &BusEvent,
) -> (usize, String, Verdict) {
    let v = match tokio::time::timeout(
        VERIFIER_TIMEOUT,
        AssertUnwindSafe(verifier.verify(event)).catch_unwind(),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(panic_payload)) => {
            let msg = panic_payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::warn!("verifier {} panicked: {msg}", verifier.name());
            Verdict::Skipped(format!("verifier panicked: {msg}"))
        }
        Err(_elapsed) => {
            tracing::warn!(
                "verifier {} timed out after {:?}",
                verifier.name(),
                VERIFIER_TIMEOUT
            );
            Verdict::Skipped(format!("verifier timed out after {VERIFIER_TIMEOUT:?}"))
        }
    };
    (idx, verifier.name().to_string(), v)
}
