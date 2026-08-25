use super::*;
use crate::session::executor::types::VerificationOutcome;
use crate::session::verifier::types::{EditEvent, ToolErrorEvent};
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
async fn test_fixable_verdict_collects_all_findings() {
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
                line: None,
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
                line: None,
            }),
        }))
        .unwrap();

    // verify() returns the most severe (Unfixable > Fixable)
    let verdict = slots.verify(&make_edit_event()).await;
    assert!(matches!(verdict, Verdict::Unfixable(_)));

    // verify_all() collects every finding
    let all = slots.verify_all(&make_edit_event()).await;
    assert_eq!(all.len(), 2, "both verifiers should report findings");
}

#[tokio::test]
async fn test_unfixable_is_most_severe() {
    let mut slots = VerifierSlots::new();
    slots
        .register(Arc::new(MockVerifier {
            name: "security".into(),
            prio: 1,
            verdict: Verdict::Unfixable(VerificationError {
                description: "API key exposed".into(),
                file: Some(PathBuf::from("config.rs")),
                details: "found sk-... pattern".into(),
                line: None,
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
            line: None,
        }),
    }));
    assert!(err.is_err(), "Should reject duplicate verifier name");
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
                line: None,
            }),
        }))
        .unwrap();
    }

    let loop_ = CorrectionLoop::new(handler);
    let event = make_edit_event();
    let results = loop_.run(&event).await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].outcome,
        VerificationOutcome::Suggestion,
        "suggestion should be reported as Suggestion"
    );
    assert!(results[0].message.contains("Verifier suggestion"));
    assert!(results[0].message.contains("ambiguous issue"));
}

#[tokio::test]
async fn test_correction_loop_runs_command_fix() {
    let dir = std::env::temp_dir();
    let path = dir.join("kf_code_command_fix.txt");
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
                line: None,
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
    assert_eq!(
        results[0].outcome,
        VerificationOutcome::Fixed,
        "command fix should report Fixed"
    );
    assert!(results[0].message.contains("Auto-formatted"));

    remove_test_file(&path);
}

#[tokio::test]
async fn test_correction_loop_stops_at_max_iterations() {
    // bucketlist 3.40: a verifier that is always Fixable (command fix
    // keeps "succeeding") drives the loop to its max_iterations cap (3)
    // and then stops — proving the bound is enforced.
    let dir = std::env::temp_dir();
    let path = dir.join("kf_code_max_iter_test.txt");
    std::fs::write(&path, "hello world").unwrap();

    struct AlwaysFixableCommandVerifier {
        file: PathBuf,
    }
    #[async_trait::async_trait]
    impl Verifier for AlwaysFixableCommandVerifier {
        fn name(&self) -> &str {
            "rustfmt"
        }
        fn priority(&self) -> u8 {
            1
        }
        async fn verify(&self, _event: &BusEvent) -> Verdict {
            Verdict::Fixable(FixSuggestion {
                description: "not formatted".into(),
                file: self.file.clone(),
                original: "".into(),
                replacement: "".into(),
                severity: "warning".into(),
                command: Some("true".into()),
                line: None,
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
        s.register(Arc::new(AlwaysFixableCommandVerifier {
            file: path.clone(),
        }))
        .unwrap();
    }

    let loop_ = CorrectionLoop::new(handler);
    assert_eq!(loop_.max_iterations(), 3, "default cap is 3");
    let event = BusEvent::Edit(EditEvent {
        path: path.clone(),
        diff: "@@ -1 +1 @@".into(),
    });
    let results = loop_.run(&event).await;
    assert_eq!(
        results.len(),
        3,
        "loop must stop at max_iterations (3), got {}",
        results.len()
    );
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.outcome,
            VerificationOutcome::Fixed,
            "iteration {i}: command fix should report Fixed"
        );
        assert!(
            r.message.contains("Auto-formatted"),
            "iteration {i}: expected formatter message, got {}",
            r.message
        );
    }

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
                line: None,
            }),
        }))
        .unwrap();
    }

    let loop_ = CorrectionLoop::new(handler);
    let event = make_edit_event();
    let results = loop_.run(&event).await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].outcome,
        VerificationOutcome::Failed,
        "Unfixable should report Failed"
    );
    assert!(results[0].message.contains("Verification failed"));
}

