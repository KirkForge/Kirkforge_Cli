# kf-code Repo State

*Current state only. History lives in `git log` and WO files.*

## Pending / Deferred

- **WO 39.1 Phase 3-4**: external runner for `claude -p`/`codex exec`/`opencode run` + same-model LiteLLM gateway. Tracked in [39.1](docs/workorders/39.1-bench-cross-tool.md).
- **WO 39.4**: Claude compat phase 3 (hook stdin-JSON contract + generic pre/post-tool events). Deferrable.
- **WO 38.9 items 4-6**: memory mtime-cache, prompt-cache stem stability, (path,mtime) minified cache, trace delta-only. Tracked in [38.9](docs/workorders/38.9-session-performance.md).
- **WO 38.10 P2s**: `--read-stdin-full` flag, JSON error-object emission, session.id in summary, replay/Ctrl-C/CLI polish. Tracked in [38.10](docs/workorders/38.10-cli-first-run.md).
- **WO 38.4 item 3**: Esc-then-input window — landed in WO 38.5 (loop_.rs), noted in [38.4](docs/workorders/38.4-orchestration-correctness.md).

## Known flakes (pre-existing, not introduced this session)

- `same_ms_double_spawn_gets_distinct_{temp_dirs,worktrees}` — real-concurrency git tests, `#[cfg(unix)]` gated, flake under extreme parallel load. Pass in isolation.

## Architecture notes (load-bearing, not in WOs)

- `panic = "abort"` in release — panic hook (WO 38.2) restores terminal before abort. Keep abort (binary size); don't switch to unwind without measuring.
- Budget guard wired in production (WO 38.8) — `set_budget_stores` + `set_stratum_store` called from `run_session.rs`. Listener registry is session-keyed `HashMap`, not the old append-only Vec.
- Windows cross-compile gate in `scripts/ci-local.sh` — `cargo clippy --target x86_64-pc-windows-gnu` runs before every push. AGENTS.md §4 enforces it. This is the structural fix for the 25+ `fix(windows)` commit pattern.
- WO drift test in `kf-budget-core/tests/adr_xref_drift.rs` — enforces WO file header ↔ README index agreement. Prevents future status drift.
- `.config/nextest.toml` profiles: `ci-fast` (30s, fail-fast), `ci-full` (60s), `nightly` (600s). CI references by name, no inline `--config`.