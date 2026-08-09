# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev`** at latest merge. WO 21 + WO 22 + WO 23 + WO 24 + WO 25 series merged. See commit log for details.

## Completed workorders

### WO 22 series (all done)

| WO | Status | Items |
|----|--------|-------|
| 22.1 | DONE | R1: landlock ABI rewrite |
| 22.2 | DONE | R1: default plugins (stratum + kf-budget) |
| 22.3 | DONE | R1: MCP URI validation, R2: capabilities handshake |
| 22.4 | DONE (R2/R3/R4 deferred) | R1: MAX_FACTS=3, FNV hash, rate limit |
| 22.5 | DONE (R3/R4 deferred) | R1: F2-F5 Enter handlers, R2: jobs_dirty refresh |
| 22.6 | DONE | R1-R6: token estimation, offload store, SearchState, PostHook, CorrectionResult, verifier-findings pinned in compaction tail (compaction.rs:247, loop_.rs:483) |
| 22.7 | DONE | R1-R6: all over-engineering cleanup |
| 22.8 | DONE | R1-R18: doc fixes |
| 22.9 | DONE (R4 deferred) | R7: ADR-070, R8: ADR-070 |
| 22.10 | DONE | R1: verifier Skipped → CorrectionResult |
| 22.11 | DONE | R1-R4: catch_unwind, Skipped, pub(crate) |
| 22.12 | DONE | R1: 28 ADRs updated, path literals fixed |
| 22.13 | DONE | R1-R3: multi-turn prompt fix, bg task Notify, configurable concurrency |
| 22.14 | DONE | R1-R3: JSON-schema structured output, ResponseFormat enum |

### WO 23 series (all done)

| WO | Status | Items |
|----|--------|-------|
| 23.5 | DONE | R1-R3: remember tool, system-prompt instruction, memory_auto_populate flag |
| 23.7 | DONE | R1: configurable task concurrency semaphore |
| 23.8 | DONE | R1-R3: doom-loop circuit breaker + auto-plan-mode + drift guard |
| 23.9 | DONE | R1-R3: max-continuation hard cap, TUI indicator |

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
| 21.7 | DONE | R1: landlock ABI correct (feature-gated behind `landlock`, not default-on; via 22.1), R2: ADR-054 quantified, R3: diff-review-before-apply, R4: cosign blocking, R5: sandbox refusal, R6: PathGuardTower rename, R7: signature default-on, R8: plugin sandbox note |
| 21.8 | DONE (AppState decomposition + themes deferred) | multi-turn subagents (task.rs:538-568), doom-loop circuit breaker, task concurrency |
| 21.9 | DONE | ADR drift fixes, test deadlock, fuzzing, dead code, overclaims (coverage >75% deferred — tracked separately as WO 24.6) |
| 21.10 | DONE | MCP-first overlay (hooks/verifiers) |
| 21.11 | DONE | Plugin real rebuild, draw/video yeet, SDK/budget/stratum |

### WO 24 series (6/8 done, 1 deferred)

| WO | Status | Items |
|----|--------|-------|
| 24.1 | DONE | R1: cargo audit split — block on critical/unsound, warn on rest |
| 24.2 | DONE | R1: cosign verify-blob step in release workflow |
| 24.3 | DONE | R1: --i-accept-unsandboxed gated to debug builds only |
| 24.4 | DONE | R1: remove not(budget) /4 fallback, R2: TUI BPE, R3: deprecate heuristic |
| 24.5 | DONE | R1-R3: diff-review-before-apply (done in WO 21.7-R3) |
| 24.6 | DEFERRED | session coverage 75% — needs coverage toolchain + executor loop tests |
| 24.7 | DONE | R1-R4: fuzz targets for SSE/NDJSON/Bedrock/JS/CSS |
| 24.8 | DONE | R1: 23 tracing::debug! → warn!/info!/trace!, zero debug! remaining |

### WO 25 series (16 done, 4 pending)

