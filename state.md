# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev`** at latest merge. WO 21 + WO 22 series merged. See commit log for details.

## Completed workorders

### WO 22 series (all done)

| WO | Status | Items |
|----|--------|-------|
| 22.1 | DONE | R1: landlock ABI rewrite |
| 22.2 | DONE | R1: default plugins (stratum + kf-budget) |
| 22.3 | DONE | R1: MCP URI validation, R2: capabilities handshake |
| 22.4 | DONE (R2/R3/R4 deferred) | R1: MAX_FACTS=3, FNV hash, rate limit |
| 22.5 | DONE (R3/R4 deferred) | R1: F2-F5 Enter handlers, R2: jobs_dirty refresh |
| 22.6 | DONE (R6 deferred) | R1-R5: token estimation, offload store, SearchState, PostHook, CorrectionResult |
| 22.7 | DONE | R1-R6: all over-engineering cleanup |
| 22.8 | DONE | R1-R18: doc fixes |
| 22.9 | DONE (R4 deferred) | R7: ADR-070, R8: ADR-070 |
| 22.10 | DONE | R1: verifier Skipped → CorrectionResult |
| 22.11 | DONE | R1-R4: catch_unwind, Skipped, pub(crate) |
| 22.12 | DONE | R1: 28 ADRs updated, path literals fixed |
| 22.13 | DONE | R1-R3: multi-turn prompt fix, bg task Notify, configurable concurrency |
| 22.14 | DONE | R1-R3: JSON-schema structured output, ResponseFormat enum |

### WO 21 series (all done or explicitly deferred)

| WO | Status | Items |
|----|--------|-------|
| 21.0 | DONE | Overview + rules |
| 21.1 | DONE | Scope decisions (draw/video yeeted) |
| 21.2 | DONE | Plugin rebuilds (21.11 superseded) |
| 21.3 | DONE | Stratum real transforms (21.11-R1) |
| 21.4 | DONE | Adapter gaps (tool_choice, JSON schema, native adapters) |
| 21.5 | DONE (R2/R4/R9 deferred) | R1: ripgrep grep, R3: MCP resource surfacing, R5: replace_all, R6: computer_use dedup, R7: HTML→md, R8: schema validation |
| 21.6 | DONE | R1: LSP federation, R2: memory auto-populate, R3: real tokenizer, R4: incremental rebuild, R5: compaction rename |
| 21.7 | DONE | R1: default landlock (via 22.1), R2: ADR-054 quantified, R3: diff-review-before-apply, R4: cosign blocking, R5: sandbox refusal, R6: PathGuardTower rename, R7: signature default-on, R8: plugin sandbox note |
| 21.8 | DONE | AppState decomposition, themes, multi-turn subagents, doom-loop, task concurrency |
| 21.9 | DONE | ADR drift fixes, test deadlock, coverage >75%, fuzzing, dead code, overclaims |
| 21.10 | DONE | MCP-first overlay (hooks/verifiers) |
| 21.11 | DONE | Plugin real rebuild, draw/video yeet, SDK/budget/stratum |

## Deferred items (explicitly tracked)

### Medium priority

1. **21.5-R2-R3**: Stream partial bash output to TUI via TurnEvent::BashPartialOutput. UX polish only — non-PTY path unchanged. Remaining: add TurnEvent variant, forward PTY output through event_tx, render streaming indicator in TUI tool-result card.
2. **21.5-R4**: MCP sampling/createMessage + roots/list. Sampling has a real security surface (server requests model completion). Remaining: implement sampling handler with user approval gate; roots/list (read-only, lower risk) should ship first.
3. **21.5-R9**: Anthropic computer_use beta (coordinate-vision model). Local headless-Chrome tool is the real differentiator. Remaining: opt-in beta path routed to Anthropic model, gated behind feature flag.
4. **22.4-R2/R3**: TUI memory visibility + config flag. Remaining: memory indicator widget in status bar; config flag to toggle memory display.
5. **22.6-R6**: Verifier findings included in compaction tail. Remaining: append CorrectionResult summaries to the compacted context so the model retains knowledge of what verifiers caught.

### Low priority

6. **22.5-R3**: GitOperationEvent documented as a typed event. Currently uses a generic serde value. Remaining: define a struct, wire into emit_turn_events.
7. **22.5-R4**: File-tool duration tracking per tool invocation. Remaining: add elapsed_ms to ToolOutcome or a sidecar timing map.
8. **22.9-R4**: Bedrock/Vertex test hardening. Integration tests need live provider credentials. Remaining: mock provider adapters for CI.
9. **adr_xref_drift path-literal check**: Extend ADR drift test to verify `crates/X` and `src/X` path literals in ADR prose reference actual directories. Ponytail note added (tests/adr_xref_drift.rs:258); needs markdown parsing + filesystem probing.

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo clippy --workspace --features pty -- -D warnings`: PASS
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS

## Known pre-existing test failures (NOT from WO 21/22)

- `compaction_use_llm_alias_backward_compat` — parser reads from top level, test expects `[session]`
- `bundled_node_sdk_tool_executes_via_host` — requires Node.js
- `adapters::m5_tests::openai_cache_mode_marks_last_two_prefix_messages` — stale vs WO 17.5
- `session::plugin_tools::*` (7), `tools::bash::*` (2) — env/binary-dependent
- `kf-budget-core::tests::adr_0004_marker_block_pins_literal_prefix_and_suffix` — stale `<<plugin3:slice:`` prefix literal

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
