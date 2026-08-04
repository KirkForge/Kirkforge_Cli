# Lessons — kf-code rename + modularization session

## What I learned about this codebase

### Rename scope was massive
- 840 files changed for the initial rename commit, plus 3 fixup commits
- Every category of name had different rules:
  - Rust crate names in Cargo.toml: `kf-code` (hyphenated)
  - Rust crate names in `use` statements: `kf_code` (underscored)
  - Binary names: `kf-code` (hyphenated)
  - Env vars: `KF_CODE_*` (uppercased, underscored)
  - Directory names: `kf-code/`, `kf-budget/` (hyphenated)
  - Plugin manifests: `kf-code.toml`
  - Serde enum renames: `#[serde(rename = "kf-code")]`
  - CARGO_BIN_EXE: uses hyphenated binary names
  - Test function names: `kf_budget_hooks` (underscored, since they're Rust identifiers)

### Cargo normalizes hyphens to underscores
- In the `deps/` directory, cargo replaces hyphens with underscores in binary filenames
- `kf-code` becomes `kf_code-<hash>` in `target/debug/deps/`
- The testdoctor's `parse_binary_name` returns the filename form (`kf_code`), not the package form (`kf-code`)
- Test assertions must use the filename form when comparing parsed binary names

### Plugin names were the trickiest
- The plugin3 → kf-budget rename touched:
  - Crate names (plugin3-core → kf-budget-core, etc.)
  - Plugin directory names (kirkforge-plugin3 → kf-budget)
  - Plugin manifest names (kirkforge.toml → kf-code.toml)
  - Tool names inside manifests (plugin3_budget_status → budget_status)
  - FOLDED_PLUGINS mapping
  - Env var KIRKFORGE_PLUGIN3 → KF_CODE_PLUGIN3 for host detection
  - Slice markers <<plugin3:slice:>> → <<kf-budget:slice:>>
  - Host::KirkForge → Host::KfCode (with serde rename)

### No Rust toolchain was available initially
- Had to install rustup and the 1.88.0 toolchain manually
- The toolchain was partially installed (missing rustc binary) — had to uninstall and reinstall

### Subagent coordination issues
- Dispatched 4 subagents for the rename, but all were canceled before results could be retrieved
- Had to redo the work manually with sed/find
- Python was needed for one tricky nested-quote substitution that sed couldn't handle

## What I tried that didn't work
- Using subagents for the rename — coordination problems when they touch overlapping files
- Using sed for Rust identifier renames with hyphens — `kf-budget_binary_path` is not a valid Rust identifier, must be `kf_budget_binary_path`
- Using sed for serde assertion `"\"kirkforge\""` — the nested quotes confused sed. Python `str.replace()` handled it.

## What I'd do differently
- Use a single comprehensive sed script executed in the correct order (longest strings first) rather than incremental fixes
- Run `cargo check` immediately after the first rename pass to catch hyphenated identifiers
- The next phases (verifier bus unification, config macro, Executor decomposition) should each be a single focused commit with a passing gate

## Gate status
- `cargo check --workspace`: PASS
- `cargo test --workspace --no-fail-fast`: 2910 passed, 1 failed (bundled_node_sdk_tool_executes_via_host — requires Node.js, pre-existing)
- `cargo fmt --check`: Not yet run
- `cargo clippy --all-targets`: Not yet run