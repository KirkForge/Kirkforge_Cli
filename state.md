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

### WO 25 series (18 done, 2 pending)

| WO | Status | Items |
|----|--------|-------|
| 25.0-R3 | DONE | rename misleading doom-loop test + correct CHANGELOG halt claim |
| 25.1 | DONE | R1-R3: create scripts/test-fast.sh + test-full.sh, update AGENTS.md tiered gate |
| 25.2 | DONE (R2+R4 deferred) | R1: #[ignore] 29 known-broken tests; R3: tokio flavor audit (no single_thread found) |
| 25.3 | DONE (R3+R4 deferred) | R1+R2: testdoctor 2.9s→1.8s via single-pass scan merge |
| 25.4 | DONE (R3 deferred) | R1+R2: coverage CI job + baseline placeholder |
| 25.5 | DONE | R1-R5: fix stale plugin3/stratum/kfd refs in 5 scripts |
| 25.6 | DONE | R1-R3: lift deadlock CI quarantine |
| 25.7 | DONE | R1-R2: benchmark link + task count fix |
| 25.8 | DONE (R4 deferred) | R1-R3: audit clean; R5: archive editors/vscode/ |
| 25.9 | DONE | remove 6 dead-code items — -408 lines |
| 25.10 | DONE (R4 deferred) | fix config.toml.example + ADR path-literal enforcement |
| 25.11 | DONE (R2 deferred) | fix file-tool duration_ms:0 bug |
| 25.12 | DONE (R1 deferred) | fix cached_tokens fork-reset + pinning test |
| 25.13 | DONE | document SLICED_LISTENERS safe + SESSION_MODE global |
| 25.14 | DONE | add line field to verifier types + propagate |
| 25.15 | DONE (R2+R3 deferred) | advertise roots in MCP init handshake |
| 25.16 | PENDING | session coverage 75% (dep: 25.4) |
| 25.17 | DONE (R1 deferred) | persona Anthropic-direct documented; landlock opt-in |
| 25.18 | DEFERRED | carry-forward: bash streaming, computer_use, memory widget, Bedrock/Vertex mocks |
| 25.19 | DONE | phased multistep workflow in AGENTS.md |

### WO 26 series (in progress)

| WO | Status | Items |
|----|--------|-------|
| 26.7 | R1+R2 DONE (R3,R4 pending) | R1: bash streaming TurnEvent; R2: MCP sampling/createMessage via approval bus + ADR-072 |

## Deferred items (explicitly tracked)

### Medium priority

0. **24.6-R1..R5 / 25.16**: Raise `src/session` coverage above 75%. CI coverage job added in WO 25.4-R1. Remaining: R1 fill baseline from first CI run, R2 executor loop tests (6), R3 budget slicing tests (4), R4 compaction tests (5), R5 verifier bus tests (4). Tracked in WO 25.16.
1. **21.5-R2-R3 / 25.18-R1**: Stream partial bash output to TUI via TurnEvent::BashPartialOutput. UX polish only — non-PTY path unchanged. Remaining: add TurnEvent variant, forward PTY output through event_tx, render streaming indicator in TUI tool-result card.
2. **21.5-R4 / 25.15-R2+R3**: MCP sampling/createMessage. R1 (roots/list capability) DONE in WO 25.15. R2 (approval-gated handler + headless policy + ADR-072) DONE in WO 26.7-R2. Resolved — sampling routes through the approval bus with default-deny headless policy.
3. **21.5-R9 / 25.18-R2 / 26.7-R4**: Anthropic computer_use beta (coordinate-vision model). DEFERRED (WO 26.7-R4, re-deferred with disclosure). (a) What: opt-in beta path routed to Anthropic's hosted computer_use API (`computer` tool type + `anthropic-beta` header + coordinate-vision model), gated behind a `computer_use` Cargo feature flag defaulting OFF. (b) Why: the existing Anthropic adapter (`src/adapters/anthropic.rs`) has no hosted computer_use contract — `build_anthropic_body` serializes tools only as `{name, description, input_schema}` (no `computer` tool type), `stream` sends no `anthropic-beta` header, and the stream parser has no `computer_tool_result` content-block handling. The local headless-Chrome CDP `computer_use` tool (`src/tools/computer_use.rs`, gated by `config.security.computer_use.enabled`, default false) is a different capability and does not satisfy R4. Implementing the hosted path is an L-sized change (adapter wire format + stream parser + tool serialization + config + feature flag + coordinate-vision subsystem); the workorder estimates L (~1-2 weeks). (c) Remaining: add `computer_use` Cargo feature (default OFF); add `anthropic-beta: computer-use-2025-01-24` header to `AnthropicAdapter::stream`; add `computer` tool-type serialization in `build_anthropic_body`; add `computer_tool_result` content-block parsing in `parse_anthropic_stream`; coordinate-vision model routing (screenshot → coordinate actions); wire feature flag through config + adapter + tool registration; assert zero computer_use API calls when flag OFF. (d) Tracked in WO 26.7-R4 + this state.md pending item.
4. **22.4-R2/R3 / 25.18-R3**: TUI memory visibility + config flag. Remaining: memory indicator widget in status bar; config flag to toggle memory display.
5. **25.11-R2**: Daemon sessions-list refresh on dirty. `sessions_dirty` flag set but no refresh path — daemon clients show stale session state. Remaining: add sessions-list refresh equivalent to jobs_dirty refresh, triggered by sessions_dirty.
6. **25.12-R1**: AppState decomposition — 68-field struct into ≤12 sub-struct fields. R2 (cached_tokens fork-reset bug) DONE. Remaining: group fields by concern (ConversationState, BudgetState, etc.), add accessors/Deref shims, preserve TUI public API. High-touch refactor.
7. **25.17-R1-remaining**: Persona adapter Bedrock/Vertex plumbing. `adapter_for` hardcodes Anthropic-direct; needs signature extension + Config ref at all call sites. Remaining: extend `adapter_for` with Bedrock/Vertex params, update persona.rs call site, add integration test. Tracked in WO 25.17.

