//! The reducer (WO 37.2, ADR-076): fold a delegation's verification
//! state into the `ReducedStatePacket` attached to its
//! `DelegationResult`. Design-from-contract — no TS source exists; the
//! fold rules are pinned by ADR-076.
//!
//! The fold covers the full `crate::routing::correction` state vocabulary,
//! but only `changes` and `verification.security` have producers today;
//! lint/types/graph stay at default until deterministic emitters ship.

use std::path::PathBuf;

use crate::routing::correction::{
    Changes, LintState, OverallVerdict, ReducedStatePacket, SecurityState, TypesState,
};

use crate::types::DelegationResult;
use crate::verifier::{apply_security_findings, scan_files};

/// Derive the overall verdict from the per-category states (ADR-076):
/// Fail ← any critical security finding or error-class category;
/// Warn ← non-error findings only; Pass ← all clean, including the
/// empty case. The reducer never emits `Unknown`.
pub fn fold_overall(
    lint: &LintState,
    types: &TypesState,
    security: &SecurityState,
) -> OverallVerdict {
    if security.critical > 0 || lint.errors > 0 || types.errors > 0 {
        OverallVerdict::Fail
    } else if security.findings > 0 || security.high > 0 || lint.warnings > 0 {
        OverallVerdict::Warn
    } else {
        OverallVerdict::Pass
    }
}

/// Changes from the delegation's written-file signals.
// ponytail: insertions/deletions stay 0 — signals carry hashes and byte
// counts, not line deltas. Add a diff-against-before_hash producer if a
// consumer needs line counts.
pub fn changes_from_result(result: &DelegationResult) -> Changes {
    let files = crate::types::extract_emission_files(result);
    Changes {
        files_changed: files.len() as i64,
        paths: files.into_iter().map(|f| f.path).collect(),
        insertions: 0,
        deletions: 0,
    }
}

/// Resolve a written path against the delegation cwd. Signal paths are
/// relative (mode executors record the file name, not the joined path).
fn resolve(path: &str, cwd: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::path::Path::new(cwd).join(p)
    }
}

