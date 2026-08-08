pub mod build;
pub mod bus;
pub mod git;
mod helpers;
/// Verifier slots — deterministic post-execution checks and correction loop.
///
/// Verifiers receive tool events directly from the dispatch layer
/// and react to specific tool outcomes. Unlike model-based tool calling,
/// verifiers run deterministic checks:
///
/// - **Build verifier**: runs `cargo build` on edited Rust files
/// - **Lint verifier**: runs linter on edited files
/// - **Type-check verifier**: runs type checker on changed code
/// - **Test verifier**: runs targeted tests for edited Rust files
/// - **Git verifier**: validates git state after operations
/// - **Security verifier**: scans written files for dangerous patterns
///
/// # Correction loop
///
/// When a verifier finds an issue, it emits a [`Verification`] which
/// the correction loop processes. The correction loop decides whether
/// to auto-fix (deterministic) or report back to the model.
///
/// # Truth model
///
/// When multiple verifiers disagree, precedence rules determine which
/// result is authoritative. The system runs verifiers in priority order
/// and stops at the first definitive result.
pub mod lint;
pub mod plugin;
pub mod rustfmt;
pub mod security;
pub mod test;

pub mod correction;
pub mod handler;
pub mod slots;
pub mod types;

pub use bus::{BusVerifier, Severity, VerdictEntry, VerifierBus, VerifierSource, VerifyContext};
pub use correction::{CorrectionLoop, CorrectionResult};
pub use handler::VerifierHandler;
pub use slots::VerifierSlots;
pub use types::{
    BashExecEvent, BusEvent, EditEvent, EventKind, FileReadEvent, FileWriteEvent, FixSuggestion,
    LintFinding, LintRunEvent, SecurityIssue, SecurityScanEvent, ToolErrorEvent, TypeCheckEvent,
    Verdict, VerificationError, Verifier,
};

#[cfg(test)]
mod tests;
