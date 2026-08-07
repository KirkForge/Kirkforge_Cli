use std::hash::Hash;
use std::path::PathBuf;

// ── Event Kinds ─────────────────────────────────────────────────────────

/// All event kinds the bus supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum EventKind {
    FileRead,
    FileWrite,
    Edit,
    BashExec,
    GitOperation,
    LintRun,
    TypeCheck,
    SecurityScan,
    ToolError,
}

impl EventKind {
    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            EventKind::FileRead => "file_read",
            EventKind::FileWrite => "file_write",
            EventKind::Edit => "edit",
            EventKind::BashExec => "bash_exec",
            EventKind::GitOperation => "git_operation",
            EventKind::LintRun => "lint_run",
            EventKind::TypeCheck => "type_check",
            EventKind::SecurityScan => "security_scan",
            EventKind::ToolError => "tool_error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_label_round_trip() {
        for kind in [
            EventKind::FileRead,
            EventKind::FileWrite,
            EventKind::Edit,
            EventKind::BashExec,
            EventKind::GitOperation,
            EventKind::LintRun,
            EventKind::TypeCheck,
            EventKind::SecurityScan,
            EventKind::ToolError,
        ] {
            assert!(!kind.label().is_empty());
            assert_eq!(format!("{kind}"), kind.label());
        }
    }

    #[test]
    fn bus_event_kind_matches_variant() {
        let path = PathBuf::from("/tmp/test.rs");
        assert_eq!(
            BusEvent::FileRead(FileReadEvent { path: path.clone(), size_bytes: 10, truncated: false }).kind(),
            EventKind::FileRead
        );
        assert_eq!(
            BusEvent::FileWrite(FileWriteEvent { path: path.clone(), content_length: 5, content_hash: 0 }).kind(),
            EventKind::FileWrite
        );
        assert_eq!(
            BusEvent::Edit(EditEvent { path: path.clone(), diff: "-a\n+b".into() }).kind(),
            EventKind::Edit
        );
        assert_eq!(
            BusEvent::BashExec(BashExecEvent {
                command: "ls".into(),
                exit_code: 0,
                stdout_len: 10,
                stderr_len: 0,
                workdir: None
            }).kind(),
            EventKind::BashExec
        );
        assert_eq!(
            BusEvent::GitOperation(GitOperationEvent { args: vec!["status".into()], output: "clean".into(), success: true }).kind(),
            EventKind::GitOperation
        );
        assert_eq!(
            BusEvent::LintRun(LintRunEvent { tool: "clippy".into(), target: "main.rs".into(), findings: vec![] }).kind(),
            EventKind::LintRun
        );
        assert_eq!(
            BusEvent::TypeCheck(TypeCheckEvent { target: "main.rs".into(), errors: vec![], success: true }).kind(),
            EventKind::TypeCheck
        );
        assert_eq!(
            BusEvent::SecurityScan(SecurityScanEvent { target: ".".into(), issues: vec![] }).kind(),
            EventKind::SecurityScan
        );
        assert_eq!(
            BusEvent::ToolError(ToolErrorEvent { tool: "bash".into(), error: "fail".into() }).kind(),
            EventKind::ToolError
        );
    }

    #[test]
    fn verdict_debug_clone() {
        let clean = Verdict::Clean;
        assert_eq!(format!("{:?}", clean.clone()), "Clean");

        let fixable = Verdict::Fixable(FixSuggestion {
            description: "fix".into(),
            file: PathBuf::from("x.rs"),
            original: "a".into(),
            replacement: "b".into(),
            severity: "low".into(),
            command: None,
        });
        let cloned = fixable.clone();
        assert!(matches!(cloned, Verdict::Fixable(_)));

        let unfixable = Verdict::Unfixable(VerificationError {
            description: "err".into(),
            file: None,
            details: "d".into(),
        });
        let cloned = unfixable.clone();
        assert!(matches!(cloned, Verdict::Unfixable(_)));

        let skipped = Verdict::Skipped("reason".into());
        let cloned = skipped.clone();
        assert!(matches!(cloned, Verdict::Skipped(_)));
    }

    #[test]
    fn event_kind_equality() {
        assert_eq!(EventKind::FileRead, EventKind::FileRead);
        assert_ne!(EventKind::FileRead, EventKind::FileWrite);
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

// ── Events ──────────────────────────────────────────────────────────────

/// A concrete event describing a tool operation.
///
/// Constructed by the executor dispatch layer and passed directly to
/// verifiers and the correction loop — no intermediate pub/sub bus.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "payload")]
pub enum BusEvent {
    FileRead(FileReadEvent),
    FileWrite(FileWriteEvent),
    Edit(EditEvent),
    BashExec(BashExecEvent),
    GitOperation(GitOperationEvent),
    LintRun(LintRunEvent),
    TypeCheck(TypeCheckEvent),
    SecurityScan(SecurityScanEvent),
    ToolError(ToolErrorEvent),
}

impl BusEvent {
    /// The event kind discriminator.
    pub fn kind(&self) -> EventKind {
        match self {
            BusEvent::FileRead(_) => EventKind::FileRead,
            BusEvent::FileWrite(_) => EventKind::FileWrite,
            BusEvent::Edit(_) => EventKind::Edit,
            BusEvent::BashExec(_) => EventKind::BashExec,
            BusEvent::GitOperation(_) => EventKind::GitOperation,
            BusEvent::LintRun(_) => EventKind::LintRun,
            BusEvent::TypeCheck(_) => EventKind::TypeCheck,
            BusEvent::SecurityScan(_) => EventKind::SecurityScan,
            BusEvent::ToolError(_) => EventKind::ToolError,
        }
    }
}

// ── Event payloads ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileReadEvent {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileWriteEvent {
    pub path: PathBuf,
    pub content_length: usize,
    pub content_hash: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EditEvent {
    pub path: PathBuf,
    pub diff: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BashExecEvent {
    pub command: String,
    pub exit_code: i32,
    pub stdout_len: usize,
    pub stderr_len: usize,
    pub workdir: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GitOperationEvent {
    pub args: Vec<String>,
    pub output: String,
    pub success: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LintRunEvent {
    pub tool: String,
    pub target: String,
    pub findings: Vec<LintFinding>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TypeCheckEvent {
    pub target: String,
    pub errors: Vec<String>,
    pub success: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityScanEvent {
    pub target: String,
    pub issues: Vec<SecurityIssue>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolErrorEvent {
    pub tool: String,
    pub error: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LintFinding {
    pub severity: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SecurityIssue {
    pub severity: String,
    pub kind: String,
    pub description: String,
    pub file: Option<String>,
}

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
    pub description: String,
    pub file: PathBuf,
    pub original: String,
    pub replacement: String,
    pub severity: String,
    pub command: Option<String>,
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
/// Verifiers are called directly by the dispatch layer after each
/// tool call. Unlike generic handlers, verifiers return a [`Verdict`]
/// that the correction loop can act on.
#[async_trait::async_trait]
pub trait Verifier: Send + Sync {
    /// Unique verifier name (e.g. "lint", "type-check", "git", "security").
    fn name(&self) -> &str;

    /// Priority: lower number = higher priority (runs first).
    /// Used by the truth model — the first definitive result wins.
    fn priority(&self) -> u8;

    /// Verify the state after a tool event.
    async fn verify(&self, event: &BusEvent) -> Verdict;
}
