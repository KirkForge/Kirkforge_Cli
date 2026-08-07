# Ponytail Debt Ledger

Updated 2026-08-07. Every `ponytail:` marker that names a deliberate simplification (ceiling) in production code. Test pins and ADR references are excluded — they're assertions, not debt.

## Upgraded (code fix implemented)

| File:Line | Simplified | Fix |
|---|---|---|
| `crates/kf-workflow/src/lib.rs:113` | `max_parallel` stored but not enforced | ✅ Semaphore-gated parallel fan-out |
| `crates/kf-workflow/src/lib.rs:750` | Fan-out runs sequentially | ✅ `tokio::spawn` + `Semaphore` with `max_parallel` cap |
| `src/tui/mod.rs:570` | Hardcoded 3s timeout | ✅ `DEFAULT_SHUTDOWN_TIMEOUT_SECS` const + config `shutdown_timeout_secs` |
| `src/session/executor/turn.rs:1473` | Hardcoded 4 KiB cap | ✅ `STEM_FILE_CAP` const + config `stem_file_cap` |
| `src/shared/minify/lang.rs:79` | JS uses TSX grammar | ✅ Dedicated `tree-sitter-javascript` grammar |

## Trigger Added (comment upgraded with explicit upgrade path)

| File:Line | Ceiling | Trigger |
|---|---|---|
| `src/session/session_index.rs:304` | Append-only file, no index | upgrade to indexed lookup if alerts exceed 10k per session |
| `src/session/verifier/git.rs:71` | Shell-out to git | upgrade to libgit2 if shell-out latency or PATH fragility matters |
| `src/adapters/auth.rs:31` | Keychain stub returns None | upgrade to keyring crate in Series 18 |
| `crates/kf-budget-core/src/test_support.rs:14` | Process-global reentrant mutex | upgrade to per-key locking if test parallelism matters |

## Verified (no change needed — test pins, ADR references, or ceiling is appropriate)

All `ponytail:` markers in test files (`*_test*.rs`, `tests/`) and ADR reference comments. These pin specific behavior for regression testing or reference spec sections. Their ceiling is the test assertion itself.

## Summary

5 code fixes implemented (semaphore, config fields, JS grammar).
4 comment upgrades (trigger text added).
26 items verified (test pins and ADR references, no change needed).
0 items remaining with no trigger.