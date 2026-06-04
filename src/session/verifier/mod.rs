pub mod git;
/// Verifier slots — deterministic post-execution checks and correction loop.
///
/// Verifiers sit on the event bus and react to tool execution events.
/// Unlike model-based tool calling, verifiers run deterministic checks:
///
/// - **Lint verifier**: runs linter on edited files
/// - **Type-check verifier**: runs type checker on changed code
/// - **Git verifier**: validates git state after operations
/// - **Security verifier**: scans written files for dangerous patterns
///
/// # Correction loop
///
/// When a verifier finds an issue, it emits a [`Verification`] which
/// the correction loop processes. The correction loop decides whether
/// to auto-fix (deterministic) or report back to the model.
///
/// # Truth model
///
/// When multiple verifiers disagree, precedence rules determine which
/// result is authoritative. The system runs verifiers in priority order
/// and stops at the first definitive result.
pub mod lint;
pub mod security;

use crate::session::event_bus::{BusEvent, EventHandler, EventKind, HandlerResult};
use std::path::PathBuf;
use std::sync::Arc;

// ── Verification result ─────────────────────────────────────────────────

/// The outcome of a verification.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Everything is clean — no issues found.
    Clean,
    /// Issues found that can be auto-corrected.
    Fixable(FixSuggestion),
    /// Issues found that require human or model attention.
    Unfixable(VerificationError),
    /// Verifier skipped (e.g., tool not available).
    Skipped(String),
}

/// A fix suggestion from a verifier.
#[derive(Debug, Clone)]
pub struct FixSuggestion {
    /// Human-readable description of the issue.
    pub description: String,
    /// The file that needs fixing.
    pub file: PathBuf,
    /// The original text to replace.
    pub original: String,
    /// The replacement text.
    pub replacement: String,
    /// Suggested severity: "error" | "warning" | "info"
    pub severity: String,
}

/// A verification error that can't be auto-corrected.
#[derive(Debug, Clone)]
pub struct VerificationError {
    pub description: String,
    pub file: Option<PathBuf>,
    pub details: String,
}

// ── Verifier trait ──────────────────────────────────────────────────────

/// A verifier performs deterministic checks on tool execution results.
///
/// Verifiers register as event bus handlers to react to specific events.
/// Unlike generic handlers, verifiers return a [`Verification`] that
/// the correction loop can act on.
#[async_trait::async_trait]
pub trait Verifier: Send + Sync {
    /// Unique verifier name (e.g. "lint", "type-check", "git", "security").
    fn name(&self) -> &str;

    /// Priority: lower number = higher priority (runs first).
    /// Used by the truth model — the first definitive result wins.
    fn priority(&self) -> u8;

    /// Verify the state after a tool event.
    ///
    /// Implementations should be fast and deterministic — this runs
    /// synchronously in the tool execution pipeline.
    async fn verify(&self, event: &BusEvent) -> Verdict;
}

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
            anyhow::bail!("Verifier '{}' is already registered", name);
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

// ── EventBus integration ────────────────────────────────────────────────

/// Wraps a [`VerifierSlots`] as an [`EventHandler`] for the event bus.
///
/// This bridges the verifier system onto the event bus so that verifiers
/// get triggered automatically when tool events fire.
pub struct VerifierHandler {
    slots: Arc<std::sync::RwLock<VerifierSlots>>,
    /// Correction results that verifiers produced — consumed by correction loop.
    pending_corrections: Arc<tokio::sync::Mutex<Vec<FixSuggestion>>>,
}

