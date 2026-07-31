//! Event loop.
//!
//! Polls crossterm for events with a 100ms tick. The tick rate is the
//! render heartbeat — at 10 fps the user sees a live editor without
//! burning CPU. Real-time input is independent of the tick: we use
//! `EventStream` semantics (read with a non-blocking poll) so
//! keystrokes feel instant.
//!
//! Key map:
//!   * q / Ctrl-C / Esc        → quit (Esc also clears selection); on a dirty document, q / Ctrl-C triggers a `save? (y/n/Esc)` confirm; y saves then quits, n discards then quits, Esc cancels
//!   * s / b / l / e / p / t   → tool (select / box / line / elbow / paint / text)
//!   * Tab / Shift+Tab         → cycle tools (forward / backward)
//!   * Delete / Backspace      → delete selected
//!   * Ctrl-Z / Ctrl-Y / Ctrl-Shift-Z → undo / redo / redo
//!   * Ctrl-S                  → save back to source path (or open save-as if no path yet)
//!   * Ctrl-Shift-S            → save as (type a new path, Enter writes, Esc cancels)
//!   * Ctrl-D                  → duplicate selection (offset +1, +1)
//!   * Ctrl-C                  → copy selection to clipboard (when selected)
//!   * Ctrl-X                  → cut selection to clipboard (copy + delete)
//!   * Ctrl-V                  → paste from clipboard
//!   * Ctrl-G                  → group selection under a new parent id
//!   * Ctrl-Shift-G            → ungroup selection (clear parent_id)
//!   * Ctrl-A                  → select every object in the document (pre-cursor for align / distribute / restyle)
//!   * Ctrl-1..8               → recolor selection (matches InkColor variant order)
//!   * Ctrl-Alt-L              → cycle LineStyle on selected lines / elbows (smooth → light → double → dashed)
//!   * Ctrl-Alt-B              → cycle BoxStyle on selected boxes (light → heavy → double → dashed → auto)
//!   * Ctrl-Alt-T              → cycle TextBorderMode for new text (none → single → double → underline)
//!   * Ctrl-Alt-P              → cycle paint brush for new paint (· → o → * → x → █ → ▒ → ░ → ▓)
//!   * Ctrl-Shift-L            → align selection to left edge
//!   * Ctrl-Shift-R            → align selection to right edge
//!   * Ctrl-Shift-T            → align selection to top edge
//!   * Ctrl-Shift-B            → align selection to bottom edge
//!   * Ctrl-Shift-H            → align selection to horizontal center
//!   * Ctrl-Shift-V            → align selection to vertical center
//!   * Ctrl-Shift-J            → distribute selection (equal horizontal spacing, endpoints pinned)
//!   * Ctrl-Shift-K            → distribute selection (equal vertical spacing, endpoints pinned)
//!   * Ctrl-Shift-I            → invert selection (flip membership of every object)
//!   * ] / [                  → raise / lower selection (z-order, jump to extreme)
//!   * Shift+] / Shift+[      → raise / lower by one step (z-order nudge)
//!   * : / /                  → open command palette (Enter run, Esc cancel)
//!   * F2                      → edit selected Text (Enter commits, Shift+Enter inserts \n, Esc cancels; Backspace/Delete edit, Left/Right step the cursor; Home/End jump; Up/Down move by line)
//!   * Arrow keys              → scroll viewport (2 cells at a time)
//!   * PageUp / PageDown       → scroll viewport one page (10 cells)
//!   * Shift+Arrow             → translate selection by 1 cell
//!   * Ctrl-Shift-Arrow        → translate selection by 10 cells (coarse nudge)
//!   * L                       → toggle layers panel
//!   * I                       → toggle properties inspector panel
//!   * Ctrl-F                  → find by id substring or text content (Enter selects first match, Esc cancels)
//!   * i (lowercase)           → cycle selection color forward (White → Red → … → Magenta → White)
//!   * Up/Down (layers panel on) → focus a layer row (Enter selects, Esc clears)
//!
//! Mouse:
//!   * Left click              → select the topmost object at the
//!     point (Shift=add, Ctrl=toggle, bare=replace — same modifier
//!     semantics as a single-cell marquee, added in tick 45)
//!   * Left click on layers panel → select that row (Shift=add,
//!     Ctrl=toggle, bare=replace)
//!   * Up/Down (panel on)      → focus layer row (Enter selects,
//!     Esc clears)
//!   * Left drag               → begin/update/commit a draft for the
//!     current tool at the document point
//!   * Left drag in Select (empty space) → marquee select
//!     (Shift=add, Ctrl=toggle, bare=replace)
//!   * Left drag on a resize handle of the selected box → resize
//!
//! Scroll keys are best-effort: they never shrink the scene, they
//! just slide the viewport across it.

use anyhow::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::time::Duration;

use kirkforge_draw_core::{hit_test_box_handles, DrawObject, Point, Rect};

use kirkforge_draw_core::save_document;

use kirkforge_draw_core::DrawMode;
use kirkforge_draw_core::{filter_palette, PaletteAction};

use crate::app::{App, MarqueeState};
use crate::ui;

const TICK: Duration = Duration::from_millis(100);
const SCROLL_STEP: i32 = 2;
const SCROLL_PAGE_STEP: i32 = 10;

/// Source of truth for the help overlay. Same lines the key map
/// doc comment above would advertise, rendered as a centered rect
/// when `?` is pressed.
pub const HELP_LINES: &[&str] = &[
    "kfd — key map",
    "",
    "q / Ctrl-C / Esc       quit (Esc clears draft, resize, or selection); on a dirty doc prompts y/n/Esc",
    "s b l e p t            tool: select / box / line / elbow / paint / text",
    "Tab / Shift+Tab        cycle tools forward / backward",
    "Delete / Backspace     delete selected",
    "Ctrl-Z / Ctrl-Y        undo / redo",
    "Ctrl-Shift-Z           redo (Figma / macOS convention)",
    "Ctrl-S                 save back to source path (or open save-as if no path yet)",
    "Ctrl-Shift-S           save as (type path, Enter writes, Esc cancels)",
    "Ctrl-D                 duplicate selection (offset +1, +1)",
    "Ctrl-C                 copy selection (when something is selected)",
    "Ctrl-X                 cut selection (copy + delete)",
    "Ctrl-V                 paste from clipboard",
    "Ctrl-G                 group selection under a new parent id",
    "Ctrl-Shift-G           ungroup selection (clear parent_id)",
    "Ctrl-A                 select every object in the document",
    "Ctrl-1..8              recolor selection (white, red, orange, yellow, green, cyan, blue, magenta)",
    "i                      cycle selection color forward (white → red → … → magenta → white)",
    "Ctrl-Alt-L             cycle LineStyle on selection (smooth → light → double → dashed)",
    "Ctrl-Alt-B             cycle BoxStyle on selection (light → heavy → double → dashed → auto)",
    "Ctrl-Alt-T             cycle TextBorderMode for new text (none → single → double → underline)",
    "Ctrl-Alt-P             cycle paint brush for new paint (· → o → * → x → █ → ▒ → ░ → ▓)",
    "Ctrl-Shift-L           align selection to left edge",
    "Ctrl-Shift-R           align selection to right edge",
    "Ctrl-Shift-T           align selection to top edge",
    "Ctrl-Shift-B           align selection to bottom edge",
    "Ctrl-Shift-H           align selection to horizontal center",
    "Ctrl-Shift-V           align selection to vertical center",
    "Ctrl-Shift-J           distribute selection (equal horizontal spacing, endpoints pinned)",
    "Ctrl-Shift-K           distribute selection (equal vertical spacing, endpoints pinned)",
    "Ctrl-Shift-I           invert selection (flip membership of every object)",
    "F2                     edit selected Text (Enter commit, Shift+Enter newline, Backspace/Delete, Left/Right step, Home/End jump, Up/Down line)",
    "] / [                  raise / lower selection (z-order, jump to extreme)",
    "Shift+] / Shift+[      raise / lower by one step (z-order nudge)",
    ": / /                  open command palette (Enter run, Esc cancel)",
    "Arrow keys             scroll viewport (2 cells at a time)",
    "PageUp / PageDown      scroll viewport one page (10 cells)",
    "Shift+Arrow            nudge selection by 1 cell",
    "Ctrl-Shift-Arrow       nudge selection by 10 cells (coarse nudge, endpoints stay in selection)",
    "L                      toggle layers panel (right sidebar)",
    "Up/Down (panel on)     focus layer row (Enter selects, Esc clears)",
    "I                      toggle inspector panel (right sidebar)",
    "Ctrl-F                 find by id or text content (Enter cycles matches, Esc closes)",
    "?                      toggle this help (Esc also closes it)",
    "",
    "Mouse:                 left-click select (Shift=add, Ctrl=toggle), drag-draft, handle-resize",
    "Marquee:               left-drag in empty space (Shift=add, Ctrl=toggle)",
    "Layers:                left-click row to select (Shift=add, Ctrl=toggle)",
    "Inspector:             left-click panel to reaffirm single id (Shift=no-op, Ctrl=deselect, empty/multi=status only)",
];

/// Half-extent in cells around each box corner that counts as a hit on
/// the resize handle. One cell of slack makes the corners easier to
/// grab without overlapping the box's neighbors.
const HANDLE_HIT_TOLERANCE: i32 = 1;

pub fn run(
    app: &mut App,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<()> {
    loop {
        // Symmetric with the key/mouse handlers below: a panic in
        // `ui::draw` (out-of-bounds cell indexing, bad Rect math)
        // should NOT terminate the editor and lose unsaved work.
        //
        // A non-panic I/O error from ratatui's draw (broken tty,
        // process reaped) IS catastrophic and should bubble out as
        // a CLI error. We let ratatui drive the inner callback and
        // only catch_unwind inside that callback so the ratatui
        // bookkeeping (double-buffer diff, cursor restore) still
        // runs after a UI panic — that's what avoids leaving the
        // user staring at a half-flushed tty.
        let draw_result = terminal.draw(|frame| {
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ui::draw(app, frame);
            })) {
                surface_panic(app, "draw", payload);
            }
        });
        if let Err(e) = draw_result {
            return Err(e.into());
        }

        if crossterm::event::poll(TICK)? {
            let ev = crossterm::event::read()?;
            // Defense in depth: a panic in a single keystroke handler
            // should not kill the editor and lose unsaved work. Catch
            // here, log to stderr, surface on the status bar, and
            // continue the loop. AssertUnwindSafe on App because we're
            // not crossing an FFI boundary — the unwind stays inside
            // the process.
            match ev {
                Event::Key(key) => {
                    if let Err(payload) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle_key(app, key)
                        }))
                    {
                        surface_panic(app, "key", payload);
                    }
                }
                Event::Mouse(mouse) => {
                    if let Err(payload) =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle_mouse(app, mouse)
                        }))
                    {
                        surface_panic(app, "mouse", payload);
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Log a caught panic to stderr and surface a user-readable summary on
/// the status bar so the next render shows it. The panic payload is
/// usually `&str` or `String` from `panic!()`; we try both before
/// falling back to a generic marker.
fn surface_panic(app: &mut App, handler: &str, payload: Box<dyn std::any::Any + Send>) {
    let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    };
    eprintln!("kfd: {handler} handler panicked: {msg}");
    app.status = format!("internal error in {handler} handler (panic caught)");
}

