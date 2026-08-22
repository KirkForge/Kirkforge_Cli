use super::slots::VerifierSlots;
use super::types::{FixSuggestion, Verdict, Verifier};
use crate::session::verifier::types::{BusEvent, EventKind, FileWriteEvent};
use crate::shared::metrics::{record, MetricEvent};

use futures_util::future::FutureExt;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    /// Correction results that verifiers produced — consumed by correction loop.
    pub(crate) pending_corrections: Arc<tokio::sync::Mutex<Vec<FixSuggestion>>>,
    /// Path guard used when applying auto-fixes.
    pub(crate) path_guard: crate::session::access::PathGuard,
    /// Verdict cache keyed by `(file_path, content_hash)`. Only `Clean`/`Skipped`
    /// verdicts are cached — `Fixable`/`Unfixable` are not, because the
    /// correction loop re-runs verifiers after applying a fix (disk content
    /// changed, so the cached verdict would be stale). Entries are dropped via
    /// [`invalidate_cache`] after a correction loop applies any fix.
    /// ponytail: unbounded HashMap — per-session, bounded by distinct files written;
    /// add LRU if a session writes thousands of distinct files.
    #[allow(clippy::type_complexity)]
    pub(crate) verdict_cache: Arc<std::sync::Mutex<HashMap<(PathBuf, u64), (Verdict, String)>>>,
}

impl VerifierHandler {
    pub fn new(
        slots: Arc<std::sync::RwLock<VerifierSlots>>,
        path_guard: crate::session::access::PathGuard,
    ) -> Self {
        Self {
            slots,
            pending_corrections: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            path_guard,
            verdict_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Access the underlying verifier slots.
    pub fn slots(&self) -> Arc<std::sync::RwLock<VerifierSlots>> {
        self.slots.clone()
    }

    /// Drain pending corrections (consumed by the correction loop).
    pub async fn drain_corrections(&self) -> Vec<FixSuggestion> {
        let mut pending = self.pending_corrections.lock().await;
        std::mem::take(&mut *pending)
    }

    /// Drop cached verdicts for `path`. Called by the correction loop after a
    /// fix is applied — disk content changed, so any cached `Clean` for this
    /// path is stale regardless of which content_hash produced it.
    pub fn invalidate_cache(&self, path: &PathBuf) {
        let mut cache = self.verdict_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|key, _| key.0 != *path);
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
            let mut all_findings: Vec<(String, Verdict)> = Vec::new();
            for verifier in &verifiers {
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
                match &v {
                    Verdict::Clean | Verdict::Skipped(_) => continue,
                    Verdict::Fixable(_) | Verdict::Unfixable(_) => {
                        // Collect all findings — every verifier runs, none are
                        // skipped just because an earlier one flagged something.
                        all_findings.push((verifier.name().to_string(), v));
                    }
                }
            }
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
                Some((name, v)) => {
                    // Push ALL FixSuggestion entries so downstream can act on
                    // every finding, not just the decisive one.
                    let mut pending = self.pending_corrections.lock().await;
                    for (_, verdict) in &all_findings {
                        if let Verdict::Fixable(fix) = verdict {
                            pending.push(fix.clone());
                        }
                    }
                    (v.clone(), name.clone())
                }
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
