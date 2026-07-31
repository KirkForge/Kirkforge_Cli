//! Grouping tests: Ctrl-G groups the selection under a new
//! parent id, Ctrl-Shift-G ungroups. Pins the routing + the
//! status messages so a future refactor of `handle_key` can't
//! silently drop Ctrl-G.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `make_app_with_two_boxes` helper
//! moves with the tests that use it.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;
use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};

// -- Grouping (Ctrl-G / Ctrl-Shift-G) ------------------------
//
// Bin layer just routes the chord to the core helpers. The
// helper-level contract is locked in the core test suite; here
// we lock the routing + the status messages so a future
// refactor of handle_key can't silently drop Ctrl-G.

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
