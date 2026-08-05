use super::MAX_UNDO;
use super::*;

#[test]
fn new_state_has_empty_document() {
    let s = DrawState::new();
    assert!(s.document.objects.is_empty());
    assert_eq!(s.tool, DrawMode::Select);
}

#[test]
fn with_document_keeps_existing_objects() {
    let doc = DrawDocument {
        version: 1,
        objects: vec![DrawObject::Line(LineObject {
            id: "l1".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x1: 0,
            y1: 0,
            x2: 3,
            y2: 0,
            style: LineStyle::Light,
        })],
    };
    let s = DrawState::with_document(doc.clone());
    assert_eq!(s.document, doc);
}

#[test]
fn set_tool_cancels_draft() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    assert!(s.has_draft());
    s.set_tool(DrawMode::Line);
    assert!(!s.has_draft());
}

#[test]
fn cycle_tool_forward_walks_then_wraps() {
    let mut s = DrawState::new();
    assert_eq!(s.tool, DrawMode::Select);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Box);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Line);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Elbow);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Paint);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Text);
    s.cycle_tool(true);
    assert_eq!(s.tool, DrawMode::Select, "should wrap back to Select");
}

#[test]
fn cycle_tool_backward_walks_then_wraps() {
    let mut s = DrawState::new();
    // From Select, Shift+Tab lands on Text (last in the order).
    s.cycle_tool(false);
    assert_eq!(s.tool, DrawMode::Text);
    s.cycle_tool(false);
    assert_eq!(s.tool, DrawMode::Paint);
    s.cycle_tool(false);
    assert_eq!(s.tool, DrawMode::Elbow);
}

#[test]
fn cycle_tool_cancels_active_draft() {
    // Mirrors set_tool behavior — cycling should also drop a draft.
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    assert!(s.has_draft());
    s.cycle_tool(true);
    assert!(!s.has_draft());
}

#[test]
fn begin_and_commit_draft_pushes_object() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Line);
    s.set_line_style(LineStyle::Light);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 5, y: 3 });
    let id = s.commit_draft().unwrap();
    assert_eq!(s.document.objects.len(), 1);
    // The new object should be auto-selected.
    assert!(s.selected_ids.contains(&id));
}

#[test]
fn cancel_draft_leaves_document_unchanged() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 5, y: 5 });
    s.cancel_draft();
    assert!(s.document.objects.is_empty());
}

#[test]
fn paint_draft_accumulates_points() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Paint);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 2, y: 0 });
    s.update_draft(Point { x: 4, y: 1 });
    let draft = s.draft().unwrap();
    if let DrawObject::Paint(p) = draft {
        assert!(p.points.len() >= 3);
    } else {
        panic!("expected paint draft");
    }
}

#[test]
fn undo_and_redo_restore_document() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 3, y: 0 });
    s.commit_draft().unwrap();
    assert_eq!(s.document.objects.len(), 1);
    assert!(s.undo());
    assert!(s.document.objects.is_empty());
    assert!(s.redo());
    assert_eq!(s.document.objects.len(), 1);
}

#[test]
fn undo_history_is_bounded() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Line);
    for i in 0..(MAX_UNDO + 5) {
        s.begin_draft(Point { x: i as i32, y: 0 });
        s.update_draft(Point {
            x: i as i32 + 1,
            y: 0,
        });
        s.commit_draft().unwrap();
    }
    // We can only undo MAX_UNDO times.
    let mut count = 0;
    while s.undo() {
        count += 1;
    }
    assert_eq!(count, MAX_UNDO);
}

#[test]
fn dirty_starts_clean() {
    let s = DrawState::new();
    assert!(!s.is_dirty());
}

#[test]
fn commit_draft_marks_dirty() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 3, y: 3 });
    assert!(!s.is_dirty(), "draft only doesn't dirty");
    s.commit_draft().unwrap();
    assert!(s.is_dirty());
}

#[test]
fn mark_saved_clears_dirty() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 2, y: 2 });
    s.commit_draft().unwrap();
    assert!(s.is_dirty(), "commit should leave the doc dirty");
    s.mark_saved();
    assert!(!s.is_dirty());
    // Mutating again re-flags the document.
    s.commit_resize(); // no-op, no flag
    s.begin_resize(BoxResizeHandle::BottomRight);
    s.update_resize(Point { x: 5, y: 5 });
    s.commit_resize();
    assert!(s.is_dirty());
}

#[test]
fn can_undo_and_can_redo_track_stacks() {
    let mut s = DrawState::new();
    assert!(!s.can_undo());
    assert!(!s.can_redo());

    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 2, y: 2 });
    s.commit_draft().unwrap();
    assert!(s.can_undo(), "commit must populate undo_stack");
    assert!(!s.can_redo());

    assert!(s.undo());
    assert!(s.can_redo(), "undo must populate redo_stack");

    assert!(s.redo());
    assert!(!s.can_redo(), "redo must drain redo_stack");
    assert!(s.can_undo());
}

#[test]
fn snapshot_pushes_undo_without_mutating_document() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 2, y: 2 });
    s.commit_draft().unwrap();
    let pre = s.document.clone();
    s.snapshot();
    // Document untouched; only the undo stack grew.
    assert_eq!(s.document, pre);
    assert!(s.can_undo());
}

#[test]
fn ink_setters_overwrite_in_place() {
    let mut s = DrawState::new();
    s.set_color(InkColor::Red);
    s.set_line_style(LineStyle::Light);
    s.set_box_style(BoxStyle::Double);
    s.set_brush("·");
    s.set_text_border(TextBorderMode::Underline);
    assert_eq!(s.color, InkColor::Red);
    assert_eq!(s.line_style, LineStyle::Light);
    assert_eq!(s.box_style, BoxStyle::Double);
    assert_eq!(s.brush, "·");
    assert_eq!(s.text_border, TextBorderMode::Underline);
    // Round-trip again with a different value to confirm they
    // overwrite (not just first-write-wins).
    s.set_color(InkColor::Blue);
    assert_eq!(s.color, InkColor::Blue);
}

#[test]
fn document_bounds_encloses_every_object() {
    let mut s = DrawState::new();
    s.document.objects.push(DrawObject::Box(BoxObject {
        id: "b".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        left: -1,
        top: -2,
        right: 4,
        bottom: 5,
        style: BoxStyle::Light,
    }));
    let r = s.document_bounds().unwrap();
    assert_eq!(
        r,
        normalize_rect(Point { x: -1, y: -2 }, Point { x: 4, y: 5 })
    );
}

#[test]
fn all_objects_includes_draft() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 3, y: 3 });
    let all = s.all_objects();
    assert_eq!(all.len(), 1);
}

#[test]
fn selection_bounds_returns_union_of_selected() {
    let (mut s, _id) = seeded_box_state();
    // The seeded box is at (10,10)-(20,20) and pre-selected.
    let r = s.selection_bounds().expect("selection has bounds");
    assert_eq!((r.left, r.top, r.right, r.bottom), (10, 10, 20, 20));

    // Empty selection → None.
    s.clear_selection();
    assert!(s.selection_bounds().is_none());
}

#[test]
fn reconcile_selection_drops_stale_ids() {
    let mut s = DrawState::new();
    s.selected_ids.insert("ghost".into());
    s.document.objects.push(DrawObject::Line(LineObject {
        id: "live".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        x1: 0,
        y1: 0,
        x2: 3,
        y2: 0,
        style: LineStyle::Light,
    }));
    s.reconcile_selection();
    assert!(!s.selected_ids.contains("ghost"));
}
