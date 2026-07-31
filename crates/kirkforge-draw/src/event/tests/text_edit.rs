//! F2 text-edit tests: multi-line (Shift+Enter inserts `\n`,
//! bare Enter commits), write-through (live buffer → document
//! on every keystroke), mid-buffer cursor (Left / Right / Home
//! / End / Up / Down / Backspace / Delete, byte-offset model,
//! multibyte CJK), and the Esc-cancel revert contract.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `make_app_with_text` helper moves
//! with the tests that use it.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;
use kirkforge_draw_core::DrawObject;

// -- Multi-line text edit --------------------------------------
//
// Bare Enter commits; Shift+Enter inserts `\n`. The bin-level
// check is just that the right key path runs — the heavy
// rendering / rect coverage lives in core. We seed a Text
// object, open the edit session via F2, type / Shift+Enter,
// and verify the in-memory buffer.

fn make_app_with_text() -> App {
    use kirkforge_draw_core::{BoxStyle, DrawMode, InkColor, TextBorderMode, TextObject};
    let mut app = App::new(kirkforge_draw_core::DrawState::new());
    app.state.set_tool(DrawMode::Select);
    // Seed via direct push so we don't go through the
    // draft/commit pipeline (we only care about the editor's
    // text-edit side, not how the Text got into the doc).
    app.state
        .document
        .objects
        .push(DrawObject::Text(TextObject {
            id: "t-1".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 0,
            content: "".into(),
            border: TextBorderMode::None,
        }));
    // Make the new Text the selection.
    app.state.select_id("t-1");
    // Suppress the unused-import lint when InkColor / BoxStyle
    // aren't otherwise referenced.
    let _ = (InkColor::White, BoxStyle::Light);
    app
}

#[test]
fn shift_enter_inserts_newline_in_text_edit_buffer() {
    let mut app = make_app_with_text();
    // F2 opens the edit session.
    handle_key(&mut app, key(KeyCode::F(2)));
    assert!(app.text_edit.is_some(), "F2 should open text edit");
    // Type "ab", Shift+Enter, type "cd".
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Char('d')));
    let buf = app.text_edit.as_ref().unwrap().buffer.clone();
    assert_eq!(buf, "ab\ncd", "Shift+Enter must insert a newline");
}

#[test]
fn plain_enter_commits_text_edit_not_newline() {
    // Regression guard: pre-multi-line, Enter committed. We
    // want to lock that bare Enter still commits — Shift+Enter
    // is the only path that inserts \n.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Enter));
    // Edit session ended.
    assert!(
        app.text_edit.is_none(),
        "bare Enter must commit, not insert"
    );
    // Buffer landed on the document.
    let id = "t-1".to_string();
    assert_eq!(app.state.text_content(&id), Some("a".to_string()));
}

#[test]
fn f2_commit_with_vanished_target_surfaces_status() {
    // If the Text object disappears between F2 open and
    // commit (e.g., an undo removed it, or an external
    // mutation cleared the document), `commit_text_content`
    // returns false and the bin surfaces "edit target
    // vanished" instead of "text edited". The F2 session
    // still closes cleanly (text_edit is taken), so the
    // user isn't left in a half-open edit mode.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('h')));
    handle_key(&mut app, key(KeyCode::Char('i')));
    // Simulate the target vanishing mid-edit: drop the
    // only Text from the document. text_edit.target_id
    // still points at "t-1", so the next commit has to
    // route through the "no such id" branch.
    app.state.document.objects.retain(|o| o.id() != "t-1");
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(
        app.text_edit.is_none(),
        "F2 session must close even when the target is gone"
    );
    assert_eq!(
        app.status, "edit target vanished",
        "commit must surface the vanished-target status, not 'text edited'"
    );
}