### Low priority

8. **25.2-R2**: Top-10 slowest individual test fix. Per-test timing data needed. Remaining: identify top-10 slowest, replace std::thread::sleep with tokio::time::advance, reduce fixture sizes, move genuinely slow tests behind feature flag.
9. **25.2-R4**: Split slow integration tests behind a feature flag or `tests/` directory separation. Fast gate should not run integration tests. Remaining: identify integration tests, gate behind `#[cfg(feature = "integration-tests")]` or move to dedicated `tests/`.
10. **25.3-R3**: testdoctor parallel directory scanning. Current runtime 1.8s. Remaining: use `std::thread::scope` for parallel dir scan (no new deps).
11. **25.3-R4**: testdoctor result caching. File-content-hash cache for unchanged files, target: second run <1s. Remaining: hash (size + mtime), store in `target/.testdoctor-cache`, invalidate on `cargo clean`.
12. **25.4-R3**: Coverage regression gate. Baseline placeholder exists but no enforcement. Remaining: `scripts/check-cov-regression.sh`, CI step comparing per-crate coverage against baseline - 1% tolerance.
13. **25.7-R3**: Benchmark manifest validation. CI step verifying bench/tasks/*.toml count matches README. Remaining: generate count from source in CI.
14. **25.8-R4 / 25.10-R4**: CI enforcement gate for dead crate/binary refs. `scripts/check-artifact-consistency.sh` covers this partially. Remaining: extend to also grep active source (src/, crates/) for `plugin3`, `kfd`, `kf-code-video` as identifiers (not historical prose), fail CI on hit.
15. **22.9-R4 / 25.18-R4**: Bedrock/Vertex test hardening. Integration tests need live provider credentials. Remaining: mock provider adapters for CI.
16. **Plugin3 env var backward compat**: PLUGIN3_* env vars renamed to KF_BUDGET_* in kf-budget-core (WO review-fix). Users who set the old env vars will break. Remaining: add a one-release backward-compat shim that checks PLUGIN3_* and warns, or document the breaking change in CHANGELOG + migration guide.

## Gate status

- `cargo check --workspace`: PASS
- `cargo fmt --check`: PASS
- `cargo clippy --workspace -- -D warnings`: PASS
- `cargo clippy --workspace --features pty -- -D warnings`: PASS
- `cargo test -p kf-budget-core --test adr_xref_drift`: PASS

## Known pre-existing test failures (NOT from WO 21/22)

All known-broken tests are now `#[ignore]`-labeled (WO 25.2-R1, 29 tests). They remain in the source as documentation of expected behavior. Run with `--ignored` to execute them.

## Rust toolchain

Rust 1.88.0 at `~/.cargo/bin/`. Run `export PATH="$HOME/.cargo/bin:$PATH"` before cargo commands.
