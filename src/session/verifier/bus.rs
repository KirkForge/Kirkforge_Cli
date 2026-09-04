//! Unified verifier bus — collects verdicts from all registered verifiers
//! after a tool call and provides structured feedback to the executor.
//!
//! ADR-043: the KVB (KirkForge Verification Bus) unifies the existing
//! verifier systems (security, lint, build, git, test, plugin) behind
//! a single `VerifierBus` struct. The executor queries the bus after
//! file-modifying tool calls; error verdicts are injected into the
//! conversation so the model sees them immediately.
//!
//! ## WO 47.14: unification complete
//!
//! The old `Verifier` trait (async, event-driven) is deleted. All 14
//! built-in verifiers now implement `BusVerifier` and register on the
//! `VerifierBus`. The `CorrectionLoop` reads verdicts from the bus. The
//! `VerifierHandler`/`VerifierSlots` are deleted.

use kf_plugin_host::PluginVerifier;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

/// Which verifier produced this finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierSource {
    Plugin(String),
    Git,
    Build,
    Test,
    Lint,
    Security,
    Rustfmt,
    Custom(String),
}

impl std::fmt::Display for VerifierSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifierSource::Plugin(name) => write!(f, "plugin:{name}"),
            VerifierSource::Git => write!(f, "git"),
            VerifierSource::Build => write!(f, "build"),
            VerifierSource::Test => write!(f, "test"),
            VerifierSource::Lint => write!(f, "lint"),
            VerifierSource::Security => write!(f, "security"),
            VerifierSource::Rustfmt => write!(f, "rustfmt"),
            VerifierSource::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Finding severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Info => write!(f, "info"),
            Severity::Warning => write!(f, "warning"),
            Severity::Error => write!(f, "error"),
        }
    }
}

/// A single finding from one verifier.
#[derive(Debug, Clone)]
pub struct VerdictEntry {
    pub source: VerifierSource,
    pub severity: Severity,
    pub message: String,
    pub file: Option<PathBuf>,
    pub line: Option<u32>,
    /// Optional fix for the correction loop to apply. WO 47.14: carried
    /// over from the old `Verdict::Fixable(FixSuggestion)` shape so the
    /// correction loop can auto-fix without the event-driven `Verifier`
    /// trait.
    pub fix: Option<crate::session::verifier::types::FixSuggestion>,
}

/// Context for a verification run.
#[derive(Debug, Clone)]
pub struct VerifyContext {
    pub sandbox_dir: PathBuf,
    pub changed_files: Vec<PathBuf>,
    /// The triggering event type (e.g. "post-tool-write_file"). WO 47.14:
    /// carried over from `BusEvent::kind()` so event-aware verifiers can
    /// gate without the event-driven `Verifier` trait.
    pub event_kind: Option<String>,
    /// The tool that triggered the verification (e.g. "write_file",
    /// "edit_file", "bash"). WO 47.14: replaces the `BusEvent` variant
    /// discriminant the old verifiers switched on.
    pub tool_name: Option<String>,
    /// Content hash for verdict cache compatibility. WO 47.14: carried
    /// over from `FileWriteEvent::content_hash` so the verdict cache
    /// (keyed by `(file_path, content_hash)`) still works.
    pub content_hash: u64,
    /// Bash command string (when `tool_name` is "bash"). WO 47.14:
    /// carried over from `BashExecEvent::command` so the git verifier
    /// can detect git-modifying commands.
    pub bash_command: Option<String>,
    /// Bash exit code (when `tool_name` is "bash"). WO 47.14: carried
    /// over from `BashExecEvent::exit_code`.
    pub bash_exit_code: Option<i32>,
    /// Bash workdir (when `tool_name` is "bash"). WO 47.14: carried
    /// over from `BashExecEvent::workdir`.
    pub bash_workdir: Option<PathBuf>,
}

/// The unified verifier bus. Verifiers register here, and the
/// executor queries verdicts after each tool call.
pub struct VerifierBus {
    verdicts: Vec<VerdictEntry>,
    verifiers: Vec<Box<dyn BusVerifier>>,
}

/// Trait for bus-aware verifiers. Unlike the event-driven `Verifier`
/// trait (which operates on `BusEvent`), `BusVerifier` receives a
/// `VerifyContext` with the changed files and returns structured
/// `VerdictEntry`s.
pub trait BusVerifier: Send + Sync {
    fn name(&self) -> &str;
    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry>;
}

impl VerifierBus {
    pub fn new() -> Self {
        Self {
            verdicts: Vec::new(),
            verifiers: Vec::new(),
        }
    }

    /// Register a verifier on the bus.
    ///
    /// `ceiling:` duplicate names are allowed and COEXIST — every
    /// registered verifier runs on each `run()` and contributes verdicts.
    /// This is intentional: a plugin-declared verifier whose name matches
    /// a built-in (`"security"`, `"git"`) augments the same slot rather
    /// than replacing it. WO 47.14: the 14 built-in verifiers now register
    /// here as `BusVerifier` impls (the old `Verifier` trait is deleted).
    /// (bucketlist 3.41)
    pub fn register(&mut self, verifier: Box<dyn BusVerifier>) {
        self.verifiers.push(verifier);
    }

