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
            let mut verdict = Verdict::Clean;
            let mut name = "aggregate".to_string();
            for verifier in &verifiers {
                let v = verifier.verify(event).await;
                match &v {
                    Verdict::Clean | Verdict::Skipped(_) => continue,
                    Verdict::Fixable(_) | Verdict::Unfixable(_) => {
                        // Truth model: first verifier to report a finding wins,
                        // subsequent verifiers are skipped for that event.
                        name = verifier.name().to_string();
                        verdict = v;
                        break;
                    }
                }
            }
            (verdict, name)
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

        if let Verdict::Fixable(ref fix) = verdict {
            let mut pending = self.pending_corrections.lock().await;
            pending.push(fix.clone());
        }

        (verdict, decisive_name)
    }
}
