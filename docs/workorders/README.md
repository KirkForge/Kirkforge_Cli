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
| 7.1 | [Budget slicing action](7.1-budget-slicing-action.md) | Planned | High | 6.7 |
| 7.2 | [Fold-in unit tests](7.2-fold-in-unit-tests.md) | Planned | High | 6.6-6.9 |
| 7.3 | [Fix bench CI gate theater](7.3-bench-gate-theater.md) | Planned | Medium | 6.3 |
| 7.4 | [Remove legacy spec-drift tests](7.4-remove-legacy-spec-drift.md) | Planned | Low | — |
| 7.5 | [Budget and Stratum config fields](7.5-budget-stratum-config.md) | Planned | Medium | 7.1 |
| 7.6 | [Windows test parity](7.6-windows-test-parity.md) | Planned | Medium | — |
| 7.7 | [KVB verifier bus bridge](7.7-kvb-verifier-bus-bridge.md) | Planned | Medium | 7.0 |
| 7.8 | [Bench task expansion](7.8-bench-task-expansion.md) | Planned | Medium | — |
| 7.9 | [Context index Phase 7: embeddings + graph-walk](7.9-context-index-phase7.md) | Planned | High | — |

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

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.