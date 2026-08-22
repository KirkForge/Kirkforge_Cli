# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Pending / Deferred

- **WO 41.1-41.5**: PipelineOrchestrator rename + patch apply (41.1), handoff escaping
  (41.2), permission docs (41.3), verifier capability UI (41.4), subagent persistence
  (41.5). All Pending — documented in WO 42.8-42.10 as they overlap.
- **WO 38.9 items 2-6**: verifier coalescing, token cache, context index, memory store
  cache, prompt stem stability. ~30% done (item 1 shipped). Tracked in [42.6].
- **WO 39.1 Phase 3-4**: external runner + same-model LiteLLM gateway. Deferred.
- **WO 42.1-42.12**: post-30h review findings. See [42.0](docs/workorders/42.0-wo42-overview.md).

## Known flakes (pre-existing, not introduced this session)

- `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}` — real-concurrency git tests,
  `#[cfg(unix)]` gated, flake under extreme parallel load. Pass in isolation.

## Architecture notes (load-bearing, not in WOs)

- `panic = "abort"` in release — panic hook (WO 38.2) restores terminal before abort.
  Keep abort (binary size); don't switch to unwind without measuring.
- Budget guard wired in production (WO 38.8) — `set_budget_stores` + `set_stratum_store`
  called from `run_session.rs`. Listener registry is session-keyed `HashMap`, not the
  old append-only Vec.
- Windows cross-compile gate in `scripts/ci-local.sh` — `cargo clippy --target
  x86_64-pc-windows-gnu` runs before every push. AGENTS.md §4 enforces it. This is the
  structural fix for the 25+ `fix(windows)` commit pattern.
- WO drift test in `kf-budget-core/tests/adr_xref_drift.rs` — enforces WO file header ↔
  README index agreement. Prevents future status drift.
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s),
  `nightly` (600s). CI references by name, no inline `--config`.