# KirkForge-Cli Production-Readiness State

Generated: 2026-06-23 22:50 UTC

## Quality gates (last run)

- `cargo test` — 819 passed; 7 integration tests ignored (need Ollama).
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo build --release` — clean; binary at `target/release/kirkforge`.

## Recently landed TUI/UI/Permissions fixes

1. @-mention truncation is now multi-byte safe (`truncate_to_char_boundary` reused across modules).
2. Search highlighting is now Unicode case-folding safe (folded offsets map back to original byte boundaries).
3. `/gh file` no longer builds shell strings; it invokes `gh api` natively.
4. Chat render cache keys moved from message index to `u64` hash of `(content, tool_output)`.
5. Git status in `/commit` is byte-capped safely using `truncate_to_char_boundary`.
6. PathGuard `check_read` now rejects symlinks before canonicalization so symlinks outside the sandbox cannot influence the sandbox check.

## Today’s fixes (P1/P2 safety + robustness)

- [x] **Background bash jobs now go through the same safety gate.** `BashJobRegistry::spawn` calls `check_bash_command_str` and validates `workdir` with `PathGuard` before spawning.
- [x] **Per-file `PathGuard` checks in `grep`/`glob`.** Both walkers now call `path_guard.check_read` / `check_traversal` on every result before returning it.
- [x] **`edit_file` rejects ambiguous matches.** If `old_string` matches more than once (exact or normalized fuzzy), the tool returns an error instead of silently editing the first hit.
- [x] **Bash permission deny rules get prefix semantics.** A `Deny` rule like `rm -rf /` now blocks `rm -rf /home` and `rm -rf /; echo`. Allow/Ask rules stay anchored, and lone `*` is promoted to `**` only for `Deny` rules.
- [x] **Tracing fallback no longer panics on missing `/dev/null`.** `init_tracing` uses a `LogWriter` that falls back to `std::io::sink()`.
- [x] **`/save` slash command now checks `PathGuard::check_write`.** Writes outside the sandbox/deny-list are rejected.
- [x] **Safe indexing in TUI `keys.rs` hot path.** Replaced `unwrap()` after `rfind` with `let-else` fallbacks in `delete_word_backward`.
- [x] **ReadGate uses the resolved canonical path for edit checks.** `ReadGate::check_edit` now takes the resolved path from `PathGuard`, avoiding a second canonicalization round and fallback mismatch.
- [x] **Flaky executor test timeout relaxed.** `test_always_approve_rule_round_trips_to_next_turn` now uses a 5-second timeout instead of 300 ms so it no longer flakes under parallel test load.
- [x] **Docs and ADRs updated.** `review.md` and `README.md` refreshed with current capabilities and test count; added ADRs 014 (background bash jobs), 015 (undo stack), and 016 (`/save` transcript).

## Remaining gaps (prioritized for tomorrow)

### P1 — safety / permissions (must fix before production)

1. ~~**Background jobs bypass bash safety gates.**~~

2. ~~**Permission rule matching is exact string only.**~~
   - Deny rules now get prefix semantics and lone `*` promotion to `**`; Allow/Ask rules stay anchored to avoid authorizing chained commands across path separators. Word-boundary matching for Allow rules remains future work.

3. ~~**`grep`/`glob` lack per-file PathGuard checks.**~~

4. ~~**`edit_file` replaces only the first occurrence.**~~

5. ~~**`PathGuard` default is fail-open / unsandboxed warning missing.**~~
   - `PathGuard::default()` remains fail-open by design, but the TUI now surfaces the unsandboxed posture via a startup banner, a system message, and a status-bar indicator.

6. ~~**`read_image` may not honor `max_read_size` / binary guard.**~~
   - Confirmed: `dispatch_tool_call` applies `PathGuard::check_read` before the tool runs; `read_image` executor test added as a regression guard.

### P2 — robustness / edge cases

7. ~~**Bash drain can wedge if `join_drain` stalls.**~~
   - `join_drain` now wraps its `handle.await` with `tokio::time::timeout(Duration::from_secs(5))`.

8. ~~**TUI `keys.rs` has unchecked `unwrap()` on `chars().next()`.**~~
   - Replaced with `let-else` fallbacks in `delete_word_backward`.

9. ~~**Tracing file-layer fallback panics if `/dev/null` is unavailable.**~~

10. ~~**Read-before-edit canonicalization fallback is unsafe?**~~
    - `ReadGate::check_edit` now takes the resolved canonical path from `PathGuard`, eliminating the second canonicalization and fallback mismatch.

11. ~~**`/save` slash command writes outside PathGuard.**~~

12. ~~**Flaky executor test under parallel load.**~~
    - `test_always_approve_rule_round_trips_to_next_turn` timeout increased from 300 ms to 5 s; full suite now passes.

### P3 — UI polish / docs

13. **Tool-card search expansion not implemented.**
    - Pressing `Enter` on a search match should expand the collapsed tool card and scroll to the match; currently search only highlights in already-rendered text.

14. **No copy-to-clipboard keybinding for code blocks.**
    - Users expect a key (e.g. `c` over a focused code block or `Ctrl+Shift+C` in code-block context) to copy a single code block.
    - Note: `Ctrl+Shift+B` already copies the most recent assistant code block; per-block focus is not yet implemented.

15. ~~**Docs drift.**~~
    - `review.md` test count and capability list updated.
    - `README.md` updated with `/save`, permission-rule semantics, and `/commit --push` details.
    - ADRs 014, 015, 016 added for background bash jobs, undo stack, and `/save` transcript.

## Remaining recommended work

1. P3 tool-card search expansion and code-block copy keybinding.
2. Pick a live regression target outside this repo and track it in that repo’s own `state.md`.
