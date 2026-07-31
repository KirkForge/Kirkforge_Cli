//! Align / distribute / invert / select-all tests:
//! Ctrl-Shift-<dir> align edges + centers, Ctrl-Shift-J / K
//! distribute, Ctrl-Shift-I invert selection, Ctrl-A select
//! every object.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `key_ctrl_a` helper moves with
//! the Ctrl-A tests that use it; `make_app_with_three_boxes`
//! and `key_ctrl_shift` are shared via `common`.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

// -- Multi-object alignment (Ctrl-Shift-<dir>) ------------------

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
