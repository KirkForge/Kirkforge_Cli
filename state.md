# KirkForge-Cli Production-Readiness State

Generated: 2026-06-23 22:50 UTC

## Quality gates (last run)

- `cargo test` — 817 passed; 7 integration tests ignored (need Ollama).
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

## Remaining gaps (prioritized for tomorrow)

### P1 — safety / permissions (must fix before production)

1. ~~**Background jobs bypass bash safety gates.**~~

2. ~~**Permission rule matching is exact string only.**~~
   - Deny rules now get prefix semantics and lone `*` promotion to `**`; Allow/Ask rules stay anchored to avoid authorizing chained commands across path separators. Word-boundary matching for Allow rules remains future work.

3. ~~**`grep`/`glob` lack per-file PathGuard checks.**~~

4. ~~**`edit_file` replaces only the first occurrence.**~~

5. **`PathGuard` default is fail-open.**
   - `PathGuard::default()` has `sandbox_dir: None` and `allowed_write_dirs: vec![]`, so writes are only restricted by deny-list and extensions.
   - This is intentional but dangerous; `warn_if_unsandboxed` only logs a `tracing::warn!`, which the TUI user never sees.
   - Fix: surface the unsandboxed warning in the TUI startup banner and add a one-time system message.

6. **`read_image` may not honor `max_read_size` / binary guard.**
   - Need to verify `src/tools/read_image.rs` applies `PathGuard::check_read` before loading pixels.

### P2 — robustness / edge cases

7. **Bash drain can wedge if `join_drain` stalls.**
   - `run_shell` joins drain tasks with a fixed 10 s timeout only inside `join_drain`? Actually `join_drain` awaits directly; the timeout is only on the timeout path.
   - On normal exit, a misbehaving child that does not close its stdout could block the join forever.
   - Fix: always wrap drain joins with `tokio::time::timeout`.

8. **TUI `keys.rs` has unchecked `unwrap()` on `chars().next()`.**
   - `src/tui/keys.rs:78`, `:87`, `:96` assume a character exists at the found position; positions come from `rfind`, so they should be valid, but the code should use safe indexing or explicit asserts.
   - Risk is low but these are in the hot input path.

9. ~~**Tracing file-layer fallback panics if `/dev/null` is unavailable.**~~

10. **Read-before-edit canonicalization fallback is unsafe?**
    - `ReadGate::mark_read` and `was_read` fall back to the unresolved literal path. A symlink outside the sandbox could be read, then the edit path canonicalized differently.
    - Verify the gate only canonicalizes after the PathGuard check_read returns the canonical path, and that `mark_read` uses that canonical path.

11. ~~**`/save` slash command writes outside PathGuard.**~~

### P3 — UI polish / docs

12. **Tool-card search expansion not implemented.**
    - Pressing `Enter` on a search match should expand the collapsed tool card and scroll to the match; currently search only highlights in already-rendered text.

13. **No copy-to-clipboard keybinding for code blocks.**
    - Users expect a key (e.g. `c` over a focused code block or `Ctrl+Shift+C` in code-block context) to copy a single code block.

14. **Streaming render still re-highlights on every token.**
    - The chat render cache helps, but search-highlight invalidation during streaming could be cheaper. Verify we only recompute spans when the content actually changes.

15. **Docs drift.**
    - `review.md` numbers are stale (unit test count, gap list).
    - `README.md` mentions `/model`, `/compact`, but not `/save` or `/commit --push` details.
    - No ADR for the subagent persona subsystem, background bash jobs, undo stack, or `/save` transcript feature.

## Tomorrow’s recommended order

1. P1 `PathGuard` fail-open warning: surface unsandboxed mode in the TUI startup banner and a one-time system message.
2. P1 verify `read_image` honors `max_read_size` / binary guard via `PathGuard::check_read`.
3. P2 bash drain wedging: always wrap drain joins with `tokio::time::timeout`.
4. P2 safe indexing in `src/tui/keys.rs` hot path (replace `unwrap()` with explicit asserts or safe indexing).
5. P2 read-before-edit canonicalization audit: ensure `ReadGate` records the canonical path returned by `PathGuard::check_read`.
6. Update `review.md`, `README.md`, and add/update ADRs for the recent changes.
7. Pick a live regression target outside this repo and track it in that repo’s own `state.md`.
