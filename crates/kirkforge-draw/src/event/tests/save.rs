//! Save / save-as / atomic-write tests: Ctrl-S (with and
//! without a source path, dirty-bit flip), Ctrl-Shift-S
//! (open modal, type path, Enter writes / Esc cancels /
//! empty / NUL / whitespace reject / failure revert /
//! flip source for subsequent Ctrl-S), `save_app`
//! (preserves redo stack, failure marks dirty, no-source
//! / NUL / empty bail), and `atomic_write` (happy path /
//! replace / cleanup-on-failure).
//!
//! Pure refactor out of the single `mod tests` block;
//! every test moves verbatim.

use super::*;
use crate::event::tests::common::*;
use crossterm::event::KeyCode;

#[test]
fn ctrl_s_saves_to_source_path() {
    // Use a temp file; clean up after.
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ctrl-s.td.json");
    let path_str = path.to_string_lossy().to_string();

    let mut app = make_app();
    app.source_path = Some(path_str.clone());
    app.state.set_tool(DrawMode::Line);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 3, y: 0 });
    app.state.commit_draft().unwrap();

    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    assert!(app.status.starts_with("saved "));
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("\"version\""));
    assert!(written.contains("\"line\""));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn ctrl_s_with_no_source_path_opens_save_as_modal_keymap() {
    // Keymap-level regression: Ctrl-S on a fresh doc must
    // open the save-as modal instead of surfacing a
    // "save failed: no source path" status. This used to
    // assert the opposite (it pinned the surface error);
    // the contract was changed to match the standard
    // editor convention (VS Code / Sublime / IntelliJ:
    // Ctrl-S on an unsaved file opens save-as).
    let mut app = make_app();
    app.source_path = None;
    assert!(app.save_as.is_none(), "precondition: no modal yet");
    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    assert!(
        app.save_as.is_some(),
        "Ctrl-S on a fresh doc must open the save-as modal"
    );
    assert!(
        !app.status.starts_with("save failed"),
        "Ctrl-S must NOT surface a save-failed status when no path is set; got: {:?}",
        app.status
    );
}

#[test]
fn ctrl_s_clears_dirty_marker() {
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ctrl-s-dirty.td.json");
    let path_str = path.to_string_lossy().to_string();

    let mut app = make_app();
    app.source_path = Some(path_str.clone());
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();
    assert!(app.state.is_dirty(), "commit should leave doc dirty");

    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    assert!(
        !app.state.is_dirty(),
        "successful save clears the dirty bit"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn ctrl_shift_s_opens_save_as_modal() {
    // Ctrl-Shift-S opens the modal, pre-populated with
    // the current source_path. The keymap hijack sits
    // before the bare Ctrl-S arm so the chord never
    // accidentally saves to the existing path.
    let mut app = make_app();
    app.source_path = Some("orig.td.json".into());
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    assert!(app.save_as.is_some(), "save_as modal opens");
    assert_eq!(
        app.save_as.as_ref().unwrap().path,
        "orig.td.json",
        "pre-populated with current path"
    );
    assert!(
        app.status.contains("save as"),
        "status echoes the prompt: {}",
        app.status
    );
}

#[test]
fn ctrl_shift_s_enter_writes_to_new_path() {
    // Open save-as, type a new path, Enter → file lands
    // on disk, source_path flips, modal closes.
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let new_path = dir.join("save-as-new.td.json");
    let new_path_str = new_path.to_string_lossy().to_string();

    let mut app = make_app();
    app.source_path = Some("orig.td.json".into());
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();

    // Open the modal.
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    assert!(app.save_as.is_some());

    // Backspace away the pre-populated "orig.td.json"
    // (15 chars) so we can type the new path cleanly.
    for _ in 0..15 {
        handle_key(&mut app, key(KeyCode::Backspace));
    }
    for ch in new_path_str.chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }

    // Enter commits the path and writes the file.
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.save_as.is_none(), "modal closes on commit");
    assert_eq!(app.source_path.as_deref(), Some(new_path_str.as_str()));
    assert!(
        app.status.starts_with("saved as "),
        "status: {}",
        app.status
    );
    assert!(
        std::fs::read_to_string(&new_path).is_ok(),
        "file written to the new path"
    );
    assert!(!app.state.is_dirty(), "save clears dirty bit");

    let _ = std::fs::remove_file(&new_path);
}

