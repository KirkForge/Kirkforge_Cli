//! Editor app: the bridge between `DrawState` and the TUI pane.
//!
//! `App` holds the `DrawState`, the viewport rect (set each frame by
//! `ui::draw`), scroll offsets, the scene origin (so mouse events
//! can be mapped to document points), and the should-quit flag.

use kirkforge_draw_core::{text_util, DrawState, Point};
use ratatui::layout::Rect;

/// Top-level app state. Cheap to clone of `DrawState`-derived data
/// already; the struct itself is kept small so the render loop can
/// inspect it on every frame without ceremony.
pub struct App {
    pub state: DrawState,
    pub source_path: Option<String>,
    pub status: String,
    pub should_quit: bool,
    /// True between a quit attempt on a dirty document and the
    /// user's `y` / `n` / `Esc` reply. Set by `request_quit`
    /// when the document is dirty and no modal (F2, palette,
    /// find) is active; cleared by `quit_confirm_yes`,
    /// `quit_confirm_no`, or `quit_confirm_cancel`. The status
    /// bar echoes the prompt for the duration.
    pub pending_quit_confirm: bool,
    /// Scene-cell coordinates of the viewport's top-left.
    pub scroll_x: i32,
    pub scroll_y: i32,
    /// Body pane rect in terminal coordinates. Set each frame by the
    /// renderer; the input handler reads it to map mouse events.
    pub body_area: Rect,
    /// Layers panel rect in terminal coordinates, when the panel
    /// is showing (`show_layers`). `None` while the panel is hidden.
    /// The input handler reads it to route layer-area clicks.
    /// ponytail: mirrors `body_area` — same per-frame write,
    /// same per-event read. Kept separate so the body-pane
    /// hit-tests don't accidentally swallow clicks on the panel.
    pub layers_area: Option<Rect>,
    /// Inspector panel rect in terminal coordinates, when the
    /// panel is showing (`show_inspector`). `None` while the
    /// panel is hidden. The input handler reads it to route
    /// inspector-area clicks.
    /// ponytail: mirrors `layers_area` — same per-frame write,
    /// same per-event read. Kept separate so the body-pane
    /// hit-tests don't accidentally swallow clicks on the panel.
    pub inspector_area: Option<Rect>,
    /// Scene origin (top-left document point of the composed scene).
    /// Updated each frame by the renderer; `None` when the document
    /// is empty.
    pub scene_origin: Option<Point>,
    /// Whether mouse capture is enabled. We opt in on first mouse
    /// event and leave it on for the rest of the session.
    pub mouse_captured: bool,
    /// When true, the renderer draws a centered key-map overlay on
    /// top of the body pane. Toggled by `?`.
    pub show_help: bool,
    /// When true, the renderer draws the layers panel on the right
    /// side of the body. Toggled by `L`. The panel shrinks the
    /// body pane to fit; mouse events are routed to the smaller
    /// body area so the bin's hit-tests don't pick up clicks on
    /// the panel.
    pub show_layers: bool,
    /// When true, the renderer draws the properties inspector
    /// panel on the right side of the body (next to or instead of
    /// the layers panel). Toggled by `I`. Same hit-test discipline
    /// as `show_layers`: the body pane shrinks so clicks on the
    /// panel don't fall through to body hit-tests.
    pub show_inspector: bool,
    /// When `Some`, the editor is in text-entry mode: keystrokes
    /// mutate `buffer` instead of being routed to the normal key
    /// handlers, and the renderer draws the buffer at the target
    /// Text object's location. Enter commits, Esc cancels.
    pub text_edit: Option<TextEditState>,
    /// Live marquee drag in Select tool. `Some` between Left-Down
    /// (anchor) and Left-Up (commit) when the user drags in empty
    /// space — the renderer draws the dotted rect each frame, and
    /// the commit goes through `DrawState::select_in_rect` with
    /// `mode` chosen by the modifier the drag started with:
    /// `Replace` (bare), `Add` (Shift), `Toggle` (Ctrl).
    pub marquee: Option<MarqueeState>,
    /// Active command-palette session. `Some` between the user
    /// typing `:` (or `/`) and either committing on Enter or
    /// discarding on Esc. While `Some`, printable keystrokes
    /// append to `buffer`, the renderer widens the status bar
    /// to show the prompt + filtered match list, and the
    /// normal key handlers are bypassed (same input-hijack
    /// pattern as `text_edit`).
    pub palette: Option<PaletteState>,
    /// Currently focused row in the layers panel (0 = topmost).
    /// `None` means "no focus" — the panel still shows the list
    /// but the user isn't navigating it. While `show_layers` is
    /// true, Up/Down keys move this focus, Enter selects the
    /// focused layer, and Esc clears it back to `None`. When
    /// the layers panel is hidden this field is ignored.
    ///
    /// ponytail: kept as `Option<usize>` rather than a
    /// `bool + usize` tuple because the only state the
    /// renderer needs to know is "should I draw a cursor on
    /// row N?" — that maps cleanly onto the option.
    pub layer_focus: Option<usize>,
    /// Active find session. `Some` between the user pressing
    /// `Ctrl-F` and committing on Enter / cancelling on Esc.
    /// `query` is the working buffer (empty while the user
    /// hasn't typed yet); `matches` is the result of
    /// `find_matches` re-run on every keystroke so the status
    /// bar can show "(N matches)" without re-scanning the
    /// document; `index` is the cursor into `matches` for the
    /// Enter-to-cycle path (today the first match is
    /// selected on Enter; cycling is the next tick).
    ///
    /// ponytail: matches are cached on the App instead of
    /// re-computed on every render frame. The pure helper is
    /// cheap but a 100-object document with a 5-char query
    /// at 60fps is 30k `to_lowercase` calls per second — a
    /// pointless budget when the matches only change on a
    /// typed char. The cache invalidates on insert /
    /// backspace.
    pub find: Option<FindState>,
    /// Active Save-As session. `Some` between the user pressing
    /// Ctrl-Shift-S and the Enter / Esc close. The path buffer
    /// is the new file path the user is composing; on commit
    /// the App's `source_path` flips to it and the bin calls
    /// `save_app`. Same "modal hijack" pattern as `find` and
    /// `palette` — the bin routes printable chars + Backspace +
    /// Enter + Esc to the modal's helpers while it's open.
    pub save_as: Option<SaveAsState>,
}