#[tokio::test]
async fn test_verifier_handler_returns_fixable_suggestion() {
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
                line: None,
            }),
        }))
        .unwrap();
    }

    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;

    // WO 43.37: pending_corrections queue was removed (write-only dead state).
    // The real contract is that verify_event returns the Fixable verdict
    // carrying the suggestion — the correction loop consumes this directly.
    assert_eq!(name, "lint");
    match verdict {
        Verdict::Fixable(fix) => assert_eq!(fix.description, "test fix"),
        other => panic!("expected Fixable, got {other:?}"),
    }
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
                    line: None,
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
    let path = dir.join("kf_code_correction_loop.txt");
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
    assert_eq!(
        results[0].outcome,
        VerificationOutcome::Fixed,
        "applied text fix should report Fixed"
    );
    assert!(results[0].message.contains("Auto-fixed"));

    // Verify file was actually fixed
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "let _x = 1;");

    remove_test_file(&path);
}

// ── VerifierHandler tests (WO 12-series coverage) ──────────────────────

use super::handler::VerifierHandler;
use crate::session::access::PathGuard;
use crate::session::verifier::slots::VerifierSlots;

#[tokio::test]
async fn handler_verify_event_clean_verdict() {
    let slots = Arc::new(std::sync::RwLock::new(VerifierSlots::new()));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;
    assert!(matches!(verdict, Verdict::Clean));
    assert_eq!(name, "aggregate");
}

