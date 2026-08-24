//! bucketlist 3.11: end-to-end cross-test of the two coexisting verifier
//! systems. The event-driven `Verifier` path (`VerifierHandler` →
//! `VerifierSlots`) and the `BusVerifier` path (`VerifierBus` →
//! `VerifyContext`) take different inputs (`&BusEvent` vs `&VerifyContext`)
//! but must agree on the same file change. This module fires a `FileWrite`
//! event through both and asserts non-conflicting verdicts.

use crate::session::access::PathGuard;
use crate::session::verifier::bus::{BusVerifier, VerdictEntry, VerifierBus, VerifyContext};
use crate::session::verifier::handler::VerifierHandler;
use crate::session::verifier::slots::VerifierSlots;
use crate::session::verifier::types::{BusEvent, FileWriteEvent};
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
    let path = PathBuf::from("/tmp/kf_code_cross_test.rs");

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
    bus.register(Box::new(CleanBusVerifier));
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

// WO 44.29: `rebuild_plugin_verifiers` retains built-ins via a hand-maintained
// allowlist (`BUILTIN_VERIFIERS`). WO 32.20 added 5 verifier registrations to
// `init_default_verifiers` but never extended the list, so the first `/plugins`
// reload silently dropped node_test/node_lint/go_test/go_vet/generic_test.
// This test registers the full default set on a fresh executor, runs a reload
// with an empty plugin registry, and asserts every built-in survived — so the
// next added verifier fails here instead of silently regressing.
#[tokio::test]
async fn rebuild_plugin_verifiers_keeps_every_built_in() {
    use super::common::{make_config, make_executor, make_info, MockAdapter};
    use crate::shared::{FinishReason, StreamEvent};

    let adapter = Box::new(MockAdapter::new(
        vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: None,
        }],
        make_info(),
    ));
    let mut executor =
        make_executor(adapter, vec![], make_config(false)).expect("build executor");

    // init_default_verifiers ran in the constructor; collect the names it
    // registered so the assertion below tracks the real registration set,
    // not a second hand-maintained list (which would drift the same way).
    let before = executor
        .correction_loop
        .as_ref()
        .expect("correction_loop set by init_default_verifiers")
        .verifier_handler()
        .slots()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .names();

    // Reload with an empty registry: no plugin verifiers, but every built-in
    // must survive the retain.
    let empty_registry = kf_plugin_host::PluginRegistry::new();
    let plugin_added = executor.rebuild_plugin_verifiers(&empty_registry);
    assert_eq!(plugin_added, 0, "empty registry adds no plugin verifiers");

    let after = executor
        .correction_loop
        .as_ref()
        .expect("correction_loop still present")
        .verifier_handler()
        .slots()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .names();

    assert_eq!(
        after, before,
        "rebuild_plugin_verifiers dropped a built-in: before={before:?} after={after:?}.\n\
         BUILTIN_VERIFIERS is out of sync with init_default_verifiers — add the missing name."
    );

    // Explicit belt-and-braces: the WO 32.20 five must be present. If this
    // fires, the fix regressed; if `before` lacks them, init_default_verifiers
    // itself dropped a registration.
    for required in [
        "security",
        "lint",
        "build",
        "git",
        "rustfmt",
        "test",
        "python_test",
        "python_lint",
        "python_typecheck",
        "node_test",
        "node_lint",
        "go_test",
        "go_vet",
        "generic_test",
    ] {
        assert!(
            after.iter().any(|n| n == required),
            "built-in `{required}` missing after rebuild_plugin_verifiers; after={after:?}"
        );
    }
}
