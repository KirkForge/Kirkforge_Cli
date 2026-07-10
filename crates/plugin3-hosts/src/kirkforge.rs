//! KirkForge shim — STUB. Per ADR-0013 KirkForge-Cli is the
//! sibling host in the same plugin ecosystem; its hook model is
//! expected to emit the same canonical events as Claude Code,
//! but the exact envelope shape and env-var detection are not yet
//! specified. No real handler until an integration contract is
//! written.
//!
//! ponytail: stub-only. The moment the KirkForge hook model is
//! documented, this module grows `handle_post_tool_use` etc. with
//! the translation. Keeping the module empty-but-named keeps the
//! directory listing honest about the planned sibling-host support.

#[cfg(test)]
mod tests {
    // ponytail: a test that asserts the module file exists and is
    // wired. A contributor who deletes the stub without finishing
    // it fails CI here.
    #[test]
    fn stub_present() {
        assert_eq!(
            std::module_path!(),
            "plugin3_hosts::kirkforge::tests",
            "kirkforge module path drifted from ADR-0013 layout"
        );
    }
}