    /// Register a plugin-declared verifier. `plugin_root` is the plugin
    /// directory and `command` is the verifier command path (resolved
    /// relative to `plugin_root`, as declared in the manifest). The
    /// verifier runs via the same env-cleared subprocess path as the host
    /// `PluginVerifier` (exit 0 = pass, non-zero = fail with stderr as the
    /// message), with `plugin_root` as the subprocess cwd. Results are
    /// tagged `VerifierSource::Plugin(name)`.
    ///
    /// See [`register`](Self::register): a plugin verifier whose declared
    /// name matches a built-in slot (`"security"`) coexists with any
    /// same-named verifier rather than replacing it.
    pub fn add_plugin_verifier(
        &mut self,
        name: String,
        priority: u8,
        plugin_root: PathBuf,
        command: PathBuf,
    ) {
        let verifier = PluginBusVerifier {
            inner: PluginVerifier {
                name,
                command,
                plugin_root,
            },
            priority,
        };
        self.verifiers.push(Box::new(verifier));
    }

    /// Run all registered verifiers against the given context.
    /// Collects all verdicts (does not short-circuit on first error).
    pub fn run(&mut self, ctx: &VerifyContext) {
        self.verdicts.clear();
        for verifier in &self.verifiers {
            // WO 47.19: capture name() BEFORE the catch_unwind — the
            // panic handler below must not call arbitrary verifier code.
            // A panic in name() there would escape the catch while a
            // panic is already being handled, unwind through the
            // executor's held `Mutex<VerifierBus>` guard (poison), and
            // permanently kill the bus verification gate.
            // (Containment contract, WO 47.23: this guard — and the
            // poison scenario it defends against — only exist in unwind
            // builds (dev/test). Release builds use panic=abort: the
            // process aborts and the WO 38.2 panic hook restores the
            // terminal.)
            let name = verifier.name().to_string();
            let entries = match catch_unwind(AssertUnwindSafe(|| verifier.verify(ctx))) {
                Ok(entries) => entries,
                Err(panic_payload) => {
                    let msg = panic_payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    tracing::warn!("verifier {name} panicked: {msg}");
                    vec![VerdictEntry {
                        source: VerifierSource::Custom(name),
                        severity: Severity::Warning,
                        message: format!("verifier panicked: {msg}"),
                        file: None,
                        line: None,
                        fix: None,
                    }]
                }
            };
            self.verdicts.extend(entries);
        }
    }

    /// All verdicts from the last run.
    pub fn verdicts(&self) -> &[VerdictEntry] {
        &self.verdicts
    }

    /// Whether any verdict has severity Error.
    pub fn has_errors(&self) -> bool {
        self.verdicts.iter().any(|v| v.severity == Severity::Error)
    }

    /// Clear all collected verdicts.
    pub fn clear(&mut self) {
        self.verdicts.clear();
    }

    /// Drop registered verifiers whose `name()` is NOT in `keep`. Used by
    /// live plugin reload to prune old plugin verifiers while keeping the
    /// built-in bus verifiers. ADR-028.
    pub fn retain_verifiers<F>(&mut self, keep: F)
    where
        F: Fn(&str) -> bool,
    {
        self.verifiers.retain(|v| keep(v.name()));
    }

    /// Number of registered verifiers (built-in + plugin).
    pub fn verifier_count(&self) -> usize {
        self.verifiers.len()
    }
}

impl Default for VerifierBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── Plugin bus verifier adapter ──────────────────────────────────────────
//
// WO 47.14: the 14 built-in verifiers now register on the bus as
// `BusVerifier` impls (in their respective files: build.rs, lint.rs, etc.).
// Plugin verifiers register via the `PluginBusVerifier` adapter below.
// The TS orchestrator bridge (`TsOrchestratorBridgeVerifier`) also
// registers on the bus for the Rust security emitter.

/// Adapter: a plugin-declared verifier on the bus.
///
/// Wraps the host crate's `PluginVerifier`, which spawns the verifier
/// command with a curated (env-cleared) environment: exit 0 means pass,
/// any non-zero exit fails with stderr as the message. This is the same
/// subprocess convention used by `PluginToolWrapper` for plugin tools.
/// ADR-028: plugin verifiers register into the unified bus — since WO
/// 47.14 this is their sole integration path (the event-driven
/// `Verifier`-trait adapter was the second, deleted).
pub struct PluginBusVerifier {
    inner: PluginVerifier,
    priority: u8,
}

impl BusVerifier for PluginBusVerifier {
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // Env-var contract (bucketlist 3.30; single path since WO 47.14
        // deleted the event-driven `PluginVerifierAdapter`): this bus path
        // passes `KF_VERIFIER_NAME` + `KF_CHANGED_FILES` (newline-separated
        // list from `VerifyContext`). The retired adapter additionally
        // passed `KF_EVENT_KIND` + `KF_EVENT_JSON` (the full serialized
        // `BusEvent`) and also fired on read/bash events — plugin scripts
        // depending on those vars must read `KF_CHANGED_FILES` instead;
        // restoring event visibility requires extending `VerifyContext`
        // (tracked in WO 47.14 remaining work).
        let mut env = HashMap::new();
        env.insert("KF_VERIFIER_NAME".to_string(), self.inner.name.clone());
        env.insert(
            "KF_CHANGED_FILES".to_string(),
            ctx.changed_files
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let _ = self.priority;
        match self.inner.run(&env) {
            Ok(kf_plugin_host::VerifierVerdict::Pass) => Vec::new(),
            Ok(kf_plugin_host::VerifierVerdict::Fail { message }) => {
                vec![VerdictEntry {
                    source: VerifierSource::Plugin(self.inner.name.clone()),
                    severity: Severity::Error,
                    message,
                    file: ctx.changed_files.first().cloned(),
                    line: None,
                    fix: None,
                }]
            }
            Err(e) => vec![VerdictEntry {
                source: VerifierSource::Plugin(self.inner.name.clone()),
                severity: Severity::Error,
                message: format!("plugin verifier execution failed: {e}"),
                file: None,
                line: None,
                fix: None,
            }],
        }
    }
}

