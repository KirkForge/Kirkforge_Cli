use super::helpers::MAX_UNDO;
use super::*;
use crate::doc::new_object_id;
use crate::geometry::normalize_rect;
use crate::types::{Align, BoxObject, DistributeAxis, LineObject, SelectionMode, TextObject};

mod active;
mod error;
mod initial;

fn seeded_box_state() -> (DrawState, String) {
    let mut s = DrawState::new();
    let id = new_object_id("box");
    s.document.objects.push(DrawObject::Box(BoxObject {
        id: id.clone(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        left: 10,
        top: 10,
        right: 20,
        bottom: 20,
        style: BoxStyle::Light,
    }));
    s.selected_ids.insert(id.clone());
    (s, id)
}

fn box_bounds(s: &DrawState, id: &str) -> Option<(i32, i32, i32, i32)> {
    s.document
        .objects
        .iter()
        .find(|o| o_id(o) == id)
        .and_then(|o| match o {
            DrawObject::Box(b) => Some((b.left, b.top, b.right, b.bottom)),
            _ => None,
        })
}

fn seed_dirty_box() -> DrawState {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 3, y: 3 });
    s.commit_draft().unwrap();
    s.mark_saved();
    s
}

fn make_box_at(s: &mut DrawState, l: i32, t: i32, r: i32, b: i32) -> String {
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: l, y: t });
    s.update_draft(Point { x: r, y: b });
    s.commit_draft().unwrap()
}

/// Seed the document with three distinct, non-overlapping boxes.
/// Caller pre-selects the box it cares about.
fn seed_three_boxes() -> DrawState {
    let mut s = DrawState::new();
    make_box_at(&mut s, 0, 0, 2, 2);
    make_box_at(&mut s, 5, 0, 7, 2);
    make_box_at(&mut s, 10, 0, 12, 2);
    s
}

fn doc_ids(s: &DrawState) -> Vec<&str> {
    s.document.objects.iter().map(|o| o.id()).collect()
}

/// Seed three boxes with **distinct, explicit ids**.
fn seed_three_boxes_with_distinct_ids() -> DrawState {
    let mut s = DrawState::new();
    for (id, x) in [("box-a", 0), ("box-b", 5), ("box-c", 10)] {
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: id.into(),
            z: 0,
            parent_id: None,
            color: InkColor::White,
            left: x,
            top: 0,
            right: x + 2,
            bottom: 2,
            style: BoxStyle::Light,
        }));
    }
    s
}

fn seed_text_object(content: &str) -> (DrawState, String) {
    let mut s = DrawState::new();
    let id = new_object_id("t");
    s.document.objects.push(DrawObject::Text(TextObject {
        id: id.clone(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        x: 0,
        y: 0,
        content: content.into(),
        border: TextBorderMode::None,
    }));
    s.selected_ids.insert(id.clone());
    (s, id)
}

// Helper: seed two lines and one elbow so restyle
// tests can exercise all three LineStyle-bearing variants.
fn seed_two_lines_one_elbow() -> DrawState {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 5, y: 0 });
    s.commit_draft().unwrap();
    s.begin_draft(Point { x: 0, y: 3 });
    s.update_draft(Point { x: 5, y: 3 });
    s.commit_draft().unwrap();
    s.set_tool(DrawMode::Elbow);
    s.begin_draft(Point { x: 0, y: 6 });
    s.update_draft(Point { x: 5, y: 9 });
    s.commit_draft().unwrap();
    s.clear_selection();
    s
}

/// Helper: seed three non-overlapping boxes for marquee tests.
fn seed_marquee_boxes() -> (DrawState, Vec<String>) {
    let mut s = DrawState::new();
    make_box_at(&mut s, 0, 0, 2, 2);
    make_box_at(&mut s, 5, 0, 7, 2);
    make_box_at(&mut s, 10, 0, 12, 2);
    let ids: Vec<String> = s
        .document
        .objects
        .iter()
        .map(|o| o.id().to_string())
        .collect();
    (s, ids)
}
