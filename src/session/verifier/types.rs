use crate::session::event_bus::BusEvent;
use std::path::PathBuf;

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
    /// Human-readable description of the issue.
    pub description: String,
    /// The file that needs fixing.
    pub file: PathBuf,
    /// The original text to replace.
    pub original: String,
    /// The replacement text.
    pub replacement: String,
    /// Suggested severity: "error" | "warning" | "info"
    pub severity: String,
    /// Optional external command that performs the fix in-place (e.g.
    /// `rustfmt`). When set and `original`/`replacement` are empty, the
    /// correction loop runs this command on `file` instead of doing a text
    /// replacement.
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
/// Verifiers register as event bus handlers to react to specific events.
/// Unlike generic handlers, verifiers return a [`Verification`] that
/// the correction loop can act on.
#[async_trait::async_trait]
pub trait Verifier: Send + Sync {
    /// Unique verifier name (e.g. "lint", "type-check", "git", "security").
    fn name(&self) -> &str;

    /// Priority: lower number = higher priority (runs first).
    /// Used by the truth model — the first definitive result wins.
    fn priority(&self) -> u8;

    /// Verify the state after a tool event.
    ///
    /// Implementations should be fast and deterministic — this runs
    /// synchronously in the tool execution pipeline.
    async fn verify(&self, event: &BusEvent) -> Verdict;
}
