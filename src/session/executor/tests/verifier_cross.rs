//! bucketlist 3.11: end-to-end cross-test of the two coexisting verifier
//! systems. The event-driven `Verifier` path (`VerifierHandler` →
//! `VerifierSlots`) and the `BusVerifier` path (`VerifierBus` →
//! `VerifyContext`) take different inputs (`&BusEvent` vs `&VerifyContext`)
//! but must agree on the same file change. This module fires a `FileWrite`
//! event through both and asserts non-conflicting verdicts.

use crate::session::access::PathGuard;
use crate::session::event_bus::{BusEvent, FileWriteEvent};
use crate::session::verifier::bus::{BusVerifier, VerdictEntry, VerifierBus, VerifyContext};
use crate::session::verifier::handler::VerifierHandler;
use crate::session::verifier::slots::VerifierSlots;
use crate::session::verifier::{Verdict, Verifier};
use std::path::PathBuf;
use std::sync::Arc;

struct CleanEventVerifier;
#[async_trait::async_trait]
impl Verifier for CleanEventVerifier {
    fn name(&self) -> &str {
        "cross-event"
    }
    fn priority(&self) -> u8 {
        1
    }
    async fn verify(&self, event: &BusEvent) -> Verdict {
        // Non-conflicting contract: a FileWrite of a benign file is Clean.
        match event {
            BusEvent::FileWrite(FileWriteEvent { path, .. }) => {
                assert!(path.as_os_str() != "", "event path must be non-empty");
                Verdict::Clean
            }
            _ => Verdict::Skipped("not a file_write".into()),
        }
    }
}

struct CleanBusVerifier;
impl BusVerifier for CleanBusVerifier {
    fn name(&self) -> &str {
        "cross-bus"
    }
    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // Same file, same verdict shape: no findings.
        assert!(
            !ctx.changed_files.is_empty(),
            "bus context must carry files"
        );
        Vec::new()
    }
}

#[tokio::test]
async fn write_file_event_is_non_conflicting_across_both_verifier_paths() {
    let path = PathBuf::from("/tmp/kirkforge_cross_test.rs");

    // ── Event-driven path ──
    let mut slots = VerifierSlots::new();
    slots
        .register(Arc::new(CleanEventVerifier))
        .expect("register CleanEventVerifier");
    let handler = VerifierHandler::new(
        Arc::new(std::sync::RwLock::new(slots)),
        PathGuard::default(),
    );
    let event = BusEvent::FileWrite(FileWriteEvent {
        path: path.clone(),
        content_length: 42,
        content_hash: 0,
    });
    let (event_verdict, event_name) = handler.verify_event(&event).await;
    assert!(
        matches!(event_verdict, Verdict::Clean),
        "event-driven path should be Clean, got {event_verdict:?}"
    );
    assert_eq!(event_name, "aggregate", "Clean → no decisive verifier name");

    // ── Bus path ──
    let mut bus = VerifierBus::new();
    bus.register(Box::new(CleanBusVerifier))
        .expect("register CleanBusVerifier");
    let ctx = VerifyContext {
        sandbox_dir: PathBuf::from("/tmp"),
        changed_files: vec![path.clone()],
    };
    bus.run(&ctx);
    assert!(
        !bus.has_errors(),
        "bus path should have no errors for the same file"
    );
    assert!(
        bus.verdicts().is_empty(),
        "bus path should produce no verdict entries for a clean file"
    );

    // ── Non-conflict ──
    // Both paths agree the benign write is clean: the event path returns
    // `Verdict::Clean` and the bus path returns zero error verdicts.
}
