//! Restyle tests: Ctrl-1..8 recolor, Ctrl-Alt-L / B / T / P
//! style cycles, the Ctrl-Shift-B / Ctrl-Shift-T "still aligns"
//! cross-checks (pin that the Ctrl-Alt arm doesn't shadow the
//! Ctrl-Shift arm), and the bare lowercase `i` ink-picker.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `commit_one_smooth_line` /
//! `commit_one_light_box` helpers move with the tests that
//! use them.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;
use kirkforge_draw_core::{DrawObject, InkColor};

#[test]
fn ctrl_digit_recolors_selection() {
    // Ctrl-2 (= Red) on a single White box must change the box's
    // color and report "recolored 1 object to red". Drives the
    // full keymap → ink_color_for_digit → recolor_selection path.
    let mut app = make_app();
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 1, y: 1 });
    app.state.update_draft(Point { x: 4, y: 3 });
    app.state.commit_draft().unwrap();
    app.state.set_tool(DrawMode::Select);
    assert_eq!(app.state.document.objects.len(), 1);

    handle_key(&mut app, key_ctrl(KeyCode::Char('2')));
    assert_eq!(app.state.document.objects[0].color(), InkColor::Red);
    assert!(
        app.status.contains("recolored 1 object to red"),
        "status should report a recolor; got {:?}",
        app.status
    );
}

#[test]
fn ctrl_digit_no_selection_reports_status() {
    // Pressing Ctrl-3 on an empty editor must not panic, must not
    // surface a recolor-success message, and must report "nothing
    // to recolor" so the user knows the keypress was received.
    let mut app = make_app();
    let dirty_before = app.state.is_dirty();
    handle_key(&mut app, key_ctrl(KeyCode::Char('3')));
    assert_eq!(app.state.is_dirty(), dirty_before);
    assert!(app.status.contains("nothing to recolor"));
}

#[test]
fn ctrl_digit_already_that_color_reports_no_change() {
    // Pressing Ctrl-1 (White) on a White selection is a silent
    // no-op from the user's POV except for the "already white"
    // status — and the dirty bit must not flip.
    let mut app = make_app();
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 1, y: 1 });
    app.state.update_draft(Point { x: 4, y: 3 });
    app.state.commit_draft().unwrap();
    // commit_draft marks dirty; mark_saved so we can detect that
    // the recolor keypress doesn't re-dirty.
    app.state.mark_saved();
    assert!(!app.state.is_dirty());
    // Box is selected after commit and is White by default.
    handle_key(&mut app, key_ctrl(KeyCode::Char('1')));
    assert!(!app.state.is_dirty(), "no-op recolor must not flip dirty");
    assert!(app.status.contains("already white"));
}

/// Helper: commit a Smooth Line from (0,0) to (5,3) and leave
/// it selected. Used by the Ctrl-Alt-L cycle tests below.
fn commit_one_smooth_line(app: &mut App) {
    use kirkforge_draw_core::LineStyle;
    app.state.set_tool(DrawMode::Line);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 5, y: 3 });
    app.state.commit_draft().unwrap();
    app.state.set_tool(DrawMode::Select);
    // Sanity: the just-committed line is selected and Smooth.
    assert_eq!(app.state.selected_count(), 1);
    match &app.state.document.objects[0] {
        DrawObject::Line(l) => assert_eq!(l.style, LineStyle::Smooth),
        other => panic!("expected Line, got {:?}", std::mem::discriminant(other)),
    }
}

#[test]
fn ctrl_alt_l_cycles_line_style_on_line() {
    // Ctrl-Alt-L on a single Smooth Line must advance to the
    // next style (Light) and report it. Drives the full keymap
    // → cycle_line_style → restyle_selection path.
    use kirkforge_draw_core::LineStyle;
    let mut app = make_app();
    commit_one_smooth_line(&mut app);

    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
    match &app.state.document.objects[0] {
        DrawObject::Line(l) => assert_eq!(l.style, LineStyle::Light),
        _ => unreachable!(),
    }
    assert!(
        app.status.contains("restyled 1 object to light"),
        "status should report a restyle; got {:?}",
        app.status
    );
}

#[test]
fn ctrl_alt_l_with_empty_selection_reports_nothing() {
    // Pressing Ctrl-Alt-L on an empty editor must report
    // "nothing to restyle" and not flip dirty.
    let mut app = make_app();
    let dirty_before = app.state.is_dirty();
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
    assert_eq!(app.state.is_dirty(), dirty_before);
    assert!(app.status.contains("nothing to restyle"));
}