/// Live Save-As session: the path buffer the user is
/// composing. Pre-populated with the current `source_path`
/// (if any) so the user can edit-in-place rather than retype
/// from scratch.
pub struct SaveAsState {
    pub path: String,
}

/// Live find session: working query, cached match list, and a
/// cursor into that list for the cycling Enter path. All three
/// fields are private to the App's find helpers — the renderer
/// reads through `find_query()` / `find_match_count()` so it
/// never has to reach into the struct.
pub struct FindState {
    pub query: String,
    pub matches: Vec<kirkforge_draw_core::TextMatch>,
    pub index: usize,
}

/// Live marquee drag anchor + current point + the mode the drag
/// committed with. Both endpoints are in document coordinates (the
/// result of `screen_to_doc`). The actual rect is normalized from
/// `anchor` / `current` at render + commit time so the user can
/// drag in any direction.
pub struct MarqueeState {
    pub anchor: Point,
    pub current: Point,
    pub mode: kirkforge_draw_core::SelectionMode,
}

/// State for an in-progress command-palette session. `trigger`
/// records which key opened the palette (`:` vs `/`) — the bin
/// uses it as the prefix char in the status-bar prompt so the
/// user can see how to dismiss and re-open. `buffer` is the
/// in-progress query; `dirty` flips after the first typed char
/// so the status bar can show "(no matches)" only when the
/// filter actually returns nothing.
pub struct PaletteState {
    pub trigger: PaletteTrigger,
    pub buffer: String,
    pub dirty: bool,
}

/// Which key opened the palette. The two triggers look identical
/// to the user today (both start with the same prompt and accept
/// the same input) but are recorded separately so a future
/// re-purposing — e.g., `/` filters to model-emitted
/// diagrams only — can split the UX without rewriting the
/// trigger-detection code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteTrigger {
    Colon,
    Slash,
}

/// State for an in-progress text edit session. `target_id` is the
/// TextObject being edited; `buffer` is the working copy of its
/// content (write-through-mirrored to the document on every
/// keystroke); `initial_content` is the pre-edit snapshot used
/// by commit (push-undo anchor) and cancel (revert anchor).
/// `cursor_offset` is the byte index into `buffer` where the next
/// insert / delete happens (and where the visible cursor paints).
pub struct TextEditState {
    pub target_id: String,
    pub buffer: String,
    /// Pre-edit content captured at `begin_text_edit`. Commit
    /// uses it as the undo snapshot so Ctrl-Z rolls back to what
    /// the user had before F2. Cancel uses it as the revert
    /// target so Esc leaves the doc untouched.
    pub initial_content: String,
    /// True if the buffer is dirty relative to the document. Flips
    /// false on commit or cancel so the status line doesn't keep
    /// nagging.
    pub dirty: bool,
    /// Byte offset into `buffer` for the next insert / delete.
    /// 0 = before the first char; `buffer.len()` = end of buffer.
    /// ponytail: byte index, not grapheme. ASCII inserts and
    /// deletes are 1-byte splices. Multi-byte graphemes (CJK,
    /// emoji) are 3–4 bytes each — a Left/Right arrow press
    /// steps the offset by 1 byte, which lands mid-grapheme
    /// until the cursor has walked out of the multi-byte span.
    /// Visually this is fine (the cursor paints at the end of
    /// the previous grapheme); grapheme-aware stepping is a
    /// future tick.
    pub cursor_offset: usize,
}

impl TextEditState {
    pub fn new(target_id: String, initial: String) -> Self {
        let end = initial.len();
        Self {
            target_id,
            buffer: initial.clone(),
            initial_content: initial,
            dirty: false,
            cursor_offset: end,
        }
    }
}

impl App {
    pub fn new(state: DrawState) -> Self {
        Self {
            state,
            source_path: None,
            status: "kfd — press q or Ctrl-C to quit".into(),
            should_quit: false,
            scroll_x: 0,
            scroll_y: 0,
            body_area: Rect::default(),
            layers_area: None,
            inspector_area: None,
            scene_origin: None,
            mouse_captured: false,
            show_help: false,
            text_edit: None,
            marquee: None,
            palette: None,
            layer_focus: None,
            find: None,
            save_as: None,
            show_layers: false,
            show_inspector: false,
            pending_quit_confirm: false,
        }
    }

    pub fn with_source(mut self, path: impl Into<String>) -> Self {
        self.source_path = Some(path.into());
        self
    }

    pub fn request_quit(&mut self) {
        // If the document is dirty and no modal is open, ask
        // first — silent data loss on quit is a real foot-gun.
        // A modal (F2 text edit, command palette, find) is
        // already a "focus capture" the user is in; surfacing a
        // quit confirm on top would just stack the prompts.
        if self.state.is_dirty()
            && self.text_edit.is_none()
            && self.palette.is_none()
            && self.find.is_none()
        {
            self.pending_quit_confirm = true;
            self.status = "unsaved changes — save? (y/n/Esc)".into();
            return;
        }
        self.should_quit = true;
    }

    /// Confirm quit by discarding unsaved changes. Always
    /// succeeds; the user said "no" so we honor it.
    pub fn quit_confirm_no(&mut self) {
        self.pending_quit_confirm = false;
        self.should_quit = true;
    }

    /// Cancel the quit prompt and stay in the editor. Clears
    /// the flag and the status line so the user can keep
    /// working without the prompt lingering in the status.
    pub fn quit_confirm_cancel(&mut self) {
        self.pending_quit_confirm = false;
        self.status = "quit cancelled".into();
    }