/// Page-scroll the viewport by `dy` pages. Positive scrolls down;
/// negative scrolls up. The y-axis uses saturating subtraction so
/// the viewport doesn't drift negative at the top, but x has no
/// upper bound — the user's keyboard can always slide them further
/// into the document. Same arithmetic shape as the arrow-scroll
/// arm above; pure helper so unit tests can pin both directions
/// without a Terminal.
fn scroll_app_pages(app: &mut App, dy: i32) {
    let delta = dy * SCROLL_PAGE_STEP;
    app.scroll_y = (app.scroll_y + delta).max(0);
}

fn handle_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // Quit-confirm hijack wins over palette / find / text-edit /
    // main keymap. The user is in the middle of "do I want to
    // lose my changes?"; their next key is the answer, not
    // anything else. Esc clears the prompt rather than clearing
    // the selection or quitting — same key, but its meaning
    // changes when the confirm is showing.
    if app.pending_quit_confirm {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                // Save then quit. `save_app` already handles
                // the validate_path_arg guard, atomic write,
                // and dirty-bit flip on failure. We forward
                // the status message unchanged on Ok; on Err
                // we keep the editor open and drop the confirm
                // so the user can fix the problem and try
                // again.
                //
                // Fresh doc (no source_path): Ctrl-S / :save
                // both already open save-as in this situation
                // (ticks 42 / 43). Mirror them here — the
                // user answered the prompt with intent to save,
                // so let them name the file. They can re-fire
                // `q` after the save-as commits if they still
                // want out.
                app.pending_quit_confirm = false;
                if app.source_path.is_none() {
                    app.begin_save_as();
                } else {
                    match save_app(app) {
                        Ok(()) => {
                            app.status =
                                format!("saved {}", app.source_path.as_deref().unwrap_or("?"));
                            app.should_quit = true;
                        }
                        Err(e) => {
                            app.status = format!("save failed: {e}");
                            // Stay in editor; user can fix and
                            // try quit again.
                        }
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                app.quit_confirm_no();
            }
            KeyCode::Esc => {
                app.quit_confirm_cancel();
            }
            _ => {
                // Swallow everything else. The prompt is modal
                // and the only valid answers are y / n / Esc;
                // letting a stray key (e.g. Backspace, Enter,
                // an arrow) through would either edit the
                // status line or trigger an action the user
                // didn't mean to take.
            }
        }
        return;
    }
    // Command-palette mode hijacks the key stream ahead of both the
    // text-edit hijack and the normal key dispatch — when the user
    // has `:` pressed they're committed to typing into the palette.
    // Printable chars append, Enter dispatches, Esc cancels,
    // Backspace pops. We reject Ctrl-anything except Ctrl-C so the
    // global quit chord still works for "give up".
    if app.palette.is_some() {
        match key.code {
            KeyCode::Esc => app.cancel_palette(),
            KeyCode::Enter => commit_palette(app),
            KeyCode::Backspace => app.palette_backspace(),
            KeyCode::Char('c') if ctrl => app.cancel_palette(),
            KeyCode::Char('u') if ctrl => app.palette_clear(),
            KeyCode::Char(ch) if !ctrl && !alt => {
                app.palette_insert(ch);
            }
            _ => {}
        }
        return;
    }
    // Text-entry mode hijacks the key stream: printable chars append
    // to the buffer at the cursor, Enter commits, Shift+Enter inserts
    // a newline (multi-line text), Esc cancels, Backspace pops the
    // byte before the cursor, Delete removes the byte at the cursor.
    // Left / Right step the cursor one byte (no-op at the buffer
    // edges); Home / End jump to the buffer start / end; Up / Down
    // move the cursor to the prior / next line (preserving the
    // column, clamped to the target line's length; no-op at the
    // buffer's first / last line). Ctrl-C still aborts the edit
    // (mirrors Ctrl-C as the universal "give up" key) — bound to
    // cancel_text_edit below.
    if app.text_edit.is_some() {
        match key.code {
            KeyCode::Esc => app.cancel_text_edit(),
            KeyCode::Enter if shift => {
                // ponytail: Shift+Enter is the line-break chord
                // because bare Enter commits — a deliberate
                // trade-off so the commit gesture stays one key.
                // Wrapping the buffer to fit a width is a future
                // tick; today the user inserts `\n` themselves.
                app.text_edit_insert('\n');
            }
            KeyCode::Enter => {
                app.commit_text_edit();
            }
            KeyCode::Backspace => app.text_edit_backspace(),
            KeyCode::Delete => app.text_edit_delete(),
            KeyCode::Left => app.text_edit_cursor_left(),
            KeyCode::Right => app.text_edit_cursor_right(),
            KeyCode::Home => app.text_edit_cursor_home(),
            KeyCode::End => app.text_edit_cursor_end(),
            KeyCode::Up => app.text_edit_cursor_up(),
            KeyCode::Down => app.text_edit_cursor_down(),
            KeyCode::Char('c') if ctrl => app.cancel_text_edit(),
            KeyCode::Char(ch) if !ctrl => {
                // Insert every printable char (crossterm already gave
                // us the Unicode scalar value for unicode chars).
                app.text_edit_insert(ch);
            }
            _ => {}
        }
        return;
    }
    // Find mode hijacks the key stream ahead of the normal
    // keymap — when the user has Ctrl-F pressed they're
    // committed to typing into the find buffer. Printable
    // chars append, Enter selects the current match (or
    // reports "no match for 'X'" and closes the session when
    // the query produced nothing), Backspace pops, Esc
    // cancels. Ctrl-C still aborts — same "give up" key as
    // the palette and text-edit modes.
    //
    // ponytail: the Enter arm commits + closes in one
    // keystroke. A "stay-in-find" mode where Enter cycles
    // through matches without closing is the natural next
    // tick (the `index` field on FindState is already
    // plumbed for it). Today's "select first match and
    // close" matches the Figma-find-in-canvas convention.
    if app.find.is_some() {
        match key.code {
            KeyCode::Esc => app.cancel_find(),
            KeyCode::Enter => app.cycle_find(),
            KeyCode::Backspace => app.find_backspace(),
            KeyCode::Char('c') if ctrl => app.cancel_find(),
            KeyCode::Char(ch) if !ctrl && !alt => {
                app.find_insert(ch);
            }
            _ => {}
        }
        return;
    }
    // Save-As mode hijack — same shape as find: printable
    // chars append, Backspace pops, Enter commits, Esc
    // cancels. Ctrl-C is the universal "give up" key so it
    // cancels the modal (matches palette / find). Sits
    // after find so a stray Ctrl-F mid-save-as opens the
    // find modal — but `begin_save_as` already refuses when
    // find is open, so this is just a defense-in-depth
    // ordering. ponytail: re-uses the find pattern instead
    // of inventing a generic modal registry.
    if app.save_as.is_some() {
        match key.code {
            KeyCode::Esc => app.cancel_save_as(),
            KeyCode::Enter => {
                // Capture prior source_path BEFORE
                // commit_save_as flips it — otherwise we'd
                // snapshot the new (possibly bad) path as
                // "prior" and the revert would be a no-op.
                let prior_source = app.source_path.clone();
                if let Some(path) = app.commit_save_as() {
                    // Mirror the Ctrl-S save flow: hand off
                    // to save_app so atomic-write +
                    // missing-source-path bail! + mark_saved
                    // stay in one place. source_path was
                    // already updated by commit_save_as —
                    // on Err we roll it back via
                    // revert_save_as so the user's next
                    // Ctrl-S lands where they came from.
                    let path_for_revert = path.clone();
                    match save_app(app) {
                        Ok(()) => {
                            app.status = format!("saved as → {path}");
                        }
                        Err(e) => {
                            // Roll source_path back to where
                            // the user came from and re-open
                            // the modal pre-populated with
                            // the path that failed. The user
                            // sees the failure, can edit the
                            // path, and try again — without
                            // losing the prior source_path.
                            app.revert_save_as(prior_source, path_for_revert);
                            app.status = format!("save as failed: {e}");
                        }
                    }
                }
            }
            KeyCode::Backspace => app.save_as_backspace(),
            KeyCode::Char('c') if ctrl => app.cancel_save_as(),
            KeyCode::Char(ch) if !ctrl && !alt => {
                app.save_as_insert(ch);
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.request_quit(),
        // Ctrl-C: copy to clipboard when there's a selection; fall
        // through to quit when there's nothing to copy. This keeps
        // the "Ctrl-C = quit" convention working on an empty editor
        // while still giving the user a copy chord when something
        // is selected.
        KeyCode::Char('c') if ctrl && app.state.selected_count() > 0 => {
            copy_selected(app);
        }
        KeyCode::Char('c') if ctrl => app.request_quit(),
        // Esc clears any active draft AND active resize, or clears the
        // selection; only quits if none are present.
        // (Layer-focus Esc must come BEFORE this arm so the panel
        // intercepts its own clear-focus; the top-level Esc would
        // otherwise try to clear the selection / quit.)
        KeyCode::Esc if app.show_layers && app.layer_focus.is_some() => {
            clear_layer_focus(app);
        }
        // Esc closes the help overlay (universal-dismiss gesture —
        // palette, find, save-as, text-edit all honor it). Without
        // this guard a clean doc + no selection + help-open + Esc
        // would fall through to `request_quit`, dismissing help
        // and starting a quit-confirm on the next tick. Placed
        // before the cascade so it wins.
        KeyCode::Esc if app.show_help => {
            app.toggle_help();
        }
        KeyCode::Esc => {
            if app.state.has_draft() || app.state.is_resizing() {
                app.state.cancel_all();
            } else if app.state.selected_count() > 0 {
                app.state.clear_selection();
            } else {
                app.request_quit();
            }
        }
        // Modifier shortcuts first (Ctrl-S overrides the bare 's'
        // tool binding below). save_app updates the dirty bit itself
        // (clears on success, sets on failure) so the keypress handler
        // only needs to surface the outcome in the status line.
        KeyCode::Char('s') if ctrl && shift => {
            // Ctrl-Shift-S — Save As. Opens a mini text-input
            // modal pre-populated with the current source
            // path (or empty if there is none). The actual
            // write happens in the save_as Enter arm above
            // so atomic-write + missing-source-path bail! +
            // mark_saved stay in save_app.
            app.begin_save_as();
        }
        KeyCode::Char('s') if ctrl => {
            // Fresh document (no `--load` path yet): Ctrl-S
            // would otherwise bail with a confusing
            // "no source path" error. Match the standard editor
            // convention and fall through to save-as so the
            // user can name the file. Once a path exists,
            // Ctrl-Shift-S is still available to rename.
            if app.source_path.is_none() {
                app.begin_save_as();
            } else {
                match save_app(app) {
                    Ok(()) => {
                        app.status = format!("saved {}", app.source_path.as_deref().unwrap_or("?"));
                    }
                    Err(e) => app.status = format!("save failed: {e}"),
                }
            }
        }
        // Ctrl-Shift-Z must match BEFORE the bare Ctrl-Z arm:
        // `match` evaluates arms in source order, and the bare
        // `ctrl` guard would otherwise swallow the chord and
        // undo (silently shadowing the redo alias). The `ctrl
        // && shift && !alt` guard is the Figma / macOS redo
        // convention paired with Ctrl-Z / undo. Ctrl-Y keeps
        // working as the Windows / Linux redo chord; both
        // arms hit the same `state.redo()` helper so the
        // outcome is identical regardless of which one the
        // user reaches for.
        KeyCode::Char(c) if (c == 'z' || c == 'Z') && ctrl && shift && !alt => {
            if !app.state.redo() {
                app.status = "nothing to redo".into();
            }
        }
        KeyCode::Char('z') if ctrl => {
            if !app.state.undo() {
                app.status = "nothing to undo".into();
            }
        }
        KeyCode::Char('y') if ctrl => {
            if !app.state.redo() {
                app.status = "nothing to redo".into();
            }
        }
        KeyCode::Char('d') if ctrl => {
            let new_ids = app.state.duplicate_selected();
            if new_ids.is_empty() {
                app.status = "nothing to duplicate".into();
            } else {
                app.status = format!(
                    "duplicated {} object{}",
                    new_ids.len(),
                    plural_s(new_ids.len())
                );
            }
        }
        // F2 enters text-entry mode for the single-selected Text object.
        KeyCode::F(2) => {
            if !app.begin_text_edit() {
                app.status = "no Text selected — F2 edits a single Text".into();
            }
        }
        // `:` and `/` open the command palette. The two triggers
        // look identical today (both start with the same prompt
        // and accept the same input) but are recorded separately
        // so a future re-purposing of `/` (e.g., to filter model-
        // emitted diagrams) can split the UX without rewriting
        // the trigger-detection code.
        // `L` toggles the layers panel — short key, easy to
        // reach, no conflict with the existing `l` (line tool)
        // because Shift is not held. If the user already has a
        // panel focused the next tick's arrow-nav handler will
        // own the rest.
        // Layers panel toggle (`L`). The arm must NOT match
        // when Ctrl is held — on a real terminal Ctrl-Shift-L
        // produces the shifted char 'L' (uppercase) with both
        // Ctrl and Shift set, so an unguarded match would
        // shadow the align-left chord below (and the user
        // would see the layers panel flip when they wanted to
        // align). Bare Shift+L is the toggle gesture, so we
        // only allow Shift (and Alt-free — Alt is reserved
        // for future related chords).
        KeyCode::Char('L') if !ctrl && !alt => app.toggle_layers(),
        // `I` toggles the properties inspector panel. Capital
        // `I` (lowercase `i` is free today — kept free for a
        // future ink-picker shortcut). Mirrors the `L` arm
        // above; the inspector has no per-row focus so no
        // nested state to clear on close.
        KeyCode::Char('I') if !ctrl && !shift => app.toggle_inspector(),
        KeyCode::Char(':') => {
            if !app.begin_palette(crate::app::PaletteTrigger::Colon) {
                // Already in a palette — ignore the extra `:`.
            }
        }
        KeyCode::Char('/') => {
            if !app.begin_palette(crate::app::PaletteTrigger::Slash) {
                // Already in a palette — ignore the extra `/`.
            }
        }
        // Ctrl-F: open a find session. Mirrors the palette
        // trigger arms above — a no-op when the user is
        // already mid-palette / mid-text-edit (those modes
        // early-return before reaching here, so this is
        // belt-and-suspenders).
        KeyCode::Char('f') if ctrl && !shift && !alt => {
            if !app.begin_find() {
                // Already mid-find / mid-palette / mid-edit.
            }
        }
        // Ctrl-C: copy the selection to the OS clipboard. We only
        // intercept the chord when there's an active selection —
        // otherwise this collides with the global "Ctrl-C = quit"
        // convention. The empty-selection fallthrough below keeps
        // the quit path working.
        KeyCode::Char('c') if ctrl && app.state.selected_count() > 0 => {
            copy_selected(app);
        }
        // Ctrl-V: paste. Same fallback concern as Ctrl-C above.
        // The `!shift` guard lets the Ctrl-Shift-V align-vertical
        // arm (below) match first — crossterm encodes Ctrl-V as
        // `Char('v') + CONTROL` with no separate shift bit, so
        // without this guard the paste arm would shadow the align
        // chord whenever both modifiers are physically held.
        KeyCode::Char('v') if ctrl && !shift => {
            paste(app);
        }
        // Ctrl-X: cut — copy selection to the clipboard AND remove
        // it from the document in a single undo step. Empty selection
        // is a no-op (status reports "nothing to cut") rather than a
        // fallback to another chord, since Ctrl-X has no universal
        // global meaning we need to preserve the way Ctrl-C does.
        KeyCode::Char('x') if ctrl => {
            cut(app);
        }
        // Ctrl-G: group the current selection under a freshly
        // generated parent id. All selected objects share the same
        // parent — flat group, no nesting. Empty selection is a
        // no-op reported on the status line. Status echoes the
        // new parent id so the user can confirm the chord took
        // without opening the layers panel.
        KeyCode::Char('g') if ctrl && !shift => {
            group_selection(app);
        }
        // Ctrl-Shift-G: ungroup the current selection. Clears
        // parent_id on every selected object. Idempotent — a
        // second press on an ungrouped selection is a no-op
        // (no undo churn).
        KeyCode::Char('g') if ctrl && shift => {
            ungroup_selection(app);
        }
        // Ctrl-A: select every object in the document. Slack /
        // Figma primitive — the natural pre-cursor to a
        // multi-object operation like align / distribute /
        // restyle. No modifiers: bare Shift and Ctrl are
        // already spoken for by other chords and "select all
        // and add to selection" is the same as "select all"
        // after a clear. The pure helper does not flip the
        // dirty flag — Ctrl-A is a navigation primitive, not
        // a mutation.
        KeyCode::Char('a') if ctrl && !shift && !alt => {
            let n = app.state.select_all();
            app.status = if n == 0 {
                "(nothing to select)".into()
            } else {
                format!("selected {n} object{}", plural_s(n))
            };
        }
        // Ctrl-1..8: recolor selection to one of the 8 InkColor
        // variants. Matches the InkColor enum's discriminant order:
        // 1=White, 2=Red, 3=Orange, 4=Yellow, 5=Green, 6=Cyan,
        // 7=Blue, 8=Magenta. Empty selection is a no-op reported
        // on the status line.
        KeyCode::Char(c) if ctrl && matches!(c, '1'..='8') => {
            recolor_selection(app, ink_color_for_digit(c));
        }
        // Ctrl-Alt-L: cycle LineStyle on every selected Line / Elbow
        // (Smooth → Light → Double → Dashed → Smooth). Boxes have a
        // separate BoxStyle enum and Paint / Text carry no style, so
        // restyle_selection silently skips non-styled selections.
        // Alt distinguishes this from any future bare-L shortcut.
        KeyCode::Char('l') if ctrl && alt => {
            cycle_line_style(app);
        }
        // Ctrl-Alt-B: cycle BoxStyle on every selected Box (Light →
        // Heavy → Double → Dashed → Auto → Light). Mirrors
        // Ctrl-Alt-L's pattern exactly: pure helper on DrawState
        // does the heavy lifting, this arm picks the next style
        // from the first selected Box and dispatches. `b` is
        // free in the Ctrl-Alt slot; bare `b` is the Box tool,
        // and Ctrl-Shift-B is the align-bottom chord, so the
        // `ctrl && alt` guard is the one that distinguishes us.
        KeyCode::Char('b') if ctrl && alt => {
            cycle_box_style(app);
        }
        // Ctrl-Alt-T cycles the active TextBorderMode (None →
        // Single → Double → Underline → None, in enum source
        // order). Sibling of the L and B arms above; T is
        // free in the Ctrl-Alt slot. Operates on tool state
        // (what future text drafts will inherit), not on
        // selection — text borders are a draft-time concern
        // and no "restyle existing text" UX is in scope yet.
        KeyCode::Char('t') if ctrl && alt => {
            cycle_text_border(app);
        }
        // Ctrl-Alt-P cycles the paint brush (what future
        // Paint drafts will stamp). Sibling of the L / B / T
        // arms; P is free in the Ctrl-Alt slot. Bare `p`
        // is the Paint tool, so the `ctrl && alt` guard is
        // the one that distinguishes this arm. The cycle
        // visits a fixed 8-glyph palette (· → o → * → x →
        // █ → ▒ → ░ → ▓ → ·) — same shape as the recolor
        // cluster's 8 entries.
        KeyCode::Char('p') if ctrl && alt => {
            cycle_brush(app);
        }
        // Ctrl-Shift-<dir>: align the selection to the union
        // bounds' matching edge / center (Slack / Figma primitive).
        // Each chord maps to one Align variant; `!alt` is future-
        // proofing — no Alt siblings exist today, but Alt is the
        // reserved slot for related chords (e.g. align-to-canvas)
        // so the guard matches the Ctrl-Alt-L discipline above.
        // Align-to-edge chords. Match both the lowercase and
        // uppercase glyph so real terminals — which report
        // Ctrl-Shift-<key> as the shifted (uppercase) char
        // with both modifiers set — still hit the right
        // arm. Without the `c == upper` alternative, the
        // uppercase from Ctrl-Shift-L would slip past every
        // align arm and fall through to `_ => {}`, silently
        // doing nothing while the user expected an align.
        // The lower-case alternative keeps the synthetic-
        // keypress tests (and any non-shifted-Ctrl layouts)
        // working unchanged.
        KeyCode::Char(c) if (c == 'l' || c == 'L') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::Left);
        }
        KeyCode::Char(c) if (c == 'r' || c == 'R') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::Right);
        }
        KeyCode::Char(c) if (c == 't' || c == 'T') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::Top);
        }
        KeyCode::Char(c) if (c == 'b' || c == 'B') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::Bottom);
        }
        KeyCode::Char(c) if (c == 'h' || c == 'H') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::HorizontalCenter);
        }
        KeyCode::Char(c) if (c == 'v' || c == 'V') && ctrl && shift && !alt => {
            align_selection(app, kirkforge_draw_core::Align::VerticalCenter);
        }
        // Ctrl-Shift-J / Ctrl-Shift-K: distribute the selection
        // along the X / Y axis (equal spacing between
        // consecutive items, endpoints pinned). J/K are free
        // (Y is taken by Ctrl-Y redo); adjacent on QWERTY for
        // symmetry with the H/V align-center pair. `!alt`
        // matches the align cluster's future-proofing.
        // Distribute chords — match the lowercase / uppercase
        // pair (same real-terminal-shift rationale as the
        // align cluster above).
        KeyCode::Char(c) if (c == 'j' || c == 'J') && ctrl && shift && !alt => {
            distribute_selection(app, kirkforge_draw_core::DistributeAxis::Horizontal);
        }
        KeyCode::Char(c) if (c == 'k' || c == 'K') && ctrl && shift && !alt => {
            distribute_selection(app, kirkforge_draw_core::DistributeAxis::Vertical);
        }
        // Ctrl-Shift-I: invert selection. Flip membership of
        // every object — currently-selected becomes
        // unselected, currently-unselected becomes selected.
        // One undo step. Pairs with Ctrl-A: grab everything,
        // Ctrl-Shift-I to flip back to empty. Figma / VSCode
        // convention. `I` alone toggles the inspector; `i`
        // alone cycles the selection color — Ctrl-Shift-I is
        // the disambiguated inverse-selection chord.
        KeyCode::Char('I') if ctrl && shift && !alt => {
            invert_selection(app);
        }
        // Tool shortcuts — bare letter only. The `!ctrl && !alt`
        // guards prevent Ctrl+<letter> from silently swapping
        // tools (e.g. Ctrl+B → Box, Ctrl+L → Line); today only
        // Ctrl+B/L/E/P/T leak through, because Ctrl+S has a
        // save arm, Ctrl+Alt-L/B/T/P have cycle-style arms, and
        // Alt+<letter> falls through elsewhere. Symmetric with
        // the tick-33 guard on the layers-toggle L arm — that
        // one was uppercase only, these are lowercase only.
        KeyCode::Char('s') if !ctrl && !alt => app.state.set_tool(DrawMode::Select),
        KeyCode::Char('b') if !ctrl && !alt => app.state.set_tool(DrawMode::Box),
        KeyCode::Char('l') if !ctrl && !alt => app.state.set_tool(DrawMode::Line),
        KeyCode::Char('e') if !ctrl && !alt => app.state.set_tool(DrawMode::Elbow),
        KeyCode::Char('p') if !ctrl && !alt => app.state.set_tool(DrawMode::Paint),
        KeyCode::Char('t') if !ctrl && !alt => app.state.set_tool(DrawMode::Text),
        // `i` (lowercase): cycle the selection's color one step
        // forward through the InkColor enum's discriminant order
        // (White → Red → ... → Magenta → White). Bare `i` is the
        // "next color" shortcut for users who don't want to
        // remember which digit maps to which variant under
        // Ctrl-1..8. Capital `I` (the line above) toggles the
        // inspector panel — crossterm emits them as distinct
        // KeyCodes so there's no collision. Multi-select
        // collapses to one undo step via `recolor_selection`;
        // a second `i` immediately after is a silent no-op
        // (the selected set is already at the new color).
        KeyCode::Char('i') => cycle_selection_color(app),
        // Tab / Shift+Tab cycle through tools in DrawMode order. Same
        // hook the letter hotkeys use (set_tool), so drafts cancel
        // on switch. crossterm emits Shift+Tab as `BackTab`, so that
        // arm handles backward cycling regardless of the SHIFT bit.
        KeyCode::Tab => app.state.cycle_tool(!shift),
        KeyCode::BackTab => app.state.cycle_tool(false),
        // Delete.
        KeyCode::Delete | KeyCode::Backspace => {
            let n = app.state.delete_selected();
            app.status = if n == 0 {
                "nothing to delete".into()
            } else {
                format!("deleted {} object{}", n, plural_s(n))
            };
        }
        // Shift+Arrow translates the selection by 1 cell; bare arrows
        // scroll the viewport. Drag handles coarser moves if the user
        // wants — keyboard gives precision nudging. Ctrl+Shift+Arrow
        // is the 10-cell nudge (Figma's "Shift+Arrow by 10px" on macOS,
        // mapped onto a slot that doesn't collide: bare Shift+Arrow is
        // already 1-cell here, so Ctrl+Shift+Arrow is the next step
        // up). ponytail: 10 cells, not configurable. A "set nudge
        // amount" command would be its own tick and today's default
        // matches Figma's coarse/medium/fine mental model
        // (drag = coarse, Shift+Arrow = fine, Ctrl+Shift+Arrow = medium).
        KeyCode::Left if ctrl && shift => app.state.move_selected(-10, 0),
        KeyCode::Right if ctrl && shift => app.state.move_selected(10, 0),
        KeyCode::Up if ctrl && shift => app.state.move_selected(0, -10),
        KeyCode::Down if ctrl && shift => app.state.move_selected(0, 10),
        KeyCode::Left if shift => app.state.move_selected(-1, 0),
        KeyCode::Right if shift => app.state.move_selected(1, 0),
        KeyCode::Up if shift => app.state.move_selected(0, -1),
        KeyCode::Down if shift => app.state.move_selected(0, 1),
        // Layers panel keyboard nav (Up/Down/Enter/Esc). Only
        // active when the panel is visible — when hidden, Up/
        // Down fall through to the scroll arms below. Trade-
        // off: while the panel is open, body scroll via arrow
        // keys is disabled (use PageUp/PageDown, or close the
        // panel with L).
        KeyCode::Up if app.show_layers => cycle_layer_focus(app, -1),
        KeyCode::Down if app.show_layers => cycle_layer_focus(app, 1),
        KeyCode::Enter if app.show_layers && app.layer_focus.is_some() => {
            commit_layer_focus(app);
        }
        // Scroll.
        KeyCode::Left => app.scroll_x = (app.scroll_x - SCROLL_STEP).max(0),
        KeyCode::Right => app.scroll_x += SCROLL_STEP,
        KeyCode::Up => app.scroll_y = (app.scroll_y - SCROLL_STEP).max(0),
        KeyCode::Down => app.scroll_y += SCROLL_STEP,
        // Page scroll. Pure helper keeps the saturating-subtract /
        // unbounded-add arithmetic — matches the arrow-scroll arm
        // exactly so a future clamp change (e.g. document_bounds
        // upper bound) only has to land in one place.
        KeyCode::PageUp => scroll_app_pages(app, -1),
        KeyCode::PageDown => scroll_app_pages(app, 1),
        // Z-order:
        //   ]           bring to front   (jump to extreme — topmost)
        //   Shift+] / } bring forward     (raise by one step)
        //   [           send to back     (jump to extreme — bottommost)
        //   Shift+[ / { send backward    (lower by one step)
        // compose_scene stamps objects in document order, so vec tail
        // = topmost. The Shift+arm matches the SHIFT-bit AND the
        // shifted glyph variant (`}` / `{` on US layouts) since some
        // terminals report the unshifted char with SHIFT set and
        // others report the shifted glyph.
        KeyCode::Char(']') => {
            if app.state.bring_to_front() {
                app.status = "raised".into();
            }
        }
        KeyCode::Char('}') if shift => {
            if app.state.bring_forward() {
                app.status = "raised one step".into();
            }
        }
        KeyCode::Char('[') => {
            if app.state.send_to_back() {
                app.status = "lowered".into();
            }
        }
        KeyCode::Char('{') if shift => {
            if app.state.send_backward() {
                app.status = "lowered one step".into();
            }
        }
        // Help overlay: toggles the key-map rect. We deliberately
        // don't gate this on any modifier — `?` is its own
        // Shift-state in most layouts.
        KeyCode::Char('?') => app.toggle_help(),
        _ => {}
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // Lazily enable mouse capture on first mouse event so the editor
    // doesn't pollute the terminal with mouse reports until the user
    // actually wants to draw. Must happen BEFORE any Moved-skip guard:
    // some terminals emit Moved without an explicit Enable, and a
    // guard that runs first would starve us of capture forever.
    if !app.mouse_captured {
        // Best-effort: if the terminal refuses EnableMouseCapture
        // (no TTY, exotic emulator), the editor still works with
        // keyboard — we just won't get mouse events. Same
        // graceful-degradation rationale as TerminalGuard::drop.
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
        app.mouse_captured = true;
    }
    // Panels claim their clicks BEFORE body hit-tests so a
    // click in a panel never falls through and re-triggers a
    // marquee / draft. Layers get first refusal because they
    // sit left of the inspector when both are open — a click
    // on the boundary is unambiguously the layers panel.
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if let Some(panel_area) = app.layers_area {
            // Inside the panel rect? Use terminal coordinates
            // directly — no scene mapping, no document point.
            if mouse.column >= panel_area.x
                && mouse.column < panel_area.right()
                && mouse.row >= panel_area.y
                && mouse.row < panel_area.bottom()
            {
                handle_layer_click(app, mouse.row, panel_area, mouse.modifiers);
                return;
            }
        }
        if let Some(panel_area) = app.inspector_area {
            if mouse.column >= panel_area.x
                && mouse.column < panel_area.right()
                && mouse.row >= panel_area.y
                && mouse.row < panel_area.bottom()
            {
                handle_inspector_click(app, mouse.modifiers);
                return;
            }
        }
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(p) = app.screen_to_doc(mouse.column, mouse.row) else {
                return;
            };
            // Resize has priority over select/draft when the user
            // grabs a handle of the (single) selected box. This keeps
            // the resize gesture inside Select tool — no need to
            // switch tools.
            if app.state.tool == DrawMode::Select {
                if let Some(handle) = hit_test_selected_box(&app.state, p, HANDLE_HIT_TOLERANCE) {
                    app.state.begin_resize(handle);
                    return;
                }
                // No handle hit → begin a marquee drag. The actual
                // selection commit happens on Up (or falls back to a
                // single-point `select_at` if the user didn't move).
                app.marquee = Some(MarqueeState {
                    anchor: p,
                    current: p,
                    mode: mode_from_modifiers(mouse.modifiers),
                });
                return;
            }
            app.state.begin_draft(p);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(p) = app.screen_to_doc(mouse.column, mouse.row) {
                if app.state.is_resizing() {
                    app.state.update_resize(p);
                } else if app.state.has_draft() {
                    app.state.update_draft(p);
                } else if let Some(m) = app.marquee.as_mut() {
                    m.current = p;
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            // ponytail: if we were resizing, commit the resize and
            // stay in Select — the drag was a transform, not a draft.
            if app.state.is_resizing() {
                if app.state.commit_resize() {
                    app.status = "resized box".into();
                }
                return;
            }
            // Marquee commit: if the drag actually moved, route
            // through select_in_rect; if it was a click (anchor ==
            // current), fall back to the existing topmost-object
            // hit-test so single-click selection still works.
            if let Some(m) = app.marquee.take() {
                let Some(p) = app.screen_to_doc(mouse.column, mouse.row) else {
                    return;
                };
                if m.anchor == p {
                    // Honor the modifier captured at marquee
                    // creation (Down) so a single click behaves
                    // like a degenerate drag: bare = Replace,
                    // Shift = Add, Ctrl = Toggle. Without this,
                    // Shift+click and Ctrl+click would silently
                    // replace the selection with the picked
                    // object — the bug that motivated the
                    // select_at_with_mode helper.
                    let _ = app.state.select_at_with_mode(p, m.mode);
                    return;
                }
                let rect = marquee_rect(m.anchor, p);
                let n = app.state.select_in_rect(rect, m.mode);
                app.status = match n {
                    0 => "no objects in marquee".into(),
                    _ => format!("selected {} object{}", n, plural_s(n)),
                };
                return;
            }
            if app.state.commit_draft().is_some() {
                app.state.set_tool(DrawMode::Select);
            }
        }
        _ => {}
    }
}

