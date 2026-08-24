# ADR-077: Bench spec task triage — implement / defer / drop

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-08-23
- **Related:** [ADR-0066](./066-kirk-bench-spec.md) (the KIRK-BENCH spec + the
  19 planned tasks this triage classifies), [ADR-0033](./0033-tool-retry-backoff.md)
  (retry telemetry — why spec task 39 is dropped)

## Context

ADR-0066 listed 19 spec tasks as "planned (honest deferral)" and 3 more as
"no mapping yet" — a 22-item backlog dump with no verdict on whether any
item would ever ship. `docs/TECHNICAL.md` had also drifted to "18" while the
table held 19 rows. WO 43.13 forced a decision gate: each row becomes
**implement**, **defer-with-ADR**, or **delete-from-spec**.

## Decision

The 22 items (19 table rows + 3 unmapped) are classified as follows.

### implement (4) — deterministic verify is cheap and the feature is user-visible

| Spec task | Why implement |
|---|---|
| 22 Build Verification | `verify = command_exits_zero "cargo build"`; standalone task, no fixture beyond a tiny crate. Exercises the build verifier slot (ADR-0031). |
| 23 Formatter Verification | `verify = command_exits_zero "cargo fmt --check"`. Standalone, deterministic. |
| 24 Lint Verification | `verify = command_exits_zero "cargo clippy -- -D warnings"`. Standalone, deterministic. |
| Implement Missing Trait (unmapped) | Deterministic: `grep -q "impl.*Trait" src/lib.rs"` or compile-check. Fills a C/D slot gap. |

These are future WOs (43.40+). Each is a single TOML file in
`benches/tasks/` with a `command_exits_zero` verify block — no new harness
code, no fixture larger than a stub crate.

### deferred (12) — real but blocked on a concrete missing capability

| Spec task | Blocker (what must ship first) |
|---|---|
| 1 Find Dead Code | `kf-context-index` unreferenced-symbol query (symbol-graph `ACCESSES`-style edges over the workspace, not the per-file index today). |
| 2 Dependency Graph Accuracy | crate-level dep-graph generation from `Cargo.toml` workspace walk — the context index is per-file symbol/import, not crate-edge. |
| 3 Call Graph Generation | per-symbol call graph export from `kf-context-index` (the index builds call edges internally but exposes no `call_graph()` API). |
| 4 Explain Module | module summarisation without hallucination — needs a grounded-retrieval path (symbol graph + doc spans) that does not exist. |
| 5 Cross-Repository Search | trait-impl search across the workspace — `kf-context-index` is single-repo; workspace support is ADR-0066's named future work. |
| 9 Split Giant File | a 2500-line fixture crate the model must split — fixture authorship, not harness code. |
| 18 Add REST Endpoint | non-Rust task setup (a Python/Node fixture with a REST handler) — the harness is Rust-only today. |
| 27 Large Repository Navigation | a Linux-scale context-index fixture (~30k files) — no such fixture exists and the index has not been load-tested at that scale. |
| 32 Large Refactor | 50+ file fixture — fixture authorship. |
| 33 Merge Conflict Resolution | realistic conflict fixture (a repo mid-merge with semantic conflicts) — fixture authorship + non-deterministic verify. |
| 35 Regression Detection | PR regression prediction — non-deterministic scoring (no deterministic verify block possible; it is a model-judgement task). |
| Fix Integration Test (unmapped) | needs a live integration-test fixture (Ollama + `qwen2.5:0.5b`) baked into the task — the harness `verify-only` path skips `requires_model` tasks. |

These remain future WOs. Each deferral is tracked in `state.md` pending
with the concrete blocker named above. The blocker, not "later", is the
deferral reason — a future worker reads this ADR to know what to build
first.

### dropped (6) — no longer a differentiating capability

| Spec task | What replaced it / why it never mattered |
|---|---|
| 36 Token Efficiency | The Token Budget Challenge (ADR-0066) already records `prompt_tokens` and `completion_tokens` per ceiling — a standalone token-efficiency task duplicates the signature benchmark's own telemetry. |
| 37 Dollar Cost | The Budget Challenge records `cost` per ceiling. A standalone cost task adds no signal the signature benchmark does not already produce. |
| 38 Time | `TaskResult.duration` is recorded for every bench run already; a standalone latency task measures the same field in isolation with no new architectural signal. |
| 39 Retry Count | Retry telemetry is a tool-layer field (ADR-0033 exponential backoff), recorded in `TaskResult` per run. A standalone task does not exercise a different feature. |
| 40 Human Intervention | The bench harness is headless and auto-approves all tool calls — "human intervention" is always zero by construction. The task cannot produce a non-zero value without breaking the harness contract. |
| Fix Compilation Error (unmapped) | Subsumed by the existing slot-16 tasks (`fix_borrow_error.toml`, `fix_lifetime_error.toml`) which already exercise compilation-error fixing. A duplicate slot. |

Dropped rows are removed from the "planned" count. They are NOT deleted
from the spec's 40-task taxonomy (ADR-0066 fixes the taxonomy); they are
marked "dropped from bench backlog" so a future maintainer does not
re-open them without a new ADR.

## Consequences

- `docs/TECHNICAL.md` "Planned tasks" table gains a **Triage** column. The
  19 rows reconcile to: 3 implement, 10 deferred (→ ADR-077), 6 dropped.
  The 3 unmapped tasks reconcile to: 1 implement, 2 deferred/dropped and
  are folded into the same table.
- The prose count is fixed (18 → 19) to match the 19 table rows; the
  arithmetic `30 implemented + 19 planned + 3 unmapped = 40`... no longer
  sums to 40 because 6 are dropped. The reconciled statement: 30
  implemented cover 18 slots; 4 implement-backlog; 12 deferred; 6 dropped
  → 40 slots accounted for.
- `state.md` gains a pending entry per deferred group, each naming the
  blocker from this ADR.
- Adding this ADR bumps the ADR count in `docs/TECHNICAL.md` (93 → 94).
- No code changes. The drift test (`adr_xref_drift`) enforces the new
  ADR's index row ↔ file header agreement.

## Notes

- The triage is a one-time classification. A future WO that ships one of
  the "implement" tasks will move its row out of "Planned" into the
  implemented mapping table and update the count — the same drift
  discipline ADR-0066 established.
- "Dropped" is reversible only by a new ADR that supersedes this one's
  drop verdict for a specific row — the same bar ADR-0066 set for
  superseding any accepted ADR.