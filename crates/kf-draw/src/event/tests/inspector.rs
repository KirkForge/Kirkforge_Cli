//! Inspector-panel tests: the click handler (empty / single /
//! multi selection, Replace / Add / Toggle, body-vs-panel
//! routing) and the `I` / lowercase-`i` toggle regression
//! pins.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `app_with_inspector_panel` helper
//! moves with the tests that use it.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use kf_draw_core::{DrawObject, DrawState, InkColor};
use ratatui::layout::Rect;

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
            .push(DrawObject::Box(kf_draw_core::BoxObject {
                id: id.into(),
                z: 0,
                parent_id: None,
                color: InkColor::White,
                left: x,
                top: 0,
                right: x + 2,
                bottom: 2,
                style: kf_draw_core::BoxStyle::Light,
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

fn mouse_down(col: u16, row: u16, mods: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: mods,
    }
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
        kf_draw_core::Rect {
            left: 0,
            top: 0,
            right: 12,
            bottom: 4,
        },
        kf_draw_core::SelectionMode::Replace,
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
        kf_draw_core::Rect {
            left: 0,
            top: 0,
            right: 12,
            bottom: 4,
        },
        kf_draw_core::SelectionMode::Replace,
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
        kf_draw_core::Rect {
            left: 0,
            top: 0,
            right: 12,
            bottom: 4,
        },
        kf_draw_core::SelectionMode::Replace,
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
