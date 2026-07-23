# Workorders — Planned and In-Progress Work

This directory contains numbered workorders that define scoped tasks for
KirkForge-Cli. Each workorder lists the problem, root cause, files to touch,
approach, gate, and done condition.

## Active series

### Series 6 — Benchmarks and Continuous Evaluation

| # | Workorder | Status | Depends on |
|---|---|---|---|
| 6.1 | [Bench harness realism](6.1-bench-realism.md) | Planned | — |
| 6.2 | [Bench delta comparison](6.2-bench-delta-comparison.md) | Planned | — |
| 6.3 | [Bench CI wiring](6.3-bench-ci-wiring.md) | Planned | 6.2 |
| 6.4 | [Bench list + verify-only](6.4-bench-list-verify-only.md) | Planned | — |
| 6.5 | [Continuous eval ADR](6.5-bench-eval-adr.md) | Planned | 6.1-6.4 |

### Series 7 — Plugin Integration

| # | Workorder | Status | Depends on |
|---|---|---|---|
| 6.6 | [Fold Stratum into core](6.6-fold-stratum.md) | Planned | — |
| 6.7 | [Fold Plugin3 into core](6.7-fold-plugin3.md) | Planned | 6.6 |
| 6.8 | [Fold Draw into core](6.8-fold-draw.md) | Planned | 6.6 |
| 6.9 | [Fold Video into core](6.9-fold-video.md) | Planned | 6.6 |
| 7.0 | [Plugin system consolidation](7.0-plugin-consolidation.md) | Planned | 6.6-6.9 |

## Conventions

- Each workorder is a single markdown file named `<number>-<slug>.md`.
- Status is one of: Planned, In Progress, Done, Superseded.
- The gate must match AGENTS.md §4 (fmt --check, check, clippy, test).
- When a workorder is done, update its Status to "Done" and note the commit SHA.
- When a workorder is superseded, update its Status and link to the replacement.
- The scratch `workplan.md` at the repo root (gitignored) is for the current
  task's working notes; the workorders here are the persistent plan.