/// Build the packet for one delegation (ADR-076): changes from the
/// written-file signals, security from scanning those files (resolved
/// against `cwd`), lint/types/graph at default (no producers in this
/// crate), overall from the fold. `turn` is always 0 — each
/// `delegate()` call is one delegation.
pub fn reduce_result(
    task_id: &str,
    turn: i64,
    ts: &str,
    cwd: &str,
    result: &DelegationResult,
) -> ReducedStatePacket {
    let changes = changes_from_result(result);
    let files: Vec<PathBuf> = changes.paths.iter().map(|p| resolve(p, cwd)).collect();
    let findings = scan_files(&files);
    let mut packet = ReducedStatePacket {
        task_id: task_id.to_string(),
        turn,
        ts: ts.to_string(),
        changes,
        ..Default::default()
    };
    apply_security_findings(&mut packet, &findings);
    packet.verification.overall = fold_overall(
        &packet.verification.lint,
        &packet.verification.types,
        &packet.verification.security,
    );
    packet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::correction::CorrectionAction;
    use serde_json::json;

    fn result_with_written(paths: &[String]) -> DelegationResult {
        let mut r = DelegationResult::default();
        r.signals.push(crate::types::Signal {
            id: "s1".into(),
            task_id: "t1".into(),
            domain: "code".into(),
            kind: "artifact.emitted".into(),
            source: "agent".into(),
            ts: "now".into(),
            value: serde_json::json!({
                "files": paths.iter().map(|p| json!({"path": p, "sha256": "x", "bytes": 1})).collect::<Vec<_>>()
            }),
            confidence: None,
        });
        r
    }

    #[test]
    fn fold_overall_table() {
        let sec = |findings, critical, high| SecurityState {
            findings,
            critical,
            high,
        };
        let lint = |errors, warnings| LintState { errors, warnings };
        let types = |errors| TypesState { errors };
        let cases: &[(&str, LintState, TypesState, SecurityState, OverallVerdict)] = &[
            (
                "empty is pass",
                lint(0, 0),
                types(0),
                sec(0, 0, 0),
                OverallVerdict::Pass,
            ),
            (
                "critical security fails",
                lint(0, 0),
                types(0),
                sec(1, 1, 0),
                OverallVerdict::Fail,
            ),
            (
                "lint errors fail",
                lint(1, 0),
                types(0),
                sec(0, 0, 0),
                OverallVerdict::Fail,
            ),
            (
                "types errors fail",
                lint(0, 0),
                types(2),
                sec(0, 0, 0),
                OverallVerdict::Fail,
            ),
            (
                "high security warns",
                lint(0, 0),
                types(0),
                sec(1, 0, 1),
                OverallVerdict::Warn,
            ),
            (
                "lint warnings warn",
                lint(0, 3),
                types(0),
                sec(0, 0, 0),
                OverallVerdict::Warn,
            ),
            (
                "mixed fail beats warn",
                lint(0, 2),
                types(0),
                sec(2, 1, 1),
                OverallVerdict::Fail,
            ),
        ];
        for (name, lint, types, security, want) in cases {
            assert_eq!(fold_overall(lint, types, security), *want, "case: {name}");
        }
    }

    #[test]
    fn changes_from_result_counts_written_paths() {
        let r = result_with_written(&["a.py".into(), "b.rs".into()]);
        let c = changes_from_result(&r);
        assert_eq!(c.files_changed, 2);
        assert_eq!(c.paths, vec!["a.py".to_string(), "b.rs".to_string()]);
        assert_eq!(c.insertions, 0);
        assert_eq!(c.deletions, 0);
    }

    #[test]
    fn changes_from_result_empty_when_no_files() {
        assert_eq!(
            changes_from_result(&DelegationResult::default()).files_changed,
            0
        );
    }

    #[test]
    fn reduce_clean_writes_pass_packet() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.py"), "print('hi')\n").unwrap();
        let abs = dir.path().join("clean.py").to_string_lossy().to_string();
        let r = result_with_written(&[abs]);
        let packet = reduce_result("t-1", 0, "123", dir.path().to_str().unwrap(), &r);
        assert_eq!(packet.task_id, "t-1");
        assert_eq!(packet.turn, 0);
        assert_eq!(packet.ts, "123");
        assert_eq!(packet.changes.files_changed, 1);
        assert_eq!(packet.verification.security.findings, 0);
        assert_eq!(packet.verification.overall, OverallVerdict::Pass);
    }

    #[test]
    fn reduce_scan_finds_critical_finding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("evil.py"), "eval('evil')\n").unwrap();
        let abs = dir.path().join("evil.py").to_string_lossy().to_string();
        let r = result_with_written(&[abs]);
        let packet = reduce_result("t-2", 0, "123", dir.path().to_str().unwrap(), &r);
        assert_eq!(packet.verification.security.critical, 1);
        assert_eq!(packet.verification.overall, OverallVerdict::Fail);
    }

    #[test]
    fn reduce_resolves_relative_paths_against_cwd() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rel.py"), "eval('evil')\n").unwrap();
        let r = result_with_written(&["rel.py".into()]);
        // Signal path is relative; the scan must find it via cwd.
        let packet = reduce_result("t-3", 0, "123", dir.path().to_str().unwrap(), &r);
        assert_eq!(
            packet.verification.security.critical, 1,
            "relative path must resolve via cwd"
        );
    }

    #[test]
    fn reduce_empty_signals_fold_to_pass() {
        let packet = reduce_result("t-4", 0, "123", "/tmp", &DelegationResult::default());
        assert_eq!(packet.changes.files_changed, 0);
        assert_eq!(packet.verification.overall, OverallVerdict::Pass);
    }

    #[test]
    fn decide_correction_consumes_reducer_packet() {
        // ADR-076: what correction consumes. Clean reducer packet → Accept
        // on turn 0 (the pre-reducer Unknown cycling is gone); critical
        // finding → Escalate.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clean.py"), "print('hi')\n").unwrap();
        std::fs::write(dir.path().join("evil.py"), "eval('evil')\n").unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();

        let clean = reduce_result(
            "t-c",
            0,
            "1",
            &cwd,
            &result_with_written(&[dir.path().join("clean.py").to_string_lossy().to_string()]),
        );
        assert_eq!(
            crate::routing::correction::decide_correction(&clean, 0, 3, 0, 0, 0.0, None, None)
                .action,
            CorrectionAction::Accept
        );

        let evil = reduce_result(
            "t-e",
            0,
            "1",
            &cwd,
            &result_with_written(&[dir.path().join("evil.py").to_string_lossy().to_string()]),
        );
        assert_eq!(
            crate::routing::correction::decide_correction(&evil, 0, 3, 0, 0, 0.0, None, None)
                .action,
            CorrectionAction::Escalate
        );
    }

    #[test]
    fn reduce_never_emits_unknown() {
        // Unknown only exists as the Default pre-reduction; the reducer's
        // range is {Pass, Warn, Fail} (ADR-076).
        let states = [
            (
                LintState::default(),
                TypesState::default(),
                SecurityState::default(),
            ),
            (
                LintState {
                    errors: 0,
                    warnings: 1,
                },
                TypesState::default(),
                SecurityState::default(),
            ),
            (
                LintState::default(),
                TypesState { errors: 1 },
                SecurityState::default(),
            ),
        ];
        for (l, t, s) in states {
            assert_ne!(fold_overall(&l, &t, &s), OverallVerdict::Unknown);
        }
    }
}
