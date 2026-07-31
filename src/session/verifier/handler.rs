use super::slots::VerifierSlots;
use super::types::{FixSuggestion, Verdict, Verifier};
use crate::session::event_bus::{BusEvent, EventHandler, EventKind, HandlerResult};
use crate::shared::metrics::{record, MetricEvent};
use std::sync::Arc;

// ── EventBus integration ────────────────────────────────────────────────

/// Wraps a [`VerifierSlots`] as an [`EventHandler`] for the event bus.
///
/// This bridges the verifier system onto the event bus so that verifiers
/// get triggered automatically when tool events fire.
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
        // Short-circuit ToolError events: no built-in verifier acts on a
        // tool-error payload (they all return Skipped), so fanning the
        // event out to every verifier is wasted work. Skip the fan-out
        // entirely. (bucketlist 3.25)
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
        // Extract verifiers from the lock to avoid holding it across .await
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

        // Run truth-model precedence: first non-clean, non-skipped wins
        let (verdict, decisive_name) = {
            let mut verdict = Verdict::Clean;
            let mut name = "aggregate".to_string();
            for verifier in &verifiers {
                let v = verifier.verify(event).await;
                match &v {
                    Verdict::Clean | Verdict::Skipped(_) => continue,
                    Verdict::Fixable(_) | Verdict::Unfixable(_) => {
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

        // Collect fixable suggestions
        if let Verdict::Fixable(ref fix) = verdict {
            let mut pending = self.pending_corrections.lock().await;
            pending.push(fix.clone());
        }

        (verdict, decisive_name)
    }
}

#[async_trait::async_trait]
impl EventHandler for VerifierHandler {
    fn id(&self) -> &str {
        "verifier"
    }

    fn subscribed_kinds(&self) -> Vec<EventKind> {
        vec![
            EventKind::Edit,
            EventKind::FileWrite,
            EventKind::BashExec,
            EventKind::GitOperation,
            EventKind::ToolError,
        ]
    }

    async fn handle(&self, event: &BusEvent) -> HandlerResult {
        let (verdict, _decisive_name) = self.verify_event(event).await;
        let msg = match &verdict {
            Verdict::Clean => "All verifiers passed".into(),
            Verdict::Fixable(f) => format!("Fixable: {} ({})", f.description, f.severity),
            Verdict::Unfixable(e) => format!("Unfixable: {} — {}", e.description, e.details),
            Verdict::Skipped(reason) => format!("Skipped: {reason}"),
        };
        HandlerResult {
            handler_id: "verifier".into(),
            success: matches!(
                verdict,
                Verdict::Clean | Verdict::Skipped(_) | Verdict::Fixable(_)
            ),
            message: msg,
        }
    }
}