    /// Flip the help-overlay visibility. `?` key in the event loop.
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Flip the layers-panel visibility. `L` key in the event loop.
    /// Also resets the layer focus on close — the focus row is
    /// meaningless when the panel is hidden, and a stale focus
    /// would re-surface if the user reopened the panel.
    pub fn toggle_layers(&mut self) {
        self.show_layers = !self.show_layers;
        if !self.show_layers {
            self.layer_focus = None;
        }
    }

    /// Flip the inspector-panel visibility. `I` key in the
    /// event loop. The inspector has no per-row focus state
    /// (the panel is a read-only summary view), so nothing
    /// needs to reset on close — opening it on an empty /
    /// multi selection shows the placeholder line.
    pub fn toggle_inspector(&mut self) {
        self.show_inspector = !self.show_inspector;
    }

    /// Begin editing a single-selected Text object. No-op when the
    /// selection isn't a Text. The buffer is seeded from the object's
    /// current content; nothing is written back to the document
    /// until `commit_text_edit` runs.
    pub fn begin_text_edit(&mut self) -> bool {
        if self.text_edit.is_some() {
            return false;
        }
        if self.state.selected_count() != 1 {
            return false;
        }
        // selected() returns refs to the selected DrawObjects. We
        // need the id of the single one and its content. Two-step:
        // get the id, then look up content via the public
        // text_content() helper.
        let Some(obj) = self.state.selected().into_iter().next() else {
            return false;
        };
        let id = obj.id().to_string();
        let Some(initial) = self.state.text_content(&id) else {
            return false;
        };
        self.text_edit = Some(TextEditState::new(id, initial));
        true
    }

