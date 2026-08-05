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