#[test]
fn ctrl_alt_l_on_selection_with_no_lines_reports_kind() {
    // Pressing Ctrl-Alt-L when only a Box is selected must
    // report "no lines / elbows in selection" — Boxes have
    // BoxStyle, not LineStyle, so they don't participate.
    let mut app = make_app();
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 1, y: 1 });
    app.state.update_draft(Point { x: 4, y: 3 });
    app.state.commit_draft().unwrap();
    app.state.set_tool(DrawMode::Select);

    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('l')));
    assert!(
        app.status.contains("no lines / elbows in selection"),
        "status should report kind mismatch; got {:?}",
        app.status
    );
}

#[test]
fn ctrl_alt_l_does_not_arm_l_line_tool() {
    // Bare 'l' (no Ctrl / Alt) must still set the Line tool —
    // Ctrl-Alt-L is a sibling, not a replacement. Regression
    // guard so a future arm-order change can't silently
    // shadow the tool hotkey.
    let mut app = make_app();
    handle_key(&mut app, key(KeyCode::Char('l')));
    assert_eq!(app.state.tool, DrawMode::Line);
}

/// Helper: commit one Box and leave it selected. Mirrors
/// `commit_one_smooth_line` for the BoxStyle cycle tests.
fn commit_one_light_box(app: &mut App) {
    use kirkforge_draw_core::BoxStyle;
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 5, y: 4 });
    app.state.commit_draft().unwrap();
    app.state.set_tool(DrawMode::Select);
    assert_eq!(app.state.selected_count(), 1);
    match &app.state.document.objects[0] {
        DrawObject::Box(b) => assert_eq!(b.style, BoxStyle::Light),
        other => panic!("expected Box, got {:?}", std::mem::discriminant(other)),
    }
}

#[test]
fn ctrl_alt_b_cycles_box_style_on_box() {
    // Ctrl-Alt-B on a single Light Box must advance to Heavy
    // (the next in the cycle) and report it. Drives the full
    // keymap → cycle_box_style → restyle_boxes_selection path.
    use kirkforge_draw_core::BoxStyle;
    let mut app = make_app();
    commit_one_light_box(&mut app);

    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
    match &app.state.document.objects[0] {
        DrawObject::Box(b) => assert_eq!(b.style, BoxStyle::Heavy),
        _ => unreachable!(),
    }
    assert!(
        app.status.contains("restyled 1 object to heavy"),
        "status should report a restyle; got {:?}",
        app.status
    );
}

#[test]
fn ctrl_alt_b_with_empty_selection_reports_nothing() {
    let mut app = make_app();
    let dirty_before = app.state.is_dirty();
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
    assert_eq!(app.state.is_dirty(), dirty_before);
    assert!(app.status.contains("nothing to restyle"));
}

#[test]
fn ctrl_alt_b_on_selection_with_no_boxes_reports_kind() {
    // Pressing Ctrl-Alt-B when only a Line is selected must
    // report "no boxes in selection" — Lines have LineStyle,
    // not BoxStyle, so they don't participate.
    let mut app = make_app();
    commit_one_smooth_line(&mut app);

    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('b')));
    assert!(
        app.status.contains("no boxes in selection"),
        "status should report kind mismatch; got {:?}",
        app.status
    );
}

#[test]
fn ctrl_alt_b_does_not_arm_b_box_tool() {
    // Bare 'b' (no Ctrl / Alt) must still set the Box tool —
    // Ctrl-Alt-B is a sibling, not a replacement. Regression
    // guard so a future arm-order change can't silently
    // shadow the tool hotkey.
    let mut app = make_app();
    handle_key(&mut app, key(KeyCode::Char('b')));
    assert_eq!(app.state.tool, DrawMode::Box);
}

#[test]
fn ctrl_shift_b_still_aligns_bottom_under_ctrl_alt_b() {
    // Ctrl-Shift-B (align bottom) must continue to work
    // alongside Ctrl-Alt-B. Order-sensitive: if the
    // Ctrl-Alt-B arm were placed AFTER the Ctrl-Shift-B
    // arm, this test would fail because ctrl && alt
    // wouldn't be matched (the Ctrl-Shift-B arm is
    // `ctrl && shift && !alt`, so a Ctrl-Alt-B press
    // wouldn't hit it — but the test exists to pin the
    // arm ordering regardless of guard shape).
    let mut app = make_app();
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();
    app.state.begin_draft(Point { x: 0, y: 5 });
    app.state.update_draft(Point { x: 2, y: 7 });
    app.state.commit_draft().unwrap();
    // Select both via the public add_to_selection helper.
    // Collect ids first to avoid borrow conflict with the
    // mutable add_to_selection call.
    let ids: Vec<String> = app
        .state
        .document
        .objects
        .iter()
        .map(|o| o.id().to_string())
        .collect();
    for id in &ids {
        app.state.add_to_selection(id);
    }
    handle_key(&mut app, key_ctrl_shift('b'));
    // align-bottom reports a moved-count, not a selected
    // count — only box 1 (top) moves; box 2 was already at
    // the bottom of the union so it doesn't count.
    // What matters here is that the status is the
    // align-bottom format, not the restyle format.
    assert!(
        app.status.contains("to bottom edge"),
        "Ctrl-Shift-B should still align bottom; got {:?}",
        app.status
    );
    assert!(
        !app.status.contains("restyled"),
        "Ctrl-Shift-B should not be intercepted by Ctrl-Alt-B; got {:?}",
        app.status
    );
}

