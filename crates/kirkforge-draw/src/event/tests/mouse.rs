//! Mouse-event tests: marquee select (bare / Shift / Ctrl /
//! both-mods), single-click fallback to `select_at`, drag-
//! draft commit, resize-handle grab / drag / commit / Esc-
//! cancel, click-outside-pane no-op, and the Shift+Arrow /
//! Ctrl+Shift+Arrow keyboard nudge (the nudge lives here
//! because it operates on the selection the mouse tests
//! set up).
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `mouse_click` / `mouse_marquee`
//! helpers move with the tests that use them;
//! `make_app_with_three_boxes` is shared via `common`.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

// -- Marquee select (mouse) -------------------------------------

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
