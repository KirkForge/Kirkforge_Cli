//! Shared test helpers used across every event-category test
//! sub-module. Each category file starts with
//! `use super::*;` (production items via the parent `tests`
//! mod's `use super::*`) and `use crate::event::tests::common::*;`
//! (the helpers below).
//!
//! Kept in one place so a fixture tweak lands once and every
//! category file picks it up — matches the prior single-block
//! layout where `make_app` / `key` / `key_ctrl` were defined
//! at the top of `mod tests` and visible to every test below.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kirkforge_draw_core::{DrawMode, DrawState, Point};
use ratatui::layout::Rect;

use crate::app::App;

pub(super) fn make_app() -> App {
    let mut app = App::new(DrawState::new());
    app.body_area = Rect::new(0, 3, 80, 20);
    app.scene_origin = Some(Point { x: 0, y: 0 });
    app
}

pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

pub(super) fn key_with_shift(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::SHIFT)
}

pub(super) fn key_ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

pub(super) fn key_with_shift_ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

pub(super) fn key_ctrl_alt(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::ALT)
}

pub(super) fn key_ctrl_shift(c: char) -> KeyEvent {
    KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )
}

/// Helper: seed three 2x2 boxes the bin tests can marquee /
/// align / distribute / invert over. Returns (app, doc-ids) so
/// each test can assert against the selected ids. Shared by the
/// align, distribute, invert, marquee, and palette test
/// sub-modules — kept here so a fixture tweak lands once.
pub(super) fn make_app_with_three_boxes() -> (App, Vec<String>) {
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
