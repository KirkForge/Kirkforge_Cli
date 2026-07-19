use super::types::{Verdict, Verifier};
use crate::session::event_bus::BusEvent;
use std::sync::Arc;

// ── Verifier Slots ──────────────────────────────────────────────────────

/// Registry of verifiers with priority-based dispatch.
///
/// Holds up to 4 verifier slots (lint, type_check, git, security).
/// When an event arrives, verifiers run in priority order. The first
/// non-`Clean` verdict that isn't `Skipped` wins (truth model).
#[derive(Default)]
pub struct VerifierSlots {
    verifiers: Vec<Arc<dyn Verifier>>,
    max_slots: usize,
}

impl VerifierSlots {
    /// Create a new slot registry (default 4 slots).
    pub fn new() -> Self {
        Self {
            verifiers: Vec::new(),
            max_slots: 4,
        }
    }

    /// Create with a custom slot limit.
    pub fn with_max_slots(max: usize) -> Self {
        Self {
            verifiers: Vec::new(),
            max_slots: max,
        }
    }

    /// Register a verifier.
    ///
    /// Returns an error if all slots are filled.
    pub fn register(&mut self, verifier: Arc<dyn Verifier>) -> anyhow::Result<()> {
        if self.verifiers.len() >= self.max_slots {
            anyhow::bail!(
                "All {} verifier slots are filled. Cannot register '{}'",
                self.max_slots,
                verifier.name()
            );
        }
        let name = verifier.name().to_string();
        if self.verifiers.iter().any(|v| v.name() == name) {
            anyhow::bail!("Verifier '{name}' is already registered");
        }
        self.verifiers.push(verifier);
        // Keep sorted by priority (stable sort preserves insertion order for equal priority)
        self.verifiers.sort_by_key(|v| v.priority());
        Ok(())
    }

    /// Unregister a verifier by name.
    pub fn unregister(&mut self, name: &str) -> bool {
        let len_before = self.verifiers.len();
        self.verifiers.retain(|v| v.name() != name);
        self.verifiers.len() < len_before
    }

    /// Retain only verifiers that satisfy the predicate.
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&Arc<dyn Verifier>) -> bool,
    {
        self.verifiers.retain(|v| f(v));
    }

    /// Run all verifiers against an event, applying truth model precedence.
    ///
    /// Returns the first definitive result:
    /// - `Fixable`/`Unfixable` wins immediately (stop at first non-clean)
    /// - `Skipped` is ignored (continue to next)
    /// - If all return `Clean` or `Skipped`, returns `Clean`
    pub async fn verify(&self, event: &BusEvent) -> Verdict {
        for verifier in &self.verifiers {
            let verdict = verifier.verify(event).await;
            match &verdict {
                Verdict::Clean | Verdict::Skipped(_) => continue,
                Verdict::Fixable(_) | Verdict::Unfixable(_) => return verdict,
            }
        }
        Verdict::Clean
    }

    /// Number of registered verifiers.
    pub fn len(&self) -> usize {
        self.verifiers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.verifiers.is_empty()
    }

    /// Get verifier names in priority order.
    pub fn names(&self) -> Vec<String> {
        self.verifiers
            .iter()
            .map(|v| v.name().to_string())
            .collect()
    }

    /// Clone all registered verifiers (for use across await boundaries).
    pub fn all_verifiers(&self) -> Vec<Arc<dyn Verifier>> {
        self.verifiers.clone()
    }
}
