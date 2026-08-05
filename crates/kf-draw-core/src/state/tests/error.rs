use super::*;

#[test]
fn begin_draft_in_select_is_noop() {
    // Bug #4 regression: begin_draft is public, so a misuse from
    // a future caller could create a "sel"-prefixed Box draft.
    // Production no-ops; debug_asserts in dev.
    let mut s = DrawState::new();
    assert_eq!(s.tool, DrawMode::Select);
    s.begin_draft(Point { x: 0, y: 0 });
    assert!(
        !s.has_draft(),
        "begin_draft in Select must not create a draft"
    );
    assert!(s.draft().is_none());
    assert!(s.document.objects.is_empty());
}

#[test]
fn commit_drops_degenerate_box() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 2, y: 2 });
    // No update — anchor == pointer → zero-area box.
    let id = s.commit_draft();
    assert!(id.is_none());
    assert!(s.document.objects.is_empty());
}

#[test]
fn select_at_misses_when_nothing_hits() {
    let mut s = DrawState::new();
    s.document.objects.push(DrawObject::Box(BoxObject {
        id: "b1".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 2,
        style: BoxStyle::Light,
    }));
    let picked = s.select_at(Point { x: 10, y: 10 });
    assert!(picked.is_none());
    assert!(s.selected_ids.is_empty());
}

#[test]
fn select_at_add_and_toggle_on_miss_preserve_selection() {
    // Click on empty space with Shift / Ctrl must NOT clear
    // the selection (mirrors select_in_rect's no-op-on-miss
    // for those modes, but spelled out for the single-click
    // path). Replace mode DOES clear, which the existing
    // `select_at_misses_when_nothing_hits` test pins.
    let mut s = DrawState::new();
    s.document.objects.push(DrawObject::Box(BoxObject {
        id: "b1".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 5,
        bottom: 5,
        style: BoxStyle::Light,
    }));
    s.selected_ids.insert("b1".into());
    let _ = s.select_at_with_mode(Point { x: 50, y: 50 }, SelectionMode::Add);
    assert!(
        s.selected_ids.contains("b1"),
        "Add+miss preserves selection"
    );
    let _ = s.select_at_with_mode(Point { x: 50, y: 50 }, SelectionMode::Toggle);
    assert!(
        s.selected_ids.contains("b1"),
        "Toggle+miss preserves selection"
    );
    // And Replace (the default) still clears — the
    // pre-existing test pins this; re-asserting here
    // documents the boundary.
    let _ = s.select_at_with_mode(Point { x: 50, y: 50 }, SelectionMode::Replace);
    assert!(s.selected_ids.is_empty(), "Replace+miss clears (legacy)");
}

#[test]
fn delete_selected_with_empty_selection_returns_zero() {
    // Empty selection must not flip dirty and must report zero so
    // the bin's status echo can match the "nothing to delete"
    // shape other editor commands use.
    let mut s = DrawState::new();
    s.mark_saved();
    assert!(!s.is_dirty());
    assert_eq!(s.delete_selected(), 0);
    assert!(!s.is_dirty(), "empty delete must not flip dirty");
}

#[test]
fn delete_selected_returns_count_of_removed_objects() {
    // Two real objects, both selected, plus a stale id the
    // user added manually. `delete_selected` returns the
    // count of `selected_ids` (the user's intent: "I
    // picked 3 things"), not the count of actually-removed
    // document rows (the retain loop is a no-op on the
    // stale id). The bin's status echo counts the
    // intent — matches how every other editor command
    // (group, ungroup, distribute) reports the selection
    // size, not the post-condition. Wipe the
    // post-commit selection first so the count is exact.
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 2, y: 2 });
    let id_a = s.commit_draft().unwrap();
    s.begin_draft(Point { x: 5, y: 0 });
    s.update_draft(Point { x: 7, y: 2 });
    let id_b = s.commit_draft().unwrap();
    s.set_tool(DrawMode::Select);
    s.clear_selection();
    s.selected_ids.insert(id_a);
    s.selected_ids.insert(id_b);
    s.selected_ids.insert("stale-id".to_string());
    assert_eq!(s.document.objects.len(), 2);
    assert_eq!(s.delete_selected(), 3);
    assert!(s.document.objects.is_empty());
}

