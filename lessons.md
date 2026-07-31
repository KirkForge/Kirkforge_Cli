# Lessons — WO 15.13 session (split kirkforge-draw/src/event.rs by event category)

## What I learned about this codebase
- `kirkforge-draw` is a **binary** crate (`[[bin]] name = "kfd"`), not a
  lib. There's no `lib.rs`. `main.rs` declares `mod event;` — Rust
  resolves that to either `event.rs` OR `event/mod.rs`. So the
  directory-form split (event.rs → event/mod.rs + event/tests/*.rs)
  needs NO change to `main.rs` or any consumer. The four public
  items consumed externally (`event::run`, `event::atomic_write`,
  `event::validate_path_arg`, `event::HELP_LINES`) stay at the same
  `crate::event::*` path automatically.
- The `event.rs` production code is **cohesive**, not separable. The
  `handle_key` 650-line `match` calls ~40 private helpers
  (`save_app`, `copy_selected`, `cut`, `paste`, `commit_palette`,
  `dispatch_palette_action`, `cycle_line_style`, `cycle_box_style`,
  `recolor_selection`, `group_selection`, `align_selection`,
  `distribute_selection`, `invert_selection`, `cycle_layer_focus`,
  `clear_layer_focus`, `commit_layer_focus`, `handle_layer_click`,
  `handle_inspector_click`, `mode_from_modifiers`, `marquee_rect`,
  `plural_s`, `status_n_objects_to`, `hit_test_selected_box`,
  `ink_color_for_digit`, `next_ink_color`, `color_name`,
  `next_line_style`, `line_style_name`, `next_box_style`,
  `box_style_name`, `next_text_border`, `text_border_name`,
  `next_brush`, `cycle_brush`, `cycle_text_border`,
  `cycle_selection_color`, `ungroup_selection`, `align_name`,
  `distribute_name`, `palette_align`, `surface_panic`,
  `scroll_app_pages`). Splitting the production helpers into separate
  sub-modules would force every one of those to `pub(super)` + a forest
  of `use` statements in `handle_key`. The WO explicitly anticipated
  this: "If the file is cohesive ... split the tests out at minimum."
  The conservative, behaviour-preserving split is: keep production in
  one `event/mod.rs`, split the 6,000-line test block by category.
  That's what shipped.
- The test block had a `use super::*` + explicit
  `use kirkforge_draw_core::{DrawObject, DrawState, InkColor, Point}`
  + `use crate::app::PaletteTrigger` + `use ratatui::layout::Rect` at
  the top of `mod tests`. When splitting into sub-modules, each sub-
  module's `use super::*` pulls in the production items (re-exported
  via the parent `tests` mod's `use super::*`), but `DrawState` and
  `InkColor` are NOT in the production `use` — they're only imported
  locally inside individual production functions. So sub-modules
  that use `DrawState`/`InkColor` at the top level need their own
  explicit `use kirkforge_draw_core::{DrawState, InkColor}`.
  `DrawObject`, `Point`, `Rect` ARE in the production top-level `use`,
  so they come via `use super::*`. `DrawMode` also (production imports
  it). The `make_app_with_two_boxes`/`make_app_with_findable`/
  `make_app_with_text` helpers use fully-qualified
  `kirkforge_draw_core::DrawState::new()` so they DON'T need a
  `DrawState` import — only the helpers that write `DrawState::new()`
  unqualified (inspector.rs, layers.rs) do.
- `include_str!("event.rs")` in a test is **path-relative to the
  source file it lives in**. When the test moves from
  `crates/kirkforge-draw/src/event.rs` to
  `crates/kirkforge-draw/src/event/tests/keyboard.rs`, the path must
  become `include_str!("../mod.rs")` (the production file moved from
  `event.rs` to `event/mod.rs`). This is the ONE behaviour-affecting
  edit in the whole refactor — documented in the CHANGELOG. A grep
  for `include_str!` in the original file before splitting would have
  caught it; I caught it during the planning read.
- Cross-category test helper dependencies decide what goes in
  `common.rs` vs the category file. The rule: if a helper is used by
  >1 category, it goes to `common`. I found: `make_app`, `key`,
  `key_with_shift`, `key_ctrl`, `key_with_shift_ctrl`, `key_ctrl_alt`
  (used everywhere), `key_ctrl_shift` (defined in the align section but
  used by restyle tests too → common), `make_app_with_three_boxes`
  (defined in marquee section but used by align/distribute/invert/
  palette → common). Helpers used by exactly one category
  (`commit_one_smooth_line`/`commit_one_light_box` → restyle,
  `app_with_three_layers_and_panel_open`/`app_with_three_layer_rows`/
  `mouse_down` → layers, `mouse_click`/`mouse_marquee` → mouse,
  `open_palette`/`run_palette_command`/`run_palette_command_into`
  → palette, `make_app_with_text` → text_edit,
  `make_app_with_two_boxes` → grouping, `app_with_inspector_panel`/
  its own `mouse_down` → inspector, `make_app_with_findable` → find,
  `key_ctrl_a` → align) stay with their category.
- The `pub(super)` visibility on `common.rs` helpers works because
  `super` from `common` is the `tests` mod, and the category sub-
  modules are children of `tests` — so they're descendants of `tests`
  and can `use crate::event::tests::common::*;`. A `pub(crate) use
  common::*;` glob re-export in the `tests` mod is NOT needed and
  produces a "glob import doesn't reexport anything... because no
  imported item is public enough" warning — the sub-modules import
  directly from `common`, not via a re-export.
- `git stash` in a worktree that has a leftover stash from ANOTHER
  branch (wo/15.14) will pop the other branch's WIP into your tree.
  I stashed my WO 15.13 work to verify the baseline, and the pop
  brought in `kirkforge-draw-core/src/state.rs` → `state/mod.rs`
  changes from WO 15.14. Had to `git reset HEAD` + `git checkout --
  ` + `rm` to clean it up, and re-delete `event.rs` (the stash had
  restored it). Lesson: in shared-worktree setups, check `git stash
  list` before stashing, and prefer `git stash push -- <specific
  paths>` to scope the stash to only your files.
- `cargo clippy --all-targets` on this worktree hit a pre-existing
  baseline failure: `FileWriteEvent` got a `content_hash` field from
  a merged WO, but `src/session/verifier/security.rs` test fixtures
  (lines 600, 622, 642, 717) construct `FileWriteEvent` without it.
  6 `E0063` errors in the `kirkforge` main binary lib test. This is
  NOT in `kirkforge-draw` and NOT caused by my changes — verified by
  stashing my changes and reproducing the identical 6 errors on the
  clean tree. The worktree branch (`fb334cb merge: wo/15.11`) is
  behind `dev` on the WO that fixed security.rs. Per AGENTS.md §6,
  fixing it is out of WO 15.13 scope ("This is a draw-TUI crate, not
  the main binary. The split is localized."). The scoped gate
  (`cargo clippy -p kirkforge-draw --all-targets -- -D warnings` and
  `cargo test -p kirkforge-draw`) is green.

## What I tried that didn't work
- First compile after the split had `use crate::app::PaletteTrigger;
  use kirkforge_draw_core::{DrawObject, DrawState, InkColor, Point};
  use ratatui::layout::Rect;` at the top of the `tests` mod in
  `event/mod.rs` (copied from the original). These all generated
  "unused import" warnings because the sub-modules import what they
  need directly. Removed them; the `tests` mod now only has
  `use super::*;` + the `pub(crate) mod` declarations.
- First compile also had a `pub(crate) use common::*;` re-export in
  the `tests` mod, intended to surface the shared helpers to the
  sub-modules. Produced a "glob import doesn't reexport anything with
  visibility pub(crate) because no imported item is public enough"
  warning (the `common` helpers are `pub(super)`, can't be re-
  exported at `pub(crate)`). Removed the re-export — the sub-modules
  `use crate::event::tests::common::*;` directly, which works because
  they're descendants of `tests`.
- `cargo clippy --all-targets` (workspace) timed out at 10 min with
  2-3 concurrent worktree builds. Used the `setsid bash -c '... >
  log 2>&1' & disown` background pattern from WO 15.10's lessons to
  run it, then polled the log with `sleep N; tail`. Same for the
  workspace test.

## What I'd do differently
- The `git stash` cross-branch contamination was the only real
  time-sink. In a worktree that shares `.git` with other branches,
  `git stash push -- crates/kirkforge-draw/src/event.rs crates/
  kirkforge-draw/src/event/` would have scoped the stash to only my
  files and avoided popping WO 15.14's state.rs work. Worth doing in
  any shared-`.git` worktree setup.
- The cross-category helper audit (which helpers go to `common` vs
  the category file) took a careful read of every test's call sites.
  A faster approach: `grep -n "<helper_name>(" event.rs` for each
  helper, count the distinct category sections it appears in. I did
  this manually during the planning read; a scripted version would
  have been quicker for a file this size.