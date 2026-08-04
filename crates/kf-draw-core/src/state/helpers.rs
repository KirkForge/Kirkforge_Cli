//! Free helpers shared across the state submodules.
//!
//! `o_id` / `o_z` read the common id / z fields off any `DrawObject`
//! variant; `align_delta` computes the translation for an alignment
//! command; `is_degenerate` filters zero-area drafts. `MAX_UNDO` caps
//! the undo history.

use crate::types::{Align, DrawObject, Rect};

/// Cap on undo history. Older entries are dropped. Keeps memory bounded
/// for long edit sessions.
pub(super) const MAX_UNDO: usize = 100;

pub(super) fn o_id(o: &DrawObject) -> &str {
    match o {
        DrawObject::Box(b) => &b.id,
        DrawObject::Line(l) => &l.id,
        DrawObject::Elbow(e) => &e.id,
        DrawObject::Paint(p) => &p.id,
        DrawObject::Text(t) => &t.id,
    }
}

pub(super) fn o_z(o: &DrawObject) -> i32 {
    match o {
        DrawObject::Box(b) => b.z,
        DrawObject::Line(l) => l.z,
        DrawObject::Elbow(e) => e.z,
        DrawObject::Paint(p) => p.z,
        DrawObject::Text(t) => t.z,
    }
}

/// The translation `(dx, dy)` that takes an object with selection
/// bounds `r` so that the edge or center named by `how` lands on
/// the same edge or center of the union bounds `u`. The caller
/// has already filtered out the `selection is empty` and
/// `no selection bounds` cases.
pub(super) fn align_delta(r: Rect, u: Rect, how: Align) -> (i32, i32) {
    match how {
        Align::Left => (u.left - r.left, 0),
        Align::Right => (u.right - r.right, 0),
        Align::Top => (0, u.top - r.top),
        Align::Bottom => (0, u.bottom - r.bottom),
        Align::HorizontalCenter => (
            i32::midpoint(u.left, u.right) - i32::midpoint(r.left, r.right),
            0,
        ),
        Align::VerticalCenter => (
            0,
            i32::midpoint(u.top, u.bottom) - i32::midpoint(r.top, r.bottom),
        ),
    }
}

// ponytail: Paint and Text arms were defensive dead code — Paint
// drafts always have ≥1 point (begin_draft seeds one) and Text has
// no degenerate concept (empty content is valid). Only Box/Line/Elbow
// can be degenerate from commit_draft's perspective.
pub(super) fn is_degenerate(o: &DrawObject) -> bool {
    match o {
        DrawObject::Box(b) => b.left == b.right && b.top == b.bottom,
        DrawObject::Line(l) => l.x1 == l.x2 && l.y1 == l.y2,
        DrawObject::Elbow(e) => e.x1 == e.x2 && e.y1 == e.y2,
        _ => false,
    }
}
