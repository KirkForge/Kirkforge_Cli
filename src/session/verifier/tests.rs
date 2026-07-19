use super::*;
use crate::session::event_bus::{BusEvent, EditEvent, EventBus};
use crate::shared::test_util::remove_test_file;
use std::path::PathBuf;
use std::sync::Arc;

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
                command: None,
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
            command: None,
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
async fn test_correction_loop_returns_suggestion_when_no_fix_available() {
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = Arc::new(VerifierHandler::new(
        slots.clone(),
        crate::session::access::PathGuard::default(),
    ));
    {
        let mut s = slots.write().unwrap();
        s.register(Arc::new(MockVerifier {
            name: "lint".into(),
            prio: 1,
            verdict: Verdict::Fixable(FixSuggestion {
                description: "ambiguous issue".into(),
                file: PathBuf::from("src/lib.rs"),
                original: "".into(),
                replacement: "".into(),
                severity: "warning".into(),
                command: None,
            }),
        }))
        .unwrap();
    }

    let loop_ = CorrectionLoop::new(handler);
    let event = make_edit_event();
    let results = loop_.run(&event).await;
    assert_eq!(results.len(), 1);
    assert!(
        results[0].success,
        "suggestion should be reported as success"
    );
    assert!(results[0].message.contains("Verifier suggestion"));
    assert!(results[0].message.contains("ambiguous issue"));
}

#[tokio::test]
async fn test_correction_loop_runs_command_fix() {
    let dir = std::env::temp_dir();
    let path = dir.join("kirkforge_command_fix.txt");
    std::fs::write(&path, "hello world").unwrap();

    struct OnceCommandVerifier {
        file: PathBuf,
        fired: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl Verifier for OnceCommandVerifier {
        fn name(&self) -> &str {
            "rustfmt"
        }
        fn priority(&self) -> u8 {
            1
        }
        async fn verify(&self, _event: &BusEvent) -> Verdict {
            if self.fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                return Verdict::Clean;
            }
            Verdict::Fixable(FixSuggestion {
                description: "not formatted".into(),
                file: self.file.clone(),
                original: "".into(),
                replacement: "".into(),
                severity: "warning".into(),
                command: Some("true".into()),
            })
        }
    }

    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = Arc::new(VerifierHandler::new(
        slots.clone(),
        crate::session::access::PathGuard::default(),
    ));
    {
        let mut s = slots.write().unwrap();
        s.register(Arc::new(OnceCommandVerifier {
            file: path.clone(),
            fired: std::sync::atomic::AtomicBool::new(false),
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
    assert!(results[0].message.contains("Auto-formatted"));

    remove_test_file(&path);
}

#[tokio::test]
async fn test_correction_loop_unfixable_stops() {
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = Arc::new(VerifierHandler::new(
        slots.clone(),
        crate::session::access::PathGuard::default(),
    ));
    {
        let mut s = slots.write().unwrap();
        s.register(Arc::new(MockVerifier {
            name: "security".into(),
            prio: 1,
            verdict: Verdict::Unfixable(VerificationError {
                description: "secret found".into(),
                file: None,
                details: "sk-...".into(),
            }),
        }))
        .unwrap();
    }

    let loop_ = CorrectionLoop::new(handler);
    let event = make_edit_event();
    let results = loop_.run(&event).await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].success);
    assert!(results[0].message.contains("Verification failed"));
}

#[tokio::test]
async fn test_verifier_handler_drain_corrections() {
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = VerifierHandler::new(slots.clone(), crate::session::access::PathGuard::default());

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
                command: None,
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
                    command: None,
                });
            }
        }
        Verdict::Clean
    }
}

#[tokio::test]
async fn test_correction_loop_applies_and_returns() {
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = Arc::new(VerifierHandler::new(
        slots.clone(),
        crate::session::access::PathGuard::default(),
    ));

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

    remove_test_file(&path);
}

#[tokio::test]
async fn test_verifier_handler_event_bus_integration() {
    let bus = EventBus::new();
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let handler = Arc::new(VerifierHandler::new(
        slots.clone(),
        crate::session::access::PathGuard::default(),
    ));

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
