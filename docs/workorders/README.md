# Workorders — Planned and In-Progress Work

This directory contains numbered workorders that define scoped tasks for
KirkForge-Cli. Each workorder lists the problem, root cause, files to touch,
approach, gate, and done condition.

## Active series

### Series 6 — Benchmarks and Continuous Evaluation

| # | Workorder | Status | Depends on |
|---|---|---|---|
| 6.1 | [Bench harness realism](6.1-bench-realism.md) | Done | — |
| 6.2 | [Bench delta comparison](6.2-bench-delta-comparison.md) | Done | — |
| 6.3 | [Bench CI wiring](6.3-bench-ci-wiring.md) | Done | 6.2 |
| 6.4 | [Bench list + verify-only](6.4-bench-list-verify-only.md) | Done | — |
| 6.5 | [Continuous eval ADR](6.5-bench-eval-adr.md) | Done | 6.1-6.4 |

### Series 7 — Plugin Integration

| # | Workorder | Status | Depends on |
|---|---|---|---|
| 6.6 | [Fold Stratum into core](6.6-fold-stratum.md) | Done | — |
| 6.7 | [Fold Plugin3 into core](6.7-fold-plugin3.md) | Done (slicing deferred) | 6.6 |
| 6.8 | [Fold Draw into core](6.8-fold-draw.md) | Done | 6.6 |
| 6.9 | [Fold Video into core](6.9-fold-video.md) | Done | 6.6 |
| 7.0 | [Plugin system consolidation](7.0-plugin-consolidation.md) | Done | 6.6-6.9 |

### Series 7.1-7.9 — Hardening and Capability Gaps

Workorders 7.1-7.9 address findings from the honest codebase assessment
(B+ overall). They close the gap between the architecture vision and reality.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 7.1 | [Budget slicing action](7.1-budget-slicing-action.md) | Done | High | 6.7 |
| 7.2 | [Fold-in unit tests](7.2-fold-in-unit-tests.md) | Done | High | 6.6-6.9 |
| 7.3 | [Fix bench CI gate theater](7.3-bench-gate-theater.md) | Done | Medium | 6.3 |
| 7.4 | [Remove legacy spec-drift tests](7.4-remove-legacy-spec-drift.md) | Done | Low | — |
| 7.5 | [Budget and Stratum config fields](7.5-budget-stratum-config.md) | Done | Medium | 7.1 |
| 7.6 | [Windows test parity](7.6-windows-test-parity.md) | Done | Medium | — |
| 7.7 | [KVB verifier bus bridge](7.7-kvb-verifier-bus-bridge.md) | Done | Medium | 7.0 |
| 7.8 | [Bench task expansion](7.8-bench-task-expansion.md) | Done | Medium | — |
| 7.9 | [Context index Phase 7: embeddings + graph-walk](7.9-context-index-phase7.md) | Done | High | — |

### Priority rationale

- **7.1 (High)**: The budget guard is a passive monitor, not an active guard.
  This is the core value prop of Plugin3 and the biggest gap between the
  architecture vision and reality.
- **7.2 (High)**: Zero tests in the fold-in modules. The coverage gate was
  lowered to accommodate this. Tests must be added before the gate can be
  raised back.
- **7.9 (High)**: Substring-match retrieval is the weakest part of the context
  system. Graph-walk retrieval would be a significant capability upgrade.
- **7.3, 7.5, 7.6, 7.7, 7.8 (Medium)**: Important hardening but not
  capability-blocking.
- **7.4 (Low)**: Dead weight cleanup. No functional impact.

### Series 8.0-8.9 — Production Hardening

Workorders 8.0-8.9 address findings from the second production-readiness
assessment (A- overall). They target coverage, retrieval quality, TUI parity,
plugin validation, and language-specific edge cases.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 8.0 | [Raise coverage threshold](8.0-raise-coverage-threshold.md) | Done | Medium | 7.2 |
| 8.1 | [Multi-model benchmark leaderboard](8.1-multi-model-leaderboard.md) | Done | High | — |
| 8.2 | [TUI parity: doom loop + session nav](8.2-tui-parity-doom-loop.md) | Done | Medium | — |
| 8.3 | [Bench task realism: self-contained + plugin tools](8.3-bench-task-realism.md) | Done (partial) | Medium | 7.8 |
| 8.4 | [Embedding quality: evaluate and tune TF-IDF](8.4-embedding-quality.md) | Done | High | 7.9 |
| 8.5 | [ADR index table unification](8.5-adr-index-unification.md) | Done | Low | — |
| 8.6 | [Stratum + budget guard coordination](8.6-stratum-budget-coordination.md) | Done | Medium | 7.1, 7.5 |
| 8.7 | [Error recovery: structured hints](8.7-error-recovery-hints.md) | Done | Medium | — |
| 8.8 | [Plugin manifest schema validation](8.8-plugin-manifest-validation.md) | Done | Medium | — |
| 8.9 | [Context index: TS/Python/Go edge cases](8.9-context-index-edge-cases.md) | Done | Medium | — |

### Priority rationale

- **8.1 (High)**: Multi-model comparison is the headline bench feature. The
  single-model harness limits the value of the benchmark system.
- **8.4 (High)**: The TF-IDF embeddings work but quality is unmeasured. Poor
  retrieval quality undermines the context index's value proposition.
- **8.0, 8.2, 8.3, 8.6, 8.7, 8.8, 8.9 (Medium)**: Important hardening but not
  capability-blocking.
- **8.5 (Low)**: Documentation cleanup. No functional impact.

### Series 9.0-9.9 — Gap Closure and Hardening

Workorders 9.0-9.9 address the remaining gaps surfaced by the audit after
the 8-series shipped. They target broken bench specs, the workflow tool
wrapper, PR-time bench deltas, version reconciliation, interactive replay,
prompt-cache stem reuse, verifier bus unification, VFS minification,
sandbox hardening, and representative bench tasks.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 9.0 | [Fix broken bench verify specs](9.0-fix-broken-bench-verify-specs.md) | Done | High | 8.3 |
| 9.1 | [Workflow tool wrapper](9.1-workflow-tool-wrapper.md) | Done | Medium | — |
| 9.2 | [Bench PR delta comment](9.2-bench-pr-delta-comment.md) | Done | Medium | 6.2, 8.1 |
| 9.3 | [Version reconciliation and v0.3.6 release](9.3-version-reconciliation.md) | Done | High | 8.0-8.9 |
| 9.4 | [Replay interactive stepper](9.4-replay-interactive-stepper.md) | Done | Medium | — |
| 9.5 | [Prompt cache stem reuse](9.5-prompt-cache-stem-reuse.md) | Done | High | — |
| 9.6 | [Verifier bus code unification](9.6-verifier-bus-unification.md) | Done | Medium | ADR-028 |
| 9.7 | [Tree-sitter VFS minification](9.7-vfs-minification.md) | Done | Medium | — |
| 9.8 | [Seccomp/rlimit sandbox hardening](9.8-seccomp-rlimit-hardening.md) | Done | Low | — |
| 9.9 | [Bench task expansion: real-world shapes](9.9-bench-task-expansion-2.md) | Done | High | 9.0 |

### Priority rationale

- **9.0 (High)**: 11/24 bench tasks have broken verify specs. The bench
  pass rate is unmeasurable until these are fixed. Blocks 9.9.
- **9.3 (High)**: state.md claims v0.3.6 but Cargo.toml is 0.3.0 and no tag
  exists. The 8-series work is unreleased. Pure process failure.
- **9.5 (High)**: Prompt-cache stem reuse is Vix's biggest token-efficiency
  differentiator. The cache markers ship but the reuse logic does not.