// -- Text border cycle (Ctrl-Alt-T) -----------------------------

#[test]
fn ctrl_alt_t_advances_text_border() {
    // Default text_border is None. Pressing Ctrl-Alt-T
    // should land on Single and surface the new name in
    // the status bar.
    let mut app = make_app();
    assert_eq!(
        app.state.text_border,
        kirkforge_draw_core::TextBorderMode::None
    );
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
    assert_eq!(
        app.state.text_border,
        kirkforge_draw_core::TextBorderMode::Single
    );
    assert!(app.status.contains("single"), "got {:?}", app.status);
}

#[test]
fn ctrl_alt_t_wraps_from_underline_to_none() {
    // Four presses must visit every variant and wrap
    // back to None on the fifth. The wrap is the bit
    // most likely to drift if a future enum addition
    // forgets the trailing arm.
    let mut app = make_app();
    let order = [
        kirkforge_draw_core::TextBorderMode::Single,
        kirkforge_draw_core::TextBorderMode::Double,
        kirkforge_draw_core::TextBorderMode::Underline,
        kirkforge_draw_core::TextBorderMode::None,
    ];
    for (i, expected) in order.iter().enumerate() {
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
        assert_eq!(
            &app.state.text_border,
            expected,
            "press #{}: expected {:?}, got {:?}",
            i + 1,
            expected,
            app.state.text_border
        );
    }
    // One more press returns to Single — the cycle is
    // closed and stable.
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('t')));
    assert_eq!(
        app.state.text_border,
        kirkforge_draw_core::TextBorderMode::Single
    );
}

#[test]
fn ctrl_alt_t_does_not_arm_t_text_tool() {
    // Bare 't' is the Text tool hotkey. Ctrl-Alt-T is a
    // sibling chord, not a replacement. Regression
    // guard so a future arm-order change can't silently
    // shadow the tool hotkey.
    let mut app = make_app();
    handle_key(&mut app, key(KeyCode::Char('t')));
    assert_eq!(app.state.tool, DrawMode::Text);
}

#[test]
fn ctrl_shift_t_still_aligns_top_under_ctrl_alt_t() {
    // Ctrl-Shift-T (align top) must continue to work
    // alongside Ctrl-Alt-T. The Ctrl-Alt-T arm is
    // `ctrl && alt`; the Ctrl-Shift-T arm is
    // `ctrl && shift && !alt`, so a Ctrl-Alt-T press
    // doesn't hit the align arm — but a Ctrl-Shift-T
    // press (no Alt) shouldn't hit the cycle arm
    // either. This test pins that the status bar shows
    // the align message, not the cycle message.
    let mut app = make_app();
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();
    app.state.begin_draft(Point { x: 0, y: 5 });
    app.state.update_draft(Point { x: 2, y: 7 });
    app.state.commit_draft().unwrap();
    let ids: Vec<String> = app
        .state
        .document
        .objects
        .iter()
        .map(|o| o.id().to_string())
        .collect();
    for id in &ids {
        app.state.add_to_selection(id);
    }
    handle_key(&mut app, key_ctrl_shift('t'));
    assert!(
        app.status.contains("to top edge"),
        "Ctrl-Shift-T should still align top; got {:?}",
        app.status
    );
    assert!(
        !app.status.contains("text border"),
        "Ctrl-Shift-T should not be intercepted by Ctrl-Alt-T; got {:?}",
        app.status
    );
}

// -- Paint brush cycle (Ctrl-Alt-P) -----------------------------

#[test]
fn ctrl_alt_p_advances_brush() {
    // Default brush is `·` (the middle dot). Pressing
    // Ctrl-Alt-P should land on `o` and surface the new
    // glyph in the status bar. The status bar echoes
    // the literal glyph so the user sees what they'll
    // draw next.
    let mut app = make_app();
    assert_eq!(app.state.brush, "·");
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
    assert_eq!(app.state.brush, "o");
    assert!(app.status.contains("o"), "got {:?}", app.status);
}