| WO | Status | Items |
|----|--------|-------|
| 25.0-R3 | DONE | rename misleading doom-loop test + correct CHANGELOG halt claim |
| 25.1 | DONE | R1-R3: create scripts/test-fast.sh + test-full.sh, update AGENTS.md tiered gate |
| 25.5 | DONE | R1-R5: fix stale plugin3/stratum/kfd refs in install.sh, build-all.sh, ci-local.sh, release.yml, README |
| 25.6 | DONE | R1-R3: verify 4 deadlock tests pass, remove CI quarantine comment block |
| 25.7 | DONE | R1-R2: fix benchmark link in README, correct task count to 30 |
| 25.8 | DONE (R4 deferred) | R1-R3: audit clean (zero active residue); R5: archive editors/vscode/ |
| 25.9 | DONE | R1: remove 6 dead-code items — -408 lines |
| 25.10 | DONE (R4 deferred) | R1: fix config.toml.example ghost plugins; R2: add ADR path-literal enforcement test |
| 25.11 | DONE (R2 deferred) | R1: pass real duration_ms through file-tool metric branch |
| 25.12 | DONE (R1 deferred) | R2: fix cached_tokens fork-reset; R3: add pinning test |
| 25.13 | DONE | R1: SLICED_LISTENERS append-only leak documented safe (ceiling: note); R2: SESSION_MODE process-global documented intentional; test added |
| 25.14 | DONE | R1-R4: add line field to verifier types, populate from clippy/build spans, propagate into CorrectionResult |
| 25.15 | DONE (R2+R3 deferred) | R1: advertise roots capability in MCP init handshake |
| 25.17 | DONE (R1 deferred, R2 opt-in) | persona Anthropic-direct documented; landlock opt-in documented |
| 25.19 | DONE | R1-R3: add phased multistep workflow to AGENTS.md, upgrade subagent decision tree, cross-layer grep |
| 25.2 | PENDING | test speed optimization (R1: #[ignore] broken tests; R2-R4: speed fixes) |
| 25.3 | PENDING | testdoctor optimization |
| 25.4 | PENDING | coverage baseline tooling |
| 25.16 | PENDING | session coverage 75% (dep: 25.4) |

## Deferred items (explicitly tracked)

### Medium priority

0. **24.6-R1..R5**: Raise `src/session` coverage above 75%. Requires `cargo llvm-cov` (not installed). Remaining: R1 baseline measurement, R2 executor loop tests (6 scenarios), R3 budget slicing tests (4), R4 compaction tests (5), R5 verifier bus tests (4). Tracked in WO 24.6.

1. **21.5-R2-R3**: Stream partial bash output to TUI via TurnEvent::BashPartialOutput. UX polish only — non-PTY path unchanged. Remaining: add TurnEvent variant, forward PTY output through event_tx, render streaming indicator in TUI tool-result card.
2. **21.5-R4**: MCP sampling/createMessage. R1 (roots/list capability) DONE in WO 25.15. Remaining: implement sampling handler with user approval gate + headless policy + ADR. Tracked in WO 25.15.
3. **21.5-R9**: Anthropic computer_use beta (coordinate-vision model). Local headless-Chrome tool is the real differentiator. Remaining: opt-in beta path routed to Anthropic model, gated behind feature flag.
4. **22.4-R2/R3**: TUI memory visibility + config flag. Remaining: memory indicator widget in status bar; config flag to toggle memory display.
8. **25.17-R1-remaining**: Persona adapter Bedrock/Vertex plumbing. `adapter_for` hardcodes Anthropic-direct; needs signature extension + Config ref at all call sites. Remaining: extend `adapter_for` with Bedrock/Vertex params, update persona.rs call site, add integration test. Tracked in WO 25.17.

### Low priority

5. **22.5-R4**: RESOLVED by WO 25.11-R1 — duration_ms now passes through file-tool branch.
6. **22.9-R4**: Bedrock/Vertex test hardening. Integration tests need live provider credentials. Remaining: mock provider adapters for CI.
7. **adr_xref_drift path-literal check**: DONE in WO 25.10-R2 — `adr_path_literals_reference_existing_crates` test now enforces path existence.

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

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