- **9.9 (High)**: Single-file tasks don't measure agent skill. The bench
  harness needs representative multi-file/multi-turn tasks to turn "agent
  capability B+" into a measured grade.
- **9.1, 9.2, 9.4, 9.6, 9.7 (Medium)**: Real capability/closure work but not
  blocking the measurement or release.
- **9.8 (Low)**: Docker already provides process isolation; seccomp/rlimit
  is a lighter-weight path for users who don't want Docker overhead.

### Series 10.0-10.9 — Release Fix, Wiring, and Depth

Workorders 10.0-10.9 address findings from Pass 13 of the rolling review
(2026-07-27). They target the P0 Windows CI red that blocks the v0.3.6
release, the WO 9.5 cache-stem-tracker wiring gap, doc-sync and branch-
hygiene cleanup, and three depth gaps (HTTP MCP session ids, TS
orchestrator verifier-bus bridge, bench leaderboard publish +
regression gate).

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 10.0 | [Fix Windows env_guard race](10.0-windows-env-guard-race.md) | Done (`4cbcfc3`) | High | — |
| 10.1 | [Re-ship v0.3.6 release after Windows green](10.1-reship-v0.3.6-release.md) | Done (`v0.3.6` tag → `4cbcfc3`; 6 binaries + sigs published, release run `30239782875` success) | High | 10.0 |
| 10.2 | [Wire WO 9.5 CacheStemTracker into executor + adapter](10.2-wire-cache-stem-tracker.md) | Done (`3f6e19d`; metric event wired, adapter short-circuit skipped — Anthropic API requires full content) | High | 9.5 |
| 10.3 | [state.md main-SHA + ADR-count doc sync](10.3-state-md-doc-sync.md) | Done (`378d163`) | Low | — |
| 10.4 | [Clean up leftover wo/* worktree branches](10.4-cleanup-wo-branches.md) | Done (`378d163`; 8 local + 8 remote `wo/8.*` deleted, 7 stale worktrees removed) | Low | — |
| 10.5 | [replay.rs sync_all batching](10.5-replay-syncall-batching.md) | Done (`30b55ee`; batch every N turns + Drop flush, 2 new tests) | Low | — |
| 10.6 | [minify/lang.rs style cleanup + dead-code removal](10.6-minify-lang-cleanup.md) | Done (`38b9c17`; `#![allow(dead_code)]` removed, dead `minify_rust` deleted, comment style fixed) | Low | — |
| 10.7 | [HTTP MCP: session-id tracking + resumable streams](10.7-http-mcp-session-id.md) | Done (`0b817f9`; `Mcp-Session-Id` + `Last-Event-ID` + reconnect backoff, 11 tests, ADR-055) | Medium | — |
| 10.8 | [Verifier bus: TS orchestrator → Rust VerifierBus NDJSON bridge](10.8-verifier-bus-ts-bridge.md) | Done (`621d777`; `TsOrchestratorBridgeVerifier` + `bridge-emitter.ts`, integration test, ADR-028 ponytail updated) | Medium | 9.6 |
| 10.9 | [Bench: leaderboard publish + regression gate + multi-model PR delta](10.9-bench-leaderboard-regression-gate.md) | Done (`bc41e8c`; `--fail-on-regression` flag, `bench-leaderboard` scheduled job with `[skip ci]`) | Medium | 9.2, 8.1 |

### Priority rationale

- **10.0 (High)**: `main` is CI-red on the v0.3.6 release commit. The
  Windows `env_guard_restores_prior_value_some_branch` test races on
  the assertion-after-Drop window. A red `main` is a broken product;
  this blocks 10.1 (the release cannot ship until CI is green).
- **10.1 (High)**: v0.3.6 is tagged but the Release workflow failed
  (its `Verify main CI is green` gate failed because the Windows job
  failed). No release artifacts were published. The WO 9.3 done
  condition was not checked before marking Done.
- **10.2 (High)**: WO 9.5 shipped the `CacheStemTracker` struct + 6
  unit tests + a metric variant, but the tracker is never called from
  the executor or adapter. The WO 9.5 done condition "adapter skips
  re-serializing cache-stable stem content" was not met. This WO wires
  the tracker in and corrects the 9.5 overclaim.
- **10.7, 10.8, 10.9 (Medium)**: Three depth gaps. HTTP MCP session ids
  (documented gap at `http.rs:395`); TS orchestrator → Rust
  VerifierBus NDJSON bridge (the second half of the verifier-bus
  unification WO 9.6 documented but did not implement); bench
  leaderboard publish + regression gate (the P1-long-2 follow-up).
- **10.3, 10.4, 10.5, 10.6 (Low)**: Doc-sync and hygiene cleanup. Can be
  batched into a single "doc + perf + cleanup hygiene" commit.

### Series 11.0-11.9 — Plugin System Hardening and Depth

Workorders 11.0-11.9 address findings from a focused review of the
plugin system (2026-07-27, post-Series-10). The plugin system shipped
two-path dispatch (ADR-050), trust tiers, minisign signature
verification, manifest validation (WO 8.8), folded plugins, and the
verifier-bus bridge (ADR-028/054). The remaining gaps are: a
headless management surface (CLI), in-process signature verification,
plugin dependency declaration, downgrade visibility, hot-reload,
per-plugin resource limits, hook audit logging, verifier-result
surfacing, authoring scaffolding, and an end-to-end integration test.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 11.0 | [Plugin CLI subcommand (`kirkforge plugin`)](11.0-plugin-cli-subcommand.md) | Done | High | — |
| 11.1 | [Plugin signature verification in Rust (no minisign shell-out)](11.1-plugin-signature-rust.md) | Done | High | — |
| 11.2 | [Plugin manifest `depends_on` (dependency graph)](11.2-plugin-depends-on.md) | Done | Medium | — |
| 11.3 | [Surface trust-tier downgrades in `/plugins list`](11.3-surface-trust-downgrades.md) | Done | Low | — |
| 11.4 | [Plugin hot-reload via file watcher](11.4-plugin-hot-reload.md) | Done | Medium | — |
| 11.5 | [Per-plugin resource limits (extend SandboxConfig)](11.5-per-plugin-resource-limits.md) | Done | Medium | 9.8 |
| 11.6 | [Plugin hook fail-open audit log](11.6-plugin-hook-audit-log.md) | Done | High | — |
| 11.7 | [Plugin verifier results in `/verify` panel + cost report](11.7-plugin-verifier-ui.md) | Done | Medium | 9.6 |
| 11.8 | [Plugin init scaffolding command (`kirkforge plugin init`)](11.8-plugin-init-scaffolding.md) | Done | Low | 11.0 |
| 11.9 | [Plugin system end-to-end integration test suite](11.9-plugin-system-e2e-test.md) | Done | High | 11.6 |

### Priority rationale

- **11.0 (High)**: Plugin management is TUI-only. A headless user
  (CI, cron, NDJSON mode, wrapper script) cannot enable/list/validate
  plugins. The `/plugins` TUI commands work; the CLI surface is missing.
- **11.1 (High)**: Signature verification shells out to `minisign` —
  a hard error if the binary isn't installed. In-process verification
  (pure-Rust ed25519) removes the external dependency and works
  everywhere `kirkforge` runs.
- **11.6 (High)**: Plugin hooks fail-open silently — denials and
  crashes go to `tracing::warn!`, not the audit log. A security-
  observability gap: a hook that denies a tool call or fails open on
  a dangerous one is not in the tamper-evident audit trail.
- **11.9 (High)**: The plugin system has unit tests for each component
  but no end-to-end test exercising the full lifecycle (skill + tool +
  hook + verifier + trust + sandbox + env-curation + audit). A
  composition regression would not be caught.
- **11.2, 11.4, 11.5, 11.7 (Medium)**: Real plugin-system depth
  (dependency graph, hot-reload, per-plugin rlimits, verifier UI).
  Not blocking but close the ecosystem gaps.
- **11.3, 11.8 (Low)**: Visibility and ergonomics. Trust downgrades
  are silent; plugin authoring is hand-copied. Cleanup, not blockers.

### Series 12.0-12.9 — Test Infrastructure, Coverage Gate, and Diagnostic Tool

Workorders 12.0-12.9 address the final CI gate (tarpaulin flake), the
coverage-threshold raise from 62/50/61 to 75/75/75, and the promotion
of the `kirkforge-testdoctor` prototype to a workspace member with
per-test timings, flaky detection, coverage-gap analysis, and smart
auto-suggest. The 12-series is the "test infrastructure + coverage"
series.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 12.0 | [Fix final CI gate (tarpaulin flake)](12.0-fix-tarpaulin-flake.md) | Done (`095699b`; `session_index::save()` re-creates parent dir before rename) | High | — |
| 12.1 | [Raise `src/session` coverage threshold 62% → 68%](12.1-raise-session-coverage-68.md) | Done (intermediate threshold landed) | High | 12.0 |
| 12.2 | [Raise `src/tools` coverage threshold 50% → 65%](12.2-raise-tools-coverage-65.md) | Done (intermediate threshold landed) | High | 12.0 |
| 12.3 | [Raise `src/adapters` coverage threshold 61% → 70%](12.3-raise-adapters-coverage-70.md) | Done (intermediate threshold landed) | High | 12.0 |
| 12.4 | [Fold `kirkforge-testdoctor` into workspace + `kirkforge doctor` CLI](12.4-fold-testdoctor-into-workspace.md) | Done (`6ee64a4`) | High | — |
| 12.5 | [Advanced testdoctor: per-test timings + flaky-test detection](12.5-testdoctor-per-test-flaky.md) | Done (`4e7ed78`) | Medium | 12.4 |
| 12.6 | [Testdoctor: smart auto-suggest (real heuristics + fix application)](12.6-testdoctor-smart-suggest.md) | Done (`c86c34d`) | Medium | 12.5 |
| 12.7 | [Testdoctor: coverage-gap report (uncovered files + suggested targets)](12.7-testdoctor-coverage-gaps.md) | Done (`095699b`; `kirkforge doctor gaps`) | High | 12.4 |
| 12.8 | [Final coverage push to 75% on all three target directories](12.8-final-coverage-push-75.md) | Done (`3fb20b9`; 144 new tests; src/session 68.6%, src/tools 76.5%, src/adapters 84.1%) | High | 12.0, 12.1-12.3, 12.7 |
| 12.9 | [Enforce 75% coverage thresholds in CI](12.9-enforce-75-percent-thresholds.md) | Done (`6bb4cc3`; ADR-065; src/session floor 68.5, src/tools 76.0, src/adapters 75.0) | High | 12.8 |

### Priority rationale

- **12.0 (High)**: The `coverage` CI job is the last non-green gate.
  The tarpaulin flake on `test_build_fork_tree_orphan_fork_is_a_root`
  blocks all coverage-threshold raises (you can't trust the gate while
  it's flaky). P0 for the 12-series.
- **12.1, 12.2, 12.3 (High)**: Intermediate threshold raises
  (session 62→68, tools 50→65, adapters 61→70). Each is gated on 12.0
  (the flake must be fixed first). These establish the baseline for
  the 75% push (12.8).
- **12.4 (High)**: The testdoctor is a standalone prototype, excluded
  from the workspace. The 12.5-12.7 advanced features need it to be a
  workspace member (so the tests run under the CI gate). The
  `kirkforge doctor` CLI integration makes it discoverable.
- **12.7 (High)**: The coverage-gap report is the tool that makes
  12.1-12.3 + 12.8 efficient. Without it, finding uncovered files is
  manual XML parsing. With it, `kirkforge doctor gaps` tells you exactly
  where to add tests.
- **12.8 (High)**: The final coverage push — add tests until all three
  directories are ≥75%. The biggest WO by test-writing effort.
- **12.9 (High)**: Raise the CI thresholds to 75% — the final step that
  makes the gate match the target. Gated on 12.8 (actual coverage must
  be ≥75% before the threshold is raised).
- **12.5, 12.6 (Medium)**: Advanced testdoctor features (per-test
  timings, flaky detection, smart suggest). Developer ergonomics, not
  blockers for the coverage push.

### Series 13 — (reserved, not yet defined)

Series 13 is intentionally unused. Series 14 was prioritized over
13 because the Pass-14 review (`REVIEW-KirkForge-Cli.md`) named UX
polish + stability as the A− holdbacks, not a capability gap. 13 is
reserved for a future capability series if needed.

### Series 14.0-14.9 — Polish, Quality, and Stability

Workorders 14.0-14.9 are the **polish / quality / stability** series.
They address the Pass-14 review's UX findings (onboarding C+, error
handling C+, discoverability B, status-bar rough edges) and the
`123.md` KIRK-BENCH spec adoption, on top of the architectural depth
the 10/11-series shipped. Each WO raises the polish level on top of
a specific focus. Series 12 (coverage/test-infra) shipped; the
14-series assumes a green CI base (WO 14.0 fixes the one
currently-red gate) and layers polish on it. WO 14.5 and 14.6 are
in progress in separate worktrees at the time of this WO.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 14.0 | [Bench Baseline Ollama-pull retry (fix the real red CI gate)](14.0-bench-baseline-ollama-retry.md) | Done (`bbc84ec`; 3-attempt retry + `/api/tags` health check) | High | — |
| 14.1 | [First-run onboarding banner](14.1-first-run-onboarding-banner.md) | Done (`455ca2e`) | High | — |
| 14.2 | [Grouped help + fill empty usage strings](14.2-grouped-help-usage-strings.md) | Done (`c25bd79`; six fixed-order groups via `GROUPS` const) | Medium | — |
| 14.3 | [Actionable errors + typed error classification](14.3-actionable-errors-typed-classification.md) | Done (`5bb731e`; `KirkForgeError::hint()` + `From<anyhow::Error>` downcast) | High | — |
| 14.4 | [Status bar graceful degradation on narrow terminals](14.4-status-bar-graceful-degradation.md) | Done (`2f26cda`) | Medium | — |
| 14.5 | [`/permissions` command (list + revoke `[A]lways` rules)](14.5-permissions-revoke-command.md) | Done (`f8e9f4c`; pure ops layer + 6 tests) | Medium | 14.2 (soft) |
| 14.6 | [Slash-command + `@`-mention autocomplete](14.6-slash-mention-autocomplete.md) | In Progress (worktree) | High | 14.2, 14.5 (soft) |
| 14.7 | [Publish KIRK-BENCH spec + signature Token Budget Challenge](14.7-kirk-bench-spec-publish.md) | Done (`327510c`; ADR-066; `token_budget_challenge.toml`) | High | 14.0 (soft) |
| 14.8 | [Dead-code + `#[allow(...)]` audit (internal stability)](14.8-dead-code-clippy-allow-audit.md) | Done (`120db95`; 20 dead items removed, 740-line `dispatch_tool_call` deleted) | Medium | — |
| 14.9 | [Doc-sync reconcile ADR count + stale claims](14.9-doc-sync-reconcile-adr-count.md) | Done (`9c6bf5a`) | Medium | 14.0 (soft) |

### Priority rationale

- **14.0 (High)**: The scheduled `Bench Baseline` CI job has failed
  3 consecutive days (Ollama-pull registry redirect flake). The
  Pass-14 review's "main CI-red on Windows" claim is stale — main is
  green; the *actual* red is the scheduled bench job. A red badge is
  a broken product; this is the P0 of the 14-series.
- **14.1, 14.3, 14.6 (High)**: The three UX surfaces the review
  grades C+ / C+ / B−: onboarding (silent first run), error handling
  (opaque messages, string-based classification), and discoverability
  (no autocomplete for 24 slash commands or `@`-mention syntax). Each
  is small (10-300 lines) and high-payoff.
- **14.7 (High)**: The `123.md` KIRK-BENCH spec + the signature
  Token Budget Challenge is the public differentiator the review
  names as KirkForge's "A" axis (bench/measurement). Publishing the
  spec + one signature benchmark showcases the tree-sitter + Stratum
  + budget architecture no competitor has.
- **14.2, 14.4, 14.5, 14.8, 14.9 (Medium)**: Polish and stability.
  Grouped help, status-bar degradation, `/permissions` revoke,
  dead-code audit, and doc-sync reconciliation. Each closes a
  named review finding or AGENTS.md contract violation; none is
  blocking, but together they raise the floor from "depth hidden
  behind a rough surface" to "depth that's reachable."

### Series 15.0.0.1 — Cross-Review Bucketlist (5-Reviewer Pass)

Five independent reviews of the `dev` branch (DeepSeek V4 Pro, MiniMax
M3, GLM 5.2, and two webchat instances) all found real dead code, bugs,
errors, security gaps, and doc-drift. WO 15.0.0.1 is the consolidated
bucketlist — 86 deduplicated findings across all 5 reviews, verified
against HEAD `8926fe2`, graded by severity (Tier 1-4), and prioritized.
It is the single-source backlog for Series 15.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 15.0.0.1 | [Cross-review bucketlist (5-reviewer pass)](15.0.0.1-cross-review-bucketlist.md) | Planned | High | — |
| 15.0.0.2 | [Series 15 execution plan (workorder map)](15.0.0.2-series-15-execution-plan.md) | Planned | High | 15.0.0.1 |
| 15.26 | [Remaining bucketlist items (Series 15 closeout)](15.26-remaining-bucketlist-closeout.md) | Planned | High | 15.1-15.15 |

### Priority rationale

- **Tier 1 (7 items)**: Honesty / Safety violations — CI gate theater
  (`|| true`-style gates that don't gate), plugin `validate()` skipped
  on the primary load path, `computer_use` SSRF via browser,
  `web_fetch` DNS rebinding, Docker bind-mount unsanitized, false
  deferral in state.md. These are direct AGENTS.md §4/§6/§7 violations
  and the highest-priority fixes because they make "CI green" mean
  less than it claims.
- **Tier 2 (24 items)**: Real correctness bugs — Bedrock OOM + dropped
  events, Vertex empty token, task leak on cancel, double-record
  AccessDenied, security scanner false positives on comments, cache
  OOM, etc. Fix soon.
- **Tier 3 (31 items)**: Architecture / Code quality — config field
  drift, 3,760-line test file split, coverage gate gaps, monolithic
  functions, AppState God object, duplicated code, dead code, provider
  abstraction leak. Action this quarter.
- **Tier 4 (24 items)**: Doc drift / Stale counts / Polish — ADR-066
  count, KIRK-BENCH arithmetic, test counts, leaderboard stub, crate
  audit, more metrics, more semantic benchmarks. Polish.

### Series 17 — vix-Parity

A seven-way parallel comparison of `kf-code` (Rust) against `vix-main` (Go),
the reference agent. Each dimension (token efficiency, daemon arch,
auth/providers, whiteboard/workflow/jobs, testing, update/telemetry/packaging,
TUI UX) was audited by reading the actual code in both repos. The full gap
inventory, with deferred-to-Series-18 items, is [WO 17.0](17.0-vix-parity-gap-inventory.md).

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 17.0 | [vix-parity gap inventory](17.0-vix-parity-gap-inventory.md) | Done | — | — |
| 17.1 | [Anthropic auth + per-provider env resolution](17.1-anthropic-auth-per-provider-env.md) | Done | P0 | — |
| 17.2 | [Daemon instance control channel + event broadcast](17.2-daemon-instance-channel-broadcast.md) | Done | High | — |
| 17.3 | [Daemon hardening: socket guard, auth, version, ownership, quit](17.3-daemon-hardening.md) | Done | High | 17.2 |
| 17.4 | [AST minification + surgical edit position map + revalidation](17.4-ast-minify-surgical-edit.md) | Done | High | — |
| 17.5 | [Stem-agents: shared cached context + top-N files + cache breakpoints](17.5-stem-agents-cache-context.md) | Done | High | — |
| 17.6 | [Workflow engine parity: bash/tool/if/fan_out/fork_from/on_error/budget](17.6-workflow-engine-parity.md) | Done | Medium | — |
| 17.7 | [Jobs↔workflow integration + alert persistence + per-job policy](17.7-jobs-workflow-alerts-policy.md) | Done | Medium | 17.6 |
| 17.8 | [E2E test harness: binary driver + mock provider + PTY + regression catalog](17.8-e2e-test-harness.md) | Done | High | — |
| 17.9 | [TUI parity: top tab bar, threads overview, interactive tabs, slash/file popups, welcome](17.9-tui-parity-pass.md) | Done | Medium | 17.2 |

### Priority rationale (Series 17)

- **17.1 (P0)**: The plain Anthropic adapter sends no auth header and reads
  no `ANTHROPIC_API_KEY` — it is non-functional against `api.anthropic.com`
  today. Defect fix, not parity; ships first.
- **17.2 / 17.3 (High)**: vix's daemon owns a per-instance control channel
  with broadcast fanout, auth, version gate, and exclusive session ownership;
  KirkForge's is an unauthenticated, blind-`remove_file` metadata cache.
- **17.4 / 17.5 (High)**: vix's headline 20–50% token reduction — AST
  minification with a surgical edit position map (17.4) and enforced
  cross-phase cache-stem reuse (17.5).
- **17.8 (High)**: vix has a real binary-driving e2e harness with a mock
  provider and tmux TUI driver; KirkForge tests the executor in-process only.
  The safety net for the rest of the series.
- **17.6 / 17.7 / 17.9 (Medium)**: Programmable workflows, self-evolving
  jobs, and TUI basics — real parity wins, not blocking.

Deferred to Series 18: OS keychain + `kf-code auth` OAuth/PKCE/device-code
CLI, OpenAI Codex / OpenRouter / Bedrock-chain adapters, data-driven
provider catalog, whiteboard mode (Mermaid + voice), self-update, crash
reporting, remote analytics, headless stream-json polish, Homebrew/Docker/
cosign packaging, command palette, attachment panel, QuestionPanel, central
theming, per-language minify annotators, formatter-gated write-back. See WO
17.0 for the full deferred list with reasons.

### Series 21 — Close Every Open Review Item (done/fail + defer-disclosure)

Series 21 is the close-out series for the brutal production-readiness review
(reality-checked @ `a870571` on 2026-08-07; see
`docs/reviews/`/`kirkforge-review.md`). It exists to fix the process failure WO
20 exposed: items shipped without explicit done/fail criteria and silent
deferral. **Every item in every 21.* sub-workorder carries both success AND
failure criteria, and no deferral is silent** (AGENTS.md rule #11). The master
overview is [WO 21.0](21.0-wo21-overview.md); the series is "Done" only when
every item's success criteria are met AND its failure criteria are demonstrably
not-triggered, evidenced by gate output on the closing commit.

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 21.0 | [WO 21 Series Overview](21.0-wo21-overview.md) | Done | — | — |
| 21.1 | [Scope-creep decisions: cut / spin-off / keep-and-finish](21.1-focus-scope.md) | Done | High | — |
| 21.2 | [Finish 3 incomplete rust-native plugins](21.2-plugins-rust-native.md) | Done | High | 21.1 |
| 21.3 | [Stratum compression: real content transforms](21.3-stratum-compression.md) | Done | High | — |
| 21.4 | [LLM adapter gaps](21.4-adapter-gaps.md) | Done | High | — |
| 21.5 | [Tools & MCP surface gaps](21.5-tools-mcp.md) | Done (R2/R4/R9 deferred) | High | — |
| 21.6 | [Context, retrieval & memory gaps](21.6-context-memory.md) | Done | High | — |
| 21.7 | [Sandbox & trust hardening](21.7-sandbox-trust.md) | Done | High | — |
| 21.8 | [TUI & agent-loop gaps](21.8-tui-agentloop.md) | Done (AppState decomposition + themes deferred) | High | — |
| 21.9 | [Engineering-discipline debt](21.9-discipline-debt.md) | Done | High | — |
 | 21.10 | [MCP-first migration](21.10-mcp-first-migration.md) | Done | High | 21.5-R3/R4 |
 | 21.11 | [Plugin real-rebuild (budget/stratum/sdk) + draw/video yeet](21.11-plugin-real-rebuild.md) | Done | High | supersedes 21.1-R1/R2, voids 21.2-R2/R3 |
 | 21.0.14 | [Deferred item tracker (all WO 21 series)](21.0.14-deferred-tracker.md) | In Progress | — | — |
 
 **12 sub-workorders, 72 items**, each with success + failure criteria. Highest-
leverage item: **21.9-R4** (test deadlock fix → re-enables CI, which gates
honest verification of everything else). Full ordering/dependencies and the
global done/fail + defer-disclosure rule are in [WO 21.0](21.0-wo21-overview.md).

### Series 25 — Test Infrastructure + Deferred Work Closure

Series 25 has two axes. **25.1-25.8** is the test-infrastructure series: split
the gate, speed the suite, ship `kf-testdoctor` optimizations, add `cargo
llvm-cov` coverage baseline + regression gate, fix stale plugin3/stratum/kfd
script refs, lift the deadlock quarantine, fix the benchmark source-of-truth,
and clean historical residue. **25.9-25.18** closes deferred work + newly-
uncovered audit findings: dead code behind `#[allow(dead_code)]`, stale
binary/path refs in user-facing config, TUI metrics + daemon push gaps,
AppState decomposition, process-global state scoping, verifier finding
location data, MCP protocol consistency, the `src/session` coverage push
(carry-forward of WO 24.6), persona Bedrock/Vertex plumbing + the landlock
default-on decision, and a backlog of smaller deferred capabilities. The
master overview is [WO 25.0](25.0-wo25-test-infra-overview.md); every item
carries success AND failure criteria per AGENTS.md rule #11 (no silent
deferral).

| # | Workorder | Status | Priority | Depends on |
|---|---|---|---|---|
| 25.0 | [WO 25 Series Overview](25.0-wo25-test-infra-overview.md) | Done | — | — |
| 25.1 | [Fast test gate split](25.1-fast-test-gate.md) | Done | P0 | — |
| 25.2 | [Test speed optimization](25.2-test-speed-optimization.md) | Done (R2+R4 deferred) | P1 | 25.1 |
| 25.3 | [kf-testdoctor optimization](25.3-testdoctor-optimization.md) | Done (R3+R4 deferred) | P1 | — |
| 25.4 | [Coverage baseline tooling](25.4-coverage-baseline.md) | Done (R3 deferred) | P2 | 25.1 |
| 25.5 | [Release/install/CI script hygiene](25.5-release-hygiene.md) | Done | P0 | — |
| 25.6 | [Deadlock quarantine lift](25.6-deadlock-quarantine-lift.md) | Done | P1 | — |
| 25.7 | [Benchmark source of truth](25.7-benchmark-hygiene.md) | Done | P1 | — |
| 25.8 | [Historical residue cleanup](25.8-historical-residue-cleanup.md) | Done (R4 deferred) | P2 | 25.5 |
| 25.9 | [Dead code & non-Rust lint stub cleanup](25.9-dead-code-and-lint-stubs.md) | Done | P1 | — |
| 25.10 | [Stale binary/path refs + ADR path-literal enforcement](25.10-stale-refs-and-adr-path-enforcement.md) | Done (R4 deferred) | P0 | 25.5 (coordinate) |
| 25.11 | [TUI metrics & daemon push gaps](25.11-tui-metrics-and-daemon-push.md) | Done (R2 deferred) | P1 | — |
| 25.12 | [AppState decomposition + cached_tokens fork-reset](25.12-appstate-decomposition.md) | Done (R1 deferred) | P2 | — |
| 25.13 | [Process-global state scoping](25.13-process-global-state-scoping.md) | Done | P2 | — |
| 25.14 | [Verifier finding location data](25.14-verifier-finding-location.md) | Done | P1 | — |
| 25.15 | [MCP protocol consistency: roots + sampling](25.15-mcp-protocol-consistency.md) | Done (R2+R3 deferred) | P1 | — |
| 25.16 | [Coverage push: src/session >75%](25.16-session-coverage-75.md) | Planned | P2 | 25.4 |
| 25.17 | [Persona adapter Bedrock/Vertex + landlock default-on decision](25.17-persona-adapter-and-landlock.md) | DONE (R1 deferred, R2 opt-in) | P1 | — |
| 25.18 | [Deferred capability carry-forward](25.18-deferred-capability-carryforward.md) | Planned | P2/P3 | — |
| 25.19 | [AGENTS.md multistep workflow upgrade](25.19-agents-md-multistep-workflow.md) | Done | P1 | — |

**19 sub-workorders** (8 test-infra + 10 deferred/audit + 1 process). Highest-leverage P0s:
**25.5** (broken release/install scripts) and **25.10** (config.toml.example
ghost plugins + ADR path-literal enforcement). Highest-leverage P1 audit
findings: **25.17** (persona Bedrock/Vertex — prior review's "fixed" claim was
false) and **25.15** (MCP `sampling/createMessage` security surface). Full
ordering/dependencies in [WO 25.0](25.0-wo25-test-infra-overview.md).

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.

### Series 27 — Post-Deep-Review Hardening + TUI Polish

Series 27 is the post-review follow-up to the 2026-08-11 deep codebase audit
(8 parallel subagents; report at `kf-code-review.md` 5th edition). The review
surfaced one CRITICAL (C1 landlock never applied), nine HIGH (H1-H9), and a
cluster of MEDIUM findings. WO 26 + the review-fix session closed 7 of them;
WO 27 closes the rest + adds TUI themes + mouse (the competitive polish gaps
vs Cursor/Claude Code). See [WO 27.0](27.0-wo27-overview.md) for the full
plan + ordering.

| WO | Title | Status | Priority | Deps |
|----|-------|--------|----------|------|
| 27.0 | [Series overview](27.0-wo27-overview.md) | In Progress | — | — |
| 27.1 | [Landlock default-on + actually applied (C1)](27.1-landlock-default-on.md) | Planned | P0 | — |
| 27.2 | [Test health: CI green + triage 49 ignored + coverage gate (H6)](27.2-test-health.md) | In Progress | P0/P1 | — |
| 27.3 | [Architecture debt: tools↔session cycle + god-objects (H7/H8)](27.3-architecture-debt.md) | Planned | P1 | — |
| 27.4 | [Plugin trust default (H9)](27.4-plugin-trust.md) | Planned | P1 | — |
| 27.5 | [Bash hardening: background jobs + deny-list posture (H3/H5)](27.5-bash-hardening.md) | Planned | P2 | 27.1 |
| 27.6 | [TUI themes (polish)](27.6-tui-themes.md) | Planned | P2 | — |
| 27.7 | [TUI mouse support (polish)](27.7-tui-mouse.md) | Planned | P2 | — |

**8 sub-workorders.** Highest leverage: **27.2** (CI green is the unblock for
everything) and **27.1** (C1 is the security moat vs codex-cli). 27.6/27.7 are
the pure-TUI polish items safe to parallelize. Full ordering + dependency graph
in [WO 27.0](27.0-wo27-overview.md).

### Series 28 — Open Issues + Tech Debt + Deferred Work (the full ledger)

Series 28 is the "close everything open" series: every remaining deep-review
finding (H3/H5/H7/H8), every gitnexus-surfaced cycle (2 Rust + 5 TS), every
state.md deferred item that's still genuinely open (verified against code at
`5296f41`), + the WO 27 follow-ups (e2e hang, coverage gate, background
landlock). 17 workorders, each with root cause + file:line + R-items + gates.
See [WO 28.0](28.0-wo28-overview.md) for the full plan + ordering.

**Quickest wins:** 28.5 (pub(crate) re-export, ~5 lines), 28.2 (move one type),
28.14 (env shim). **Highest leverage:** 28.1 (tools↔session cycle break),
28.6 (real e2e coverage), 28.7+28.9 (coverage enforcement).

| WO | Title | Status | Est |
|----|-------|--------|-----|
| 28.1 | [Break tools↔session cycle (H7)](28.1-tools-session-cycle.md) | Done (R1; 3 non-cyclic residuals) | L |
| 28.2 | [Break budget↔stratum cycle](28.2-budget-stratum-cycle.md) | Done (R1; R2 optional deferred) | S |
| 28.3 | [Split turn.rs god-object (H8)](28.3-turn-rs-split.md) | Done (2087→1191 LOC) | M |
| 28.4 | [Fix TS plugin-SDK cycles](28.4-ts-plugin-cycles.md) | Superseded (TS tree deleted, WO 29.9) | M |
| 28.5 | [Landlock on background bash jobs](28.5-background-landlock.md) | Done (R1+R2; R3 test deferred) | S |
| 28.6 | [e2e binary-spawn hang debug/convert](28.6-e2e-hang-debug.md) | Planned | M |
| 28.7 | [Coverage regression gate](28.7-coverage-gate.md) | Done (R1-R4 local; ci.yml step deferred) | M |
| 28.8 | [daemon/client.rs tests](28.8-daemon-client-tests.md) | Done (R1-R5; R6 Windows deferred) | M |
| 28.9 | [Session coverage >75%](28.9-session-coverage-75.md) | Done (5 tests; R2.5/R2.6 disclosed) | XL |
| 28.10 | [Slow-test feature-flag split](28.10-slow-test-split.md) | Done (R1+R3+R4; R2 n/a) | S-M |
| 28.11 | [Bench manifest validation](28.11-bench-manifest-validation.md) | Done (R1+R2; R3 deferred) | S |
| 28.12 | [Dead-ref CI gate for src/](28.12-dead-ref-ci-gate.md) | Done (R1+R4; R2 n/a) | S-M |
| 28.13 | [Bedrock/Vertex test mocks](28.13-bedrock-vertex-test-mocks.md) | Partial (R1 Bedrock; R2 Vertex + full-turn deferred) | M |
| 28.14 | [Plugin3 env backward-compat shim](28.14-plugin3-env-shim.md) | Done | S |
| 28.15 | [Memory semantic dedup](28.15-memory-semantic-dup.md) | Done (R1+R3; R2 verified no-op) | M |
| 28.16 | [Anthropic computer_use beta](28.16-anthropic-computer-use-beta.md) | Partial (R1-R3 adapter wire; R4 deferred) | L |
| 28.17 | [Bash evasion tripwire posture (H3)](28.17-bash-evasion-tripwire-posture.md) | Done (R1; R2 deferred) | S |

### Series 29 — TS→Rust SDK Migration (Eliminate the Node Runtime)

Series 29 migrates the entire `npm/kf-plugin/` TypeScript plugin SDK to Rust.
Three exhaustive inventory subagents (2026-08-12) mapped every TS function/type/
class + the Rust plugin architecture. Key findings: NO JSON-RPC exists (plugins
are env-var + exit-code + stdout); `core-sandbox` is DEAD (delete, don't port);
the Rust host calls exactly ONE TS path (bridge-emitter.ts NDJSON); ~1700 LOC of
pure modules are ready for direct translation; the orchestrator (7955 LOC) is the
biggest target. See [WO 29.0](29.0-wo29-overview.md) for the full phased plan.

| WO | Title | Status | Est | Phase |
|----|-------|--------|-----|-------|
| 29.0 | [Series overview](29.0-wo29-overview.md) | Planned | — | — |
| 29.1 | [Fold bundled plugin into compiled-in Rust tools](29.1-fold-bundled-plugin.md) | In Progress (Phase 1 shipped; verify tools deferred to 29.7) | M | 1 |
| 29.2 | [Rust security emitter (replace bridge-emitter.ts)](29.2-rust-security-emitter.md) | Done (R1+R2; R3 folded into 29.9) | S | 1 |
| 29.3 | [Port pure modules (classifier + routing + correction + truth-model)](29.3-pure-module-ports.md) | Done | M | 2 |
| 29.4 | [Port EventBus + AuditLogger (core-events)](29.4-event-bus-audit-logger.md) | Done (R1-R3; R4 dead sinks skipped) | M | 3 |
| 29.5 | [Port RBAC + JWT verification (core-rbac)](29.5-rbac-jwt-port.md) | Done | M | 3 |
| 29.6 | [Port memory-palace (routing store + adapters)](29.6-memory-palace-port.md) | Done (R1-R3; R4 skipped per workorder) | M | 3 |
| 29.7 | [Port orchestrator delegation + decompose + correction loop](29.7-orchestrator-port.md) | In Progress (R1-R5 ported; ModelClient prod impl + validator deferred) | L | 4 |
| 29.8 | [Port health-server (HTTP + auth + Prometheus)](29.8-health-server-port.md) | Skipped (not needed for CLI) | M | 4 |
| 29.9 | [Delete npm/kf-plugin/ + remove TS CI + deps](29.9-delete-npm-cleanup.md) | Done | S | 5 |

**9 sub-workorders.** Phase 1 (29.1-29.2) ships independently — eliminates the
bundled-plugin Node hop + the last Rust→TS call path. Phase 2-3 port the
foundation. Phase 4 is the orchestrator. Phase 5 deletes everything TS.

### Series 30 — External-Review Action Items + OS Isolation + Subagent Isolation

Series 30 is driven by the ChatGPT external review (2026-08-13) and the WO 30
master index ([WO 30.0.0](30.0.0-wo30-overview.md)). It adds the missing
OS-isolation layer (seccomp), real subagent isolation (worktree + approval
forwarding), and the TaskManager lifecycle. Status below verified against HEAD
`2bfc2fa`; the overview is the living master index.

| WO | Title | Status | Est |
|----|-------|--------|-----|
| 30.1 | Subagent worktree isolation | Done (`8717bd1`; coder subagent gets its own git worktree; bash CWD confinement deferred) | M |
| 30.2 | TaskManager lifecycle (status/cancel/metadata/list) | Done (`63800ed`) | M |
| 30.3 | Parallel agent orchestration (scout/coder/reviewer) | Planned (dep: 30.1 + 30.2) | L |
| 30.4 | Seccomp syscall filter (default-off, opt-in `seccomp` feature) | Done (`2bfc2fa`; allowlist tuning + cross-arch + default-on flip deferred) | M |
| 30.5 | Context-index target/ filter (e2e hang secondary root cause) | Done (`04c8f60`; `filter_entry` skips `target`/`.git`/`node_modules`/`.venv`/`dist`/`build`) | S |
| 30.6 | Subagent approval forwarding to parent session | Done (`8717bd1`; supersedes the conservative P0 auto-approve deny at `5fbd955`) | M |
| 30.7 | Benchmark: KirkForge vs Codex vs Claude Code (token-budget challenge) | Planned | M |

**7 items.** Shipped at HEAD `2bfc2fa`: the seccomp OS-isolation layer, subagent
worktree isolation, approval forwarding, the TaskManager lifecycle, and the
context-index walker filter. Open: the parallel-orchestration pattern (30.3)
and the cross-tool benchmark (30.7).

### Series 32 — Post-Review Audit Corrections + Full Deferral Inventory

Series 32 is driven by the 2026-08-14 evidence audit (4 parallel read-only
subagents grepped every claim in the 8th-edition production-readiness review
against source). The audit found 11 factual errors in the review (all corrected
in-place) and surfaced every stubbed/deferred/skipped item from WO 27-31. A
second pass verified each item against source: 11 RESOLVED, 9 OPEN. The OPEN
items get individual WO 32 sub-workorders for tracking. See
[WO 32.0](32.0-wo32-overview.md) for the full inventory + resolved/open split.

| WO | Title | Status | Est | Priority |
|----|-------|--------|-----|----------|
| 32.0 | [Series overview + full deferral inventory](32.0-wo32-overview.md) | In Progress | — | — |
| 32.1 | [Fix stale WO 30 overview statuses](32.1-fix-wo30-overview-statuses.md) | Done | S | — |
| 32.2 | [Fix stale plugin_verify deferral message](32.2-fix-plugin-verify-message.md) | Done (`f2c33d4`) | S | — |
| 32.3 | [Clean up untracked npm/ build artifacts](32.3-clean-npm-artifacts.md) | Pending | S | — |
| 32.4 | [Subagent CWD confinement](32.4-subagent-cwd-confinement.md) | Pending | M | High |
| 32.5 | [Parallel orchestration (scout/coder/reviewer)](32.5-parallel-orchestration.md) | Done (`9965478`; partially implemented — per-subagent worktree + Reviewer BusVerifier deferred) | L | Medium |
| 32.6 | [Cross-tool benchmark vs Codex/Claude Code](32.6-cross-tool-benchmark.md) | Done (`d769d4b`; framework + runner shipped; live run pending) | M | High |
| 32.7 | [WO 28.9 missing 11 tests (R3/R4/R5 — silent deferral)](32.7-missing-28.9-tests.md) | Done (`b294295`) | M | High |
| 32.8 | [WO 28.1 port-trait residuals (3 imports)](32.8-port-trait-residuals.md) | Done (`d828a89`) | M | Medium |
| 32.9 | [WO 28.6 e2e tests still `#[ignore]`'d](32.9-e2e-tests-unignore.md) | Done (`78586f9`; e2e now behind `e2e-tests` Cargo feature) | S | Medium |
| 32.10 | [WO 29.5 ES512 verifier gap](32.10-es512-verifier-gap.md) | Done (`c20c832`; documented with pinned test) | S | Low |
| 32.11 | [WO 29.3 ClassifierMemory + PathGuard unification](32.11-classifier-memory-pathguard.md) | Done (`1e7f523`; comment-only cleanup; all three deferrals disclosed, not implemented) | M | Low |
| 32.12 | [WO 27.7 R2 click-in-prompt cursor positioning](32.12-click-in-prompt-cursor.md) | Done (`fced991`) | S | Low |
| 32.13 | [Streaming timeout per-adapter configurability](32.13-streaming-timeout-configurable.md) | Done (`d6e507e`; default 180s) | S | Low |
| 32.14 | [WO 28.7 R4 ci.yml coverage step](32.14-ci-yml-coverage-step.md) | Done (`b31f4a4`; now in ci-nightly.yml per ADR-074) | S | Medium |
| 32.15 | [WO 28.5 R3 landlock FS confinement test](32.15-landlock-fs-confinement-test.md) | Pending | S | Low |
| 32.16 | [WO 28.8 R6 Windows stub test](32.16-windows-stub-test.md) | Pending | S | Low |
| 32.17 | [WO 28.16 R4 computer_use vision loop (re-deferred 3×)](32.17-computer-use-vision-loop.md) | Done (`55aa9f0`; WO 32.17 shipped the hosted computer_use beta) | L | Medium |
| 32.18 | [WO 28.17 R2 bash require_allowlist mode](32.18-bash-require-allowlist.md) | Done (`ef2d765`; default-off) | S | Low |
| 32.19 | [WO 29.7 R6 SLO monitor + R7 security-emitter integration](32.19-slo-monitor-security-emitter.md) | Done (`41014b7`; R7 shipped; R6 disclosed as YAGNI) | M | Low |
| 32.20 | [WO 31.2/31.3/31.5 Node + Go + generic verifiers](32.20-node-go-generic-verifiers.md) | Done (`598841d`) | M | Medium |

**21 items.** 32.1-32.2, 32.5-32.14, 32.17-32.20 done (13 shipped). 32.3, 32.4,
32.15, 32.16 pending (4 open). 32.0 overview in progress. High priority
remaining: 32.4 (CWD confinement — security). Most persistent was 32.17
(computer_use vision loop — re-deferred 3× since WO 25.18, finally shipped in
WO 32.17). Note: several WO 32 file `## Status` headers still say "Pending"
despite the work shipping — the sessions that shipped them did not update the
WO file status (AGENTS.md §9 violation); the README index is corrected here
to reflect actual status per `git log` + `state.md` + `CHANGELOG.md`.

### Series 33 — CI Architecture Reset + Test Debt Elimination

Series 33 is driven by the `gpt-test_and_ci.md` external review. It resets
the CI architecture (ADR-074: PR / merge / nightly tiers, no Ollama or
coverage on PR/merge, parallel sibling jobs, scoped clippy, `fmt` →
`static` rename) and eliminates test debt in three phases: kill wall-clock
sleeps (phase 1), kill global env mutation via `EnvGuard` (phase 2), and
replace real subprocess tests with fakes (phase 3 — `CommandRunner` trait
for Cargo/Clippy, `JwksResolver` for kf-rbac JWT). See
[WO 33.0](33.0-wo33-overview.md) for the full plan.

| WO | Title | Status | Priority |
|----|-------|--------|----------|
| 33.0 | [Series overview](33.0-wo33-overview.md) | In Progress | — |
| 33.1 | [Concurrency cancellation](33.1-concurrency-cancellation.md) | Done (`839d39d`) | P0 |
| 33.2 | [Remove artificial needs: dependencies](33.2-remove-artificial-needs.md) | Done (`839d39d`) | P0 |
| 33.3 | [Split CI into PR / post-merge / nightly workflows](33.3-split-ci-workflows.md) | Done (`cd79462`) | P0 |
| 33.5 | [Nextest profiles (ci-fast, ci-full, integration, e2e)](33.5-nextest-profiles.md) | Done (`7e82ea2`) | P1 |
| 33.6 | [Path-aware / changed-package test selection](33.6-changed-package-selection.md) | Done (`95c9588`) | P1 |
| 33.11 | [Profile: measure top-20 slowest tests](33.11-slowest-tests.md) | Done (measurement) (`7820522`) | P1 |
| 33.12 | [Kill wall-clock sleeps in tests (phase 1)](33.12-kill-sleeps.md) | Done (`b010aa4` + `ac8df2b`) | P1 |
| 33.13 | [Kill global env mutation in tests (phase 2)](33.13-kill-env-mutation.md) | Done (EnvGuard mechanism shipped; rollout complete per WO 33.16 session) (`35d1a21` + `d89689d`-`a2a2f80`) | P1 |
| 33.14 | [Replace real subprocess tests with fakes (phase 3)](33.14-replace-subprocess-tests.md) | Done (phase 3 — minimal win; items 4/5/6 deferred) (`e6b7ccb` + `f2d53ab`) | P2 |
| 33.15 | [Reduce #[serial] usage via dependency injection](33.15-reduce-serial-usage.md) | Done (no-op: zero #[serial] in repo) (`71e31cb`) | P2 |
| 33.16 | [Config-overwrite-on-install bug fix](33.16-config-overwrite-bug.md) | Done (config migration from legacy kirkforge path shipped) (`15dd08a`) | P0 |
| 33.17 | [Update module (auto-pull latest from main)](33.17-update-module.md) | Done (`8ab0098`; self-update subcommand with SHA256 verification) | P1 |

**13 sub-workorders.** The CI architecture reset is pinned in ADR-074. The
test-tier phases (sleep elimination, env-mutation elimination, subprocess
fakes) are complete; the `EnvGuard` RAII helper (`src/shared/test_util.rs`)
is the canonical pattern for test env mutation. Phase 3 items 4/5/6 (bash
Docker mock, bash_jobs 64-process fake, E2E collapse) are deferred per
AGENTS.md §11 — tracked in `state.md` pending. Note: the env-mutation work
is tracked as WO 33.13 in the workorder file but referenced as "WO 33.16"
in some `state.md`/`CHANGELOG.md` session blocks (a numbering inconsistency
from the session that shipped it); the actual WO 33.16 workorder file is
the config-overwrite-on-install bug.

### Series 34 — TUI Information Architecture Reset

Series 34 is driven by an external UX analysis of the TUI. 70% of the
analysis recommendations were validated against the codebase; the series
resets the TUI information architecture across 10 implementation
workorders + 1 doc-audit workorder. See
[WO 34.0](34.0-wo34-overview.md) for the full plan.

| WO | Title | Status | Priority |
|----|-------|--------|----------|
| 34.0 | [Series overview](34.0-wo34-overview.md) | In Progress | — |
| 34.1 | [Kill F1-F6 tab bar + command palette (Ctrl+K)](34.1-kill-tab-bar-command-palette.md) | Pending | — |
| 34.2 | [/help overlay (stop polluting conversation)](34.2-help-overlay.md) | Pending | — |
| 34.3 | [Simplify status bar to 4 items](34.3-simplify-status-bar.md) | Pending | — |
| 34.4 | [Settings → semantic controls](34.4-settings-semantic-controls.md) | Pending | — |
| 34.5 | [Models → chooser](34.5-model-chooser.md) | Pending | — |
| 34.6 | [Jobs → structured monitor](34.6-jobs-monitor.md) | Pending | — |
| 34.7 | [Unify sessions/threads/forks naming](34.7-unify-sessions-naming.md) | Pending | — |
| 34.8 | [Welcome screen quick actions](34.8-welcome-screen.md) | Pending | — |
| 34.9 | [Slash command taxonomy (everyday/advanced/developer)](34.9-slash-command-taxonomy.md) | Pending | — |
| 34.10 | [Approval dialog action-first](34.10-approval-dialog.md) | Pending | — |
| 34.11 | [Doc audit (CHANGELOG, state.md, TECHNICAL.md)](34.11-doc-audit.md) | In Progress | — |

**12 sub-workorders.** All 11 implementation workorders (34.1-34.11) are
pending; only the planning commit (`d94c575`) has landed. WO 34.11 (this
doc audit) is in progress. The series is the TUI IA reset — no Rust
architecture changes, no new ADRs expected.

### Series 35 — External review verification → orchestration hardening

Series 35 is driven by an external architecture review (2026-08-19).
Every review claim was verified against the codebase by 5 read-only
subagents (see [WO 35.0](35.0-wo35-overview.md) for the full
verdict table, including refuted claims). 7 workorders cover the
confirmed findings: real pipeline semantics, subagent isolation,
cooperative cancellation, sandbox posture visibility, integration
tests, the kf-orchestrator seam, and a version-badge gate.

| WO | Title | Status | Priority | Depends on |
|----|-------|--------|----------|------------|
| 35.0 | [Series overview + verification verdicts](35.0-wo35-overview.md) | Done | — | — |
| 35.1 | [Real pipeline semantics for scout/coder/reviewer](35.1-pipeline-semantics.md) | Pending | P0 | 35.2 |
| 35.2 | [Per-subagent worktree isolation + artifact return](35.2-subagent-worktrees.md) | Pending | P1 | — |
| 35.3 | [Cooperative cancellation + temp-dir cleanup](35.3-cooperative-cancellation.md) | Pending | P1 | — |
| 35.4 | [Sandbox posture indicator (real layer state)](35.4-sandbox-posture-indicator.md) | Pending | P2 | — |
| 35.5 | [Cross-component integration tests (2 chains)](35.5-cross-component-integration-tests.md) | Pending | P2 | 35.1, 35.3 |
| 35.6 | [Wire Executor into kf-orchestrator ModelClient](35.6-orchestrator-modelclient-wiring.md) | Pending | P2 | 35.1, 35.3 |
| 35.7 | [Version badge consistency gate](35.7-version-badge-gate.md) | Pending | P3 | — |

### Series 36 — Close the WO 35 remainders; unify orchestration

Driven by the WO 35 session-close debt list (every item a disclosed
deferral, not a new wish). See [WO 36.0](36.0-wo36-overview.md).

| WO | Title | Status | Priority | Depends on |
|----|-------|--------|----------|------------|
| 36.0 | [Series overview](36.0-wo36-overview.md) | In Progress | — | — |
| 36.1 | [Measure release binary; decide rusqlite](36.1-binary-size-rusqlite.md) | Pending | P1 | — |
| 36.2 | [Bash-job owner tracking + cancel-by-owner](36.2-bash-job-owner-tracking.md) | Pending | P1 | — |
| 36.3 | [Abort model streams mid-request on cancel](36.3-stream-abort.md) | Pending | P1 | — |
| 36.4 | [Live cancel token for the parent session](36.4-parent-cancel-token.md) | Pending | P2 | 36.3 |
| 36.5 | [Production-wire the ModelClient seam](36.5-modelclient-production-wiring.md) | Pending | P2 | 36.3, 36.4 |
| 36.6 | [EventSink → EventBus bridge](36.6-eventsink-bridge.md) | Pending | P3 | folds into 36.5 |
### Series 37 — Registry hardening + the reducer

Post-WO-36 residuals. See [WO 37.0](37.0-wo37-overview.md). CLI/UX
deferred to its own analysis-first series (WO 38 candidate).

| WO | Title | Status | Priority | Depends on |
|----|-------|--------|----------|------------|
| 37.0 | [Series overview](37.0-wo37-overview.md) | Done | — | — |
| 37.1 | [BashJobRegistry hardening](37.1-registry-hardening.md) | Pending | P1 | — |
| 37.2 | [The reducer: make packet real](37.2-reducer.md) | Pending | P2 | — |