/// Map a `KeyModifiers` set to a `SelectionMode`. Used by the
/// marquee drag, the layers-panel click, and the inspector-panel
/// click — three sites that previously inlined the exact same
/// seven-line `if-else` chain (and would have been a fourth the
/// next time a panel was added).
///
/// Precedence: Ctrl wins (Toggle), then Shift (Add), then bare
/// (Replace). Ctrl first because Toggle is the most stateful
/// mode — the user has to opt in — and matches the Figma / VS
/// Code convention.
fn mode_from_modifiers(mods: KeyModifiers) -> kirkforge_draw_core::SelectionMode {
    use kirkforge_draw_core::SelectionMode;
    if mods.contains(KeyModifiers::CONTROL) {
        SelectionMode::Toggle
    } else if mods.contains(KeyModifiers::SHIFT) {
        SelectionMode::Add
    } else {
        SelectionMode::Replace
    }
}

/// Normalize two document points (anchor + current) into a `Rect`
/// the selection-bounds intersection test can consume. The order
/// doesn't matter — the rect is always (min.x, min.y) → (max.x,
/// max.y). An anchor == current collapses to a 1x1 rect (left ==
/// right, top == bottom), which is intentional: a zero-distance
/// drag falls back to `select_at` upstream and never reaches this
/// path.
fn marquee_rect(a: Point, b: Point) -> kirkforge_draw_core::Rect {
    kirkforge_draw_core::Rect {
        left: a.x.min(b.x),
        top: a.y.min(b.y),
        right: a.x.max(b.x),
        bottom: a.y.max(b.y),
    }
}

