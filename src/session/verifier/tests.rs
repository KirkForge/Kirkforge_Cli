use super::*;
use crate::session::executor::types::VerificationOutcome;
use crate::session::verifier::bus::{
    BusVerifier, Severity, VerdictEntry, VerifierBus, VerifierSource, VerifyContext,
};
use crate::session::verifier::types::FixSuggestion;
use crate::shared::test_util::remove_test_file;
use std::path::PathBuf;

// ── BusVerifier-based test helpers (WO 47.14: replaces MockVerifier) ────

struct StubBusVerifier {
    name: String,
    entries: Vec<VerdictEntry>,
}

impl BusVerifier for StubBusVerifier {
    fn name(&self) -> &str {
        &self.name
    }
    fn verify(&self, _ctx: &VerifyContext) -> Vec<VerdictEntry> {
        self.entries.clone()
    }
}

fn make_verify_ctx() -> VerifyContext {
    VerifyContext {
        sandbox_dir: PathBuf::from("/tmp"),
        changed_files: vec![PathBuf::from("/tmp/test.rs")],
        event_kind: None,
        tool_name: None,
        content_hash: 0,
        bash_command: None,
        bash_exit_code: None,
        bash_workdir: None,
    }
}

fn fix_entry(description: &str, file: &str, original: &str, replacement: &str) -> VerdictEntry {
    VerdictEntry {
        source: VerifierSource::Lint,
        severity: Severity::Warning,
        message: description.into(),
        file: Some(PathBuf::from(file)),
        line: None,
        fix: Some(FixSuggestion {
            description: description.into(),
            file: PathBuf::from(file),
            original: original.into(),
            replacement: replacement.into(),
            severity: "warning".into(),
            command: None,
            line: None,
        }),
    }
}

#[tokio::test]
async fn test_empty_bus_returns_no_corrections() {
    let bus = VerifierBus::new();
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(bus.verdicts(), &[]).await;
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_fixable_verdict_applies_text_fix() {
    let dir = std::env::temp_dir();
    let path = dir.join("kf_code_bus_fix_test.rs");
    std::fs::write(&path, "let x = 1;").unwrap();

    let entries = vec![fix_entry(
        "unused variable",
        path.to_str().unwrap(),
        "let x = 1;",
        "let _x = 1;",
    )];
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(&entries, &[]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, VerificationOutcome::Fixed);
    assert!(results[0].message.contains("Auto-fixed"));

    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "let _x = 1;");
    remove_test_file(&path);
}

#[tokio::test]
async fn test_suggestion_when_no_fix_available() {
    let entries = vec![VerdictEntry {
        source: VerifierSource::Lint,
        severity: Severity::Warning,
        message: "ambiguous issue".into(),
        file: Some(PathBuf::from("src/lib.rs")),
        line: None,
        fix: Some(FixSuggestion {
            description: "ambiguous issue".into(),
            file: PathBuf::from("src/lib.rs"),
            original: "".into(),
            replacement: "".into(),
            severity: "warning".into(),
            command: None,
            line: None,
        }),
    }];
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(&entries, &[]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, VerificationOutcome::Suggestion);
    assert!(results[0].message.contains("Verifier suggestion"));
}

#[tokio::test]
async fn test_command_fix_runs_formatter() {
    let dir = std::env::temp_dir();
    let path = dir.join("kf_code_bus_command_fix.txt");
    std::fs::write(&path, "hello world").unwrap();

    let entries = vec![VerdictEntry {
        source: VerifierSource::Rustfmt,
        severity: Severity::Warning,
        message: "not formatted".into(),
        file: Some(path.clone()),
        line: None,
        fix: Some(FixSuggestion {
            description: "not formatted".into(),
            file: path.clone(),
            original: "".into(),
            replacement: "".into(),
            severity: "warning".into(),
            command: Some("true".into()),
            line: None,
        }),
    }];
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(&entries, &[]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, VerificationOutcome::Fixed);
    assert!(results[0].message.contains("Auto-formatted"));
    remove_test_file(&path);
}

#[tokio::test]
async fn test_error_verdict_produces_failed_result() {
    let entries = vec![VerdictEntry {
        source: VerifierSource::Security,
        severity: Severity::Error,
        message: "secret found".into(),
        file: None,
        line: None,
        fix: None,
    }];
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(&entries, &[]).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, VerificationOutcome::Failed);
    assert!(results[0].message.contains("secret found"));
}

#[tokio::test]
async fn test_bus_collects_verdicts_from_multiple_verifiers() {
    let mut bus = VerifierBus::new();
    bus.register(Box::new(StubBusVerifier {
        name: "lint".into(),
        entries: vec![VerdictEntry {
            source: VerifierSource::Lint,
            severity: Severity::Warning,
            message: "unused variable".into(),
            file: Some(PathBuf::from("test.rs")),
            line: None,
            fix: None,
        }],
    }));
    bus.register(Box::new(StubBusVerifier {
        name: "security".into(),
        entries: vec![VerdictEntry {
            source: VerifierSource::Security,
            severity: Severity::Error,
            message: "dangerous".into(),
            file: None,
            line: None,
            fix: None,
        }],
    }));

    bus.run(&make_verify_ctx());
    assert_eq!(bus.verdicts().len(), 2);
    assert!(bus.has_errors());
}

// ── CorrectionLoop constructor tests ─────────────────────────────────────

#[test]
fn correction_loop_new_uses_default_max_iterations() {
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    assert_eq!(loop_.max_iterations(), 3);
}

#[test]
fn correction_loop_with_max_iterations_overrides_default() {
    let loop_ =
        CorrectionLoop::new(crate::session::access::PathGuard::default()).with_max_iterations(7);
    assert_eq!(loop_.max_iterations(), 7);
}

#[test]
fn correction_loop_with_max_iterations_zero_allows_zero_iterations() {
    let loop_ =
        CorrectionLoop::new(crate::session::access::PathGuard::default()).with_max_iterations(0);
    assert_eq!(loop_.max_iterations(), 0);
}

#[tokio::test]
async fn correction_loop_run_from_verdicts_empty_returns_empty() {
    let loop_ = CorrectionLoop::new(crate::session::access::PathGuard::default());
    let results = loop_.run_from_verdicts(&[], &[]).await;
    assert!(results.is_empty());
}
