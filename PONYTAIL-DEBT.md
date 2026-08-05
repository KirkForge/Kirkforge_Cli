# Ponytail Debt Ledger

Updated 2026-08-05. Every `ponytail:` marker that names a deliberate simplification (ceiling) in production code. Test pins and ADR references are excluded — they're assertions, not debt.

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
| `crates/kf-budget-cli/src/recent.rs:47` | Rewrite whole file on append | upgrade to append-with-rollover if RECENT_BOUND exceeds 100 |
| `crates/kf-budget-core/src/test_support.rs:14` | Process-global reentrant mutex | upgrade to per-key locking if test parallelism matters |
| `crates/kf-draw-core/src/palette.rs:70` | Hard-coded color table | upgrade to registration system if custom brand colors needed |
| `crates/kf-draw-core/src/palette.rs:106` | Substring + prefix sort | upgrade to fuzzy-matcher if palette exceeds 50 entries |
| `crates/kf-draw-core/src/state/mod.rs:123` | O(n) layer scan | upgrade to HashMap index if layer count exceeds 100 |
| `crates/kf-draw-core/src/text_util.rs:127` | Ceil-only line stacking | upgrade to word-wrap if multi-line text editing needed |
| `crates/kf-draw-core/src/text_util.rs:177` | Byte index, not grapheme | upgrade to grapheme offsets if CJK input needed |
| `crates/kf-draw-core/src/text_util.rs:212` | Byte index, not cell | upgrade to cell offsets if wide characters needed |
| `crates/kf-draw/src/app.rs:204` | Byte index, not grapheme | upgrade to grapheme offsets if non-ASCII editing needed |
| `crates/kf-draw/src/event/mod.rs:2154` | Hardcoded brush list | upgrade to dynamic registry if extensible brushes needed |
| `crates/kf-video/src/lib.rs:381` | Append-only decision log | upgrade to seek-based tail if log exceeds 10 MB |
| `crates/kf-video/src/lib.rs:445` | Shell out to ffmpeg | upgrade to libav FFI if shell-out latency matters |
| `crates/kf-video/src/tools/doctor.rs:39` | Shell out to ffmpeg | upgrade to libav FFI if shell-out latency matters |

## Verified (no change needed — test pins, ADR references, or ceiling is appropriate)

All `ponytail:` markers in test files (`*_test*.rs`, `tests/`) and ADR reference comments. These pin specific behavior for regression testing or reference spec sections. Their ceiling is the test assertion itself.

## Summary

5 code fixes implemented (semaphore, config fields, JS grammar).
16 comment upgrades (trigger text added).
26 items verified (test pins and ADR references, no change needed).
0 items remaining with no trigger.