#[test]
fn ctrl_alt_p_wraps_after_eight_presses() {
    // Eight presses must visit every palette entry and
    // wrap back to `·` on the ninth. The wrap is the
    // bit most likely to drift if a future palette
    // addition forgets the modular index.
    let mut app = make_app();
    let order = ["o", "*", "x", "█", "▒", "░", "▓", "·"];
    for (i, expected) in order.iter().enumerate() {
        handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
        assert_eq!(
            &app.state.brush,
            expected,
            "press #{}: expected {:?}, got {:?}",
            i + 1,
            expected,
            app.state.brush
        );
    }
    // One more press advances to `o` — the cycle is
    // closed and stable.
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
    assert_eq!(app.state.brush, "o");
}

#[test]
fn ctrl_alt_p_recovers_from_unknown_brush() {
    // ponytail: the user can `set_brush(anything)` —
    // any character not in the palette should snap to
    // the first palette entry so the next press keeps
    // them in the cycle. Pin this so a future "user
    // types a custom brush" path doesn't strand the
    // cycle arm. The first press lands on `·` (first
    // palette entry); the second press lands on `o`,
    // matching the wrap behavior of a known brush.
    let mut app = make_app();
    app.state.set_brush("Z");
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
    assert_eq!(app.state.brush, "·");
    handle_key(&mut app, key_ctrl_alt(KeyCode::Char('p')));
    assert_eq!(app.state.brush, "o");
}

#[test]
fn ctrl_alt_p_does_not_arm_p_paint_tool() {
    // Bare `p` is the Paint tool hotkey. Ctrl-Alt-P is
    // a sibling chord, not a replacement. Regression
    // guard so a future arm-order change can't silently
    // shadow the tool hotkey.
    let mut app = make_app();
    handle_key(&mut app, key(KeyCode::Char('p')));
    assert_eq!(app.state.tool, DrawMode::Paint);
}

// --- `i` ink-picker (cycle color forward) -------------------
//
// Bin tests for the bare lowercase `i` shortcut, which
// advances the selection's InkColor one step through the
// enum's discriminant order (White → Red → … → Magenta →
// White). Mirrors the existing Ctrl-1..8 cluster but with
// a "next color" gesture instead of "jump to color N".
// The pure `cycle_*` helpers (next_ink_color etc.) live
// in bin because they're trivial 1-line matches and have
// no observable side effects to test directly — these
// tests cover the bin wiring (arm fires, status echoes,
// undo batch, multi-select normalization, wrap, no-op
// spam resistance, empty-selection message).

#[test]
fn lower_i_advances_single_selected_box_color() {
    // Bare `i` on a single White box → Red, status reports
    // "recolored 1 object to red". Mirrors the recolor
    // status format so users get the same feedback whether
    // they press Ctrl-2 (jump to red) or `i` (advance from
    // white to red).
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "x".into(),
        z: 0,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.select_id("x");
    handle_key(&mut app, key(KeyCode::Char('i')));
    let DrawObject::Box(b) = &app.state.document.objects[0] else {
        panic!("expected box");
    };
    assert_eq!(b.color, InkColor::Red);
    assert!(app.status.contains("recolored 1 object to red"));
}

#[test]
fn lower_i_wraps_from_magenta_to_white() {
    // Eight consecutive presses from White should return
    // to White (full enum cycle). Verifies the wrap
    // behavior at the end of the InkColor order so a user
    // spamming `i` can't get stuck on the last variant.
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "x".into(),
        z: 0,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.select_id("x");
    for _ in 0..8 {
        handle_key(&mut app, key(KeyCode::Char('i')));
    }
    let DrawObject::Box(b) = &app.state.document.objects[0] else {
        panic!("expected box");
    };
    assert_eq!(b.color, InkColor::White, "8 presses = full cycle");
}

#[test]
fn lower_i_normalizes_mixed_selection_to_next_of_first() {
    // Two selected boxes, one White and one Red. First
    // (document-order) is White → next is Red, so both
    // should land on Red. Mirrors `recolor_selection`'s
    // "normalize the batch" semantics so the cycle
    // shortcut behaves the same as Ctrl-2 for mixed
    // selections.
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "first".into(),
        z: 0,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "second".into(),
        z: 1,
        parent_id: None,
        color: InkColor::Green,
        left: 4,
        top: 0,
        right: 6,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.add_to_selection("first");
    app.state.add_to_selection("second");
    handle_key(&mut app, key(KeyCode::Char('i')));
    let mut colors: Vec<InkColor> = app
        .state
        .document
        .objects
        .iter()
        .map(|o| o.color())
        .collect();
    colors.sort_by_key(|c| match c {
        InkColor::White => 0,
        InkColor::Red => 1,
        InkColor::Orange => 2,
        InkColor::Yellow => 3,
        InkColor::Green => 4,
        InkColor::Cyan => 5,
        InkColor::Blue => 6,
        InkColor::Magenta => 7,
    });
    assert_eq!(colors, vec![InkColor::Red, InkColor::Red]);
    assert!(app.status.contains("recolored 2 objects to red"));
}

