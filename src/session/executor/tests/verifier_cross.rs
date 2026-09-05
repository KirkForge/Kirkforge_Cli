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
// that all 15 built-ins (14 language verifiers + ts-bridge security
// emitter, WO 50.02) are registered after init_default_verifiers and
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

    // With no plugins, the count should be the same (15 built-ins:
    // 14 language verifiers + ts-bridge security emitter, WO 50.02).
    assert_eq!(
        after, before,
        "reload_plugins changed verifier count: before={before} after={after}"
    );
    assert!(
        after >= 15,
        "expected at least 15 built-in verifiers, got {after}"
    );
}

// WO 50.02 P0: the TsOrchestratorBridgeVerifier (14 regex security rules
// for obfuscated eval/exec/pickle patterns) was defined and tested but
// never registered in init_default_verifiers. Pin its registration so a
// future refactor that drops the registration fails this test, not
// production. The bridge registers under the name "ts-bridge".
#[tokio::test]
async fn init_default_verifiers_registers_ts_bridge_security_emitter() {
    use super::common::{make_config, make_executor, make_info, MockAdapter};
    use crate::shared::{FinishReason, StreamEvent};
    use crate::session::verifier::bus::BusVerifier;

    let adapter = Box::new(MockAdapter::new(
        vec![StreamEvent::Done {
            finish_reason: FinishReason::Stop,
            usage: None,
        }],
        make_info(),
    ));
    let executor = make_executor(adapter, vec![], make_config(false)).expect("build executor");

    let bus_lock = executor
        .verifier_bus
        .as_ref()
        .expect("verifier_bus must be set up by init_default_verifiers");
    let bus = bus_lock.lock().unwrap_or_else(|e| e.into_inner());

    // The bus does not expose its verifier list directly, but
    // `retain_verifiers` drops verifiers by name and `verifier_count`
    // reflects the change. If `ts-bridge` is registered, retaining only
    // `ts-bridge` leaves exactly 1 verifier; if it is NOT registered,
    // retaining only `ts-bridge` leaves 0.
    let mut probe_bus = std::mem::take(bus);
    let before = probe_bus.verifier_count();
    probe_bus.retain_verifiers(|n| n == "ts-bridge");
    let ts_bridge_count = probe_bus.verifier_count();
    assert!(
        ts_bridge_count == 1,
        "ts-bridge must be registered by init_default_verifiers; \
         before={before} ts_bridge_count={ts_bridge_count}"
    );
}
