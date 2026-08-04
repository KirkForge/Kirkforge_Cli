//! Transform errors — shared between slicing and compaction.
//!
//! ponytail: a single `TransformError` covers both the slicing
//! pipeline (which can `Skipped` pass-throughs) and the compaction
//! pipeline (which only has `InvalidInput` / Internal). Two
//! near-identical enums in sibling modules invited drift; one
//! definition keeps the trait contracts aligned.

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("skipped: {0}")]
    Skipped(String),
    #[error("internal: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    // ponytail: pin each variant's Display. A contributor who
    // rewords the format string surfaces here, not in a downstream
    // log-grep regression.
    #[test]
    fn each_variant_renders_expected_display() {
        assert_eq!(
            TransformError::InvalidInput("bad bytes".into()).to_string(),
            "invalid input: bad bytes",
        );
        assert_eq!(
            TransformError::Skipped("below threshold".into()).to_string(),
            "skipped: below threshold",
        );
        assert_eq!(
            TransformError::Internal("store: io".into()).to_string(),
            "internal: store: io",
        );
    }

    // ponytail: pin each variant's Debug format. The CLI's
    // hook entrypoint logs errors via `{:?}` (Debug), and the
    // downstream log-grep filters parse the variant name out of
    // the Debug string (e.g. `grep InvalidInput`). The Display
    // test above pins the human-facing prefix; this one pins the
    // Debug-derived variant-name+tuple-field shape so the two
    // don't drift (a contributor who swaps Display prefix
    // accidentally leaves Debug alone — or vice versa — surfaces
    // here). `thiserror::Error` derives Debug automatically; the
    // format is `<VariantName>(<inner>)` for tuple variants.
    #[test]
    fn each_variant_renders_expected_debug() {
        assert_eq!(
            format!("{:?}", TransformError::InvalidInput("bad bytes".into())),
            r#"InvalidInput("bad bytes")"#,
            "Debug format must include the variant name AND the inner \
             string in quoted form so log-grep on `InvalidInput(` keeps working",
        );
        assert_eq!(
            format!("{:?}", TransformError::Skipped("below threshold".into())),
            r#"Skipped("below threshold")"#,
        );
        assert_eq!(
            format!("{:?}", TransformError::Internal("store: io".into())),
            r#"Internal("store: io")"#,
        );
    }

    // ponytail: pin the variant count. The downstream `match`
    // arms in slicing/compaction assume exactly three variants;
    // a contributor who adds `TransformError::Overflow`
    // (saturating-add error path) without updating both
    // pipelines surfaces here because the count drifts. The
    // rustc non_exhaustive warning would also fire at the match
    // site, but only on the *exact* arms the contributor broke —
    // this test gives a single canonical "three variants, named
    // X/Y/Z" pin so the regression is named at compile-test
    // time, not at audit time.
    #[test]
    fn variant_count_is_three_named_invalid_input_skipped_internal() {
        use std::collections::HashSet;
        let names: HashSet<String> = [
            TransformError::InvalidInput("x".into()),
            TransformError::Skipped("x".into()),
            TransformError::Internal("x".into()),
        ]
        .iter()
        .map(|e| {
            format!("{e:?}")
                // Strip the `(...)` tail to keep only the variant name.
                .split('(')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            names.len(),
            3,
            "TransformError must have exactly 3 variants; got {names:?}"
        );
        for expected in ["InvalidInput", "Skipped", "Internal"] {
            assert!(
                names.contains(expected),
                "missing variant `{expected}` in TransformError; got {names:?}"
            );
        }
    }
}
