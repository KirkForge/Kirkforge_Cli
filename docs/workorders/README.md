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
| 9.1 | [Workflow tool wrapper](9.1-workflow-tool-wrapper.md) | Planned | Medium | — |
| 9.2 | [Bench PR delta comment](9.2-bench-pr-delta-comment.md) | Done | Medium | 6.2, 8.1 |
| 9.3 | [Version reconciliation and v0.3.6 release](9.3-version-reconciliation.md) | Planned | High | 8.0-8.9 |
| 9.4 | [Replay interactive stepper](9.4-replay-interactive-stepper.md) | Done | Medium | — |
| 9.5 | [Prompt cache stem reuse](9.5-prompt-cache-stem-reuse.md) | Done | High | — |
| 9.6 | [Verifier bus code unification](9.6-verifier-bus-unification.md) | Planned | Medium | ADR-028 |
| 9.7 | [Tree-sitter VFS minification](9.7-vfs-minification.md) | Planned | Medium | — |
| 9.8 | [Seccomp/rlimit sandbox hardening](9.8-seccomp-rlimit-hardening.md) | Planned | Low | — |
| 9.9 | [Bench task expansion: real-world shapes](9.9-bench-task-expansion-2.md) | Planned | High | 9.0 |

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

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.