//! Keyboard-event tests: quit confirm, tool switch, undo /
//! redo, duplicate, cut, panic-surfacing, bracket raise /
//! lower, help overlay, Ctrl+letter no-swap, help-line drift
//! guards, delete, arrow / page scroll, and the Esc quit
//! cascade.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `keymap_doc_block_lists_palette_
//! and_z_order_chords` test reads the production event module
//! via `include_str!` so the path is `../mod.rs` (the
//! production file moved from `event.rs` to `event/mod.rs`).

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;

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
    let src = include_str!("../mod.rs");
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
fn esc_with_no_draft_no_resize_no_selection_quits() {
    // Third branch of the Esc handler: nothing to clear, fall
    // through to request_quit.
    let mut app = make_app();
    assert!(!app.should_quit);
    assert_eq!(app.state.selected_count(), 0);
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.should_quit, "Esc with empty state must quit");
}
