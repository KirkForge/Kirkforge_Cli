# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`wo/21-integrate`** at commit `307b47e`. WO 21 + WO 22 series complete. ~75 commits ahead of `origin/dev`.

## WO 22 series — complete

(All items done or explicitly deferred. See earlier sessions.)

### This session's commits (WO 21.5-R2, 21.6-R1, 21.7-R2, 21.7-R3)

- `946a4a7` docs(21.7-R2): ADR-054 seccomp deferral quantified with landlock evidence
- `1516e1e` feat(21.7-R3): diff-review-before-apply safety gate
- `82b1517` feat(21.6-R1): LSP federation for context index call-edge resolution
- `307b47e` feat(21.5-R2): PTY support for interactive bash commands (feature-gated)

## Remaining deferred items

### Medium priority

1. **21.5-R2-R3**: Stream partial output to TUI (UX polish, not correctness)
2. **22.4-R2/R3**: TUI memory visibility + config flag
3. **22.6-R6**: Verifier findings in compaction tail

### Low priority

4. **22.5-R3/R4**: GitOperationEvent delete/wire + file-tool duration
5. **22.9-R4**: Bedrock/Vertex test hardening

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo clippy --workspace --features pty -- -D warnings`: PASS
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS
- HEAD: `307b47e`

## Known pre-existing test failures (NOT from WO 21/22)

- `compaction_use_llm_alias_backward_compat` — test expects `compaction_use_llm` under `[session]` TOML but parser only reads it from top level
- `bundled_node_sdk_tool_executes_via_host` (requires Node.js)
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5
- `session::plugin_tools::*` (7), `tools::bash::*` (2) — env/binary-dependent
- `kf-budget-core::tests::adr_0004_marker_block_pins_literal_prefix_and_suffix` — stale prefix literal

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
