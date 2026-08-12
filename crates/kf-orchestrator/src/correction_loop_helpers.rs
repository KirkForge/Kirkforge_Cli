//! Small helpers shared across the correction loop. Kept in a separate
//! module so the loop's main file reads top-to-bottom without misc noise.

use crate::types::TaskValidationResult;

/// Map a `TaskValidationResult` to the memory-outcome bucket the empirical
/// router uses ("pass" / "error"). Mirrors `taskOutcomeFromValidation` in
/// `correction-core`.
pub fn task_outcome_from_validation(v: &TaskValidationResult) -> &'static str {
    match v.status.as_str() {
        "pass" => "pass",
        "fail" => "task_fail",
        "error" => "validator_error",
        _ => "validator_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_status_buckets() {
        for (status, want) in [
            ("pass", "pass"),
            ("fail", "task_fail"),
            ("error", "validator_error"),
            ("skipped", "validator_error"),
            ("other", "validator_error"),
        ] {
            let v = TaskValidationResult {
                status: status.into(),
                ..Default::default()
            };
            assert_eq!(task_outcome_from_validation(&v), want);
        }
    }
}
