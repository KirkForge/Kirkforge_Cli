//! Layers panel data model: a topmost-first view of the document.
//!
//! The layers panel is a small UX surface — the user sees a
//! vertical list of every object, topmost at the top, and can
//! navigate / select from it. Figma, Sketch, and most editors
//! follow the same convention. This module is the pure half:
//! it walks `DrawState` once and produces a flat `Vec<LayerEntry>`
//! the bin's renderer formats.
//!
//! ponytail: returns a `Vec`, not a custom iterator. Layers are
//! a small fixed cap per document (a few hundred objects is
//! already an unusually large diagram) so the allocation is
//! negligible, and the bin wants random access (index-based
//! click-to-select). If we ever ship a 10k-object document
//! we'll revisit — but until then, the simpler API wins.

use crate::doc::ObjectKind;
use crate::state::DrawState;

/// One row of the layers panel. Stable ordering across calls —
/// topmost object first, bottommost last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEntry {
    /// Object id. The bin's click-to-select handler maps this
    /// back through `DrawState::select_id`.
    pub id: String,
    /// Discriminator (Box / Line / Elbow / Paint / Text). Cheap
    /// to copy; used by the panel's icon column.
    pub kind: ObjectKind,
    /// Z-order value from the underlying `DrawObject`. Higher
    /// renders on top. Kept on the row so the panel can sort /
    /// show it without re-querying the document.
    pub z: i32,
    /// True iff this id is in the selection set. The panel uses
    /// it to draw the highlight column.
    pub selected: bool,
}

/// Topmost-first list of every object in the document, with a
/// `selected` flag set for each id in `state.selected_ids()`.
///
/// Ordering matches the renderer's z-order: tail of
/// `document.objects` is topmost (see `compose_scene`), so the
/// panel reverses the vec before producing entries.
///
/// ponytail: `selected_ids` is private to `DrawState`. We pull
/// a snapshot via `state.selected_count()` + `state.selected()`
/// rather than asking for the raw set, mirroring how the
/// inspector and bin layer avoid breaking encapsulation.
pub fn layer_list(state: &DrawState) -> Vec<LayerEntry> {
    // Build the selected set in one pass. `state.selected()`
    // returns refs to the selected objects; collecting ids
    // here lets the loop below stay O(n) over the full
    // document vec.
    let selected_ids: std::collections::HashSet<&str> =
        state.selected().into_iter().map(|o| o.id()).collect();

    // document.objects is in render order: head = bottommost,
    // tail = topmost. The panel wants the reverse, so we walk
    // the vec from the end.
    state
        .document
        .objects
        .iter()
        .rev()
        .map(|o| LayerEntry {
            id: o.id().to_string(),
            kind: ObjectKind::of(o),
            z: o.z(),
            selected: selected_ids.contains(o.id()),
        })
        .collect()
}

/// Cheap pretty name for the kind. The bin's renderer would
/// call this on every row every frame, so we expose it as a
/// free function with no allocation.
///
/// ponytail: returns `&'static str` so the caller doesn't have
/// to clone. Adding a new `ObjectKind` variant requires adding
/// an arm here AND a test asserting the variant is covered.
pub fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Box => "Box",
        ObjectKind::Line => "Line",
        ObjectKind::Elbow => "Elbow",
        ObjectKind::Paint => "Paint",
        ObjectKind::Text => "Text",
    }
}

