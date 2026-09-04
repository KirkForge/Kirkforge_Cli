//! WO 47.14: the two coexisting verifier systems are now unified onto
//! `BusVerifier`/`VerifierBus`. This module fires a `FileWrite`-equivalent
//! `VerifyContext` through the bus and asserts clean verdicts.

use crate::session::verifier::bus::{BusVerifier, VerdictEntry, VerifierBus, VerifyContext};
use std::path::PathBuf;

struct CleanBusVerifier;
impl BusVerifier for CleanBusVerifier {
    fn name(&self) -> &str {
        "cross-bus"
    }
    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        assert!(
            !ctx.changed_files.is_empty(),
            "bus context must carry files"
        );
        Vec::new()
    }
}

#[tokio::test]
async fn write_file_event_is_clean_via_bus() {
    let path = PathBuf::from("/tmp/kf_code_cross_test.rs");

    let mut bus = VerifierBus::new();
    bus.register(Box::new(CleanBusVerifier));
    let ctx = VerifyContext {
        sandbox_dir: PathBuf::from("/tmp"),
        changed_files: vec![path.clone()],
        event_kind: None,
        tool_name: None,
        content_hash: 0,
        bash_command: None,
        bash_exit_code: None,
        bash_workdir: None,
    };
    bus.run(&ctx);
    assert!(
        !bus.has_errors(),
        "bus path should have no errors for a clean file"
    );
    assert!(
        bus.verdicts().is_empty(),
        "bus path should produce no verdict entries for a clean file"
    );
}

// WO 44.29 heritage (reworked for WO 47.14): the built-in verifiers now
// register on the VerifierBus as BusVerifier impls. This test verifies
// that all 14 built-ins are registered after init_default_verifiers and
// survive a reload_plugins call.
#[tokio::test]
async fn reload_plugins_keeps_every_built_in_verifier() {
    use super::common::{make_config, make_executor, make_info, MockAdapter};
    use crate::shared::{FinishReason, StreamEvent};

    let adapter = Box::new(MockAdapter::new(
        vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: None,
        }],
        make_info(),
    ));
    let mut executor = make_executor(adapter, vec![], make_config(false)).expect("build executor");

    // init_default_verifiers ran in the constructor; collect the verifier
    // count from the bus so we can assert it's unchanged after reload.
    let before = executor
        .verifier_bus
        .as_ref()
        .map(|bus_lock| {
            let bus = bus_lock.lock().unwrap_or_else(|e| e.into_inner());
            bus.verifier_count()
        })
        .unwrap_or(0);

    let empty_registry = kf_plugin_host::PluginRegistry::new();
    executor.reload_plugins(&empty_registry);

    let after = executor
        .verifier_bus
        .as_ref()
        .map(|bus_lock| {
            let bus = bus_lock.lock().unwrap_or_else(|e| e.into_inner());
            bus.verifier_count()
        })
        .unwrap_or(0);

    // With no plugins, the count should be the same (14 built-ins).
    assert_eq!(
        after, before,
        "reload_plugins changed verifier count: before={before} after={after}"
    );
    assert!(
        after >= 14,
        "expected at least 14 built-in verifiers, got {after}"
    );
}
