# kf-code Repo State

*Current-state-only. Resolved-issue archaeology lives in `git log`.*

## Branch

**`dev`** at latest merge. WO 21 + WO 22 + WO 23 + WO 24 + WO 25 + WO 26 series merged. See commit log for details.

## WO 26 series (merged into dev, commit cb82b05)

| WO | Status | Items |
|----|--------|-------|
| 26.1 | DONE | F0: gate drift test (#[ignore]); F1: cargo-audit --deny syntax |
| 26.2 | DONE | F2: notebook_edit unwrap guard; F3: web_fetch char-boundary slice |
| 26.3 | DONE | F17: landlock feature compiles (missing `mod landlock` declaration) |
| 26.4 | DONE | F4: saturating prune; F5: defer job map removal until reaped; F6: sub-second timeouts; F7: unique run-id; F8: drop lock before await; F9: compaction div-by-zero guard; F10: dedup key includes tool fields |
| 26.5 | DONE | F11: bound broadcast channels; F12: daemon client timeouts; F13: stale worktree cleanup; F14: schedule tag case; F15: canonicalize cwd before fork; F16: orphan .snap cleanup |
| 26.6 | DONE | R1: sessions-list dirty refresh; R2: persona adapter provider routing; R3: non-Rust linting (eslint wired) |
| 26.7 | DONE (R4 re-deferred) | R1: bash streaming TurnEvent; R2: MCP sampling/createMessage (ADR-072); R3: TUI memory widget; R4: computer_use re-deferred with disclosure |
| 26.8 | DONE | AppState decomposition → 11 sub-structs |
| 26.9 | DONE (partial) | R1: top-10 slowest tests fixed/skipped; R3+R4: testdoctor parallel scan + caching |
| 26.10 | NOT STARTED | provider hardening (mocks, Plugin3 shim, landlock default, memory dedup) |

## Current state / where the session stopped (2026-08-10)

- WO 26 series merged into `dev` (commit `cb82b05`), pushed to origin/dev.
- Two follow-up commits on `dev` NOT yet pushed: `76e037a` (cargo-audit severity blocking via `.cargo/audit.toml`) and `cdb3b42` (e2e scenarios deliver prompt via stdin).
- **CI is still RED.** Last run (31408134938) failed on `audit` and `windows` jobs:
  - `audit`: fixed by `76e037a` (cargo-audit 0.22 rejects `--deny critical` — severity blocking moved to `.cargo/audit.toml` `severity_threshold`). Needs a re-run to confirm.
  - `windows`: e2e tests fail. Root cause found: scenarios passed the prompt as a positional CLI arg, but `kf-code run` has no positional field → clap exits code 2 → zero mock requests. Fixed by `cdb3b42` (pipe prompt via stdin). **BUT a second pre-existing bug remains: the stdin-piping path HANGS** — the binary never completes the turn against the mock. Root cause NOT yet fixed. This is the blocker for green CI.
- **Version bump to 0.3.7: NOT done.**
- **`main` fast-forward: NOT done** (main still at e95c347).
- **WO 27 series: NOT started.**
- **Local install at /home/henrik/own-code/kf-code: NOT done.**

### Pending / blocked
- **BLOCKED (CI red):** e2e stdin-piping hang. The binary `kf-code run --no-tui --non-interactive --max-turns 1` with a prompt piped to stdin, pointed at a wiremock Ollama `/api/chat` NDJSON mock, never exits. Suspected: adapter waits for HTTP body close, or a channel never closes, or the executor loop doesn't terminate on `done:true`. Investigate `src/session/executor/turn.rs` `stream_iteration` + `src/adapters/ollama_ndjson.rs` + `src/main/line_mode.rs`. Fix before pushing `cdb3b42`/`76e037a` and before any version bump.
- **PENDING:** push `76e037a` + `cdb3b42` once e2e hang is fixed; confirm CI green; fast-forward `main`; bump to 0.3.7; update review; start WO 27; install locally.

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
| 26.8 | DONE | AppState decomposed from flat ~66-field struct into 11 sub-structs (conversation, generation, budget, session, provider, approval, search, ui, doom, services + dirty) with accessor shims; TUI unchanged |

## Deferred items (explicitly tracked)

### Medium priority

0. **24.6-R1..R5 / 25.16**: Raise `src/session` coverage above 75%. CI coverage job added in WO 25.4-R1. Remaining: R1 fill baseline from first CI run, R2 executor loop tests (6), R3 budget slicing tests (4), R4 compaction tests (5), R5 verifier bus tests (4). Tracked in WO 25.16.
1. **21.5-R2-R3 / 25.18-R1**: Stream partial bash output to TUI via TurnEvent::BashPartialOutput. DONE (WO 26.7-R1) — `TurnEvent::BashPartialOutput` added, PTY output forwarded through event_tx, TUI tool-result card renders streaming spinner + incremental text. Non-PTY path unchanged.
2. **21.5-R4 / 25.15-R2+R3**: MCP sampling/createMessage. R1 (roots/list capability) DONE in WO 25.15. R2 (approval-gated handler + headless policy + ADR-072) DONE in WO 26.7-R2. Resolved — sampling routes through the approval bus with default-deny headless policy.
3. **21.5-R9 / 25.18-R2 / 26.7-R4**: Anthropic computer_use beta (coordinate-vision model). DEFERRED (WO 26.7-R4, re-deferred with disclosure). (a) What: opt-in beta path routed to Anthropic's hosted computer_use API (`computer` tool type + `anthropic-beta` header + coordinate-vision model), gated behind a `computer_use` Cargo feature flag defaulting OFF. (b) Why: the existing Anthropic adapter (`src/adapters/anthropic.rs`) has no hosted computer_use contract — `build_anthropic_body` serializes tools only as `{name, description, input_schema}` (no `computer` tool type), `stream` sends no `anthropic-beta` header, and the stream parser has no `computer_tool_result` content-block handling. The local headless-Chrome CDP `computer_use` tool (`src/tools/computer_use.rs`, gated by `config.security.computer_use.enabled`, default false) is a different capability and does not satisfy R4. Implementing the hosted path is an L-sized change (adapter wire format + stream parser + tool serialization + config + feature flag + coordinate-vision subsystem); the workorder estimates L (~1-2 weeks). (c) Remaining: add `computer_use` Cargo feature (default OFF); add `anthropic-beta: computer-use-2025-01-24` header to `AnthropicAdapter::stream`; add `computer` tool-type serialization in `build_anthropic_body`; add `computer_tool_result` content-block parsing in `parse_anthropic_stream`; coordinate-vision model routing (screenshot → coordinate actions); wire feature flag through config + adapter + tool registration; assert zero computer_use API calls when flag OFF. (d) Tracked in WO 26.7-R4 + this state.md pending item.
4. **22.4-R2/R3 / 25.18-R3**: TUI memory visibility + config flag. DONE (WO 26.7-R3) — memory indicator widget in status bar (`🧠N@tT`), `memory_show_in_status` config flag (default true), real-time updates via `TurnEvent::MemoryExtracted`.
5. **25.11-R2**: Daemon sessions-list refresh on dirty. DONE (WO 26.6-R1) — `sessions_dirty` flag now wired to a refresh path in the TUI event loop (mirrors `jobs_dirty`).
6. **25.12-R1**: AppState decomposition — DONE (WO 26.8). `AppState` is now 11 sub-structs (conversation, generation, budget, session, provider, approval, search, ui, doom, services + `dirty`). All call sites migrated; helper methods retained as accessor shims. TUI renders identically; session persistence format unchanged.
7. **25.17-R1-remaining**: Persona adapter Bedrock/Vertex plumbing. DONE (WO 26.6-R2) — persona path now uses `adapter_for_with_provider` forwarding `anthropic_provider` + full provider config; no hardcoded "anthropic".

### Low priority

8. **25.2-R2**: Top-10 slowest individual test fix. DONE (WO 26.9-R1) — 3 proptest tests fixed (256→32 cases, ~210s saved), 8 genuinely slow/flaky tests `#[ignore]`-gated with documented reasons. Total test time reduced ~25% (169s→127s).
9. **25.2-R4**: Split slow integration tests behind a feature flag or `tests/` directory separation. NOT DONE — still open. The e2e tests in `tests/e2e/` run in the `windows` CI job's `--workspace` and are currently broken (see "Current state" above). Remaining: gate e2e behind a feature flag or exclude from the Windows job, and fix the stdin-piping hang.
10. **25.3-R3**: testdoctor parallel directory scanning. DONE (WO 26.9-R3) — `rayon::par_iter` for file analysis.
11. **25.3-R4**: testdoctor result caching. DONE (WO 26.9-R4) — `target/testdoctor-cache.json` keyed by content hash + version; second run 65% faster.
12. **25.4-R3**: Coverage regression gate. NOT DONE — still open. Baseline placeholder exists but no enforcement. Remaining: `scripts/check-cov-regression.sh`, CI step comparing per-crate coverage against baseline - 1% tolerance.
13. **25.7-R3**: Benchmark manifest validation. NOT DONE — still open. Remaining: generate count from source in CI.
14. **25.8-R4 / 25.10-R4**: CI enforcement gate for dead crate/binary refs. NOT DONE — still open. `scripts/check-artifact-consistency.sh` covers this partially. Remaining: extend to also grep active source (src/, crates/) for `plugin3`, `kfd`, `kf-code-video` as identifiers (not historical prose), fail CI on hit.
15. **22.9-R4 / 25.18-R4**: Bedrock/Vertex test hardening. NOT DONE — still open (WO 26.10-R1, not started). Remaining: mock provider adapters for CI.
16. **Plugin3 env var backward compat**: PLUGIN3_* env vars renamed to KF_BUDGET_* in kf-budget-core (WO review-fix). NOT DONE — still open (WO 26.10-R2, not started). Remaining: add a one-release backward-compat shim that checks PLUGIN3_* and warns, or document the breaking change in CHANGELOG + migration guide.

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
