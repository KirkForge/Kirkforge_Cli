# KirkForge-Cli — polish / debug status

> Updated: 2026-06-23 after a full TUI/UI/Permissions hardening pass and local install of the polished binary.

## Quality gates

- `cargo test` → **812 passed**, 0 failed, 1 ignored; 7 integration tests ignored (need Ollama).
- `cargo clippy --all-targets -- -D warnings` → **clean**.
- `cargo build --release` → **clean**, binary installed to `~/.cargo/bin/kirkforge`.

## Recently completed

### TUI/UI/Permissions hardening pass (this session)
1. **Ctrl+W word-backward deletion** — fixed leading-whitespace bug and extracted a tested helper in `keys.rs`.
2. **`/memory` multi-byte safety** — `truncate_to_char_boundary` now never splits UTF-8 characters when truncating names/descriptions.
3. **`/save` relative filename support** — relative paths like `/save chat.md` no longer fail; parent directories are created on demand.
4. **`!` timeout preserves partial output** — `format_bang_output` keeps the stdout/stderr captured before the timeout and strips the duplicate runner prefix.
5. **Search navigation modifier checks** — `n` / `Shift+N` no longer fire with extra modifiers held.
6. **Recent-sessions hint in machine-readable modes** — suppressed in JSON/StreamJson output.
7. **Tool sidecar output restored on reload** — `messages_to_entries` rebuilds `ConversationEntry::tool(summary, full)` correctly for `/resume`, `/fork`, and persona merges.
8. **DSML tool-call fallback** — DeepSeek cloud models now get a regex fallback when the provider omits structured tool calls.
9. **Non-interactive approval bypass, stdout flush, broken system prompt template** — fixed the three CLI/permissions/prompt regressions.

### Earlier work still valid
10. Session daemon with JSON-RPC over Unix socket, `--auto-resume`, `--attach`, and TUI session picker.
11. `/compact`, `/undo`, `/model`, `/fork`, `/resume`, `/jobs`, `/status`, `/test`, `/plan`, `/explore`, `/coder`, `/commit`, `/sessions`, `/memory`, `/gh`, `/init`.
12. Diff preview for edit approvals, search with highlighting, tool collapse (Ctrl+T), carryover profiles, transcript export `/save`.

## Next focus: TUI chat rendering

Most of the original "messy daily coding UI" complaints have been addressed in this pass. Current status of the items from the previous plan:

- ✅ Assistant markdown now uses `pulldown-cmark` and supports headings, paragraphs, lists, links, code blocks, inline styles, blockquotes, rules, and task-list markers.
- ✅ Message headers collapsed to a single subtle prefix line with color-coded role badges; timestamps are dropped for messages within the same minute.
- ✅ Tool cards are compact `Block`-based cards with collapse/expand via Enter/Ctrl+T.
- ✅ Code blocks have a language badge, dim soft-yellow background, and lightweight syntax highlighting.
- ✅ Connection banner is hidden once connected.
- ⏳ Streaming stability is still open: each token append re-wraps the whole conversation.
- ✅ Thinking block is inlined under the latest assistant message.

### Additional hardening done in this pass
8. ✅ **Search-highlight Unicode safety** — `highlight_spans`/`highlight_line_spans` now map folded offsets back to original byte boundaries so `İstanbul` searches don't slice mid-character.
9. ✅ **Chat render cache invalidation** — cache key is now a hash of `content + tool_output`, not byte length, so different same-length messages no longer collide.
10. ✅ **`@`-mention multi-byte truncation** — head/tail slices are aligned to character boundaries.
11. ✅ **`/gh file` shell-injection risk** — removed `bash -c` interpolation entirely; uses native `gh api` args with the raw-content media type.
12. ✅ **`git status` byte cap** — status capture now uses byte-safe truncation instead of character counting.
13. ✅ **Symlink guard ordering on read** — symlinks are rejected before canonicalizing the target, so the denial reason is accurate and sandbox checks run on the real path.

### Proposed remaining plan
1. **Streaming stability** (`src/tui/widgets/chat.rs`): cache wrapped geometry per message so appending a token only recomputes the last few lines instead of the whole conversation.
2. **Tool card search expansion** (`src/tui/search.rs` + `chat.rs`): auto-expand a collapsed tool card when the user navigates to a match that lives in `tool_output`.
3. **Copy keybinding for code blocks** (`src/tui/widgets/chat.rs`): add a `c` keybinding over a focused code block to copy its contents (follow-up after the internal copy buffer exists).
4. **Error recovery polish** (`src/tui/app.rs`): if the model stream errors mid-turn, surface a clean retry prompt rather than dumping raw adapter output.
5. **Visual spacing pass** (`src/tui/widgets/chat.rs`): reduce duplicate blank lines between entries and fine-tune padding around tool cards/thinking blocks.

## Remaining joblist

1. **TUI chat polish** — see remaining plan above. Next concrete file: `src/tui/widgets/chat.rs` for streaming stability.
2. **Cross-compilation** — `rustup target add` done for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, but a musl host linker or `cross` is still required. Blocked on user approval to install/use one of those tools.
3. **PetSense live regression run** — blocked on `gh auth login`; Ollama is available.