#[test]
fn duplicate_selected_is_noop_with_empty_selection() {
    let mut s = DrawState::new();
    assert!(s.selected_ids.is_empty());
    let new_ids = s.duplicate_selected();
    assert!(new_ids.is_empty());
    assert!(s.document.objects.is_empty());
}

#[test]
fn duplicate_selected_cancels_when_draft_in_progress() {
    let mut s = DrawState::new();
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 3, y: 3 });
    s.commit_draft().unwrap();
    // Begin a new draft.
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 5, y: 5 });
    let ids = s.duplicate_selected();
    assert!(
        ids.is_empty(),
        "duplicate must not run while a draft is in flight"
    );
}

#[test]
fn serialize_selected_to_json_with_empty_selection_is_empty_array() {
    let s = DrawState::new();
    let json = s.serialize_selected_to_json();
    assert_eq!(json, "[]");
}

#[test]
fn paste_objects_from_json_with_invalid_payload_is_noop() {
    let mut s = DrawState::new();
    let before = s.document.objects.len();
    let ids = s.paste_objects_from_json("not json at all");
    assert!(ids.is_empty());
    assert_eq!(s.document.objects.len(), before);
    // A JSON object (not an array of objects) should also be a
    // silent no-op — pasting arbitrary shapes must not panic.
    let ids = s.paste_objects_from_json(r#"{"version":1,"objects":[]}"#);
    assert!(ids.is_empty());
    assert_eq!(s.document.objects.len(), before);
}

#[test]
fn paste_objects_from_json_is_noop_with_draft_in_progress() {
    let (mut s, _id) = seeded_box_state();
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 0, y: 0 });
    let json = s.serialize_selected_to_json();
    let ids = s.paste_objects_from_json(&json);
    assert!(
        ids.is_empty(),
        "paste must not run while a draft is in flight"
    );
}

#[test]
fn cut_selected_to_json_with_empty_selection_is_empty_array() {
    let mut s = DrawState::new();
    let json = s.cut_selected_to_json();
    assert_eq!(json, "[]");
    // No mutation when nothing is selected.
    assert!(s.document.objects.is_empty());
    assert!(!s.is_dirty());
}

#[test]
fn cut_selected_to_json_is_noop_with_draft_in_progress() {
    let (mut s, _id) = seeded_box_state();
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 0, y: 0 });
    let obj_count_before = s.document.objects.len();
    let json = s.cut_selected_to_json();
    assert_eq!(json, "[]");
    assert_eq!(
        s.document.objects.len(),
        obj_count_before,
        "cut must not mutate the doc while a draft is in flight"
    );
}

#[test]
fn cut_selected_to_json_marks_dirty() {
    let mut s = seed_dirty_box();
    // seed_dirty_box leaves the doc dirty=False after mark_saved.
    s.mark_saved();
    assert!(!s.is_dirty());
    let _json = s.cut_selected_to_json();
    assert!(s.is_dirty());
}

#[test]
fn replace_text_content_with_same_value_is_noop() {
    let (mut s, id) = seed_text_object("hello");
    assert!(!s.can_undo());
    assert!(s.replace_text_content(&id, "hello"));
    // Same content must NOT push an undo step (commit-on-empty
    // edits shouldn't churn the undo stack).
    assert!(!s.can_undo());
}

#[test]
fn replace_text_content_missing_id_returns_false() {
    let mut s = DrawState::new();
    assert!(!s.replace_text_content("ghost", "anything"));
}

#[test]
fn replace_text_content_on_non_text_returns_false() {
    // Seed a Box (not a Text), try to edit it by id — should
    // return false so the edit-mode UI can drop the buffer.
    let (mut s, id) = seeded_box_state();
    assert!(!s.replace_text_content(&id, "anything"));
}