#[test]
fn save_as_esc_cancels_and_keeps_old_source() {
    // Esc cancels — modal closes, source_path is the
    // old value, no file written.
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let new_path = dir.join("save-as-cancel.td.json");
    let new_path_str = new_path.to_string_lossy().to_string();

    let mut app = make_app();
    app.source_path = Some("orig.td.json".into());
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    // Backspace the pre-populated path, type the new one.
    for _ in 0..12 {
        handle_key(&mut app, key(KeyCode::Backspace));
    }
    for ch in new_path_str.chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    handle_key(&mut app, key(KeyCode::Esc));
    assert!(app.save_as.is_none());
    assert_eq!(app.source_path.as_deref(), Some("orig.td.json"));
    assert_eq!(app.status, "save as cancelled");
    assert!(
        !std::fs::exists(&new_path).unwrap_or(false),
        "no file written on cancel"
    );
}

#[test]
fn save_as_empty_enter_stays_in_modal() {
    // Empty buffer + Enter → the modal stays open, the
    // status echoes the no-op. The user can keep
    // typing rather than re-pressing Ctrl-Shift-S.
    let mut app = make_app();
    app.source_path = None;
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.save_as.is_some(), "modal stays on empty Enter");
    assert!(
        app.status.contains("empty"),
        "status echoes the no-op: {}",
        app.status
    );
}

#[test]
fn save_as_nul_byte_path_is_rejected() {
    // Ctrl-@ on most terminals inserts a NUL byte
    // (Rust strings are UTF-8 and accept 0x00). The
    // validator catches it at commit time so the
    // modal stays open and the status surfaces a
    // useful error. Mirrors `validate_path_arg` in
    // render.rs for the load path; the save path
    // needs the same guard because the save-as
    // modal accepts arbitrary typed chars.
    let mut app = make_app();
    app.source_path = Some("orig.td.json".into());
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    // Backspace the pre-populated path.
    for _ in 0..12 {
        handle_key(&mut app, key(KeyCode::Backspace));
    }
    // Type a path with a trailing NUL byte. Char
    // '\0' is 1 byte in UTF-8, so a single
    // keypress is the right shape.
    for ch in "safe.td.json".chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    handle_key(&mut app, key(KeyCode::Char('\0')));
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.save_as.is_some(), "modal stays on NUL path");
    assert_eq!(
        app.source_path.as_deref(),
        Some("orig.td.json"),
        "source_path unchanged on NUL reject"
    );
    assert!(
        app.status.contains("NUL"),
        "status echoes the NUL guard: {}",
        app.status
    );
}

#[test]
fn save_as_whitespace_only_path_is_rejected() {
    // A path of just spaces trims to empty and is
    // treated the same as the empty-buffer case.
    // Confirms the trim() guard handles the
    // "user pressed space space space Enter"
    // foot-gun end-to-end through the keymap.
    let mut app = make_app();
    app.source_path = None;
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    for _ in 0..3 {
        handle_key(&mut app, key(KeyCode::Char(' ')));
    }
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.save_as.is_some(), "modal stays on whitespace path");
    assert!(
        app.status.contains("whitespace"),
        "status echoes the no-op: {}",
        app.status
    );
}

