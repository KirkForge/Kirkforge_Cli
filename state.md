# KirkForge-Cli Production-Readiness State

Generated: 2026-06-23 22:50 UTC

## Quality gates (last run)

- `cargo test` — 812 passed; 7 integration tests ignored (need Ollama).
- `cargo clippy --all-targets -- -D warnings` — clean.
- `cargo build --release` — clean; binary installed to `~/.cargo/bin/kirkforge`.

## Recently landed TUI/UI/Permissions fixes

1. @-mention truncation is now multi-byte safe (`truncate_to_char_boundary` reused across modules).
2. Search highlighting is now Unicode case-folding safe (folded offsets map back to original byte boundaries).
3. `/gh file` no longer builds shell strings; it invokes `gh api` natively.
4. Chat render cache keys moved from message index to `u64` hash of `(content, tool_output)`.
5. Git status in `/commit` is byte-capped safely using `truncate_to_char_boundary`.
6. PathGuard `check_read` now rejects symlinks before canonicalization so symlinks outside the sandbox cannot influence the sandbox check.

## Remaining gaps (prioritized for tomorrow)

### P1 — safety / permissions (must fix before production)

1. **Background jobs bypass bash safety gates.**
   - `BashJobRegistry::spawn` in `src/session/bash_jobs.rs` runs `sh -c <command>` directly without calling `check_bash_command_str` or applying the `DenyList`/`PathGuard`.
   - Also no `workdir` sandbox containment or symlink guard for the job’s working directory.
   - Fix: route `bash` with `background: true` through the same `check_bash_command` gate, then validate `workdir` with `PathGuard`, before spawning.

2. **Permission rule matching is exact string only.**
   - `src/shared/permission.rs` matches command strings literally; `rm -rf /home/foo` does not match a rule for `rm -rf /`.
   - This makes allow/ask/deny rules brittle and easy to bypass with extra arguments or different quoting.
   - Fix: add glob/prefix/word-boundary matching for command rules (start with prefix or word-boundary match).

3. **`grep`/`glob` lack per-file PathGuard checks.**
   - `src/tools/grep.rs` and `src/tools/glob.rs` walk the filesystem but only check extension/size and gitignore; they do not invoke `PathGuard` per file.
   - This lets a model discover paths inside `~/.ssh`, `/etc`, or outside the sandbox.
   - Fix: call `path_guard.check_read` on every file before returning/reading it.

4. **`edit_file` replaces only the first occurrence.**
   - `content.replacen(&old, &new, 1)` can apply to the wrong occurrence if the same substring appears earlier in the file.
   - Models already include context; we should fail if the match is ambiguous rather than silently edit the first hit.
   - Fix: if `old` occurs more than once, require unique match or use line-boundary disambiguation.

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

9. **Tracing file-layer fallback panics if `/dev/null` is unavailable.**
   - `src/main.rs:48` uses `.expect("/dev/null open")` on a fallback file open. On a sandboxed or Windows environment this can crash startup.
   - Fix: use `std::io::sink()` instead of opening `/dev/null`, or wrap in a safe fallback chain.

10. **Read-before-edit canonicalization fallback is unsafe?**
    - `ReadGate::mark_read` and `was_read` fall back to the unresolved literal path. A symlink outside the sandbox could be read, then the edit path canonicalized differently.
    - Verify the gate only canonicalizes after the PathGuard check_read returns the canonical path, and that `mark_read` uses that canonical path.

11. **`/save` slash command writes outside PathGuard.**
    - `src/tui/commands/save.rs` writes a transcript file without any sandbox or deny-list check. A typo or malicious argument could write anywhere the user can write.
    - Fix: run the resolved path through `PathGuard::check_write` or at least require it to be under the project root / home directory.

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

1. Fix P1 background-job safety gate (touches `src/session/bash_jobs.rs` + `src/tools/bash.rs`).
2. Fix P1 `grep`/`glob` PathGuard checks.
3. Fix P1 permission rule prefix/word-boundary matching.
4. Address P2 tracing fallback panic and `/save` sandboxing.
5. Update `review.md`, `README.md`, and add/update ADRs.
6. Pick a live regression target outside this repo and track it in that repo’s own `state.md`.
