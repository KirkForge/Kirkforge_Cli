# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`wo/21-integrate`** at commit `21a2ee6`. All 11 `wo/21.*` branches merged. 34 commits ahead of `origin/dev`.

## WO 21 series — completed this session

All `wo/21.*` topic branches merged into `wo/21-integrate`. Post-merge fixes and remaining R-items completed in the same session.

### Per-workorder status (verified against code, not just commit messages)

| WO | Done | Partial | Not Done | Tracking |
|----|------|---------|----------|----------|
| 21.0 overview | — | — | — | Tracking doc only |
| 21.1 focus-scope | — | — | — | Decisions folded into 21.11-R0 (draw/video yeeted) |
| 21.2 plugins-rust-native | — | — | — | Folded into 21.11 |
| 21.3 stratum-compression | — | — | — | Folded into 21.11-R1/R2/R3 |
| 21.4 adapter-gaps | — | — | — | Empty branch (0 unique commits); work in 21.5 |
| 21.5 tools-mcp | R1,R3,R4(roots),R5,R6,R7,R8 | R4(sampling deferred) | R2(PTY),R9(Anthropic beta) | 21.0.14 |
| 21.6 context-memory | R3(tokenizer),R4(incremental),R5(rename),R2(memory auto-populate) | — | R1(LSP federation) | 21.0.14 |
| 21.7 sandbox-trust | R5(production refusal),R6(PathGuardTower),R7(sig default-on),R8(ponytail) | — | R1(landlock),R2(seccomp),R3(diff-review) | 21.0.14 |
| 21.8 tui-agentloop | — | — | — | Empty branch (0 unique commits) |
| 21.9 discipline-debt | R1,R3,R4,R6,R7,R8,R9,R10 | — | R2(coverage bump),R5(fuzzing) | R2 and R5 DONE this session |
| 21.10 mcp-first | — | — | — | Empty branch (0 unique commits) |
| 21.11 plugin-rebuild | R0-R8 | R9(ADR done) | — | All done |
| 21.0.12 doc-verify | — | — | — | TECHNICAL.md rewritten, stale refs purged |
| 21.0.14 deferred-tracker | — | — | — | 11 deferred items tracked |

### Cross-cutting work this session

- **Clippy**: all 8 pre-existing errors fixed, now fully green
- **Orphan cleanup**: `kf-budget-cli/`, `kf-compress-cli/` deleted
- **PONYTAIL-DEBT.md**: 11 entries for deleted crates removed
- **Shell plugins yeeted**: `plugins/kf-budget/`, `plugins/kf-stratum/` deleted (code folded into Rust)
- **kf-draw/kf-video**: fully removed from codebase and docs
- **Plugin repos**: all 3 originals confirmed fully folded (Plugin1→kf-plugin-sdk/host, Plugin2→kf-compress-core, Plugin3→kf-budget-core)
- **WO status lines**: all updated from "Planned" to "Partial" or "Done"
- **ADR-068**: CLI yeet decision documented
- **Swap**: 8GB disk swap + 8GB zram (zstd, priority 100) added to fix OOM kills

## Remaining work (WO 21.0.14 — all tracked)

### High priority (security/correctness)

1. **21.7-R1**: Default OS sandbox — landlock on Linux (filesystem confinement + network egress block). Plan ready: ~120 lines of unsafe syscall glue in `sandbox.rs`, no new deps, runtime kernel version detection. Must allow-list Ollama/MCP server ports.
2. **21.7-R3**: Diff-review-before-apply — every file-modifying tool presents real diff for y/n before write. Plan ready: `DiffReviewGate` callback in `ToolContext`, `DiffReviewPolicy` enum (Off/Always/UntrustedOnly), reuse existing `diff_preview.rs` + `approval.rs` TUI components.

### Medium priority (functionality gaps)

3. **21.6-R1**: LSP federation — augment context-index call-graph resolution with rust-analyzer go-to-definition results. Plan ready: `enrich_via_lsp()` method on `ContextIndex`, optional `kf-lsp` dep, async bridge.
4. **21.5-R2**: Bash PTY/streaming — `portable-pty` behind feature flag for interactive commands. Adds ~2MB to binary.
5. **21.5-R4 (sampling)**: MCP sampling/createMessage — server-initiated reverse LLM call. Security surface needs approval UX.

### Low priority (defer with reason)

6. **21.7-R2**: seccomp syscall filter — ADR-054 says libseccomp breaks static binary. Needs formal close with updated evidence.
7. **21.5-R9**: Anthropic computer-use beta — local headless-Chrome tool already works with any model. Anthropic's server-side computer_use is redundant.

## Plugin architecture

Two-path dispatch (ADR-050). Folded plugins (stratum, kf-budget) compiled-in via `stratum`/`budget` feature flags (both default-on). Shell fallbacks yeeted this session. Runtime `enabled_plugins`/`disabled_plugins` gate registration. `/plugins toggle` works for both compiled-in and shell plugins. `kf-plugin` (Node SDK) remains shell-only.

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --all-targets -- -D warnings`: PASS
- HEAD: `21a2ee6`

## Known pre-existing test failures (NOT from WO 21)

- `bundled_node_sdk_tool_executes_via_host` (requires Node.js)
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5
- `tui::keys::slash_commands::tests::slash_command_table_covers_all_triggers` — drift test needs update
- `session::plugin_tools::*` (7), `tools::bash::*` (2) — env/binary-dependent

## Infra note

- Machine: 15GB RAM, 16GB swap (8GB zram zstd priority 100 + 8GB disk priority -1)
- Prior OOM kills (4 in 24h) caused by: 1.6GB opencode.db + Rust compiles exhausting 15GB RAM + 512MB swap
- Fix: swap increased, opencode.db cleanup task ran (subagent — verify if it actually reduced size)
- `cargo test --workspace` can still OOM — run per-module tests

## Next steps (prioritized)

1. **21.7-R1**: Landlock default sandbox (plan ready, execute)
2. **21.7-R3**: Diff-review-before-apply (plan ready, execute)
3. **21.6-R1**: LSP federation (plan ready, execute)
4. **21.5-R2**: Bash PTY (plan ready, execute)
5. Update `state.md` after each item

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