/// English plural suffix: `""` for one, `"s"` for anything else.
/// `format!("deleted {} object{}", n, plural_s(n))` reads cleanly
/// and matches the dozens of `selected N object(s)` status lines
/// scattered through this file — every "object" status passes
/// through here so a future localization only has one site to
/// swap. `usize` only because every status-count in this crate is
/// a `selected_count`, a deletion count, or a paste count.
fn plural_s(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `"{verb} {n} object(s) to {dest}"` status for the four
/// settings that act on N selected objects with a named
/// destination: restyle (LineStyle / BoxStyle), align
/// (EdgeName), distribute (SpacingName). Same shape repeated
/// four times; one helper, four callers, one place to fix the
/// wording. Take `verb: &str` so callers can stay trivially
/// rerouted later if a future "X'ify" verb lands.
fn status_n_objects_to(n: usize, verb: &str, dest: &str) -> String {
    format!("{verb} {n} object{} to {dest}", plural_s(n))
}

/// Map a Left-Down mouse event inside the layers panel to a
/// selection update. The panel's first row is the header
/// (`layers`); subsequent rows map 1:1 onto `layer_list` (top of
/// the vec = topmost-first). A click that lands on the header,
/// below the last row, or on an empty panel is a no-op (status
/// message confirms the click was on the panel).
///
/// `panel_area` is `App.layers_area` for the frame the click
/// happened in. We index by `panel_area.y + 1 + row` so the
/// header consumes one row before the first layer row — this
/// matches the renderer's layout in `render_layers_panel`.
///
/// Modifier semantics mirror `mode_from_modifiers`: bare = Replace,
/// Shift = Add, Ctrl = Toggle. Replace routes through
/// `DrawState::select_id` so the selection is exactly the
/// clicked object — no need to seed a marquee rect. Add and
/// Toggle mutate the selection set directly because no public
/// single-id path exists in core for those modes today, and
/// adding one solely to back the layers-panel click would
/// expand the API surface for a single bin caller — kept
/// inline.
///
/// ponytail: the panel ignores Right-Middle clicks and drag
/// events. The panel is too narrow to make a panel-local
/// drag meaningful, and a drag that started outside the panel
/// routes through body-area hit-tests before reaching the
/// layers panel. If we ever want drag-reorder, the panel
/// gets its own `app.dragging_layer: Option<usize>` field
/// with the same `Drag`/`Up` pair the body uses today.
fn handle_layer_click(
    app: &mut App,
    row: u16,
    panel_area: ratatui::layout::Rect,
    modifiers: KeyModifiers,
) {
    let layers = kirkforge_draw_core::layer_list(&app.state);
    // Header consumes panel_area.y; first layer row is
    // panel_area.y + 1. Subtract both to get the layer index
    // (0 = topmost). row < panel_area.y + 1 → header click
    // (no-op).
    let header_offset: u16 = 1;
    if row < panel_area.y + header_offset {
        return;
    }
    let rel = (row - panel_area.y - header_offset) as usize;
    let Some(layer) = layers.get(rel) else {
        // Below the last layer or empty document — confirm
        // the click was on the panel, but no object to select.
        app.status = "(layers panel: empty row)".into();
        return;
    };
    let id = layer.id.clone();
    // Anchor the panel's keyboard focus to the clicked row.
    // Without this, a stale focus from a prior Up/Down walk
    // would survive the click — the next Enter from the
    // keyboard would commit the stale row, not the clicked
    // one. Keeping focus and click in lockstep matches the
    // renderer's "focus wins visually" stance: a focused row
    // is the row the user is on, full stop. Modifier branches
    // below only mutate the selection, not the focus.
    app.layer_focus = Some(rel);
    let mode = mode_from_modifiers(modifiers);
    let before = app.state.selected_count();
    match mode {
        kirkforge_draw_core::SelectionMode::Replace => {
            if app.state.select_id(&id) {
                app.status = format!("selected '{id}'");
            } else {
                app.status = format!("(id vanished: {id})");
            }
        }
        kirkforge_draw_core::SelectionMode::Add => {
            app.state.add_to_selection(&id);
            let after = app.state.selected_count();
            if after > before {
                app.status = format!("selected {after} object{}", plural_s(after));
            } else {
                app.status = format!("'{id}' already in selection");
            }
        }
        kirkforge_draw_core::SelectionMode::Toggle => {
            app.state.toggle_selection(&id);
            let after = app.state.selected_count();
            // Toggle flips membership: count grew → added, count
            // shrank → removed. Same suffix rule as Add above.
            app.status = if after > before {
                format!("selected {after} object{}", plural_s(after))
            } else {
                format!("toggled '{id}' (now {after} selected)")
            };
        }
    }
}

/// Map a Left-Down click inside the inspector panel to a
/// selection update. The panel has no per-row hit-test: when
/// exactly one object is selected, the inspector renders the
/// `format_summary_rows` for that one object, so any click
/// inside the panel targets the same single id. Modifier
/// semantics mirror `mode_from_modifiers` and `handle_layer_click`:
/// bare = Replace (re-affirms the current pick), Shift = Add
/// (no-op when the only selected id is already in the set),
/// Ctrl = Toggle (the meaningful gesture — deselect the lone
/// object). Empty selection and multi-selection are status-
/// only echo so the user knows the click landed on the panel
/// but had nothing to act on.
///
/// ponytail: row / column hit-tests are already done in
/// `handle_mouse` (so we only reach this fn for clicks inside
/// `app.inspector_area`); the panel being 22 cells wide has
/// no per-row navigation, so this helper carries no `row`
/// argument. A future "click a field to edit" feature would
/// add the field index back here — the inspector summary has
/// a stable row order (id / kind / z / color / bounds /
/// kind-specific / parent).
fn handle_inspector_click(app: &mut App, modifiers: KeyModifiers) {
    let count = app.state.selected_count();
    if count == 0 {
        app.status = "(inspector: empty selection)".into();
        return;
    }
    if count > 1 {
        app.status = format!("(inspector: {count} selected)");
        return;
    }
    // Exactly one object is selected; the helper that produced
    // the summary is the source of truth for which id the
    // panel is showing. `selected()` borrows immutably so the
    // borrow ends before we hand `id` to a `&mut self` method
    // below.
    let id = match app.state.selected().first() {
        Some(obj) => obj.id().to_string(),
        None => return, // unreachable: count == 1 above.
    };
    let before = app.state.selected_count();
    let mode = mode_from_modifiers(modifiers);
    match mode {
        kirkforge_draw_core::SelectionMode::Replace => {
            // Already the only selected id — Replace is
            // statefully a no-op; the status echo confirms the
            // click landed on the panel so the user knows
            // their click was received.
            if app.state.select_id(&id) {
                app.status = format!("(inspector re-select: '{id}')");
            } else {
                app.status = format!("(id vanished: {id})");
            }
        }
        kirkforge_draw_core::SelectionMode::Add => {
            app.state.add_to_selection(&id);
            // Add on an already-selected single id is a no-op
            // (count stays at 1). Mirror the layers-panel
            // "already in selection" status for parity.
            let after = app.state.selected_count();
            if after > before {
                app.status = format!("selected {after} object{}", plural_s(after));
            } else {
                app.status = format!("'{id}' already in selection");
            }
        }
        kirkforge_draw_core::SelectionMode::Toggle => {
            app.state.toggle_selection(&id);
            let after = app.state.selected_count();
            // Toggle on the only selected id removes it: count
            // drops from 1 to 0 and selection is now empty.
            app.status = format!("toggled '{id}' (now {after} selected)");
        }
    }
}

/// Move the layers-panel focus by `delta` rows, clamping to the
/// document's layer list. `delta = -1` is Up, `+1` is Down. If
/// `app.layer_focus` is `None`, the first press sets it to the
/// topmost row (delta=-1) or bottommost (delta=+1) so a single
/// keypress is enough to enter the panel. The renderer reads
/// `layer_focus` to draw a cursor next to the focused row.
///
/// ponytail: clamping instead of wrap-around matches every
/// desktop layer panel I've used (Figma, Sketch, Affinity) —
/// hitting the top or bottom of the list is a no-op, not a
/// wrap to the other end. The user can always scroll the
/// viewport to see what's hidden.
fn cycle_layer_focus(app: &mut App, delta: i32) {
    let layers = kirkforge_draw_core::layer_list(&app.state);
    if layers.is_empty() {
        app.layer_focus = None;
        app.status = "(layers panel: empty document)".into();
        return;
    }
    let n = layers.len();
    let current = app.layer_focus.unwrap_or_else(|| {
        // No prior focus: pick the topmost (delta=-1) or
        // bottommost (delta=+1) row as the starting point so
        // the user's first press lands them inside the list.
        if delta < 0 {
            0
        } else {
            n - 1
        }
    });
    let next = if delta < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(n - 1)
    };
    app.layer_focus = Some(next);
    // Status echoes the focused row so the user has feedback
    // even before they hit Enter. Mirrors the layers panel's
    // own row format (kind label + id).
    let layer = &layers[next];
    app.status = format!(
        "layer {}/{}: {} {}",
        next + 1,
        n,
        kirkforge_draw_core::kind_label(layer.kind),
        layer.id
    );
}