#[test]
fn write_text_content_returns_true_when_unchanged() {
    // Ponytail: identical content reports true (the value
    // matches the live state) but doesn't churn anything —
    // the no-op is observable only as "no mutation happened".
    let (mut s, id) = seed_text_object("same");
    assert!(s.write_text_content(&id, "same"));
    assert_eq!(s.text_content(&id).as_deref(), Some("same"));
}

#[test]
fn write_text_content_returns_false_for_unknown_id() {
    let (mut s, _id) = seed_text_object("");
    assert!(!s.write_text_content("does-not-exist", "anything"));
}

#[test]
fn write_text_content_on_non_text_returns_false() {
    let (mut s, id) = seeded_box_state();
    assert!(!s.write_text_content(&id, "anything"));
}

#[test]
fn write_text_content_does_not_push_undo_step() {
    // F2-edit write-through must not grow the undo stack on
    // every keystroke — otherwise one edit session would
    // produce dozens of undo steps and Ctrl-Z would only
    // roll back one char at a time.
    let (mut s, id) = seed_text_object("");
    let before = s.undo_stack.len();
    s.write_text_content(&id, "a");
    s.write_text_content(&id, "ab");
    s.write_text_content(&id, "abc");
    assert_eq!(s.undo_stack.len(), before, "no undo steps while editing");
}

#[test]
fn write_text_content_does_not_flip_dirty() {
    // The document dirty flag is anchored to commit. While
    // the buffer is mid-edit the document is in flight, not
    // modified. The commit path is the only thing that
    // flips the marker.
    let (mut s, id) = seed_text_object("");
    assert!(!s.is_dirty(), "seed state is clean");
    s.write_text_content(&id, "abc");
    assert!(!s.is_dirty(), "write-through keeps dirty false");
    s.write_text_content(&id, "abcd");
    assert!(!s.is_dirty(), "subsequent writes still keep it clean");
}

#[test]
fn commit_text_content_no_op_when_buffer_equals_initial() {
    // User opened F2, didn't type, hit Enter — should be
    // a clean no-op. No undo, no dirty.
    let (mut s, id) = seed_text_object("hello");
    let undo_before = s.undo_stack.len();
    assert!(s.commit_text_content(&id, "hello", "hello"));
    assert_eq!(s.text_content(&id).as_deref(), Some("hello"));
    assert!(!s.is_dirty(), "no-op commit keeps dirty clean");
    assert_eq!(
        s.undo_stack.len(),
        undo_before,
        "no-op commit pushes no undo step"
    );
}

#[test]
fn commit_text_content_returns_false_for_unknown_id() {
    let (mut s, _id) = seed_text_object("");
    assert!(!s.commit_text_content("does-not-exist", "anything", ""));
}

#[test]
fn commit_text_content_on_non_text_returns_false() {
    let (mut s, id) = seeded_box_state();
    assert!(!s.commit_text_content(&id, "anything", ""));
}

#[test]
fn revert_text_content_no_op_when_current_equals_initial() {
    let (mut s, id) = seed_text_object("same");
    // User opened F2, didn't type, Esc — content already
    // matches initial, revert is a no-op.
    assert!(s.revert_text_content(&id, "same"));
    assert_eq!(s.text_content(&id).as_deref(), Some("same"));
}

#[test]
fn revert_text_content_returns_false_for_unknown_id() {
    let (mut s, _id) = seed_text_object("");
    assert!(!s.revert_text_content("does-not-exist", ""));
}

#[test]
fn revert_text_content_on_non_text_returns_false() {
    let (mut s, id) = seeded_box_state();
    assert!(!s.revert_text_content(&id, ""));
}

