//! Layers-panel tests: keyboard nav (Up / Down / Enter / Esc)
//! through the panel rows, the click handler (Replace / Add /
//! Toggle, header / below-last-row / empty-doc no-ops, focus
//! anchoring, body-vs-panel routing, claims-before-inspector),
//! and the `L` / lowercase-`l` toggle regression pins.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `app_with_three_layers_and_panel_
//! open`, `app_with_three_layer_rows`, and `mouse_down` helpers
//! move with the tests that use them.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use kirkforge_draw_core::{DrawObject, DrawState, InkColor};
use ratatui::layout::Rect;

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