/// Select the currently focused layer in the panel. Mirrors
/// `handle_layer_click`'s Replace branch — no Shift/Ctrl
/// modifiers come through the keyboard path, so a keyboard
/// select always replaces the current selection. Shift+Enter
/// for Add and Ctrl+Enter for Toggle are out of scope today;
/// the mouse path still supports them.
fn commit_layer_focus(app: &mut App) {
    let Some(focus) = app.layer_focus else {
        return;
    };
    let layers = kirkforge_draw_core::layer_list(&app.state);
    let Some(layer) = layers.get(focus) else {
        // Document changed under us (an undo, a load). Drop
        // the stale focus and surface a status message so the
        // user knows the Enter didn't silently no-op.
        app.layer_focus = None;
        app.status = "(layers panel: focus row out of range)".into();
        return;
    };
    let id = layer.id.clone();
    if app.state.select_id(&id) {
        app.status = format!("selected '{id}'");
    } else {
        app.status = format!("(id vanished: {id})");
    }
}

/// Drop the layers-panel focus (Esc inside the panel). The
/// panel keeps showing the list — this only clears the
/// highlighted "cursor" row. The next Up/Down press re-enters
/// the panel at the topmost (delta=-1) or bottommost
/// (delta=+1) row.
fn clear_layer_focus(app: &mut App) {
    if app.layer_focus.is_some() {
        app.layer_focus = None;
        app.status = "layers panel: focus cleared".into();
    }
}