#[test]
fn commit_resize_drops_box_when_drag_collapses_to_point() {
    // 1×1 box at (5,5)-(6,6). Dragging the TopLeft handle
    // exactly onto the BottomRight corner collapses the bounds
    // to (6,6)-(6,6) — a zero-area point. `commit_resize`
    // mirrors `commit_draft`'s is_degenerate filter and drops
    // the box; a single undo (snapshot pushed at begin_resize)
    // restores it.
    let mut s = DrawState::new();
    let id = make_box_at(&mut s, 5, 5, 6, 6);
    s.mark_saved();
    assert!(s.begin_resize(BoxResizeHandle::TopLeft));
    s.update_resize(Point { x: 6, y: 6 });
    // During the drag the in-place mutation shows the box
    // collapsed — only commit removes it.
    assert_eq!(s.document.objects.len(), 1);
    s.commit_resize();
    assert!(
        s.document.objects.is_empty(),
        "degenerate box should be dropped at commit_resize"
    );
    assert!(!s.selected_ids.contains(&id));
    s.undo();
    assert_eq!(
        s.document.objects.len(),
        1,
        "single undo restores the pre-drag box"
    );
}

#[test]
fn bring_to_front_is_noop_when_already_last() {
    let mut s = seed_three_boxes();
    let id_a = s.document.objects[0].id().to_string();
    let id_b = s.document.objects[1].id().to_string();
    let id_c = s.document.objects[2].id().to_string();
    // Select C (the last one).
    s.clear_selection();
    s.select_at(Point { x: 11, y: 1 });
    s.mark_saved();

    assert!(
        !s.bring_to_front(),
        "object already at top should not push undo"
    );
    assert_eq!(
        doc_ids(&s),
        vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
    );
    assert!(!s.is_dirty(), "no-op must not flip dirty");
}

#[test]
fn send_to_back_is_noop_when_already_first() {
    let mut s = seed_three_boxes();
    let id_a = s.document.objects[0].id().to_string();
    let id_b = s.document.objects[1].id().to_string();
    let id_c = s.document.objects[2].id().to_string();
    s.clear_selection();
    s.select_at(Point { x: 1, y: 1 });
    s.mark_saved();

    assert!(!s.send_to_back());
    assert_eq!(
        doc_ids(&s),
        vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
    );
    assert!(!s.is_dirty());
}

#[test]
fn bring_forward_is_noop_when_already_last() {
    let mut s = seed_three_boxes();
    let id_a = s.document.objects[0].id().to_string();
    let id_b = s.document.objects[1].id().to_string();
    let id_c = s.document.objects[2].id().to_string();
    s.clear_selection();
    s.select_at(Point { x: 11, y: 1 });
    s.mark_saved();
    assert!(!s.bring_forward());
    assert_eq!(
        doc_ids(&s),
        vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
    );
    assert!(!s.is_dirty());
}

#[test]
fn send_backward_is_noop_when_already_first() {
    let mut s = seed_three_boxes();
    let id_a = s.document.objects[0].id().to_string();
    let id_b = s.document.objects[1].id().to_string();
    let id_c = s.document.objects[2].id().to_string();
    s.clear_selection();
    s.select_at(Point { x: 1, y: 1 });
    s.mark_saved();
    assert!(!s.send_backward());
    assert_eq!(
        doc_ids(&s),
        vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
    );
    assert!(!s.is_dirty());
}

#[test]
fn z_order_noop_with_empty_selection_is_false() {
    let mut s = DrawState::new();
    assert!(!s.send_to_back());
    assert!(!s.bring_to_front());
    assert!(!s.bring_forward());
    assert!(!s.send_backward());
}

#[test]
fn bring_to_front_with_two_selected_is_false() {
    let mut s = seed_three_boxes();
    // select_at replaces the selection, so synthesize a
    // two-element selection through the test module access.
    let id_a = s.document.objects[0].id().to_string();
    let id_b = s.document.objects[1].id().to_string();
    s.clear_selection();
    s.selected_ids.insert(id_a);
    s.selected_ids.insert(id_b);
    assert_eq!(s.selected_count(), 2);

    s.mark_saved();
    assert!(
        !s.bring_to_front(),
        "multi-select raise/lower is intentionally a no-op"
    );
    assert!(!s.is_dirty());
}