#[test]
fn f2_commit_with_no_changes_surfaces_no_changes_status() {
    // Open F2 on a Text, press Enter without typing. The
    // buffer equals initial_content so `edit.dirty` stays
    // false and `commit_text_edit` takes the early-out
    // branch: "edit cancelled (no changes)" + return false
    // + F2 session closes (text_edit is taken). This is
    // the common "oops, I didn't mean to F2" exit and
    // must not push an undo step or echo "text edited"
    // (which would imply something happened).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    assert!(app.text_edit.is_some(), "F2 must open a session");
    // Don't type — press Enter straight away.
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(
        app.text_edit.is_none(),
        "F2 session must close even on a no-op commit"
    );
    assert_eq!(
        app.status, "edit cancelled (no changes)",
        "no-typing commit must surface the no-changes status"
    );
    // No content was written: the Text object's content
    // is still the initial empty string.
    let text = app
        .state
        .document
        .objects
        .iter()
        .find(|o| o.id() == "t-1")
        .expect("t-1 must still be in the document");
    if let DrawObject::Text(t) = text {
        assert_eq!(t.content, "", "no-typing commit must not mutate content");
    } else {
        panic!("expected Text object, got a non-Text");
    }
}

#[test]
fn shift_enter_then_plain_enter_commits_multiline() {
    // Full commit pipeline: type + newline + type, plain
    // Enter writes the multi-line buffer back to the doc.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('x')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('y')));
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.text_edit.is_none());
    let id = "t-1".to_string();
    assert_eq!(app.state.text_content(&id), Some("x\ny".to_string()));
}

// -- F2 write-through (live buffer → document) ---------------
//
// These tests pin the contract that the editor sees their
// typing in real time. Pre-write-through, the buffer was
// invisible until commit — every F2 session was effectively
// "type blind, hope you remember what you typed". Now the
// helper stamps the buffer onto the TextObject on every
// keystroke so the rendered scene reflects the user's input
// live. Dirty / undo stay anchored to commit; that's
// pinned in core, here we just lock the bin side.

#[test]
fn f2_insert_char_writes_through_to_document_text_object() {
    // Without write-through the buffer is invisible until
    // commit. With it, the document's TextObject.content
    // mirrors the buffer on every char.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('h')));
    let id = "t-1".to_string();
    assert_eq!(
        app.state.text_content(&id).as_deref(),
        Some("h"),
        "typed char lands on the document, not just the buffer"
    );
}

#[test]
fn f2_backspace_writes_through_to_document_text_object() {
    // Backspace during edit updates both buffer and document
    // — the user sees the last glyph disappear live.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    let id = "t-1".to_string();
    assert_eq!(app.state.text_content(&id).as_deref(), Some("ab"));
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(
        app.state.text_content(&id).as_deref(),
        Some("a"),
        "backspace writes through too"
    );
}

// -- F2 mid-buffer cursor (arrow-key navigation) -------------
//
// Cursor offset is the byte index in the edit buffer where
// the next insert / delete lands, and where the visible
// cursor paints. Left / Right step it one byte (clamped at
// the buffer edges); Backspace pops the byte before the
// offset; insert splices at the offset then advances.

#[test]
fn f2_starts_with_cursor_at_buffer_end() {
    // Fresh edit session: cursor sits at the end of the
    // initial content (replacing the prior "always EOB"
    // contract with the same behavior, but now it's an
    // explicit field on TextEditState).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.cursor_offset, edit.buffer.len());
}

#[test]
fn f2_left_arrow_steps_cursor_back_one_byte() {
    // Buffer "abc" → cursor_offset 3 (EOB).
    // Two Left presses → offset 1 (between 'a' and 'b').
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
}

#[test]
fn f2_right_arrow_advances_cursor_one_byte() {
    // Symmetric: walk back, walk forward.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    // cursor_offset is 2 (EOB). One Left → 1; one Right → 2.
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Right));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 2);
}

#[test]
fn f2_left_clamps_at_buffer_start() {
    // Pressing Left at offset 0 is a no-op (doesn't panic).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
}

#[test]
fn f2_right_clamps_at_buffer_end() {
    // Pressing Right at offset == buffer.len() is a no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('z')));
    handle_key(&mut app, key(KeyCode::Right));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.cursor_offset, edit.buffer.len());
}

#[test]
fn f2_insert_at_mid_buffer_splices_in_place() {
    // Type "ac", walk cursor Left, type 'b'. The result is
    // "abc" — the splice happened at the cursor, not at EOB.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Char('b')));
    let buf = app.text_edit.as_ref().unwrap().buffer.clone();
    assert_eq!(buf, "abc", "mid-buffer insert splices, not appends");
}

