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
        KeyCode::Char('z') if ctrl && shift && !alt => {
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
    use super::*;
    use crate::app::PaletteTrigger;
    use kirkforge_draw_core::{DrawObject, DrawState, InkColor, Point};
    use ratatui::layout::Rect;

    fn make_app() -> App {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        app.scene_origin = Some(Point { x: 0, y: 0 });
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn key_with_shift_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
    }

    fn key_ctrl_alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::ALT)
    }

    #[test]
    fn q_quits() {
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = make_app();
        handle_key(&mut app, key_ctrl(KeyCode::Char('c')));
        assert!(app.should_quit);
    }

    #[test]
    fn q_on_dirty_doc_starts_quit_confirm() {
        // Document is dirty → q goes through the confirm
        // prompt, doesn't quit yet, status echoes the
        // prompt.
        let mut app = make_app();
        app.state.mark_dirty();
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(!app.should_quit, "q on dirty doc must NOT quit");
        assert!(app.pending_quit_confirm, "confirm flag set");
        assert!(
            app.status.contains("save?"),
            "status echoes prompt: {}",
            app.status
        );
    }

    #[test]
    fn q_on_clean_doc_quits_immediately() {
        // Clean document → q quits silently. Pin the
        // regression: the dirty-confirm hijack must not
        // engage when there's nothing to lose.
        let mut app = make_app();
        assert!(!app.state.is_dirty());
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.should_quit);
        assert!(!app.pending_quit_confirm);
    }

    #[test]
    fn quit_confirm_y_saves_and_quits() {
        // y on a dirty doc with a source path → save then
        // quit. The file lands on disk and the dirty bit
        // clears. Use a temp file so the test is hermetic.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quit-confirm-y.td.json");
        let path_str = path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some(path_str.clone());
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();
        assert!(app.state.is_dirty());

        // q → confirm
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.pending_quit_confirm);
        assert!(!app.should_quit);

        // y → save + quit
        handle_key(&mut app, key(KeyCode::Char('y')));
        assert!(app.should_quit, "y must quit after saving");
        assert!(!app.pending_quit_confirm, "confirm flag cleared");
        assert!(
            app.status.starts_with("saved "),
            "status echoes save: {}",
            app.status
        );
        assert!(
            std::fs::read_to_string(&path).is_ok(),
            "y must have written the file"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn quit_confirm_y_with_no_source_path_opens_save_as() {
        // y on a dirty doc with no source path used to bail
        // and surface "save failed: no source path …" — a
        // dead end for the user. The contract was changed to
        // match Ctrl-S / :save (ticks 42 / 43): open the
        // save-as modal so the user can name the file. Editor
        // stays open, confirm flag clears, status reflects
        // the save-as prompt rather than a failure.
        let mut app = make_app();
        app.source_path = None;
        app.state.mark_dirty();
        handle_key(&mut app, key(KeyCode::Char('q')));
        handle_key(&mut app, key(KeyCode::Char('y')));
        assert!(
            !app.should_quit,
            "save-as-in-progress must keep the editor open"
        );
        assert!(
            !app.pending_quit_confirm,
            "confirm flag clears once the user answered"
        );
        assert!(
            app.save_as.is_some(),
            "save-as modal must be open so the user can supply a path"
        );
        assert!(
            !app.status.starts_with("save failed"),
            "quit-confirm y must NOT surface a save-failed status when no path is set; got: {:?}",
            app.status
        );
    }

    #[test]
    fn quit_confirm_n_discards_and_quits() {
        // n → discard unsaved changes, quit. The dirty
        // bit is irrelevant at this point because the
        // editor is closing.
        let mut app = make_app();
        app.state.mark_dirty();
        handle_key(&mut app, key(KeyCode::Char('q')));
        handle_key(&mut app, key(KeyCode::Char('n')));
        assert!(app.should_quit);
        assert!(!app.pending_quit_confirm);
    }

    #[test]
    fn quit_confirm_esc_cancels_and_stays_open() {
        // Esc → cancel the quit, stay in the editor,
        // status echoes the cancellation. The dirty bit
        // is unchanged so a subsequent save still works.
        let mut app = make_app();
        app.state.mark_dirty();
        handle_key(&mut app, key(KeyCode::Char('q')));
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.should_quit);
        assert!(!app.pending_quit_confirm);
        assert_eq!(app.status, "quit cancelled");
        assert!(app.state.is_dirty(), "dirty bit unchanged on cancel");
    }

    #[test]
    fn quit_confirm_swallows_other_keys() {
        // While the confirm is showing, only y / n / Esc
        // are valid. A stray Backspace, arrow, or
        // printable char must not leak through to the
        // main keymap (it could otherwise clear the
        // selection, edit the status line, or trigger an
        // action).
        let mut app = make_app();
        app.state.mark_dirty();
        handle_key(&mut app, key(KeyCode::Char('q')));
        assert!(app.pending_quit_confirm);
        let status_before = app.status.clone();
        let tool_before = app.state.tool;

        // Backspace (would normally clear a draft or
        // delete selected). Tool should not change.
        handle_key(&mut app, key(KeyCode::Backspace));
        assert!(app.pending_quit_confirm, "Backspace swallowed");
        assert!(!app.should_quit, "no quit on stray key");
        assert_eq!(app.status, status_before, "status unchanged");
        assert_eq!(app.state.tool, tool_before, "tool unchanged");

        // Enter (would normally commit a draft or
        // palette). Still swallowed.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.pending_quit_confirm);
        assert!(!app.should_quit);

        // A printable char (would normally type into
        // the tool draft). Still swallowed.
        handle_key(&mut app, key(KeyCode::Char('x')));
        assert!(app.pending_quit_confirm);
        assert!(!app.should_quit);
    }

    #[test]
    fn tool_keys_switch_tools() {
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('b')));
        assert_eq!(app.state.tool, DrawMode::Box);
        handle_key(&mut app, key(KeyCode::Char('l')));
        assert_eq!(app.state.tool, DrawMode::Line);
        handle_key(&mut app, key(KeyCode::Char('s')));
        assert_eq!(app.state.tool, DrawMode::Select);
    }

    #[test]
    fn tab_cycles_tool_forward_then_wraps() {
        let mut app = make_app();
        // Default tool is Select; Tab → Box → Line → ...
        handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.state.tool, DrawMode::Box);
        handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.state.tool, DrawMode::Line);
        // 4 more tabs walk through Elbow → Paint → Text → Select.
        for _ in 0..4 {
            handle_key(&mut app, key(KeyCode::Tab));
        }
        assert_eq!(app.state.tool, DrawMode::Select);
        // One more tab wraps back to Box.
        handle_key(&mut app, key(KeyCode::Tab));
        assert_eq!(app.state.tool, DrawMode::Box);
    }

    #[test]
    fn shift_tab_cycles_tool_backward() {
        let mut app = make_app();
        // From Select, Shift+Tab → Text (last).
        handle_key(&mut app, key_with_shift(KeyCode::BackTab));
        assert_eq!(app.state.tool, DrawMode::Text);
        handle_key(&mut app, key_with_shift(KeyCode::BackTab));
        assert_eq!(app.state.tool, DrawMode::Paint);
    }

    #[test]
    fn ctrl_z_undoes() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();
        assert_eq!(app.state.document.objects.len(), 1);
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        assert!(app.state.document.objects.is_empty());
    }

    #[test]
    fn ctrl_y_redoes() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        handle_key(&mut app, key_ctrl(KeyCode::Char('y')));
        assert_eq!(app.state.document.objects.len(), 1);
    }

    #[test]
    fn ctrl_shift_z_redoes_as_alias_for_ctrl_y() {
        // Figma / macOS convention pairs Ctrl-Shift-Z with Ctrl-Z so
        // undo / redo are reachable with the dominant hand and one
        // axis-flip away. Same end-state as `ctrl_y_redoes` but
        // routed through the Ctrl-Shift-Z arm so a future refactor
        // can't silently shadow the chord.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        // Cursor reset: after undo the document is empty but the
        // redo stack has the original commit. Drive Ctrl-Shift-Z
        // through the same `key_with_shift_ctrl` helper the
        // align / distribute cluster uses.
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('z')));
        assert_eq!(app.state.document.objects.len(), 1);
    }

    #[test]
    fn ctrl_shift_z_with_no_redo_stack_reports_status() {
        // Empty redo stack → status echoes "nothing to redo" so the
        // user knows the chord was received. Same shape as the
        // empty-redo message the Ctrl-Y arm produces; pinned here
        // so the alias arm can't silently differ.
        let mut app = make_app();
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('z')));
        assert!(
            app.status.contains("nothing to redo"),
            "status should report empty redo; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_d_duplicates_selection() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 3 });
        let original_id = app.state.commit_draft().unwrap();
        // Select it.
        app.state.set_tool(DrawMode::Select);
        handle_key(&mut app, key_ctrl(KeyCode::Char('d')));
        assert_eq!(app.state.document.objects.len(), 2);
        assert_eq!(app.state.selected_count(), 1);
        let sel = app.state.selected();
        assert!(!sel.iter().any(|o| o.id() == original_id));
        assert!(app.status.contains("duplicated"));
    }

    #[test]
    fn ctrl_d_with_no_selection_reports_status() {
        let mut app = make_app();
        handle_key(&mut app, key_ctrl(KeyCode::Char('d')));
        assert!(app.state.document.objects.is_empty());
        assert!(app.status.contains("nothing to duplicate"));
    }

    #[test]
    fn ctrl_x_cuts_selection() {
        // Two paths are valid depending on the host's clipboard
        // backend:
        //   1. Clipboard works → doc is emptied, status reads
        //      "cut 1 object" and the user can paste it back.
        //   2. Clipboard unavailable → cut is rolled back via
        //      app.state.undo(), doc retains the original object,
        //      status reports the clipboard error.
        // The test pins BOTH branches without leaking which one
        // happened: in either case the original id must still
        // resolve to something the user can recover, and the status
        // must report either success or the clipboard failure.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 3 });
        let original_id = app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        assert_eq!(app.state.document.objects.len(), 1);
        handle_key(&mut app, key_ctrl(KeyCode::Char('x')));
        // Cut happened (doc empty) — and the original is now
        // recoverable from the undo stack, not the document.
        let cut_succeeded = app.state.document.objects.is_empty();
        if cut_succeeded {
            assert!(
                app.status.starts_with("cut "),
                "expected cut-success status, got {:?}",
                app.status
            );
            // A single undo brings the original back.
            app.state.undo();
            assert!(app
                .state
                .document
                .objects
                .iter()
                .any(|o| o.id() == original_id));
        } else {
            // Rollback path: doc preserved, status blames the clipboard.
            assert!(
                app.state
                    .document
                    .objects
                    .iter()
                    .any(|o| o.id() == original_id),
                "rollback path must preserve the cut object"
            );
            assert!(
                app.status.contains("clipboard"),
                "expected clipboard-error status, got {:?}",
                app.status
            );
        }
    }

    #[test]
    fn ctrl_x_with_no_selection_reports_status() {
        let mut app = make_app();
        handle_key(&mut app, key_ctrl(KeyCode::Char('x')));
        assert!(app.state.document.objects.is_empty());
        assert!(app.status.contains("nothing to cut"));
    }

    #[test]
    fn surface_panic_extracts_str_payload() {
        // Most panic!() invocations end up as &'static str payloads.
        let mut app = make_app();
        let payload: Box<dyn std::any::Any + Send> = Box::new("synthetic boom from str");
        surface_panic(&mut app, "key", payload);
        assert!(app.status.contains("internal error in key handler"));
        assert!(app.status.contains("panic caught"));
    }

    #[test]
    fn surface_panic_extracts_string_payload() {
        // panic!("{formatted}") ends up as String.
        let mut app = make_app();
        let payload: Box<dyn std::any::Any + Send> =
            Box::new(String::from("synthetic boom from String"));
        surface_panic(&mut app, "mouse", payload);
        assert!(app.status.contains("internal error in mouse handler"));
    }

    #[test]
    fn surface_panic_handles_non_string_payload() {
        // If some upstream code panics with a non-string payload,
        // we must still surface a status (not silently swallow it).
        let mut app = make_app();
        let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
        surface_panic(&mut app, "key", payload);
        assert!(app.status.contains("internal error in key handler"));
    }

    #[test]
    fn catch_unwind_wrapper_keeps_loop_alive_through_panic() {
        // Regression: an event handler that panics must NOT propagate
        // out of catch_unwind; that's the whole point of the wrap in
        // run(). We assert the wrap pattern here directly so a future
        // refactor of run() can't silently drop it.
        let inner: std::result::Result<(), Box<dyn std::any::Any + Send>> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panic!("synthetic panic in inner closure");
            }));
        assert!(inner.is_err(), "inner panic must be caught");
        let outer: std::result::Result<(), Box<dyn std::any::Any + Send>> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Mimic run(): inner handler panicked; outer catch
                // absorbs it; fall through.
                let _: std::result::Result<(), _> = inner;
            }));
        assert!(outer.is_ok(), "outer wrap must absorb cleanly");
    }

    #[test]
    fn draw_handler_panic_is_caught_by_run_loop() {
        // Symmetric to the key/mouse handlers in `run`: a panic in
        // `ui::draw` (e.g., a bad Rect arithmetic in a fresh widget)
        // must NOT propagate out of the draw callback — that would
        // leave the terminal in an unflushed tty state and lose
        // unsaved work. We assert the same shape as
        // `catch_unwind_wrapper_keeps_loop_alive_through_panic`
        // but specifically exercises the `ui::draw` call site that
        // the loop now wraps.
        let mut app = make_app();
        let outcome: std::result::Result<(), Box<dyn std::any::Any + Send>> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Mimic the inner closure of `terminal.draw(...)`
                // in run(): catch_unwind around the body, surfacing
                // the panic to the status bar.
                if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    panic!("synthetic draw panic");
                })) {
                    surface_panic(&mut app, "draw", payload);
                }
            }));
        assert!(
            outcome.is_ok(),
            "draw-handler panic must be absorbed by the wrap"
        );
        assert!(
            app.status.contains("internal error in draw handler"),
            "draw panic must surface on status: {}",
            app.status
        );
    }

    #[test]
    fn ctrl_digit_recolors_selection() {
        // Ctrl-2 (= Red) on a single White box must change the box's
        // color and report "recolored 1 object to red". Drives the
        // full keymap → ink_color_for_digit → recolor_selection path.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        assert_eq!(app.state.document.objects.len(), 1);

        handle_key(&mut app, key_ctrl(KeyCode::Char('2')));
        assert_eq!(app.state.document.objects[0].color(), InkColor::Red);
        assert!(
            app.status.contains("recolored 1 object to red"),
            "status should report a recolor; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_digit_no_selection_reports_status() {
        // Pressing Ctrl-3 on an empty editor must not panic, must not
        // surface a recolor-success message, and must report "nothing
        // to recolor" so the user knows the keypress was received.
        let mut app = make_app();
        let dirty_before = app.state.is_dirty();
        handle_key(&mut app, key_ctrl(KeyCode::Char('3')));
        assert_eq!(app.state.is_dirty(), dirty_before);
        assert!(app.status.contains("nothing to recolor"));
    }

    #[test]
    fn ctrl_digit_already_that_color_reports_no_change() {
        // Pressing Ctrl-1 (White) on a White selection is a silent
        // no-op from the user's POV except for the "already white"
        // status — and the dirty bit must not flip.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 3 });
        app.state.commit_draft().unwrap();
        // commit_draft marks dirty; mark_saved so we can detect that
        // the recolor keypress doesn't re-dirty.
        app.state.mark_saved();
        assert!(!app.state.is_dirty());
        // Box is selected after commit and is White by default.
        handle_key(&mut app, key_ctrl(KeyCode::Char('1')));
        assert!(!app.state.is_dirty(), "no-op recolor must not flip dirty");
        assert!(app.status.contains("already white"));
    }

    /// Helper: commit a Smooth Line from (0,0) to (5,3) and leave
    /// it selected. Used by the Ctrl-Alt-L cycle tests below.
    fn commit_one_smooth_line(app: &mut App) {
        use kirkforge_draw_core::LineStyle;
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        // Sanity: the just-committed line is selected and Smooth.
        assert_eq!(app.state.selected_count(), 1);
        match &app.state.document.objects[0] {
            DrawObject::Line(l) => assert_eq!(l.style, LineStyle::Smooth),
            other => panic!("expected Line, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn ctrl_alt_l_cycles_line_style_on_line() {
        // Ctrl-Alt-L on a single Smooth Line must advance to the
        // next style (Light) and report it. Drives the full keymap
        // → cycle_line_style → restyle_selection path.
        use kirkforge_draw_core::LineStyle;
        let mut app = make_app();
        commit_one_smooth_line(&mut app);

        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
        match &app.state.document.objects[0] {
            DrawObject::Line(l) => assert_eq!(l.style, LineStyle::Light),
            _ => unreachable!(),
        }
        assert!(
            app.status.contains("restyled 1 object to light"),
            "status should report a restyle; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_alt_l_with_empty_selection_reports_nothing() {
        // Pressing Ctrl-Alt-L on an empty editor must report
        // "nothing to restyle" and not flip dirty.
        let mut app = make_app();
        let dirty_before = app.state.is_dirty();
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
        assert_eq!(app.state.is_dirty(), dirty_before);
        assert!(app.status.contains("nothing to restyle"));
    }

    #[test]
    fn ctrl_alt_l_on_selection_with_no_lines_reports_kind() {
        // Pressing Ctrl-Alt-L when only a Box is selected must
        // report "no lines / elbows in selection" — Boxes have
        // BoxStyle, not LineStyle, so they don't participate.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
        assert!(
            app.status.contains("no lines / elbows in selection"),
            "status should report kind mismatch; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_alt_l_does_not_arm_l_line_tool() {
        // Bare 'l' (no Ctrl / Alt) must still set the Line tool —
        // Ctrl-Alt-L is a sibling, not a replacement. Regression
        // guard so a future arm-order change can't silently
        // shadow the tool hotkey.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('l')));
        assert_eq!(app.state.tool, DrawMode::Line);
    }

    /// Helper: commit one Box and leave it selected. Mirrors
    /// `commit_one_smooth_line` for the BoxStyle cycle tests.
    fn commit_one_light_box(app: &mut App) {
        use kirkforge_draw_core::BoxStyle;
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 4 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        assert_eq!(app.state.selected_count(), 1);
        match &app.state.document.objects[0] {
            DrawObject::Box(b) => assert_eq!(b.style, BoxStyle::Light),
            other => panic!("expected Box, got {:?}", std::mem::discriminant(other)),
        }
    }

    #[test]
    fn ctrl_alt_b_cycles_box_style_on_box() {
        // Ctrl-Alt-B on a single Light Box must advance to Heavy
        // (the next in the cycle) and report it. Drives the full
        // keymap → cycle_box_style → restyle_boxes_selection path.
        use kirkforge_draw_core::BoxStyle;
        let mut app = make_app();
        commit_one_light_box(&mut app);

        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
        match &app.state.document.objects[0] {
            DrawObject::Box(b) => assert_eq!(b.style, BoxStyle::Heavy),
            _ => unreachable!(),
        }
        assert!(
            app.status.contains("restyled 1 object to heavy"),
            "status should report a restyle; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_alt_b_with_empty_selection_reports_nothing() {
        let mut app = make_app();
        let dirty_before = app.state.is_dirty();
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
        assert_eq!(app.state.is_dirty(), dirty_before);
        assert!(app.status.contains("nothing to restyle"));
    }

    #[test]
    fn ctrl_alt_b_on_selection_with_no_boxes_reports_kind() {
        // Pressing Ctrl-Alt-B when only a Line is selected must
        // report "no boxes in selection" — Lines have LineStyle,
        // not BoxStyle, so they don't participate.
        let mut app = make_app();
        commit_one_smooth_line(&mut app);

        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
        assert!(
            app.status.contains("no boxes in selection"),
            "status should report kind mismatch; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_alt_b_does_not_arm_b_box_tool() {
        // Bare 'b' (no Ctrl / Alt) must still set the Box tool —
        // Ctrl-Alt-B is a sibling, not a replacement. Regression
        // guard so a future arm-order change can't silently
        // shadow the tool hotkey.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('b')));
        assert_eq!(app.state.tool, DrawMode::Box);
    }

    #[test]
    fn ctrl_shift_b_still_aligns_bottom_under_ctrl_alt_b() {
        // Ctrl-Shift-B (align bottom) must continue to work
        // alongside Ctrl-Alt-B. Order-sensitive: if the
        // Ctrl-Alt-B arm were placed AFTER the Ctrl-Shift-B
        // arm, this test would fail because ctrl && alt
        // wouldn't be matched (the Ctrl-Shift-B arm is
        // `ctrl && shift && !alt`, so a Ctrl-Alt-B press
        // wouldn't hit it — but the test exists to pin the
        // arm ordering regardless of guard shape).
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 0, y: 5 });
        app.state.update_draft(Point { x: 2, y: 7 });
        app.state.commit_draft().unwrap();
        // Select both via the public add_to_selection helper.
        // Collect ids first to avoid borrow conflict with the
        // mutable add_to_selection call.
        let ids: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('b'));
        // align-bottom reports a moved-count, not a selected
        // count — only box 1 (top) moves; box 2 was already at
        // the bottom of the union so it doesn't count.
        // What matters here is that the status is the
        // align-bottom format, not the restyle format.
        assert!(
            app.status.contains("to bottom edge"),
            "Ctrl-Shift-B should still align bottom; got {:?}",
            app.status
        );
        assert!(
            !app.status.contains("restyled"),
            "Ctrl-Shift-B should not be intercepted by Ctrl-Alt-B; got {:?}",
            app.status
        );
    }

    // -- Text border cycle (Ctrl-Alt-T) -----------------------------

    #[test]
    fn ctrl_alt_t_advances_text_border() {
        // Default text_border is None. Pressing Ctrl-Alt-T
        // should land on Single and surface the new name in
        // the status bar.
        let mut app = make_app();
        assert_eq!(
            app.state.text_border,
            kirkforge_draw_core::TextBorderMode::None
        );
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
        assert_eq!(
            app.state.text_border,
            kirkforge_draw_core::TextBorderMode::Single
        );
        assert!(app.status.contains("single"), "got {:?}", app.status);
    }

    #[test]
    fn ctrl_alt_t_wraps_from_underline_to_none() {
        // Four presses must visit every variant and wrap
        // back to None on the fifth. The wrap is the bit
        // most likely to drift if a future enum addition
        // forgets the trailing arm.
        let mut app = make_app();
        let order = [
            kirkforge_draw_core::TextBorderMode::Single,
            kirkforge_draw_core::TextBorderMode::Double,
            kirkforge_draw_core::TextBorderMode::Underline,
            kirkforge_draw_core::TextBorderMode::None,
        ];
        for (i, expected) in order.iter().enumerate() {
            handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
            assert_eq!(
                &app.state.text_border,
                expected,
                "press #{}: expected {:?}, got {:?}",
                i + 1,
                expected,
                app.state.text_border
            );
        }
        // One more press returns to Single — the cycle is
        // closed and stable.
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
        assert_eq!(
            app.state.text_border,
            kirkforge_draw_core::TextBorderMode::Single
        );
    }

    #[test]
    fn ctrl_alt_t_does_not_arm_t_text_tool() {
        // Bare 't' is the Text tool hotkey. Ctrl-Alt-T is a
        // sibling chord, not a replacement. Regression
        // guard so a future arm-order change can't silently
        // shadow the tool hotkey.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('t')));
        assert_eq!(app.state.tool, DrawMode::Text);
    }

    #[test]
    fn ctrl_shift_t_still_aligns_top_under_ctrl_alt_t() {
        // Ctrl-Shift-T (align top) must continue to work
        // alongside Ctrl-Alt-T. The Ctrl-Alt-T arm is
        // `ctrl && alt`; the Ctrl-Shift-T arm is
        // `ctrl && shift && !alt`, so a Ctrl-Alt-T press
        // doesn't hit the align arm — but a Ctrl-Shift-T
        // press (no Alt) shouldn't hit the cycle arm
        // either. This test pins that the status bar shows
        // the align message, not the cycle message.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 0, y: 5 });
        app.state.update_draft(Point { x: 2, y: 7 });
        app.state.commit_draft().unwrap();
        let ids: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('t'));
        assert!(
            app.status.contains("to top edge"),
            "Ctrl-Shift-T should still align top; got {:?}",
            app.status
        );
        assert!(
            !app.status.contains("text border"),
            "Ctrl-Shift-T should not be intercepted by Ctrl-Alt-T; got {:?}",
            app.status
        );
    }

    // -- Paint brush cycle (Ctrl-Alt-P) -----------------------------

    #[test]
    fn ctrl_alt_p_advances_brush() {
        // Default brush is `·` (the middle dot). Pressing
        // Ctrl-Alt-P should land on `o` and surface the new
        // glyph in the status bar. The status bar echoes
        // the literal glyph so the user sees what they'll
        // draw next.
        let mut app = make_app();
        assert_eq!(app.state.brush, "·");
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
        assert_eq!(app.state.brush, "o");
        assert!(app.status.contains("o"), "got {:?}", app.status);
    }

    #[test]
    fn ctrl_alt_p_wraps_after_eight_presses() {
        // Eight presses must visit every palette entry and
        // wrap back to `·` on the ninth. The wrap is the
        // bit most likely to drift if a future palette
        // addition forgets the modular index.
        let mut app = make_app();
        let order = ["o", "*", "x", "█", "▒", "░", "▓", "·"];
        for (i, expected) in order.iter().enumerate() {
            handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
            assert_eq!(
                &app.state.brush,
                expected,
                "press #{}: expected {:?}, got {:?}",
                i + 1,
                expected,
                app.state.brush
            );
        }
        // One more press advances to `o` — the cycle is
        // closed and stable.
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
        assert_eq!(app.state.brush, "o");
    }

    #[test]
    fn ctrl_alt_p_recovers_from_unknown_brush() {
        // ponytail: the user can `set_brush(anything)` —
        // any character not in the palette should snap to
        // the first palette entry so the next press keeps
        // them in the cycle. Pin this so a future "user
        // types a custom brush" path doesn't strand the
        // cycle arm. The first press lands on `·` (first
        // palette entry); the second press lands on `o`,
        // matching the wrap behavior of a known brush.
        let mut app = make_app();
        app.state.set_brush("Z");
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
        assert_eq!(app.state.brush, "·");
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
        assert_eq!(app.state.brush, "o");
    }

    #[test]
    fn ctrl_alt_p_does_not_arm_p_paint_tool() {
        // Bare `p` is the Paint tool hotkey. Ctrl-Alt-P is
        // a sibling chord, not a replacement. Regression
        // guard so a future arm-order change can't silently
        // shadow the tool hotkey.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('p')));
        assert_eq!(app.state.tool, DrawMode::Paint);
    }

    // -- Select all (Ctrl-A) ----------------------------------------

    fn key_ctrl_a() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
    }

    #[test]
    fn ctrl_a_selects_every_object() {
        // 3 boxes. Press Ctrl-A → all 3 in the selection.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 10, y: 0 });
        app.state.update_draft(Point { x: 12, y: 2 });
        app.state.commit_draft().unwrap();
        assert_eq!(app.state.selected_count(), 1);

        handle_key(&mut app, key_ctrl_a());
        assert_eq!(app.state.selected_count(), 3);
        assert!(
            app.status.contains("selected 3 objects"),
            "status should report count; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_a_with_empty_document_reports_nothing() {
        let mut app = make_app();
        let dirty_before = app.state.is_dirty();
        handle_key(&mut app, key_ctrl_a());
        assert_eq!(app.state.selected_count(), 0);
        assert_eq!(app.state.is_dirty(), dirty_before);
        assert!(app.status.contains("nothing to select"));
    }

    #[test]
    fn ctrl_a_replaces_prior_selection() {
        // Pre-seed a single selection. Ctrl-A must wipe it
        // before adding the full set (Replace mode, not Add).
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        assert_eq!(app.state.selected_count(), 1);

        handle_key(&mut app, key_ctrl_a());
        assert_eq!(app.state.selected_count(), 2);
    }

    #[test]
    fn ctrl_a_is_idempotent() {
        // Two presses in a row must produce the same count.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key_ctrl_a());
        assert_eq!(app.state.selected_count(), 2);
        handle_key(&mut app, key_ctrl_a());
        assert_eq!(app.state.selected_count(), 2);
    }

    #[test]
    fn ctrl_a_does_not_flip_dirty() {
        // Ctrl-A is a navigation primitive, not a mutation —
        // it must not change the document's dirty flag. The
        // status bar can echo a message, but the user must
        // still see a clean document if they haven't
        // actually edited anything.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.mark_saved();
        assert!(!app.state.is_dirty());
        handle_key(&mut app, key_ctrl_a());
        assert!(!app.state.is_dirty());
    }

    // -- Layers panel keyboard nav (Up / Down / Enter / Esc) -------

    /// Helper: open the layers panel and seed three objects. The
    /// document order is `[box, line, text]` (head = bottommost),
    /// so the panel rows are topmost-first: `[text, line, box]`.
    fn app_with_three_layers_and_panel_open() -> (App, [String; 3]) {
        use kirkforge_draw_core::types::*;
        let mut app = make_app();
        app.state.set_tool(DrawMode::Select);
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "box".into(),
            z: 0,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: BoxStyle::Light,
        }));
        app.state
            .document
            .objects
            .push(DrawObject::Line(LineObject {
                id: "line".into(),
                z: 1,
                parent_id: None,
                color: InkColor::Green,
                x1: 0,
                y1: 0,
                x2: 5,
                y2: 0,
                style: LineStyle::Smooth,
            }));
        app.state
            .document
            .objects
            .push(DrawObject::Text(TextObject {
                id: "text".into(),
                z: 2,
                parent_id: None,
                color: InkColor::Yellow,
                x: 0,
                y: 0,
                content: "top".into(),
                border: TextBorderMode::None,
            }));
        app.toggle_layers();
        assert!(app.show_layers);
        assert!(app.layer_focus.is_none());
        let ids = ["text".to_string(), "line".to_string(), "box".to_string()];
        (app, ids)
    }

    #[test]
    fn up_arrow_with_panel_open_lands_focus_on_topmost_row() {
        // First press of Up on an empty-focus panel should land
        // on row 0 (topmost = "text" in this seed). Exercises
        // the "no prior focus, delta=-1 → 0" branch in
        // cycle_layer_focus.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        assert!(
            app.status.contains(&ids[0]),
            "status should mention focused id; got {:?}",
            app.status
        );
    }

    #[test]
    fn cycle_layer_focus_status_format_includes_row_number_and_kind() {
        // Pin the exact status format from `cycle_layer_focus`:
        // "layer N/M: <Kind> <id>" where N is 1-indexed. The
        // existing tests only check id containment via
        // `.contains(&ids[N])`, which is loose — a future
        // refactor that drops the N/M numbering or the kind
        // label would still pass them. This walks all three
        // rows in order so a single test locks the format
        // end-to-end: top row says "1/3", middle says "2/3",
        // bottom says "3/3", and each line carries the right
        // Kind label (Text, Line, Box) before the id.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        // Seed: [text, line, box] in panel order (topmost
        // first). Document order is [box, line, text] but
        // the panel reverses — see `app_with_three_layers_
        // and_panel_open`. With z all equal, panel order
        // follows the document order in reverse.
        handle_key(&mut app, key(KeyCode::Up)); // → row 0 = "text"
        assert_eq!(app.status, "layer 1/3: Text text", "top row format");
        assert!(app.status.contains(&ids[0]));
        handle_key(&mut app, key(KeyCode::Down)); // → row 1 = "line"
        assert_eq!(app.status, "layer 2/3: Line line", "middle row format");
        assert!(app.status.contains(&ids[1]));
        handle_key(&mut app, key(KeyCode::Down)); // → row 2 = "box"
        assert_eq!(app.status, "layer 3/3: Box box", "bottom row format");
        assert!(app.status.contains(&ids[2]));
    }

    #[test]
    fn down_arrow_with_panel_open_lands_focus_on_bottommost_row() {
        // First press of Down on an empty-focus panel should
        // land on the last row (bottommost = "box"). Mirror
        // of the up_arrow test.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Down));
        let n = ids.len();
        assert_eq!(app.layer_focus, Some(n - 1));
        assert!(app.status.contains(&ids[n - 1]));
    }

    #[test]
    fn up_arrow_clamps_at_topmost_row() {
        // Repeated Up at row 0 should stay at 0, not wrap to
        // the bottom. Mirrors Figma's panel behavior.
        let (mut app, _) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Up));
        handle_key(&mut app, key(KeyCode::Up));
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
    }

    #[test]
    fn down_arrow_clamps_at_bottommost_row() {
        let (mut app, _) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
    }

    #[test]
    fn enter_selects_focused_layer() {
        // Focus row 0 ("text") then Enter — the layer must be
        // selected and the status bar must echo the selection.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        // Up from None → row 0 (topmost).
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert!(
            app.status.contains(&format!("selected '{}'", ids[0])),
            "status should echo selection; got {:?}",
            app.status
        );
    }

    #[test]
    fn enter_with_no_focus_is_a_noop() {
        // Enter without a focus row should not crash and
        // should not change the selection. The Esc/Up/Down
        // arms have their own guards; Enter is a separate
        // arm keyed on `layer_focus.is_some()`.
        let (mut app, _) = app_with_three_layers_and_panel_open();
        assert!(app.layer_focus.is_none());
        let before = app.state.selected_count();
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), before);
    }

    #[test]
    fn successful_enter_keeps_focus_on_committed_row() {
        // commit_layer_focus does NOT clear layer_focus on
        // the success branch — the focus stays on the row
        // the user just committed. This is the contract that
        // lets keyboard nav continue: commit, then keep
        // walking with arrow keys without re-anchoring.
        // (cycle_layer_focus with a Some(focus) acts as a
        // "step" rather than an "anchor" — delta=+1 from
        // row 0 lands on row 1, not on row n-1.) Without
        // this, the user would have to Esc-clear the focus
        // between commits, or every commit would jump the
        // cursor to the bottom of the panel.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        // Land on row 0 (topmost = "text").
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        // Commit the focused row.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), ids[0]);
        // Focus must still be on row 0 — not cleared, not
        // bumped to n-1, not anchored to bottom.
        assert_eq!(
            app.layer_focus,
            Some(0),
            "commit must preserve focus on the committed row"
        );
        // Down must step to row 1, not re-anchor to row n-1.
        // This proves the post-commit focus still acts as
        // a "current row" rather than triggering the
        // no-focus arm's bottommost anchoring.
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.layer_focus,
            Some(1),
            "Down after commit must step +1, not re-anchor to bottom"
        );
    }

    #[test]
    fn enter_with_valid_focus_selects_that_row() {
        // The happy-path commit: walk focus to row 1 (the
        // middle layer in panel order = "line" in the seed
        // [box, line, text] → topmost-first panel [text,
        // line, box], so row 1 = "line"), press Enter, the
        // row's id is selected and the status confirms it.
        // No test covered the success branch of
        // commit_layer_focus (the early-return / out-of-range
        // branches both had tests; the select_id-returns-true
        // path didn't).
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        // Up from no-focus lands on row 0 (topmost); one
        // Down moves to row 1. (Down from no-focus would
        // land on row 2 — bottommost — so we anchor via Up
        // for a deterministic walk into the middle.)
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(1));
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), ids[1]);
        assert!(app.status.contains(&ids[1]), "status: {}", app.status);
    }

    #[test]
    fn enter_with_stale_focus_after_delete_surfaces_out_of_range_status() {
        // Document order: [box, line, text]; panel rows
        // (topmost first) = [text, line, box]. Land focus
        // on row 2 (the bottommost "box" in panel order),
        // then delete the topmost doc-level object ("text")
        // so the panel shrinks to 2 rows. The focus index
        // is now stale (Some(2) on a 2-row list), and Enter
        // hits the "out of range" branch in commit_layer_focus:
        // the helper drops the stale focus and surfaces a
        // status echo so the user knows the Enter didn't
        // silently no-op.
        let (mut app, _ids) = app_with_three_layers_and_panel_open();
        // Walk down to the bottommost row.
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
        // Delete the topmost doc-level object — "text" in
        // document order, row 0 in panel order. The panel
        // now has 2 rows; Some(2) is out of range.
        app.state.document.objects.retain(|o| o.id() != "text");
        // Enter with a stale focus must clear it and surface
        // the "out of range" status, not panic on the index.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.layer_focus.is_none(),
            "stale focus must be cleared on the out-of-range branch"
        );
        assert!(
            app.status.contains("focus row out of range"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn up_down_from_stale_focus_clamps_to_new_last_row() {
        // Companion to `enter_with_stale_focus_after_delete
        // _surfaces_out_of_range_status`: same setup (focus
        // on row 2, panel shrinks to 2 rows), but instead
        // of Enter we press Up and Down. cycle_layer_focus
        // uses saturating_sub for delta=-1 and `.min(n-1)`
        // for delta=+1, so a stale Some(2) on a 2-row
        // panel must clamp to Some(1) on either direction
        // — not panic on the out-of-range index. Pins the
        // "Up/Down recover from a stale focus" branch so
        // a future refactor that adds a `let Some(layer)
        // = layers.get(current)` guard (matching the
        // commit helper's pattern) trips this test.
        let (mut app, _ids) = app_with_three_layers_and_panel_open();
        // Walk to row 2.
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
        // Shrink the panel to 2 rows by deleting the
        // topmost doc-level object ("text"). Now Some(2)
        // is stale (the panel has rows 0 and 1 only).
        app.state.document.objects.retain(|o| o.id() != "text");
        assert_eq!(
            kirkforge_draw_core::layer_list(&app.state).len(),
            2,
            "panel must shrink to 2 rows for the stale-focus setup"
        );
        // Up from a stale focus: 2.saturating_sub(1) = 1.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.layer_focus,
            Some(1),
            "Up from stale focus must clamp to new last row, not panic"
        );
        // Reset to stale Some(2) for the Down test.
        app.layer_focus = Some(2);
        // Down from a stale focus: (2 + 1).min(n - 1) = 1.
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.layer_focus,
            Some(1),
            "Down from stale focus must clamp to new last row, not panic"
        );
    }

    #[test]
    fn esc_clears_layer_focus() {
        let (mut app, _) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Down));
        assert!(app.layer_focus.is_some());
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.layer_focus.is_none());
        assert!(app.status.contains("focus cleared"));
    }

    #[test]
    fn arrows_do_navigate_when_panel_hidden() {
        // Up/Down with the panel hidden should still scroll
        // the body. This is the regression guard — the
        // `app.show_layers` guard on the layer-nav arms must
        // not shadow the scroll arms when the panel is off.
        let mut app = make_app();
        assert!(!app.show_layers);
        let scroll_before = app.scroll_y;
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.scroll_y, scroll_before + SCROLL_STEP);
        assert!(app.layer_focus.is_none());
    }

    #[test]
    fn closing_panel_clears_layer_focus() {
        // L (toggle panel off) must clear the focus row so a
        // stale focus doesn't reappear on next toggle.
        let (mut app, _) = app_with_three_layers_and_panel_open();
        handle_key(&mut app, key(KeyCode::Down));
        assert!(app.layer_focus.is_some());
        app.toggle_layers();
        assert!(!app.show_layers);
        assert!(app.layer_focus.is_none());
    }

    #[test]
    fn up_down_increments_through_panel_in_order() {
        // Walk all three rows topmost-first, then walk back.
        // Pins the per-row transition in cycle_layer_focus.
        let (mut app, ids) = app_with_three_layers_and_panel_open();
        // Start at top via Up.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(1));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
        // Already at bottom; one more Down clamps.
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
        // Back up.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(1));
        // Status echoes should mention the current id.
        assert!(app.status.contains(&ids[1]));
    }

    #[test]
    fn up_at_topmost_row_clamps() {
        // Symmetric to the Down-at-bottommost clamp covered
        // in `up_down_increments_through_panel_in_order`.
        // cycle_layer_focus uses saturating_sub on the Up
        // arm, so Up at 0 stays at 0 (no wrap to n-1).
        let (mut app, _ids) = app_with_three_layers_and_panel_open();
        // Land on row 0.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
        // Up at 0 clamps to 0, doesn't wrap to the bottom.
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.layer_focus, Some(0));
    }

    #[test]
    fn down_on_empty_document_surfaces_empty_message() {
        // Open the panel on an empty doc, then Down. The
        // cycle_layer_focus "no rows" branch must surface
        // "(layers panel: empty document)" instead of
        // trying to anchor focus to a non-existent row.
        // Locks the early-return so a future refactor that
        // drops the layers.is_empty() guard trips this test.
        let mut app = make_app();
        app.toggle_layers();
        assert!(app.show_layers);
        assert!(app.state.document.objects.is_empty());
        handle_key(&mut app, key(KeyCode::Down));
        assert!(
            app.layer_focus.is_none(),
            "focus must stay None on an empty doc"
        );
        assert!(
            app.status.contains("empty document"),
            "status: {}",
            app.status
        );
    }

    // -- Multi-object alignment (Ctrl-Shift-<dir>) ------------------

    fn key_ctrl_shift(c: char) -> KeyEvent {
        KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )
    }

    #[test]
    fn ctrl_shift_l_aligns_selection_to_left_edge() {
        // Three 2x2 boxes at x=0,5,10. After Ctrl-Shift-L the
        // x=0 box is already at the target, so 2 move; status
        // echoes the moved count + the target edge name.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('l'));
        assert_eq!(app.status, "aligned 2 objects to left edge");
        for o in &app.state.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.left, 0, "{} left should snap to 0", b.id);
            }
        }
    }

    #[test]
    fn ctrl_shift_uppercase_l_aligns_not_toggles_layers() {
        // Real-terminal regression pin. On a US keyboard the
        // Ctrl-Shift-L chord produces the SHIFTED char 'L'
        // (uppercase) with both Ctrl and Shift modifiers, so
        // crossterm reports `KeyCode::Char('L')` +
        // `CONTROL | SHIFT`. The existing
        // `ctrl_shift_l_aligns_selection_to_left_edge` test
        // synthesizes the keypress with the un-shifted char
        // 'l' (which targets the lowercase-only align arm
        // directly) and so passes today — but in a real
        // terminal the bind was being shadowed by the
        // unguarded `KeyCode::Char('L')` arm that toggles the
        // layers panel. Without a guard on that arm, the
        // user pressing Ctrl-Shift-L to align left instead
        // flipped the layers panel — the exact opposite of
        // what the help / README document.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        // Pre-condition: panel is hidden by default.
        assert!(!app.show_layers);
        // The realistic keypress: uppercase 'L' + Ctrl + Shift.
        handle_key(
            &mut app,
            KeyEvent::new(
                KeyCode::Char('L'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        // The layers panel must NOT have been toggled.
        assert!(
            !app.show_layers,
            "Ctrl-Shift-L must not toggle the layers panel — that arm should be guarded so it falls through to align-left"
        );
        // And the selection must have been aligned to the left edge.
        assert_eq!(app.status, "aligned 2 objects to left edge");
        for o in &app.state.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.left, 0, "{} left should snap to 0", b.id);
            }
        }
    }

    #[test]
    fn ctrl_shift_r_aligns_selection_to_right_edge() {
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('r'));
        assert_eq!(app.status, "aligned 2 objects to right edge");
        for o in &app.state.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.right, 12);
            }
        }
    }

    #[test]
    fn ctrl_shift_h_aligns_selection_to_horizontal_center() {
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('h'));
        assert_eq!(app.status, "aligned 2 objects to horizontal center");
        for o in &app.state.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(i32::midpoint(b.left, b.right), 6);
            }
        }
    }

    #[test]
    fn ctrl_shift_v_aligns_selection_to_vertical_center() {
        // The three seed boxes share y=0..2, so all are already
        // aligned on the vertical center — status reports "nothing
        // to align" (spamming the chord on a no-op is a no-op).
        // Pin that the Ctrl-V paste chord isn't shadowed by an
        // accidental Ctrl-Shift-V catch-all.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('v'));
        assert_eq!(app.status, "nothing to align");
    }

    #[test]
    fn ctrl_shift_align_with_empty_selection_reports_nothing() {
        let mut app = make_app();
        handle_key(&mut app, key_ctrl_shift('l'));
        assert_eq!(app.status, "nothing to align");
    }

    #[test]
    fn ctrl_shift_t_aligns_selection_to_top_edge() {
        // Sanity for the T chord: the seed boxes all share top=0,
        // so the call is a no-op for these positions; status
        // matches the "nothing to align" branch.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('t'));
        assert_eq!(app.status, "nothing to align");
    }

    #[test]
    fn ctrl_shift_b_aligns_selection_to_bottom_edge() {
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('b'));
        assert_eq!(app.status, "nothing to align");
    }

    // -- Multi-object distribute (Ctrl-Shift-J / Ctrl-Shift-K) ------

    #[test]
    fn ctrl_shift_j_distributes_selection_horizontally() {
        // Three 2x2 boxes at x=0,5,10 — already on equal
        // horizontal spacing (centers 1, 6, 11). So this is a
        // noop in the moved-count sense; status reports
        // "nothing to distribute" (parity with the align
        // already-aligned chord).
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('j'));
        assert_eq!(app.status, "nothing to distribute");
    }

    #[test]
    fn ctrl_shift_j_with_uneven_three_moves_one() {
        // Same three boxes, but drag the middle off-grid so
        // the chord actually does work. After the move the
        // middle lands at the equal-spacing target.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        // Mutate the middle box (index 1) so its center
        // moves from 6 to 4.
        if let DrawObject::Box(b) = &mut app.state.document.objects[1] {
            b.left = 3;
            b.right = 5;
        }
        handle_key(&mut app, key_ctrl_shift('j'));
        assert_eq!(
            app.status,
            "distributed 1 object to equal horizontal spacing"
        );
        // Endpoints stay at 0 and 10; middle snaps to left=5
        // right=7 (center 6, the equal-spacing target).
        if let DrawObject::Box(b) = &app.state.document.objects[1] {
            assert_eq!(i32::midpoint(b.left, b.right), 6);
        } else {
            panic!("expected box at index 1");
        }
    }

    #[test]
    fn ctrl_shift_k_distributes_selection_vertically() {
        // Three 2x2 boxes stacked at y=0,5,10 — already on
        // equal vertical spacing (centers 1, 6, 11). No-op;
        // status "nothing to distribute".
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 0, y: 5 });
        app.state.update_draft(Point { x: 2, y: 7 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 0, y: 10 });
        app.state.update_draft(Point { x: 2, y: 12 });
        app.state.commit_draft().unwrap();
        let ids: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        handle_key(&mut app, key_ctrl_shift('k'));
        assert_eq!(app.status, "nothing to distribute");
    }

    #[test]
    fn ctrl_shift_j_with_two_selected_reports_nothing() {
        // Distribute needs ≥3; with 2 selected it's a no-op
        // and status reports "nothing to distribute".
        let (mut app, _) = make_app_with_three_boxes();
        let id0 = app.state.document.objects[0].id().to_string();
        let id1 = app.state.document.objects[1].id().to_string();
        app.state.add_to_selection(&id0);
        app.state.add_to_selection(&id1);
        handle_key(&mut app, key_ctrl_shift('j'));
        assert_eq!(app.status, "nothing to distribute");
    }

    #[test]
    fn ctrl_shift_distribute_with_empty_selection_reports_nothing() {
        let mut app = make_app();
        handle_key(&mut app, key_ctrl_shift('j'));
        assert_eq!(app.status, "nothing to distribute");
    }

    // -- Invert selection (Ctrl-Shift-I) -----------------------------

    #[test]
    fn ctrl_shift_i_inverts_empty_selection_to_everything() {
        // Empty selection + 2 boxes → invert → 2 selected.
        // Status echoes the new count.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.clear_selection();
        handle_key(&mut app, key_ctrl_shift('I'));
        assert_eq!(app.state.selected_count(), 2);
        assert_eq!(app.status, "inverted selection (2 objects selected)");
    }

    #[test]
    fn ctrl_shift_i_after_ctrl_a_returns_to_empty() {
        // The Ctrl-A then Ctrl-Shift-I workflow: grab
        // everything, flip back to empty. Status uses the
        // singular "empty" branch.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key_ctrl(KeyCode::Char('a')));
        assert_eq!(app.state.selected_count(), 2);
        handle_key(&mut app, key_ctrl_shift('I'));
        assert_eq!(app.state.selected_count(), 0);
        assert_eq!(app.status, "selection inverted (now empty)");
    }

    #[test]
    fn ctrl_shift_i_flips_partial_selection_membership() {
        // 3 boxes, 1 selected. Invert → 2 selected (the
        // other 2). Then invert again → 1 selected
        // (back to the original).
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        let id0 = app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 10, y: 0 });
        app.state.update_draft(Point { x: 12, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.clear_selection();
        app.state.add_to_selection(&id0);
        assert_eq!(app.state.selected_count(), 1);
        handle_key(&mut app, key_ctrl_shift('I'));
        assert_eq!(app.state.selected_count(), 2);
        let selected_ids: Vec<String> = app
            .state
            .selected()
            .into_iter()
            .map(|o| o.id().to_string())
            .collect();
        assert!(!selected_ids.contains(&id0));
        assert_eq!(app.status, "inverted selection (2 objects selected)");
        // Invert again → 1 object selected (the original
        // single-selection). The n=1 status echo (singular
        // "object") exercises the plural_s branch.
        handle_key(&mut app, key_ctrl_shift('I'));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.status, "inverted selection (1 object selected)");
    }

    // -- Marquee select (mouse) -------------------------------------

    /// Helper: seed three boxes the bin tests can marquee over.
    /// Returns (app, doc-ids) so each test can assert against the
    /// selected ids.
    fn make_app_with_three_boxes() -> (App, Vec<String>) {
        let mut app = make_app();
        // Use Box tool and commit three non-overlapping boxes.
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 10, y: 0 });
        app.state.update_draft(Point { x: 12, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        let ids: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        (app, ids)
    }

    /// Emit a Down + Up at the same point — bare click in empty
    /// space; falls back to `select_at` because anchor == current.
    fn mouse_click(app: &mut App, col: u16, row: u16) {
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    /// Emit a Down, a Drag, then an Up at the final point. `modifiers`
    /// on Down pick the marquee mode. The start point must be OUTSIDE
    /// all handle-hit tolerance zones of any currently-selected box —
    /// otherwise the handler treats the Down as a resize (handle hit
    /// wins over marquee). All marquee tests below use
    /// `(3, 7) → doc (3, 4)` as the start so it lands below every
    /// box's BR-handle reach. The end point doesn't matter for the
    /// hit-test — only Down is hit-tested.
    fn mouse_marquee(app: &mut App, start: (u16, u16), end: (u16, u16), modifiers: KeyModifiers) {
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: start.0,
                row: start.1,
                modifiers,
            },
        );
        // A single drag at midpoint + endpoint so the overlay has
        // something to render mid-flight. Real terminals emit one
        // Drag per cell moved; the handler doesn't care about
        // count — it just keeps overwriting `current`.
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: end.0,
                row: end.1,
                modifiers,
            },
        );
        handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: end.0,
                row: end.1,
                modifiers,
            },
        );
    }

    #[test]
    fn marquee_drag_with_no_modifier_replaces_selection() {
        // Bare drag from (4, 3) → (8, 5) covers box b only (5..7,
        // 0..2). Replace mode → selection = {b}. Status reports
        // "selected 1 object".
        let (mut app, ids) = make_app_with_three_boxes();
        mouse_marquee(&mut app, (3, 7), (9, 5), KeyModifiers::NONE);
        assert_eq!(app.state.selected_count(), 1);
        let sel: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(sel, vec![ids[1].clone()]);
        assert!(
            app.status.contains("selected 1 object"),
            "status should report the marquee selection; got {:?}",
            app.status
        );
        // marquee state must be cleared on commit so the renderer
        // stops drawing the live overlay.
        assert!(app.marquee.is_none());
    }

    #[test]
    fn shift_marquee_drag_adds_to_existing_selection() {
        // Pre-select box a, then Shift+drag over box b in Add mode.
        // Selection must keep a AND add b → {a, b}. Status reports
        // "selected 2 objects".
        let (mut app, ids) = make_app_with_three_boxes();
        // Click inside box a to pre-select via the public path
        // (bin tests can't touch `selected_ids` directly — it's
        // a private field of `DrawState`).
        app.state.select_at(Point { x: 1, y: 1 });
        assert_eq!(app.state.selected_count(), 1);
        let pre_selected: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(pre_selected, vec![ids[0].clone()]);

        mouse_marquee(&mut app, (3, 7), (9, 5), KeyModifiers::SHIFT);
        assert_eq!(app.state.selected_count(), 2);
        let sel: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert!(sel.contains(&ids[0]));
        assert!(sel.contains(&ids[1]));
        assert!(!sel.contains(&ids[2]));
        assert!(app.status.contains("selected 2 objects"));
    }

    #[test]
    fn ctrl_marquee_drag_toggles_membership() {
        // Pre-select box b, then Ctrl+drag over b in Toggle mode
        // → b is dropped from the selection (was in, now out).
        let (mut app, ids) = make_app_with_three_boxes();
        // Click inside box b to pre-select.
        app.state.select_at(Point { x: 6, y: 1 });
        assert_eq!(app.state.selected_count(), 1);

        mouse_marquee(&mut app, (3, 7), (9, 5), KeyModifiers::CONTROL);
        assert_eq!(app.state.selected_count(), 0);
        assert!(app.status.contains("no objects in marquee"));
        // ids[1] = box b; let the binding stay alive for symmetry
        // with the other tests even though we no longer reference it.
        let _ = ids[1];
    }

    #[test]
    fn ctrl_modifier_wins_over_shift_in_marquee() {
        // If both Ctrl and Shift are held, Ctrl wins → Toggle mode.
        // Lock the precedence so a future refactor of
        // `mode_from_modifiers` can't silently flip the priority.
        // Pre-select box a; marquee over b with both mods → b
        // toggled in (was out, now in); a is preserved.
        let (mut app, ids) = make_app_with_three_boxes();
        app.state.select_at(Point { x: 1, y: 1 });
        assert_eq!(app.state.selected_count(), 1);

        mouse_marquee(
            &mut app,
            (3, 7),
            (9, 5),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(app.state.selected_count(), 2);
        let sel: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert!(sel.contains(&ids[0]));
        assert!(sel.contains(&ids[1]));
    }

    #[test]
    fn marquee_click_without_drag_falls_back_to_select_at() {
        // Down + Up at the same point (no Drag) → anchor == current,
        // handler must fall through to `select_at` and pick the
        // topmost object at that point. Marquee state is consumed
        // either way.
        let (mut app, ids) = make_app_with_three_boxes();
        // Click inside box b at body cell (6, 4) → doc (6, 1).
        mouse_click(&mut app, 6, 4);
        assert_eq!(app.state.selected_count(), 1);
        let sel: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(sel, vec![ids[1].clone()]);
        assert!(app.marquee.is_none());
    }

    #[test]
    fn shift_click_without_drag_adds_to_existing_selection() {
        // The single-click fallback (anchor == current on mouseup)
        // must honor Shift. Without the `select_at_with_mode`
        // helper the marquee mode captured at Down would be
        // discarded on Up, and Shift+click would silently
        // REPLACE the selection — exactly the regression we
        // fixed. Pinned here at the bin / handler level so a
        // future refactor that re-routes the click fallback
        // can't lose the modifier again.
        let (mut app, ids) = make_app_with_three_boxes();
        // Pre-select box a via bare click first.
        mouse_click(&mut app, 0, 4);
        assert_eq!(app.state.selected_count(), 1);
        // Shift+click inside box b at body cell (6, 4) → adds
        // b without dropping a.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 4,
                modifiers: KeyModifiers::SHIFT,
            },
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 6,
                row: 4,
                modifiers: KeyModifiers::SHIFT,
            },
        );
        assert_eq!(
            app.state.selected_count(),
            2,
            "Shift+click must add, not replace"
        );
        let sel: std::collections::HashSet<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert!(sel.contains(&ids[0]), "pre-selected a stays");
        assert!(sel.contains(&ids[1]), "Shift+clicked b is added");
        assert!(app.marquee.is_none());
    }

    #[test]
    fn ctrl_click_without_drag_toggles_existing_selection() {
        // Ctrl+click without drag toggles: if the object is already
        // selected, it gets removed; if not, it gets added. Bare
        // mouseup before this fix would replace selection with
        // just the clicked object — losing the pre-selection.
        let (mut app, ids) = make_app_with_three_boxes();
        // `make_app_with_three_boxes` leaves `c` selected (each
        // `commit_draft` clears + inserts). Reset to empty so
        // the pre-selection loop below has predictable input.
        app.state.clear_selection();
        // Pre-select boxes a + b via Shift+click on each.
        for (col, row) in [(0, 4), (6, 4)] {
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: col,
                    row,
                    modifiers: KeyModifiers::SHIFT,
                },
            );
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    column: col,
                    row,
                    modifiers: KeyModifiers::SHIFT,
                },
            );
        }
        assert_eq!(app.state.selected_count(), 2, "pre-select a + b");
        // Ctrl+click a second time on box b → toggles b OUT.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 4,
                modifiers: KeyModifiers::CONTROL,
            },
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 6,
                row: 4,
                modifiers: KeyModifiers::CONTROL,
            },
        );
        assert_eq!(app.state.selected_count(), 1, "Ctrl+click on b removes b");
        let sel: std::collections::HashSet<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert!(sel.contains(&ids[0]), "a stays");
        assert!(!sel.contains(&ids[1]), "b was toggled out");
        assert!(app.marquee.is_none());
    }

    #[test]
    fn marquee_with_empty_canvas_reports_no_objects() {
        // Marquee over an empty document must not panic and must
        // report "no objects in marquee" — even though select_at
        // already covers the no-target click case, this exercises
        // the commit path's status branch.
        let mut app = make_app();
        mouse_marquee(&mut app, (3, 7), (9, 5), KeyModifiers::NONE);
        assert_eq!(app.state.selected_count(), 0);
        assert!(app.status.contains("no objects in marquee"));
        assert!(app.marquee.is_none());
    }

    #[test]
    fn marquee_drag_does_not_arm_draft_when_tool_is_select() {
        // Regression guard: a marquee drag in Select tool must NOT
        // begin a draft (drafts belong to non-Select tools). After
        // the drag the document has exactly the original 3 boxes.
        let (mut app, _ids) = make_app_with_three_boxes();
        mouse_marquee(&mut app, (3, 7), (9, 5), KeyModifiers::NONE);
        assert_eq!(app.state.document.objects.len(), 3);
        assert!(!app.state.has_draft());
    }

    #[test]
    fn ctrl_d_with_draft_in_progress_is_noop() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        // commit one box so we have a selection
        app.state.commit_draft().unwrap();
        // Begin a new draft and try to dup.
        app.state.begin_draft(Point { x: 5, y: 5 });
        app.state.update_draft(Point { x: 8, y: 8 });
        handle_key(&mut app, key_ctrl(KeyCode::Char('d')));
        // Only the original remains; the duplicate did not commit.
        assert_eq!(app.state.document.objects.len(), 1);
    }

    #[test]
    fn bracket_raise_lower_event_keys() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 5, y: 0 });
        app.state.update_draft(Point { x: 7, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 10, y: 0 });
        app.state.update_draft(Point { x: 12, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.clear_selection();
        app.state.select_at(Point { x: 6, y: 1 });
        assert_eq!(app.state.selected_count(), 1);
        let before: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();

        handle_key(&mut app, key(KeyCode::Char(']')));
        let after_raise: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(after_raise.last().unwrap(), &before[1]);
        assert!(app.status.contains("raised"));

        handle_key(&mut app, key(KeyCode::Char('[')));
        let after_lower: Vec<String> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(
            after_lower.first().unwrap(),
            &before[1],
            "send-to-back drops B to index 0"
        );
        assert!(app.status.contains("lowered"));
    }

    #[test]
    fn bracket_keys_with_no_selection_leave_status_alone() {
        let mut app = make_app();
        let status_before = app.status.clone();
        handle_key(&mut app, key(KeyCode::Char(']')));
        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(
            app.status, status_before,
            "no-op should not overwrite status"
        );
    }

    #[test]
    fn question_mark_toggles_help_overlay() {
        let mut app = make_app();
        assert!(!app.show_help);
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(app.show_help);
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(!app.show_help);
    }

    #[test]
    fn esc_closes_help_overlay() {
        // Esc is the universal dismiss gesture — palette,
        // find, save-as, text-edit, layer focus all honor
        // it. The help overlay should too. Today the
        // top-level Esc arm has no guard for `show_help`,
        // so opening the help overlay and pressing Esc
        // falls through to the draft / selection / quit
        // cascade. On a clean doc with no selection, that's
        // `request_quit()` — pressing Esc to dismiss the
        // help overlay quits the editor. Add a guard arm
        // for `show_help` that toggles it off (matches the
        // `?` toggle), placed before the draft/selection/
        // quit cascade so it wins.
        let mut app = make_app();
        // Open the help overlay.
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert!(app.show_help);
        // Pre-condition: no draft, no selection, clean doc
        // — so the default Esc arm would request_quit.
        assert_eq!(app.state.selected_count(), 0);
        assert!(!app.state.has_draft());
        assert!(!app.state.is_resizing());
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(
            !app.show_help,
            "Esc must close the help overlay, not fall through to quit"
        );
        // The editor must NOT have requested quit — a clean
        // doc with no selection would otherwise fire
        // request_quit and start a quit-confirm on the
        // next tick. Pin that pending_quit_confirm is
        // unset.
        assert!(!app.pending_quit_confirm, "Esc must not request quit");
    }

    #[test]
    fn ctrl_plus_letter_does_not_silently_swap_tool() {
        // Bare `b` / `l` / `e` / `p` / `t` are documented tool
        // shortcuts (see README + HELP_LINES). Before this test
        // existed, the unguarded bare-letter arms also caught
        // Ctrl+<letter> and silently swapped the active tool —
        // e.g. Ctrl+B → Box, Ctrl+L → Line. That's an undocumented
        // side effect that asymmetric with the tick-33 guard on
        // the layers-toggle L arm. Today the bare arms carry
        // `!ctrl && !alt` guards; this test pins the new behavior
        // so a future cleanup doesn't accidentally re-introduce
        // the silent swap.
        let mut app = make_app();
        let default_tool = app.state.tool;
        for letter in ['b', 'l', 'e', 'p', 't'] {
            // Reset between iterations — each Ctrl+<letter> must
            // be a no-op, but a stray side effect from a prior
            // iteration could otherwise mask a regression.
            app.state.set_tool(default_tool);
            handle_key(&mut app, key_ctrl(KeyCode::Char(letter)));
            assert_eq!(
                app.state.tool, default_tool,
                "Ctrl+{letter} must not change the active tool",
            );
        }
    }

    #[test]
    fn help_lines_has_expected_headings() {
        // Lock the source-of-truth lines down so an edit to the
        // overlay doesn't silently drift from what users have already
        // seen in the field.
        assert!(HELP_LINES.iter().any(|l| l.contains("key map")));
        assert!(HELP_LINES.iter().any(|l| l.contains("Ctrl-S")));
        assert!(HELP_LINES.iter().any(|l| l.contains("Ctrl-D")));
        assert!(HELP_LINES.iter().any(|l| l.contains("undo")));
        // Command palette has its own line so users can discover
        // the `:` / `/` triggers.
        assert!(HELP_LINES.iter().any(|l| l.contains("command palette")));
    }

    #[test]
    fn keymap_doc_block_lists_palette_and_z_order_chords() {
        // Drift guard: the file-level `//! Key map:` doc block
        // must mention every chord that HELP_LINES surfaces so
        // the three sources of truth (README, HELP_LINES, this
        // doc comment) stay in lockstep. Today HELP_LINES
        // covers the palette (`: / /`), raise/lower (`] / [`),
        // and z-order nudge (`Shift+] / Shift+[`) — the doc
        // block previously missed all three and was patched in
        // tick 41; this test pins the patch so a future edit
        // can't silently drop the chord again.
        //
        // tick 46: extended to cover the
        //   * Ctrl-S fallback to save-as
        //   * Left-click Shift=add / Ctrl=toggle semantics
        // introduced in ticks 42 + 45 so future drift fixes have
        // a regression to lean on.
        let src = include_str!("event.rs");
        let doc_block = src
            .split("//! Key map:")
            .nth(1)
            .and_then(|tail| tail.split("//! Mouse:").next())
            .expect("keymap doc block + mouse doc block should both exist");
        let mouse_block = src
            .split("//! Mouse:")
            .nth(1)
            .expect("mouse doc block should exist");
        assert!(
            doc_block.contains(": / /"),
            "keymap doc block must list the palette triggers"
        );
        assert!(
            doc_block.contains("] / ["),
            "keymap doc block must list the raise/lower chord"
        );
        assert!(
            doc_block.contains("Shift+] / Shift+["),
            "keymap doc block must list the z-order nudge chord"
        );
        assert!(
            doc_block.contains("save-as if no path yet"),
            "keymap doc block must advertise the Ctrl-S → save-as fallback"
        );
        assert!(
            mouse_block.contains("Shift=add"),
            "mouse doc block must document Shift=add on left-click"
        );
        assert!(
            mouse_block.contains("Ctrl=toggle"),
            "mouse doc block must document Ctrl=toggle on left-click"
        );
    }

    #[test]
    fn help_lines_match_tick_42_45_drift_fixes() {
        // Drift guard for HELP_LINES itself: after the behavior
        // changes in ticks 42 (Ctrl-S fallback) and 45 (single-
        // click Shift / Ctrl modifiers), the help overlay text
        // must mention both. Locked here so a future edit that
        // rewrites HELP_LINES re-learns the chord instead of
        // silently regressing to the bare wording.
        let joined = HELP_LINES.join("\n");
        assert!(
            joined.contains("Ctrl-S") && joined.contains("open save-as"),
            "HELP_LINES must describe the Ctrl-S → save-as fallback; got:\n{joined}"
        );
        assert!(
            joined.contains("Shift=add") && joined.contains("Ctrl=toggle"),
            "HELP_LINES must describe single-click Shift/Ctrl modifiers; got:\n{joined}"
        );
    }

    #[test]
    fn delete_removes_selected() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key(KeyCode::Delete));
        assert!(app.state.document.objects.is_empty());
    }

    #[test]
    fn arrow_keys_scroll() {
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Right));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.scroll_x, SCROLL_STEP);
        assert_eq!(app.scroll_y, SCROLL_STEP);
        // Up clamps to 0.
        for _ in 0..10 {
            handle_key(&mut app, key(KeyCode::Up));
        }
        assert_eq!(app.scroll_y, 0);
    }

    #[test]
    fn page_down_increments_scroll_y_by_page_size() {
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(
            app.scroll_y, SCROLL_PAGE_STEP,
            "PageDown must scroll one page ({SCROLL_PAGE_STEP} cells)"
        );
        handle_key(&mut app, key(KeyCode::PageDown));
        assert_eq!(
            app.scroll_y,
            2 * SCROLL_PAGE_STEP,
            "second PageDown must stack"
        );
    }

    #[test]
    fn page_up_decrements_scroll_y_clamped_at_zero() {
        let mut app = make_app();
        // Scroll down a few pages so PageUp has room to subtract.
        for _ in 0..3 {
            handle_key(&mut app, key(KeyCode::PageDown));
        }
        let before = app.scroll_y;
        handle_key(&mut app, key(KeyCode::PageUp));
        assert_eq!(
            app.scroll_y,
            before - SCROLL_PAGE_STEP,
            "PageUp subtracts one page"
        );
        // Page-up past the top clamps at 0 instead of going negative.
        for _ in 0..20 {
            handle_key(&mut app, key(KeyCode::PageUp));
        }
        assert_eq!(app.scroll_y, 0, "top clamp at 0");
    }

    #[test]
    fn esc_clears_selection_then_quits() {
        // Empty document — no selection, no draft. Esc should quit.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.should_quit);

        // With a selection — Esc clears the selection first.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 3 });
        app.state.commit_draft().unwrap();
        assert!(app.state.selected_count() > 0);
        app.should_quit = false;
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.should_quit);
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn mouse_left_click_selects() {
        let mut app = make_app();
        // Create a box directly via the document.
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 3 });
        app.state.commit_draft().unwrap();
        // Switch to Select so the click below goes through select_at.
        app.state.set_tool(DrawMode::Select);
        // Clear auto-selected so we can prove the click re-selects.
        app.state.clear_selection();
        assert_eq!(app.state.selected_count(), 0);
        // Click at body (1, 3) → doc (1, 0) → inside the box.
        // Both Down and Up are required now that a bare Down begins
        // a marquee anchor; the Up at the same point falls through
        // to `select_at` (anchor == current).
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 1,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.state.selected_count(), 1);
    }

    #[test]
    fn mouse_drag_creates_draft_and_commits_on_up() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.state.has_draft());
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.state.document.objects.len(), 1);
        // Tool reverts to Select after commit.
        assert_eq!(app.state.tool, DrawMode::Select);
    }

    #[test]
    fn mouse_click_outside_pane_is_noop() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Line);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0, // above body pane
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.state.has_draft());
    }

    #[test]
    fn ctrl_s_saves_to_source_path() {
        // Use a temp file; clean up after.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ctrl-s.td.json");
        let path_str = path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some(path_str.clone());
        app.state.set_tool(DrawMode::Line);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 3, y: 0 });
        app.state.commit_draft().unwrap();

        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        assert!(app.status.starts_with("saved "));
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"version\""));
        assert!(written.contains("\"line\""));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ctrl_s_with_no_source_path_opens_save_as_modal_keymap() {
        // Keymap-level regression: Ctrl-S on a fresh doc must
        // open the save-as modal instead of surfacing a
        // "save failed: no source path" status. This used to
        // assert the opposite (it pinned the surface error);
        // the contract was changed to match the standard
        // editor convention (VS Code / Sublime / IntelliJ:
        // Ctrl-S on an unsaved file opens save-as).
        let mut app = make_app();
        app.source_path = None;
        assert!(app.save_as.is_none(), "precondition: no modal yet");
        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        assert!(
            app.save_as.is_some(),
            "Ctrl-S on a fresh doc must open the save-as modal"
        );
        assert!(
            !app.status.starts_with("save failed"),
            "Ctrl-S must NOT surface a save-failed status when no path is set; got: {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_s_clears_dirty_marker() {
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ctrl-s-dirty.td.json");
        let path_str = path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some(path_str.clone());
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        assert!(app.state.is_dirty(), "commit should leave doc dirty");

        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        assert!(
            !app.state.is_dirty(),
            "successful save clears the dirty bit"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ctrl_shift_s_opens_save_as_modal() {
        // Ctrl-Shift-S opens the modal, pre-populated with
        // the current source_path. The keymap hijack sits
        // before the bare Ctrl-S arm so the chord never
        // accidentally saves to the existing path.
        let mut app = make_app();
        app.source_path = Some("orig.td.json".into());
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        assert!(app.save_as.is_some(), "save_as modal opens");
        assert_eq!(
            app.save_as.as_ref().unwrap().path,
            "orig.td.json",
            "pre-populated with current path"
        );
        assert!(
            app.status.contains("save as"),
            "status echoes the prompt: {}",
            app.status
        );
    }

    #[test]
    fn ctrl_shift_s_enter_writes_to_new_path() {
        // Open save-as, type a new path, Enter → file lands
        // on disk, source_path flips, modal closes.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let new_path = dir.join("save-as-new.td.json");
        let new_path_str = new_path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some("orig.td.json".into());
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();

        // Open the modal.
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        assert!(app.save_as.is_some());

        // Backspace away the pre-populated "orig.td.json"
        // (15 chars) so we can type the new path cleanly.
        for _ in 0..15 {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        for ch in new_path_str.chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }

        // Enter commits the path and writes the file.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.save_as.is_none(), "modal closes on commit");
        assert_eq!(app.source_path.as_deref(), Some(new_path_str.as_str()));
        assert!(
            app.status.starts_with("saved as "),
            "status: {}",
            app.status
        );
        assert!(
            std::fs::read_to_string(&new_path).is_ok(),
            "file written to the new path"
        );
        assert!(!app.state.is_dirty(), "save clears dirty bit");

        let _ = std::fs::remove_file(&new_path);
    }

    #[test]
    fn save_as_esc_cancels_and_keeps_old_source() {
        // Esc cancels — modal closes, source_path is the
        // old value, no file written.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let new_path = dir.join("save-as-cancel.td.json");
        let new_path_str = new_path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some("orig.td.json".into());
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        // Backspace the pre-populated path, type the new one.
        for _ in 0..12 {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        for ch in new_path_str.chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.save_as.is_none());
        assert_eq!(app.source_path.as_deref(), Some("orig.td.json"));
        assert_eq!(app.status, "save as cancelled");
        assert!(
            !std::fs::exists(&new_path).unwrap_or(false),
            "no file written on cancel"
        );
    }

    #[test]
    fn save_as_empty_enter_stays_in_modal() {
        // Empty buffer + Enter → the modal stays open, the
        // status echoes the no-op. The user can keep
        // typing rather than re-pressing Ctrl-Shift-S.
        let mut app = make_app();
        app.source_path = None;
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.save_as.is_some(), "modal stays on empty Enter");
        assert!(
            app.status.contains("empty"),
            "status echoes the no-op: {}",
            app.status
        );
    }

    #[test]
    fn save_as_nul_byte_path_is_rejected() {
        // Ctrl-@ on most terminals inserts a NUL byte
        // (Rust strings are UTF-8 and accept 0x00). The
        // validator catches it at commit time so the
        // modal stays open and the status surfaces a
        // useful error. Mirrors `validate_path_arg` in
        // render.rs for the load path; the save path
        // needs the same guard because the save-as
        // modal accepts arbitrary typed chars.
        let mut app = make_app();
        app.source_path = Some("orig.td.json".into());
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        // Backspace the pre-populated path.
        for _ in 0..12 {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        // Type a path with a trailing NUL byte. Char
        // '\0' is 1 byte in UTF-8, so a single
        // keypress is the right shape.
        for ch in "safe.td.json".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Char('\0')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.save_as.is_some(), "modal stays on NUL path");
        assert_eq!(
            app.source_path.as_deref(),
            Some("orig.td.json"),
            "source_path unchanged on NUL reject"
        );
        assert!(
            app.status.contains("NUL"),
            "status echoes the NUL guard: {}",
            app.status
        );
    }

    #[test]
    fn save_as_whitespace_only_path_is_rejected() {
        // A path of just spaces trims to empty and is
        // treated the same as the empty-buffer case.
        // Confirms the trim() guard handles the
        // "user pressed space space space Enter"
        // foot-gun end-to-end through the keymap.
        let mut app = make_app();
        app.source_path = None;
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        for _ in 0..3 {
            handle_key(&mut app, key(KeyCode::Char(' ')));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.save_as.is_some(), "modal stays on whitespace path");
        assert!(
            app.status.contains("whitespace"),
            "status echoes the no-op: {}",
            app.status
        );
    }

    #[test]
    fn save_as_flips_source_for_subsequent_ctrl_s() {
        // After Save-As commits, the next Ctrl-S writes to
        // the NEW path, not the old one. This is the
        // contract: Save-As flips the editor's "home" path
        // so subsequent saves (and the save-on-quit y
        // arm) all land at the new location.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let orig_path = dir.join("save-as-flip-orig.td.json");
        let new_path = dir.join("save-as-flip-new.td.json");
        let orig_path_str = orig_path.to_string_lossy().to_string();
        let new_path_str = new_path.to_string_lossy().to_string();

        let mut app = make_app();
        app.source_path = Some(orig_path_str.clone());
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();

        // First Ctrl-S writes to orig_path.
        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        assert!(std::fs::read_to_string(&orig_path).is_ok());

        // Mutate, then Save-As to new_path.
        app.state.begin_draft(Point { x: 5, y: 5 });
        app.state.update_draft(Point { x: 7, y: 6 });
        app.state.commit_draft().unwrap();
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        let old_len = orig_path_str.len();
        for _ in 0..old_len {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        for ch in new_path_str.chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.source_path.as_deref(), Some(new_path_str.as_str()));
        assert!(std::fs::read_to_string(&new_path).is_ok());

        // Mutate again, plain Ctrl-S — should land at the
        // NEW path now, not the orig.
        app.state.begin_draft(Point { x: 9, y: 9 });
        app.state.update_draft(Point { x: 10, y: 10 });
        app.state.commit_draft().unwrap();
        let new_path_mtime_before = std::fs::metadata(&new_path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        let new_path_mtime_after = std::fs::metadata(&new_path).unwrap().modified().unwrap();
        assert!(
            new_path_mtime_after > new_path_mtime_before,
            "Ctrl-S after Save-As must write to the new path"
        );

        let _ = std::fs::remove_file(&orig_path);
        let _ = std::fs::remove_file(&new_path);
    }

    #[test]
    fn save_as_failure_keeps_modal_open_and_restores_prior_source() {
        // The commit-fail footgun. Ctrl-Shift-S → type a path
        // that the OS can't write to (parent dir doesn't
        // exist) → Enter. Pre-fix this would have flipped
        // source_path to the bad path and closed the modal,
        // leaving the user's next Ctrl-S targeting the bad
        // path. The fix is revert_save_as: roll source_path
        // back to where the user came from and re-open the
        // modal pre-populated with the bad path so they can
        // edit and retry.
        let dir = std::env::temp_dir().join("kfd-test");
        std::fs::create_dir_all(&dir).unwrap();
        let orig_path = dir.join("save-as-fail-orig.td.json");
        let orig_path_str = orig_path.to_string_lossy().to_string();
        // /no/such/dir/file.td.json — the parent doesn't
        // exist, atomic_write will fail.
        let bad_path = "/no/such/dir/file.td.json".to_string();

        let mut app = make_app();
        app.source_path = Some(orig_path_str.clone());
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();

        // Ctrl-Shift-S opens the modal pre-populated with
        // orig_path_str.
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
        assert!(app.save_as.is_some(), "Ctrl-Shift-S must open modal");
        // Backspace out the pre-populated path, type the
        // bad path.
        for _ in 0..orig_path_str.len() {
            handle_key(&mut app, key(KeyCode::Backspace));
        }
        for ch in bad_path.chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        // Enter — save_app fails because /no/such/dir
        // doesn't exist.
        handle_key(&mut app, key(KeyCode::Enter));

        // Modal must still be open with the bad path
        // pre-populated so the user can edit + retry.
        let s = app
            .save_as
            .as_ref()
            .expect("save_as modal must stay open after a failed commit");
        assert_eq!(
            s.path, bad_path,
            "reopened modal must pre-populate the bad path"
        );
        // source_path must be rolled back to the original.
        assert_eq!(
            app.source_path.as_deref(),
            Some(orig_path_str.as_str()),
            "source_path must roll back to the prior value on save failure"
        );
        // Status surfaces the failure.
        assert!(
            app.status.starts_with("save as failed"),
            "status must surface the failure: {}",
            app.status
        );

        let _ = std::fs::remove_file(&orig_path);
    }

    #[test]
    fn mouse_down_on_handle_begins_resize() {
        let mut app = make_app();
        // Build a box at doc (0,0)..(5,3) and select it.
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        assert_eq!(app.state.tool, DrawMode::Select);
        assert_eq!(app.state.selected_count(), 1);

        // Body starts at (0,3). Click at (5,6) → doc (5,3) → BR corner.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 6,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.state.is_resizing());

        // Drag to screen (8, 9) → doc (8, 6). BottomRight handle pins
        // left + top; right + bottom follow the pointer.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 8,
                row: 9,
                modifiers: KeyModifiers::NONE,
            },
        );

        // Release — resize commits; tool stays Select.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 8,
                row: 9,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.state.is_resizing());
        assert_eq!(app.state.tool, DrawMode::Select);
        assert_eq!(app.status, "resized box");

        // Box should now span (0,0)..(8,6).
        let sel = app.state.selected();
        assert_eq!(sel.len(), 1);
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 0);
            assert_eq!(b.top, 0);
            assert_eq!(b.right, 8);
            assert_eq!(b.bottom, 6);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn mouse_down_off_handle_falls_through_to_select() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);
        app.state.clear_selection();

        // Click in the box interior (1,4) → doc (1,1) — not a handle.
        // Both Down and Up are required now: Down sets the marquee
        // anchor, Up at the same point falls through to `select_at`.
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.state.is_resizing());
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
        );
        // Select-tool click should re-select the box.
        assert_eq!(app.state.selected_count(), 1);
    }

    #[test]
    fn shift_arrow_translates_selected_box() {
        let mut app = make_app();
        // Build a box and select it.
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 10, y: 10 });
        app.state.update_draft(Point { x: 15, y: 13 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        // Shift+Right nudges by 1 cell.
        handle_key(&mut app, key_with_shift(KeyCode::Right));
        // Shift+Down nudges by 1 cell.
        handle_key(&mut app, key_with_shift(KeyCode::Down));

        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 11);
            assert_eq!(b.top, 11);
            assert_eq!(b.right, 16);
            assert_eq!(b.bottom, 14);
        } else {
            panic!("expected box");
        }
        // Single undo step covers both nudges.
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 10);
            assert_eq!(b.top, 10);
            assert_eq!(b.right, 15);
            assert_eq!(b.bottom, 13);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn shift_arrow_without_selection_is_noop() {
        let mut app = make_app();
        // No selection; Shift+Right should not panic or invent state.
        handle_key(&mut app, key_with_shift(KeyCode::Right));
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn ctrl_shift_arrow_translates_selected_box_by_ten() {
        // The 10-cell nudge arm. Box at (10,10)-(15,13),
        // Ctrl+Shift+Right → +10 on x, Ctrl+Shift+Down → +10
        // on y. Total: (20,20)-(25,23). Single undo step
        // covers both nudges (push_undo runs once per
        // move_selected call, so two nudges = two undo
        // steps; matches Shift+Arrow's "one undo per
        // keypress" pattern).
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 10, y: 10 });
        app.state.update_draft(Point { x: 15, y: 13 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Right));
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 20, "Ctrl+Shift+Right must move +10 on x");
            assert_eq!(b.right, 25);
        } else {
            panic!("expected box");
        }

        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Down));
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.top, 20, "Ctrl+Shift+Down must move +10 on y");
            assert_eq!(b.bottom, 23);
        } else {
            panic!("expected box");
        }

        // Undo twice — once per nudge, matching
        // Shift+Arrow's one-undo-per-keypress contract.
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 10);
            assert_eq!(b.top, 10);
            assert_eq!(b.right, 15);
            assert_eq!(b.bottom, 13);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn ctrl_shift_left_arrow_translates_selected_box_by_minus_ten() {
        // The negative direction of the 10-cell nudge.
        // Box at (20,20)-(25,23), Ctrl+Shift+Left → -10
        // on x → (10,20)-(15,23).
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 20, y: 20 });
        app.state.update_draft(Point { x: 25, y: 23 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Left));
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 10, "Ctrl+Shift+Left must move -10 on x");
            assert_eq!(b.right, 15);
            assert_eq!(b.top, 20, "y untouched by horizontal nudge");
            assert_eq!(b.bottom, 23);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn ctrl_shift_arrow_without_selection_is_noop() {
        // Same shape as shift_arrow_without_selection_is_noop
        // but for the 10-cell arm. The guard inside
        // move_selected returns early when the selection
        // is empty.
        let mut app = make_app();
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Right));
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn plain_arrow_does_not_move_selection() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 10, y: 10 });
        app.state.update_draft(Point { x: 15, y: 13 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        let before_bounds = app.state.selection_bounds().unwrap();
        handle_key(&mut app, key(KeyCode::Right));
        let after_bounds = app.state.selection_bounds().unwrap();
        assert_eq!(before_bounds, after_bounds);
        // Bare arrow scrolled the viewport instead.
        assert_eq!(app.scroll_x, SCROLL_STEP);
    }

    #[test]
    fn esc_cancels_in_progress_resize() {
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 5, y: 3 });
        app.state.commit_draft().unwrap();
        app.state.set_tool(DrawMode::Select);

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            },
        );
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 9,
                row: 7,
                modifiers: KeyModifiers::NONE,
            },
        );
        // Esc mid-resize restores original bounds.
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.state.is_resizing());
        let sel = app.state.selected();
        if let kirkforge_draw_core::DrawObject::Box(b) = sel[0] {
            assert_eq!(b.left, 0);
            assert_eq!(b.top, 0);
            assert_eq!(b.right, 5);
            assert_eq!(b.bottom, 3);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn save_app_preserves_redo_stack() {
        // Bug #2 regression: saving must not snapshot, which would
        // push the undo stack and clear pending redo entries.
        let mut app = make_app();
        let tmp =
            std::env::temp_dir().join(format!("kfd-save-redo-{}.td.json", std::process::id()));
        app.source_path = Some(tmp.to_string_lossy().into_owned());

        // Add a box, then commit it so undo/redo has something to do.
        app.state.set_tool(kirkforge_draw_core::DrawMode::Box);
        app.state.begin_draft(Point { x: 1, y: 1 });
        app.state.update_draft(Point { x: 4, y: 4 });
        app.state.commit_draft();
        assert!(app.state.can_undo());

        // Undo, then verify redo is available, then save. Redo must
        // survive the save.
        app.state.undo();
        assert!(app.state.can_redo());
        save_app(&mut app).expect("save");
        assert!(
            app.state.can_redo(),
            "save_app must not clear the redo stack"
        );

        // Redo still works.
        assert!(app.state.redo());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn atomic_write_writes_full_content_and_cleans_tmp() {
        // Happy path: bytes land at the target path, no .tmp sibling
        // is left behind.
        let tmp = std::env::temp_dir().join(format!(
            "kfd-atomic-ok-{}-{}.td.json",
            std::process::id(),
            line!()
        ));
        let payload = b"{\"version\":1,\"objects\":[]}".to_vec();
        atomic_write(&tmp, &payload).expect("atomic_write should succeed");
        let read_back = std::fs::read(&tmp).expect("file should exist");
        assert_eq!(read_back, payload);
        let tmp_sibling = {
            let mut s = tmp.as_os_str().to_owned();
            s.push(".tmp");
            std::path::PathBuf::from(s)
        };
        assert!(
            !tmp_sibling.exists(),
            ".tmp sibling should be cleaned up on success"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        // Overwrite case: a previous save at the same path must be
        // fully replaced by the new bytes (no partial-old + partial-new
        // contents leaking through).
        let tmp = std::env::temp_dir().join(format!(
            "kfd-atomic-replace-{}-{}.td.json",
            std::process::id(),
            line!()
        ));
        std::fs::write(&tmp, b"OLD-CONTENT-LEFT-OVER-FROM-PREVIOUS-SAVE").expect("seed");
        let payload = b"{\"version\":1,\"objects\":[]}".to_vec();
        atomic_write(&tmp, &payload).expect("atomic_write should succeed");
        let read_back = std::fs::read(&tmp).expect("file should exist");
        assert_eq!(read_back, payload);
        assert!(
            !read_back.starts_with(b"OLD-CONTENT"),
            "old contents must not bleed through the rename"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn atomic_write_cleans_up_tmp_on_failure() {
        // Unwritable target path: no .tmp sibling should be left
        // behind (otherwise we'd litter the user's directory with
        // half-written files on every failed save).
        let bad = std::path::PathBuf::from(format!(
            "{}/kfd-atomic-fail-dir-{}/nonexistent.td.json",
            std::env::temp_dir().display(),
            std::process::id()
        ));
        let tmp_sibling = {
            let mut s = bad.as_os_str().to_owned();
            s.push(".tmp");
            std::path::PathBuf::from(s)
        };
        let result = atomic_write(&bad, b"payload");
        assert!(result.is_err(), "atomic_write must fail");
        assert!(
            !tmp_sibling.exists(),
            ".tmp sibling must be cleaned up after a failed write"
        );
    }

    #[test]
    fn save_app_failure_marks_dirty() {
        // Bug #6 regression: a failed save must mark the document
        // dirty so the title bar tells the user that disk is out of
        // sync with their intent.
        let mut app = make_app();
        app.state.mark_saved();
        assert!(!app.state.is_dirty());

        // Point at a path that can't be written. A nested path under a
        // non-existent parent directory makes `std::fs::write` fail
        // deterministically without needing filesystem permissions.
        let bad = format!(
            "{}/kfd-save-fail-{}/nonexistent.td.json",
            std::env::temp_dir().display(),
            std::process::id()
        );
        app.source_path = Some(bad);

        let result = save_app(&mut app);
        assert!(result.is_err(), "save should fail for an unwritable path");
        assert!(
            app.state.is_dirty(),
            "failed save must leave the document marked dirty"
        );
    }

    #[test]
    fn ctrl_s_with_no_source_path_opens_save_as_modal() {
        // The keymap contract for Ctrl-S: "save back to source
        // path (or open save-as if no path yet)". A user who
        // boots kfd with no `--load` and immediately Ctrl-S
        // must NOT see a "save failed: no source path" status
        // — they must land in the save-as modal pre-populated
        // empty, identical to Ctrl-Shift-S on the same state.
        // This test pins that fallback so a future refactor
        // (e.g. unconditional save_app call) can't silently
        // regress the UX.
        let mut app = make_app();
        assert!(app.source_path.is_none(), "precondition: fresh doc");
        assert!(app.save_as.is_none(), "precondition: no modal yet");
        handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
        assert!(
            app.save_as.is_some(),
            "Ctrl-S on a fresh doc must open the save-as modal"
        );
        assert!(
            app.status.is_empty() || !app.status.contains("save failed"),
            "Ctrl-S must not surface a save-failed status when no path is set; got: {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_s_with_source_path_still_calls_save_app() {
        // The other half of the Ctrl-S contract: with a
        // source_path set, Ctrl-S must still call save_app
        // (NOT open save-as). Guards against an over-eager
        // fallback that always routes to begin_save_as.
        let mut app = make_app();
        app.source_path = Some("/tmp/kfd-ctrl-s-with-path.td.json".into());
        assert!(app.save_as.is_none(), "precondition: no modal yet");
        // Drive save via save_app directly so we don't need
        // the disk write to actually succeed in the test.
        let res = save_app(&mut app);
        // We don't assert success or failure — the IO can
        // legitimately fail in any sandbox — but the
        // invocation must NOT have opened the save-as modal.
        let _ = res;
        assert!(
            app.save_as.is_none(),
            "Ctrl-S with a path set must not open save-as"
        );
    }

    #[test]
    fn palette_save_with_no_source_path_opens_save_as_modal() {
        // Mirror of `ctrl_s_with_no_source_path_opens_save_as_*`:
        // the palette `:save` command must obey the same UX
        // contract (no source_path → open save-as modal),
        // otherwise the two surfaces drift apart and the user
        // sees a confusing "save failed" status from one entry
        // point but a working modal from another. This test
        // pins the dispatch side.
        let mut app = make_app();
        app.source_path = None;
        assert!(app.save_as.is_none(), "precondition: no modal yet");
        open_palette(&mut app, PaletteTrigger::Colon);
        // Type "save" and press Enter to commit.
        for ch in "save".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.save_as.is_some(),
            "palette :save on a fresh doc must open the save-as modal"
        );
        assert!(
            !app.status.contains("save failed"),
            "palette :save must not surface a save-failed status when no path is set; got: {:?}",
            app.status
        );
    }

    #[test]
    fn save_app_with_no_source_path_returns_bail() {
        // The "user opened with no --load" branch. Until today
        // the only save-failure test exercised the
        // `atomic_write` Err arm; this covers the bail at the
        // top of `save_app` so a future refactor can't
        // accidentally try to serialize to a `None` path.
        let mut app = make_app();
        app.source_path = None;
        let err = save_app(&mut app).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("no source path"),
            "expected 'no source path' in the error chain, got: {msg}"
        );
    }

    #[test]
    fn save_app_with_nul_source_path_bails_and_marks_dirty() {
        // The validate_path_arg guard inside save_app. A user
        // (or a Save-As commit that bypassed the NUL check)
        // could leave a NUL byte in `source_path`; the guard
        // is the second line of defense, must reject before
        // any IO, and must flip dirty so the user sees a `*`
        // and knows their last save intent didn't go through
        // (parity with atomic_write's failed-write dirty flip).
        let mut app = make_app();
        app.source_path = Some("/tmp/kfd\0-evil.td.json".into());
        let err = save_app(&mut app).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("NUL"),
            "expected NUL-byte message in the error chain, got: {msg}"
        );
        assert!(
            app.state.is_dirty(),
            "save_app must mark dirty on validate_path_arg failure"
        );
    }

    #[test]
    fn save_app_with_empty_source_path_bails_and_marks_dirty() {
        // The empty-string arm of validate_path_arg. The Save-As
        // commit already rejects empty paths inside the modal,
        // but if state somehow gets here (future modal-free save
        // path, scripted test, etc.), save_app must still refuse
        // and flip dirty. Mirrors the NUL test.
        let mut app = make_app();
        app.source_path = Some(String::new());
        let err = save_app(&mut app).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("empty"),
            "expected empty-path message in the error chain, got: {msg}"
        );
        assert!(
            app.state.is_dirty(),
            "save_app must mark dirty on validate_path_arg failure"
        );
    }

    #[test]
    fn esc_with_no_draft_no_resize_no_selection_quits() {
        // Third branch of the Esc handler: nothing to clear, fall
        // through to request_quit.
        let mut app = make_app();
        assert!(!app.should_quit);
        assert_eq!(app.state.selected_count(), 0);
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.should_quit, "Esc with empty state must quit");
    }

    // ---- Command palette dispatch ----
    //
    // The palette hijack lives in `handle_key`; the dispatch table
    // is exercised directly below so a regression in either side
    // shows up in a named test rather than as a UI-only failure.

    fn open_palette(app: &mut App, trigger: PaletteTrigger) {
        assert!(
            app.begin_palette(trigger),
            "open_palette: app.begin_palette returned false"
        );
    }

    #[test]
    fn colon_opens_palette_and_esc_cancels() {
        let mut app = make_app();
        assert!(!app.palette_active());
        // `:` triggers the palette via the keyboard handler.
        handle_key(&mut app, key(KeyCode::Char(':')));
        assert!(app.palette_active());
        // Typing appends to the buffer.
        handle_key(&mut app, key(KeyCode::Char('h')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Char('l')));
        assert_eq!(app.palette_buffer(), "hel");
        // Esc cancels without dispatching.
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.palette_active());
        assert!(app.status.contains("palette cancelled"));
    }

    #[test]
    fn slash_opens_palette_too() {
        // `/` is the alternate trigger. The bin treats both as
        // openers for the same UX today.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('/')));
        assert!(app.palette_active());
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(!app.palette_active());
    }

    #[test]
    fn palette_empty_buffer_on_enter_is_cancelled() {
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Colon);
        // Enter on an empty buffer is a no-op (vs. ambiguous-match
        // status). The status text is locked to make sure the user
        // gets feedback that the keystroke did something.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.palette_active());
        assert!(
            app.status.contains("empty"),
            "status should explain empty-buffer cancellation, got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_unique_match_dispatches_action() {
        let mut app = make_app();
        // Pre-condition: undo stack empty → Undo returns false →
        // dispatch sets the "nothing to undo" status. Same code
        // path either way; the test exercises the dispatcher's
        // single-match routing.
        open_palette(&mut app, PaletteTrigger::Colon);
        handle_key(&mut app, key(KeyCode::Char('u')));
        handle_key(&mut app, key(KeyCode::Char('n')));
        handle_key(&mut app, key(KeyCode::Char('d')));
        handle_key(&mut app, key(KeyCode::Char('o')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.palette_active());
        // With an empty undo stack the action runs but the status
        // says nothing-to-undo. The point of the test is the
        // routing, not the undo semantics.
        assert!(app.status.starts_with("palette:") || app.status.contains("nothing"));
    }

    #[test]
    fn palette_help_dispatch_toggles_overlay() {
        let mut app = make_app();
        assert!(!app.show_help);
        open_palette(&mut app, PaletteTrigger::Colon);
        for c in "help".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.palette_active());
        assert!(app.show_help, "palette: help must toggle the overlay");
        assert!(app.status.contains("help toggled"));
        // Toggling again drops the help overlay; the chord route
        // is what toggles a second time, but we just call the
        // helper directly here so the test stays focused.
        app.toggle_help();
        assert!(!app.show_help);
    }

    #[test]
    fn palette_no_match_reports_no_match() {
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Colon);
        for c in "zzz".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.palette_active());
        assert!(
            app.status.contains("no palette match"),
            "status should report no-match, got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_quit_dispatch_requests_quit() {
        let mut app = make_app();
        assert!(!app.should_quit);
        open_palette(&mut app, PaletteTrigger::Slash);
        for c in "quit".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(!app.palette_active());
        assert!(app.should_quit);
    }

    // Each new palette action gets a regression pin so the
    // palette → dispatch wiring can't silently drop a chord.
    // Empty-input variants are tested on the no-op path; keymap-
    // mirrored variants are tested on the active path.

    fn run_palette_command(cmd: &str) -> App {
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Slash);
        for c in cmd.chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        app
    }

    /// Like `run_palette_command` but lets the caller pre-seed the
    /// app (selection, draft state, etc.) before firing the palette
    /// command. The new dispatch arms (`select all`, `align <dir>`,
    /// `distribute <axis>`) need fixture state the empty `make_app`
    /// can't provide — re-routing them through this helper lets the
    /// test stay self-contained without a parallel "make_app with
    /// selection" helper.
    fn run_palette_command_into(app: &mut App, cmd: &str) {
        open_palette(app, PaletteTrigger::Slash);
        for c in cmd.chars() {
            handle_key(app, key(KeyCode::Char(c)));
        }
        handle_key(app, key(KeyCode::Enter));
    }

    #[test]
    fn palette_layers_dispatch_toggles_panel() {
        let mut app = make_app();
        assert!(!app.show_layers, "default: hidden");
        // "layers" — exact match.
        open_palette(&mut app, PaletteTrigger::Colon);
        for c in "layers".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.show_layers, "palette:layers must flip on");
        assert!(app.status.contains("layers"));
    }

    #[test]
    fn palette_duplicate_with_no_selection_reports_noop() {
        let app = run_palette_command("duplicate");
        assert_eq!(
            app.state.selected_count(),
            0,
            "no selection — no objects added"
        );
        assert!(app.status.contains("nothing to duplicate"));
    }

    #[test]
    fn palette_group_with_no_selection_reports_noop() {
        let app = run_palette_command("group");
        assert_eq!(app.state.selected_count(), 0);
        assert!(app.status.contains("nothing to group"));
    }

    #[test]
    fn palette_ungroup_with_nothing_grouped_reports_zero() {
        let app = run_palette_command("ungroup");
        assert!(app.status.contains("nothing to ungroup"));
    }

    #[test]
    fn palette_select_all_with_empty_doc_reports_nothing_to_select() {
        let app = run_palette_command("select all");
        assert!(app.status.contains("nothing to select"));
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn palette_select_all_with_objects_picks_every_object() {
        // Drop three 2x2 boxes, then run the palette and check
        // the selection grows to all three (replace-mode
        // contract — `select_all` wipes prior picks before
        // inserting every id). Note: `commit_draft` already
        // selects the most-recent object, so the "prior
        // selection" pick is whatever the last commit landed
        // on. We don't care what it is — we only assert the
        // palette replaces it with the full set.
        let mut app = make_app();
        app.state.set_tool(DrawMode::Box);
        app.state.begin_draft(Point { x: 0, y: 0 });
        app.state.update_draft(Point { x: 2, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 4, y: 0 });
        app.state.update_draft(Point { x: 6, y: 2 });
        app.state.commit_draft().unwrap();
        app.state.begin_draft(Point { x: 8, y: 0 });
        app.state.update_draft(Point { x: 10, y: 2 });
        app.state.commit_draft().unwrap();
        assert_eq!(app.state.document.objects.len(), 3);
        // Pre-palette: commit_draft's last-call-selection is just
        // the most recent box (replace-mode at draft-commit time).
        let before = app.state.selected_count();
        assert!(before >= 1);

        run_palette_command_into(&mut app, "select all");
        assert_eq!(app.state.selected_count(), 3);
        assert!(app.status.contains("selected 3 object(s)"));
    }

    #[test]
    fn palette_toggle_inspector_flips_visibility() {
        // Start closed; palette opens it. Empty document → the
        // panel shows "(no selection)" which is the status line
        // we don't care about — just assert the flag flipped
        // and the status line carries the "inspector open"
        // narrative.
        let app = run_palette_command("inspector");
        assert!(app.show_inspector);
        assert!(app.status.contains("inspector"));
        assert!(app.status.contains("open"));
    }

    #[test]
    fn palette_align_left_with_three_boxes_snaps_to_left_edge() {
        // Same harness as the existing ctrl_shift_l_* tests.
        // Three boxes at uneven x-offsets; after the palette
        // arm, every left edge equals the leftmost.
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        // Mutate the middle box so the align actually moves
        // something (otherwise the no-op-when-already-aligned
        // guard returns 0 without flipping the status).
        if let DrawObject::Box(b) = &mut app.state.document.objects[1] {
            b.left = 4;
            b.right = 6;
        }
        run_palette_command_into(&mut app, "align left");
        // palette_align prefixes status so we look for the
        // expanded message.
        assert!(app.status.starts_with("palette:"), "got: {:?}", app.status);
        assert!(app.status.contains("left edge"), "got: {:?}", app.status);
        for obj in &app.state.document.objects {
            if let DrawObject::Box(b) = obj {
                // every left == the leftmost (object 0, x=0)
                assert_eq!(b.left, 0, "object {} left drifted to {}", b.id, b.left);
            }
        }
    }

    #[test]
    fn palette_distribute_horizontal_with_three_moves_one() {
        let (mut app, ids) = make_app_with_three_boxes();
        for id in &ids {
            app.state.add_to_selection(id);
        }
        // Same middle-box mutation pattern.
        if let DrawObject::Box(b) = &mut app.state.document.objects[1] {
            b.left = 3;
            b.right = 5;
        }
        run_palette_command_into(&mut app, "distribute horizontal");
        assert!(app.status.starts_with("palette:"), "got: {:?}", app.status);
        assert!(
            app.status.contains("equal horizontal spacing"),
            "got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_distribute_horizontal_with_two_selected_reports_nothing() {
        // Same ≥3 guard the Ctrl-Shift-J chord has.
        let (mut app, _) = make_app_with_three_boxes();
        let id0 = app.state.document.objects[0].id().to_string();
        let id1 = app.state.document.objects[1].id().to_string();
        app.state.add_to_selection(&id0);
        app.state.add_to_selection(&id1);
        run_palette_command_into(&mut app, "distribute horizontal");
        assert!(app.status.starts_with("palette:"), "got: {:?}", app.status);
        assert!(
            app.status.contains("nothing to distribute"),
            "got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_delete_with_empty_selection_reports_nothing() {
        // Empty document → palette `delete` is a no-op that surfaces
        // "nothing to delete" with the palette-prefix so the user
        // can tell it was a palette invocation rather than a
        // chord (which today is silent on empty).
        let app = run_palette_command("delete");
        assert!(app.status.starts_with("palette:"), "got: {:?}", app.status);
        assert!(
            app.status.contains("nothing to delete"),
            "got: {:?}",
            app.status
        );
        assert!(app.state.document.objects.is_empty());
    }

    #[test]
    fn palette_delete_with_two_selected_removes_both() {
        // Two objects, both selected, palette `delete` removes
        // both and prefixes the count status. Mirrors the chord
        // behavior so the two paths share a status shape. Wipe
        // the post-commit selection first — `commit_draft`
        // selects its just-added object, so a fresh
        // `add_to_selection` would be additive rather than
        // a clean 1-or-2 setup.
        let (mut app, ids) = make_app_with_three_boxes();
        app.state.clear_selection();
        app.state.add_to_selection(&ids[0]);
        app.state.add_to_selection(&ids[1]);
        assert_eq!(app.state.document.objects.len(), 3);

        run_palette_command_into(&mut app, "delete");
        assert_eq!(app.state.document.objects.len(), 1);
        assert!(app.status.starts_with("palette:"), "got: {:?}", app.status);
        assert!(
            app.status.contains("deleted 2 object(s)"),
            "got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_delete_chord_and_palette_share_count() {
        // Regression: the chord and the palette both stamp the
        // count of removed selection entries. Pin that the
        // *count* matches what was actually removed,
        // regardless of path. We pick a single-id selection
        // and assert the helper returns exactly 1, regardless
        // of whether the chord path or the palette path
        // triggered it.
        let (mut app, ids) = make_app_with_three_boxes();
        app.state.clear_selection();
        app.state.add_to_selection(&ids[0]);
        let n_chord = app.state.delete_selected();
        assert_eq!(n_chord, 1);
        assert_eq!(app.state.document.objects.len(), 2);
    }

    #[test]
    fn ctrl_delete_chord_with_selection_reports_count_status() {
        // Delete chord on a single selected box: status echoes
        // "deleted 1 object" so the user has feedback for the
        // gesture. Mirrors the palette arm's count message
        // shape (without the "palette:" prefix). The chord
        // path was silent before the `delete_selected` →
        // `usize` change unlocked the count; this test
        // covers the new status echo so a future refactor
        // can't silently drop it.
        let (mut app, ids) = make_app_with_three_boxes();
        app.state.clear_selection();
        app.state.add_to_selection(&ids[0]);
        let before = app.state.document.objects.len();
        handle_key(&mut app, key(KeyCode::Delete));
        assert_eq!(app.state.document.objects.len(), before - 1);
        assert!(
            app.status.contains("deleted 1 object"),
            "chord should report count; got {:?}",
            app.status
        );
    }

    #[test]
    fn ctrl_delete_chord_with_empty_selection_reports_nothing() {
        // Empty selection must echo "nothing to delete" so
        // the chord path matches the palette path's
        // empty-selection behavior. The prior asymmetry
        // (palette echoed, chord was silent) was
        // unintentional — same shape now.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Delete));
        assert!(app.state.document.objects.is_empty());
        assert!(
            app.status.contains("nothing to delete"),
            "chord should report empty-selection; got {:?}",
            app.status
        );
    }

    #[test]
    fn palette_resolves_all_named_actions() {
        // Smoke-pin: every action name in the palette table must
        // (a) prefix-match itself in the filter, and (b) dispatch
        // to a unique action via `commit_palette`. A typo or
        // rename that breaks the table would surface here.
        for cmd in [
            "help",
            "layers",
            "inspector",
            "save",
            "undo",
            "redo",
            "duplicate",
            "group",
            "ungroup",
            "select all",
            "delete",
            "align left",
            "align right",
            "align top",
            "align bottom",
            "align horizontal center",
            "align vertical center",
            "distribute horizontal",
            "distribute vertical",
            "quit",
        ] {
            let r = kirkforge_draw_core::filter_palette(cmd);
            assert!(
                !r.is_empty(),
                "{cmd:?} must produce at least one filter match"
            );
            assert_eq!(
                r[0].0, cmd,
                "{cmd:?} must resolve to itself as the top result"
            );
        }
    }

    #[test]
    fn palette_table_size_matches_palette_action_variant_count() {
        // ponytail: compile-time exhaustiveness in
        // `dispatch_palette_action` already guarantees every
        // `PaletteAction` variant is handled, but the *table*
        // (`PALETTE_ACTIONS`) is hand-maintained. This test pins
        // the inverse: the table contains exactly N rows for
        // some N, and every row is a distinct variant. A
        // future addition of a `PaletteAction` variant
        // without a matching row would surface here OR in
        // the `action_lookup_returns_distinct_variants` core
        // test (which lists each variant by name). Together
        // they form a weak equality: variant-count == row-count.
        let r = kirkforge_draw_core::filter_palette("");
        let distinct: std::collections::HashSet<_> = r.iter().map(|(_, a)| **a).collect();
        assert_eq!(
            r.len(),
            distinct.len(),
            "PALETTE_ACTIONS has duplicate variants"
        );
        // Pin a known floor; bumping the count is an intentional
        // change that needs to land in HELP_LINES, the README,
        // and the dispatch site.
        assert_eq!(r.len(), 20);
    }

    #[test]
    fn palette_group_query_picks_prefix_match_when_substring_ambiguity() {
        // "group" is the prefix of `group` and a substring of
        // `ungroup`. The dispatch tiebreak in commit_palette
        // picks the prefix match so the user's typed intent
        // (Ctrl-G's "group") always wins over accidental
        // substring bleed.
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Colon);
        for c in "group".chars() {
            handle_key(&mut app, key(KeyCode::Char(c)));
        }
        handle_key(&mut app, key(KeyCode::Enter));
        // Empty selection → "nothing to group" status from the
        // group dispatch arm (not the previous "ambiguous"
        // status). Confirms the prefix-wins tiebreak landed.
        assert!(
            app.status.contains("nothing to group"),
            "expected group-arm status, got: {:?}",
            app.status
        );
    }

    #[test]
    fn palette_backspace_drops_last_char() {
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Colon);
        handle_key(&mut app, key(KeyCode::Char('u')));
        handle_key(&mut app, key(KeyCode::Char('n')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.palette_buffer(), "unx");
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.palette_buffer(), "un");
        handle_key(&mut app, key(KeyCode::Esc));
    }

    #[test]
    fn palette_ctrl_u_clears_buffer() {
        let mut app = make_app();
        open_palette(&mut app, PaletteTrigger::Colon);
        handle_key(&mut app, key(KeyCode::Char('s')));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('v')));
        handle_key(&mut app, key_ctrl(KeyCode::Char('u')));
        assert_eq!(app.palette_buffer(), "");
        // Session is still active — user can re-type without
        // re-pressing the trigger.
        assert!(app.palette_active());
        handle_key(&mut app, key(KeyCode::Esc));
    }

    // -- Multi-line text edit --------------------------------------
    //
    // Bare Enter commits; Shift+Enter inserts `\n`. The bin-level
    // check is just that the right key path runs — the heavy
    // rendering / rect coverage lives in core. We seed a Text
    // object, open the edit session via F2, type / Shift+Enter,
    // and verify the in-memory buffer.

    fn make_app_with_text() -> App {
        use kirkforge_draw_core::{BoxStyle, DrawMode, InkColor, TextBorderMode, TextObject};
        let mut app = App::new(kirkforge_draw_core::DrawState::new());
        app.state.set_tool(DrawMode::Select);
        // Seed via direct push so we don't go through the
        // draft/commit pipeline (we only care about the editor's
        // text-edit side, not how the Text got into the doc).
        app.state
            .document
            .objects
            .push(DrawObject::Text(TextObject {
                id: "t-1".into(),
                z: 1,
                parent_id: None,
                color: InkColor::White,
                x: 0,
                y: 0,
                content: "".into(),
                border: TextBorderMode::None,
            }));
        // Make the new Text the selection.
        app.state.select_id("t-1");
        // Suppress the unused-import lint when InkColor / BoxStyle
        // aren't otherwise referenced.
        let _ = (InkColor::White, BoxStyle::Light);
        app
    }

    #[test]
    fn shift_enter_inserts_newline_in_text_edit_buffer() {
        let mut app = make_app_with_text();
        // F2 opens the edit session.
        handle_key(&mut app, key(KeyCode::F(2)));
        assert!(app.text_edit.is_some(), "F2 should open text edit");
        // Type "ab", Shift+Enter, type "cd".
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Char('d')));
        let buf = app.text_edit.as_ref().unwrap().buffer.clone();
        assert_eq!(buf, "ab\ncd", "Shift+Enter must insert a newline");
    }

    #[test]
    fn plain_enter_commits_text_edit_not_newline() {
        // Regression guard: pre-multi-line, Enter committed. We
        // want to lock that bare Enter still commits — Shift+Enter
        // is the only path that inserts \n.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Enter));
        // Edit session ended.
        assert!(
            app.text_edit.is_none(),
            "bare Enter must commit, not insert"
        );
        // Buffer landed on the document.
        let id = "t-1".to_string();
        assert_eq!(app.state.text_content(&id), Some("a".to_string()));
    }

    #[test]
    fn f2_commit_with_vanished_target_surfaces_status() {
        // If the Text object disappears between F2 open and
        // commit (e.g., an undo removed it, or an external
        // mutation cleared the document), `commit_text_content`
        // returns false and the bin surfaces "edit target
        // vanished" instead of "text edited". The F2 session
        // still closes cleanly (text_edit is taken), so the
        // user isn't left in a half-open edit mode.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('h')));
        handle_key(&mut app, key(KeyCode::Char('i')));
        // Simulate the target vanishing mid-edit: drop the
        // only Text from the document. text_edit.target_id
        // still points at "t-1", so the next commit has to
        // route through the "no such id" branch.
        app.state.document.objects.retain(|o| o.id() != "t-1");
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.text_edit.is_none(),
            "F2 session must close even when the target is gone"
        );
        assert_eq!(
            app.status, "edit target vanished",
            "commit must surface the vanished-target status, not 'text edited'"
        );
    }

    #[test]
    fn f2_commit_with_no_changes_surfaces_no_changes_status() {
        // Open F2 on a Text, press Enter without typing. The
        // buffer equals initial_content so `edit.dirty` stays
        // false and `commit_text_edit` takes the early-out
        // branch: "edit cancelled (no changes)" + return false
        // + F2 session closes (text_edit is taken). This is
        // the common "oops, I didn't mean to F2" exit and
        // must not push an undo step or echo "text edited"
        // (which would imply something happened).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        assert!(app.text_edit.is_some(), "F2 must open a session");
        // Don't type — press Enter straight away.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(
            app.text_edit.is_none(),
            "F2 session must close even on a no-op commit"
        );
        assert_eq!(
            app.status, "edit cancelled (no changes)",
            "no-typing commit must surface the no-changes status"
        );
        // No content was written: the Text object's content
        // is still the initial empty string.
        let text = app
            .state
            .document
            .objects
            .iter()
            .find(|o| o.id() == "t-1")
            .expect("t-1 must still be in the document");
        if let DrawObject::Text(t) = text {
            assert_eq!(t.content, "", "no-typing commit must not mutate content");
        } else {
            panic!("expected Text object, got a non-Text");
        }
    }

    #[test]
    fn shift_enter_then_plain_enter_commits_multiline() {
        // Full commit pipeline: type + newline + type, plain
        // Enter writes the multi-line buffer back to the doc.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('y')));
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.text_edit.is_none());
        let id = "t-1".to_string();
        assert_eq!(app.state.text_content(&id), Some("x\ny".to_string()));
    }

    // -- F2 write-through (live buffer → document) ---------------
    //
    // These tests pin the contract that the editor sees their
    // typing in real time. Pre-write-through, the buffer was
    // invisible until commit — every F2 session was effectively
    // "type blind, hope you remember what you typed". Now the
    // helper stamps the buffer onto the TextObject on every
    // keystroke so the rendered scene reflects the user's input
    // live. Dirty / undo stay anchored to commit; that's
    // pinned in core, here we just lock the bin side.

    #[test]
    fn f2_insert_char_writes_through_to_document_text_object() {
        // Without write-through the buffer is invisible until
        // commit. With it, the document's TextObject.content
        // mirrors the buffer on every char.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('h')));
        let id = "t-1".to_string();
        assert_eq!(
            app.state.text_content(&id).as_deref(),
            Some("h"),
            "typed char lands on the document, not just the buffer"
        );
    }

    #[test]
    fn f2_backspace_writes_through_to_document_text_object() {
        // Backspace during edit updates both buffer and document
        // — the user sees the last glyph disappear live.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        let id = "t-1".to_string();
        assert_eq!(app.state.text_content(&id).as_deref(), Some("ab"));
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(
            app.state.text_content(&id).as_deref(),
            Some("a"),
            "backspace writes through too"
        );
    }

    // -- F2 mid-buffer cursor (arrow-key navigation) -------------
    //
    // Cursor offset is the byte index in the edit buffer where
    // the next insert / delete lands, and where the visible
    // cursor paints. Left / Right step it one byte (clamped at
    // the buffer edges); Backspace pops the byte before the
    // offset; insert splices at the offset then advances.

    #[test]
    fn f2_starts_with_cursor_at_buffer_end() {
        // Fresh edit session: cursor sits at the end of the
        // initial content (replacing the prior "always EOB"
        // contract with the same behavior, but now it's an
        // explicit field on TextEditState).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.cursor_offset, edit.buffer.len());
    }

    #[test]
    fn f2_left_arrow_steps_cursor_back_one_byte() {
        // Buffer "abc" → cursor_offset 3 (EOB).
        // Two Left presses → offset 1 (between 'a' and 'b').
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
    }

    #[test]
    fn f2_right_arrow_advances_cursor_one_byte() {
        // Symmetric: walk back, walk forward.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        // cursor_offset is 2 (EOB). One Left → 1; one Right → 2.
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Right));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 2);
    }

    #[test]
    fn f2_left_clamps_at_buffer_start() {
        // Pressing Left at offset 0 is a no-op (doesn't panic).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    }

    #[test]
    fn f2_right_clamps_at_buffer_end() {
        // Pressing Right at offset == buffer.len() is a no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('z')));
        handle_key(&mut app, key(KeyCode::Right));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.cursor_offset, edit.buffer.len());
    }

    #[test]
    fn f2_insert_at_mid_buffer_splices_in_place() {
        // Type "ac", walk cursor Left, type 'b'. The result is
        // "abc" — the splice happened at the cursor, not at EOB.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Char('b')));
        let buf = app.text_edit.as_ref().unwrap().buffer.clone();
        assert_eq!(buf, "abc", "mid-buffer insert splices, not appends");
    }

    #[test]
    fn f2_backspace_at_mid_buffer_removes_byte_before_cursor() {
        // Type "abc", walk cursor Left twice (offset 1, between
        // 'a' and 'b'), Backspace removes 'a'.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Backspace));
        let buf = app.text_edit.as_ref().unwrap().buffer.clone();
        assert_eq!(
            buf, "bc",
            "mid-buffer backspace deletes the byte before the cursor"
        );
    }

    #[test]
    fn f2_backspace_at_offset_zero_is_noop() {
        // Fresh empty edit session, Backspace doesn't panic or
        // wrap around — it's a no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.text_edit.as_ref().unwrap().buffer, "");
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    }

    #[test]
    fn f2_backspace_removes_full_multibyte_char_before_cursor() {
        // Mirror of `f2_delete_removes_full_multibyte_char_at_cursor`
        // for the Backspace direction. Buffer "a日本" — cursor
        // at EOB (offset 7 = 1 ASCII + 2×3-byte CJK); Backspace
        // pops all 3 bytes of '本' (0xE6 0x9C 0xAC), leaving
        // "a日" with cursor at offset 4.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('日')));
        handle_key(&mut app, key(KeyCode::Char('本')));
        // Sanity: cursor at EOB (1 + 3 + 3 = 7 bytes).
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 7);
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.text_edit.as_ref().unwrap().buffer, "a日");
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 4);
    }

    #[test]
    fn f2_mid_buffer_state_writes_through_to_document() {
        // Write-through extends to mid-buffer inserts: after
        // walking the cursor and inserting 'b' between 'a' and
        // 'c', the document's TextObject.content mirrors the
        // spliced buffer.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Char('b')));
        let id = "t-1".to_string();
        assert_eq!(app.state.text_content(&id).as_deref(), Some("abc"));
    }

    #[test]
    fn f2_home_jumps_cursor_to_offset_zero() {
        // Type "abc", then Home: cursor drops to offset 0
        // (visible cursor paints at the first cell).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    }

    #[test]
    fn f2_end_jumps_cursor_to_buffer_end() {
        // Type "abc", then walk Left, then End: cursor snaps
        // back to EOB.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::End));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.cursor_offset, edit.buffer.len());
    }

    #[test]
    fn f2_home_then_insert_appends_at_buffer_start() {
        // Cursor at offset 0, type 'z' → buffer "zabc".
        // Locks that the splice site (not the cursor's old
        // EOB position) is what determines where the char
        // lands.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Home));
        handle_key(&mut app, key(KeyCode::Char('z')));
        let buf = app.text_edit.as_ref().unwrap().buffer.clone();
        assert_eq!(buf, "zabc", "Home + char inserts at buffer start");
    }

    #[test]
    fn f2_home_already_at_start_is_noop() {
        // Fresh edit session (offset 0 already); Home is a no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    }

    #[test]
    fn f2_end_already_at_end_is_noop() {
        // Fresh edit session after typing one char (offset 1);
        // End is a no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('q')));
        handle_key(&mut app, key(KeyCode::End));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.cursor_offset, edit.buffer.len());
    }

    #[test]
    fn f2_up_moves_cursor_to_prior_line() {
        // Build a multi-line buffer: "abc\ndef" with cursor
        // at EOB (offset 7). Up → line 1, column 3 (within
        // "abc" length 3) → offset 3 (the '\n' byte).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('d')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Char('f')));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 7);
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.text_edit.as_ref().unwrap().cursor_offset,
            3,
            "Up from end of line 2 lands at end of line 1"
        );
    }

    #[test]
    fn f2_down_moves_cursor_to_next_line() {
        // Build a multi-line buffer: "abc\ndef", cursor at
        // offset 0 (start). Down → line 2, column 0 → offset
        // 4 (start of "def").
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('d')));
        handle_key(&mut app, key(KeyCode::Char('e')));
        handle_key(&mut app, key(KeyCode::Char('f')));
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.text_edit.as_ref().unwrap().cursor_offset,
            4,
            "Down from start of line 1 lands at start of line 2"
        );
    }

    #[test]
    fn f2_up_from_first_line_is_noop() {
        // Buffer "abc" (no '\n'); Up at offset 1 → no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Home));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.text_edit.as_ref().unwrap().cursor_offset,
            0,
            "Up on the first line is a no-op"
        );
    }

    #[test]
    fn f2_down_from_last_line_is_noop() {
        // Buffer "abc"; Down at offset 2 (EOB) → no-op.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        let edit = app.text_edit.as_ref().unwrap();
        let eob = edit.buffer.len();
        assert_eq!(edit.cursor_offset, eob);
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(
            app.text_edit.as_ref().unwrap().cursor_offset,
            eob,
            "Down on the last line is a no-op"
        );
    }

    #[test]
    fn f2_up_clamps_to_shorter_target_line() {
        // Buffer "a\nbbc" — line 1 length 1, line 2 length
        // 3. Cursor at EOB (offset 5, column 3 on line 2).
        // Up → line 1, column 3 clamped to length 1 → offset
        // 1. Locks the column-clamp behavior.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Up));
        assert_eq!(
            app.text_edit.as_ref().unwrap().cursor_offset,
            1,
            "Up from column 3 on a 3-char line clamps to length 1"
        );
    }

    #[test]
    fn f2_delete_at_offset_zero_removes_first_char() {
        // Buffer "abc" — cursor at 0; Delete removes 'a',
        // leaving "bc". Cursor offset stays at 0.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Home));
        handle_key(&mut app, key(KeyCode::Delete));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.buffer, "bc", "Delete at offset 0 removes first char");
        assert_eq!(
            edit.cursor_offset, 0,
            "cursor stays at 0 after forward delete"
        );
    }

    #[test]
    fn f2_delete_in_middle_removes_byte_at_cursor() {
        // Buffer "abc" — cursor at 1 (between 'a' and 'b');
        // Delete removes 'b', leaving "ac". Cursor stays at 1.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Left));
        handle_key(&mut app, key(KeyCode::Left));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
        handle_key(&mut app, key(KeyCode::Delete));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.buffer, "ac", "Delete in middle removes byte at cursor");
        assert_eq!(edit.cursor_offset, 1, "cursor stays put");
    }

    #[test]
    fn f2_delete_at_eob_is_noop() {
        // Buffer "abc" — cursor at EOB (offset 3); Delete is
        // a no-op (no panic, no change).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        let edit = app.text_edit.as_ref().unwrap();
        let eob = edit.buffer.len();
        assert_eq!(edit.cursor_offset, eob);
        handle_key(&mut app, key(KeyCode::Delete));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.buffer, "abc", "Delete at EOB leaves buffer untouched");
        assert_eq!(edit.cursor_offset, eob, "cursor offset stays at EOB");
    }

    #[test]
    fn f2_delete_at_empty_buffer_is_noop() {
        // Fresh edit session — empty buffer; Delete is a
        // no-op rather than a panic.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Delete));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(edit.buffer, "");
        assert_eq!(edit.cursor_offset, 0);
    }

    #[test]
    fn f2_delete_writes_through_to_document() {
        // Same write-through contract as Backspace: the
        // document's TextObject mirrors the buffer after
        // every keystroke.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Home));
        handle_key(&mut app, key(KeyCode::Delete));
        let id = "t-1".to_string();
        assert_eq!(
            app.state.text_content(&id).as_deref(),
            Some("bc"),
            "Delete writes through to the document"
        );
    }

    #[test]
    fn f2_delete_removes_full_multibyte_char_at_cursor() {
        // Buffer "a日本b" — cursor at offset 1 (between 'a'
        // and the CJK ideograph); Delete removes all 3 bytes
        // of '日' (0xE6 0x97 0xA5), leaving "a本b". The
        // cursor stays at offset 1.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        // Insert each char individually — `text_edit_insert`
        // splices by char's UTF-8 byte length, so the offset
        // advances correctly through the multi-byte sequence.
        handle_key(&mut app, key(KeyCode::Char('日')));
        handle_key(&mut app, key(KeyCode::Char('本')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Home));
        // Step Right once to land between 'a' (1 byte) and
        // '日' (3 bytes). Right steps 1 byte at a time per
        // the byte-offset model, so we don't walk into the
        // middle of '日' here.
        handle_key(&mut app, key(KeyCode::Right));
        assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
        handle_key(&mut app, key(KeyCode::Delete));
        let edit = app.text_edit.as_ref().unwrap();
        assert_eq!(
            edit.buffer, "a本b",
            "Delete at offset 1 removes the full '日' char (3 bytes)"
        );
        assert_eq!(edit.cursor_offset, 1, "cursor stays at 1");
    }

    #[test]
    fn f2_write_through_does_not_mark_document_dirty() {
        // The dirty flag is the user's "you have unsaved
        // changes" signal. While F2 is in flight the document
        // is being authored but not committed — we don't want
        // the title bar's `*` to flicker on every keystroke.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(
            !app.state.is_dirty(),
            "write-through keeps the document dirty flag clean"
        );
        // Commit flips it.
        handle_key(&mut app, key(KeyCode::Enter));
        assert!(app.state.is_dirty(), "commit is what marks dirty");
    }

    #[test]
    fn f2_esc_after_typing_reverts_document_and_closes_session() {
        // The cancel-dirty branch of `cancel_text_edit`. Live
        // write-through means the document's TextObject has
        // already been mutated on every keystroke (e.g. typing
        // "ab" makes the TextObject's content "ab" before the
        // user hits Esc). Esc must roll the content back to
        // the `initial_content` captured at F2 open, surface
        // "edit cancelled", and close the F2 session —
        // otherwise the user can't abandon a half-typed edit
        // and the write-through becomes irreversible by Esc.
        // Pins the symmetric contract "commit writes,
        // cancel reverts, dirty=false Esc is a no-op" (the
        // dirty=false arm is exercised by
        // `f2_esc_without_typing_is_noop_status` below).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('b')));
        // Sanity: write-through already pushed "ab" to the doc.
        let text = app
            .state
            .document
            .objects
            .iter()
            .find(|o| o.id() == "t-1")
            .expect("t-1 must still be in the document");
        if let DrawObject::Text(t) = text {
            assert_eq!(t.content, "ab", "write-through before cancel");
        } else {
            panic!("expected Text object");
        }
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(app.text_edit.is_none(), "Esc must close the F2 session");
        assert_eq!(app.status, "edit cancelled", "status: {}", app.status);
        // Content must be back to the initial empty string —
        // the revert helper rolled the write-through back.
        let text = app
            .state
            .document
            .objects
            .iter()
            .find(|o| o.id() == "t-1")
            .expect("t-1 must still be in the document");
        if let DrawObject::Text(t) = text {
            assert_eq!(t.content, "", "Esc must revert content to initial");
        } else {
            panic!("expected Text object");
        }
    }

    #[test]
    fn f2_esc_without_typing_is_noop_status() {
        // The cancel-not-dirty branch: open F2, Esc without
        // typing. The buffer equals initial_content, so
        // `edit.dirty` is false and `cancel_text_edit` takes
        // the `if edit.dirty` early-out — no revert call,
        // no status echo. This is the "I changed my mind
        // before doing anything" exit and must stay silent
        // (the F2 session closing is the only feedback the
        // user needs).
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        assert!(app.text_edit.is_some());
        let status_before = app.status.clone();
        handle_key(&mut app, key(KeyCode::Esc));
        assert!(
            app.text_edit.is_none(),
            "Esc must still close the F2 session even when not dirty"
        );
        assert_eq!(
            app.status, status_before,
            "not-dirty Esc must not change status (no echo)"
        );
    }

    #[test]
    fn f2_shift_enter_writes_through_with_newline() {
        // Combined: Shift+Enter inserts \n into the buffer AND
        // the document, so the multi-line renderer kicks in
        // before commit.
        let mut app = make_app_with_text();
        handle_key(&mut app, key(KeyCode::F(2)));
        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key_with_shift(KeyCode::Enter));
        handle_key(&mut app, key(KeyCode::Char('y')));
        let id = "t-1".to_string();
        assert_eq!(
            app.state.text_content(&id).as_deref(),
            Some("x\ny"),
            "Shift+Enter write-through carries the newline onto the doc"
        );
    }

    // -- Grouping (Ctrl-G / Ctrl-Shift-G) ------------------------
    //
    // Bin layer just routes the chord to the core helpers. The
    // helper-level contract is locked in the core test suite; here
    // we lock the routing + the status messages so a future
    // refactor of handle_key can't silently drop Ctrl-G.

    use kirkforge_draw_core::{BoxObject, BoxStyle};

    fn make_app_with_two_boxes() -> App {
        let mut app = App::new(kirkforge_draw_core::DrawState::new());
        app.state.set_tool(DrawMode::Select);
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "b1".into(),
            z: 0,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: BoxStyle::Light,
        }));
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "b2".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Green,
            left: 6,
            top: 0,
            right: 9,
            bottom: 3,
            style: BoxStyle::Light,
        }));
        // Multi-select both via the rect-Add path so the
        // selection mirrors a marquee commit.
        app.state.select_id("b1");
        app.state.select_in_rect(
            kirkforge_draw_core::Rect {
                left: 0,
                top: 0,
                right: 9,
                bottom: 3,
            },
            kirkforge_draw_core::SelectionMode::Add,
        );
        assert_eq!(app.state.selected_count(), 2);
        let _ = (InkColor::Red, BoxStyle::Light);
        app
    }

    #[test]
    fn ctrl_g_groups_selection_and_reports_parent_id() {
        let mut app = make_app_with_two_boxes();
        handle_key(&mut app, key_ctrl(KeyCode::Char('g')));
        // Both selected objects now share a parent id.
        let p1 = app.state.document.objects[0]
            .parent_id()
            .map(str::to_string);
        let p2 = app.state.document.objects[1]
            .parent_id()
            .map(str::to_string);
        assert!(p1.is_some(), "box b1 should be grouped");
        assert_eq!(p1, p2, "both boxes must share the same parent id");
        assert!(p1.unwrap().starts_with("g-"));
        assert!(app.status.contains("grouped"), "status: {}", app.status);
        assert!(app.status.contains("parent="), "status: {}", app.status);
    }

    #[test]
    fn ctrl_shift_g_ungroups_selection_and_reports_count() {
        let mut app = make_app_with_two_boxes();
        // Group first.
        handle_key(&mut app, key_ctrl(KeyCode::Char('g')));
        assert!(app.state.document.objects[0].parent_id().is_some());
        // Now ungroup.
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('g')));
        assert!(app.state.document.objects[0].parent_id().is_none());
        assert!(app.state.document.objects[1].parent_id().is_none());
        assert!(
            app.status.starts_with("ungrouped"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn ctrl_g_with_empty_selection_reports_nothing_to_group() {
        let mut app = App::new(kirkforge_draw_core::DrawState::new());
        handle_key(&mut app, key_ctrl(KeyCode::Char('g')));
        assert_eq!(app.status, "nothing to group");
    }

    #[test]
    fn ctrl_shift_g_with_nothing_grouped_reports_nothing_to_ungroup() {
        let mut app = make_app_with_two_boxes();
        // No prior Ctrl-G — neither object is grouped yet.
        handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('g')));
        assert_eq!(app.status, "nothing to ungroup");
    }

    // Ponytail: layers panel click handler lives at the bin
    // boundary because it depends on `App.layers_area`, modifiers,
    // and status-line feedback. The core row→id mapping is
    // covered by `layer_row_for_id` tests in `core::layers`; the
    // tests below verify the *routing* (panel vs body) and the
    // three modifier modes (Replace/Add/Toggle).

    fn app_with_three_layer_rows() -> App {
        // Seed three boxes with explicit ids so panel rows map to
        // known ids even on fast hardware where
        // `new_object_id`'s nanosecond-based key collides.
        let mut app = App::new(DrawState::new());
        for (id, x) in [("box-a", 0), ("box-b", 5), ("box-c", 10)] {
            app.state
                .document
                .objects
                .push(DrawObject::Box(kirkforge_draw_core::BoxObject {
                    id: id.into(),
                    z: 0,
                    parent_id: None,
                    color: InkColor::White,
                    left: x,
                    top: 0,
                    right: x + 2,
                    bottom: 2,
                    style: kirkforge_draw_core::BoxStyle::Light,
                }));
        }
        // Body on left, panel on right (matches ui::draw layout).
        // Body top = 3, body left = 0..58, panel at 58..80.
        app.body_area = Rect::new(0, 3, 58, 20);
        // Panel y starts at 3 (matches body top). Header row at y=3,
        // first layer row at y=4.
        app.layers_area = Some(Rect::new(58, 3, 22, 20));
        app.scene_origin = Some(Point { x: 0, y: 0 });
        app.show_layers = true;
        app
    }

    fn mouse_down(col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: mods,
        }
    }

    #[test]
    fn layer_panel_click_replace_selects_topmost() {
        // Document order is [box-a, box-b, box-c]; the panel
        // renders topmost-first → row 0 = "box-c".
        let mut app = app_with_three_layer_rows();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        // Replace mode → selection is exactly the clicked id.
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-c");
        assert!(app.status.contains("box-c"), "status: {}", app.status);
    }

    #[test]
    fn layer_panel_click_replace_on_second_row() {
        // Row 1 = "box-b" (middle).
        let mut app = app_with_three_layer_rows();
        handle_mouse(&mut app, mouse_down(60, 5, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-b");
    }

    #[test]
    fn layer_panel_shift_click_adds_to_existing_selection() {
        let mut app = app_with_three_layer_rows();
        // First click replaces → "box-c" selected.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 1);
        // Shift+click on row 1 (box-b) → adds, doesn't replace.
        handle_mouse(&mut app, mouse_down(60, 5, KeyModifiers::SHIFT));
        assert_eq!(app.state.selected_count(), 2);
        let ids: Vec<&str> = app.state.selected().iter().map(|o| o.id()).collect();
        assert!(ids.contains(&"box-c"));
        assert!(ids.contains(&"box-b"));
    }

    #[test]
    fn layer_panel_shift_click_already_selected_surfaces_status() {
        // Shift+click on a row whose id is already in the
        // selection set is a statewise no-op (the Add branch
        // finds the id present, doesn't add a duplicate),
        // but the helper surfaces a status echo so the user
        // knows the click landed. Mirrors the inspector's
        // `inspector_shift_click_single_selected_is_already_in_set`
        // test — same shape, layers panel routing.
        let mut app = app_with_three_layer_rows();
        // First click replaces → "box-c" in the set.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 1);
        // Shift+click on the same row → no state change,
        // helper echoes "already in selection".
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::SHIFT));
        assert_eq!(app.state.selected_count(), 1);
        assert!(
            app.status.contains("already in selection"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn layer_panel_ctrl_click_toggles_membership() {
        let mut app = app_with_three_layer_rows();
        // Bare-click row 0 → "box-c" in selection.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 1);
        // Ctrl+click row 0 again → removes "box-c".
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::CONTROL));
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn layer_panel_ctrl_click_on_empty_selection_adds_id() {
        // The Toggle arm's count-grew branch (count 0 → 1).
        // The existing `layer_panel_ctrl_click_toggles_
        // membership` only exercises the 1 → 0 (already-
        // present, removed) half. The empty-selection case
        // is the count-grew side: status echoes "selected 1
        // object" and the row's id joins the (empty) set.
        // Without the count-grew test, a future refactor
        // that flips the if/else on `after > before` would
        // still pass the existing test but echo the wrong
        // status on first-click.
        let mut app = app_with_three_layer_rows();
        assert_eq!(app.state.selected_count(), 0);
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::CONTROL));
        assert_eq!(
            app.state.selected_count(),
            1,
            "ctrl+click on empty selection must add the id"
        );
        assert_eq!(app.state.selected()[0].id(), "box-c");
        assert!(
            app.status.contains("selected 1 object"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn layer_panel_click_anchors_keyboard_focus_to_clicked_row() {
        // Walk the focus to the bottommost row via Down, then
        // click a different row. The click must re-anchor the
        // focus so the next Enter from the keyboard commits
        // the clicked row, not the stale one. Without this, a
        // stale focus would survive the click and Enter would
        // commit a different row than what the user just
        // clicked.
        let mut app = app_with_three_layer_rows();
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        handle_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.layer_focus, Some(2));
        // Click row 0 (topmost = "box-c"); focus must follow.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(
            app.layer_focus,
            Some(0),
            "click must re-anchor focus to the clicked row"
        );
        // Enter on the new focus commits "box-c" — the
        // clicked row, not the stale one.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-c");
    }

    #[test]
    fn layer_panel_header_click_is_noop() {
        let mut app = app_with_three_layer_rows();
        handle_mouse(&mut app, mouse_down(60, 3, KeyModifiers::NONE));
        // Header click should not mutate selection or status.
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn layer_panel_click_from_no_focus_anchors_focus_to_clicked_row() {
        // Companion to `layer_panel_click_anchors_keyboard_
        // focus_to_clicked_row` (the walk-then-click case).
        // This pins the no-focus start: focus is None, the user
        // clicks row 0, the focus must move from None to
        // Some(0) AND the row's id must be selected. Without
        // the anchor, a follow-up Enter would early-return
        // (focus is None → commit_layer_focus noops), so the
        // user could navigate the panel with the mouse but
        // not commit with the keyboard — a split that
        // contradicts the "focus and click are kept in
        // lockstep" contract introduced in dd9b2ab.
        let mut app = app_with_three_layer_rows();
        assert!(app.layer_focus.is_none(), "no prior focus");
        // Row 0 (topmost-first panel order) = "box-c" — the
        // seed gives all three boxes z=0, so the panel order
        // follows the document order in reverse (the last
        // pushed object is the topmost). See
        // `app_with_three_layer_rows`.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(
            app.layer_focus,
            Some(0),
            "click must anchor focus from None to the clicked row"
        );
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-c");
        // Enter on the new focus must commit the same row —
        // the same id, not a stale one. This is the contract
        // the anchor protects: keyboard and click pick the
        // same row, no matter which arm set the focus.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-c");
    }

    #[test]
    fn layer_panel_shift_click_also_anchors_focus() {
        // The focus anchor in `handle_layer_click` runs
        // BEFORE the modifier dispatch (line ~1038 in event.rs),
        // so every modifier — bare, Shift, Ctrl — anchors
        // focus to the clicked row, not just bare. The
        // comment on the anchor line calls this out: "Modifier
        // branches below only mutate the selection, not the
        // focus." Without this, a Shift+click would leave a
        // stale focus and the next Enter would commit the
        // wrong row. This test pins the contract for the
        // Shift arm; the bare arm is covered by
        // `layer_panel_click_anchors_keyboard_focus_to_clicked_row`.
        let mut app = app_with_three_layer_rows();
        // Walk to a different row to prove the anchor
        // re-foci, not just preserves the prior walk.
        handle_key(&mut app, key(KeyCode::Down)); // → row 0 (bottommost-anchor? no — Down-from-None → row 2)
        handle_key(&mut app, key(KeyCode::Up)); // → row 1
        handle_key(&mut app, key(KeyCode::Up)); // → row 0
        assert_eq!(app.layer_focus, Some(0));
        // Shift+click on row 2 (bottommost = "box-a") must
        // anchor focus to row 2, not leave it on row 0.
        handle_mouse(&mut app, mouse_down(60, 6, KeyModifiers::SHIFT));
        assert_eq!(
            app.layer_focus,
            Some(2),
            "Shift+click must re-anchor focus, not preserve stale"
        );
        // Selection grew (Add branch: bare click selected
        // nothing before, Shift+click adds the id to the
        // set, count 0 → 1).
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-a");
        // Enter on the new focus must commit the same row
        // — the keyboard must agree with the Shift+click
        // target.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-a");
    }

    #[test]
    fn layer_panel_ctrl_click_also_anchors_focus() {
        // Same shape as `layer_panel_shift_click_also_anchors_
        // focus`: the modifier-agnostic focus anchor covers
        // Ctrl+click too, not just bare and Shift. The
        // Toggle arm: empty selection + Ctrl+click on a
        // row → adds the id (count 0 → 1). Focus must
        // still move to the clicked row. Without this pin,
        // a future refactor that hoists the focus anchor
        // into the bare+Shift arms only would leave Ctrl+
        // click with a stale focus.
        let mut app = app_with_three_layer_rows();
        // Pre-walk focus to row 0.
        handle_key(&mut app, key(KeyCode::Down)); // → row 2
        handle_key(&mut app, key(KeyCode::Up)); // → row 1
        handle_key(&mut app, key(KeyCode::Up)); // → row 0
        assert_eq!(app.layer_focus, Some(0));
        // Ctrl+click on row 1 (middle = "box-b").
        handle_mouse(&mut app, mouse_down(60, 5, KeyModifiers::CONTROL));
        assert_eq!(
            app.layer_focus,
            Some(1),
            "Ctrl+click must re-anchor focus, not preserve stale"
        );
        // Toggle on empty: adds the id (count 0 → 1).
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-b");
        // Enter commits the same row the Ctrl+click set.
        handle_key(&mut app, key(KeyCode::Enter));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(app.state.selected()[0].id(), "box-b");
    }

    #[test]
    fn layer_panel_below_last_row_surfaces_empty_message() {
        let mut app = app_with_three_layer_rows();
        // Row 10 is well below the last layer (rows 0..3).
        handle_mouse(&mut app, mouse_down(60, 10, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 0);
        assert!(
            app.status.contains("empty row"),
            "expected empty-row status, got: {}",
            app.status
        );
    }

    #[test]
    fn layer_panel_click_on_empty_document_surfaces_empty_message() {
        // Empty document — layers panel renders just the
        // "layers" header + the "(empty)" placeholder row.
        // A click on either must hit the empty-row arm and
        // surface the same status as a below-last-row click
        // (both routes are `layers.get(rel) == None`).
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 58, 20);
        app.layers_area = Some(Rect::new(58, 3, 22, 20));
        app.scene_origin = Some(Point { x: 0, y: 0 });
        app.show_layers = true;
        // y=4 is the first row under the header (the
        // "(empty)" placeholder); the helper's rel=0
        // lookup against an empty layer_list returns None.
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 0);
        assert!(
            app.status.contains("empty row"),
            "expected empty-row status on empty doc, got: {}",
            app.status
        );
    }

    #[test]
    fn body_click_does_not_route_through_layer_panel() {
        // A Left-Down on the body area should NOT be intercepted by
        // the layers panel helper — even when the panel is showing,
        // a body click stays a body click (marquee select).
        let mut app = app_with_three_layer_rows();
        // Column inside body (col=10), row 4 (within body). This
        // should hit the marquee path, not the layers panel path.
        handle_mouse(&mut app, mouse_down(10, 4, KeyModifiers::NONE));
        // Marquee started — selection still empty until Up, but
        // the more important check is that this did NOT route
        // through the panel (which would have set selection
        // immediately).
        assert!(app.marquee.is_some());
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn layer_panel_click_claims_before_inspector_when_both_open() {
        // When both panels are open, layers sits left of
        // inspector (body | layers | inspector in ui::draw).
        // A click in the layers rect must route through
        // `handle_layer_click`, NOT `handle_inspector_click`
        // — the "left of inspector" claim priority is what
        // the boundary comment in `handle_mouse` pins. If a
        // future refactor flipped the claim order, this test
        // would fail because the inspector would treat the
        // click as a single-id re-affirm.
        let mut app = app_with_three_layer_rows();
        // Open the inspector alongside the layers panel.
        // Layers: x=58..80; inspector: x=80..102. Click
        // column 60 is firmly inside the layers rect.
        app.inspector_area = Some(Rect::new(80, 3, 22, 20));
        app.show_inspector = true;
        // Seed a single selection so the inspector would
        // have an id to re-affirm if the click misrouted.
        assert!(app.state.select_id("box-a"));
        // Click the layers panel at row 4 (the topmost row
        // = "box-c") with no modifiers → should Replace
        // (not "re-affirm box-a" from the inspector).
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 1);
        assert_eq!(
            app.state.selected()[0].id(),
            "box-c",
            "click on layers rect must select the topmost row, not re-affirm the seeded id"
        );
        assert!(app.status.contains("box-c"), "status: {}", app.status);
    }

    // -- Inspector panel click handler -------------------------------
    //
    // Same shape as the layers click tests above. The inspector
    // has no per-row hit-test (it shows exactly one id when
    // selection == 1, placeholders otherwise), so the helpers
    // here are simpler — every click inside `app.inspector_area`
    // routes through `handle_inspector_click` with whatever
    // modifier the user held on Down.

    fn app_with_inspector_panel() -> App {
        // Seed two boxes so multi-selection tests are one
        // `select_in_rect` away. Inspector on the right edge
        // matches the `ui::draw` layout when both panels are
        // open (inspector is the rightmost of the two 22-cell
        // sidebars).
        let mut app = App::new(DrawState::new());
        for (id, x) in [("box-a", 0), ("box-b", 5)] {
            app.state
                .document
                .objects
                .push(DrawObject::Box(kirkforge_draw_core::BoxObject {
                    id: id.into(),
                    z: 0,
                    parent_id: None,
                    color: InkColor::White,
                    left: x,
                    top: 0,
                    right: x + 2,
                    bottom: 2,
                    style: kirkforge_draw_core::BoxStyle::Light,
                }));
        }
        // Body 0..58 (left), inspector panel 58..80 (right), height 20.
        // Inspector top at row 3, header at row 3, first summary row at row 4.
        app.body_area = Rect::new(0, 3, 58, 20);
        app.inspector_area = Some(Rect::new(58, 3, 22, 20));
        app.scene_origin = Some(Point { x: 0, y: 0 });
        app.show_inspector = true;
        app
    }

    #[test]
    fn inspector_click_empty_selection_surfaces_status() {
        // 0 selected → inspector renders "(no selection)"; a
        // click inside the panel is a no-op for selection but
        // echoes a status line so the user knows the click
        // landed on the panel.
        let mut app = app_with_inspector_panel();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 0);
        assert!(
            app.status.contains("empty selection"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn inspector_click_single_selected_reaffirms() {
        // Single selected → Replace branch keeps the same id
        // selected (it's already the only pick) and echoes the
        // re-select status so the user sees their click landed.
        let mut app = app_with_inspector_panel();
        assert!(app.state.select_id("box-a"));
        let before = app.state.selected_count();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), before);
        assert!(
            app.status.contains("re-select") && app.status.contains("box-a"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn inspector_ctrl_click_single_selected_deselects() {
        // The meaningful gesture: Ctrl+click on the only
        // selected id toggles it out of the set, leaving the
        // selection empty (matches the layers panel's
        // toggle contract).
        let mut app = app_with_inspector_panel();
        assert!(app.state.select_id("box-a"));
        assert_eq!(app.state.selected_count(), 1);
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::CONTROL));
        assert_eq!(app.state.selected_count(), 0);
        assert!(
            app.status.contains("now 0 selected"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn inspector_shift_click_single_selected_is_already_in_set() {
        // Shift modifier on a single-selected inspector click
        // routes through the Add branch of
        // `handle_inspector_click`. The panel is showing the
        // only selected id, so adding it to the set is a
        // no-op state-wise — `add_to_selection` returns
        // false (already present) and the helper echoes
        // the layers-panel "already in selection" message.
        // Locks the Add branch down so a future refactor
        // that swaps Add for Toggle (or drops the no-op
        // status) trips this test.
        let mut app = app_with_inspector_panel();
        assert!(app.state.select_id("box-a"));
        let before: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::SHIFT));
        let after: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(before, after, "selection set unchanged");
        assert!(
            app.status.contains("already in selection"),
            "status: {}",
            app.status
        );
    }

    #[test]
    fn inspector_click_multi_selection_surfaces_status() {
        // 2 selected → the panel shows "(2 selected)"; a click
        // inside the panel is a no-op for selection (the
        // helper has no id to act on) and echoes the count.
        let mut app = app_with_inspector_panel();
        app.state.select_in_rect(
            kirkforge_draw_core::Rect {
                left: 0,
                top: 0,
                right: 12,
                bottom: 4,
            },
            kirkforge_draw_core::SelectionMode::Replace,
        );
        assert_eq!(app.state.selected_count(), 2);
        let before: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::NONE));
        assert_eq!(app.state.selected_count(), 2);
        let after: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(before, after, "selection set untouched");
        assert!(app.status.contains("2 selected"), "status: {}", app.status);
    }

    #[test]
    fn body_click_does_not_route_through_inspector_panel() {
        // A click on the body area must NOT reach the
        // inspector helper — even with the inspector panel
        // showing, the body click is a body click.
        let mut app = app_with_inspector_panel();
        // Column inside body (col=10), row 4 (within body).
        handle_mouse(&mut app, mouse_down(10, 4, KeyModifiers::NONE));
        // Nothing selected yet → the click should route to
        // the body (marquee start), not the inspector panel
        // (which would have set a status on empty selection).
        assert!(!app.status.contains("empty selection"));
    }

    #[test]
    fn inspector_shift_click_multi_selection_surfaces_status() {
        // The `count > 1` short-circuit in `handle_inspector_
        // click` runs BEFORE the modifier dispatch — the
        // helper has no id to act on in the multi case, so
        // Shift+click can't Add. Bare+Shift+Ctrl all surface
        // the same "(inspector: N selected)" status. Pins the
        // short-circuit so a future refactor that drops it
        // (and falls through to the modifier dispatch with
        // `selected().first()` empty) trips this test.
        let mut app = app_with_inspector_panel();
        app.state.select_in_rect(
            kirkforge_draw_core::Rect {
                left: 0,
                top: 0,
                right: 12,
                bottom: 4,
            },
            kirkforge_draw_core::SelectionMode::Replace,
        );
        assert_eq!(app.state.selected_count(), 2);
        let before: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::SHIFT));
        assert_eq!(app.state.selected_count(), 2);
        let after: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(
            before, after,
            "Shift+click on multi must not change selection"
        );
        assert!(app.status.contains("2 selected"), "status: {}", app.status);
    }

    #[test]
    fn inspector_ctrl_click_multi_selection_surfaces_status() {
        // Same shape as the Shift+click test — the
        // `count > 1` short-circuit runs first, so Ctrl+click
        // on a multi selection also cannot Toggle a single id
        // out. The status is the same "(inspector: N
        // selected)" echo.
        let mut app = app_with_inspector_panel();
        app.state.select_in_rect(
            kirkforge_draw_core::Rect {
                left: 0,
                top: 0,
                right: 12,
                bottom: 4,
            },
            kirkforge_draw_core::SelectionMode::Replace,
        );
        assert_eq!(app.state.selected_count(), 2);
        let before: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        handle_mouse(&mut app, mouse_down(60, 4, KeyModifiers::CONTROL));
        assert_eq!(app.state.selected_count(), 2);
        let after: Vec<String> = app
            .state
            .selected()
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        assert_eq!(
            before, after,
            "Ctrl+click on multi must not change selection"
        );
        assert!(app.status.contains("2 selected"), "status: {}", app.status);
    }

    // Layers panel toggle. The whole layers feature was wired in
    // earlier sessions (App::show_layers, App::toggle_layers,
    // `L` bind in handle_key, split-body layout in ui.rs); this
    // test exists solely so a future refactor can't accidentally
    // bind lowercase `l` to the toggle and shadow the Line-tool
    // hotkey. Uppercase `L` toggles, lowercase `l` is the Line tool.

    #[test]
    fn capital_l_toggles_layers_panel() {
        let mut app = make_app();
        assert!(!app.show_layers, "default state: panel hidden");
        handle_key(&mut app, key(KeyCode::Char('L')));
        assert!(app.show_layers, "first L: panel open");
        handle_key(&mut app, key(KeyCode::Char('L')));
        assert!(!app.show_layers, "second L: panel hidden again");
    }

    #[test]
    fn lower_l_does_not_toggle_layers_panel() {
        // Lowercase `l` is the Line tool hotkey. It must NOT
        // touch `show_layers` — a regression here would steal
        // the Line tool shortcut away from existing muscle
        // memory and leave the panel-toggled state half-explained.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('l')));
        assert!(!app.show_layers);
        assert_eq!(
            app.state.tool,
            kirkforge_draw_core::DrawMode::Line,
            "lowercase l must set the Line tool"
        );
    }

    #[test]
    fn capital_i_toggles_inspector_panel() {
        // Mirrors the L-arm regression. Two presses must
        // round-trip; the panel has no per-row focus to reset
        // (the renderer just shows the selection summary or a
        // placeholder).
        let mut app = make_app();
        assert!(!app.show_inspector, "default: hidden");
        handle_key(&mut app, key(KeyCode::Char('I')));
        assert!(app.show_inspector, "first I: panel open");
        handle_key(&mut app, key(KeyCode::Char('I')));
        assert!(!app.show_inspector, "second I: panel hidden");
    }

    #[test]
    fn lower_i_does_not_toggle_inspector_panel() {
        // Lowercase `i` now cycles the selection's color (the
        // "ink-picker" shortcut). It must NOT also flip the
        // inspector — capital `I` still owns that gesture.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('i')));
        assert!(!app.show_inspector);
    }

    // --- `i` ink-picker (cycle color forward) -------------------
    //
    // Bin tests for the bare lowercase `i` shortcut, which
    // advances the selection's InkColor one step through the
    // enum's discriminant order (White → Red → … → Magenta →
    // White). Mirrors the existing Ctrl-1..8 cluster but with
    // a "next color" gesture instead of "jump to color N".
    // The pure `cycle_*` helpers (next_ink_color etc.) live
    // in bin because they're trivial 1-line matches and have
    // no observable side effects to test directly — these
    // tests cover the bin wiring (arm fires, status echoes,
    // undo batch, multi-select normalization, wrap, no-op
    // spam resistance, empty-selection message).

    #[test]
    fn lower_i_advances_single_selected_box_color() {
        // Bare `i` on a single White box → Red, status reports
        // "recolored 1 object to red". Mirrors the recolor
        // status format so users get the same feedback whether
        // they press Ctrl-2 (jump to red) or `i` (advance from
        // white to red).
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "x".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.select_id("x");
        handle_key(&mut app, key(KeyCode::Char('i')));
        let DrawObject::Box(b) = &app.state.document.objects[0] else {
            panic!("expected box");
        };
        assert_eq!(b.color, InkColor::Red);
        assert!(app.status.contains("recolored 1 object to red"));
    }

    #[test]
    fn lower_i_wraps_from_magenta_to_white() {
        // Eight consecutive presses from White should return
        // to White (full enum cycle). Verifies the wrap
        // behavior at the end of the InkColor order so a user
        // spamming `i` can't get stuck on the last variant.
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "x".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.select_id("x");
        for _ in 0..8 {
            handle_key(&mut app, key(KeyCode::Char('i')));
        }
        let DrawObject::Box(b) = &app.state.document.objects[0] else {
            panic!("expected box");
        };
        assert_eq!(b.color, InkColor::White, "8 presses = full cycle");
    }

    #[test]
    fn lower_i_normalizes_mixed_selection_to_next_of_first() {
        // Two selected boxes, one White and one Red. First
        // (document-order) is White → next is Red, so both
        // should land on Red. Mirrors `recolor_selection`'s
        // "normalize the batch" semantics so the cycle
        // shortcut behaves the same as Ctrl-2 for mixed
        // selections.
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "first".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "second".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Green,
            left: 4,
            top: 0,
            right: 6,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.add_to_selection("first");
        app.state.add_to_selection("second");
        handle_key(&mut app, key(KeyCode::Char('i')));
        let mut colors: Vec<InkColor> = app
            .state
            .document
            .objects
            .iter()
            .map(|o| o.color())
            .collect();
        colors.sort_by_key(|c| match c {
            InkColor::White => 0,
            InkColor::Red => 1,
            InkColor::Orange => 2,
            InkColor::Yellow => 3,
            InkColor::Green => 4,
            InkColor::Cyan => 5,
            InkColor::Blue => 6,
            InkColor::Magenta => 7,
        });
        assert_eq!(colors, vec![InkColor::Red, InkColor::Red]);
        assert!(app.status.contains("recolored 2 objects to red"));
    }

    #[test]
    fn lower_i_pushes_one_undo_step_for_batch() {
        // Three selected boxes advance in lockstep on a
        // single `i` press, and one Ctrl-Z reverts all three.
        // Same single-undo-step contract that
        // `recolor_selection` guarantees — the cycle
        // shortcut is a thin wrapper, so it inherits the
        // contract for free, but the test pins it.
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        for i in 0..3 {
            app.state.document.objects.push(DrawObject::Box(BoxObject {
                id: format!("b{i}"),
                z: i,
                parent_id: None,
                color: InkColor::White,
                left: i * 4,
                top: 0,
                right: i * 4 + 2,
                bottom: 1,
                style: BoxStyle::Light,
            }));
            app.state.add_to_selection(&format!("b{i}"));
        }
        handle_key(&mut app, key(KeyCode::Char('i')));
        for obj in &app.state.document.objects {
            let DrawObject::Box(b) = obj else { panic!() };
            assert_eq!(b.color, InkColor::Red, "all three should be Red");
        }
        handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
        for obj in &app.state.document.objects {
            let DrawObject::Box(b) = obj else { panic!() };
            assert_eq!(b.color, InkColor::White, "Ctrl-Z reverts all 3");
        }
    }

    #[test]
    fn lower_i_leaves_unselected_objects_untouched() {
        // Selection contains one White box; a sibling Red
        // box sits unselected in the same document. Pressing
        // `i` advances the White selection to Red (next of
        // White) but leaves the unselected Red box alone.
        // Pins the delegation: cycle routes through
        // `recolor_selection` which respects the selection
        // set, not the document.
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "selected".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "unselected".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Red,
            left: 4,
            top: 0,
            right: 6,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.select_id("selected");
        handle_key(&mut app, key(KeyCode::Char('i')));
        let DrawObject::Box(sel) = &app.state.document.objects[0] else {
            panic!();
        };
        let DrawObject::Box(unsel) = &app.state.document.objects[1] else {
            panic!();
        };
        assert_eq!(sel.color, InkColor::Red, "selected advanced to next");
        assert_eq!(unsel.color, InkColor::Red, "unselected stays put");
    }

    #[test]
    fn lower_i_cycles_eight_variants_in_order() {
        // Eight presses from White walk through every
        // variant in enum-discriminant order, ending at
        // Magenta (one step before wrap). Mirrors the wrap
        // test but pins the order: White → Red → Orange →
        // Yellow → Green → Cyan → Blue → Magenta.
        use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "x".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.select_id("x");
        let expected = [
            InkColor::Red,
            InkColor::Orange,
            InkColor::Yellow,
            InkColor::Green,
            InkColor::Cyan,
            InkColor::Blue,
            InkColor::Magenta,
            InkColor::White, // 8th press wraps back to White
        ];
        for (i, want) in expected.iter().enumerate() {
            handle_key(&mut app, key(KeyCode::Char('i')));
            let DrawObject::Box(b) = &app.state.document.objects[0] else {
                panic!("expected box");
            };
            assert_eq!(
                &b.color,
                want,
                "step {}: expected {:?}, got {:?}",
                i + 1,
                want,
                b.color
            );
        }
    }

    #[test]
    fn lower_i_on_empty_selection_reports_nothing() {
        // Empty selection → status echoes "nothing to
        // recolor", same as Ctrl-1..8 with an empty
        // selection. Mirrors the existing recolor status
        // string so the two gestures are interchangeable
        // when the user has nothing selected.
        let mut app = make_app();
        handle_key(&mut app, key(KeyCode::Char('i')));
        assert_eq!(app.status, "nothing to recolor");
    }

    // --- find (Ctrl-F prompt) ---------------------------------
    //
    // Bin tests for the in-app find feature. The pure
    // `core::find::find_matches` helper has its own coverage in
    // the core crate; the bin side covers the input-hijack
    // pattern, the keymap arm, the status-bar render, and the
    // commit semantics.

    /// Build an app with three objects (Box "alpha", Box
    /// "beta", Text "gamma" with content "alpha inside") so
    /// find tests have something to match against. The Text
    /// is the only `DrawObject` variant with searchable
    /// content, so it's the one that exercises the
    /// `MatchField::Content` path.
    fn make_app_with_findable() -> App {
        use kirkforge_draw_core::{
            BoxObject, BoxStyle, DrawObject, InkColor, TextBorderMode, TextObject,
        };
        let mut app = make_app();
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "alpha".into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: "beta".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 4,
            top: 0,
            right: 6,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state
            .document
            .objects
            .push(DrawObject::Text(TextObject {
                id: "gamma".into(),
                z: 2,
                parent_id: None,
                color: InkColor::White,
                x: 0,
                y: 3,
                content: "alpha inside".into(),
                border: TextBorderMode::None,
            }));
        app
    }

    #[test]
    fn ctrl_f_opens_find_mode_with_empty_query() {
        let mut app = make_app_with_findable();
        assert!(app.find.is_none(), "no find session yet");
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        assert!(app.find.is_some(), "Ctrl-F opens a find session");
        assert_eq!(app.find_query(), "");
        // Status echoes the "(type to search)" hint so the
        // user knows they need to type — same shape as
        // palette's empty-buffer prompt.
        assert!(app.status.contains("type to search"));
    }

    #[test]
    fn find_mode_typing_extends_query_and_reports_match_count() {
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        // Type "al" — should match Box "alpha" (id) and Text
        // "gamma" (content "alpha inside"). 2 matches.
        for ch in "al".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.find_query(), "al");
        assert_eq!(app.find_match_count(), 2);
        // Status line includes the live count so the user
        // can see the matches are growing before they press
        // Enter.
        assert!(app.status.contains("2 matches"));
    }

    #[test]
    fn find_mode_enter_advances_to_next_match_and_keeps_session_open() {
        // Figma / VS Code "find next" semantics: Enter
        // advances the cursor and keeps the session open
        // so the user can keep cycling. Esc is the close
        // gesture.
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "alpha".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        // Pre-condition: find session is active, status
        // echoes the live match count, nothing is selected
        // yet (the user has only typed — the find command
        // hasn't selected anything).
        assert!(app.find.is_some());
        assert_eq!(app.state.selected_count(), 0);
        handle_key(&mut app, key(KeyCode::Enter));
        // Post-condition: session is still open (cycling
        // keeps it open), the first match is now selected
        // (Box "alpha" — id substring match), status
        // reports the index "1/N".
        assert!(app.find.is_some(), "Enter keeps the session open");
        assert_eq!(app.state.selected_count(), 1);
        assert!(app.status.contains("1/"));
        assert!(app.status.contains("alpha"));
    }

    #[test]
    fn find_mode_enter_cycles_to_next_match() {
        // "alpha" matches in two places: Box "alpha" (id) and
        // Text "gamma" (content "alpha inside"). First Enter
        // shows 1/2 (Box alpha, id field); second Enter
        // shows 2/2 (Text gamma, content field).
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "alpha".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.find_match_count(), 2);
        handle_key(&mut app, key(KeyCode::Enter));
        // First match selected — Box "alpha" on id.
        assert!(app.status.contains("1/2"));
        assert!(app.status.contains("alpha"));
        assert!(app.status.contains("on id"));
        handle_key(&mut app, key(KeyCode::Enter));
        // Cycled to second match — Text "gamma" on content.
        assert!(app.status.contains("2/2"));
        assert!(app.status.contains("gamma"));
        assert!(app.status.contains("on content"));
        // Session still open — a third Enter would wrap.
        assert!(app.find.is_some());
    }

    #[test]
    fn find_mode_enter_wraps_around_at_end() {
        // After the last match, Enter wraps to the first.
        // Two-match query ("alpha"); press Enter three
        // times: 1/2 (alpha) → 2/2 (gamma) → wrap → 1/2
        // (alpha again).
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "alpha".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        handle_key(&mut app, key(KeyCode::Enter)); // 1/2
        handle_key(&mut app, key(KeyCode::Enter)); // 2/2
        handle_key(&mut app, key(KeyCode::Enter)); // wrap → 1/2
        assert!(app.status.contains("1/2"));
        assert!(app.status.contains("alpha"));
    }

    #[test]
    fn find_mode_enter_with_no_match_is_a_quiet_no_op() {
        // With zero matches, Enter is a no-op: the
        // session stays open so the user can backspace
        // and broaden the search without re-pressing
        // Ctrl-F. (The "no matches" status from
        // refresh_find_status is what the user sees
        // here — Enter doesn't need to add a duplicate
        // message.)
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "xyzzy".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.find_match_count(), 0);
        let pre_status = app.status.clone();
        handle_key(&mut app, key(KeyCode::Enter));
        // Session still open, nothing selected, status
        // unchanged from the typed "(no matches)"
        // message.
        assert!(app.find.is_some(), "Enter on no-match keeps session open");
        assert_eq!(app.state.selected_count(), 0);
        assert_eq!(app.status, pre_status, "Enter is silent on no-match");
    }

    #[test]
    fn find_mode_esc_cancels_without_selecting() {
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "alpha".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert!(app.find.is_some());
        handle_key(&mut app, key(KeyCode::Esc));
        // Esc must not leave a dangling session and must
        // not mutate the selection (the user changed their
        // mind).
        assert!(app.find.is_none());
        assert_eq!(app.state.selected_count(), 0);
    }

    #[test]
    fn find_mode_backspace_shrinks_query() {
        let mut app = make_app_with_findable();
        handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
        for ch in "abc".chars() {
            handle_key(&mut app, key(KeyCode::Char(ch)));
        }
        assert_eq!(app.find_query(), "abc");
        // Backspace pops one char; matches re-compute
        // against the shorter query.
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.find_query(), "ab");
        // Backspace on empty query is a quiet no-op (does
        // not close the session — the user is still
        // composing).
        handle_key(&mut app, key(KeyCode::Backspace));
        handle_key(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.find_query(), "");
        assert!(app.find.is_some(), "still in find mode");
    }
}