impl VerifierHandler {
    pub fn new(slots: Arc<std::sync::RwLock<VerifierSlots>>) -> Self {
        Self {
            slots,
            pending_corrections: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    /// Drain pending corrections (consumed by the correction loop).
    pub async fn drain_corrections(&self) -> Vec<FixSuggestion> {
        let mut pending = self.pending_corrections.lock().await;
        std::mem::take(&mut *pending)
    }

    /// Run verification and return the verdict (used by correction loop).
    pub async fn verify_event(&self, event: &BusEvent) -> Verdict {
        // Extract verifiers from the lock to avoid holding it across .await
        let verifiers: Vec<Arc<dyn Verifier>> = {
            let slots = self.slots.read().unwrap();
            if slots.is_empty() {
                return Verdict::Clean;
            }
            slots.all_verifiers()
        };

        // Run truth-model precedence: first non-clean, non-skipped wins
        let verdict = {
            let mut verdict = Verdict::Clean;
            for verifier in &verifiers {
                let v = verifier.verify(event).await;
                match &v {
                    Verdict::Clean | Verdict::Skipped(_) => continue,
                    Verdict::Fixable(_) | Verdict::Unfixable(_) => {
                        verdict = v;
                        break;
                    }
                }
            }
            verdict
        };

        // Collect fixable suggestions
        if let Verdict::Fixable(ref fix) = verdict {
            let mut pending = self.pending_corrections.lock().await;
            pending.push(fix.clone());
        }

        verdict
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
        let verdict = self.verify_event(event).await;
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

// ── Correction Loop ─────────────────────────────────────────────────────

/// Manages the correction loop: after tool execution, check verifiers,
/// apply auto-fixes, and report results back to the conversation.
pub struct CorrectionLoop {
    verifier_handler: Arc<VerifierHandler>,
    max_iterations: usize,
}

impl CorrectionLoop {
    /// Create a new correction loop.
    pub fn new(verifier_handler: Arc<VerifierHandler>) -> Self {
        Self {
            verifier_handler,
            max_iterations: 3,
        }
    }

    /// Create with a custom iteration limit.
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        Self { ..self }
    }

    /// Run the correction loop after a tool execution event.
    ///
    /// Re-checks after each auto-fix to catch cascading issues.
    /// Returns a list of correction messages that should be appended to
    /// the conversation as tool results.
    pub async fn run(&self, event: &BusEvent) -> Vec<CorrectionResult> {
        let mut results = Vec::new();

        for _iteration in 0..self.max_iterations {
            let verdict = self.verifier_handler.verify_event(event).await;
            match verdict {
                Verdict::Clean | Verdict::Skipped(_) => break,
                Verdict::Fixable(fix) => {
                    let applied = apply_fix(&fix).await;
                    results.push(CorrectionResult {
                        verifier: "auto-fix".into(),
                        success: applied,
                        message: if applied {
                            format!("Auto-fixed: {} — {}", fix.severity, fix.description)
                        } else {
                            format!(
                                "Failed to auto-fix: {} — {}",
                                fix.severity, fix.description
                            )
                        },
                        fix: Some(fix),
                    });
                    if !applied {
                        break; // can't fix → stop looping
                    }
                }
                Verdict::Unfixable(err) => {
                    results.push(CorrectionResult {
                        verifier: "verifier".into(),
                        success: false,
                        message: format!(
                            "Verification failed: {} — {}",
                            err.description, err.details
                        ),
                        fix: None,
                    });
                    break; // unfixable → stop
                }
            }
        }

        results
    }

    pub fn max_iterations(&self) -> usize {
        self.max_iterations
    }
}

/// Result of a correction attempt.
#[derive(Debug, Clone)]
pub struct CorrectionResult {
    pub verifier: String,
    pub success: bool,
    pub message: String,
    pub fix: Option<FixSuggestion>,
}

/// Apply a fix suggestion to the filesystem.
/// Replaces only the first occurrence of the original text.
async fn apply_fix(fix: &FixSuggestion) -> bool {
    let path = &fix.file;
    if !path.exists() {
        return false;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if !content.contains(&fix.original) {
        return false;
    }
    let new_content = content.replacen(&fix.original, &fix.replacement, 1);
    std::fs::write(path, new_content).is_ok()
}

/// Determine which event kinds a verifier should subscribe to.
/// Convenience helper for creating event-bus subscriptions.
pub fn verifier_event_kinds(verifier_name: &str) -> Vec<EventKind> {
    match verifier_name {
        "lint" => vec![EventKind::Edit, EventKind::FileWrite],
        "type-check" => vec![EventKind::Edit, EventKind::FileWrite],
        "git" => vec![EventKind::GitOperation, EventKind::BashExec],
        "security" => vec![EventKind::FileWrite, EventKind::BashExec],
        _ => vec![],
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::event_bus::EditEvent;

    struct MockVerifier {
        name: String,
        prio: u8,
        verdict: Verdict,
    }

    #[async_trait::async_trait]
    impl Verifier for MockVerifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> u8 {
            self.prio
        }
        async fn verify(&self, _event: &BusEvent) -> Verdict {
            self.verdict.clone()
        }
    }

    fn make_edit_event() -> BusEvent {
        BusEvent::Edit(EditEvent {
            path: PathBuf::from("/tmp/test.rs"),
            diff: "@@ -1 +1 @@\n-foo\n+bar".into(),
        })
    }

    #[tokio::test]
    async fn test_empty_slots_return_clean() {
        let slots = VerifierSlots::new();
        let verdict = slots.verify(&make_edit_event()).await;
        assert!(matches!(verdict, Verdict::Clean));
    }

    #[tokio::test]
    async fn test_fixable_verdict_stops_at_first() {
        let mut slots = VerifierSlots::new();
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Fixable(FixSuggestion {
                    description: "unused variable".into(),
                    file: PathBuf::from("test.rs"),
                    original: "let x = 1;".into(),
                    replacement: "let _x = 1;".into(),
                    severity: "warning".into(),
                }),
            }))
            .unwrap();
        slots
            .register(Arc::new(MockVerifier {
                name: "security".into(),
                prio: 2,
                verdict: Verdict::Unfixable(VerificationError {
                    description: "dangerous".into(),
                    file: None,
                    details: "hardcoded password".into(),
                }),
            }))
            .unwrap();

        let verdict = slots.verify(&make_edit_event()).await;
        // Should stop at lint (priority 1) even though security would also fire
        assert!(matches!(verdict, Verdict::Fixable(_)));
    }

    #[tokio::test]
    async fn test_unfixable_stops_chain() {
        let mut slots = VerifierSlots::new();
        slots
            .register(Arc::new(MockVerifier {
                name: "security".into(),
                prio: 1,
                verdict: Verdict::Unfixable(VerificationError {
                    description: "API key exposed".into(),
                    file: Some(PathBuf::from("config.rs")),
                    details: "found sk-... pattern".into(),
                }),
            }))
            .unwrap();
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 2,
                verdict: Verdict::Clean,
            }))
            .unwrap();

        let verdict = slots.verify(&make_edit_event()).await;
        assert!(matches!(verdict, Verdict::Unfixable(_)));
    }

    #[tokio::test]
    async fn test_skipped_verifiers_are_skipped() {
        let mut slots = VerifierSlots::new();
        slots
            .register(Arc::new(MockVerifier {
                name: "git".into(),
                prio: 1,
                verdict: Verdict::Skipped("no git repo".into()),
            }))
            .unwrap();
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 2,
                verdict: Verdict::Clean,
            }))
            .unwrap();

        let verdict = slots.verify(&make_edit_event()).await;
        assert!(matches!(verdict, Verdict::Clean));
    }

    #[tokio::test]
    async fn test_register_overflow() {
        let mut slots = VerifierSlots::with_max_slots(1);
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Clean,
            }))
            .unwrap();
        let err = slots.register(Arc::new(MockVerifier {
            name: "security".into(),
            prio: 2,
            verdict: Verdict::Clean,
        }));
        assert!(err.is_err(), "Should reject when all slots filled");
    }

    #[tokio::test]
    async fn test_duplicate_registration_rejected() {
        let mut slots = VerifierSlots::new();
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Clean,
            }))
            .unwrap();
        let err = slots.register(Arc::new(MockVerifier {
            name: "lint".into(),
            prio: 1,
            verdict: Verdict::Fixable(FixSuggestion {
                description: "dup".into(),
                file: PathBuf::from("x.rs"),
                original: "a".into(),
                replacement: "b".into(),
                severity: "error".into(),
            }),
        }));
        assert!(err.is_err(), "Should reject duplicate verifier name");
    }

    #[tokio::test]
    async fn test_unregister_by_name() {
        let mut slots = VerifierSlots::new();
        slots
            .register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Clean,
            }))
            .unwrap();
        assert_eq!(slots.len(), 1);
        assert!(slots.unregister("lint"));
        assert_eq!(slots.len(), 0);
        assert!(!slots.unregister("nonexistent"));
    }

    #[tokio::test]
    async fn test_apply_fix_basic() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fix_test.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        let fix = FixSuggestion {
            description: "unused variable".into(),
            file: path.clone(),
            original: "let x = 1;".into(),
            replacement: "let _x = 1;".into(),
            severity: "warning".into(),
        };

        assert!(apply_fix(&fix).await);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "let _x = 1;");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_apply_fix_nonexistent_file() {
        let fix = FixSuggestion {
            description: "fix".into(),
            file: PathBuf::from("/tmp/kirkforge_nonexistent_fix.txt"),
            original: "old".into(),
            replacement: "new".into(),
            severity: "warning".into(),
        };
        assert!(!apply_fix(&fix).await);
    }

    #[tokio::test]
    async fn test_apply_fix_original_not_found() {
        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_fix_nomatch.txt");
        std::fs::write(&path, "hello world").unwrap();

        let fix = FixSuggestion {
            description: "fix".into(),
            file: path.clone(),
            original: "not present".into(),
            replacement: "replacement".into(),
            severity: "error".into(),
        };
        assert!(!apply_fix(&fix).await);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_verifier_handler_drain_corrections() {
        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
        let handler = VerifierHandler::new(slots.clone());

        {
            let mut s = slots.write().unwrap();
            s.register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Fixable(FixSuggestion {
                    description: "test fix".into(),
                    file: PathBuf::from("x.rs"),
                    original: "a".into(),
                    replacement: "b".into(),
                    severity: "warning".into(),
                }),
            }))
            .unwrap();
        }

        let event = make_edit_event();
        let _ = handler.verify_event(&event).await;

        let corrections = handler.drain_corrections().await;
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].description, "test fix");

        // Second drain should be empty
        let empty = handler.drain_corrections().await;
        assert!(empty.is_empty());
    }

    /// A verifier that checks the actual file content and only returns Fixable
    /// if the old_string still exists — simulates a real verifier that stops
    /// flagging after the fix is applied.
    struct OnceVerifier {
        name: String,
        file: PathBuf,
        original: String,
        replacement: String,
    }

    #[async_trait::async_trait]
    impl Verifier for OnceVerifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> u8 {
            1
        }
        async fn verify(&self, _event: &BusEvent) -> Verdict {
            if let Ok(content) = std::fs::read_to_string(&self.file) {
                if content.contains(&self.original) {
                    return Verdict::Fixable(FixSuggestion {
                        description: "unused variable".into(),
                        file: self.file.clone(),
                        original: self.original.clone(),
                        replacement: self.replacement.clone(),
                        severity: "warning".into(),
                    });
                }
            }
            Verdict::Clean
        }
    }

    #[tokio::test]
    async fn test_correction_loop_applies_and_returns() {
        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
        let handler = Arc::new(VerifierHandler::new(slots.clone()));

        let dir = std::env::temp_dir();
        let path = dir.join("kirkforge_correction_loop.txt");
        std::fs::write(&path, "let x = 1;").unwrap();

        {
            let mut s = slots.write().unwrap();
            s.register(Arc::new(OnceVerifier {
                name: "lint".into(),
                file: path.clone(),
                original: "let x = 1;".into(),
                replacement: "let _x = 1;".into(),
            }))
            .unwrap();
        }

        let loop_ = CorrectionLoop::new(handler);
        let event = BusEvent::Edit(EditEvent {
            path: path.clone(),
            diff: "@@ -1 +1 @@".into(),
        });

        let results = loop_.run(&event).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].success);
        assert!(results[0].message.contains("Auto-fixed"));

        // Verify file was actually fixed
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "let _x = 1;");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_verifier_handler_event_bus_integration() {
        use crate::session::event_bus::EventBus;

        let bus = EventBus::new();
        let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
        let handler = Arc::new(VerifierHandler::new(slots.clone()));

        // Register as event bus handler
        bus.register(handler.clone()).await.unwrap();

        // Register a verifier
        {
            let mut s = slots.write().unwrap();
            s.register(Arc::new(MockVerifier {
                name: "lint".into(),
                prio: 1,
                verdict: Verdict::Clean,
            }))
            .unwrap();
        }

        // Dispatch via bus
        let event = BusEvent::Edit(EditEvent {
            path: PathBuf::from("/tmp/test.rs"),
            diff: "test diff".into(),
        });
        let results = bus.dispatch(&event).await;

        // VerifierHandler should have been called
        let verifier_results: Vec<_> = results
            .iter()
            .filter(|r| r.handler_id == "verifier")
            .collect();
        assert_eq!(verifier_results.len(), 1);
        assert_eq!(verifier_results[0].message, "All verifiers passed");
    }
}