/// Hit-test the four resize handles of the (only) selected box, if
/// it's a box. Returns `None` for text/line/paint selections or when
/// the selection is empty / multi.
fn hit_test_selected_box(
    state: &kirkforge_draw_core::DrawState,
    point: kirkforge_draw_core::Point,
    tolerance: i32,
) -> Option<kirkforge_draw_core::BoxResizeHandle> {
    // ponytail: inline the single-box-selection check — `selected()`
    // returns all selected objects; we only hit-test when exactly one
    // and it is a box.
    let sel = state.selected();
    if sel.len() != 1 {
        return None;
    }
    let kirkforge_draw_core::DrawObject::Box(b) = sel[0] else {
        return None;
    };
    let r = Rect {
        left: b.left,
        top: b.top,
        right: b.right,
        bottom: b.bottom,
    };
    hit_test_box_handles(r, point, tolerance)
}

/// Write `bytes` to `path` via a sibling `.tmp` file plus an atomic
/// rename. `std::fs::write` truncates the target first; if the
/// process dies (or the OS does) mid-write the on-disk file is
/// shorter than the in-memory document, which on next load is
/// either a JSON parse error or a quietly truncated diagram.
/// POSIX `rename(2)` is atomic; Windows `MoveFileEx` with
/// `MOVEFILE_REPLACE_EXISTING` (what `std::fs::rename` calls on
/// win32) is atomic on the same volume. Either way, observers
/// see either the old file or the new file — never a partial
/// one. The `.tmp` is `sync_all`'d before the rename so the bytes
/// are durable on disk by the time the rename is observable —
/// without this, a power loss after rename but before the OS
/// flushes the temp's data blocks could leave an empty or
/// truncated file at `path`. The `.tmp` sibling is best-effort
/// cleaned up on the error paths so we don't litter the user's
/// directory. Shared with the `kfd --render --output` path in
/// `main.rs` so the CLI's renumber-write and the editor's save
/// both follow the same crash-safety contract.
/// Pre-flight check for a path argument that crosses the
/// bin / OS boundary (the load path, the validate path,
/// the save path, the save-as commit path). Catches the
/// three cross-platform footguns — empty path, interior
/// NUL byte, whitespace-only path — before the OS sees
/// the path so the user gets a clean message instead of
/// an OS-specific underlying type (e.g. Linux's
/// `InvalidInput` on `open` for a NUL, Windows' silent
/// truncation at the NUL, or a confusing "no such file
/// or directory" for a filename of `"   "`). Lives in
/// `event.rs` so the load path, the save_app path, and
/// the save-as commit path share one source of truth.
///
/// Whitespace-only is a *save-as*-only input shape (the
/// save-as dialog accepts printable chars, so the user
/// can land on `"   "` after a fat-fingered Tab or
/// whitespace). The CLI's `--load` path can't produce
/// one (clap rejects before reaching us), but rejecting
/// it here too means a future fourth call site inherits
/// the same guard for free.
///
/// `pub(crate)` because the validator is a bin-internal
/// helper, not part of the bin's public surface (the
/// bin is consumed via `kfd` as a CLI, not as a
/// library).
pub(crate) fn validate_path_arg(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        anyhow::bail!("path argument is empty");
    }
    if path.trim().is_empty() {
        anyhow::bail!("path argument is whitespace-only");
    }
    if path.contains('\0') {
        anyhow::bail!("path argument contains a NUL byte");
    }
    Ok(())
}

pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    // Open explicitly so we can fsync (sync_all) before close —
    // `fs::write` doesn't expose the file handle.
    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write_result {
        // Best-effort cleanup; ignore the cleanup error so we
        // surface the original write failure.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Serialize the current document to the source path. Returns an
/// error if there's no source path (user opened with no --load) or
/// if the write/serialize fails. On success, clears the dirty bit;
/// on failure, marks dirty so the title bar tells the user that disk
/// is out of sync with their intent.
fn save_app(app: &mut App) -> Result<()> {
    let Some(path) = app.source_path.clone() else {
        anyhow::bail!("no source path (open with --load <FILE> first)");
    };
    // ponytail: guard the cross-OS path before handing it to atomic_write.
    // Empty / NUL paths here mean the user (or a Save-As commit) handed
    // us a path that didn't pass validate_path_arg's filter — share
    // the rule with render::load_doc so the load path and save path
    // reject the same shapes. Same exception type as the bail above
    // so the caller's `match` doesn't need a new arm.
    if let Err(e) = validate_path_arg(&path) {
        app.state.mark_dirty();
        return Err(e);
    }
    let json = save_document(&app.state.document)?;
    let path = std::path::PathBuf::from(path);
    if let Err(e) = atomic_write(&path, json.as_bytes()) {
        // Failed write: disk diverges from memory. Mark dirty so the
        // user sees a `*` and knows their last save intent didn't go
        // through.
        app.state.mark_dirty();
        return Err(e.into());
    }
    // No snapshot: writing the file to disk isn't a document mutation.
    // Snapshotting here would clear the redo stack (push_undo always
    // does) and leave the user unable to redo a recent undo.
    app.state.mark_saved();
    Ok(())
}

/// Copy the current selection to the OS clipboard. The serialization
/// is a JSON array of `DrawObject`s (the format `paste` reads back).
/// Arboard's clipboard is best-effort: in headless contexts it may
/// fail to open (no GUI session). We surface that as a status
/// message rather than crashing the editor.
fn copy_selected(app: &mut App) {
    let payload = app.state.serialize_selected_to_json();
    if payload == "[]" {
        app.status = "nothing to copy".into();
        return;
    }
    let count = app.state.selected_count();
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(payload) {
            Ok(()) => {
                app.status = format!("copied {} object{}", count, plural_s(count));
            }
            Err(e) => app.status = format!("clipboard write failed: {e}"),
        },
        Err(e) => app.status = format!("clipboard unavailable: {e}"),
    }
}

/// Paste from the OS clipboard. Reads the clipboard text, hands it to
/// the state's `paste_objects_from_json` (which parses, mints fresh
/// ids, nudges by +1/+1, and selects the new objects). Same
/// graceful-degrade policy as `copy_selected`.
fn paste(app: &mut App) {
    let payload = match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.get_text() {
            Ok(t) => t,
            Err(e) => {
                app.status = format!("clipboard read failed: {e}");
                return;
            }
        },
        Err(e) => {
            app.status = format!("clipboard unavailable: {e}");
            return;
        }
    };
    let new_ids = app.state.paste_objects_from_json(&payload);
    if new_ids.is_empty() {
        app.status = "nothing pasteable on clipboard".into();
    } else {
        app.status = format!("pasted {} object{}", new_ids.len(), plural_s(new_ids.len()));
    }
}

/// Cut: copy selection to the OS clipboard AND remove it from the
/// document in one undo step. The clipboard payload is the same
/// format `copy_selected` writes, so cut → paste in another session
/// round-trips cleanly. Arboard's clipboard is best-effort: in
/// headless contexts it may fail to open (no GUI session). On
/// clipboard failure we surface a status message AND roll back the
/// deletion so the user doesn't lose work to a backend hiccup.
fn cut(app: &mut App) {
    // Capture the count before the core helper clears the selection.
    let n = app.state.selected_count();
    let payload = app.state.cut_selected_to_json();
    if payload == "[]" {
        app.status = "nothing to cut".into();
        return;
    }
    match arboard::Clipboard::new() {
        Ok(mut cb) => match cb.set_text(payload) {
            Ok(()) => {
                app.status = format!("cut {} object{}", n, plural_s(n));
            }
            Err(e) => {
                // Clipboard write failed: roll back the cut so the
                // user doesn't lose work to a backend hiccup.
                app.state.undo();
                app.status = format!("clipboard write failed: {e}");
            }
        },
        Err(e) => {
            app.state.undo();
            app.status = format!("clipboard unavailable: {e}");
        }
    }
}

/// Map a Ctrl-1..8 key chord to the matching `InkColor` variant. The
/// digit is the same index the variant has in the `InkColor` enum's
/// declaration order (1=White, 2=Red, 3=Orange, 4=Yellow, 5=Green,
/// 6=Cyan, 7=Blue, 8=Magenta) — keeps the keymap grep-able against
/// the enum source.
/// Commit an in-progress palette session: pop the buffer, filter it
/// against the action table, dispatch the unique prefix match (or
/// report "no match" / "ambiguous"). An empty buffer reports
/// "palette cancelled" and dispatches nothing — pressing Enter on
/// an un-typed prompt is a no-op, the same as opening and dismissing.
///
/// ponytail: only the *first* prefix-match entry runs when the
/// filter yields multiple results. A real command palette surfaces
/// every match and uses Up/Down to walk them — that's a separate
/// tick. The single-match behavior today covers the 5-of-5 case
/// without requiring either an interactive menu widget or arrow-key
/// handling in this module.
fn commit_palette(app: &mut App) {
    let Some(state) = app.take_palette() else {
        return;
    };
    let buf = state.buffer.trim();
    if buf.is_empty() {
        app.status = "palette cancelled (empty)".into();
        return;
    }
    let matches = filter_palette(buf);
    // The filter partitions into a starts-with bucket and a
    // contains-only bucket; both can fire when a query like
    // "group" prefix-matches `group` and substring-matches
    // `ungroup`. Prefix matches are the user's intent — pick
    // them when any prefix matches exist. Falls back to the
    // substring bucket only when no prefix match exists.
    // ponytail: small structural change vs. the prior "report
    // ambiguous" branch — a flat `match` was sufficient when
    // the table was 5 unique names; the new `group` /
    // `ungroup` pair forces a tiebreak.
    let picked: Vec<_> = if matches.first().is_some_and(|(n, _)| n.starts_with(buf)) {
        matches
            .iter()
            .take_while(|(n, _)| n.starts_with(buf))
            .copied()
            .collect()
    } else {
        matches.clone()
    };
    match picked.len() {
        0 => {
            app.status = format!("no palette match for \"{buf}\"");
        }
        1 => {
            let (name, action) = picked[0];
            dispatch_palette_action(app, *action, name);
        }
        _ => {
            let names: Vec<&str> = picked.iter().map(|(n, _)| *n).collect();
            app.status = format!("ambiguous: {}", names.join(", "));
        }
    }
}

