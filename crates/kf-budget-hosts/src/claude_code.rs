//! Claude Code shim — STUB. Per ADR-0013 the real payload
//! translation for Claude Code is performed by `kf-budget-cli::hooks`
//! consuming the canonical types from `kf-budget-hosts::canonical`
//! directly. No per-host shim module is wired today.
//!
//! ponytail: stub-only. Keeping the module file honest about what's
//! planned mirrors the `cursor` and `aider` stub modules. A future
//! contributor who extracts the CLI hook handlers' host-specific
//! envelope parsing into this crate grows real `handle_*` functions
//! here and wires them from `kf-budget-cli::hooks`.

#[cfg(test)]
mod tests {
    // ponytail: a test that asserts the module file exists and is
    // wired. A contributor who deletes the stub without finishing
    // it fails CI here.
    #[test]
    fn stub_present() {
        assert_eq!(
            std::module_path!(),
            "kf_budget_hosts::claude_code::tests",
            "claude_code module path drifted from ADR-0013 layout"
        );
    }
}