#[test]
fn recolor_selection_with_empty_selection_is_zero_and_clean() {
    // Build a state with one box but no selection.
    let mut s = seed_dirty_box();
    s.clear_selection();
    s.mark_saved();
    assert!(!s.is_dirty());
    let undo_before = s.undo_stack.len();
    let changed = s.recolor_selection(InkColor::Red);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty(), "empty-selection recolor must not flip dirty");
    assert_eq!(
        s.undo_stack.len(),
        undo_before,
        "empty-selection recolor must not push undo"
    );
}

#[test]
fn recolor_selection_is_noop_when_already_that_color() {
    // Spam-resistance: pressing Ctrl-1 (White) on a White-only
    // selection must not push a NEW undo step or flip dirty.
    // (commit_draft inside seed_dirty_box already pushed one
    // baseline step; we measure that the stack doesn't grow.)
    let mut s = seed_dirty_box();
    // Default ink is White; box is already White.
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let dirty_before = s.is_dirty();
    let changed = s.recolor_selection(InkColor::White);
    assert_eq!(changed, 0);
    assert_eq!(s.undo_stack.len(), undo_before, "no new undo step");
    assert_eq!(s.is_dirty(), dirty_before, "dirty bit unchanged");
}

#[test]
fn align_selection_with_empty_selection_is_zero_and_clean() {
    let mut s = seed_three_boxes();
    s.clear_selection();
    s.mark_saved();
    assert!(!s.is_dirty());
    let undo_before = s.undo_stack.len();
    let moved = s.align_selection(Align::Left);
    assert_eq!(moved, 0);
    assert_eq!(s.undo_stack.len(), undo_before, "no undo push");
    assert!(!s.is_dirty(), "no dirty flip");
}

#[test]
fn align_selection_with_draft_in_progress_is_zero() {
    // Mirrors duplicate_selected: an in-progress shape
    // shouldn't be yanked to a shared edge while the user
    // is mid-draft.
    let mut s = seed_three_boxes();
    s.clear_selection();
    s.select_at(Point { x: 6, y: 1 });
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 4, y: 4 });
    assert!(s.has_draft());
    let moved = s.align_selection(Align::Left);
    assert_eq!(moved, 0);
}

#[test]
fn align_selection_is_noop_when_already_aligned() {
    // Spam-resistance parity with recolor_selection:
    // calling align twice in a row doesn't grow the undo
    // stack the second time.
    let mut s = seed_three_boxes();
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.align_selection(Align::Left);
    let undo_after_first = s.undo_stack.len();
    assert_eq!(
        s.align_selection(Align::Left),
        0,
        "second call reports 0 moved"
    );
    assert_eq!(
        s.undo_stack.len(),
        undo_after_first,
        "second call does not push undo"
    );
}

#[test]
fn distribute_selection_with_empty_selection_is_zero_and_clean() {
    let mut s = seed_three_boxes();
    s.clear_selection();
    s.mark_saved();
    assert!(!s.is_dirty());
    let undo_before = s.undo_stack.len();
    let moved = s.distribute_selection(DistributeAxis::Horizontal);
    assert_eq!(moved, 0);
    assert_eq!(s.undo_stack.len(), undo_before, "no undo push");
    assert!(!s.is_dirty(), "no dirty flip");
}

#[test]
fn distribute_selection_with_two_objects_is_zero() {
    // Distribute needs ≥3 — with 2 items the "gap" IS the
    // whole selection, nothing to redistribute. The chord
    // must be a clean no-op (no undo, no dirty).
    let mut s = seed_three_boxes();
    s.clear_selection();
    let a = s.document.objects[0].id().to_string();
    let b = s.document.objects[1].id().to_string();
    s.selected_ids.insert(a);
    s.selected_ids.insert(b);
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let moved = s.distribute_selection(DistributeAxis::Horizontal);
    assert_eq!(moved, 0);
    assert_eq!(s.undo_stack.len(), undo_before, "no undo push");
    assert!(!s.is_dirty());
}

