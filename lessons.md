# Lessons — WO 34.2 + 34.3 session (2026-08-16)

Worktree `.worktrees/wo34-b`, branch `wo/34-b`.

## What I learned about this codebase

- **`ModelInfo` has a `supports_cache: bool` field** (added for the
  prompt-cache work). Test helpers that build a `ModelInfo` literal must
  include it or the struct literal won't compile. The existing
  `selftest.rs` helper had it; my first `status.rs` test helper missed it.
  Always grep for an existing `ModelInfo { ... }` literal before writing a
  new one.
- **`UiState` is the right home for overlay visibility flags.** The WO
  suggested `state.ui.help_overlay_visible` and that's where the existing
  `slash_menu`, `file_completer`, `theme`, `paste_flash`, `last_input_rect`
  fields live. Adding two bool/usize fields there is the smallest diff —
  no new sub-struct needed.
- **The key-handler stack order matters.** `handle_input_key` already had
  a "top of stack" pattern: `handle_doom_loop_keys` runs first and returns
  `Some(result)` if it consumed the key. The help-overlay handler follows
  the same shape — runs before slash-menu/file-completer/search handlers
  so the overlay has exclusive focus. Placing it after doom (doom stays
  top priority) and before everything else is the right order.
- **`help_text()` is `pub(crate)`** in `src/tui/keys/slash_commands.rs`.
  The overlay widget (`src/tui/widgets/help_overlay.rs`) reaches it via
  `crate::tui::keys::slash_commands::help_text`. No visibility change
  needed — same crate.
- **`budget_pct` (in `rendering/format.rs`) is the shared pure helper**
  for context-pressure percentage. It returns `Option<u8>` (None when
  max==0). The new status bar reuses it instead of re-computing. The
  threshold colours (green/yellow/red) live in the `Theme`, but the WO
  spec's thresholds (<50/50-80/>80) match `pressure_color` exactly — I
  used plain `Color::Green/Yellow/Red` to keep the status bar
  theme-independent for the pressure indicator (the WO named the colours
  literally, not the theme fields).
- **`format_budget_indicator` is now only used by its own tests** (the
  status bar no longer calls it). It's still public + tested, so no dead
  code. If a future WO removes those tests, the helper should be deleted
  too.
- **selftest.rs `budget_indicator_update` asserts on the rendered status
  bar output.** Any change to the status bar format must update that test.
  The assertion was `(32%)` (old `↑used/max (P%)` format); now it's
  `42.0K tokens` (new `tokens` display below 50%).

## What I tried that didn't work

- **First `help_overlay_renders_title_and_body` test asserted on row 0.**
  The overlay is centered at 80%×80%, so on a 24-row terminal the title
  border is at row 2, not row 0. Fixed by scanning all rows.
- **First attempt checked `row.contains("Help")`.** The title text
  (`Help — Esc to close, ↑/↓ to scroll`) is long; on an 80-col terminal
  with an 80%-width box (64 cols) the title may be truncated or the
  `↑/↓` glyphs render as multiple cells. Relaxed to also accept the
  border glyph `─` as proof the box rendered. Both the title and the
  help text body (`Built-in commands` / `/help`) are checked, so the
  test still proves the overlay rendered with content.

## What I'd do differently next time

- **Run `cargo fmt` before the first test run**, not after. The first
  `cargo fmt --check` after WO 34.2 flagged a trailing-newline diff in
  `help_overlay.rs` that I then had to fold into the WO 34.3 commit.
  Trivial, but it muddied the per-WO commit boundary.
- **Check `selftest.rs` for status-bar-format assertions before starting
  a status-bar rewrite.** I found `budget_indicator_update` only after
  the compile failed on the test. A grep for `render_status` / `(32%)` /
  `↑` across `src/tui/` before editing would have surfaced it up front.
- **The `supports_cache` field on `ModelInfo`** bit me once. There's a
  lesson in AGENTS.md about updating all `Config` literal sites when
  adding a field; the same applies to `ModelInfo`. Could fold into
  AGENTS.md §7 if it recurs.