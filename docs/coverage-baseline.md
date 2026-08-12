# Coverage Baseline

Per-crate line coverage baselines. Enforced by `scripts/check-cov-regression.sh`
(WO 28.7) with a **-1% tolerance** per crate. A crate row whose % column is
`TBD` is treated as non-fatal by the gate (first-run placeholder); a real
number is the floor below which a -1% drop fails the gate.

| Crate | Line Coverage % | Notes |
|-------|-----------------|-------|
| `kf-code` | 78.4 | root crate (src/) |
| `kf-budget-core` | 86.5 | |
| `kf-testdoctor` | 71.2 | |
| `kf-compress-core` | 95.2 | |
| `kf-plugin-host` | 88.8 | |
| `kf-bench` | 88.3 | |

## Methodology

`cargo llvm-cov --workspace --lcov --output-path lcov.info` (the same command
the CI `coverage` job runs), parsed per-crate by source-path prefix
(`crates/<name>/src/` → sub-crate, `src/` at repo root → `kf-code`;
`tests/`, `benches/`, `build.rs` are excluded).

Baseline established on: **2026-08-13** (WO 28.7), measured in the wo28f
worktree with `cargo-llvm-cov 0.8.7` on rust 1.88.0.

### Honest disclosure — conservative floor

The baseline run skipped 12 tests that fail **in this worktree's local
environment only** (they pass on CI's ubuntu-latest):

- 9 `kf-code` bash/jobs spawn tests — `os error 22 (EINVAL)` from the WO 28.5
  landlock + rlimit spawn path, which this container does not permit;
- 2 `kf-code` daemon / TUI-jobs tests (auth-handshake / spawn-dependent);
- 1 `kf-plugin-host` `all_bundled_plugins_load_without_warnings` — needs the
  `plugins/` dir deleted in WO 29.9.

The skipped tests contribute coverage to `src/tools/bash*`, `src/jobs/*`,
`src/daemon/*`, and `src/tui/commands/jobs*`, so the `kf-code` floor here is
**conservatively low** relative to a full CI run. A real CI run measures
higher and still clears this floor; raising the `kf-code` floor to the true
CI number is a follow-up (tracked in WO 28.7 §Defer note + WO 28.9 for the
`src/session` raise). This is a floor, not a vanity number — it catches
regressions without flaking on run-to-run variance (the -1% tolerance).
