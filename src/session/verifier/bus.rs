//! Unified verifier bus — collects verdicts from all registered verifiers
//! after a tool call and provides structured feedback to the executor.
//!
//! ADR-043: the KVB (KirkForge Verification Bus) unifies the existing
//! verifier systems (security, lint, build, git, test, plugin) behind
//! a single `VerifierBus` struct. The executor queries the bus after
//! file-modifying tool calls; error verdicts are injected into the
//! conversation so the model sees them immediately.

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

    pub fn register(&mut self, verifier: Box<dyn BusVerifier>) {
        self.verifiers.push(verifier);
    }

    /// Run all registered verifiers against the given context.
    /// Collects all verdicts (does not short-circuit on first error).
    pub fn run(&mut self, ctx: &VerifyContext) {
        self.verdicts.clear();
        for verifier in &self.verifiers {
            let entries = verifier.verify(ctx);
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
}

impl Default for VerifierBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in bus verifier adapters ──────────────────────────────────────
//
// These adapters are stubs that register on the bus. The existing
// event-driven verifier system (VerifierHandler + CorrectionLoop)
// already handles async verification via BusEvents. The bus collects
// structured findings from BusVerifiers that don't need async I/O.
// Async verifiers continue to operate through the event bus.
//
// Future work: migrate the async verifiers to implement BusVerifier
// once the bus supports async verification.

/// Adapter: security verifier stub on the bus.
///
/// The full async security verifier runs via the event bus. This stub
/// registers on the bus so it's counted in `verifier_bus.verifiers()`
/// and can be extended later.
pub struct SecurityBusVerifier;

impl BusVerifier for SecurityBusVerifier {
    fn name(&self) -> &str {
        "security"
    }

    fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
        Vec::new()
    }
}

/// Adapter: git verifier stub on the bus.
pub struct GitBusVerifier;

impl BusVerifier for GitBusVerifier {
    fn name(&self) -> &str {
        "git"
    }

    fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
        Vec::new()
    }
}

/// Build a VerifierBus with all built-in verifiers registered.
pub fn default_verifier_bus() -> VerifierBus {
    let mut bus = VerifierBus::new();
    bus.register(Box::new(SecurityBusVerifier));
    bus.register(Box::new(GitBusVerifier));
    bus
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

    fn make_ctx() -> VerifyContext {
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

        bus.run(&make_ctx());
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

        bus.run(&make_ctx());
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

        bus.run(&make_ctx());
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

        bus.run(&make_ctx());
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
}