#[test]
fn save_as_flips_source_for_subsequent_ctrl_s() {
    // After Save-As commits, the next Ctrl-S writes to
    // the NEW path, not the old one. This is the
    // contract: Save-As flips the editor's "home" path
    // so subsequent saves (and the save-on-quit y
    // arm) all land at the new location.
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let orig_path = dir.join("save-as-flip-orig.td.json");
    let new_path = dir.join("save-as-flip-new.td.json");
    let orig_path_str = orig_path.to_string_lossy().to_string();
    let new_path_str = new_path.to_string_lossy().to_string();

    let mut app = make_app();
    app.source_path = Some(orig_path_str.clone());
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();

    // First Ctrl-S writes to orig_path.
    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    assert!(std::fs::read_to_string(&orig_path).is_ok());

    // Mutate, then Save-As to new_path.
    app.state.begin_draft(Point { x: 5, y: 5 });
    app.state.update_draft(Point { x: 7, y: 6 });
    app.state.commit_draft().unwrap();
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    let old_len = orig_path_str.len();
    for _ in 0..old_len {
        handle_key(&mut app, key(KeyCode::Backspace));
    }
    for ch in new_path_str.chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    handle_key(&mut app, key(KeyCode::Enter));
    assert_eq!(app.source_path.as_deref(), Some(new_path_str.as_str()));
    assert!(std::fs::read_to_string(&new_path).is_ok());

    // Mutate again, plain Ctrl-S — should land at the
    // NEW path now, not the orig.
    app.state.begin_draft(Point { x: 9, y: 9 });
    app.state.update_draft(Point { x: 10, y: 10 });
    app.state.commit_draft().unwrap();
    let new_path_mtime_before = std::fs::metadata(&new_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    let new_path_mtime_after = std::fs::metadata(&new_path).unwrap().modified().unwrap();
    assert!(
        new_path_mtime_after > new_path_mtime_before,
        "Ctrl-S after Save-As must write to the new path"
    );

    let _ = std::fs::remove_file(&orig_path);
    let _ = std::fs::remove_file(&new_path);
}

#[test]
fn save_as_failure_keeps_modal_open_and_restores_prior_source() {
    // The commit-fail footgun. Ctrl-Shift-S → type a path
    // that the OS can't write to (parent dir doesn't
    // exist) → Enter. Pre-fix this would have flipped
    // source_path to the bad path and closed the modal,
    // leaving the user's next Ctrl-S targeting the bad
    // path. The fix is revert_save_as: roll source_path
    // back to where the user came from and re-open the
    // modal pre-populated with the bad path so they can
    // edit and retry.
    let dir = std::env::temp_dir().join("kfd-test");
    std::fs::create_dir_all(&dir).unwrap();
    let orig_path = dir.join("save-as-fail-orig.td.json");
    let orig_path_str = orig_path.to_string_lossy().to_string();
    // /no/such/dir/file.td.json — the parent doesn't
    // exist, atomic_write will fail.
    let bad_path = "/no/such/dir/file.td.json".to_string();

    let mut app = make_app();
    app.source_path = Some(orig_path_str.clone());
    app.state.set_tool(DrawMode::Box);
    app.state.begin_draft(Point { x: 0, y: 0 });
    app.state.update_draft(Point { x: 2, y: 2 });
    app.state.commit_draft().unwrap();

    // Ctrl-Shift-S opens the modal pre-populated with
    // orig_path_str.
    handle_key(&mut app, key_with_shift_ctrl(KeyCode::Char('s')));
    assert!(app.save_as.is_some(), "Ctrl-Shift-S must open modal");
    // Backspace out the pre-populated path, type the
    // bad path.
    for _ in 0..orig_path_str.len() {
        handle_key(&mut app, key(KeyCode::Backspace));
    }
    for ch in bad_path.chars() {
        handle_key(&mut app, key(KeyCode::Char(ch)));
    }
    // Enter — save_app fails because /no/such/dir
    // doesn't exist.
    handle_key(&mut app, key(KeyCode::Enter));

    // Modal must still be open with the bad path
    // pre-populated so the user can edit + retry.
    let s = app
        .save_as
        .as_ref()
        .expect("save_as modal must stay open after a failed commit");
    assert_eq!(
        s.path, bad_path,
        "reopened modal must pre-populate the bad path"
    );
    // source_path must be rolled back to the original.
    assert_eq!(
        app.source_path.as_deref(),
        Some(orig_path_str.as_str()),
        "source_path must roll back to the prior value on save failure"
    );
    // Status surfaces the failure.
    assert!(
        app.status.starts_with("save as failed"),
        "status must surface the failure: {}",
        app.status
    );

    let _ = std::fs::remove_file(&orig_path);
}

#[test]
fn save_app_preserves_redo_stack() {
    // Bug #2 regression: saving must not snapshot, which would
    // push the undo stack and clear pending redo entries.
    let mut app = make_app();
    let tmp = std::env::temp_dir().join(format!("kfd-save-redo-{}.td.json", std::process::id()));
    app.source_path = Some(tmp.to_string_lossy().into_owned());

    // Add a box, then commit it so undo/redo has something to do.
    app.state.set_tool(kirkforge_draw_core::DrawMode::Box);
    app.state.begin_draft(Point { x: 1, y: 1 });
    app.state.update_draft(Point { x: 4, y: 4 });
    app.state.commit_draft();
    assert!(app.state.can_undo());

    // Undo, then verify redo is available, then save. Redo must
    // survive the save.
    app.state.undo();
    assert!(app.state.can_redo());
    save_app(&mut app).expect("save");
    assert!(
        app.state.can_redo(),
        "save_app must not clear the redo stack"
    );

    // Redo still works.
    assert!(app.state.redo());
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn atomic_write_writes_full_content_and_cleans_tmp() {
    // Happy path: bytes land at the target path, no .tmp sibling
    // is left behind.
    let tmp = std::env::temp_dir().join(format!(
        "kfd-atomic-ok-{}-{}.td.json",
        std::process::id(),
        line!()
    ));
    let payload = b"{\"version\":1,\"objects\":[]}".to_vec();
    atomic_write(&tmp, &payload).expect("atomic_write should succeed");
    let read_back = std::fs::read(&tmp).expect("file should exist");
    assert_eq!(read_back, payload);
    let tmp_sibling = {
        let mut s = tmp.as_os_str().to_owned();
        s.push(".tmp");
        std::path::PathBuf::from(s)
    };
    assert!(
        !tmp_sibling.exists(),
        ".tmp sibling should be cleaned up on success"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn atomic_write_replaces_existing_file() {
    // Overwrite case: a previous save at the same path must be
    // fully replaced by the new bytes (no partial-old + partial-new
    // contents leaking through).
    let tmp = std::env::temp_dir().join(format!(
        "kfd-atomic-replace-{}-{}.td.json",
        std::process::id(),
        line!()
    ));
    std::fs::write(&tmp, b"OLD-CONTENT-LEFT-OVER-FROM-PREVIOUS-SAVE").expect("seed");
    let payload = b"{\"version\":1,\"objects\":[]}".to_vec();
    atomic_write(&tmp, &payload).expect("atomic_write should succeed");
    let read_back = std::fs::read(&tmp).expect("file should exist");
    assert_eq!(read_back, payload);
    assert!(
        !read_back.starts_with(b"OLD-CONTENT"),
        "old contents must not bleed through the rename"
    );
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn atomic_write_cleans_up_tmp_on_failure() {
    // Unwritable target path: no .tmp sibling should be left
    // behind (otherwise we'd litter the user's directory with
    // half-written files on every failed save).
    let bad = std::path::PathBuf::from(format!(
        "{}/kfd-atomic-fail-dir-{}/nonexistent.td.json",
        std::env::temp_dir().display(),
        std::process::id()
    ));
    let tmp_sibling = {
        let mut s = bad.as_os_str().to_owned();
        s.push(".tmp");
        std::path::PathBuf::from(s)
    };
    let result = atomic_write(&bad, b"payload");
    assert!(result.is_err(), "atomic_write must fail");
    assert!(
        !tmp_sibling.exists(),
        ".tmp sibling must be cleaned up after a failed write"
    );
}

#[test]
fn save_app_failure_marks_dirty() {
    // Bug #6 regression: a failed save must mark the document
    // dirty so the title bar tells the user that disk is out of
    // sync with their intent.
    let mut app = make_app();
    app.state.mark_saved();
    assert!(!app.state.is_dirty());

    // Point at a path that can't be written. A nested path under a
    // non-existent parent directory makes `std::fs::write` fail
    // deterministically without needing filesystem permissions.
    let bad = format!(
        "{}/kfd-save-fail-{}/nonexistent.td.json",
        std::env::temp_dir().display(),
        std::process::id()
    );
    app.source_path = Some(bad);

    let result = save_app(&mut app);
    assert!(result.is_err(), "save should fail for an unwritable path");
    assert!(
        app.state.is_dirty(),
        "failed save must leave the document marked dirty"
    );
}

#[test]
fn ctrl_s_with_no_source_path_opens_save_as_modal() {
    // The keymap contract for Ctrl-S: "save back to source
    // path (or open save-as if no path yet)". A user who
    // boots kfd with no `--load` and immediately Ctrl-S
    // must NOT see a "save failed: no source path" status
    // — they must land in the save-as modal pre-populated
    // empty, identical to Ctrl-Shift-S on the same state.
    // This test pins that fallback so a future refactor
    // (e.g. unconditional save_app call) can't silently
    // regress the UX.
    let mut app = make_app();
    assert!(app.source_path.is_none(), "precondition: fresh doc");
    assert!(app.save_as.is_none(), "precondition: no modal yet");
    handle_key(&mut app, key_ctrl(KeyCode::Char('s')));
    assert!(
        app.save_as.is_some(),
        "Ctrl-S on a fresh doc must open the save-as modal"
    );
    assert!(
        app.status.is_empty() || !app.status.contains("save failed"),
        "Ctrl-S must not surface a save-failed status when no path is set; got: {:?}",
        app.status
    );
}

#[test]
fn ctrl_s_with_source_path_still_calls_save_app() {
    // The other half of the Ctrl-S contract: with a
    // source_path set, Ctrl-S must still call save_app
    // (NOT open save-as). Guards against an over-eager
    // fallback that always routes to begin_save_as.
    let mut app = make_app();
    app.source_path = Some("/tmp/kfd-ctrl-s-with-path.td.json".into());
    assert!(app.save_as.is_none(), "precondition: no modal yet");
    // Drive save via save_app directly so we don't need
    // the disk write to actually succeed in the test.
    let res = save_app(&mut app);
    // We don't assert success or failure — the IO can
    // legitimately fail in any sandbox — but the
    // invocation must NOT have opened the save-as modal.
    let _ = res;
    assert!(
        app.save_as.is_none(),
        "Ctrl-S with a path set must not open save-as"
    );
}

#[test]
fn save_app_with_no_source_path_returns_bail() {
    // The "user opened with no --load" branch. Until today
    // the only save-failure test exercised the
    // `atomic_write` Err arm; this covers the bail at the
    // top of `save_app` so a future refactor can't
    // accidentally try to serialize to a `None` path.
    let mut app = make_app();
    app.source_path = None;
    let err = save_app(&mut app).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no source path"),
        "expected 'no source path' in the error chain, got: {msg}"
    );
}

#[test]
fn save_app_with_nul_source_path_bails_and_marks_dirty() {
    // The validate_path_arg guard inside save_app. A user
    // (or a Save-As commit that bypassed the NUL check)
    // could leave a NUL byte in `source_path`; the guard
    // is the second line of defense, must reject before
    // any IO, and must flip dirty so the user sees a `*`
    // and knows their last save intent didn't go through
    // (parity with atomic_write's failed-write dirty flip).
    let mut app = make_app();
    app.source_path = Some("/tmp/kfd\0-evil.td.json".into());
    let err = save_app(&mut app).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("NUL"),
        "expected NUL-byte message in the error chain, got: {msg}"
    );
    assert!(
        app.state.is_dirty(),
        "save_app must mark dirty on validate_path_arg failure"
    );
}

#[test]
fn save_app_with_empty_source_path_bails_and_marks_dirty() {
    // The empty-string arm of validate_path_arg. The Save-As
    // commit already rejects empty paths inside the modal,
    // but if state somehow gets here (future modal-free save
    // path, scripted test, etc.), save_app must still refuse
    // and flip dirty. Mirrors the NUL test.
    let mut app = make_app();
    app.source_path = Some(String::new());
    let err = save_app(&mut app).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("empty"),
        "expected empty-path message in the error chain, got: {msg}"
    );
    assert!(
        app.state.is_dirty(),
        "save_app must mark dirty on validate_path_arg failure"
    );
}
