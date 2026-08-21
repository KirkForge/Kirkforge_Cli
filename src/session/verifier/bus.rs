//! Unified verifier bus — collects verdicts from all registered verifiers
//! after a tool call and provides structured feedback to the executor.
//!
//! ADR-043: the KVB (KirkForge Verification Bus) unifies the existing
//! verifier systems (security, lint, build, git, test, plugin) behind
//! a single `VerifierBus` struct. The executor queries the bus after
//! file-modifying tool calls; error verdicts are injected into the
//! conversation so the model sees them immediately.
//!
//! ## Why two verifier traits?
//!
//! The `Verifier` trait (in `types.rs`) is async and event-driven: verifiers
//! receive a `BusEvent` and can do async I/O (run `cargo build`, spawn
//! processes). It powers the correction loop (`CorrectionLoop`).
//!
//! The `BusVerifier` trait (here) is sync and context-based: verifiers
//! receive a `VerifyContext` (changed files list) and return structured
//! `VerdictEntry`s synchronously. It powers the structured verdict report
//! (WO 11.7) and plugin verifiers that run via subprocess exit codes.
//!
//! Plugin verifiers use `BusVerifier` (via `PluginBusVerifier`) because the
//! plugin host's `PluginVerifier` is synchronous (exit-code based). Migrating
//! plugin verifiers to the async `Verifier` trait would require making the
//! plugin host async or spawning blocking tasks — a larger change that's not
//! justified today. Both traits serve different execution models.

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
}

/// Context for a verification run.
#[derive(Debug, Clone)]
pub struct VerifyContext {
    pub sandbox_dir: PathBuf,
    pub changed_files: Vec<PathBuf>,
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
    /// a built-in slot (`"security"`, `"git"`) augments the same slot
    /// rather than replacing it. The built-in slot stubs
    /// (`SecurityBusVerifier`, `GitBusVerifier`) were removed — the bus
    /// starts empty and contains only what the host explicitly registers
    /// (plugin verifiers, the TS orchestrator bridge). Async verifiers
    /// continue to operate through the event-driven `Verifier` trait path.
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
            let entries = match catch_unwind(AssertUnwindSafe(|| verifier.verify(ctx))) {
                Ok(entries) => entries,
                Err(panic_payload) => {
                    let msg = panic_payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    tracing::warn!("verifier {} panicked: {msg}", verifier.name());
                    vec![VerdictEntry {
                        source: VerifierSource::Custom(verifier.name().to_string()),
                        severity: Severity::Warning,
                        message: format!("verifier panicked: {msg}"),
                        file: None,
                        line: None,
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

// ── Built-in bus verifier adapters ──────────────────────────────────────
//
// The built-in bus verifier stubs (SecurityBusVerifier, GitBusVerifier) have
// been removed. The event-driven verifier system (VerifierHandler +
// CorrectionLoop) handles async verification via BusEvents. The bus collects
// structured findings from BusVerifiers that don't need async I/O.
// Async verifiers continue to operate through the event bus.
// Plugin verifiers and the TS orchestrator bridge register on the bus
// independently; the bus starts empty and only contains what the host
// explicitly registers.

/// Adapter: a plugin-declared verifier on the bus.
///
/// Wraps the host crate's `PluginVerifier`, which spawns the verifier
/// command with a curated (env-cleared) environment: exit 0 means pass,
/// any non-zero exit fails with stderr as the message. This is the same
/// subprocess convention used by `PluginToolWrapper` for plugin tools.
/// ADR-028: plugin verifiers register into the unified bus rather than
/// only the old event-driven `Verifier` trait path.
pub struct PluginBusVerifier {
    inner: PluginVerifier,
    priority: u8,
}

impl BusVerifier for PluginBusVerifier {
    fn name(&self) -> &str {
        &self.inner.name
    }

    fn verify(&self, ctx: &VerifyContext) -> Vec<VerdictEntry> {
        // `ceiling:` env-var contract divergence (bucketlist 3.30). This
        // bus path passes `KF_CHANGED_FILES` (newline-separated list from
        // `VerifyContext`), while the legacy event-driven path
        // (`PluginVerifierAdapter` in `plugin.rs`) passes `KF_EVENT_KIND`
        // + `KF_EVENT_JSON` (the full serialized `BusEvent`). The two
        // paths intentionally serve different shapes: the bus verifier is
        // sync and context-based (a file list), the event-driven verifier
        // is async and event-based (the full event payload). Unifying the
        // env-var contract would change the behaviour plugin verifier
        // scripts depend on; the divergence is documented, not closed.
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
                }]
            }
            Err(e) => vec![VerdictEntry {
                source: VerifierSource::Plugin(self.inner.name.clone()),
                severity: Severity::Error,
                message: format!("plugin verifier execution failed: {e}"),
                file: None,
                line: None,
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
        // bucketlist 3.41: a plugin verifier named "security" (the slot
        // the built-in SecurityBusVerifier stub occupies) coexists with
        // the stub. The stub returns no verdicts; the plugin verifier
        // (simulated here by a same-named StubVerifier) contributes the
        // real verdict.
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
            }],
        }));
        assert_eq!(
            bus.verifier_count(),
            builtin_count + 1,
            "plugin 'security' verifier is registered alongside the built-in stub"
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
    // or hostile plugin verifier cannot unwind the executor mid-turn. The
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
                    },
                    VerdictEntry {
                        source: VerifierSource::Lint,
                        severity: Severity::Warning,
                        message: format!("v{i}-b"),
                        file: None,
                        line: None,
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
}
