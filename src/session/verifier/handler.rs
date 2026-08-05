use super::slots::VerifierSlots;
use super::types::{FixSuggestion, Verdict, Verifier};
use crate::session::verifier::types::{BusEvent, EventKind};
use crate::shared::metrics::{record, MetricEvent};
use std::sync::Arc;

/// Wraps a [`VerifierSlots`] and runs verification on tool events.
///
/// Called directly by the dispatch layer — no intermediate pub/sub bus.
pub struct VerifierHandler {
    slots: Arc<std::sync::RwLock<VerifierSlots>>,
    /// Correction results that verifiers produced — consumed by correction loop.
    pub(crate) pending_corrections: Arc<tokio::sync::Mutex<Vec<FixSuggestion>>>,
    /// Path guard used when applying auto-fixes.
    pub(crate) path_guard: crate::session::access::PathGuard,
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
                let v = verifier.verify(event).await;
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

        (verdict, decisive_name)
    }
}