#[test]
fn distribute_selection_with_draft_in_progress_is_zero() {
    // Mirrors align_selection: an in-progress shape
    // shouldn't be yanked mid-draft.
    let mut s = seed_three_boxes();
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.set_tool(DrawMode::Box);
    s.begin_draft(Point { x: 6, y: 6 });
    s.update_draft(Point { x: 8, y: 8 });
    assert!(s.has_draft());
    let moved = s.distribute_selection(DistributeAxis::Horizontal);
    assert_eq!(moved, 0);
}

#[test]
fn distribute_horizontal_three_already_equal_is_zero() {
    // The seed three boxes sit at centers 1, 6, 11 (gap 5).
    // Already equal — the short-circuit should return 0
    // without pushing undo.
    let mut s = seed_three_boxes();
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let moved = s.distribute_selection(DistributeAxis::Horizontal);
    assert_eq!(moved, 0);
    assert_eq!(s.undo_stack.len(), undo_before, "no undo push");
}

#[test]
fn distribute_selection_is_noop_when_already_equal() {
    // Spam-resistance parity with align_selection /
    // recolor_selection.
    let mut s = seed_three_boxes();
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.distribute_selection(DistributeAxis::Horizontal);
    let undo_after_first = s.undo_stack.len();
    assert_eq!(
        s.distribute_selection(DistributeAxis::Horizontal),
        0,
        "second call reports 0 moved"
    );
    assert_eq!(
        s.undo_stack.len(),
        undo_after_first,
        "second call does not push undo"
    );
}

#[test]
fn group_selection_with_empty_selection_is_none_and_clean() {
    let mut s = seed_three_boxes_with_distinct_ids();
    s.clear_selection();
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let parent = s.group_selection();
    assert!(parent.is_none());
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before, "no undo step on no-op");
}