#[tokio::test]
async fn handler_verify_event_fixable_verdict() {
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "fix-verifier".into(),
        prio: 1,
        verdict: Verdict::Fixable(FixSuggestion {
            description: "unused import".into(),
            file: PathBuf::from("test.rs"),
            original: "use foo;".into(),
            replacement: "".into(),
            severity: "low".into(),
            command: None,
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;
    assert!(matches!(verdict, Verdict::Fixable(_)));
    assert_eq!(name, "fix-verifier");
}

#[tokio::test]
async fn handler_verify_event_unfixable_verdict() {
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "strict-verifier".into(),
        prio: 1,
        verdict: Verdict::Unfixable(super::VerificationError {
            description: "syntax error".into(),
            file: None,
            details: "missing semicolon".into(),
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;
    assert!(matches!(verdict, Verdict::Unfixable(_)));
    assert_eq!(name, "strict-verifier");
}

#[tokio::test]
async fn handler_verify_event_skipped_verdict() {
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "skip-verifier".into(),
        prio: 1,
        verdict: Verdict::Skipped("not applicable".into()),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, _) = handler.verify_event(&event).await;
    // Skipped is treated as Clean by the verify_event loop (continue),
    // so the aggregate verdict is Clean.
    assert!(matches!(verdict, Verdict::Clean));
}

#[tokio::test]
async fn handler_tool_error_event_short_circuits_without_fanout() {
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "would-fail".into(),
        prio: 1,
        verdict: Verdict::Unfixable(super::VerificationError {
            description: "should not run".into(),
            file: None,
            details: "ToolError must short-circuit before the fan-out".into(),
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let event = BusEvent::ToolError(ToolErrorEvent {
        tool: "bash".into(),
        error: "exit code 1".into(),
    });

    let (verdict, name) = handler.verify_event(&event).await;
    assert!(
        matches!(&verdict, Verdict::Skipped(_)),
        "ToolError should short-circuit to Skipped, got {verdict:?}"
    );
    assert_eq!(name, "aggregate");
}

#[tokio::test]
async fn handler_verify_event_returns_fixable_suggestion() {
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "fix-verifier".into(),
        prio: 1,
        verdict: Verdict::Fixable(FixSuggestion {
            description: "unused import".into(),
            file: PathBuf::from("test.rs"),
            original: "use foo;".into(),
            replacement: "".into(),
            severity: "low".into(),
            command: None,
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;
    assert_eq!(name, "fix-verifier");
    match verdict {
        Verdict::Fixable(fix) => assert_eq!(fix.description, "unused import"),
        other => panic!("expected Fixable, got {other:?}"),
    }
}

#[tokio::test]
async fn handler_verify_event_fixable_is_not_cached() {
    // WO 43.37: Fixable/Unfixable verdicts are never cached (disk content
    // changes after a correction). Re-running verify_event on the same
    // event must re-run the verifier, not return a stale verdict.
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(MockVerifier {
        name: "fix-verifier".into(),
        prio: 1,
        verdict: Verdict::Fixable(FixSuggestion {
            description: "unused import".into(),
            file: PathBuf::from("test.rs"),
            original: "use foo;".into(),
            replacement: "".into(),
            severity: "low".into(),
            command: None,
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (first, _) = handler.verify_event(&event).await;
    let (second, _) = handler.verify_event(&event).await;
    assert!(matches!(first, Verdict::Fixable(_)));
    assert!(
        matches!(second, Verdict::Fixable(_)),
        "Fixable must not be cached"
    );
}

// A verifier that never resolves — simulates a wedged `cargo build`. The
// test build shrinks `VERIFIER_TIMEOUT` to 50ms so this fires fast without
// tokio's `test-util` mock clock.
struct HangingVerifier;

#[async_trait::async_trait]
impl Verifier for HangingVerifier {
    fn name(&self) -> &str {
        "hanging"
    }
    fn priority(&self) -> u8 {
        1
    }
    async fn verify(&self, _event: &BusEvent) -> Verdict {
        std::future::pending::<()>().await;
        Verdict::Clean
    }
}

#[tokio::test]
async fn handler_verify_event_times_out_slow_verifier() {
    // A wedged verifier is bounded by `VERIFIER_TIMEOUT` (50ms in tests), so
    // it must NOT hang the turn or starve sibling verifiers. We register a
    // HangingVerifier alongside a Fixable one: if the timeout works, the
    // hanging verifier is skipped (→ no finding) and the sibling still runs,
    // yielding a Fixable aggregate. Without the timeout, `verify_event`
    // would hang on the pending verifier and the test would time out.
    let mut s = VerifierSlots::new();
    let _ = s.register(Arc::new(HangingVerifier));
    let _ = s.register(Arc::new(MockVerifier {
        name: "lint".into(),
        prio: 2,
        verdict: Verdict::Fixable(FixSuggestion {
            description: "unused import".into(),
            file: PathBuf::from("test.rs"),
            original: "use foo;".into(),
            replacement: "".into(),
            severity: "low".into(),
            command: None,
            line: None,
        }),
    }));
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);
    let event = make_edit_event();
    let (verdict, name) = handler.verify_event(&event).await;
    match verdict {
        Verdict::Fixable(_) => assert_eq!(name, "lint"),
        other => panic!("expected Fixable from the sibling, got {other:?}"),
    }
}

// ── Verdict cache tests (WO 42.11 / WO 42.6 item 2) ─────────────────────
//
// `content_hash` is computed at dispatch time but was never read. The verdict
// cache keys on (file_path, content_hash) so a re-verify of unchanged content
// skips re-running cargo build/clippy/test. Only Clean/Skipped verdicts are
// cached — Fixable/Unfixable are not (the correction loop re-verifies after a
// fix, and disk content has changed by then).

use crate::session::verifier::types::FileWriteEvent;

/// Verifier that counts how many times `verify` was called. Returns `Clean`
/// so the verdict is cacheable.
struct CountingVerifier {
    name: String,
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingVerifier {
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Verifier for CountingVerifier {
    fn name(&self) -> &str {
        &self.name
    }
    fn priority(&self) -> u8 {
        1
    }
    async fn verify(&self, _event: &BusEvent) -> Verdict {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Verdict::Clean
    }
}

fn make_file_write_event(hash: u64) -> BusEvent {
    BusEvent::FileWrite(FileWriteEvent {
        path: PathBuf::from("/tmp/kf_code_cache_test.rs"),
        content_length: 10,
        content_hash: hash,
    })
}

#[tokio::test]
async fn verdict_cache_same_content_hash_skips_reverification() {
    // Same file+content_hash → verifier runs once, second call is a cache hit.
    let mut s = VerifierSlots::new();
    let counter = Arc::new(CountingVerifier {
        name: "build".into(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    s.register(counter.clone()).unwrap();
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let event = make_file_write_event(42);
    let _ = handler.verify_event(&event).await;
    assert_eq!(counter.calls(), 1, "first call runs the verifier");

    let _ = handler.verify_event(&event).await;
    assert_eq!(
        counter.calls(),
        1,
        "second call with same content_hash must hit the cache, not re-run"
    );
}

#[tokio::test]
async fn verdict_cache_different_content_hash_runs_verifier_again() {
    // Different content_hash → cache miss → verifier runs again.
    let mut s = VerifierSlots::new();
    let counter = Arc::new(CountingVerifier {
        name: "build".into(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    s.register(counter.clone()).unwrap();
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let _ = handler.verify_event(&make_file_write_event(1)).await;
    assert_eq!(counter.calls(), 1, "first content_hash runs the verifier");

    let _ = handler.verify_event(&make_file_write_event(2)).await;
    assert_eq!(
        counter.calls(),
        2,
        "different content_hash must miss the cache and re-run the verifier"
    );
}

#[tokio::test]
async fn verdict_cache_zero_hash_never_caches() {
    // content_hash == 0 means the event predates the hash wiring (or the
    // producer couldn't compute it). Such events must always run verifiers
    // and never populate the cache — otherwise a 0-hash entry would shadow
    // a later real-hash event for the same path.
    let mut s = VerifierSlots::new();
    let counter = Arc::new(CountingVerifier {
        name: "build".into(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    s.register(counter.clone()).unwrap();
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let event = make_file_write_event(0);
    let _ = handler.verify_event(&event).await;
    let _ = handler.verify_event(&event).await;
    assert_eq!(
        counter.calls(),
        2,
        "content_hash == 0 must never hit the cache"
    );
}

#[tokio::test]
async fn verdict_cache_invalidate_after_fix_clears_entry() {
    // After the correction loop applies a fix, the cached Clean for that path
    // is stale (disk content changed). `invalidate_cache` must drop it so the
    // next verify_event re-runs verifiers.
    let mut s = VerifierSlots::new();
    let counter = Arc::new(CountingVerifier {
        name: "build".into(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    s.register(counter.clone()).unwrap();
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let path = PathBuf::from("/tmp/kf_code_cache_invalidate.rs");
    let event = BusEvent::FileWrite(FileWriteEvent {
        path: path.clone(),
        content_length: 10,
        content_hash: 99,
    });
    let _ = handler.verify_event(&event).await;
    assert_eq!(counter.calls(), 1);

    // Cache hit before invalidation.
    let _ = handler.verify_event(&event).await;
    assert_eq!(counter.calls(), 1, "cache hit before invalidation");

    // After a fix, the loop invalidates → next call re-runs.
    handler.invalidate_cache(&path);
    let _ = handler.verify_event(&event).await;
    assert_eq!(
        counter.calls(),
        2,
        "invalidate_cache must drop the entry so the verifier re-runs"
    );
}

#[tokio::test]
async fn verdict_cache_fixable_verdict_not_cached() {
    // Fixable verdicts must not be cached: the correction loop re-verifies
    // after applying a fix, and re-running the verifier is the whole point
    // (it should now see the fixed content and return Clean).
    struct AlwaysFixable;
    #[async_trait::async_trait]
    impl Verifier for AlwaysFixable {
        fn name(&self) -> &str {
            "lint"
        }
        fn priority(&self) -> u8 {
            1
        }
        async fn verify(&self, _event: &BusEvent) -> Verdict {
            Verdict::Fixable(FixSuggestion {
                description: "unused".into(),
                file: PathBuf::from("/tmp/kf_code_cache_fixable.rs"),
                original: "a".into(),
                replacement: "b".into(),
                severity: "low".into(),
                command: None,
                line: None,
            })
        }
    }

    let mut s = VerifierSlots::new();
    s.register(Arc::new(AlwaysFixable)).unwrap();
    let slots = Arc::new(std::sync::RwLock::new(s));
    let guard = PathGuard::default();
    let handler = VerifierHandler::new(slots, guard);

    let event = make_file_write_event(7);
    let (v1, _) = handler.verify_event(&event).await;
    assert!(matches!(v1, Verdict::Fixable(_)));
    let (v2, _) = handler.verify_event(&event).await;
    assert!(
        matches!(v2, Verdict::Fixable(_)),
        "Fixable must not be cached — the correction loop needs to re-verify after a fix"
    );
}