#[test]
fn f2_backspace_at_mid_buffer_removes_byte_before_cursor() {
    // Type "abc", walk cursor Left twice (offset 1, between
    // 'a' and 'b'), Backspace removes 'a'.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Backspace));
    let buf = app.text_edit.as_ref().unwrap().buffer.clone();
    assert_eq!(
        buf, "bc",
        "mid-buffer backspace deletes the byte before the cursor"
    );
}

#[test]
fn f2_backspace_at_offset_zero_is_noop() {
    // Fresh empty edit session, Backspace doesn't panic or
    // wrap around — it's a no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.text_edit.as_ref().unwrap().buffer, "");
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
}

#[test]
fn f2_backspace_removes_full_multibyte_char_before_cursor() {
    // Mirror of `f2_delete_removes_full_multibyte_char_at_cursor`
    // for the Backspace direction. Buffer "a日本" — cursor
    // at EOB (offset 7 = 1 ASCII + 2×3-byte CJK); Backspace
    // pops all 3 bytes of '本' (0xE6 0x9C 0xAC), leaving
    // "a日" with cursor at offset 4.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('日')));
    handle_key(&mut app, key(KeyCode::Char('本')));
    // Sanity: cursor at EOB (1 + 3 + 3 = 7 bytes).
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 7);
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.text_edit.as_ref().unwrap().buffer, "a日");
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 4);
}

#[test]
fn f2_mid_buffer_state_writes_through_to_document() {
    // Write-through extends to mid-buffer inserts: after
    // walking the cursor and inserting 'b' between 'a' and
    // 'c', the document's TextObject.content mirrors the
    // spliced buffer.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Char('b')));
    let id = "t-1".to_string();
    assert_eq!(app.state.text_content(&id).as_deref(), Some("abc"));
}

#[test]
fn f2_home_jumps_cursor_to_offset_zero() {
    // Type "abc", then Home: cursor drops to offset 0
    // (visible cursor paints at the first cell).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Home));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
}

#[test]
fn f2_end_jumps_cursor_to_buffer_end() {
    // Type "abc", then walk Left, then End: cursor snaps
    // back to EOB.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::End));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.cursor_offset, edit.buffer.len());
}

#[test]
fn f2_home_then_insert_appends_at_buffer_start() {
    // Cursor at offset 0, type 'z' → buffer "zabc".
    // Locks that the splice site (not the cursor's old
    // EOB position) is what determines where the char
    // lands.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Home));
    handle_key(&mut app, key(KeyCode::Char('z')));
    let buf = app.text_edit.as_ref().unwrap().buffer.clone();
    assert_eq!(buf, "zabc", "Home + char inserts at buffer start");
}

#[test]
fn f2_home_already_at_start_is_noop() {
    // Fresh edit session (offset 0 already); Home is a no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Home));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
}

#[test]
fn f2_end_already_at_end_is_noop() {
    // Fresh edit session after typing one char (offset 1);
    // End is a no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('q')));
    handle_key(&mut app, key(KeyCode::End));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.cursor_offset, edit.buffer.len());
}

#[test]
fn f2_up_moves_cursor_to_prior_line() {
    // Build a multi-line buffer: "abc\ndef" with cursor
    // at EOB (offset 7). Up → line 1, column 3 (within
    // "abc" length 3) → offset 3 (the '\n' byte).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('d')));
    handle_key(&mut app, key(KeyCode::Char('e')));
    handle_key(&mut app, key(KeyCode::Char('f')));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 7);
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(
        app.text_edit.as_ref().unwrap().cursor_offset,
        3,
        "Up from end of line 2 lands at end of line 1"
    );
}

#[test]
fn f2_down_moves_cursor_to_next_line() {
    // Build a multi-line buffer: "abc\ndef", cursor at
    // offset 0 (start). Down → line 2, column 0 → offset
    // 4 (start of "def").
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('d')));
    handle_key(&mut app, key(KeyCode::Char('e')));
    handle_key(&mut app, key(KeyCode::Char('f')));
    handle_key(&mut app, key(KeyCode::Home));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    handle_key(&mut app, key(KeyCode::Down));
    assert_eq!(
        app.text_edit.as_ref().unwrap().cursor_offset,
        4,
        "Down from start of line 1 lands at start of line 2"
    );
}

