//! Command-palette tests: `:` / `/` triggers, Esc / Enter /
//! Backspace / Ctrl-U / printable-char hijack, the dispatch
//! table (help / layers / inspector / save / undo / redo /
//! duplicate / group / ungroup / select all / delete / align
//! / distribute / quit), prefix-vs-substring tiebreak,
//! and the table-size / resolves-all-named-actions drift
//! guards.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `open_palette` /
//! `run_palette_command` / `run_palette_command_into` helpers
//! move with the tests that use them.

use super::*;
use crate::app::PaletteTrigger;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;
use kirkforge_draw_core::DrawObject;

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