/// Build a VerifierBus with no built-in stubs registered.
/// Plugin verifiers and the TS orchestrator bridge register independently.
#[deprecated(note = "use VerifierBus::new() directly")]
pub fn default_verifier_bus() -> VerifierBus {
    VerifierBus::new()
}

// ── WO 41.4: verifier capability discovery ─────────────────────────────
//
// A forward-looking capability map: which verifier categories are active
// (have a producer), stub (emitter not ported), or external (delegates to
// cargo/eslint/etc.). The static set mirrors the honest stub disclosure
// already printed by `plugin_verify` (`src/session/plugin_tools/native.rs`
// `render_verify`): security is active (Rust emitter, WO 29.2), lint/types/
// graph are stubs (emitters not ported), verify-workspace is deferred
// (reducer not ported). Surfaced via `/verify-capabilities` so the user can
// see at a glance what contributes to a verdict without reading source.

/// A verifier category's implementation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Has a real producer (e.g. the Rust security emitter).
    Active,
    /// Emitter not ported yet — the category reports nothing.
    Stub,
    /// Delegates to an external toolchain (cargo/eslint/etc.) or is
    /// otherwise deferred to a not-yet-ported subsystem.
    External,
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::Active => write!(f, "active"),
            Capability::Stub => write!(f, "stub"),
            Capability::External => write!(f, "external"),
        }
    }
}

/// One row in the capability map.
#[derive(Debug, Clone)]
pub struct VerifierCapability {
    pub category: &'static str,
    pub status: Capability,
    pub note: &'static str,
}

/// The static capability map. Mirrors the stub set in `native.rs`
/// `render_verify` and `deferred_message`. Update both when a category
/// gains a producer.
pub fn verifier_capabilities() -> &'static [VerifierCapability] {
    &[
        VerifierCapability {
            category: "security",
            status: Capability::Active,
            note: "Rust regex emitter (14 rules, WO 29.2)",
        },
        VerifierCapability {
            category: "lint",
            status: Capability::Stub,
            note: "emitter not ported",
        },
        VerifierCapability {
            category: "types",
            status: Capability::Stub,
            note: "emitter not ported",
        },
        VerifierCapability {
            category: "graph",
            status: Capability::Stub,
            note: "emitter not ported",
        },
        VerifierCapability {
            category: "verify-workspace",
            status: Capability::External,
            note: "reducer not ported (ReducedStatePacket assembly pending)",
        },
    ]
}

/// Human-readable capability report for `/verify-capabilities`.
pub fn verifier_capability_report() -> String {
    let mut out = String::from("verifier capabilities:\n");
    for cap in verifier_capabilities() {
        out.push_str(&format!(
            "  {}: {} — {}\n",
            cap.category, cap.status, cap.note
        ));
    }
    out.push_str("overall: PASS (security-only coverage) until lint/types/graph emitters ship");
    out
}

// ── WO 29.2: Rust-native security emitter ──────────────────────────────
//
// The TS orchestrator NDJSON bridge (WO 10.8) is retired. The 14 regex
// security rules now live in Rust (`security_emitter.rs`) and produce
// `VerdictEntry`s directly — no Node subprocess, no NDJSON round-trip.
// This was the last Rust→TS call path (WO 29.2). The ADR-028 NDJSON wire
// format is retired alongside the TS bridge; the Rust emitter returns
// typed structs. If a future plugin needs an NDJSON emitter, the format
// is documented in ADR-028 and can be re-added then (YAGNI for now).

/// A `BusVerifier` that runs the Rust security emitter over the changed
/// files. Originally (WO 10.8) this shelled out to the TS
/// `bridge-emitter.ts` and parsed NDJSON; WO 29.2 replaced the subprocess
/// with a compiled-in function. The name is retained for bus-wiring
/// stability (verifiers register by name).
///
/// `ceiling:` the struct name still says "TsOrchestratorBridge" for
/// historical continuity — the TS dependency is gone. Rename in a later
/// sweep if desired (not done here to keep the diff small).
pub struct TsOrchestratorBridgeVerifier {
    name: String,
}

impl TsOrchestratorBridgeVerifier {
    pub fn new(name: String) -> Self {
        Self { name }
    }
}

impl BusVerifier for TsOrchestratorBridgeVerifier {
    fn name(&self) -> &str {
        &self.name
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // Resolve relative changed-file paths against the sandbox dir
        // (mirrors the old TS bridge, which resolved against its cwd).
        let resolved: Vec<PathBuf> = ctx
            .changed_files
            .iter()
            .map(|f| {
                if f.is_absolute() {
                    f.clone()
                } else {
                    ctx.sandbox_dir.join(f)
                }
            })
            .collect();
        super::security_emitter::emit_security_findings(&resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubVerifier {
        name: String,
        entries: Vec<VerdictEntry>,
    }

    impl BusVerifier for StubVerifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
            self.entries.clone()
        }
    }

    fn make_verify_ctx() -> VerifyContext {
        VerifyContext {
            sandbox_dir: PathBuf::from("/tmp/test"),
            changed_files: vec![PathBuf::from("src/lib.rs")],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        }
    }