/// Map a `PaletteAction` to its existing event-loop side effect.
/// New variants must be added here AND in core's `PaletteAction`;
/// the dispatch table is intentionally identical to the chord
/// handlers (`q`, `Ctrl-S`, `Ctrl-Z`, `Ctrl-Y`, `?`) so a user
/// who learns the chord learns the palette entry for free.
/// ponytail: this is a small switch today (5 arms). If the table
/// crosses ~20 arms, lift the (action, side_effect_name) pair
/// into a table so the dispatch is a single match. Until then,
/// the compiler-checked exhaustiveness of `match` is the cheap
/// guarantee.
fn dispatch_palette_action(app: &mut App, action: PaletteAction, name: &str) {
    match action {
        PaletteAction::Help => {
            app.toggle_help();
            app.status = format!("palette: {name} → help toggled");
        }
        PaletteAction::ToggleLayers => {
            let was_open = app.show_layers;
            app.toggle_layers();
            let now = if app.show_layers { "open" } else { "closed" };
            app.status = format!(
                "palette: {name} → layers {now} (was {})",
                if was_open { "open" } else { "closed" }
            );
        }
        PaletteAction::Save => {
            // Mirror the Ctrl-S keymap contract: a fresh
            // document (no source_path) cannot be saved to
            // a known path, so fall through to save-as. The
            // user types ":save" or hits Ctrl-S — both must
            // behave identically. Ctrl-Shift-S remains the
            // explicit rename.
            if app.source_path.is_none() {
                app.begin_save_as();
            } else {
                match save_app(app) {
                    Ok(()) => {
                        app.status = format!(
                            "palette: {name} → saved {}",
                            app.source_path.as_deref().unwrap_or("?")
                        );
                    }
                    Err(e) => app.status = format!("palette: {name} → save failed: {e}"),
                }
            }
        }
        PaletteAction::Undo => {
            if app.state.undo() {
                app.status = format!("palette: {name} → undid");
            } else {
                app.status = "nothing to undo".into();
            }
        }
        PaletteAction::Redo => {
            if app.state.redo() {
                app.status = format!("palette: {name} → redid");
            } else {
                app.status = "nothing to redo".into();
            }
        }
        PaletteAction::Duplicate => {
            // Reuse the same state method as Ctrl-D — empty
            // selection reports "nothing to duplicate" via the
            // returned Vec being empty.
            let new_ids = app.state.duplicate_selected();
            if new_ids.is_empty() {
                app.status = format!("palette: {name} → nothing to duplicate");
            } else {
                app.status = format!("palette: {name} → duplicated {} object(s)", new_ids.len());
            }
        }
        PaletteAction::Group => {
            // `group_selection` returns the new parent id, or None
            // on empty selection (which already reports on the
            // status line via group_selection's own messaging).
            // Here we add the palette-prefix so the user can tell
            // it was a palette invocation.
            let was_empty = app.state.selected_count() == 0;
            if was_empty {
                app.status = "palette: group → nothing to group".into();
            } else {
                match app.state.group_selection() {
                    Some(parent) => {
                        app.status = format!("palette: group → grouped under {parent}");
                    }
                    None => {
                        app.status = "palette: group → nothing to group".into();
                    }
                }
            }
        }
        PaletteAction::Ungroup => {
            let n = app.state.ungroup_selection();
            if n == 0 {
                app.status = format!("palette: {name} → nothing to ungroup");
            } else {
                app.status = format!("palette: {name} → ungrouped {n} object(s)");
            }
        }
        PaletteAction::SelectAll => {
            let n = app.state.select_all();
            app.status = match n {
                0 => format!("palette: {name} → nothing to select"),
                n => format!("palette: {name} → selected {n} object(s)"),
            };
        }
        PaletteAction::Delete => {
            // Routes through the same `state.delete_selected()`
            // helper as the Delete / Backspace chord so the
            // two paths share the resize-target guard and the
            // single-undo-step behavior. The helper returns
            // the count of removed selection entries (a small
            // change vs. the prior `()` return) so the bin
            // can echo a "deleted N object(s)" message. Both
            // the chord and the palette echo
            // "nothing to delete" on empty selection — the
            // prior asymmetry (palette echoed, chord was
            // silent) was unintentional and now matches.
            let n = app.state.delete_selected();
            app.status = match n {
                0 => format!("palette: {name} → nothing to delete"),
                n => format!("palette: {name} → deleted {n} object(s)"),
            };
        }
        PaletteAction::ToggleInspector => {
            let was_open = app.show_inspector;
            app.toggle_inspector();
            let now = if app.show_inspector { "open" } else { "closed" };
            app.status = format!(
                "palette: {name} → inspector {now} (was {})",
                if was_open { "open" } else { "closed" }
            );
        }
        PaletteAction::AlignLeft => {
            palette_align(app, name, kirkforge_draw_core::Align::Left);
        }
        PaletteAction::AlignRight => {
            palette_align(app, name, kirkforge_draw_core::Align::Right);
        }
        PaletteAction::AlignTop => {
            palette_align(app, name, kirkforge_draw_core::Align::Top);
        }
        PaletteAction::AlignBottom => {
            palette_align(app, name, kirkforge_draw_core::Align::Bottom);
        }
        PaletteAction::AlignHorizontalCenter => {
            palette_align(app, name, kirkforge_draw_core::Align::HorizontalCenter);
        }
        PaletteAction::AlignVerticalCenter => {
            palette_align(app, name, kirkforge_draw_core::Align::VerticalCenter);
        }
        PaletteAction::DistributeHorizontal => {
            distribute_selection(app, kirkforge_draw_core::DistributeAxis::Horizontal);
            // Status is overwritten by `distribute_selection`'s
            // own format. Re-stamp the palette prefix so the user
            // can tell the action came from the palette rather
            // than the Ctrl-Shift-J chord. ponytail: doing it
            // by string-prefix instead of a return tuple because
            // `distribute_selection` already wraps the user-
            // facing message — adding a "palette: …" prefix in
            // place is a one-liner that doesn't unwind the
            // helper's contract.
            app.status = format!("palette: {name} → {}", app.status);
        }
        PaletteAction::DistributeVertical => {
            distribute_selection(app, kirkforge_draw_core::DistributeAxis::Vertical);
            app.status = format!("palette: {name} → {}", app.status);
        }
        PaletteAction::Quit => app.request_quit(),
    }
}

/// Palette-prefix wrapper around `align_selection`. The helper
/// itself formats a status line like "aligned 3 objects to left
/// edge"; we add the "palette: <name> → " prefix here so the
/// user can tell the action came from the palette rather than
/// the Ctrl-Shift-<letter> chord.
fn palette_align(app: &mut App, name: &str, how: kirkforge_draw_core::Align) {
    align_selection(app, how);
    app.status = format!("palette: {name} → {}", app.status);
}

fn ink_color_for_digit(digit: char) -> kirkforge_draw_core::InkColor {
    use kirkforge_draw_core::InkColor;
    match digit {
        '1' => InkColor::White,
        '2' => InkColor::Red,
        '3' => InkColor::Orange,
        '4' => InkColor::Yellow,
        '5' => InkColor::Green,
        '6' => InkColor::Cyan,
        '7' => InkColor::Blue,
        _ => InkColor::Magenta, // '8'
    }
}

/// Next variant in the InkColor enum's discriminant order,
/// wrapping Magenta → White. Matches the Ctrl-1..8 digit
/// mapping in `ink_color_for_digit` so a user cycling from
/// White reaches White again after exactly 8 presses.
fn next_ink_color(color: kirkforge_draw_core::InkColor) -> kirkforge_draw_core::InkColor {
    use kirkforge_draw_core::InkColor;
    match color {
        InkColor::White => InkColor::Red,
        InkColor::Red => InkColor::Orange,
        InkColor::Orange => InkColor::Yellow,
        InkColor::Yellow => InkColor::Green,
        InkColor::Green => InkColor::Cyan,
        InkColor::Cyan => InkColor::Blue,
        InkColor::Blue => InkColor::Magenta,
        InkColor::Magenta => InkColor::White,
    }
}

/// Repaint the current selection in the chosen color. Status bar
/// reports the count of objects that actually changed (recoloring a
/// White selection back to White is a silent no-op so the user can
/// spam the color palette without churning the undo stack). Empty
/// selection is a "nothing to recolor" status.
fn recolor_selection(app: &mut App, color: kirkforge_draw_core::InkColor) {
    let n = app.state.recolor_selection(color);
    if n == 0 {
        if app.state.selected_count() == 0 {
            app.status = "nothing to recolor".into();
        } else {
            app.status = format!("already {}", color_name(color));
        }
    } else {
        app.status = format!(
            "recolored {} object{} to {}",
            n,
            plural_s(n),
            color_name(color)
        );
    }
}

/// Cycle the selection's color one step forward through the
/// InkColor enum's discriminant order, wrapping Magenta back
/// to White. The "from" color is the first selected object's
/// color (selection is document-order, so the first hit is
/// deterministic). Mirrors Ctrl-1..8 but advances one
/// variant per press instead of jumping to a specific one —
/// useful when the user just wants "next color" without
/// remembering which digit maps to which variant. Multi-
/// select collapses to one undo step via
/// `recolor_selection`. Empty selection is a "nothing to
/// recolor" status, matching Ctrl-1..8's empty-selection
/// message.
///
/// ponytail: forward-only. A backward cycle would need a
/// second chord and Shift+I conflicts with the inspector
/// toggle. The Ctrl-1..8 cluster is the "jump to a specific
/// color" gesture; `i` is the "next color" gesture. The
/// `recolor_selection` short-circuit (no-op when the
/// selection is already at the target color) cannot fire
/// from this path — the target is `next(from)`, so a
/// selection where every object already equals `next(from)`
/// would require `from == next(from)`, which is impossible
/// for the InkColor enum.
fn cycle_selection_color(app: &mut App) {
    let Some(from) = app.state.selected().into_iter().next().map(|o| o.color()) else {
        app.status = "nothing to recolor".into();
        return;
    };
    let next = next_ink_color(from);
    let n = app.state.recolor_selection(next);
    app.status = format!(
        "recolored {} object{} to {}",
        n,
        plural_s(n),
        color_name(next)
    );
}

/// Ctrl-G handler. Wraps the core `group_selection` helper so
/// the bin owns the status-bar message (core stays pure and
/// only knows how to mutate). Empty selection → "nothing to
/// group"; otherwise echo the new parent id so the user can
/// confirm the chord took without opening the layers panel.
fn group_selection(app: &mut App) {
    match app.state.group_selection() {
        Some(parent) => {
            app.status = format!(
                "grouped {} object{} (parent={parent})",
                app.state.selected_count(),
                if app.state.selected_count() == 1 {
                    ""
                } else {
                    "s"
                },
            );
        }
        None => {
            app.status = "nothing to group".into();
        }
    }
}

/// Ctrl-Shift-G handler. Wraps `ungroup_selection`. Empty
/// selection → "nothing to ungroup"; nothing in selection is
/// actually grouped → "nothing to ungroup" (the core helper
/// reports zero on that case so we don't churn the undo stack
/// for a no-op spam); otherwise echo the count cleared.
fn ungroup_selection(app: &mut App) {
    let n = app.state.ungroup_selection();
    if n == 0 {
        app.status = "nothing to ungroup".into();
    } else {
        app.status = format!("ungrouped {} object{}", n, plural_s(n),);
    }
}

/// Pretty name for status-bar messages. Kept here (not in core) since
/// it's a UI concern, not part of the document model.
fn color_name(color: kirkforge_draw_core::InkColor) -> &'static str {
    use kirkforge_draw_core::InkColor;
    match color {
        InkColor::White => "white",
        InkColor::Red => "red",
        InkColor::Orange => "orange",
        InkColor::Yellow => "yellow",
        InkColor::Green => "green",
        InkColor::Cyan => "cyan",
        InkColor::Blue => "blue",
        InkColor::Magenta => "magenta",
    }
}