#[test]
fn f2_up_from_first_line_is_noop() {
    // Buffer "abc" (no '\n'); Up at offset 1 → no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Home));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 0);
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(
        app.text_edit.as_ref().unwrap().cursor_offset,
        0,
        "Up on the first line is a no-op"
    );
}

#[test]
fn f2_down_from_last_line_is_noop() {
    // Buffer "abc"; Down at offset 2 (EOB) → no-op.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    let edit = app.text_edit.as_ref().unwrap();
    let eob = edit.buffer.len();
    assert_eq!(edit.cursor_offset, eob);
    handle_key(&mut app, key(KeyCode::Down));
    assert_eq!(
        app.text_edit.as_ref().unwrap().cursor_offset,
        eob,
        "Down on the last line is a no-op"
    );
}

#[test]
fn f2_up_clamps_to_shorter_target_line() {
    // Buffer "a\nbbc" — line 1 length 1, line 2 length
    // 3. Cursor at EOB (offset 5, column 3 on line 2).
    // Up → line 1, column 3 clamped to length 1 → offset
    // 1. Locks the column-clamp behavior.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(
        app.text_edit.as_ref().unwrap().cursor_offset,
        1,
        "Up from column 3 on a 3-char line clamps to length 1"
    );
}

#[test]
fn f2_delete_at_offset_zero_removes_first_char() {
    // Buffer "abc" — cursor at 0; Delete removes 'a',
    // leaving "bc". Cursor offset stays at 0.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Home));
    handle_key(&mut app, key(KeyCode::Delete));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.buffer, "bc", "Delete at offset 0 removes first char");
    assert_eq!(
        edit.cursor_offset, 0,
        "cursor stays at 0 after forward delete"
    );
}

#[test]
fn f2_delete_in_middle_removes_byte_at_cursor() {
    // Buffer "abc" — cursor at 1 (between 'a' and 'b');
    // Delete removes 'b', leaving "ac". Cursor stays at 1.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Left));
    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
    handle_key(&mut app, key(KeyCode::Delete));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.buffer, "ac", "Delete in middle removes byte at cursor");
    assert_eq!(edit.cursor_offset, 1, "cursor stays put");
}

#[test]
fn f2_delete_at_eob_is_noop() {
    // Buffer "abc" — cursor at EOB (offset 3); Delete is
    // a no-op (no panic, no change).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    let edit = app.text_edit.as_ref().unwrap();
    let eob = edit.buffer.len();
    assert_eq!(edit.cursor_offset, eob);
    handle_key(&mut app, key(KeyCode::Delete));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.buffer, "abc", "Delete at EOB leaves buffer untouched");
    assert_eq!(edit.cursor_offset, eob, "cursor offset stays at EOB");
}

#[test]
fn f2_delete_at_empty_buffer_is_noop() {
    // Fresh edit session — empty buffer; Delete is a
    // no-op rather than a panic.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Delete));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(edit.buffer, "");
    assert_eq!(edit.cursor_offset, 0);
}

#[test]
fn f2_delete_writes_through_to_document() {
    // Same write-through contract as Backspace: the
    // document's TextObject mirrors the buffer after
    // every keystroke.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    handle_key(&mut app, key(KeyCode::Home));
    handle_key(&mut app, key(KeyCode::Delete));
    let id = "t-1".to_string();
    assert_eq!(
        app.state.text_content(&id).as_deref(),
        Some("bc"),
        "Delete writes through to the document"
    );
}

#[test]
fn f2_delete_removes_full_multibyte_char_at_cursor() {
    // Buffer "a日本b" — cursor at offset 1 (between 'a'
    // and the CJK ideograph); Delete removes all 3 bytes
    // of '日' (0xE6 0x97 0xA5), leaving "a本b". The
    // cursor stays at offset 1.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    // Insert each char individually — `text_edit_insert`
    // splices by char's UTF-8 byte length, so the offset
    // advances correctly through the multi-byte sequence.
    handle_key(&mut app, key(KeyCode::Char('日')));
    handle_key(&mut app, key(KeyCode::Char('本')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Home));
    // Step Right once to land between 'a' (1 byte) and
    // '日' (3 bytes). Right steps 1 byte at a time per
    // the byte-offset model, so we don't walk into the
    // middle of '日' here.
    handle_key(&mut app, key(KeyCode::Right));
    assert_eq!(app.text_edit.as_ref().unwrap().cursor_offset, 1);
    handle_key(&mut app, key(KeyCode::Delete));
    let edit = app.text_edit.as_ref().unwrap();
    assert_eq!(
        edit.buffer, "a本b",
        "Delete at offset 1 removes the full '日' char (3 bytes)"
    );
    assert_eq!(edit.cursor_offset, 1, "cursor stays at 1");
}