    /// Stamp the current text-edit buffer onto the document's
    /// TextObject and flip `edit.dirty`. Called from every
    /// keystroke handler (insert / backspace / delete) — keep
    /// the contract in one place so the three callers can't drift
    /// on what counts as "modified".
    ///
    /// ponytail: three identical 3-line endings (dirty + clone
    /// id + write_through) had inlined into each helper. Pulled
    /// to a private so the helpers now read as "do the splice,
    /// then sync to doc." No-op when no edit session is active
    /// (defensive — callers already early-return on `None`, but
    /// keeping this idempotent costs nothing).
    fn sync_text_edit_buffer(&mut self) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.dirty = true;
            let id = edit.target_id.clone();
            self.state.write_text_content(&id, &edit.buffer);
        }
    }

    /// Push a printable character into the edit buffer at the
    /// current `cursor_offset`. Caller handles Enter / Backspace /
    /// Esc — this helper only adds non-control chars.
    ///
    /// Write-through: after updating the buffer, also stamp the
    /// new content onto the document's TextObject so the
    /// rendered scene reflects the buffer live. The document
    /// `dirty` flag stays clean until commit — write-through
    /// mutates the field but doesn't push undo or flip dirty.
    /// Both side effects belong to `commit_text_edit`.
    pub fn text_edit_insert(&mut self, ch: char) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        // Splice at cursor_offset (clamped so a misbehaving
        // caller can't OOB); then advance the offset by the
        // char's UTF-8 byte length.
        let off = edit.cursor_offset.min(edit.buffer.len());
        let mut buf = std::mem::take(&mut edit.buffer);
        buf.insert(off, ch);
        edit.buffer = buf;
        edit.cursor_offset = off + ch.len_utf8();
        self.sync_text_edit_buffer();
    }

    /// Remove the byte before `cursor_offset` from the edit
    /// buffer (Backspace). At offset 0 the buffer is unchanged
    /// (a no-op rather than a panic). Same write-through
    /// contract as `text_edit_insert`: the document's TextObject
    /// mirrors the buffer on every keystroke, but undo / dirty
    /// stay anchored to commit.
    pub fn text_edit_backspace(&mut self) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        if edit.cursor_offset == 0 {
            return;
        }
        // Find the byte index of the last char whose end is at
        // or before the cursor. For ASCII the answer is
        // `cursor_offset - 1`; for multi-byte UTF-8 it's the
        // start byte of the grapheme cluster that straddles
        // `cursor_offset - 1`. ponytail: byte walk, not
        // grapheme walk — continuing-edge bytes match the same
        // trade-off as the arrow-key stepping (see TextEditState
        // doc). A future grapheme-aware tick will replace this.
        let start = edit.buffer[..edit.cursor_offset]
            .char_indices()
            .last()
            .map_or(0, |(i, _)| i);
        edit.buffer.replace_range(start..edit.cursor_offset, "");
        edit.cursor_offset = start;
        self.sync_text_edit_buffer();
    }

    /// Remove the byte at `cursor_offset` from the edit buffer
    /// (Delete, a.k.a. forward delete). At EOB the buffer is
    /// unchanged (a no-op rather than a panic). Same
    /// write-through contract as `text_edit_backspace`: the
    /// document's TextObject mirrors the buffer on every
    /// keystroke, but undo / dirty stay anchored to commit.
    /// Symmetric with `text_edit_backspace`: backspace pops
    /// the byte *before* the cursor, delete removes the byte
    /// *at* the cursor.
    pub fn text_edit_delete(&mut self) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        if edit.cursor_offset >= edit.buffer.len() {
            return;
        }
        // Find the byte length of the char starting at the
        // cursor. ponytail: byte walk, not grapheme walk —
        // matches the byte-offset model used everywhere else
        // in the editor (see TextEditState doc). Walking from
        // `cursor_offset` skips the prefix and finds the next
        // full grapheme; for ASCII that's 1 byte, for CJK
        // ideographs 3 bytes.
        let next = edit.buffer[edit.cursor_offset..]
            .char_indices()
            .nth(1)
            .map_or(edit.buffer.len(), |(i, _)| edit.cursor_offset + i);
        edit.buffer.replace_range(edit.cursor_offset..next, "");
        // cursor_offset is unchanged (the bytes after the
        // cursor slide left, but the cursor itself stays at
        // the same offset — the new char at that offset is
        // what used to be one position to the right).
        self.sync_text_edit_buffer();
    }

    /// Move the edit cursor one byte to the left, clamped at 0.
    /// No-op when the offset is already at the start of the buffer.
    pub fn text_edit_cursor_left(&mut self) {
        if let Some(edit) = self.text_edit.as_mut() {
            if edit.cursor_offset > 0 {
                edit.cursor_offset -= 1;
            }
        }
    }

    /// Move the edit cursor one byte to the right, clamped at
    /// `buffer.len()`. No-op when the offset is already at the
    /// end of the buffer.
    pub fn text_edit_cursor_right(&mut self) {
        if let Some(edit) = self.text_edit.as_mut() {
            if edit.cursor_offset < edit.buffer.len() {
                edit.cursor_offset += 1;
            }
        }
    }

    /// Jump the edit cursor to offset 0 (Home). No-op when
    /// already at the start. ponytail: buffer start, not the
    /// current line's start — line-aware Home (jump to column
    /// 0, then to buffer start on a second press) is what
    /// vim and most editors do, but it adds a "previous line
    /// length" lookup the byte-offset model doesn't track.
    /// Pure byte offset is the simplest thing that matches the
    /// visible cursor helper; revisit if users complain.
    pub fn text_edit_cursor_home(&mut self) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.cursor_offset = 0;
        }
    }

    /// Jump the edit cursor to `buffer.len()` (End). No-op
    /// when already at the end. Symmetric with `text_edit_
    /// cursor_home` — same ponytail note applies (line-aware
    /// End would jump to the end of the current line; today
    /// it jumps to the end of the buffer).
    pub fn text_edit_cursor_end(&mut self) {
        if let Some(edit) = self.text_edit.as_mut() {
            edit.cursor_offset = edit.buffer.len();
        }
    }

    /// Move the edit cursor up one line, preserving the
    /// column (clamped to the target line's length). No-op
    /// when already on the first line.
    pub fn text_edit_cursor_up(&mut self) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        if let Some(new_off) = text_util::line_nav_offset(&edit.buffer, edit.cursor_offset, -1) {
            edit.cursor_offset = new_off;
        }
    }

    /// Move the edit cursor down one line, preserving the
    /// column (clamped to the target line's length). No-op
    /// when already on the last line.
    pub fn text_edit_cursor_down(&mut self) {
        let Some(edit) = self.text_edit.as_mut() else {
            return;
        };
        if let Some(new_off) = text_util::line_nav_offset(&edit.buffer, edit.cursor_offset, 1) {
            edit.cursor_offset = new_off;
        }
    }

    /// Write the buffer back to the document and exit edit mode.
    /// Returns true when the write happened (target still existed).
    /// Drops edit mode either way so a stale target id (object
    /// vanished) ends the session cleanly.
    ///
    /// Routes through `commit_text_content(id, buffer, initial)`:
    /// the helper uses `initial` as the undo snapshot anchor so
    /// Ctrl-Z rolls back to the pre-edit state, even though
    /// write-through has already mirrored the buffer onto the
    /// document. Without the initial anchor, push_undo would
    /// capture the post-edit state (doc.content == buffer) and
    /// undo would be a no-op.
    pub fn commit_text_edit(&mut self) -> bool {
        let Some(edit) = self.text_edit.take() else {
            return false;
        };
        if !edit.dirty {
            self.status = "edit cancelled (no changes)".into();
            return false;
        }
        let wrote =
            self.state
                .commit_text_content(&edit.target_id, &edit.buffer, &edit.initial_content);
        if wrote {
            self.status = "text edited".into();
        } else {
            self.status = "edit target vanished".into();
        }
        wrote
    }

    /// Drop the buffer and revert the document's TextObject to
    /// the pre-edit content captured at begin_text_edit. Esc
    /// and Ctrl-C both go through here. Revert is what makes
    /// write-through safe for cancel: the buffer might have
    /// already mutated doc.content on every keystroke, but
    /// cancel must leave the document as if F2 was never opened.
    ///
    /// ponytail: cancel = commit-and-then-rollback, not
    /// "skip the side effects". Write-through means we can't
    /// skip — we have to actively revert. The revert helper is
    /// one line and only mutates the field.
    pub fn cancel_text_edit(&mut self) {
        if let Some(edit) = self.text_edit.take() {
            if edit.dirty {
                self.state
                    .revert_text_content(&edit.target_id, &edit.initial_content);
                self.status = "edit cancelled".into();
            }
        }
    }

    /// Open a command-palette session. Returns false when one is
    /// already open (the existing session continues) so the caller
    /// doesn't drop the in-flight buffer by accident.
    pub fn begin_palette(&mut self, trigger: PaletteTrigger) -> bool {
        if self.palette.is_some() {
            return false;
        }
        self.palette = Some(PaletteState {
            trigger,
            buffer: String::new(),
            dirty: false,
        });
        true
    }

    /// Append a printable char into the palette buffer. Caller is
    /// responsible for filtering control sequences — this helper
    /// just pushes the char.
    pub fn palette_insert(&mut self, ch: char) {
        if let Some(p) = self.palette.as_mut() {
            p.buffer.push(ch);
            p.dirty = true;
        }
    }

    /// Remove the last char from the palette buffer (Backspace).
    pub fn palette_backspace(&mut self) {
        if let Some(p) = self.palette.as_mut() {
            p.buffer.pop();
            p.dirty = true;
        }
    }

    /// Empty the buffer in one keystroke (Ctrl-U). Doesn't close
    /// the palette — the user can re-type without re-pressing the
    /// trigger key.
    pub fn palette_clear(&mut self) {
        if let Some(p) = self.palette.as_mut() {
            if !p.buffer.is_empty() {
                p.buffer.clear();
                p.dirty = true;
            }
        }
    }

    /// Close the palette without dispatching. Status is left alone
    /// when the buffer was empty so an accidental trigger+Esc
    /// doesn't spam the status bar; otherwise we surface
    /// "palette cancelled" so the user knows it took the keystrokes.
    pub fn cancel_palette(&mut self) {
        if let Some(p) = self.palette.take() {
            if p.dirty {
                self.status = "palette cancelled".into();
            }
        }
    }

    /// Snapshot the current palette state: the trigger + buffer at
    /// commit time, popped from `App` so a re-entrant dispatch
    /// (e.g. `quit`) doesn't leave a dangling `Some`. Returns
    /// `None` when no palette session is active — a defensive
    /// fallback for future callers.
    ///
    /// ponytail: this method only knows how to take the state.
    /// It does NOT run any action — `dispatch_palette` in
    /// `event.rs` does that, since the side effects are about
    /// the bin's keyboard / status / file system, not the core
    /// data model.
    pub fn take_palette(&mut self) -> Option<PaletteState> {
        self.palette.take()
    }

    /// Read-only view of the buffer (for the renderer to echo the
    /// prompt without taking ownership). Empty string when no
    /// session is active.
    pub fn palette_buffer(&self) -> &str {
        self.palette.as_ref().map_or("", |p| p.buffer.as_str())
    }

    /// True iff a palette session is active. The renderer uses
    /// this to widen the status bar; nothing else needs it.
    pub fn palette_active(&self) -> bool {
        self.palette.is_some()
    }

    /// Open a find session. Returns `false` (and is a no-op) when
    /// the user is already mid-palette / mid-text-edit — the
    /// keymap arm calls this only after those input-hijack modes
    /// have early-returned, but the guard is here as a
    /// second line of defense so a future re-entrant caller
    /// can't double-open.
    pub fn begin_find(&mut self) -> bool {
        if self.find.is_some() || self.palette.is_some() || self.text_edit.is_some() {
            return false;
        }
        self.status = "find: (type to search)".into();
        self.find = Some(FindState {
            query: String::new(),
            matches: Vec::new(),
            index: 0,
        });
        true
    }

    /// Append a printable char to the find query and re-run
    /// `find_matches` so the cached match list stays in sync.
    /// Status echoes the live count so the user has feedback
    /// before pressing Enter.
    pub fn find_insert(&mut self, ch: char) {
        let Some(f) = self.find.as_mut() else {
            return;
        };
        f.query.push(ch);
        f.matches = kirkforge_draw_core::find_matches(&self.state, &f.query);
        f.index = 0;
        self.refresh_find_status();
    }

    /// Pop the last char from the find query and re-run
    /// `find_matches`. No-op when the query is already empty
    /// (Backspace on an empty query is a quiet no-op, not a
    /// close — the user is still composing).
    pub fn find_backspace(&mut self) {
        let Some(f) = self.find.as_mut() else {
            return;
        };
        if f.query.pop().is_none() {
            return;
        }
        f.matches = kirkforge_draw_core::find_matches(&self.state, &f.query);
        f.index = 0;
        self.refresh_find_status();
    }

    /// Advance the find cursor to the next match (wraps at
    /// the end) and select it. The session stays open so a
    /// second Enter cycles to the match after that, matching
    /// the Figma / VS Code "find next" convention. Esc is
    /// the close gesture; the current selection stays in
    /// place when the user dismisses.
    ///
    /// The first Enter after typing shows the first match
    /// (`1/N`), not the second — "display-then-advance" so
    /// a single Enter is enough to land on the most likely
    /// hit. The second Enter shows `2/N`, and so on. After
    /// the last match the cursor wraps back to `1/N`.
    ///
    /// No-op when the query has zero matches (the user
    /// already sees "(no matches)" in the status line from
    /// `refresh_find_status`); they can Backspace the query
    /// to broaden the search without re-pressing Ctrl-F.
    pub fn cycle_find(&mut self) {
        let Some(f) = self.find.as_mut() else {
            return;
        };
        if f.matches.is_empty() {
            return;
        }
        let m = &f.matches[f.index];
        let total = f.matches.len();
        let n = f.index + 1;
        if self.state.select_id(&m.id) {
            let field = match m.field {
                kirkforge_draw_core::MatchField::Id => "id",
                kirkforge_draw_core::MatchField::Content => "content",
            };
            self.status = format!(
                "find {n}/{total}: matched '{:?}' '{}' on {field}",
                m.kind, m.id
            );
        } else {
            // The id vanished between the find_matches scan
            // and the select_id call (an undo / load). Status
            // echoes the id so the user can re-target by hand.
            self.status = format!("find: (id vanished: {})", m.id);
        }
        // Advance for the next press; wraps at the end so
        // the loop is unbounded in either direction.
        f.index = (f.index + 1) % f.matches.len();
    }

    /// Close the find session without committing. Status is
    /// left alone when the query was empty so an accidental
    /// trigger+Esc doesn't spam the status bar; otherwise
    /// we surface "find cancelled".
    pub fn cancel_find(&mut self) {
        if let Some(f) = self.find.take() {
            if !f.query.is_empty() {
                self.status = "find cancelled".into();
            }
        }
    }

    /// Read-only view of the live query (for the renderer's
    /// status-bar prompt). Empty string when no session is
    /// active.
    pub fn find_query(&self) -> &str {
        self.find.as_ref().map_or("", |f| f.query.as_str())
    }

    /// Open a Save-As session. Refuses when another modal is
    /// already open so the user can't stack prompts. Pre-
    /// populates the path buffer with the current `source_path`
    /// (if any) so editing the path is a one-keystroke change
    /// rather than a full re-type.
    pub fn begin_save_as(&mut self) -> bool {
        if self.save_as.is_some()
            || self.find.is_some()
            || self.palette.is_some()
            || self.text_edit.is_some()
        {
            return false;
        }
        let initial = self.source_path.clone().unwrap_or_default();
        self.status = "save as: (type path, Enter to write, Esc to cancel)".into();
        self.save_as = Some(SaveAsState { path: initial });
        true
    }

    /// Append a printable char to the Save-As path buffer. No
    /// validation while typing — the path is checked at commit
    /// time when the user presses Enter. This matches the
    /// find-modal shape (typed chars append, no validation
    /// mid-keystroke).
    pub fn save_as_insert(&mut self, ch: char) {
        if let Some(s) = self.save_as.as_mut() {
            s.path.push(ch);
        }
    }

    /// Pop the last char from the Save-As path buffer. No-op
    /// when the buffer is already empty (Backspace on an
    /// empty path is a quiet no-op, not a close — the user
    /// is still composing).
    pub fn save_as_backspace(&mut self) {
        if let Some(s) = self.save_as.as_mut() {
            // Pop is a silent no-op on an empty path; the user
            // is still composing.
            s.path.pop();
        }
    }

    /// Commit the Save-As session: close the modal, flip
    /// `source_path` to the new path, and return the path so
    /// the bin can call `save_app` next. An empty path is
    /// treated as a no-op (the user pressed Enter on a blank
    /// buffer) — the modal stays open and the status echoes
    /// the error so the user can keep typing.
    pub fn commit_save_as(&mut self) -> Option<String> {
        let path = self.save_as.as_ref()?.path.clone();
        // Reuse `event::validate_path_arg` — the third call
        // site (after render::load_doc and event::save_app)
        // the original ponytail comment predicted. Catches
        // empty / whitespace-only / NUL-byte paths with a
        // status echo so the user can keep typing in the
        // modal without losing their input.
        if let Err(e) = crate::event::validate_path_arg(&path) {
            self.status = format!("save as: ({e})");
            return None;
        }
        self.save_as = None;
        self.source_path = Some(path.clone());
        Some(path)
    }

    /// Close the Save-As session without committing. Status
    /// surfaces "save as cancelled" so the user has visible
    /// feedback that the keypress landed; `source_path` is
    /// unchanged.
    pub fn cancel_save_as(&mut self) {
        if self.save_as.take().is_some() {
            self.status = "save as cancelled".into();
        }
    }

    /// Re-open the Save-As modal after `commit_save_as` succeeded
    /// but the subsequent `save_app` failed (e.g. invalid path,
    /// unwritable parent directory). Restores `source_path` to
    /// its prior value so the user's next Ctrl-S lands where
    /// they came from, and pre-populates the modal buffer with
    /// the path that failed — the user can edit and retry
    /// without retyping. ponytail: the alternative would be to
    /// move the commit/flip out of `commit_save_as` and let the
    /// bin decide when to flip, but that splits the modal's
    /// commit contract in two (validate, then commit) and pushes
    /// state-machine ownership into the bin. The revert helper
    /// keeps the contract "commit_save_as flips + closes,
    /// revert_save_as undoes both" — symmetric, easy to test,
    /// and the bin's failure arm stays a 3-line match.
    pub fn revert_save_as(&mut self, prior_source: Option<String>, bad_path: String) {
        self.source_path = prior_source;
        self.save_as = Some(SaveAsState { path: bad_path });
    }

    /// True iff a find session is active. The renderer uses
    /// this to swap the status-bar format.
    pub fn find_active(&self) -> bool {
        self.find.is_some()
    }

    /// Read-only view of the current match count (for the
    /// renderer's "(N matches)" suffix). Zero when no
    /// session is active.
    pub fn find_match_count(&self) -> usize {
        self.find.as_ref().map_or(0, |f| f.matches.len())
    }

    /// Re-paint the live status line for the in-progress
    /// find. Called from `find_insert` / `find_backspace`
    /// so the user's typing is reflected in the bar without
    /// them having to press Enter.
    fn refresh_find_status(&mut self) {
        let Some(f) = self.find.as_ref() else {
            return;
        };
        let n = f.matches.len();
        self.status = if f.query.is_empty() {
            "find: (type to search)".into()
        } else if n == 0 {
            format!("find: '{}' (no matches)", f.query)
        } else if n == 1 {
            format!("find: '{}' (1 match)", f.query)
        } else {
            format!("find: '{}' ({n} matches)", f.query)
        };
    }

    /// Map a terminal (mouse) coordinate to a document point, taking
    /// the body pane and current scroll into account. Returns `None`
    /// when the coordinate is outside the pane or the scene origin
    /// is unknown (empty document).
    pub fn screen_to_doc(&self, col: u16, row: u16) -> Option<Point> {
        if col < self.body_area.x
            || col >= self.body_area.right()
            || row < self.body_area.y
            || row >= self.body_area.bottom()
        {
            return None;
        }
        let origin = self.scene_origin?;
        let sx = col as i32 - self.body_area.x as i32 + self.scroll_x;
        let sy = row as i32 - self.body_area.y as i32 + self.scroll_y;
        Some(Point {
            x: origin.x + sx,
            y: origin.y + sy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_defaults_to_running() {
        let app = App::new(DrawState::new());
        assert!(!app.should_quit);
        assert!(app.source_path.is_none());
        assert_eq!(app.scroll_x, 0);
        assert_eq!(app.scroll_y, 0);
    }

    #[test]
    fn with_source_records_path() {
        let app = App::new(DrawState::new()).with_source("foo.td.json");
        assert_eq!(app.source_path.as_deref(), Some("foo.td.json"));
    }

    #[test]
    fn request_quit_sets_flag() {
        let mut app = App::new(DrawState::new());
        app.request_quit();
        assert!(app.should_quit);
    }

    #[test]
    fn request_quit_on_clean_doc_quits_immediately() {
        // A fresh document has dirty = false, so the
        // confirm-hijack should not engage — the user
        // asked to quit, they quit.
        let mut app = App::new(DrawState::new());
        assert!(!app.state.is_dirty());
        app.request_quit();
        assert!(app.should_quit);
        assert!(!app.pending_quit_confirm);
    }

    #[test]
    fn request_quit_on_dirty_doc_starts_confirm() {
        // Document has unsaved changes — request_quit sets
        // the confirm flag, stamps a status prompt, and
        // does NOT quit yet.
        let mut app = App::new(DrawState::new());
        app.state.mark_dirty();
        app.request_quit();
        assert!(app.pending_quit_confirm, "confirm flag set");
        assert!(!app.should_quit, "no quit yet");
        assert!(
            app.status.contains("save?"),
            "status echoes the prompt: {}",
            app.status
        );
    }

    #[test]
    fn quit_confirm_no_discards_and_quits() {
        // User replied "no" — discard the unsaved changes
        // and quit. The flag clears, should_quit flips.
        let mut app = App::new(DrawState::new());
        app.pending_quit_confirm = true;
        app.quit_confirm_no();
        assert!(app.should_quit);
        assert!(!app.pending_quit_confirm);
    }

    #[test]
    fn quit_confirm_cancel_clears_prompt() {
        // User replied "Esc" — stay in the editor. The
        // confirm flag drops, status changes to a
        // "quit cancelled" echo so the user has visible
        // feedback that the keypress landed.
        let mut app = App::new(DrawState::new());
        app.pending_quit_confirm = true;
        app.status = "unsaved changes — save? (y/n/Esc)".into();
        app.quit_confirm_cancel();
        assert!(!app.pending_quit_confirm);
        assert!(!app.should_quit);
        assert_eq!(app.status, "quit cancelled");
    }

    #[test]
    fn toggle_help_flips_show_help() {
        let mut app = App::new(DrawState::new());
        assert!(!app.show_help);
        app.toggle_help();
        assert!(app.show_help);
        app.toggle_help();
        assert!(!app.show_help);
    }

    #[test]
    fn new_app_has_no_layer_focus() {
        // Pin: a fresh app starts with layer_focus = None so
        // the panel renders without a cursor on first open.
        let app = App::new(DrawState::new());
        assert!(app.layer_focus.is_none());
    }

    #[test]
    fn closing_layers_panel_clears_stale_focus() {
        // Pin: toggling the panel off drops a stale focus
        // row so a future re-open starts clean.
        let mut app = App::new(DrawState::new());
        app.toggle_layers();
        assert!(app.show_layers);
        app.layer_focus = Some(2);
        app.toggle_layers();
        assert!(!app.show_layers);
        assert!(app.layer_focus.is_none());
    }

    #[test]
    fn new_app_has_no_inspector_visible() {
        // Pin: a fresh app starts with the inspector hidden
        // and no rect reserved — same default as the layers
        // panel. The bin wires `I` to `toggle_inspector`.
        let app = App::new(DrawState::new());
        assert!(!app.show_inspector);
        assert!(app.inspector_area.is_none());
    }

    #[test]
    fn toggle_inspector_flips_visibility() {
        // Two presses must round-trip cleanly. The inspector
        // has no per-row focus (the panel is a read-only
        // summary), so the only state to flip is the bool
        // itself.
        let mut app = App::new(DrawState::new());
        app.toggle_inspector();
        assert!(app.show_inspector);
        app.toggle_inspector();
        assert!(!app.show_inspector);
    }

    #[test]
    fn screen_to_doc_returns_none_outside_pane() {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        app.scene_origin = Some(Point { x: 0, y: 0 });
        // Above the body pane.
        assert!(app.screen_to_doc(0, 0).is_none());
        // Below the body pane.
        assert!(app.screen_to_doc(0, 23).is_none());
    }

    #[test]
    fn screen_to_doc_returns_none_when_no_scene_origin() {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        app.scene_origin = None;
        assert!(app.screen_to_doc(5, 5).is_none());
    }

    #[test]
    fn screen_to_doc_maps_origin_to_top_left() {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        app.scene_origin = Some(Point { x: 0, y: 0 });
        let p = app.screen_to_doc(0, 3).unwrap();
        assert_eq!(p, Point { x: 0, y: 0 });
        let p = app.screen_to_doc(5, 8).unwrap();
        assert_eq!(p, Point { x: 5, y: 5 });
    }

    #[test]
    fn screen_to_doc_respects_scroll() {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        app.scene_origin = Some(Point { x: 0, y: 0 });
        app.scroll_x = 3;
        app.scroll_y = 2;
        // Body (0, 3) now shows scene cell (3, 2).
        let p = app.screen_to_doc(0, 3).unwrap();
        assert_eq!(p, Point { x: 3, y: 2 });
    }

    #[test]
    fn screen_to_doc_respects_scene_origin() {
        let mut app = App::new(DrawState::new());
        app.body_area = Rect::new(0, 3, 80, 20);
        // Scene origin at (-2, -1) — body (0, 3) shows doc point (-2, -1).
        app.scene_origin = Some(Point { x: -2, y: -1 });
        let p = app.screen_to_doc(0, 3).unwrap();
        assert_eq!(p, Point { x: -2, y: -1 });
        let p = app.screen_to_doc(4, 7).unwrap();
        assert_eq!(p, Point { x: 2, y: 3 });
    }

    #[test]
    fn begin_save_as_pre_populates_with_source_path() {
        // Pre-existing source path → the modal opens with
        // that path in the buffer so the user can edit
        // in-place rather than retyping.
        let mut app = App::new(DrawState::new());
        app.source_path = Some("orig.td.json".into());
        assert!(app.begin_save_as());
        let s = app.save_as.as_ref().unwrap();
        assert_eq!(s.path, "orig.td.json");
    }

    #[test]
    fn begin_save_as_starts_empty_without_source() {
        // No source path yet (e.g. fresh editor) → the
        // modal opens with an empty path buffer.
        let mut app = App::new(DrawState::new());
        assert!(app.source_path.is_none());
        assert!(app.begin_save_as());
        let s = app.save_as.as_ref().unwrap();
        assert_eq!(s.path, "");
    }

    #[test]
    fn begin_save_as_refuses_when_find_open() {
        // Defense in depth: a modal is open, so another
        // modal refuses to open. The bin also gates on
        // this (begin_save_as returns false) so a stray
        // Ctrl-Shift-S mid-find doesn't stack prompts.
        let mut app = App::new(DrawState::new());
        app.find = Some(FindState {
            query: "x".into(),
            matches: Vec::new(),
            index: 0,
        });
        assert!(!app.begin_save_as());
        assert!(app.save_as.is_none());
    }

    #[test]
    fn save_as_insert_appends_chars() {
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        app.save_as_insert('f');
        app.save_as_insert('o');
        app.save_as_insert('o');
        assert_eq!(app.save_as.as_ref().unwrap().path, "foo");
    }

    #[test]
    fn save_as_backspace_pops_last_char() {
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        app.save_as_insert('a');
        app.save_as_insert('b');
        app.save_as_backspace();
        assert_eq!(app.save_as.as_ref().unwrap().path, "a");
    }

    #[test]
    fn save_as_backspace_on_empty_is_noop() {
        // Quiet no-op, not a panic. Mirrors find_backspace.
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        app.save_as_backspace();
        assert_eq!(app.save_as.as_ref().unwrap().path, "");
    }

    #[test]
    fn commit_save_as_flips_source_path_and_closes_modal() {
        // Enter on a non-empty path → modal closes, the
        // App's source_path flips to the new path so the
        // next Ctrl-S writes there. No pre-populated
        // source so the typed "new.td.json" lands clean.
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        for ch in "new.td.json".chars() {
            app.save_as_insert(ch);
        }
        let committed = app.commit_save_as();
        assert_eq!(committed.as_deref(), Some("new.td.json"));
        assert_eq!(app.source_path.as_deref(), Some("new.td.json"));
        assert!(app.save_as.is_none(), "modal closes on commit");
    }

    #[test]
    fn commit_save_as_with_empty_path_is_noop() {
        // Empty buffer + Enter → the modal stays open so
        // the user can keep typing; status echoes the
        // error. Returning None lets the bin skip the
        // save_app call entirely.
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        let committed = app.commit_save_as();
        assert_eq!(committed, None);
        assert!(app.save_as.is_some(), "modal stays on empty Enter");
        assert!(
            app.status.contains("empty"),
            "status echoes the no-op: {}",
            app.status
        );
    }

    #[test]
    fn commit_save_as_with_nul_byte_is_noop() {
        // Ctrl-@ on most terminals inserts a NUL byte;
        // the path would otherwise reach
        // `std::fs::OpenOptions::open` and surface as
        // a cryptic "Invalid argument" IO error. The
        // validator catches it here so the user gets a
        // useful status line and the modal stays open
        // for editing. Mirrors `validate_path_arg` in
        // render.rs for the load path.
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        for ch in "safe.td.json\0".chars() {
            app.save_as_insert(ch);
        }
        let committed = app.commit_save_as();
        assert_eq!(committed, None);
        assert!(app.save_as.is_some(), "modal stays on NUL path");
        assert!(
            app.status.contains("NUL"),
            "status echoes the NUL guard: {}",
            app.status
        );
        assert!(app.source_path.is_none(), "source_path unchanged");
    }

    #[test]
    fn commit_save_as_with_whitespace_only_path_is_noop() {
        // A path of just spaces / tabs trims to empty
        // and is treated the same as the empty-buffer
        // case. Confirms the trim() guard handles the
        // "user pressed space space space Enter"
        // foot-gun.
        let mut app = App::new(DrawState::new());
        app.begin_save_as();
        for ch in "   \t  ".chars() {
            app.save_as_insert(ch);
        }
        let committed = app.commit_save_as();
        assert_eq!(committed, None);
        assert!(app.save_as.is_some(), "modal stays on whitespace path");
        assert!(
            app.status.contains("whitespace"),
            "status echoes the no-op: {}",
            app.status
        );
    }

    #[test]
    fn cancel_save_as_closes_modal_and_keeps_source_path() {
        // Esc → modal closes, source_path is unchanged.
        // Status surfaces "save as cancelled" so the user
        // has feedback that the keypress landed. Start
        // with an empty buffer (no pre-population) so the
        // typed 'x' lands clean.
        let mut app = App::new(DrawState::new());
        app.source_path = Some("old.td.json".into());
        app.begin_save_as();
        for _ in 0..12 {
            app.save_as_backspace();
        }
        app.save_as_insert('x');
        app.cancel_save_as();
        assert!(app.save_as.is_none());
        assert_eq!(app.source_path.as_deref(), Some("old.td.json"));
        assert_eq!(app.status, "save as cancelled");
    }

    #[test]
    fn revert_save_as_restores_prior_source_and_reopens_modal_with_bad_path() {
        // The commit-fail footgun fix. After commit_save_as
        // flips source_path to a new path and the subsequent
        // save_app fails (e.g. unwritable parent dir), the
        // bin calls revert_save_as to roll source_path
        // back to where the user came from and re-open the
        // modal pre-populated with the bad path — the user
        // can edit and retry without retyping.
        let mut app = App::new(DrawState::new());
        app.source_path = Some("old.td.json".into());
        let bad_path = "/no/such/dir/file.td.json".to_string();
        app.revert_save_as(Some("old.td.json".into()), bad_path.clone());
        assert_eq!(
            app.source_path.as_deref(),
            Some("old.td.json"),
            "revert must restore prior source_path"
        );
        let s = app
            .save_as
            .as_ref()
            .expect("revert must re-open the save_as modal");
        assert_eq!(
            s.path, bad_path,
            "revert must pre-populate the modal with the bad path"
        );
    }

    #[test]
    fn revert_save_as_with_no_prior_source_clears_to_none() {
        // Edge: prior source_path was None (the user
        // opened with no --load, hit Ctrl-Shift-S, typed a
        // bad path, Enter → save fail). Revert should
        // restore source_path to None, not leave it
        // pointing at the bad path.
        let mut app = App::new(DrawState::new());
        app.source_path = None;
        app.revert_save_as(None, "/no/such/dir/file.td.json".into());
        assert!(app.source_path.is_none(), "revert must clear to None");
        assert!(app.save_as.is_some(), "revert must re-open the modal");
    }
}