/// Cycle to the next `LineStyle` in enum-discriminant order. The
/// order matches the visual jump-cut (Smooth → Light → Double →
/// Dashed → Smooth) the user gets from repeated keypresses.
fn next_line_style(s: kirkforge_draw_core::LineStyle) -> kirkforge_draw_core::LineStyle {
    use kirkforge_draw_core::LineStyle;
    match s {
        LineStyle::Smooth => LineStyle::Light,
        LineStyle::Light => LineStyle::Double,
        LineStyle::Double => LineStyle::Dashed,
        LineStyle::Dashed => LineStyle::Smooth,
    }
}

/// Map a `LineStyle` to its pretty name for status messages.
fn line_style_name(s: kirkforge_draw_core::LineStyle) -> &'static str {
    use kirkforge_draw_core::LineStyle;
    match s {
        LineStyle::Smooth => "smooth",
        LineStyle::Light => "light",
        LineStyle::Double => "double",
        LineStyle::Dashed => "dashed",
    }
}

/// Cycle `LineStyle` on every selected Line / Elbow to the next
/// variant. The pure helper `restyle_selection` collapses to a
/// single undo step for the batch and silently skips objects that
/// don't carry a `LineStyle` (boxes have `BoxStyle`, paint / text
/// have none). Status mirrors the recolor style: count + new
/// style, "already <style>" if every selected object is already
/// at the target, "nothing to restyle" if the selection is empty
/// or contains no styled objects.
fn cycle_line_style(app: &mut App) {
    // Pick the next style from the first styled selected object so
    // the cycle is consistent across the batch — every selected
    // line / elbow ends up at the same target style.
    let Some(next) = app
        .state
        .document
        .objects
        .iter()
        .find(|o| {
            app.state.selected().iter().any(|s| s.id() == o.id())
                && matches!(o, DrawObject::Line(_) | DrawObject::Elbow(_))
        })
        .and_then(|o| match o {
            DrawObject::Line(l) => Some(l.style),
            DrawObject::Elbow(e) => Some(e.style),
            // ponytail: the outer `find` filter already
            // restricts to Line | Elbow via `matches!`. The
            // wildcard is unreachable in practice; kept
            // because Rust's `&DrawObject` borrow doesn't
            // carry the type-narrowing into the closure.
            _ => None,
        })
        .map(next_line_style)
    else {
        app.status = if app.state.selected_count() == 0 {
            "nothing to restyle".into()
        } else {
            "no lines / elbows in selection".into()
        };
        return;
    };
    let n = app.state.restyle_selection(next);
    if n == 0 {
        app.status = format!("already {}", line_style_name(next));
    } else {
        app.status = status_n_objects_to(n, "restyled", line_style_name(next));
    }
}

/// Cycle to the next `BoxStyle` in enum-discriminant order. The
/// order matches the visual jump-cut (Light → Heavy → Double →
/// Dashed → Auto → Light) the user gets from repeated keypresses.
/// Auto sits last in the rotation (after the four named styles) so
/// the user can step back to a "let the renderer pick" state
/// without it appearing as the first option in any status echo.
fn next_box_style(s: kirkforge_draw_core::BoxStyle) -> kirkforge_draw_core::BoxStyle {
    use kirkforge_draw_core::BoxStyle;
    match s {
        BoxStyle::Light => BoxStyle::Heavy,
        BoxStyle::Heavy => BoxStyle::Double,
        BoxStyle::Double => BoxStyle::Dashed,
        BoxStyle::Dashed => BoxStyle::Auto,
        BoxStyle::Auto => BoxStyle::Light,
    }
}

/// Map a `BoxStyle` to its pretty name for status messages.
fn box_style_name(s: kirkforge_draw_core::BoxStyle) -> &'static str {
    use kirkforge_draw_core::BoxStyle;
    match s {
        BoxStyle::Light => "light",
        BoxStyle::Heavy => "heavy",
        BoxStyle::Double => "double",
        BoxStyle::Dashed => "dashed",
        BoxStyle::Auto => "auto",
    }
}

/// Cycle `BoxStyle` on every selected Box to the next variant.
/// The pure helper `restyle_boxes_selection` collapses to a
/// single undo step for the batch and silently skips objects
/// that don't carry a `BoxStyle` (lines have LineStyle, paint /
/// text have none). Status mirrors `cycle_line_style`:
/// count + new style, "already <style>" if every selected
/// object is already at the target, "nothing to restyle" /
/// "no boxes in selection" depending on whether the selection
/// is empty or just contains non-Box shapes.
fn cycle_box_style(app: &mut App) {
    let Some(next) = app
        .state
        .document
        .objects
        .iter()
        .find(|o| {
            app.state.selected().iter().any(|s| s.id() == o.id()) && matches!(o, DrawObject::Box(_))
        })
        .and_then(|o| match o {
            DrawObject::Box(b) => Some(b.style),
            // ponytail: the outer `find` filter already
            // restricts to Box via `matches!`. The wildcard
            // is unreachable in practice; kept because
            // Rust's `&DrawObject` borrow doesn't carry
            // the type-narrowing into the closure.
            _ => None,
        })
        .map(next_box_style)
    else {
        app.status = if app.state.selected_count() == 0 {
            "nothing to restyle".into()
        } else {
            "no boxes in selection".into()
        };
        return;
    };
    let n = app.state.restyle_boxes_selection(next);
    if n == 0 {
        app.status = format!("already {}", box_style_name(next));
    } else {
        app.status = status_n_objects_to(n, "restyled", box_style_name(next));
    }
}

/// Next variant of `TextBorderMode` in enum source order, wrapping
/// at the end. Source order is `None → Single → Double → Underline
/// → None` — same shape as the L / B cycle arms above. Used by the
/// Ctrl-Alt-T bin arm; the pure helper is a stand-alone function
/// so the wrap arithmetic is unit-testable without an `App`.
fn next_text_border(s: kirkforge_draw_core::TextBorderMode) -> kirkforge_draw_core::TextBorderMode {
    use kirkforge_draw_core::TextBorderMode;
    match s {
        TextBorderMode::None => TextBorderMode::Single,
        TextBorderMode::Single => TextBorderMode::Double,
        TextBorderMode::Double => TextBorderMode::Underline,
        TextBorderMode::Underline => TextBorderMode::None,
    }
}

/// Pretty name for the status bar.
fn text_border_name(s: kirkforge_draw_core::TextBorderMode) -> &'static str {
    use kirkforge_draw_core::TextBorderMode;
    match s {
        TextBorderMode::None => "none",
        TextBorderMode::Single => "single",
        TextBorderMode::Double => "double",
        TextBorderMode::Underline => "underline",
    }
}

/// Cycle the active `TextBorderMode` (the draft-time setting
/// for new Text objects). Tool-state operation, not a
/// selection mutation — there's no "restyle existing text"
/// primitive yet, so the chord just rotates the value future
/// drafts will inherit. Status echoes the new border name.
fn cycle_text_border(app: &mut App) {
    let next = next_text_border(app.state.text_border);
    app.state.set_text_border(next);
    app.status = format!("text border: {}", text_border_name(next));
}

/// The paint brush palette cycled through by Ctrl-Alt-P. Eight
/// entries — same cardinality as the recolor cluster (1..8).
/// Ordered "thin / clean → thick / noisy": the middle dot
/// (`·`) is the default and reads as a fine pencil mark;
/// `o` and `*` are open-loop stamps; `x` and `█` are
/// closed/filled; `▒`, `░`, `▓` are dithered textures at
/// increasing density. Custom brushes (any other string the
/// user might have typed into the field) cycle back to the
/// start so the loop is always closed.
/// ponytail: hardcoded list, not an enum. `brush: String`
/// on `DrawState` is intentionally untyped so the user can
/// type a single-cell character; a future "brush picker"
/// tick can replace this list with the same shape (or grow
/// it to whatever subset the picker shows).
const BRUSH_PALETTE: &[&str] = &["·", "o", "*", "x", "█", "▒", "░", "▓"];

/// Next brush in the palette, wrapping at the end. Unknown
/// brushes (anything not in `BRUSH_PALETTE`) cycle back to
/// the first entry so a custom brush doesn't strand the
/// user — they get a known glyph on the next press and can
/// keep going from there.
fn next_brush(s: &str) -> &'static str {
    BRUSH_PALETTE
        .iter()
        .position(|b| *b == s)
        .map(|i| BRUSH_PALETTE[(i + 1) % BRUSH_PALETTE.len()])
        .unwrap_or(BRUSH_PALETTE[0])
}

/// Cycle the active paint brush. Tool-state operation —
/// the chord rotates the glyph future Paint drafts will
/// stamp. Status echoes the new glyph. The status bar
/// message uses the literal glyph so the user sees what
/// they'll draw next, not a description.
fn cycle_brush(app: &mut App) {
    let next = next_brush(&app.state.brush);
    app.state.set_brush(next);
    app.status = format!("paint brush: {next}");
}

fn align_selection(app: &mut App, how: kirkforge_draw_core::Align) {
    let n = app.state.align_selection(how);
    app.status = match n {
        0 => "nothing to align".into(),
        n => status_n_objects_to(n, "aligned", align_name(how)),
    };
}

fn align_name(how: kirkforge_draw_core::Align) -> &'static str {
    match how {
        kirkforge_draw_core::Align::Left => "left edge",
        kirkforge_draw_core::Align::Right => "right edge",
        kirkforge_draw_core::Align::Top => "top edge",
        kirkforge_draw_core::Align::Bottom => "bottom edge",
        kirkforge_draw_core::Align::HorizontalCenter => "horizontal center",
        kirkforge_draw_core::Align::VerticalCenter => "vertical center",
    }
}

fn distribute_selection(app: &mut App, axis: kirkforge_draw_core::DistributeAxis) {
    let n = app.state.distribute_selection(axis);
    app.status = match n {
        0 => "nothing to distribute".into(),
        n => status_n_objects_to(n, "distributed", distribute_name(axis)),
    };
}

fn distribute_name(axis: kirkforge_draw_core::DistributeAxis) -> &'static str {
    match axis {
        kirkforge_draw_core::DistributeAxis::Horizontal => "equal horizontal spacing",
        kirkforge_draw_core::DistributeAxis::Vertical => "equal vertical spacing",
    }
}

fn invert_selection(app: &mut App) {
    let n = app.state.invert_selection();
    app.status = if n == 0 {
        "selection inverted (now empty)".into()
    } else {
        format!("inverted selection ({n} object{} selected)", plural_s(n))
    };
}

#[cfg(test)]
mod tests {
    // The category sub-modules each `use super::*;` to pull the
    // production items (re-exported here via the parent's
    // `use super::*` is implicit — actually each sub-module
    // brings its own `use super::*;`), and
    // `use crate::event::tests::common::*;` for the shared
    // helpers. The production items (handle_key, handle_mouse,
    // save_app, atomic_write, HELP_LINES, etc.) are private to
    // `event::mod.rs`; the `tests` mod's `use super::*;` below
    // brings them into the `tests` namespace, and each
    // sub-module's `use super::*;` re-pulls them from here.
    use super::*;

    pub(crate) mod align;
    pub(crate) mod common;
    pub(crate) mod find;
    pub(crate) mod grouping;
    pub(crate) mod inspector;
    pub(crate) mod keyboard;
    pub(crate) mod layers;
    pub(crate) mod mouse;
    pub(crate) mod palette;
    pub(crate) mod restyle;
    pub(crate) mod save;
    pub(crate) mod text_edit;
}
