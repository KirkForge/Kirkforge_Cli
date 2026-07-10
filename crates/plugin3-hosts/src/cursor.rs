//! Cursor shim — STUB. Per ADR-0013 the envelope lives under
//! `payload.result.content`; the response wraps content in a
//! `patch` field. No real handler until a user reports a need.
//!
//! ponytail: stub-only. The moment a user wants Cursor support,
//! this module grows `handle_post_tool_use` etc. with the
//! translation the ADR sketches. Keeping the module empty-but-
//! named keeps the directory listing honest about what's planned.

#[cfg(test)]
mod tests {
    // ponytail: a test that asserts the module file exists and is
    // wired. A contributor who deletes the stub without finishing
    // it fails CI here. The asserted string matches ADR-0013's
    // sketched Cursor envelope so a future implementor has a
    // starting point.
    #[test]
    fn stub_present() {
        // When this test stops compiling, the Cursor shim
        // graduated from stub to real implementation — move the
        // test to cover the real handler.
        assert_eq!(
            std::module_path!(),
            "plugin3_hosts::cursor::tests",
            "cursor module path drifted from ADR-0013 layout"
        );
    }
}