#[test]
fn lower_i_pushes_one_undo_step_for_batch() {
    // Three selected boxes advance in lockstep on a
    // single `i` press, and one Ctrl-Z reverts all three.
    // Same single-undo-step contract that
    // `recolor_selection` guarantees — the cycle
    // shortcut is a thin wrapper, so it inherits the
    // contract for free, but the test pins it.
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    for i in 0..3 {
        app.state.document.objects.push(DrawObject::Box(BoxObject {
            id: format!("b{i}"),
            z: i,
            parent_id: None,
            color: InkColor::White,
            left: i * 4,
            top: 0,
            right: i * 4 + 2,
            bottom: 1,
            style: BoxStyle::Light,
        }));
        app.state.add_to_selection(&format!("b{i}"));
    }
    handle_key(&mut app, key(KeyCode::Char('i')));
    for obj in &app.state.document.objects {
        let DrawObject::Box(b) = obj else { panic!() };
        assert_eq!(b.color, InkColor::Red, "all three should be Red");
    }
    handle_key(&mut app, key_ctrl(KeyCode::Char('z')));
    for obj in &app.state.document.objects {
        let DrawObject::Box(b) = obj else { panic!() };
        assert_eq!(b.color, InkColor::White, "Ctrl-Z reverts all 3");
    }
}

#[test]
fn lower_i_leaves_unselected_objects_untouched() {
    // Selection contains one White box; a sibling Red
    // box sits unselected in the same document. Pressing
    // `i` advances the White selection to Red (next of
    // White) but leaves the unselected Red box alone.
    // Pins the delegation: cycle routes through
    // `recolor_selection` which respects the selection
    // set, not the document.
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "selected".into(),
        z: 0,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "unselected".into(),
        z: 1,
        parent_id: None,
        color: InkColor::Red,
        left: 4,
        top: 0,
        right: 6,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.select_id("selected");
    handle_key(&mut app, key(KeyCode::Char('i')));
    let DrawObject::Box(sel) = &app.state.document.objects[0] else {
        panic!();
    };
    let DrawObject::Box(unsel) = &app.state.document.objects[1] else {
        panic!();
    };
    assert_eq!(sel.color, InkColor::Red, "selected advanced to next");
    assert_eq!(unsel.color, InkColor::Red, "unselected stays put");
}

#[test]
fn lower_i_cycles_eight_variants_in_order() {
    // Eight presses from White walk through every
    // variant in enum-discriminant order, ending at
    // Magenta (one step before wrap). Mirrors the wrap
    // test but pins the order: White → Red → Orange →
    // Yellow → Green → Cyan → Blue → Magenta.
    use kirkforge_draw_core::{BoxObject, BoxStyle, DrawObject, InkColor};
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "x".into(),
        z: 0,
        parent_id: None,
        color: InkColor::White,
        left: 0,
        top: 0,
        right: 2,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state.select_id("x");
    let expected = [
        InkColor::Red,
        InkColor::Orange,
        InkColor::Yellow,
        InkColor::Green,
        InkColor::Cyan,
        InkColor::Blue,
        InkColor::Magenta,
        InkColor::White, // 8th press wraps back to White
    ];
    for (i, want) in expected.iter().enumerate() {
        handle_key(&mut app, key(KeyCode::Char('i')));
        let DrawObject::Box(b) = &app.state.document.objects[0] else {
            panic!("expected box");
        };
        assert_eq!(
            &b.color,
            want,
            "step {}: expected {:?}, got {:?}",
            i + 1,
            want,
            b.color
        );
    }
}

#[test]
fn lower_i_on_empty_selection_reports_nothing() {
    // Empty selection → status echoes "nothing to
    // recolor", same as Ctrl-1..8 with an empty
    // selection. Mirrors the existing recolor status
    // string so the two gestures are interchangeable
    // when the user has nothing selected.
    let mut app = make_app();
    handle_key(&mut app, key(KeyCode::Char('i')));
    assert_eq!(app.status, "nothing to recolor");
}