#[test]
fn f2_write_through_does_not_mark_document_dirty() {
    // The dirty flag is the user's "you have unsaved
    // changes" signal. While F2 is in flight the document
    // is being authored but not committed — we don't want
    // the title bar's `*` to flicker on every keystroke.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    handle_key(&mut app, key(KeyCode::Char('c')));
    assert!(
        !app.state.is_dirty(),
        "write-through keeps the document dirty flag clean"
    );
    // Commit flips it.
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.state.is_dirty(), "commit is what marks dirty");
}

#[test]
fn f2_esc_after_typing_reverts_document_and_closes_session() {
    // The cancel-dirty branch of `cancel_text_edit`. Live
    // write-through means the document's TextObject has
    // already been mutated on every keystroke (e.g. typing
    // "ab" makes the TextObject's content "ab" before the
    // user hits Esc). Esc must roll the content back to
    // the `initial_content` captured at F2 open, surface
    // "edit cancelled", and close the F2 session —
    // otherwise the user can't abandon a half-typed edit
    // and the write-through becomes irreversible by Esc.
    // Pins the symmetric contract "commit writes,
    // cancel reverts, dirty=false Esc is a no-op" (the
    // dirty=false arm is exercised by
    // `f2_esc_without_typing_is_noop_status` below).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('a')));
    handle_key(&mut app, key(KeyCode::Char('b')));
    // Sanity: write-through already pushed "ab" to the doc.
    let text = app
        .state
        .document
        .objects
        .iter()
        .find(|o| o.id() == "t-1")
        .expect("t-1 must still be in the document");
    if let DrawObject::Text(t) = text {
        assert_eq!(t.content, "ab", "write-through before cancel");
    } else {
        panic!("expected Text object");
    }
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.text_edit.is_none(), "Esc must close the F2 session");
    assert_eq!(app.status, "edit cancelled", "status: {}", app.status);
    // Content must be back to the initial empty string —
    // the revert helper rolled the write-through back.
    let text = app
        .state
        .document
        .objects
        .iter()
        .find(|o| o.id() == "t-1")
        .expect("t-1 must still be in the document");
    if let DrawObject::Text(t) = text {
        assert_eq!(t.content, "", "Esc must revert content to initial");
    } else {
        panic!("expected Text object");
    }
}

#[test]
fn f2_esc_without_typing_is_noop_status() {
    // The cancel-not-dirty branch: open F2, Esc without
    // typing. The buffer equals initial_content, so
    // `edit.dirty` is false and `cancel_text_edit` takes
    // the `if edit.dirty` early-out — no revert call,
    // no status echo. This is the "I changed my mind
    // before doing anything" exit and must stay silent
    // (the F2 session closing is the only feedback the
    // user needs).
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    assert!(app.text_edit.is_some());
    let status_before = app.status.clone();
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(
        app.text_edit.is_none(),
        "Esc must still close the F2 session even when not dirty"
    );
    assert_eq!(
        app.status, status_before,
        "not-dirty Esc must not change status (no echo)"
    );
}

#[test]
fn f2_shift_enter_writes_through_with_newline() {
    // Combined: Shift+Enter inserts \n into the buffer AND
    // the document, so the multi-line renderer kicks in
    // before commit.
    let mut app = make_app_with_text();
    handle_key(&mut app, key(KeyCode::F(2)));
    handle_key(&mut app, key(KeyCode::Char('x')));
    handle_key(&mut app, key_with_shift(KeyCode::Enter));
    handle_key(&mut app, key(KeyCode::Char('y')));
    let id = "t-1".to_string();
    assert_eq!(
        app.state.text_content(&id).as_deref(),
        Some("x\ny"),
        "Shift+Enter write-through carries the newline onto the doc"
    );
}
