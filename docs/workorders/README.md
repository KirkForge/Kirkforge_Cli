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
| 11.3 | [Surface trust-tier downgrades in `/plugins list`](11.3-surface-trust-downgrades.md) | Planned | Low | — |
| 11.4 | [Plugin hot-reload via file watcher](11.4-plugin-hot-reload.md) | Planned | Medium | — |
| 11.5 | [Per-plugin resource limits (extend SandboxConfig)](11.5-per-plugin-resource-limits.md) | Done | Medium | 9.8 |
| 11.6 | [Plugin hook fail-open audit log](11.6-plugin-hook-audit-log.md) | Done | High | — |
| 11.7 | [Plugin verifier results in `/verify` panel + cost report](11.7-plugin-verifier-ui.md) | Done | Medium | 9.6 |
| 11.8 | [Plugin init scaffolding command (`kirkforge plugin init`)](11.8-plugin-init-scaffolding.md) | Planned | Low | 11.0 |
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

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.