    #[test]
    fn verifier_bus_register_and_run() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub_a".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "clean build".into(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: None,
                fix: None,
            }],
        }));
        bus.register(Box::new(StubVerifier {
            name: "stub_b".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Git,
                severity: Severity::Warning,
                message: "dirty worktree".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));

        bus.run(&make_verify_ctx());
        assert_eq!(
            bus.verdicts().len(),
            2,
            "should collect verdicts from both stubs"
        );
    }

    #[test]
    fn verifier_bus_has_errors() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "error_stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Security,
                severity: Severity::Error,
                message: "secret detected".into(),
                file: Some(PathBuf::from("src/config.rs")),
                line: Some(42),
                fix: None,
            }],
        }));

        bus.run(&make_verify_ctx());
        assert!(bus.has_errors(), "should detect error verdicts");
    }

    #[test]
    fn verifier_bus_no_errors_when_clean() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "clean_stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Info,
                message: "no issues".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));

        bus.run(&make_verify_ctx());
        assert!(
            !bus.has_errors(),
            "no error verdicts → has_errors() is false"
        );
    }

    #[test]
    fn verifier_bus_clear() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Test,
                severity: Severity::Error,
                message: "test failed".into(),
                file: Some(PathBuf::from("src/lib.rs")),
                line: None,
                fix: None,
            }],
        }));

        bus.run(&make_verify_ctx());
        assert!(!bus.verdicts().is_empty());
        bus.clear();
        assert!(bus.verdicts().is_empty(), "clear() should empty verdicts");
    }

    #[test]
    fn verify_context_changed_files() {
        let ctx = VerifyContext {
            sandbox_dir: PathBuf::from("/tmp/project"),
            changed_files: vec![PathBuf::from("src/main.rs"), PathBuf::from("src/lib.rs")],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        assert_eq!(ctx.changed_files.len(), 2);
        assert_eq!(ctx.sandbox_dir, PathBuf::from("/tmp/project"));
    }

    #[test]
    fn verdict_source_display() {
        assert_eq!(VerifierSource::Git.to_string(), "git");
        assert_eq!(VerifierSource::Build.to_string(), "build");
        assert_eq!(
            VerifierSource::Plugin("my_plugin".into()).to_string(),
            "plugin:my_plugin"
        );
        assert_eq!(
            VerifierSource::Custom("lsp".into()).to_string(),
            "custom:lsp"
        );
    }

    #[test]
    fn severity_display() {
        assert_eq!(Severity::Info.to_string(), "info");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Error.to_string(), "error");
    }

    // ── WO 41.4: capability discovery ────────────────────────────────

    #[test]
    fn capability_display_labels() {
        assert_eq!(Capability::Active.to_string(), "active");
        assert_eq!(Capability::Stub.to_string(), "stub");
        assert_eq!(Capability::External.to_string(), "external");
    }

    #[test]
    fn verifier_capabilities_lists_all_known_categories() {
        let caps = verifier_capabilities();
        let names: Vec<&str> = caps.iter().map(|c| c.category).collect();
        assert!(names.contains(&"security"), "security must be listed");
        assert!(names.contains(&"lint"), "lint must be listed");
        assert!(names.contains(&"types"), "types must be listed");
        assert!(names.contains(&"graph"), "graph must be listed");
        assert!(
            names.contains(&"verify-workspace"),
            "verify-workspace must be listed"
        );
    }

    #[test]
    fn verifier_capabilities_security_is_active_others_are_stubs() {
        let caps = verifier_capabilities();
        let security = caps
            .iter()
            .find(|c| c.category == "security")
            .expect("security category present");
        assert_eq!(security.status, Capability::Active);
        for cat in ["lint", "types", "graph"] {
            let c = caps
                .iter()
                .find(|c| c.category == cat)
                .expect("category present");
            assert_eq!(
                c.status,
                Capability::Stub,
                "{cat} must be a stub (emitter not ported)"
            );
        }
        let vw = caps
            .iter()
            .find(|c| c.category == "verify-workspace")
            .expect("verify-workspace present");
        assert_eq!(vw.status, Capability::External);
    }

    #[test]
    fn verifier_capability_report_names_every_category_and_coverage() {
        let report = verifier_capability_report();
        for cap in verifier_capabilities() {
            assert!(
                report.contains(cap.category),
                "report missing category {:?}: {report}",
                cap.category
            );
            assert!(
                report.contains(cap.status.to_string().as_str()),
                "report missing status for {:?}: {report}",
                cap.category
            );
        }
        assert!(
            report.contains("PASS (security-only coverage)"),
            "report must state the security-only coverage scope: {report}"
        );
    }

    #[cfg(unix)]
    fn make_pass_script(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("pass.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[cfg(unix)]
    fn make_fail_script(dir: &std::path::Path, body: &str) -> PathBuf {
        let script = dir.join("fail.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    // WO 43.26: pin that a hung plugin-bus verifier surfaces a timeout
    // error verdict — not an indefinite hang. The underlying watchdog is
    // WO 38.3 (`kf-plugin-host/verifier.rs` 5s killpg + `TimedOut`); this
    // test locks the behavior at the bus-wrapper level so a future
    // change to the host crate cannot silently re-introduce an
    // unbounded bus-path wait. Bounded by an outer 30s wall: if the
    // internal 5s watchdog regresses, the test fails fast instead of
    // hanging the suite.
    #[cfg(unix)]
    #[test]
    fn plugin_bus_verifier_hung_returns_timeout_error_not_hang() {
        let tmp = tempfile::tempdir().unwrap();
        let script = make_fail_script(tmp.path(), "sleep 60\nexit 1\n");
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "hang_v".into(),
            5,
            tmp.path().to_path_buf(),
            std::path::Path::new(script.file_name().unwrap()).to_path_buf(),
        );
        let run = std::thread::spawn(move || {
            bus.run(&make_verify_ctx());
            bus
        });
        let bus = match run.join() {
            Ok(b) => b,
            Err(p) => std::panic::resume_unwind(p),
        };
        assert_eq!(
            bus.verdicts().len(),
            1,
            "hung verifier must surface exactly one verdict: {:?}",
            bus.verdicts()
        );
        let v = &bus.verdicts()[0];
        assert_eq!(v.severity, Severity::Error);
        assert!(
            v.message.contains("timed out"),
            "verdict must name the timeout, got: {}",
            v.message
        );
        assert!(bus.has_errors());
    }

    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_pass_yields_no_verdicts() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_pass_script(tmp.path());
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "pass_v".into(),
            5,
            tmp.path().to_path_buf(),
            PathBuf::from("pass.sh"),
        );
        bus.run(&make_verify_ctx());
        assert!(
            bus.verdicts().is_empty(),
            "passing verifier adds no verdicts"
        );
        assert!(!bus.has_errors());
    }

    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_fail_yields_error_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let _ = make_fail_script(tmp.path(), "echo 'bad pattern' >&2\nexit 1\n");
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "fail_v".into(),
            5,
            tmp.path().to_path_buf(),
            PathBuf::from("fail.sh"),
        );
        bus.run(&make_verify_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        let v = &bus.verdicts()[0];
        assert_eq!(v.source, VerifierSource::Plugin("fail_v".into()));
        assert_eq!(v.severity, Severity::Error);
        assert!(v.message.contains("bad pattern"));
        assert!(bus.has_errors());
    }

    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_missing_command_yields_error_verdict() {
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "ghost".into(),
            1,
            PathBuf::from("/nonexistent"),
            PathBuf::from("does-not-exist.sh"),
        );
        bus.run(&make_verify_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        assert_eq!(bus.verdicts()[0].severity, Severity::Error);
        assert!(bus.has_errors());
    }

    // Ported from the deleted event-driven `PluginVerifierAdapter` (WO
    // 47.14): a plugin verifier must not inherit sensitive session
    // variables such as API keys. The host crate env_clear()s before
    // overlaying the curated allowlist + the KF_* variables — this pins
    // that contract on the now-sole bus path.
    #[cfg(unix)]
    #[test]
    fn add_plugin_verifier_does_not_leak_session_env() {
        use crate::shared::test_util::EnvGuard;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("envleak.sh");
        std::fs::write(&script, "#!/bin/sh\necho \"$OPENAI_API_KEY\" >&2\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let _env = EnvGuard::set("OPENAI_API_KEY", "sk-leaked-secret");
        let mut bus = VerifierBus::new();
        bus.add_plugin_verifier(
            "envleak".into(),
            1,
            tmp.path().to_path_buf(),
            PathBuf::from("envleak.sh"),
        );
        bus.run(&make_verify_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        let v = &bus.verdicts()[0];
        assert_eq!(v.severity, Severity::Error);
        assert!(
            !v.message.contains("sk-leaked-secret"),
            "session env leaked into plugin verifier stderr: {}",
            v.message
        );
    }

    // ── WO 29.2: TsOrchestratorBridgeVerifier delegates to the Rust emitter ──

    /// WO 29.2: the verifier no longer spawns a Node subprocess. It scans
    /// the changed files with the Rust security emitter and returns the
    /// findings as typed `VerdictEntry`s.
    #[test]
    fn ts_orchestrator_bridge_verifier_delegates_to_rust_emitter() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("evil.py");
        std::fs::write(&target, "eval('1+1')\n").unwrap();
        let ctx = VerifyContext {
            sandbox_dir: dir.path().to_path_buf(),
            changed_files: vec![target.clone()],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        let v = TsOrchestratorBridgeVerifier::new("ts-bridge".into());
        let entries = v.verify(&ctx);
        assert!(
            entries.iter().any(|e| e.message.contains("[py-eval]")),
            "delegation should surface the py-eval finding: {entries:?}"
        );
        assert!(entries
            .iter()
            .all(|e| matches!(e.source, VerifierSource::Custom(ref s) if s == "ts:security")));
        assert_eq!(v.name(), "ts-bridge");
    }

    /// WO 29.2: relative changed-file paths are resolved against the
    /// sandbox dir before scanning (mirrors the old TS bridge cwd).
    #[test]
    fn ts_orchestrator_bridge_verifier_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rel.py"), "eval('x')\n").unwrap();
        let ctx = VerifyContext {
            sandbox_dir: dir.path().to_path_buf(),
            changed_files: vec![PathBuf::from("rel.py")],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        let v = TsOrchestratorBridgeVerifier::new("ts-bridge".into());
        let entries = v.verify(&ctx);
        assert!(
            entries.iter().any(|e| e.message.contains("[py-eval]")),
            "relative path should resolve against sandbox_dir: {entries:?}"
        );
    }

    /// WO 29.2: clean files produce no findings through the wrapper.
    #[test]
    fn ts_orchestrator_bridge_verifier_clean_file_yields_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("clean.ts");
        std::fs::write(&target, "export const x = 1;\n").unwrap();
        let ctx = VerifyContext {
            sandbox_dir: dir.path().to_path_buf(),
            changed_files: vec![target],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        let v = TsOrchestratorBridgeVerifier::new("ts-bridge".into());
        assert!(v.verify(&ctx).is_empty());
    }

    #[test]
    #[allow(deprecated)]
    fn default_verifier_bus_is_empty() {
        let bus = default_verifier_bus();
        assert_eq!(bus.verifier_count(), 0);
    }

    #[test]
    #[allow(deprecated)]
    fn default_verifier_bus_verifies_without_errors_on_empty_changed_files() {
        let mut bus = default_verifier_bus();
        let ctx = VerifyContext {
            sandbox_dir: PathBuf::from("/tmp"),
            changed_files: vec![],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        bus.run(&ctx);
        assert!(!bus.has_errors(), "built-in stubs emit no verdicts");
        assert!(bus.verdicts().is_empty());
    }

    #[test]
    fn verifier_bus_duplicate_names_coexist() {
        // bucketlist 3.41: duplicate names are intentionally allowed to
        // coexist — the built-in slot stubs share their slot name with
        // plugin verifiers that augment the same slot. Two same-name
        // verifiers both run and both contribute verdicts.
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "dup".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "first".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        bus.register(Box::new(StubVerifier {
            name: "dup".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Warning,
                message: "second".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        assert_eq!(bus.verifier_count(), 2, "both same-name verifiers kept");
        bus.run(&make_verify_ctx());
        assert_eq!(
            bus.verdicts().len(),
            2,
            "both same-name verifiers ran and contributed verdicts"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn verifier_bus_plugin_verifier_coexists_with_builtin_stub() {
        // bucketlist 3.41: a plugin verifier named "security" can coexist
        // with a same-named verifier on the bus. WO 47.14: the built-in
        // SecurityVerifier registers on the bus at init time; this test
        // uses default_verifier_bus() (empty) so it only verifies the
        // coexistence contract with a same-named plugin verifier.
        let mut bus = default_verifier_bus();
        let builtin_count = bus.verifier_count();
        bus.register(Box::new(StubVerifier {
            name: "security".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Plugin("sec-plugin".into()),
                severity: Severity::Error,
                message: "plugin finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        assert_eq!(
            bus.verifier_count(),
            builtin_count + 1,
            "plugin 'security' verifier is registered alongside the built-in"
        );
        bus.run(&make_verify_ctx());
        assert!(
            bus.verdicts().iter().any(|v| v.message == "plugin finding"),
            "the plugin verifier's verdict survived alongside the stub"
        );
    }

    #[test]
    fn verifier_bus_retain_drops_verifiers_not_matching_predicate() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "keep_me".into(),
            entries: vec![],
        }));
        bus.register(Box::new(StubVerifier {
            name: "drop_me".into(),
            entries: vec![],
        }));
        assert_eq!(bus.verifier_count(), 2);
        bus.retain_verifiers(|n| n == "keep_me");
        assert_eq!(bus.verifier_count(), 1);
        bus.run(&make_verify_ctx());
        assert!(bus.verdicts().is_empty());
    }

    #[test]
    fn verifier_bus_run_overwrites_previous_verdicts() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(StubVerifier {
            name: "stub".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Error,
                message: "first run".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        bus.run(&make_verify_ctx());
        assert_eq!(bus.verdicts().len(), 1);
        assert_eq!(bus.verdicts()[0].message, "first run");
        bus.run(&make_verify_ctx());
        assert_eq!(
            bus.verdicts().len(),
            1,
            "run() clears prior verdicts before collecting"
        );
        assert_eq!(bus.verdicts()[0].message, "first run");
    }

    // ── R5.4 — bus resilience to a panicking verifier ─────────────────
    //
    // `VerifierBus::run` wraps each verifier in `catch_unwind` so a buggy
    // or hostile plugin verifier cannot unwind the executor mid-turn —
    // true in unwind builds (this test profile). Release builds use
    // panic=abort: the process aborts and the WO 38.2 panic hook
    // restores the terminal (WO 47.23 contract). The
    // panic must surface as a `Severity::Warning` verdict naming the
    // verifier, and sibling verifiers must still contribute their findings
    // (proving the bus keeps running after the panic).

    struct PanickingVerifier {
        name: String,
    }
    impl BusVerifier for PanickingVerifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
            panic!("intentional verifier panic for R5.4");
        }
    }

    #[test]
    fn verifier_bus_survives_panicking_verifier_and_keeps_siblings() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(PanickingVerifier {
            name: "boom".into(),
        }));
        bus.register(Box::new(StubVerifier {
            name: "sibling".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Error,
                message: "sibling finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));

        // Must not panic.
        bus.run(&make_verify_ctx());

        // The panicking verifier contributes a "verifier panicked" warning.
        assert!(
            bus.verdicts()
                .iter()
                .any(|v| v.severity == Severity::Warning
                    && v.message.contains("verifier panicked")
                    && v.message.contains("intentional verifier panic")),
            "panicking verifier should surface a warning verdict, got: {:?}",
            bus.verdicts()
        );
        // The sibling verifier's finding survives the panic.
        assert!(
            bus.verdicts()
                .iter()
                .any(|v| v.message == "sibling finding"),
            "sibling verdict must survive the panic"
        );
        assert!(bus.has_errors(), "sibling's Error verdict must be counted");
    }

    #[test]
    fn parse_ndjson_contract_is_retired() {
        // WO 29.2: the NDJSON wire format + NdjsonVerdict/parse_severity/
        // parse_ndjson were retired with the TS bridge. The Rust emitter
        // returns typed VerdictEntry structs directly. This test guards
        // against an accidental re-introduction of the old field shape.
        let v = TsOrchestratorBridgeVerifier::new("ts-bridge".into());
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.ts");
        std::fs::write(&target, "vm.runInNewContext(c)\n").unwrap();
        let ctx = VerifyContext {
            sandbox_dir: dir.path().to_path_buf(),
            changed_files: vec![target],
            event_kind: None,
            tool_name: None,
            content_hash: 0,
            bash_command: None,
            bash_exit_code: None,
            bash_workdir: None,
        };
        let entries = v.verify(&ctx);
        assert!(
            entries.iter().all(|e| e.severity == Severity::Error),
            "findings from the Rust emitter are already typed VerdictEntry"
        );
    }

    // ── R5.1 — bus broadcasts every tool result to all verifiers ────────
    //
    // `VerifierBus::run` must call EVERY registered verifier (no
    // short-circuit on the first error). Pin that N verifiers each
    // contributing M verdicts yields N*M total verdicts — proving the
    // broadcast reaches all of them.

    #[test]
    fn verifier_bus_broadcasts_every_tool_result() {
        let mut bus = VerifierBus::new();
        for i in 0..3 {
            bus.register(Box::new(StubVerifier {
                name: format!("v{i}"),
                entries: vec![
                    VerdictEntry {
                        source: VerifierSource::Build,
                        severity: Severity::Info,
                        message: format!("v{i}-a"),
                        file: None,
                        line: None,
                        fix: None,
                    },
                    VerdictEntry {
                        source: VerifierSource::Lint,
                        severity: Severity::Warning,
                        message: format!("v{i}-b"),
                        file: None,
                        line: None,
                        fix: None,
                    },
                ],
            }));
        }
        assert_eq!(bus.verifier_count(), 3);
        bus.run(&make_verify_ctx());
        // 3 verifiers × 2 verdicts each = 6.
        assert_eq!(
            bus.verdicts().len(),
            6,
            "all 3 verifiers must run and each contribute 2 verdicts: {:?}",
            bus.verdicts()
        );
        // Each verifier's name should appear in at least one message
        // (proving all ran, not just the first).
        for i in 0..3 {
            assert!(
                bus.verdicts()
                    .iter()
                    .any(|v| v.message == format!("v{i}-a")),
                "verifier v{i} must have run"
            );
        }
    }

    // ── R5.2 — registration supports dynamic add/remove ────────────────
    //
    // `register` adds and `retain_verifiers` (the reload prune path)
    // removes verifiers by name. Pin both: after a register + retain
    // cycle the bus reports the correct count and only the kept
    // verifiers run.

    #[test]
    fn verifier_registration_supports_dynamic_add_remove() {
        let mut bus = VerifierBus::new();
        assert_eq!(bus.verifier_count(), 0, "fresh bus has no verifiers");

        // Add three.
        bus.register(Box::new(StubVerifier {
            name: "a".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "a".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        bus.register(Box::new(StubVerifier {
            name: "b".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "b".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        bus.register(Box::new(StubVerifier {
            name: "c".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Build,
                severity: Severity::Info,
                message: "c".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        assert_eq!(bus.verifier_count(), 3);

        // Remove "b" by keeping only a and c.
        bus.retain_verifiers(|n| n != "b");
        assert_eq!(
            bus.verifier_count(),
            2,
            "retain must drop the removed verifier"
        );

        // Only a and c run.
        bus.run(&make_verify_ctx());
        let names: Vec<&str> = bus.verdicts().iter().map(|v| v.message.as_str()).collect();
        assert!(names.contains(&"a"), "kept verifier a must run");
        assert!(names.contains(&"c"), "kept verifier c must run");
        assert!(
            !names.contains(&"b"),
            "removed verifier b must NOT run: {names:?}"
        );
    }

    // ── R5.3 — verdicts aggregate across multiple verifiers ─────────────
    //
    // Multiple verifiers with different severities must all contribute
    // to the aggregate. `has_errors` must reflect any Error verdict
    // across all verifiers, not just the first.

    #[test]
    fn verifier_verdicts_aggregate_across_multiple_verifiers() {
        let mut bus = VerifierBus::new();
        // Verifier 1: only Info.
        bus.register(Box::new(StubVerifier {
            name: "info_only".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Info,
                message: "info finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        // Verifier 2: only Warning.
        bus.register(Box::new(StubVerifier {
            name: "warn_only".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Git,
                severity: Severity::Warning,
                message: "warn finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));
        // Verifier 3: an Error.
        bus.register(Box::new(StubVerifier {
            name: "err_only".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Security,
                severity: Severity::Error,
                message: "err finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));

        bus.run(&make_verify_ctx());
        // All three verdicts aggregate.
        assert_eq!(
            bus.verdicts().len(),
            3,
            "all three verifiers' verdicts must aggregate: {:?}",
            bus.verdicts()
        );
        // has_errors reflects the Error from the third verifier.
        assert!(
            bus.has_errors(),
            "has_errors must be true when any verifier reports an Error"
        );

        // Drop the error verifier; has_errors must go false.
        bus.retain_verifiers(|n| n != "err_only");
        bus.run(&make_verify_ctx());
        assert_eq!(bus.verdicts().len(), 2);
        assert!(
            !bus.has_errors(),
            "has_errors must be false after removing the error verifier"
        );
    }

    // ── R5.4 — bus disconnect mid-turn does not panic ───────────────────
    //
    // If a verifier panics mid-run (simulating a disconnect/crash),
    // `run()` must catch the unwind, convert it to a Warning verdict,
    // and continue to the next verifier. After the panic, the bus
    // must still be usable — running again without the panicking
    // verifier (removed via `retain_verifiers`) must produce clean
    // verdicts from the survivors, proving the bus recovered.

    #[test]
    fn bus_disconnect_mid_turn_does_not_panic() {
        let mut bus = VerifierBus::new();
        bus.register(Box::new(PanickingVerifier {
            name: "crash".into(),
        }));
        bus.register(Box::new(StubVerifier {
            name: "survivor".into(),
            entries: vec![VerdictEntry {
                source: VerifierSource::Lint,
                severity: Severity::Error,
                message: "survivor finding".into(),
                file: None,
                line: None,
                fix: None,
            }],
        }));

        // First run: must not propagate the panic.
        bus.run(&make_verify_ctx());
        assert!(
            bus.verdicts()
                .iter()
                .any(|v| v.severity == Severity::Warning
                    && v.message.contains("verifier panicked")),
            "panicking verifier must surface a warning verdict: {:?}",
            bus.verdicts()
        );
        assert!(
            bus.verdicts()
                .iter()
                .any(|v| v.message == "survivor finding"),
            "sibling verifier must still run after the panic"
        );

        // Disconnect (remove) the crashing verifier and run again.
        bus.retain_verifiers(|n| n != "crash");
        bus.run(&make_verify_ctx());
        assert!(
            !bus.verdicts()
                .iter()
                .any(|v| v.message.contains("verifier panicked")),
            "after removing the crashing verifier, no panic warning should appear"
        );
        assert!(
            bus.verdicts()
                .iter()
                .any(|v| v.message == "survivor finding"),
            "survivor must still run after the crash is removed"
        );
    }

    // ── WO 45.47: verifier-bus verdict-set determinism invariant ─────────
    //
    // Verifiers are documented as deterministic (`verifier/mod.rs:13`,
    // `types.rs:405`), but no test pinned the cross-cutting invariant:
    // the whole bus, run N times on the same `VerifyContext`, must produce
    // the same multiset of verdicts. A future Vec→HashMap swap of
    // `VerifierBus::verifiers`, a verifier that reads `SystemTime::now()`,
    // or a racing spawn would silently break this — only these tests catch
    // it at CI time (the `flaky` doctor is a manual dev tool, not a gate).

    // Sort key for verdict multiset comparison. `VerdictEntry` has no `Ord`
    // impl; derive a tuple from `Display` (source/severity both impl it) +
    // message + file + line so `sort_by_key` yields a canonical order and
    // `Vec` equality is multiset equality.
    fn verdict_sort_key(v: &VerdictEntry) -> (String, String, String, String, u32) {
        (
            v.source.to_string(),
            v.severity.to_string(),
            v.message.clone(),
            v.file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            v.line.unwrap_or(0),
        )
    }

    fn sorted_verdicts(bus: &VerifierBus) -> Vec<(String, String, String, String, u32)> {
        let mut keys: Vec<_> = bus.verdicts().iter().map(verdict_sort_key).collect();
        keys.sort();
        keys
    }

    // Four stubs with distinct verdicts (distinct sources/severities/files
    // so a multiset collision can't mask a dropped/added verdict).
    fn four_distinct_stubs() -> [StubVerifier; 4] {
        [
            StubVerifier {
                name: "alpha".into(),
                entries: vec![VerdictEntry {
                    source: VerifierSource::Build,
                    severity: Severity::Info,
                    message: "alpha-finding".into(),
                    file: Some(PathBuf::from("src/a.rs")),
                    line: Some(10),
                    fix: None,
                }],
            },
            StubVerifier {
                name: "beta".into(),
                entries: vec![VerdictEntry {
                    source: VerifierSource::Lint,
                    severity: Severity::Warning,
                    message: "beta-finding".into(),
                    file: Some(PathBuf::from("src/b.rs")),
                    line: None,
                    fix: None,
                }],
            },
            StubVerifier {
                name: "gamma".into(),
                entries: vec![VerdictEntry {
                    source: VerifierSource::Security,
                    severity: Severity::Error,
                    message: "gamma-finding".into(),
                    file: None,
                    line: Some(42),
                    fix: None,
                }],
            },
            StubVerifier {
                name: "delta".into(),
                entries: vec![
                    VerdictEntry {
                        source: VerifierSource::Git,
                        severity: Severity::Info,
                        message: "delta-1".into(),
                        file: None,
                        line: None,
                        fix: None,
                    },
                    VerdictEntry {
                        source: VerifierSource::Test,
                        severity: Severity::Warning,
                        message: "delta-2".into(),
                        file: Some(PathBuf::from("tests/d.rs")),
                        line: Some(7),
                        fix: None,
                    },
                ],
            },
        ]
    }

    #[test]
    fn same_verify_ctx_produces_same_verdict_multiset_across_runs() {
        let ctx = make_verify_ctx();
        let mut bus = VerifierBus::new();
        for stub in four_distinct_stubs() {
            bus.register(Box::new(stub));
        }
        // Run 1 establishes the baseline; runs 2..=10 must match it exactly.
        bus.run(&ctx);
        let baseline = sorted_verdicts(&bus);
        assert!(!baseline.is_empty(), "baseline should have verdicts");
        for run_idx in 2..=10 {
            bus.run(&ctx);
            let got = sorted_verdicts(&bus);
            assert_eq!(
                baseline, got,
                "verdict multiset differs on run {run_idx}: determinism invariant violated"
            );
        }
    }

    #[test]
    fn verifier_insertion_order_does_not_change_verdict_set() {
        let ctx = make_verify_ctx();
        let stubs = four_distinct_stubs();

        let mut bus_a = VerifierBus::new();
        for s in [&stubs[0], &stubs[1], &stubs[2], &stubs[3]] {
            // clone the stub by reconstructing from its entries
            bus_a.register(Box::new(StubVerifier {
                name: s.name.clone(),
                entries: s.entries.clone(),
            }));
        }
        bus_a.run(&ctx);
        let set_a = sorted_verdicts(&bus_a);

        let mut bus_b = VerifierBus::new();
        // reversed order
        for s in [&stubs[3], &stubs[2], &stubs[1], &stubs[0]] {
            bus_b.register(Box::new(StubVerifier {
                name: s.name.clone(),
                entries: s.entries.clone(),
            }));
        }
        bus_b.run(&ctx);
        let set_b = sorted_verdicts(&bus_b);

        assert_eq!(
            set_a, set_b,
            "verdict multiset must not depend on registration order"
        );
    }
}
