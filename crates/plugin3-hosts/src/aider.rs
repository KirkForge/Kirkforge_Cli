//! Aider shim — STUB. Per ADR-0013 Aider pipes tool results via
//! stdin; the shim just adapts to JSON-in/JSON-out the same way
//! the Claude Code shim does. No real handler until a user
//! reports a need.
//!
//! ponytail: stub-only. Same rationale as `cursor.rs`.

#[cfg(test)]
mod tests {
    #[test]
    fn stub_present() {
        assert_eq!(
            std::module_path!(),
            "plugin3_hosts::aider::tests",
            "aider module path drifted from ADR-0013 layout"
        );
    }
}
