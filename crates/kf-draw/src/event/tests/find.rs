//! Find-mode tests (Ctrl-F prompt): opening the session,
//! typing to extend the query + live match count, Enter
//! advancing / cycling / wrapping / no-match quiet no-op,
//! Esc cancelling without selecting, Backspace shrinking
//! the query.
//!
//! Pure refactor out of the single `mod tests` block; every
//! test moves verbatim. The `make_app_with_findable` helper
//! moves with the tests that use it.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;

// --- find (Ctrl-F prompt) ---------------------------------
//
// Bin tests for the in-app find feature. The pure
// `core::find::find_matches` helper has its own coverage in
// the core crate; the bin side covers the input-hijack
// pattern, the keymap arm, the status-bar render, and the
// commit semantics.

/// Build an app with three objects (Box "alpha", Box
/// "beta", Text "gamma" with content "alpha inside") so
/// find tests have something to match against. The Text
/// is the only `DrawObject` variant with searchable
/// content, so it's the one that exercises the
/// `MatchField::Content` path.
fn make_app_with_findable() -> App {
    use kf_draw_core::{
        BoxObject, BoxStyle, DrawObject, InkColor, TextBorderMode, TextObject,
    };
    let mut app = make_app();
    app.state.document.objects.push(DrawObject::Box(BoxObject {
        id: "alpha".into(),
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
        id: "beta".into(),
        z: 1,
        parent_id: None,
        color: InkColor::White,
        left: 4,
        top: 0,
        right: 6,
        bottom: 1,
        style: BoxStyle::Light,
    }));
    app.state
        .document
        .objects
        .push(DrawObject::Text(TextObject {
            id: "gamma".into(),
            z: 2,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 3,
            content: "alpha inside".into(),
            border: TextBorderMode::None,
        }));
    app
}

#[test]
fn ctrl_f_opens_find_mode_with_empty_query() {
    let mut app = make_app_with_findable();
    assert!(app.find.is_none(), "no find session yet");
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    assert!(app.find.is_some(), "Ctrl-F opens a find session");
    assert_eq!(app.find_query(), "");
    // Status echoes the "(type to search)" hint so the
    // user knows they need to type — same shape as
    // palette's empty-buffer prompt.
    assert!(app.status.contains("type to search"));
}

#[test]
fn find_mode_typing_extends_query_and_reports_match_count() {
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    // Type "al" — should match Box "alpha" (id) and Text
    // "gamma" (content "alpha inside"). 2 matches.
    for ch in "al".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    assert_eq!(app.find_query(), "al");
    assert_eq!(app.find_match_count(), 2);
    // Status line includes the live count so the user
    // can see the matches are growing before they press
    // Enter.
    assert!(app.status.contains("2 matches"));
}

#[test]
fn find_mode_enter_advances_to_next_match_and_keeps_session_open() {
    // Figma / VS Code "find next" semantics: Enter
    // advances the cursor and keeps the session open
    // so the user can keep cycling. Esc is the close
    // gesture.
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "alpha".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    // Pre-condition: find session is active, status
    // echoes the live match count, nothing is selected
    // yet (the user has only typed — the find command
    // hasn't selected anything).
    assert!(app.find.is_some());
    assert_eq!(app.state.selected_count(), 0);
    handle_key(&mut app, key(KeyCode::Enter));
    // Post-condition: session is still open (cycling
    // keeps it open), the first match is now selected
    // (Box "alpha" — id substring match), status
    // reports the index "1/N".
    assert!(app.find.is_some(), "Enter keeps the session open");
    assert_eq!(app.state.selected_count(), 1);
    assert!(app.status.contains("1/"));
    assert!(app.status.contains("alpha"));
}

#[test]
fn find_mode_enter_cycles_to_next_match() {
    // "alpha" matches in two places: Box "alpha" (id) and
    // Text "gamma" (content "alpha inside"). First Enter
    // shows 1/2 (Box alpha, id field); second Enter
    // shows 2/2 (Text gamma, content field).
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "alpha".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    assert_eq!(app.find_match_count(), 2);
    handle_key(&mut app, key(KeyCode::Enter));
    // First match selected — Box "alpha" on id.
    assert!(app.status.contains("1/2"));
    assert!(app.status.contains("alpha"));
    assert!(app.status.contains("on id"));
    handle_key(&mut app, key(KeyCode::Enter));
    // Cycled to second match — Text "gamma" on content.
    assert!(app.status.contains("2/2"));
    assert!(app.status.contains("gamma"));
    assert!(app.status.contains("on content"));
    // Session still open — a third Enter would wrap.
    assert!(app.find.is_some());
}

#[test]
fn find_mode_enter_wraps_around_at_end() {
    // After the last match, Enter wraps to the first.
    // Two-match query ("alpha"); press Enter three
    // times: 1/2 (alpha) → 2/2 (gamma) → wrap → 1/2
    // (alpha again).
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "alpha".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    handle_key(&mut app, key(KeyCode::Enter)); // 1/2
    handle_key(&mut app, key(KeyCode::Enter)); // 2/2
    handle_key(&mut app, key(KeyCode::Enter)); // wrap → 1/2
    assert!(app.status.contains("1/2"));
    assert!(app.status.contains("alpha"));
}

#[test]
fn find_mode_enter_with_no_match_is_a_quiet_no_op() {
    // With zero matches, Enter is a no-op: the
    // session stays open so the user can backspace
    // and broaden the search without re-pressing
    // Ctrl-F. (The "no matches" status from
    // refresh_find_status is what the user sees
    // here — Enter doesn't need to add a duplicate
    // message.)
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "xyzzy".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    assert_eq!(app.find_match_count(), 0);
    let pre_status = app.status.clone();
    handle_key(&mut app, key(KeyCode::Enter));
    // Session still open, nothing selected, status
    // unchanged from the typed "(no matches)"
    // message.
    assert!(app.find.is_some(), "Enter on no-match keeps session open");
    assert_eq!(app.state.selected_count(), 0);
    assert_eq!(app.status, pre_status, "Enter is silent on no-match");
}

#[test]
fn find_mode_esc_cancels_without_selecting() {
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "alpha".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    assert!(app.find.is_some());
    handle_key(&mut app, key(KeyCode::Esc));
    // Esc must not leave a dangling session and must
    // not mutate the selection (the user changed their
    // mind).
    assert!(app.find.is_none());
    assert_eq!(app.state.selected_count(), 0);
}

#[test]
fn find_mode_backspace_shrinks_query() {
    let mut app = make_app_with_findable();
    handle_key(&mut app, key_ctrl(KeyCode::Char('f')));
    for ch in "abc".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    assert_eq!(app.find_query(), "abc");
    // Backspace pops one char; matches re-compute
    // against the shorter query.
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.find_query(), "ab");
    // Backspace on empty query is a quiet no-op (does
    // not close the session — the user is still
    // composing).
    handle_key(&mut app, key(KeyCode::Backspace));
    handle_key(&mut app, key(KeyCode::Backspace));
    assert_eq!(app.find_query(), "");
    assert!(app.find.is_some(), "still in find mode");
}