#[test]
fn ungroup_selection_with_empty_selection_is_zero_and_clean() {
    let mut s = seed_three_boxes_with_distinct_ids();
    s.clear_selection();
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let cleared = s.ungroup_selection();
    assert_eq!(cleared, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn ungroup_selection_is_noop_when_nothing_grouped() {
    // Selection has objects but none have a parent_id —
    // ungroup should report 0 and skip the undo push so
    // spamming the key doesn't churn undo.
    let mut s = seed_three_boxes_with_distinct_ids();
    s.selected_ids.insert("box-a".into());
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let cleared = s.ungroup_selection();
    assert_eq!(cleared, 0);
    assert_eq!(s.undo_stack.len(), undo_before);
    assert!(!s.is_dirty());
}

#[test]
fn add_to_selection_unknown_id_is_noop() {
    let mut s = seed_three_boxes_with_distinct_ids();
    assert!(!s.add_to_selection("nope"));
    assert_eq!(s.selected_count(), 0);
}

#[test]
fn toggle_selection_unknown_id_is_noop() {
    let mut s = seed_three_boxes_with_distinct_ids();
    assert!(!s.toggle_selection("nope"));
    assert_eq!(s.selected_count(), 0);
}

#[test]
fn restyle_selection_with_empty_selection_is_zero_and_clean() {
    let mut s = seed_two_lines_one_elbow();
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_selection(LineStyle::Dashed);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_selection_is_noop_when_all_styled_already() {
    // Pre-set every Line/Elbow to Dashed; restyle to Dashed must
    // not push an undo step (spam-resistance, mirrors recolor).
    let mut s = seed_two_lines_one_elbow();
    for o in s.document.objects.iter_mut() {
        match o {
            DrawObject::Line(l) => l.style = LineStyle::Dashed,
            DrawObject::Elbow(e) => e.style = LineStyle::Dashed,
            // ponytail: Box / Paint / Text carry no LineStyle;
            // the setup loop intentionally skips them. A new
            // LineStyle-bearing kind would need its own arm.
            _ => {}
        }
    }
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_selection(LineStyle::Dashed);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_selection_with_only_boxes_returns_zero() {
    // If the selection contains nothing that carries a LineStyle
    // (e.g. only Boxes + Paint + Text), the helper must silently
    // skip — not push undo, not flip dirty, return 0.
    let mut s = DrawState::new();
    make_box_at(&mut s, 0, 0, 4, 4);
    make_box_at(&mut s, 6, 0, 10, 4);
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_selection(LineStyle::Light);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_boxes_selection_with_empty_selection_is_zero_and_clean() {
    let mut s = seed_three_boxes();
    // seed_three_boxes leaves the last-committed box selected
    // (commit_draft selects on insert). Clear so we have a
    // truly empty selection for this test.
    s.clear_selection();
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_boxes_selection(BoxStyle::Heavy);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_boxes_selection_with_only_lines_returns_zero() {
    // Mirrors the LineStyle reverse: a selection with no Boxes
    // must silently skip (no undo, no dirty, return 0).
    let mut s = DrawState::new();
    // Two lines, one elbow.
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 0, y: 0 });
    s.update_draft(Point { x: 5, y: 5 });
    s.commit_draft().unwrap();
    s.set_tool(DrawMode::Line);
    s.begin_draft(Point { x: 6, y: 0 });
    s.update_draft(Point { x: 11, y: 5 });
    s.commit_draft().unwrap();
    s.set_tool(DrawMode::Elbow);
    s.begin_draft(Point { x: 12, y: 0 });
    s.update_draft(Point { x: 17, y: 5 });
    s.commit_draft().unwrap();
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_boxes_selection(BoxStyle::Dashed);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_boxes_selection_is_noop_when_all_already_target() {
    // Pre-set every Box to Double; restyle to Double must not push
    // an undo step (spam resistance, mirrors restyle_selection).
    let mut s = seed_three_boxes();
    for o in s.document.objects.iter_mut() {
        if let DrawObject::Box(b) = o {
            b.style = BoxStyle::Double;
        }
    }
    s.clear_selection();
    for o in &s.document.objects {
        s.selected_ids.insert(o.id().to_string());
    }
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_boxes_selection(BoxStyle::Double);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn restyle_boxes_selection_skips_unselected_objects() {
    // 4 boxes total, 2 selected. Only the selected 2 should
    // change; the unselected 2 keep their Light style.
    let mut s = DrawState::new();
    let id_a = make_box_at(&mut s, 0, 0, 2, 2);
    let _id_b = make_box_at(&mut s, 4, 0, 6, 2);
    let _id_c = make_box_at(&mut s, 8, 0, 10, 2);
    let _id_d = make_box_at(&mut s, 12, 0, 14, 2);
    s.clear_selection();
    s.selected_ids.insert(id_a.clone());
    s.selected_ids.insert(_id_d.clone());
    let changed = s.restyle_boxes_selection(BoxStyle::Heavy);
    assert_eq!(changed, 2);
    // The two unselected keep Light.
    for o in &s.document.objects {
        match o {
            DrawObject::Box(b) if b.id == id_a || b.id == _id_d => {
                assert_eq!(b.style, BoxStyle::Heavy);
            }
            DrawObject::Box(b) => assert_eq!(b.style, BoxStyle::Light),
            _ => {}
        }
    }
}

#[test]
fn restyle_boxes_selection_counts_only_changed_boxes() {
    // One selected Box is already Heavy; restyle to Heavy on
    // that single selection must report 0 and skip the undo /
    // dirty flip.
    let mut s = DrawState::new();
    let id = make_box_at(&mut s, 0, 0, 4, 4);
    if let Some(DrawObject::Box(b)) = s.document.objects.iter_mut().find(|o| o.id() == id) {
        b.style = BoxStyle::Heavy;
    }
    s.clear_selection();
    s.selected_ids.insert(id);
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    let changed = s.restyle_boxes_selection(BoxStyle::Heavy);
    assert_eq!(changed, 0);
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn select_in_rect_with_empty_document_returns_zero() {
    // Empty doc + any mode → 0, no panic.
    let mut s = DrawState::new();
    let n = s.select_in_rect(
        Rect {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        },
        SelectionMode::Replace,
    );
    assert_eq!(n, 0);
    assert_eq!(s.selected_count(), 0);
}

#[test]
fn select_all_with_empty_document_returns_zero() {
    // Empty doc → 0, no panic, no selection. The "every
    // object" loop must tolerate an empty vec.
    let mut s = DrawState::new();
    let n = s.select_all();
    assert_eq!(n, 0);
    assert_eq!(s.selected_count(), 0);
}

#[test]
fn select_all_does_not_touch_dirty() {
    // select_all is a read-mostly operation against
    // selected_ids only — it must not flip the dirty
    // flag, push undo, or otherwise mutate the
    // document. Today the bin uses Ctrl-A in tandem with
    // the restyle cycles and we want those to still see
    // a clean "all selected" state, not a dirtied
    // document.
    let mut s = DrawState::new();
    make_box_at(&mut s, 0, 0, 2, 2);
    make_box_at(&mut s, 5, 0, 7, 2);
    s.clear_selection();
    s.mark_saved();
    let undo_before = s.undo_stack.len();
    s.select_all();
    assert!(!s.is_dirty());
    assert_eq!(s.undo_stack.len(), undo_before);
}

#[test]
fn select_in_rect_inverted_is_noop() {
    // An inverted marquee (right < left or bottom < top)
    // represents a click without a drag — must not mutate the
    // selection, must not panic, must report the current count.
    let (mut s, ids) = seed_marquee_boxes();
    s.selected_ids.insert(ids[0].clone());
    let before = s.selected_count();

    let n = s.select_in_rect(
        Rect {
            left: 8,
            top: 4,
            right: 4,   // inverted: right < left
            bottom: -1, // inverted: bottom < top
        },
        SelectionMode::Replace,
    );
    assert_eq!(n, before);
    assert_eq!(s.selected_count(), before);
    assert!(s.selected_ids.contains(&ids[0]));
}

#[test]
fn resize_aborts_when_no_box_selected() {
    let mut s = DrawState::new();
    // nothing selected
    assert!(!s.begin_resize(BoxResizeHandle::TopLeft));
}

#[test]
fn resize_aborts_when_selection_is_not_a_box() {
    let mut s = DrawState::new();
    s.document.objects.push(DrawObject::Line(LineObject {
        id: "l".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        x1: 0,
        y1: 0,
        x2: 5,
        y2: 5,
        style: LineStyle::Smooth,
    }));
    s.selected_ids.insert("l".into());
    assert!(!s.begin_resize(BoxResizeHandle::TopLeft));
}

#[test]
fn cancel_resize_restores_box_and_drops_snapshot() {
    let (mut s, id) = seeded_box_state();
    s.begin_resize(BoxResizeHandle::TopLeft);
    assert!(s.is_resizing());
    s.update_resize(Point { x: 0, y: 0 });
    // Box moved off the original bounds.
    assert_ne!(box_bounds(&s, &id), Some((10, 10, 20, 20)));
    // cancel_all drops the pre-drag snapshot; cancel_resize alone
    // restores bounds but doesn't pop (see undo/redo rationale
    // in the source — undo_during_resize_preserves_prior_history).
    s.cancel_all();
    assert!(!s.is_resizing());
    // cancel restored the bounds, so the document is back to seed.
    assert_eq!(box_bounds(&s, &id), Some((10, 10, 20, 20)));
}

#[test]
fn cancel_draft_does_not_abort_resize() {
    // Bug #1 regression: set_tool calls cancel_draft, which must
    // NOT silently abort an in-progress resize.
    let (mut s, _id) = seeded_box_state();
    s.set_tool(DrawMode::Line); // triggers cancel_draft internally
    s.begin_resize(BoxResizeHandle::TopLeft);
    assert!(s.is_resizing());
    s.set_tool(DrawMode::Box); // mid-resize tool switch
    assert!(
        s.is_resizing(),
        "set_tool must leave an active resize alone"
    );
    s.cancel_all();
    assert!(!s.is_resizing());
}