/// Map an object id to its row index in the panel (0 = topmost,
/// i.e. the last object in `document.objects`). Returns `None`
/// when the id is not in the document.
///
/// ponytail: linear walk, no side index. The panel rebuilds its
/// row vec every frame anyway (the render path calls
/// `layer_list` once per draw), so a dedicated map would just
/// duplicate work. The cost is O(n) per click — a non-issue
/// for the typical "few hundred objects" document.
pub fn layer_row_for_id(state: &DrawState, id: &str) -> Option<usize> {
    // document.objects[0] is the bottommost (row n-1 in the
    // panel); document.objects[n-1] is the topmost (row 0).
    // Walk in reverse and return the first match index.
    let n = state.document.objects.len();
    for (rev_idx, obj) in state.document.objects.iter().rev().enumerate() {
        if obj.id() == id {
            return Some(rev_idx);
        }
    }
    let _ = n;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        BoxObject, DrawMode, DrawObject, ElbowObject, ElbowOrientation, InkColor, LineObject,
        LineStyle, PaintObject, Point, TextBorderMode, TextObject,
    };

    fn empty() -> DrawState {
        DrawState::new()
    }

    fn three_layers() -> DrawState {
        // Build a document with three objects in the order they
        // appear in `document.objects` (head = bottommost):
        //   [0] Box  (bottom)
        //   [1] Line (middle)
        //   [2] Text (top)
        let mut s = DrawState::new();
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "box".into(),
            z: 0,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: crate::types::BoxStyle::Light,
        }));
        s.document.objects.push(DrawObject::Line(LineObject {
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
        s.document.objects.push(DrawObject::Text(TextObject {
            id: "text".into(),
            z: 2,
            parent_id: None,
            color: InkColor::Yellow,
            x: 0,
            y: 0,
            content: "top".into(),
            border: TextBorderMode::None,
        }));
        s
    }

    #[test]
    fn empty_document_yields_empty_layers() {
        let s = empty();
        assert!(layer_list(&s).is_empty());
    }

    #[test]
    fn layers_are_returned_topmost_first() {
        // The order in `three_layers` is [box, line, text] (head
        // = bottommost). The panel wants topmost first, so the
        // expected order is [text, line, box].
        let s = three_layers();
        let layers = layer_list(&s);
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].id, "text");
        assert_eq!(layers[1].id, "line");
        assert_eq!(layers[2].id, "box");
    }

    #[test]
    fn each_layer_carries_kind_z_and_label() {
        let s = three_layers();
        let layers = layer_list(&s);
        // Topmost first; verify kind + z + label for each row.
        assert_eq!(layers[0].kind, ObjectKind::Text);
        assert_eq!(layers[0].z, 2);
        assert_eq!(kind_label(layers[0].kind), "Text");
        assert_eq!(layers[1].kind, ObjectKind::Line);
        assert_eq!(layers[1].z, 1);
        assert_eq!(kind_label(layers[1].kind), "Line");
        assert_eq!(layers[2].kind, ObjectKind::Box);
        assert_eq!(layers[2].z, 0);
        assert_eq!(kind_label(layers[2].kind), "Box");
    }

    #[test]
    fn selection_flag_matches_state() {
        let mut s = three_layers();
        // Set tool to Select and pick one object by id.
        s.set_tool(DrawMode::Select);
        s.select_id("line");
        let layers = layer_list(&s);
        // Find the line row; selection is independent of order.
        let line_row = layers.iter().find(|r| r.id == "line").unwrap();
        assert!(line_row.selected);
        // Others must not be selected.
        for row in &layers {
            if row.id != "line" {
                assert!(!row.selected);
            }
        }
    }

    #[test]
    fn multi_selection_marks_every_picked_row() {
        // Two-object document so the Add-rect can target both
        // without picking up a third.
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Select);
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "box".into(),
            z: 0,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: crate::types::BoxStyle::Light,
        }));
        s.document.objects.push(DrawObject::Line(LineObject {
            id: "line".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Green,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 0,
            style: LineStyle::Smooth,
        }));
        // Seed one id, then Add the second via a rect that
        // covers both objects' bounds.
        s.select_id("box");
        s.select_in_rect(
            crate::types::Rect {
                left: 0,
                top: 0,
                right: 4,
                bottom: 3,
            },
            crate::types::SelectionMode::Add,
        );
        let layers = layer_list(&s);
        let selected_count = layers.iter().filter(|r| r.selected).count();
        assert_eq!(selected_count, 2);
        assert!(layers.iter().find(|r| r.id == "box").unwrap().selected);
        assert!(layers.iter().find(|r| r.id == "line").unwrap().selected);
    }

    #[test]
    fn empty_selection_marks_nothing() {
        let s = three_layers();
        let layers = layer_list(&s);
        assert!(layers.iter().all(|r| !r.selected));
    }

    #[test]
    fn kind_label_covers_every_variant() {
        // Lock the label table so a new ObjectKind variant
        // can't sneak in without an explicit update here.
        let all = [
            (ObjectKind::Box, "Box"),
            (ObjectKind::Line, "Line"),
            (ObjectKind::Elbow, "Elbow"),
            (ObjectKind::Paint, "Paint"),
            (ObjectKind::Text, "Text"),
        ];
        for (k, expected) in all {
            assert_eq!(kind_label(k), expected, "kind_label({k:?})");
        }
    }

    #[test]
    fn kind_label_table_is_stable() {
        // Snap the entire kind_label table so a refactor that
        // accidentally renames a label (e.g., 'Text' → 'T') is
        // caught. Lowercase labels would also be caught.
        let labels: Vec<&'static str> = [
            ObjectKind::Box,
            ObjectKind::Line,
            ObjectKind::Elbow,
            ObjectKind::Paint,
            ObjectKind::Text,
        ]
        .iter()
        .map(|k| kind_label(*k))
        .collect();
        assert_eq!(
            labels,
            vec!["Box", "Line", "Elbow", "Paint", "Text"],
            "kind_label output changed — update intentionally"
        );
    }

    #[test]
    fn elbow_and_paint_appear_with_correct_kinds() {
        // Add an elbow + a paint to the three_layers document
        // and confirm they sort to the top in their actual
        // document order.
        let mut s = three_layers();
        s.document.objects.push(DrawObject::Elbow(ElbowObject {
            id: "elbow".into(),
            z: 3,
            parent_id: None,
            color: InkColor::Cyan,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 4,
            style: LineStyle::Light,
            orientation: ElbowOrientation::HorizontalFirst,
        }));
        s.document.objects.push(DrawObject::Paint(PaintObject {
            id: "paint".into(),
            z: 4,
            parent_id: None,
            color: InkColor::Magenta,
            points: vec![Point { x: 0, y: 0 }, Point { x: 1, y: 0 }],
            brush: "round".into(),
        }));
        let layers = layer_list(&s);
        // document order: [box, line, text, elbow, paint]
        // topmost first:        [paint, elbow, text, line, box]
        let ids: Vec<&str> = layers.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["paint", "elbow", "text", "line", "box"]);
        // Kind columns for the new rows.
        assert_eq!(layers[0].kind, ObjectKind::Paint);
        assert_eq!(layers[1].kind, ObjectKind::Elbow);
    }

    #[test]
    fn layer_entry_is_cloneable_for_panel_state() {
        // The bin may want to keep the previous frame's layer
        // list around for diffing / animation; Clone keeps that
        // future open without re-querying.
        let s = three_layers();
        let layers = layer_list(&s);
        let _copy = layers.clone();
        let row = layers[0].clone();
        let _row2 = row.clone();
    }

    #[test]
    fn layer_row_for_id_returns_topmost_index() {
        // Same setup as `three_layers`: document order is
        // [box, line, text] (head = bottommost). The panel is
        // topmost-first, so:
        //   row 0 → "text" (topmost)
        //   row 1 → "line"
        //   row 2 → "box"  (bottommost)
        let s = three_layers();
        assert_eq!(layer_row_for_id(&s, "text"), Some(0));
        assert_eq!(layer_row_for_id(&s, "line"), Some(1));
        assert_eq!(layer_row_for_id(&s, "box"), Some(2));
    }

    #[test]
    fn layer_row_for_id_returns_none_for_missing_id() {
        let s = three_layers();
        assert_eq!(layer_row_for_id(&s, "no-such"), None);
    }

    #[test]
    fn layer_row_for_id_returns_none_on_empty_doc() {
        let s = empty();
        assert_eq!(layer_row_for_id(&s, "anything"), None);
    }

    #[test]
    fn layer_row_for_id_matches_layer_list_index() {
        // Cross-check: the row index from `layer_row_for_id` must
        // match the position of the same id in the Vec returned
        // by `layer_list`. Both functions describe the same
        // topmost-first ordering; if they ever diverge, the
        // click-to-select flow breaks silently.
        let s = three_layers();
        let layers = layer_list(&s);
        for (idx, layer) in layers.iter().enumerate() {
            assert_eq!(
                layer_row_for_id(&s, &layer.id),
                Some(idx),
                "id {} should map to row {} in layer_list",
                layer.id,
                idx
            );
        }
    }
}
