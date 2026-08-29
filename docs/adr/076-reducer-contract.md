# ADR-076: Reducer contract — fold verification state into `DelegationResult.packet`

<!-- adr-predicates
status: accepted
implemented: true
supersedes: []
affects-crates: []
-->

- **Status:** Accepted
- **Date:** 2026-08-20
- **Related:** [ADR-075](./075-emission-final-assistant-message.md) (the
  `ModelClient` seam the reducer's host pipeline runs on),
  [ADR-050](./050-plugin-system-consolidation.md) (the folded plugin system —
  the external-linter boundary why lint/types have no in-crate producers)

## Context

`DelegationResult.packet` (type
`kf_orchestrator::routing::correction::ReducedStatePacket`)
has been `None` on every delegation since WO 29.7: `delegate.rs` and the mode
finalizers hard-code `packet: None`, and three crate doc-lines call the
reducer "NOT ported". The original was TS (`reducer.ts` +
`orchestrator-verifiers.ts`), but **no TS source exists in this repo**
(deleted in WO 29.9) — there is nothing to port. What does exist:

- the full state vocabulary in `kf_orchestrator::routing::correction`
  (`Changes`,
  `GraphState`, `LintState`, `TypesState`, `SecurityState`, `Verification`,
  `OverallVerdict`, `VerifierPolicy`),
- `kf_orchestrator::verifier::scan_files` producing `SecurityFinding`s
  (14 arbitrary-code-execution rules, all critical),
- written-file metadata on the delegation's `files.written` /
  `artifact.emitted` signals (`extract_emission_files`).

The missing piece is the **fold**: many verification results → one packet.
This ADR is the design document (design-from-contract, not a port).

## Decision

### Inputs

The reducer runs per delegation with:

- `task_id` and `ts` — identifying metadata for the serialized packet.
- `turn` — **always 0**: each `Orchestrator::delegate` call is one
  delegation; loop-iteration context lives in the correction loop (which
  bumps `task_id` per re-delegation), not in the packet.
- `cwd` — the delegation's working directory. Written-file paths in signals
  are **relative** (the mode executors persist `name`, not the joined path);
  the reducer joins them against `cwd` before scanning so the security scan
  actually reads the files.
- the `DelegationResult` itself — changes come from its written-file
  signals, security from scanning those files.

### Fold rules

`Changes` ← written-file signals: `files_changed` = count, `paths` = the
paths. `insertions`/`deletions` stay `0` — signals carry hashes and byte
counts, not line deltas.

`SecurityState` ← `scan_files` over the resolved written paths, mapped via
`apply_security_findings` (every finding critical).

`LintState`, `TypesState`, `GraphState` ← **default (all zeros)**. No
in-crate producers exist: external linters stay external subprocesses
(ADR-050) and no import-graph verifier has been ported. The fold rules below
 nevertheless cover the full vocabulary, so a future producer only fills its
 state — the fold needs no change.

`OverallVerdict` ← `fold_overall(lint, types, security)`:

1. **Fail** if `security.critical > 0` OR `lint.errors > 0` OR
   `types.errors > 0` — any error-class category fails the delegation.
2. else **Warn** if `security.findings > 0` OR `security.high > 0` OR
   `lint.warnings > 0` — non-error findings degrade without failing.
3. else **Pass** — including the **empty case**: a delegation with no
   findings (and no files at all) folds to `Pass`, not `Unknown`.

The reducer **never emits `Unknown`**: `Unknown` remains only the
`Default` value of a packet nobody reduced (e.g. hand-built test packets).
Empty→Pass is deliberate: before the reducer, a packet-less clean delegation
left `overall = Unknown`, so `decide_correction` returned `Correct` on every
turn until exhaustion — a clean delegation must be acceptable.

`verifier_policy` stays `None` (= the no-policy default, every slot
Required — the same backward-compat default `decide_correction` documents).

### Where it runs

In `Orchestrator::delegate`, **after mode execution** (the `match` over
`DelegationMode`) and **before** the sink flush — one site covering all four
modes, including the synthetic task-decompose result (whose `packet: None`
at `delegate.rs:269` this replaces). It does not run inside the correction
loop: the loop consumes the packet the delegate produced.

### What correction consumes

`run_correction_loop` keeps its existing packet path — no logic change:
`result.packet` flows into `last_packet`, the loop's own verify-cycle re-scan
(`apply_security_findings`, R7/WO 32.19) re-applies on top, and
`decide_correction` reads `verification.overall` (Accept iff `Pass`),
`verification.security.critical` (Escalate when > 0 and the security slot is
Required — the default), and `verification.lint.errors` (correction-prompt
text); `compute_final_verdict` reads `verification.overall`.

## Consequences

- Clean delegations now accept on turn 0 (overall `Pass`) instead of
  cycling `Correct` until exhaustion. This is the point of the reducer.
- `execute_decomposition` subtask verdicts become real (`pass` instead of
  `unknown`) whenever its `DelegateFn` wraps `Orchestrator::delegate`.
- The loop's re-scan is idempotent when its process cwd equals the
  delegation cwd (the normal case). When they differ, the loop's scan of
  relative paths finds nothing and zeroes its copy of `security` while
  `overall` stays `Fail` — decision actions are unchanged
  (correct-then-escalate) and the packet on the `DelegationResult` retains
  the delegate-recorded findings. Fixing the loop's path resolution is
  deferred with it (it has no cwd in scope).
- Deterministic lint/types/graph producers and a real correction-prompt
  template remain unported; until they ship, those categories stay at
  default and the fold rules for them are contract, not exercised state.
- The binary's `plugin_verify_workspace` deferral message ("reducer not
  ported") is now stale in its *reason* — the reducer exists — but the tool
  is still unwired to it; wiring it is follow-up work outside WO 37.2.

## Amendment (2026-08-29) — crate paths post-47.4 fold

WO 47.4 folded the `kf-routing` crate into `kf-orchestrator`. The two
crate paths cited above were renamed in place: the packet type and the
state vocabulary now live at
`kf_orchestrator::routing::correction`
(`crates/kf-orchestrator/src/routing/correction.rs`). Path rename only —
no contract change.
