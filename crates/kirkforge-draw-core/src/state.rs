//! Editor state machine.
//!
//! `DrawState` owns the in-memory document, the active tool, the
//! selection, the in-progress draft, and the undo/redo stacks. The TUI
//! crate drives it via the `begin/update/commit/cancel draft` and the
//! selection / mutation methods.
//!
//! Mirrors `termdraw`'s `draw-state/state.ts`. Undo entries are full
//! `DrawDocument` snapshots — simple, predictable, and easy to reason
//! about for a v1 editor. We can move to a diff-based log if memory
//! ever becomes a concern.

use std::collections::HashSet;

use crate::doc::new_object_id;
use crate::line::{append_paint_segment, constrain_line_point};
use crate::object::{
    clone_objects, compute_resized_bounds, get_object_bounds, get_object_selection_bounds,
    object_contains_point, translate_object,
};
use crate::types::{
    Align, BoxObject, BoxResizeHandle, BoxStyle, DistributeAxis, DrawDocument, DrawMode,
    DrawObject, ElbowObject, ElbowOrientation, InkColor, LineObject, LineStyle, PaintObject, Point,
    Rect, SelectionMode, TextBorderMode, TextObject,
};

/// Cap on undo history. Older entries are dropped. Keeps memory bounded
/// for long edit sessions.
const MAX_UNDO: usize = 100;

/// The editor state. Cheap to clone for read-only inspection; mutations
/// go through methods that record an undo snapshot.
#[derive(Debug, Clone)]
pub struct DrawState {
    pub document: DrawDocument,
    pub tool: DrawMode,
    pub color: InkColor,
    pub line_style: LineStyle,
    pub box_style: BoxStyle,
    pub brush: String,
    pub text_border: TextBorderMode,

    selected_ids: HashSet<String>,
    /// The drag anchor of an in-progress draft. `Some` only between
    /// `begin_draft` and `commit_draft` / `cancel_draft`.
    draft_anchor: Option<Point>,
    draft_object: Option<DrawObject>,
    /// The most recent constrained point (e.g. line endpoint) so
    /// re-renders between pointer events can use it.
    draft_pointer: Option<Point>,

    /// Active resize drag of an already-committed box: the box id, the
    /// pre-drag bounds (kept so undo only rolls back to one snapshot),
    /// and the corner the user grabbed. `None` when not resizing.
    resize_target: Option<(String, Rect, BoxResizeHandle)>,

    undo_stack: Vec<DrawDocument>,
    redo_stack: Vec<DrawDocument>,
    /// True when the document has been mutated since the last
    /// `mark_saved()`. Read by the UI to render a `*` badge on the
    /// status line; cleared by the save handler. `false` for a
    /// freshly-loaded / freshly-built state.
    dirty: bool,
}

impl Default for DrawState {
    fn default() -> Self {
        Self::new()
    }
}

impl DrawState {
    pub fn new() -> Self {
        Self {
            document: DrawDocument {
                version: crate::types::DRAW_DOCUMENT_VERSION,
                objects: vec![],
            },
            tool: DrawMode::Select,
            color: InkColor::White,
            line_style: LineStyle::Smooth,
            box_style: BoxStyle::Light,
            brush: "·".into(),
            text_border: TextBorderMode::None,
            selected_ids: HashSet::new(),
            draft_anchor: None,
            draft_object: None,
            draft_pointer: None,
            resize_target: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            dirty: false,
        }
    }

    pub fn with_document(doc: DrawDocument) -> Self {
        let mut s = Self::new();
        s.document = doc;
        s
    }

    /// Linear search for an object's index in the document by id.
    /// Returns `None` when no object has that id. Used by every
    /// "look up the box for this id" site (resize, group edit,
    /// text edit, parent assign, z-order swap, etc.) — centralizing
    /// the iteration shape here means a future O(1) id→index
    /// upgrade (hashmap) touches one method instead of twelve
    /// scattered `iter().position(...)` call sites.
    ///
    /// ponytail: O(n) is fine because normal documents hold a few
    /// dozen objects — adding a `HashMap<String, usize>` would
    /// buy nothing at this scale and would force a parallel-write
    /// discipline on every insert / remove.
    fn find_object_index(&self, id: impl AsRef<str>) -> Option<usize> {
        self.document
            .objects
            .iter()
            .position(|o| o_id(o) == id.as_ref())
    }

    // -- Tool / ink setters -----------------------------------------

    pub fn set_tool(&mut self, tool: DrawMode) {
        self.tool = tool;
        // Switching tool cancels any in-progress draft — the new tool
        // shouldn't inherit a half-drawn object from the old one. We
        // deliberately do NOT cancel an active resize; the user can be
        // mid-drag and tab between tools (or hit a hotkey) without
        // silently losing the gesture.
        self.cancel_draft();
    }

    /// Move to the next (or previous) tool in `DrawMode` order. Wraps
    /// at both ends so Tab from the last tool lands back on Select.
    /// Used by Tab / Shift+Tab so the user can cycle without knowing
    /// the single-letter hotkeys.
    pub fn cycle_tool(&mut self, forward: bool) {
        // ponytail: derive the order from the enum's discriminants so
        // adding a new tool in the middle automatically extends the
        // cycle.
        let order = [
            DrawMode::Select,
            DrawMode::Box,
            DrawMode::Line,
            DrawMode::Elbow,
            DrawMode::Paint,
            DrawMode::Text,
        ];
        let cur = order.iter().position(|m| *m == self.tool).unwrap_or(0);
        let next = if forward {
            (cur + 1) % order.len()
        } else {
            (cur + order.len() - 1) % order.len()
        };
        self.set_tool(order[next]);
    }

    pub fn set_color(&mut self, color: InkColor) {
        self.color = color;
    }

    pub fn set_line_style(&mut self, style: LineStyle) {
        self.line_style = style;
    }

    pub fn set_box_style(&mut self, style: BoxStyle) {
        self.box_style = style;
    }

    pub fn set_brush(&mut self, brush: impl Into<String>) {
        self.brush = brush.into();
    }

    pub fn set_text_border(&mut self, border: TextBorderMode) {
        self.text_border = border;
    }

    // -- Undo / redo -------------------------------------------------

    fn push_undo(&mut self) {
        self.undo_stack.push(self.document.clone());
        if self.undo_stack.len() > MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    pub fn snapshot(&mut self) {
        self.push_undo();
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.document.clone());
            self.document = prev;
            // After the pop, the document is already at the state the
            // begin_resize snapshot captured. The resize_target still
            // holds the *original* (pre-drag) bounds, so cancel_resize
            // is effectively a no-op on the document but it does clear
            // the resize_target field. We call cancel_resize +
            // cancel_draft directly (NOT cancel_all) because cancel_all
            // also pops the undo stack — undo's body already popped,
            // and a second pop would silently destroy prior history.
            self.cancel_resize();
            self.cancel_draft();
            self.reconcile_selection();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.document.clone());
            self.document = next;
            // See undo(): same rationale, no second pop.
            self.cancel_resize();
            self.cancel_draft();
            self.reconcile_selection();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// True when the document has been mutated since the last
    /// `mark_saved()`. Read by the UI to render a `*` in the title bar.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Called by the save handler once the document has been written
    /// to disk. Clears the dirty bit until the next mutation.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Ponytail: keep the mutation hooks in one place. Pairs of
    /// "snapshot + mutate + dirty" are now "snapshot + mutate" +
    /// `mark_dirty` at the end; the only places that touch
    /// `self.dirty` live here. Public because the save handler in the
    /// bin crate needs to flag dirty on a failed save.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    // -- Selection ---------------------------------------------------

    pub fn selected(&self) -> Vec<&DrawObject> {
        self.document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .collect()
    }

    pub fn selected_count(&self) -> usize {
        self.selected_ids.len()
    }

    pub fn clear_selection(&mut self) {
        self.selected_ids.clear();
    }

    /// Replace the selection with a single id. Returns true when the
    /// id matched an existing object (selection then holds exactly
    /// one item); returns false when the id is unknown (selection is
    /// cleared to keep the invariant "an id in selected_ids refers to
    /// a real object").
    ///
    /// ponytail: replaces `clear + insert` from the bin and the
    /// inspector tests without exposing `selected_ids` publicly.
    pub fn select_id(&mut self, id: &str) -> bool {
        self.selected_ids.clear();
        if self.find_object_index(id).is_some() {
            self.selected_ids.insert(id.to_string());
            true
        } else {
            false
        }
    }

    /// Select every object in the document. Returns the count
    /// after the call. No-op (returns 0) when the document is
    /// empty. No draft / resize guards — those would be
    /// inconsistent with how `select_id` and `select_in_rect`
    /// behave today. The bin's Ctrl-A arm is expected to be
    /// the only caller, and a Ctrl-A press during a draft is
    /// rare enough not to need special handling.
    ///
    /// ponytail: returns `usize` (not `bool`) for the same
    /// reason `select_in_rect` does — the bin's status echo
    /// needs the count, and the `> 0` short-circuit is cheaper
    /// to write as `n > 0` than as `!selected_ids.is_empty()`.
    pub fn select_all(&mut self) -> usize {
        self.selected_ids.clear();
        for o in &self.document.objects {
            self.selected_ids.insert(o_id(o).to_string());
        }
        self.selected_ids.len()
    }

    /// Flip the selection: every currently-selected object becomes
    /// unselected, and every currently-unselected object becomes
    /// selected. Returns the new selection count. Pairs with
    /// `select_all`: Ctrl-A to grab everything, Ctrl-Shift-I to
    /// flip it back to empty; Ctrl-Shift-I alone from an empty
    /// selection selects everything; from a partial selection it
    /// completes the inverse.
    ///
    /// Pushes a single undo step. ponytail: the Figma / Slack
    /// convention treats "invert" as a single edit — one undo step
    /// per inversion matches the "one undo per keypress" contract
    /// the rest of the selection commands use (select_all, clear,
    /// delete). The membership flip is a pure set operation, so
    /// even an "all → empty" inversion undoes cleanly back to
    /// "all selected" without a snapshot of the prior selection
    /// state.
    pub fn invert_selection(&mut self) -> usize {
        self.push_undo();
        let current: std::collections::HashSet<String> =
            self.selected_ids.iter().cloned().collect();
        self.selected_ids.clear();
        for o in &self.document.objects {
            let id = o_id(o).to_string();
            if !current.contains(&id) {
                self.selected_ids.insert(id);
            }
        }
        let n = self.selected_ids.len();
        if n > 0 {
            self.mark_dirty();
        }
        n
    }

    /// Add a single id to the existing selection. No-op when the id
    /// is unknown. Mirrors `select_id`'s "true = matched an object"
    /// contract — the bin's layers-panel click handler uses the
    /// boolean to decide whether to surface "selected N" vs.
    /// "id already in selection" in the status bar.
    ///
    /// ponytail: paired with `select_id` for the layers-panel
    /// click flow. Same rationale — pull the bin's `selected_ids`
    /// access behind a public single-id API rather than leaking
    /// the set.
    pub fn add_to_selection(&mut self, id: &str) -> bool {
        if self.find_object_index(id).is_some() {
            self.selected_ids.insert(id.to_string());
            true
        } else {
            false
        }
    }

    /// Toggle a single id's membership. No-op when the id is
    /// unknown. Returns true when the toggle actually matched an
    /// object (regardless of which way it flipped) so the caller
    /// can tell "I touched the selection" from "the id was
    /// bogus".
    pub fn toggle_selection(&mut self, id: &str) -> bool {
        if self.find_object_index(id).is_some() {
            if !self.selected_ids.remove(id) {
                self.selected_ids.insert(id.to_string());
            }
            true
        } else {
            false
        }
    }

    /// Hit-test selection: select the topmost object whose hit test
    /// passes. Replaces the current selection. Returns the selected
    /// object (if any).
    pub fn select_at(&mut self, point: Point) -> Option<&DrawObject> {
        // Forwarder preserved so every existing call site (tests
        // and the bare "no-modifier" mouseup fallback) keeps
        // the legacy Replace semantics. The mode-aware variant
        // is `select_at_with_mode` — see its doc comment for
        // the Shift+click / Ctrl+click rationale.
        self.select_at_with_mode(point, SelectionMode::Replace)
    }

    /// Pick the topmost object at `point` and combine it with the
    /// existing selection per `mode`. Mirrors `select_in_rect`'s
    /// three modes so a single click honors the same Shift /
    /// Ctrl modifiers a marquee drag does.
    ///
    /// ponytail: keep the hit-test (`object_contains_point`) in
    /// one place; the bare `select_at` already iterated `.rev()`
    /// to grab topmost, and reusing that order here means the
    /// click picks the same object the user visually clicked on.
    /// The mode dispatch is a small flat match — three arms —
    /// so a lookup table buys nothing.
    pub fn select_at_with_mode(
        &mut self,
        point: Point,
        mode: SelectionMode,
    ) -> Option<&DrawObject> {
        for obj in self.document.objects.iter().rev() {
            if object_contains_point(obj, point) {
                let id = o_id(obj).to_string();
                match mode {
                    SelectionMode::Replace => {
                        self.selected_ids.clear();
                        self.selected_ids.insert(id.clone());
                    }
                    SelectionMode::Add => {
                        // If already in the set, HashSet::insert is
                        // a no-op — no churn, no allocation.
                        self.selected_ids.insert(id.clone());
                    }
                    SelectionMode::Toggle => {
                        if !self.selected_ids.remove(&id) {
                            self.selected_ids.insert(id.clone());
                        }
                    }
                }
                return Some(obj);
            }
        }
        // No hit. For Replace we keep today's "click on empty
        // space clears the selection" — most editors do this.
        // Add / Toggle clicks on empty space leave the existing
        // selection alone, matching every standard editor:
        // Shift+clicking on background doesn't deselect, and
        // Ctrl+clicking on background is a no-op.
        if mode == SelectionMode::Replace {
            self.selected_ids.clear();
        }
        None
    }

    /// Marquee selection: select every object whose selection-bounds
    /// intersect `rect`, combined with the existing selection
    /// according to `mode`. Returns the total selection count after
    /// the merge so the caller can report "selected N objects" on
    /// the status bar.
    ///
    /// An empty / inverted rect (`right < left` or `bottom < top`)
    /// is a no-op — there's no marquee to honor. Selection bounds
    /// (not render bounds) are used so a tall-but-thin Text object
    /// is still selectable when its content rect is touched by the
    /// marquee.
    pub fn select_in_rect(&mut self, rect: Rect, mode: SelectionMode) -> usize {
        if rect.left > rect.right || rect.top > rect.bottom {
            return self.selected_ids.len();
        }
        // Snapshot the intersecting ids once so Toggle can flip
        // membership without re-scanning the document on each side
        // of the membership test.
        let intersecting: Vec<String> = self
            .document
            .objects
            .iter()
            .filter_map(|o| {
                let b = get_object_selection_bounds(o)?;
                // Edge-touching counts as intersecting — matches the
                // existing `rects_intersect` test in geometry.rs.
                if rect.left <= b.right
                    && rect.right >= b.left
                    && rect.top <= b.bottom
                    && rect.bottom >= b.top
                {
                    Some(o_id(o).to_string())
                } else {
                    None
                }
            })
            .collect();
        match mode {
            SelectionMode::Replace => {
                self.selected_ids.clear();
                for id in &intersecting {
                    self.selected_ids.insert(id.clone());
                }
            }
            SelectionMode::Add => {
                for id in &intersecting {
                    self.selected_ids.insert(id.clone());
                }
            }
            SelectionMode::Toggle => {
                for id in &intersecting {
                    if !self.selected_ids.remove(id) {
                        self.selected_ids.insert(id.clone());
                    }
                }
            }
        }
        self.selected_ids.len()
    }

    fn reconcile_selection(&mut self) {
        let live: HashSet<String> = self
            .document
            .objects
            .iter()
            .map(|o| o_id(o).to_string())
            .collect();
        self.selected_ids.retain(|id| live.contains(id));
    }

    // -- Drafts ------------------------------------------------------

    pub fn has_draft(&self) -> bool {
        self.draft_object.is_some()
    }

    pub fn draft(&self) -> Option<&DrawObject> {
        self.draft_object.as_ref()
    }

    /// Start a new draft object of the current tool kind. The first
    /// point is recorded as the anchor; subsequent `update_draft` calls
    /// reshape the draft. No-op when the current tool is Select — the
    /// editor routes Select clicks to [`Self::select_at`], not draft
    /// creation. Ponytail: silent rejection is enough; callers that
    /// care should check `self.tool` first.
    pub fn begin_draft(&mut self, point: Point) {
        if self.tool == DrawMode::Select {
            return;
        }
        // Starting a new draft aborts any prior draft AND any in-flight
        // resize — only one interactive gesture at a time.
        self.cancel_all();
        self.draft_anchor = Some(point);
        self.draft_pointer = Some(point);
        self.draft_object = Some(self.new_draft_object(point, point));
    }

    fn new_draft_object(&self, anchor: Point, pointer: Point) -> DrawObject {
        let z = self.next_z();
        // ponytail: Select is rejected in begin_draft before we get
        // here, so the id-prefix match and the kind match both drop
        // the Select arm without a panic.
        let id = new_object_id(match self.tool {
            DrawMode::Box => "box",
            DrawMode::Line => "line",
            DrawMode::Elbow => "elbow",
            DrawMode::Paint => "paint",
            DrawMode::Text => "text",
            DrawMode::Select => "sel",
        });
        match self.tool {
            DrawMode::Box => DrawObject::Box(BoxObject {
                id,
                z,
                parent_id: None,
                color: self.color,
                left: anchor.x,
                top: anchor.y,
                right: pointer.x,
                bottom: pointer.y,
                style: self.box_style,
            }),
            DrawMode::Line => DrawObject::Line(LineObject {
                id,
                z,
                parent_id: None,
                color: self.color,
                x1: anchor.x,
                y1: anchor.y,
                x2: pointer.x,
                y2: pointer.y,
                style: self.line_style,
            }),
            DrawMode::Elbow => DrawObject::Elbow(ElbowObject {
                id,
                z,
                parent_id: None,
                color: self.color,
                x1: anchor.x,
                y1: anchor.y,
                x2: pointer.x,
                y2: pointer.y,
                style: self.line_style,
                orientation: ElbowOrientation::VerticalFirst,
            }),
            DrawMode::Paint => DrawObject::Paint(PaintObject {
                id,
                z,
                parent_id: None,
                color: self.color,
                points: vec![anchor],
                brush: self.brush.clone(),
            }),
            DrawMode::Text => DrawObject::Text(TextObject {
                id,
                z,
                parent_id: None,
                color: self.color,
                x: anchor.x,
                y: anchor.y,
                content: String::new(),
                border: self.text_border,
            }),
            // ponytail: begin_draft returns early on Select so we
            // never reach here. Keep the arm so the match stays
            // exhaustive; unreachable! documents the invariant for
            // future readers.
            DrawMode::Select => unreachable!("begin_draft rejects Select"),
        }
    }

    /// Update the in-progress draft with the new pointer position.
    /// For paint strokes, this appends a Bresenham segment from the
    /// previous pointer. For line/elbow, the pointer is constrained
    /// to the dominant axis relative to the anchor.
    pub fn update_draft(&mut self, pointer: Point) {
        // ponytail: let-else over guard+unwrap so a future refactor
        // of the early-return can't reintroduce a panic by deleting
        // one line. The else branch is the no-op we already wanted.
        let Some(anchor) = self.draft_anchor else {
            return;
        };
        let constrained = match self.tool {
            DrawMode::Line | DrawMode::Elbow => constrain_line_point(anchor, pointer),
            _ => pointer,
        };
        self.draft_pointer = Some(constrained);
        let mut next = self.new_draft_object(anchor, constrained);
        if self.tool == DrawMode::Paint {
            // Carry over previous points, then append a Bresenham
            // segment to the new pointer.
            if let Some(DrawObject::Paint(p)) = self.draft_object.as_ref() {
                if let DrawObject::Paint(np) = &mut next {
                    np.points = append_paint_segment(
                        &p.points,
                        p.points.last().copied().unwrap_or(anchor),
                        constrained,
                    );
                }
            }
        }
        self.draft_object = Some(next);
    }

    /// Commit the in-progress draft into the document. Returns the new
    /// object's id (for selection after creation), or `None` if there
    /// was no draft to commit OR the draft is degenerate (zero-area
    /// Box / Line / Elbow).
    pub fn commit_draft(&mut self) -> Option<String> {
        let obj = self.draft_object.take()?;
        let id = o_id(&obj).to_string();
        // Empty paint strokes or zero-area boxes are dropped — there's
        // nothing to render and we'd rather not pollute the document.
        if is_degenerate(&obj) {
            self.draft_anchor = None;
            self.draft_pointer = None;
            return None;
        }
        self.push_undo();
        self.document.objects.push(obj);
        self.selected_ids.clear();
        self.selected_ids.insert(id.clone());
        self.draft_anchor = None;
        self.draft_pointer = None;
        self.mark_dirty();
        Some(id)
    }

    /// Discard any in-progress draft. Leaves an active resize alone —
    /// callers that want to abort the resize too should call
    /// [`Self::cancel_resize`] afterwards (or [`Self::cancel_all`]
    /// to abort both). See also [`Self::cancel_all`] for the
    /// one-shot "abort everything in flight" helper.
    pub fn cancel_draft(&mut self) {
        self.draft_anchor = None;
        self.draft_object = None;
        self.draft_pointer = None;
    }

    /// Abort an in-progress resize: restore the dragged box to its
    /// pre-drag bounds. No-op when no resize is active. Does NOT pop
    /// the pre-drag snapshot from the undo stack — callers that want
    /// that behavior (begin_draft, Esc) should use `cancel_all`. The
    /// split exists because `undo`/`redo` already pop from the undo
    /// stack themselves, and `cancel_resize` popping a second time
    /// silently destroyed prior history (see the
    /// `undo_during_resize_preserves_prior_history` regression test).
    pub fn cancel_resize(&mut self) {
        if let Some((id, original, _)) = self.resize_target.take() {
            if let Some(idx) = self.find_object_index(id) {
                if let Some(DrawObject::Box(b)) = self.document.objects.get_mut(idx) {
                    b.left = original.left;
                    b.top = original.top;
                    b.right = original.right;
                    b.bottom = original.bottom;
                }
            }
        }
    }

    /// Abort every in-flight interaction (resize + draft) and drop
    /// the pre-drag snapshot the resize pushed. Callers: keyboard-Esc,
    /// `begin_draft` (starting a new draft discards any prior gesture),
    /// and any UI path that wants a one-shot "abort everything" helper.
    /// `undo` / `redo` deliberately do NOT use this — they pop the
    /// undo stack themselves, and a second pop here would eat prior
    /// history.
    pub fn cancel_all(&mut self) {
        let had_resize = self.resize_target.is_some();
        self.cancel_resize();
        if had_resize {
            // Drop the pre-drag snapshot the resize pushed: the
            // document is already back at pre-drag, so the snapshot
            // is stale. Lives here (not in cancel_resize) so undo /
            // redo can call cancel_resize without paying the pop.
            self.undo_stack.pop();
        }
        self.cancel_draft();
    }

    /// Begin a resize drag on the currently-selected box. Returns
    /// `true` if a resize was started. Fails (returns `false`) when
    /// there is no selected box, more than one box is selected, or the
    /// selected object isn't a box. Pushes an undo snapshot so the
    /// whole gesture is one undo step.
    pub fn begin_resize(&mut self, handle: BoxResizeHandle) -> bool {
        if self.resize_target.is_some() {
            return false;
        }
        let Some(id) = self.single_selected_box_id() else {
            return false;
        };
        let Some(idx) = self.find_object_index(&id) else {
            return false;
        };
        let Some(bounds) = get_object_bounds(&self.document.objects[idx]) else {
            return false;
        };
        // Abort any leftover draft, but not a resize — `begin_resize` is
        // called specifically to start one. (A second `begin_resize`
        // while a resize is in flight already early-returned above.)
        self.cancel_draft();
        self.push_undo();
        self.resize_target = Some((id, bounds, handle));
        true
    }

    /// Update the active resize to follow a new pointer position.
    /// No-op when no resize is in flight. The box's bounds are mutated
    /// in place; the pre-drag bounds stay stashed so the snapshot
    /// taken at `begin_resize` undoes the whole drag at once.
    pub fn update_resize(&mut self, pointer: Point) {
        let Some((id, original, handle)) = self.resize_target.clone() else {
            return;
        };
        let Some(idx) = self.find_object_index(id) else {
            return;
        };
        let next = compute_resized_bounds(original, handle, pointer);
        if let Some(DrawObject::Box(b)) = self.document.objects.get_mut(idx) {
            b.left = next.left;
            b.top = next.top;
            b.right = next.right;
            b.bottom = next.bottom;
        }
    }

    /// Finalize the active resize: clears the drag state. The undo
    /// snapshot was pushed at `begin_resize` so the whole gesture is
    /// one step. Returns `true` if a resize was active.
    pub fn commit_resize(&mut self) -> bool {
        let target = self.resize_target.take();
        let was_resizing = target.is_some();
        if let Some((id, _, _)) = target {
            // If the resize collapsed the box to a point (e.g. the
            // user dragged a handle exactly onto the opposite
            // corner), drop the object — mirrors the is_degenerate
            // filter `commit_draft` already applies. The undo
            // snapshot taken at `begin_resize` still holds the
            // pre-drag document, so a single undo restores it.
            if let Some(idx) = self.find_object_index(&id) {
                if is_degenerate(&self.document.objects[idx]) {
                    self.document.objects.remove(idx);
                    self.selected_ids.remove(&id);
                }
            }
            // The final bounds weren't applied directly here
            // (update_resize mutated the box in place); flag the
            // document so the UI can render a * and so save-to-disk
            // acknowledges the change.
            self.mark_dirty();
        }
        was_resizing
    }

    /// The single selected box's id, or `None` if zero or many are
    /// selected, or the only selection isn't a box.
    fn single_selected_box_id(&self) -> Option<String> {
        if self.selected_ids.len() != 1 {
            return None;
        }
        // The set has exactly one element by the guard above; pull
        // it via the iterator's `Some` directly so a future change
        // to the selection backing doesn't leave a panic site here.
        let id = self.selected_ids.iter().next()?;
        self.document
            .objects
            .iter()
            .find(|o| o_id(o) == id && matches!(o, DrawObject::Box(_)))
            .map(|o| o_id(o).to_string())
    }

    /// Whether a resize drag is currently active.
    pub fn is_resizing(&self) -> bool {
        self.resize_target.is_some()
    }

    // -- Mutations on the selection ---------------------------------

    pub fn delete_selected(&mut self) -> usize {
        if self.selected_ids.is_empty() {
            return 0;
        }
        // If the user deletes while a resize is in flight on the only
        // selected box, drop the resize so commit_resize can't reach a
        // dangling id.
        if let Some((id, _, _)) = &self.resize_target {
            if self.selected_ids.contains(id) {
                self.resize_target = None;
            }
        }
        let n = self.selected_ids.len();
        self.push_undo();
        self.document
            .objects
            .retain(|o| !self.selected_ids.contains(o_id(o)));
        self.selected_ids.clear();
        self.mark_dirty();
        n
    }

    /// Translate every selected object by `(dx, dy)`. No-op when the
    /// selection is empty or when a draft is in progress.
    pub fn move_selected(&mut self, dx: i32, dy: i32) {
        if self.selected_ids.is_empty() || dx == 0 && dy == 0 {
            return;
        }
        self.push_undo();
        for obj in self.document.objects.iter_mut() {
            if self.selected_ids.contains(o_id(obj)) {
                *obj = translate_object(obj, dx, dy);
            }
        }
        self.mark_dirty();
    }

    /// Repaint every selected Line / Elbow object with the given
    /// `LineStyle`. Boxes keep their `BoxStyle` (a separate enum) and
    /// Paint / Text objects have no line-style concept at all — the
    /// pure helper silently skips them so the user doesn't have to
    /// think about which of their selected objects carry a line
    /// style. Same no-op / single-undo / dirty semantics as
    /// `recolor_selection`. Returns the count of objects whose
    /// style actually changed (lines + elbows only).
    pub fn restyle_selection(&mut self, style: LineStyle) -> usize {
        if self.selected_ids.is_empty() {
            return 0;
        }
        // Short-circuit when every selected line/elbow is already this
        // style: skip the undo push and dirty flip. Boxes/paint/text are
        // skipped silently — `already` returns true if the only
        // selected objects are ones we wouldn't touch anyway.
        let any_styled = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .any(|o| matches!(o, DrawObject::Line(_) | DrawObject::Elbow(_)));
        if !any_styled {
            return 0;
        }
        let already = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .filter(|o| matches!(o, DrawObject::Line(_) | DrawObject::Elbow(_)))
            .all(|o| match o {
                DrawObject::Line(l) => l.style == style,
                DrawObject::Elbow(e) => e.style == style,
                _ => true,
            });
        if already {
            return 0;
        }
        self.push_undo();
        let mut changed = 0;
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            match obj {
                DrawObject::Line(l) if l.style != style => {
                    l.style = style;
                    changed += 1;
                }
                DrawObject::Elbow(e) if e.style != style => {
                    e.style = style;
                    changed += 1;
                }
                // ponytail: the outer loop already filters to
                // Line | Elbow kinds via the `selected_ids ∩
                // restyle-eligible` set built earlier in this
                // function, so the wildcard here is unreachable
                // in practice. Kept because Rust's pattern
                // matching on `&mut DrawObject` doesn't carry
                // the type-narrowing through the loop. Add a
                // new restyle-eligible variant here AND in the
                // outer filter when one is introduced.
                _ => {}
            }
        }
        self.mark_dirty();
        changed
    }

    /// Apply `BoxStyle` to every selected Box. Silent no-op for
    /// selected objects that don't carry a `BoxStyle` (Line,
    /// Elbow, Paint, Text) so the user can keep Boxes mixed in
    /// with other shapes without first deselecting. Same
    /// no-op / single-undo / dirty semantics as
    /// `restyle_selection`. Returns the count of objects whose
    /// style actually changed (boxes only).
    ///
    /// ponytail: parallel to `restyle_selection` (which is for
    /// `LineStyle` on Line / Elbow). Don't unify behind a trait
    /// — the two enums have different variant sets and a
    /// generic "set restyle field" helper would obscure the
    /// per-kind eligibility. Mirror the structure of
    /// `restyle_selection` exactly so the two cycle keymaps
    /// behave identically.
    pub fn restyle_boxes_selection(&mut self, style: BoxStyle) -> usize {
        if self.selected_ids.is_empty() {
            return 0;
        }
        let any_box = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .any(|o| matches!(o, DrawObject::Box(_)));
        if !any_box {
            return 0;
        }
        let already = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .filter(|o| matches!(o, DrawObject::Box(_)))
            .all(|o| match o {
                DrawObject::Box(b) => b.style == style,
                _ => true,
            });
        if already {
            return 0;
        }
        self.push_undo();
        let mut changed = 0;
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            match obj {
                DrawObject::Box(b) if b.style != style => {
                    b.style = style;
                    changed += 1;
                }
                // ponytail: outer `matches!` filter restricted
                // to Box above; wildcard here is unreachable in
                // practice. Kept for the same reason as in
                // restyle_selection: borrow of `&mut DrawObject`
                // doesn't carry the type-narrowing into the
                // match arm.
                _ => {}
            }
        }
        self.mark_dirty();
        changed
    }

    /// Repaint every selected object in `color`. Pushes one undo step
    /// for the whole batch, so a single `Ctrl-Z` reverts the recolor
    /// regardless of how many objects were selected. No-op (no undo,
    /// no dirty) when the selection is empty. Returns the number of
    /// objects whose color actually changed — callers can use this to
    /// suppress a status message when the keypress was a no-op (e.g.
    /// recoloring a white-only selection back to white).
    pub fn recolor_selection(&mut self, color: InkColor) -> usize {
        if self.selected_ids.is_empty() {
            return 0;
        }
        // Short-circuit when every selected object is already this
        // color: skip the undo push and the dirty flip so the user
        // can spam Ctrl-1 without churning the undo stack.
        let already = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .all(|o| o.color() == color);
        if already {
            return 0;
        }
        self.push_undo();
        let mut changed = 0;
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            let cur = obj.color();
            if cur == color {
                continue;
            }
            match obj {
                DrawObject::Box(o) => o.color = color,
                DrawObject::Line(o) => o.color = color,
                DrawObject::Elbow(o) => o.color = color,
                DrawObject::Paint(o) => o.color = color,
                DrawObject::Text(o) => o.color = color,
            }
            changed += 1;
        }
        self.mark_dirty();
        changed
    }

    /// Translate every selected object so the chosen edge or
    /// center of its selection bounds lines up with the same
    /// edge or center of the union of all selected bounds
    /// (Left / Right / Top / Bottom / HorizontalCenter /
    /// VerticalCenter). Pushes one undo step for the whole
    /// batch, so a single `Ctrl-Z` reverts the alignment
    /// regardless of selection size. No-op (no undo, no dirty)
    /// when the selection is empty, when a draft is in
    /// progress (mirrors `duplicate_selected` — an
    /// in-progress shape shouldn't be yanked to a shared
    /// edge), or when every selected object is already at
    /// the target (spam-resistance parity with
    /// `recolor_selection`). Returns the number of objects
    /// that actually moved.
    ///
    /// ponytail: integer division for the center cases drops
    /// the trailing half-cell, which matches `nudge_selection`'s
    /// 1-cell integer grid. Sub-pixel alignment is a future
    /// "snap to half-cell" tick.
    pub fn align_selection(&mut self, how: Align) -> usize {
        if self.selected_ids.is_empty() || self.has_draft() {
            return 0;
        }
        let Some(union) = self.selection_bounds() else {
            return 0;
        };
        // Short-circuit when every selected object already
        // satisfies the target edge/center, so the user can
        // spam Ctrl-Shift-L without churning the undo stack.
        let already_aligned = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .all(|o| match get_object_selection_bounds(o) {
                Some(r) => align_delta(r, union, how) == (0, 0),
                None => true,
            });
        if already_aligned {
            return 0;
        }
        self.push_undo();
        let mut moved = 0;
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            let Some(r) = get_object_selection_bounds(obj) else {
                continue;
            };
            let (dx, dy) = align_delta(r, union, how);
            if dx == 0 && dy == 0 {
                continue;
            }
            *obj = translate_object(obj, dx, dy);
            moved += 1;
        }
        self.mark_dirty();
        moved
    }

    /// Distribute the selection along the chosen axis so the gaps
    /// between consecutive items (measured by their center on
    /// that axis) are equal. Endpoints are pinned: only the
    /// inner `n-2` objects translate. Returns the count of
    /// objects that actually moved.
    ///
    /// Needs ≥3 selected objects with bounds — fewer is a no-op
    /// (two items have one gap, which IS the whole selection;
    /// nothing to redistribute). An in-progress draft also
    /// short-circuits to 0 (parity with `align_selection`).
    ///
    /// ponytail: integer-division arithmetic on centers. The
    /// trailing half-cell bias is the same trade-off as
    /// `align_delta` / `nudge_selection`'s 1-cell grid — a
    /// future snap-to-half-cell tick could revisit all three
    /// at once.
    pub fn distribute_selection(&mut self, axis: DistributeAxis) -> usize {
        if self.selected_ids.len() < 3 || self.has_draft() {
            return 0;
        }
        // Collect (doc-index, center-on-axis) for every selected
        // object that has a selection-bounds rect. Paint with
        // an empty stroke could miss; skip it for safety
        // (matches how `selection_bounds` filters).
        let mut entries: Vec<(usize, i32)> = self
            .document
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| self.selected_ids.contains(o_id(o)))
            .filter_map(|(i, o)| {
                let r = get_object_selection_bounds(o)?;
                let center = match axis {
                    DistributeAxis::Horizontal => i32::midpoint(r.left, r.right),
                    DistributeAxis::Vertical => i32::midpoint(r.top, r.bottom),
                };
                Some((i, center))
            })
            .collect();
        if entries.len() < 3 {
            return 0;
        }
        // Stable sort so ties (two objects with identical center)
        // keep their relative input order. Rust's sort_by is
        // stable.
        entries.sort_by_key(|(_, c)| *c);
        // ponytail: direct indexing instead of `.first().unwrap()`
        // / `.last().unwrap()`. The `len() < 3` guard above means
        // entries has at least 3 elements, so `entries[0]` and
        // `entries[len-1]` are always in-bounds — but the unwraps
        // read as "panic if the guard ever moves", and a future
        // refactor that drops the guard (or hoists the sort above
        // it) wouldn't trip the test suite because the inputs
        // today always satisfy the invariant. Indexing makes the
        // invariant explicit and keeps the panic-in-event-loop
        // audit happy: this helper is on the bin's hot path and
        // must never panic on user input.
        let first = entries[0].1;
        let last = entries[entries.len() - 1].1;
        let n = entries.len() as i32;
        let gap = (last - first) / (n - 1);
        // Spam-resistance: compute every middle object's target
        // and check whether all of them already land there. If
        // so, the user can re-trigger the chord without undo
        // churn (parity with align_selection's `already_aligned`).
        let targets: Vec<i32> = (0..entries.len())
            .map(|i| first + (i as i32) * gap)
            .collect();
        let already = entries.iter().zip(targets.iter()).all(|((_, c), t)| c == t);
        if already {
            return 0;
        }
        self.push_undo();
        let mut moved = 0;
        for (i, (doc_idx, current_center)) in entries.iter().enumerate() {
            // Endpoints (i == 0 and i == n-1) stay pinned.
            if i == 0 || i + 1 == entries.len() {
                continue;
            }
            let target = targets[i];
            if *current_center == target {
                continue;
            }
            let delta = target - *current_center;
            let (dx, dy) = match axis {
                DistributeAxis::Horizontal => (delta, 0),
                DistributeAxis::Vertical => (0, delta),
            };
            self.document.objects[*doc_idx] =
                translate_object(&self.document.objects[*doc_idx], dx, dy);
            moved += 1;
        }
        self.mark_dirty();
        moved
    }

    /// Tag every selected object with the same freshly-generated
    /// parent id. Returns the new parent id when at least one
    /// object was tagged; returns `None` when the selection is
    /// empty (no-op, no undo, no dirty). The new id is generated
    /// via `new_object_id("g")` so a glance at the document
    /// reveals what's a group.
    ///
    /// ponytail: grouping is metadata-only. No transform parent,
    /// no nested bounds math, no children-move-with-parent
    /// behavior. The user's `parent_id` field has been on every
    /// variant since v0.1.0 as JSON-clean metadata; today we're
    /// just wiring a setter. A real "group is a transform parent"
    /// UX is a future tick (status, hit-test, multi-select
    /// propagation all want a coherent design first).
    pub fn group_selection(&mut self) -> Option<String> {
        if self.selected_ids.is_empty() {
            return None;
        }
        let parent = crate::doc::new_object_id("g");
        self.push_undo();
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            match obj {
                DrawObject::Box(o) => o.parent_id = Some(parent.clone()),
                DrawObject::Line(o) => o.parent_id = Some(parent.clone()),
                DrawObject::Elbow(o) => o.parent_id = Some(parent.clone()),
                DrawObject::Paint(o) => o.parent_id = Some(parent.clone()),
                DrawObject::Text(o) => o.parent_id = Some(parent.clone()),
            }
        }
        self.mark_dirty();
        Some(parent)
    }

    /// Clear `parent_id` on every selected object. Returns the
    /// number of objects whose parent was actually cleared (a
    /// "grouped-only-once" user pressing ungroup a second time
    /// gets zero, no undo churn). No-op (no undo, no dirty) when
    /// the selection is empty.
    pub fn ungroup_selection(&mut self) -> usize {
        if self.selected_ids.is_empty() {
            return 0;
        }
        // Short-circuit when nothing in the selection has a parent
        // — matches the recolor/restyle helpers' "spam the key
        // without churning undo" behavior.
        let any_grouped = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .any(|o| o.parent_id().is_some());
        if !any_grouped {
            return 0;
        }
        self.push_undo();
        let mut cleared = 0;
        for obj in self.document.objects.iter_mut() {
            if !self.selected_ids.contains(o_id(obj)) {
                continue;
            }
            if obj.parent_id().is_none() {
                continue;
            }
            match obj {
                DrawObject::Box(o) => o.parent_id = None,
                DrawObject::Line(o) => o.parent_id = None,
                DrawObject::Elbow(o) => o.parent_id = None,
                DrawObject::Paint(o) => o.parent_id = None,
                DrawObject::Text(o) => o.parent_id = None,
            }
            cleared += 1;
        }
        self.mark_dirty();
        cleared
    }

    /// Replace the content of a single Text object by id. Pushes one
    /// undo step. Returns true if a Text with `id` was found and
    /// updated; false otherwise (caller should leave edit mode in that
    /// case — the object vanished under us, perhaps via an external
    /// load). No-op when `new_content` equals the current content so
    /// commit-on-empty-edit doesn't churn the undo stack.
    pub fn replace_text_content(&mut self, id: &str, new_content: &str) -> bool {
        let Some(idx) = self.find_object_index(id) else {
            return false;
        };
        // Read the current content (immutable borrow) so we can decide
        // whether to push_undo before mutating.
        let same = match self.document.objects.get(idx) {
            Some(DrawObject::Text(t)) => t.content == new_content,
            // ponytail: id matched a non-Text object — would only
            // happen if a caller passed a stale id. Returning
            // false surfaces the bug via the caller's "leave
            // edit mode" path without panicking. The pre-check
            // at the top of this function already filters to
            // indices that exist, so the `None` arm handles
            // "no such id" and this arm handles "wrong kind".
            _ => return false,
        };
        if same {
            return true;
        }
        self.push_undo();
        if let Some(DrawObject::Text(t)) = self.document.objects.get_mut(idx) {
            t.content = new_content.to_string();
        }
        self.mark_dirty();
        true
    }

    /// Write the content of a single Text object by id without
    /// pushing an undo step or flipping the document dirty flag.
    /// Returns true if the content changed; false if the id is
    /// unknown, the object isn't a Text, or the content was
    /// already equal (spam-resistant no-op).
    ///
    /// This is the F2-edit write-through path: typed chars and
    /// backspace updates are written to the document on every
    /// keystroke so the rendered scene reflects the buffer live,
    /// but undo / dirty stay anchored to the eventual commit.
    /// The commit path (`commit_text_content`) is what actually
    /// marks the document dirty and pushes the single undo step
    /// that captures the whole edit session.
    ///
    /// ponytail: paired helper to `replace_text_content` and
    /// `commit_text_content`. Three paths, three contracts:
    ///
    /// - `write_text_content` — per-keystroke live mirror, no
    ///   side effects, no-op if unchanged.
    /// - `replace_text_content` — public content-replacement
    ///   API: same-content short-circuits, otherwise push
    ///   undo + mark dirty.
    /// - `commit_text_content` — commit anchor: ALWAYS push
    ///   undo + mark dirty, regardless of whether the content
    ///   changed. The buffer is by construction different from
    ///   the pre-edit snapshot; the same-content short-circuit
    ///   would drop the very side effects the user expects.
    ///
    /// Don't unify behind a flag — three distinct contracts
    /// earn three distinct helpers.
    pub fn write_text_content(&mut self, id: &str, new_content: &str) -> bool {
        let Some(idx) = self.find_object_index(id) else {
            return false;
        };
        // Read the current content (immutable borrow) so we can
        // short-circuit when nothing changed. Same match-arm
        // shape as `replace_text_content`: a stale id hitting a
        // non-Text object returns false rather than panicking.
        let same = match self.document.objects.get(idx) {
            Some(DrawObject::Text(t)) => t.content == new_content,
            _ => return false,
        };
        if same {
            return true;
        }
        if let Some(DrawObject::Text(t)) = self.document.objects.get_mut(idx) {
            t.content = new_content.to_string();
        }
        true
    }

    /// Commit-side content write for a single Text object by id.
    /// Pushes an undo step and flips the document dirty flag
    /// when `new_content != initial_content`. The undo snapshot
    /// captures `initial_content` (the pre-edit state), so a
    /// follow-up Ctrl-Z rolls back to what the user had before
    /// opening F2.
    ///
    /// Algorithm: write-through has already mirrored the buffer
    /// onto `doc.content`, so a naive `push_undo` would capture
    /// the post-edit state and Ctrl-Z would be a no-op. We
    /// temporarily revert `doc.content` to `initial_content`,
    /// push the undo snapshot, then re-apply the buffer. The
    /// user never sees the revert because it's masked by the
    /// push+restore.
    ///
    /// Returns true if the write happened (target existed and
    /// was a Text and content actually changed). Returns true
    /// without side effects when content equals initial (the
    /// commit was a no-op — no undo, no dirty, just an ack).
    /// Returns false when the id is unknown or not a Text.
    ///
    /// ponytail: paired helper to `write_text_content`. The
    /// two-path split (write-through per keystroke, commit
    /// with explicit initial) keeps undo + dirty semantics
    /// clean without smuggling a flag through the API.
    pub fn commit_text_content(
        &mut self,
        id: &str,
        new_content: &str,
        initial_content: &str,
    ) -> bool {
        let Some(idx) = self.find_object_index(id) else {
            return false;
        };
        let is_text = matches!(self.document.objects.get(idx), Some(DrawObject::Text(_)));
        if !is_text {
            return false;
        }
        // No-op commit: buffer matches initial, the user opened
        // F2 and committed without typing. No undo, no dirty,
        // just an ack so the caller can show "no changes".
        if new_content == initial_content {
            return true;
        }
        // Temporarily revert doc.content to initial_content
        // so push_undo captures the pre-edit snapshot. The
        // restore below is unconditional so any early return
        // path keeps the document consistent.
        let prior = std::mem::replace(
            &mut self.document.objects[idx],
            DrawObject::Text(TextObject {
                id: id.to_string(),
                z: 0,
                parent_id: None,
                color: InkColor::White,
                x: 0,
                y: 0,
                content: initial_content.to_string(),
                border: TextBorderMode::None,
            }),
        );
        self.push_undo();
        // Restore the write-through'd content (the buffer the
        // user just typed).
        self.document.objects[idx] = prior;
        // And overwrite the content field with the buffer
        // value, in case `prior.content` was something else
        // (it should be equal to new_content, but be explicit).
        if let Some(DrawObject::Text(t)) = self.document.objects.get_mut(idx) {
            t.content = new_content.to_string();
        }
        self.mark_dirty();
        true
    }

    /// Revert a Text object's content to `initial_content` without
    /// pushing an undo step or flipping dirty. Used by the F2
    /// cancel path: write-through mirrored the user's mid-edit
    /// buffer onto the document, but Esc should leave the doc
    /// as if F2 was never opened.
    ///
    /// No-op (returns true without side effects) when
    /// `current_content == initial_content` — nothing to revert.
    /// Returns false when the id is unknown or not a Text.
    pub fn revert_text_content(&mut self, id: &str, initial_content: &str) -> bool {
        let Some(idx) = self.find_object_index(id) else {
            return false;
        };
        if !matches!(self.document.objects.get(idx), Some(DrawObject::Text(_))) {
            return false;
        }
        if let Some(DrawObject::Text(t)) = self.document.objects.get_mut(idx) {
            if t.content == initial_content {
                return true;
            }
            t.content = initial_content.to_string();
        }
        true
    }

    /// Read the current content of a single Text object by id.
    /// Returns None if the object isn't found or isn't a Text.
    /// Used to seed the edit buffer when entering text-entry mode.
    pub fn text_content(&self, id: &str) -> Option<String> {
        self.document
            .objects
            .iter()
            .find(|o| o_id(o) == id)
            .and_then(|o| match o {
                DrawObject::Text(t) => Some(t.content.clone()),
                // ponytail: a hit on a non-Text id means the
                // caller passed the wrong kind — same outcome
                // as a miss, which is fine because the edit-mode
                // path bails either way.
                _ => None,
            })
    }

    /// Borrow the full `TextObject` for a given id. Returns None
    /// if the id isn't present or isn't a Text. Used by the F2
    /// cursor overlay to compute the buffer-end cell without
    /// re-walking the document.
    ///
    /// ponytail: `text_content` already walks the document and
    /// clones the buffer; this is the same walk but returns the
    /// whole struct, which the cursor helper needs for `x` and
    /// `border`. No second pass — caller pulls both fields from
    /// the same `Option<&TextObject>`.
    pub fn text_object(&self, id: &str) -> Option<&TextObject> {
        self.document
            .objects
            .iter()
            .find(|o| o_id(o) == id)
            .and_then(|o| match o {
                DrawObject::Text(t) => Some(t),
                _ => None,
            })
    }

    /// Clone every selected object with a fresh id, nudge by (+1, +1)
    /// so the copy is visibly offset, push one undo snapshot, and
    /// replace the selection with the new ids. Returns the new ids
    /// (in original selection order) for callers that want to chain
    /// (e.g. immediately nudge further). No-op when nothing is
    /// selected or when a draft is in flight.
    pub fn duplicate_selected(&mut self) -> Vec<String> {
        if self.selected_ids.is_empty() || self.has_draft() {
            return Vec::new();
        }
        // Snapshot the originals first; we capture their geometry
        // below, then push one undo step before mutating the document.
        let originals: Vec<DrawObject> = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o.id()))
            .cloned()
            .collect();
        if originals.is_empty() {
            return Vec::new();
        }
        self.push_undo();

        let mut new_ids = Vec::with_capacity(originals.len());
        for obj in originals {
            // Fresh id per object so undo history and snapshot
            // diffs stay unique even if two originals share an id
            // prefix.
            let fresh = crate::new_object_id(obj.id());
            let clone = crate::clone_object_with_id(&obj, &fresh);
            // Translate the duplicate so it doesn't sit on top of
            // the original. +1/+1 matches the visual "drop beside"
            // pattern users expect.
            let clone = crate::translate_object(&clone, 1, 1);
            new_ids.push(fresh);
            self.document.objects.push(clone);
        }
        self.selected_ids.clear();
        for id in &new_ids {
            self.selected_ids.insert(id.clone());
        }
        self.mark_dirty();
        new_ids
    }

    /// Serialize the currently-selected objects as a JSON array of
    /// `DrawObject`s. Empty when nothing is selected. Caller (the bin
    /// crate) is responsible for putting the string on the OS
    /// clipboard; keeping the JSON step here means the format and the
    /// paste-side parser live next to each other in one type-checked
    /// pipeline.
    pub fn serialize_selected_to_json(&self) -> String {
        let selected: Vec<&DrawObject> = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o.id()))
            .collect();
        serde_json::to_string(&selected).unwrap_or_else(|_| "[]".to_string())
    }

    /// Paste objects parsed from a JSON array (the format produced by
    /// `serialize_selected_to_json`). Each pasted object gets a fresh
    /// id, is nudged by (+1, +1) so it's visibly offset from any
    /// in-document copy, and the selection is replaced with the new
    /// ids. Pushes one undo step covering the whole batch. Returns
    /// the new ids. Returns an empty vec when the JSON doesn't parse
    /// to an array of objects — pasting non-kfd content into the
    /// editor is silently a no-op so a stray clipboard shape can't
    /// panic the editor.
    pub fn paste_objects_from_json(&mut self, json: &str) -> Vec<String> {
        let parsed: Result<Vec<DrawObject>, _> = serde_json::from_str(json);
        let Ok(objs) = parsed else {
            return Vec::new();
        };
        if objs.is_empty() || self.has_draft() {
            return Vec::new();
        }
        self.push_undo();
        let mut new_ids = Vec::with_capacity(objs.len());
        for obj in objs {
            let fresh = crate::new_object_id(obj.id());
            let clone = crate::clone_object_with_id(&obj, &fresh);
            let clone = crate::translate_object(&clone, 1, 1);
            new_ids.push(fresh);
            self.document.objects.push(clone);
        }
        self.selected_ids.clear();
        for id in &new_ids {
            self.selected_ids.insert(id.clone());
        }
        self.mark_dirty();
        new_ids
    }

    /// Cut the current selection: serialize it as a JSON array of
    /// `DrawObject`s (the same format `serialize_selected_to_json`
    /// produces) AND remove it from the document in one undo step.
    /// Returns the JSON payload for the caller to push to the OS
    /// clipboard; returns `"[]"` and performs no mutation when the
    /// selection is empty or a draft is in flight. The clipboard
    /// payload is round-trip-compatible with `paste_objects_from_json`
    /// so the user can paste the cut objects back in another session.
    pub fn cut_selected_to_json(&mut self) -> String {
        if self.selected_ids.is_empty() || self.has_draft() {
            return "[]".to_string();
        }
        let payload = self.serialize_selected_to_json();
        if payload == "[]" {
            return "[]".to_string();
        }
        // Mirror `delete_selected`'s resize-guard so commit_resize
        // can't reach a dangling id when the user cuts the box being
        // resized.
        if let Some((id, _, _)) = &self.resize_target {
            if self.selected_ids.contains(id) {
                self.resize_target = None;
            }
        }
        // One undo step covers the whole "snapshot-then-remove" batch
        // so a single Ctrl-Z restores everything that was on the
        // clipboard.
        self.push_undo();
        self.document
            .objects
            .retain(|o| !self.selected_ids.contains(o_id(o)));
        self.selected_ids.clear();
        self.mark_dirty();
        payload
    }

    /// Move the single selected object to the top of the document
    /// object vector (highest z-order). `compose_scene` stamps
    /// objects in vec order, so the back-to-last position is "in
    /// front". No-op when nothing is selected or more than one thing
    /// is selected.
    pub fn bring_to_front(&mut self) -> bool {
        if self.selected_ids.len() != 1 {
            return false;
        }
        let target = match self.selected_ids.iter().next() {
            Some(id) => id.clone(),
            None => return false,
        };
        let Some(idx) = self.find_object_index(target) else {
            return false;
        };
        let already_last = idx + 1 == self.document.objects.len();
        if already_last {
            return false;
        }
        self.push_undo();
        let obj = self.document.objects.remove(idx);
        self.document.objects.push(obj);
        self.mark_dirty();
        true
    }

    /// Mirror of `bring_to_front`: drop the single selected object to
    /// the very first position so it renders beneath everything else.
    pub fn send_to_back(&mut self) -> bool {
        if self.selected_ids.len() != 1 {
            return false;
        }
        let target = match self.selected_ids.iter().next() {
            Some(id) => id.clone(),
            None => return false,
        };
        let Some(idx) = self.find_object_index(target) else {
            return false;
        };
        if idx == 0 {
            return false;
        }
        self.push_undo();
        let obj = self.document.objects.remove(idx);
        self.document.objects.insert(0, obj);
        self.mark_dirty();
        true
    }

    /// Raise the single selected object by one z-step toward the front
    /// (toward the end of the objects vector, which renders on top).
    /// Pairs with `bring_to_front` (which jumps all the way) the same
    /// way Figma's `]` and `Cmd+]` pair: by-one vs. to-extreme.
    /// No-op when the selection is empty, multi, or already at the
    /// last index — same "don't churn undo for no visible change"
    /// policy as `bring_to_front`.
    pub fn bring_forward(&mut self) -> bool {
        if self.selected_ids.len() != 1 {
            return false;
        }
        let target = match self.selected_ids.iter().next() {
            Some(id) => id.clone(),
            None => return false,
        };
        let Some(idx) = self.find_object_index(target) else {
            return false;
        };
        if idx + 1 == self.document.objects.len() {
            return false;
        }
        self.push_undo();
        // Swap with the next index — a single step is a swap, not a
        // pop-and-reinsert.
        self.document.objects.swap(idx, idx + 1);
        self.mark_dirty();
        true
    }

    /// Lower the single selected object by one z-step toward the back
    /// (toward the start of the objects vector, which renders
    /// underneath). Mirror of `bring_forward`. No-op when the
    /// selection is empty, multi, or already at index 0.
    pub fn send_backward(&mut self) -> bool {
        if self.selected_ids.len() != 1 {
            return false;
        }
        let target = match self.selected_ids.iter().next() {
            Some(id) => id.clone(),
            None => return false,
        };
        let Some(idx) = self.find_object_index(target) else {
            return false;
        };
        if idx == 0 {
            return false;
        }
        self.push_undo();
        self.document.objects.swap(idx, idx - 1);
        self.mark_dirty();
        true
    }

    // -- Queries -----------------------------------------------------

    pub fn selection_bounds(&self) -> Option<crate::types::Rect> {
        let rects: Vec<_> = self
            .document
            .objects
            .iter()
            .filter(|o| self.selected_ids.contains(o_id(o)))
            .filter_map(get_object_selection_bounds)
            .collect();
        crate::object::get_bounds_union(&rects)
    }

    pub fn document_bounds(&self) -> Option<crate::types::Rect> {
        let rects: Vec<_> = self
            .document
            .objects
            .iter()
            .filter_map(get_object_bounds)
            .collect();
        crate::object::get_bounds_union(&rects)
    }

    /// The draft + committed objects, for the renderer.
    pub fn all_objects(&self) -> Vec<DrawObject> {
        let mut out = clone_objects(&self.document.objects);
        if let Some(d) = &self.draft_object {
            out.push(d.clone());
        }
        out
    }

    fn next_z(&self) -> i32 {
        self.document
            .objects
            .iter()
            .map(|o| o_z(o) + 1)
            .max()
            .unwrap_or(1)
    }
}

fn o_id(o: &DrawObject) -> &str {
    match o {
        DrawObject::Box(b) => &b.id,
        DrawObject::Line(l) => &l.id,
        DrawObject::Elbow(e) => &e.id,
        DrawObject::Paint(p) => &p.id,
        DrawObject::Text(t) => &t.id,
    }
}

fn o_z(o: &DrawObject) -> i32 {
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
fn align_delta(r: Rect, u: Rect, how: Align) -> (i32, i32) {
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
fn is_degenerate(o: &DrawObject) -> bool {
    match o {
        DrawObject::Box(b) => b.left == b.right && b.top == b.bottom,
        DrawObject::Line(l) => l.x1 == l.x2 && l.y1 == l.y2,
        DrawObject::Elbow(e) => e.x1 == e.x2 && e.y1 == e.y2,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::normalize_rect;

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
    fn select_at_picks_topmost_object() {
        let mut s = DrawState::new();
        // Two objects; the later one is "on top" (higher z, later in
        // the objects vec).
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
        s.document.objects.push(DrawObject::Line(LineObject {
            id: "l2".into(),
            z: 2,
            parent_id: None,
            color: InkColor::White,
            x1: 0,
            y1: 2,
            x2: 5,
            y2: 2,
            style: LineStyle::Light,
        }));
        let picked = s.select_at(Point { x: 2, y: 2 }).unwrap();
        assert_eq!(o_id(picked), "l2");
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

    // ----- `select_at_with_mode` (Shift / Ctrl click honors) -----
    //
    // `select_at` (no mode) is now a thin forwarder that calls
    // `select_at_with_mode(.., Replace)`; the tests below pin
    // the Add / Toggle arms and the "miss clears only for
    // Replace" rule.

    #[test]
    fn select_at_add_preserves_existing_selection_on_hit() {
        // Shift+click on an object: existing selection stays,
        // picked object is added. Mirrors
        // `select_in_rect_add_preserves_existing_selection`.
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
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "b2".into(),
            z: 2,
            parent_id: None,
            color: InkColor::White,
            left: 10,
            top: 10,
            right: 15,
            bottom: 15,
            style: BoxStyle::Light,
        }));
        // Pre-select b1.
        s.selected_ids.insert("b1".into());
        let picked = s
            .select_at_with_mode(Point { x: 12, y: 12 }, SelectionMode::Add)
            .expect("shift+click on b2 must hit");
        assert_eq!(o_id(picked), "b2");
        // Both selected — b1 stays, b2 added.
        assert_eq!(s.selected_count(), 2, "Add preserves existing + adds");
        assert!(s.selected_ids.contains("b1"));
        assert!(s.selected_ids.contains("b2"));
    }

    #[test]
    fn select_at_toggle_flips_membership_on_hit() {
        // Ctrl+click on a selected object: removes it.
        // Ctrl+click on an unselected object: adds it.
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
        // Pre-select b1.
        s.selected_ids.insert("b1".into());
        let picked = s
            .select_at_with_mode(Point { x: 2, y: 2 }, SelectionMode::Toggle)
            .expect("ctrl+click on b1 must hit");
        assert_eq!(o_id(picked), "b1");
        assert!(
            !s.selected_ids.contains("b1"),
            "Toggle on already-selected removes it"
        );
        // Click again — now back in.
        let _ = s.select_at_with_mode(Point { x: 2, y: 2 }, SelectionMode::Toggle);
        assert!(s.selected_ids.contains("b1"), "Toggle on empty set adds it");
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
    fn delete_selected_removes_object() {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Line);
        s.begin_draft(Point { x: 0, y: 0 });
        s.update_draft(Point { x: 3, y: 0 });
        let _id = s.commit_draft().unwrap();
        assert_eq!(s.document.objects.len(), 1);
        assert_eq!(s.delete_selected(), 1);
        assert!(s.document.objects.is_empty());
        // Undo restores the document (selection itself isn't restored —
        // that's a ponytail-scope punt; can be added when needed).
        s.undo();
        assert_eq!(s.document.objects.len(), 1);
    }

    #[test]
    fn move_selected_translates_every_selected() {
        let mut s = DrawState::new();
        s.document.objects.push(DrawObject::Box(BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 4,
            bottom: 3,
            style: BoxStyle::Light,
        }));
        s.selected_ids.insert("b".into());
        s.move_selected(2, 1);
        if let DrawObject::Box(b) = &s.document.objects[0] {
            assert_eq!(b.left, 2);
            assert_eq!(b.top, 1);
            assert_eq!(b.right, 6);
            assert_eq!(b.bottom, 4);
        } else {
            panic!();
        }
    }

    #[test]
    fn duplicate_selected_clones_and_nudges() {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Box);
        s.begin_draft(Point { x: 10, y: 10 });
        s.update_draft(Point { x: 15, y: 13 });
        let original_id = s.commit_draft().unwrap();
        assert_eq!(s.document.objects.len(), 1);

        let new_ids = s.duplicate_selected();
        assert_eq!(new_ids.len(), 1);
        assert_ne!(new_ids[0], original_id, "duplicate must get a fresh id");
        assert_eq!(s.document.objects.len(), 2);
        // Selection moved to the new id.
        assert_eq!(s.selected_count(), 1);
        assert!(s.selected_ids.contains(&new_ids[0]));

        // Original stays put.
        if let DrawObject::Box(b) = &s.document.objects[0] {
            assert_eq!(b.left, 10);
            assert_eq!(b.top, 10);
            assert_eq!(b.right, 15);
            assert_eq!(b.bottom, 13);
        } else {
            panic!("expected original box");
        }
        // Duplicate is offset by +1, +1.
        if let DrawObject::Box(b) = &s.document.objects[1] {
            assert_eq!(b.left, 11);
            assert_eq!(b.top, 11);
            assert_eq!(b.right, 16);
            assert_eq!(b.bottom, 14);
        } else {
            panic!("expected duplicate box");
        }
    }

    #[test]
    fn duplicate_selected_pushes_one_undo_step() {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Box);
        s.begin_draft(Point { x: 0, y: 0 });
        s.update_draft(Point { x: 3, y: 3 });
        s.commit_draft().unwrap();

        s.duplicate_selected();
        assert_eq!(s.document.objects.len(), 2);
        s.undo();
        assert_eq!(
            s.document.objects.len(),
            1,
            "one undo step should remove both the duplicate and any selection movement"
        );
        s.redo();
        assert_eq!(s.document.objects.len(), 2);
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
    fn serialize_selected_to_json_round_trips_through_paste() {
        // Seed one box and select it.
        let (mut s, id) = seeded_box_state();
        assert_eq!(s.selected_ids.len(), 1);
        let json = s.serialize_selected_to_json();
        // The clipboard payload is a JSON array; round-tripping it
        // through paste must yield a fresh object with the same
        // geometry (translated by +1, +1) and a different id.
        let new_ids = s.paste_objects_from_json(&json);
        assert_eq!(new_ids.len(), 1);
        assert_ne!(new_ids[0], id, "paste must mint a fresh id");
        let pasted_bounds = box_bounds(&s, &new_ids[0]).unwrap();
        assert_eq!(pasted_bounds, (11, 11, 21, 21));
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
    fn paste_objects_from_json_pushes_one_undo_step() {
        let (mut s, _id) = seeded_box_state();
        let json = s.serialize_selected_to_json();
        // seeded_box_state has an empty undo_stack (the seeded box is
        // pushed directly, no commit happened). Paste should add one
        // undo step so the user can revert.
        assert!(!s.can_undo());
        let new_ids = s.paste_objects_from_json(&json);
        assert_eq!(new_ids.len(), 1);
        assert!(s.can_undo());
        s.undo();
        // After undo, the pasted object is gone but the seeded
        // selection is still there.
        assert!(s.document.objects.iter().all(|o| o.id() != new_ids[0]));
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
    fn cut_selected_to_json_removes_selection_and_returns_payload() {
        let (mut s, id) = seeded_box_state();
        let json = s.cut_selected_to_json();
        // Payload is a non-empty JSON array (the clipboard gets it).
        assert!(json.starts_with('[') && json.ends_with(']'));
        // The selected object is gone from the document.
        assert!(s.document.objects.is_empty());
        // Selection is cleared post-cut.
        assert!(s.selected_ids.is_empty());
        // Payload round-trips back into the document via paste.
        let new_ids = s.paste_objects_from_json(&json);
        assert_eq!(new_ids.len(), 1);
        assert_ne!(new_ids[0], id);
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
    fn cut_selected_to_json_pushes_one_undo_step() {
        let (mut s, _id) = seeded_box_state();
        let obj_count_before = s.document.objects.len();
        assert!(!s.can_undo(), "seeded state has no undo history");
        let _json = s.cut_selected_to_json();
        assert!(s.can_undo(), "cut must push exactly one undo step");
        // One undo restores the cut objects in a single step —
        // verified by counting, not by stepping the stack twice.
        s.undo();
        assert_eq!(s.document.objects.len(), obj_count_before);
        assert!(!s.can_undo());
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
    fn delete_selected_marks_dirty() {
        let mut s = seed_dirty_box();
        s.delete_selected();
        assert!(s.is_dirty());
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
    fn move_selected_marks_dirty() {
        let mut s = seed_dirty_box();
        s.move_selected(1, 0);
        assert!(s.is_dirty());
    }

    #[test]
    fn duplicate_selected_marks_dirty() {
        let mut s = seed_dirty_box();
        let new_ids = s.duplicate_selected();
        assert!(!new_ids.is_empty());
        assert!(s.is_dirty());
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

    #[test]
    fn text_content_returns_seed_value() {
        let (s, id) = seed_text_object("hello");
        assert_eq!(s.text_content(&id).as_deref(), Some("hello"));
    }

    #[test]
    fn text_content_returns_none_for_missing_id() {
        let s = DrawState::new();
        assert!(s.text_content("nope").is_none());
    }

    #[test]
    fn text_content_returns_none_for_non_text_object() {
        // The seeded-box fixture has a Box; text_content on its id
        // must not return Some — otherwise the edit-mode UI would
        // think a Box is editable.
        let (s, id) = seeded_box_state();
        assert!(s.text_content(&id).is_none());
    }

    #[test]
    fn text_object_returns_full_struct_for_text_id() {
        // text_object is the cursor-overlay's read path: it needs
        // x / border / content for the cursor position helper,
        // not just the content string. The returned struct must
        // match the doc's stored TextObject.
        let (s, id) = seed_text_object("ab\ncd");
        let t = s.text_object(&id).expect("text_object should return Some");
        assert_eq!(t.content, "ab\ncd");
        assert_eq!(t.x, 0);
        assert_eq!(t.y, 0);
    }

    #[test]
    fn text_object_returns_none_for_missing_id() {
        let s = DrawState::new();
        assert!(s.text_object("nope").is_none());
    }

    #[test]
    fn text_object_returns_none_for_non_text_object() {
        // Same miss-on-wrong-kind contract as text_content —
        // a Box id must not yield Some.
        let (s, id) = seeded_box_state();
        assert!(s.text_object(&id).is_none());
    }

    #[test]
    fn replace_text_content_updates_and_pushes_undo() {
        let (mut s, id) = seed_text_object("hello");
        assert!(s.replace_text_content(&id, "world"));
        assert_eq!(s.text_content(&id).as_deref(), Some("world"));
        assert!(s.can_undo());
        s.undo();
        assert_eq!(s.text_content(&id).as_deref(), Some("hello"));
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

    // -- write_text_content (F2 write-through path) -----------------
    //
    // write_text_content is the per-keystroke mirror of
    // replace_text_content. It must mutate the document so the
    // scene redraws the buffer live, but must NOT push undo or
    // flip the dirty flag — both side effects belong to the
    // eventual commit.

    #[test]
    fn write_text_content_updates_text_object_in_place() {
        let (mut s, id) = seed_text_object("hello");
        assert!(s.write_text_content(&id, "hello world"));
        assert_eq!(s.text_content(&id).as_deref(), Some("hello world"));
    }

    #[test]
    fn write_text_content_supports_multiline_content() {
        // F2 Shift+Enter inserts \n into the buffer, then
        // write_text_content stamps that onto the TextObject so
        // the multi-line renderer kicks in.
        let (mut s, id) = seed_text_object("");
        assert!(s.write_text_content(&id, "ab\ncd"));
        assert_eq!(s.text_content(&id).as_deref(), Some("ab\ncd"));
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

    // -- commit_text_content (F2 commit anchor) -------------------
    //
    // The commit path pushes undo (capturing the pre-edit state)
    // and flips dirty when the buffer differs from the initial
    // snapshot. Same-content commits (user opened F2, didn't
    // type, hit Enter) are no-ops: no undo, no dirty. The
    // helper takes both `new_content` and `initial_content`
    // because write-through has already mirrored the buffer
    // onto doc.content, so doc.content alone can't tell us
    // what changed during this edit session.

    #[test]
    fn commit_text_content_writes_buffer_when_changed() {
        let (mut s, id) = seed_text_object("initial");
        s.write_text_content(&id, "hello");
        assert!(!s.is_dirty(), "precondition: write-through is clean");
        let undo_before = s.undo_stack.len();
        assert!(s.commit_text_content(&id, "hello", "initial"));
        assert_eq!(s.text_content(&id).as_deref(), Some("hello"));
        assert!(s.is_dirty(), "commit flips dirty when content changed");
        assert_eq!(
            s.undo_stack.len(),
            undo_before + 1,
            "commit pushes one undo step"
        );
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
    fn commit_text_content_undo_restores_pre_edit_state() {
        // The whole point of the initial_content anchor: Ctrl-Z
        // after commit must roll back to what the user had
        // before opening F2.
        let (mut s, id) = seed_text_object("initial");
        s.write_text_content(&id, "typed-something");
        s.commit_text_content(&id, "typed-something", "initial");
        assert!(s.undo());
        assert_eq!(s.text_content(&id).as_deref(), Some("initial"));
    }

    #[test]
    fn commit_text_content_undo_after_multiline_edit_restores_initial() {
        // F2 Shift+Enter write-through carries \n onto the doc;
        // commit still needs to restore the original content
        // on Ctrl-Z.
        let (mut s, id) = seed_text_object("first\nsecond");
        s.write_text_content(&id, "ab\ncd");
        s.commit_text_content(&id, "ab\ncd", "first\nsecond");
        assert!(s.undo());
        assert_eq!(s.text_content(&id).as_deref(), Some("first\nsecond"));
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

    // -- revert_text_content (F2 cancel path) ---------------------
    //
    // Cancel-with-changes: write-through has mutated
    // doc.content, but Esc must leave the doc as if F2 was never
    // opened. revert_text_content rolls the field back to the
    // pre-edit snapshot.

    #[test]
    fn revert_text_content_restores_initial_after_write_through() {
        let (mut s, id) = seed_text_object("initial");
        s.write_text_content(&id, "typed-something");
        assert_eq!(s.text_content(&id).as_deref(), Some("typed-something"));
        assert!(s.revert_text_content(&id, "initial"));
        assert_eq!(s.text_content(&id).as_deref(), Some("initial"));
        // Revert must not push undo or flip dirty — the user
        // explicitly chose to discard.
        assert!(!s.is_dirty());
        let undo_before = s.undo_stack.len();
        let _ = undo_before;
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
    fn commit_resize_marks_dirty() {
        let mut s = seed_dirty_box();
        s.begin_resize(BoxResizeHandle::BottomRight);
        s.update_resize(Point { x: 9, y: 9 });
        assert!(s.is_resizing());
        s.commit_resize();
        assert!(s.is_dirty());
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

    fn seed_dirty_box() -> DrawState {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Box);
        s.begin_draft(Point { x: 0, y: 0 });
        s.update_draft(Point { x: 3, y: 3 });
        s.commit_draft().unwrap();
        // commit_draft already marks dirty; mark_saved here so each
        // test starts "saved, then do one mutation".
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

    #[test]
    fn bring_to_front_moves_selection_to_last_index() {
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        // Select B by clicking inside it.
        s.clear_selection();
        s.select_at(Point { x: 6, y: 1 });
        assert_eq!(s.selected_count(), 1);

        assert!(s.bring_to_front());
        assert_eq!(
            doc_ids(&s),
            vec![id_a.as_str(), id_c.as_str(), id_b.as_str()],
            "B should jump to the end of the doc vector"
        );
        assert!(s.is_dirty());
    }

    #[test]
    fn send_to_back_moves_selection_to_index_zero() {
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        s.select_at(Point { x: 6, y: 1 });

        assert!(s.send_to_back());
        assert_eq!(
            doc_ids(&s),
            vec![id_b.as_str(), id_a.as_str(), id_c.as_str()],
            "B should drop to the start of the doc vector"
        );
        assert!(s.is_dirty());
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
    fn z_order_noop_with_empty_selection_is_false() {
        let mut s = DrawState::new();
        assert!(!s.send_to_back());
        assert!(!s.bring_to_front());
        assert!(!s.bring_forward());
        assert!(!s.send_backward());
    }

    #[test]
    fn bring_forward_swaps_with_next_index() {
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        // Select A (first); bring_forward should swap A with B.
        s.select_at(Point { x: 1, y: 1 });
        assert!(s.bring_forward());
        assert_eq!(
            doc_ids(&s),
            vec![id_b.as_str(), id_a.as_str(), id_c.as_str()],
            "A should swap with B (one step toward front)"
        );
        assert!(s.is_dirty());
        // One undo restores the pre-step order.
        s.undo();
        assert_eq!(
            doc_ids(&s),
            vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
        );
    }

    #[test]
    fn send_backward_swaps_with_previous_index() {
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        // Select C (last); send_backward should swap C with B.
        s.select_at(Point { x: 11, y: 1 });
        assert!(s.send_backward());
        assert_eq!(
            doc_ids(&s),
            vec![id_a.as_str(), id_c.as_str(), id_b.as_str()],
            "C should swap with B (one step toward back)"
        );
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
    fn bring_forward_then_send_backward_round_trips() {
        // Raise A one step, then lower the same object one step —
        // the two ops cancel out but each must push its own undo
        // step so the user can step backward through the trail.
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        s.select_at(Point { x: 1, y: 1 });
        assert!(s.bring_forward());
        // Selection follows the moved object — confirm by re-selecting.
        s.clear_selection();
        // After bring_forward, A is at index 1 (swapped with B).
        // Select it from its new position to confirm it's still the
        // same object identity-wise.
        s.select_at(Point { x: 1, y: 1 });
        s.bring_forward(); // A is now at index 2, behind C
        assert_eq!(
            doc_ids(&s),
            vec![id_b.as_str(), id_c.as_str(), id_a.as_str()],
            "after two bring_forward, A should be at the tail"
        );
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
    fn recolor_selection_sets_color_on_single_box() {
        let mut s = seed_dirty_box();
        // seed_dirty_box's box is selected after commit. Default ink is
        // White; switch to Red and confirm.
        s.mark_saved();
        let changed = s.recolor_selection(InkColor::Red);
        assert_eq!(changed, 1);
        assert!(s.is_dirty());
        if let DrawObject::Box(b) = &s.document.objects[0] {
            assert_eq!(b.color, InkColor::Red);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn recolor_selection_pushes_one_undo_step_for_batch() {
        // Multi-select recolor must collapse to a single undo entry so
        // Ctrl-Z reverts the whole recolor in one go.
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        // select_at replaces the selection; multi-select has to go
        // through the test module's direct insert, mirroring the
        // pattern at line ~1965 (bring_to_front_with_two_selected).
        s.selected_ids.insert(id_a.clone());
        s.selected_ids.insert(id_b.clone());
        s.selected_ids.insert(id_c.clone());
        assert_eq!(s.selected_count(), 3);

        let undo_before = s.undo_stack.len();
        let changed = s.recolor_selection(InkColor::Cyan);
        assert_eq!(changed, 3);
        assert_eq!(
            s.undo_stack.len(),
            undo_before + 1,
            "batch recolor pushes exactly one undo step"
        );
        // One Ctrl-Z restores the pre-recolor colors for all three.
        s.undo();
        for (id, expected_color) in [
            (&id_a, InkColor::White),
            (&id_b, InkColor::White),
            (&id_c, InkColor::White),
        ] {
            let obj = s
                .document
                .objects
                .iter()
                .find(|o| o.id() == id.as_str())
                .expect("object must survive undo");
            assert_eq!(
                obj.color(),
                expected_color,
                "{id} should be back to its original color after one undo"
            );
        }
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
    fn recolor_selection_partial_change_only_counts_changed() {
        // Two boxes selected, one already target color, one not.
        // Returns 1 (not 2), and only one object's color field flips
        // inside the undo step.
        let mut s = seed_three_boxes();
        // Pre-color box B (index 1) Cyan, leave A and C White.
        if let DrawObject::Box(b) = &mut s.document.objects[1] {
            b.color = InkColor::Cyan;
        }
        s.clear_selection();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        s.selected_ids.insert(id_a);
        s.selected_ids.insert(id_b);
        s.mark_saved();

        let changed = s.recolor_selection(InkColor::Cyan);
        assert_eq!(changed, 1, "only A should report a change");
        // A should now be Cyan; B should still be Cyan (no churn).
        let a_color = s.document.objects[0].color();
        let b_color = s.document.objects[1].color();
        assert_eq!(a_color, InkColor::Cyan);
        assert_eq!(b_color, InkColor::Cyan);
        assert!(s.is_dirty());
    }

    // ---- align_selection ----
    //
    // ponytail: alignment is integer-grid, matches nudge_selection's
    // 1-cell grid. Center alignment uses / 2 so an odd-width union
    // drops the trailing half-cell (the leftmost cell is shared).
    // Six directions, batch semantics, single undo step per call.

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
    fn align_left_aligns_all_to_leftmost_edge() {
        // Three 2x2 boxes at x=0, x=5, x=10. The x=0 box is
        // already at the left edge, so `align_selection`
        // moves 2 boxes (the x=5 and x=10 ones snap to left=0);
        // the x=0 box stays put. After the call, every box's
        // `left` edge equals the union's left (= 0).
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::Left), 2);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.left, 0, "{} left should snap to 0", b.id);
            }
        }
    }

    #[test]
    fn align_right_aligns_all_to_rightmost_edge() {
        // Same seed; the rightmost box (right=12) is already
        // at the target, so 2 boxes move to right=12.
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::Right), 2);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.right, 12, "{} right should snap to 12", b.id);
            }
        }
    }

    #[test]
    fn align_top_aligns_all_to_topmost_edge() {
        // All three boxes share the same top (y=0), so every
        // one is already at the target — `moved` is 0 but the
        // undo push is also short-circuited (already_aligned).
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::Top), 0);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.top, 0);
            }
        }
    }

    #[test]
    fn align_bottom_aligns_all_to_bottommost_edge() {
        // Same shape as top: all three share bottom=2.
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::Bottom), 0);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.bottom, 2);
            }
        }
    }

    #[test]
    fn align_horizontal_center_centers_all_on_shared_axis() {
        // Three 2x2 boxes; the union spans x=0..12, center=6.
        // Every box should land with its center at 6.
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::HorizontalCenter), 2);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                let center = i32::midpoint(b.left, b.right);
                assert_eq!(center, 6, "{} horizontal center should be 6", b.id);
            }
        }
    }

    #[test]
    fn align_vertical_center_centers_all_on_shared_axis() {
        // Three boxes share y=0..2, vertical center=1. All
        // already at the target.
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        assert_eq!(s.align_selection(Align::VerticalCenter), 0);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                let center = i32::midpoint(b.top, b.bottom);
                assert_eq!(center, 1, "{} vertical center should be 1", b.id);
            }
        }
    }

    #[test]
    fn align_selection_pushes_one_undo_step_for_batch() {
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let undo_before = s.undo_stack.len();
        s.align_selection(Align::Left);
        assert_eq!(
            s.undo_stack.len(),
            undo_before + 1,
            "batch align pushes exactly one undo step"
        );
        s.undo();
        // Original positions: lefts were 0, 5, 10.
        let lefts: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(b.left)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(lefts, vec![0, 5, 10], "undo restored original positions");
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
    fn align_selection_skips_unselected_objects() {
        // 5 boxes total, 2 selected (the first and last).
        // The first is already at the union's left, so only
        // the last moves.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        make_box_at(&mut s, 10, 0, 12, 2);
        make_box_at(&mut s, 15, 0, 17, 2);
        make_box_at(&mut s, 20, 0, 22, 2);
        s.clear_selection();
        let first_id = s.document.objects[0].id().to_string();
        let last_id = s.document.objects[4].id().to_string();
        s.selected_ids.insert(first_id.clone());
        s.selected_ids.insert(last_id.clone());
        let moved = s.align_selection(Align::Left);
        assert_eq!(moved, 1, "only the last (rightmost) moves");
        // Middle three keep their original lefts; first stays
        // at 0 (was already at target); last snaps to 0.
        let lefts: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(b.left)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(lefts, vec![0, 5, 10, 15, 0], "middle three unchanged");
    }

    // ---- distribute (equal spacing) ----
    //
    // ponytail: mirror of `align_selection` tests. The 3-box seed
    // (centers 1, 6, 11) is already on equal horizontal spacing
    // so most horizontal calls hit the `already` short-circuit;
    // tests that need a real move pick a non-equal starting
    // arrangement and assert the count + the post-state.

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
    fn distribute_horizontal_three_with_middle_off_moves_one() {
        // Seed three boxes, then shift the middle one so the
        // centers become 1, 4, 11. After distribute, the middle
        // should land at center 6 (the average); the two
        // endpoints (1, 11) are pinned. `moved` is 1.
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        // Drag the middle box (left=5, right=7) so its center
        // moves from 6 to 4.
        if let DrawObject::Box(b) = &mut s.document.objects[1] {
            b.left = 3;
            b.right = 5;
        }
        assert_eq!(s.distribute_selection(DistributeAxis::Horizontal), 1);
        let centers: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.left, b.right))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(centers, vec![1, 6, 11], "endpoints pinned, middle at 6");
    }

    #[test]
    fn distribute_vertical_three_with_middle_off_moves_one() {
        // Three 2x2 boxes stacked at y=0, y=5, y=10. After the
        // move, vertical centers should be 1, 6, 11.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 0, 5, 2, 7);
        make_box_at(&mut s, 0, 10, 2, 12);
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        // Mutate the middle so it's no longer on the equal grid.
        if let DrawObject::Box(b) = &mut s.document.objects[1] {
            b.top = 3;
            b.bottom = 5;
        }
        assert_eq!(s.distribute_selection(DistributeAxis::Vertical), 1);
        let centers: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.top, b.bottom))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(centers, vec![1, 6, 11]);
    }

    #[test]
    fn distribute_horizontal_four_creates_three_equal_gaps() {
        // Four boxes at centers 1, 4, 8, 12. After distribute
        // (endpoints pinned at 1 and 12, gap = 11/3 = 3 in
        // integer division), centers should be 1, 4, 8, 12 —
        // wait, those are already on a (3, 4, 4) grid which is
        // NOT equal. So this triggers a real move.
        //
        // Compute the expected: n=4, first=1, last=12, gap =
        // 11/3 = 3. targets: 1, 1+3=4, 1+6=7, 1+9=10. The last
        // endpoint is "pinned" to 12 in the algorithm but the
        // integer-division gap means the algorithm's internal
        // 4th target is 10, not 12. The endpoint SKIP is by
        // index (`i == 0 || i + 1 == entries.len()`), so the
        // last object does NOT move regardless of the math
        // diverging at the end — confirming the endpoint-pin
        // semantics: the last object's CURRENT center is 12,
        // the algorithm would target 10, but i+1==n so it's
        // skipped. Result: centers end up 1, 4, 7, 12.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2); // center 1
        make_box_at(&mut s, 3, 0, 5, 2); // center 4
        make_box_at(&mut s, 7, 0, 9, 2); // center 8
        make_box_at(&mut s, 11, 0, 13, 2); // center 12
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let moved = s.distribute_selection(DistributeAxis::Horizontal);
        // centers sorted: 1, 4, 8, 12. gap = (12-1)/3 = 3.
        // targets: 1, 4, 7, 10. i=0 skip (endpoint), i=1
        // current=4 target=4 no move, i=2 current=8 target=7
        // moves, i=3 skip (endpoint). So moved = 1, not 2.
        assert_eq!(moved, 1, "only the second middle object moves");
        let centers: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.left, b.right))
                } else {
                    None
                }
            })
            .collect();
        // Endpoints stay at 1 and 12; only the second middle
        // slides from 8 to 7. Document the integer-division
        // reality: distribute is not "perfect" in every case
        // — the pin wins, and the moved middle lands at
        // `first + i * gap`. Equal gaps between consecutive
        // *moved* items, with the trailing endpoint gap being
        // the leftover.
        assert_eq!(centers, vec![1, 4, 7, 12]);
    }

    #[test]
    fn distribute_selection_pushes_one_undo_step_for_batch() {
        // 4 selected, 1 undo step; undo restores all 4 positions.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 3, 0, 5, 2);
        make_box_at(&mut s, 7, 0, 9, 2);
        make_box_at(&mut s, 11, 0, 13, 2);
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let centers_before: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.left, b.right))
                } else {
                    None
                }
            })
            .collect();
        let undo_before = s.undo_stack.len();
        s.distribute_selection(DistributeAxis::Horizontal);
        assert_eq!(
            s.undo_stack.len(),
            undo_before + 1,
            "batch distribute pushes exactly one undo step"
        );
        s.undo();
        let centers_after: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.left, b.right))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(centers_after, centers_before, "undo restored positions");
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
    fn distribute_selection_skips_unselected_objects() {
        // 5 boxes total. Select 3 of them (the leftmost, the
        // middle, the rightmost); the other 2 are unselected.
        // Mutate the selected middle so it's not on the equal
        // grid — the algorithm should move only that one, and
        // the unselected boxes must stay put.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2); // center 1 (SELECTED)
        make_box_at(&mut s, 3, 0, 5, 2); // center 4 — UNSELECTED
        make_box_at(&mut s, 6, 0, 8, 2); // center 7 (SELECTED)
        make_box_at(&mut s, 9, 0, 11, 2); // center 10 — UNSELECTED
        make_box_at(&mut s, 12, 0, 14, 2); // center 13 (SELECTED)
        s.clear_selection();
        // Select leftmost, middle, and rightmost by id.
        let selected_ids = [
            s.document.objects[0].id().to_string(),
            s.document.objects[2].id().to_string(),
            s.document.objects[4].id().to_string(),
        ];
        for id in &selected_ids {
            s.selected_ids.insert(id.clone());
        }
        // Selected set: centers 1, 7, 13. Targets: 1, 7, 13.
        // Already on equal — short-circuit. Make the test
        // actually exercise a real move by dragging the
        // selected middle off-grid first.
        if let DrawObject::Box(b) = &mut s.document.objects[2] {
            b.left = 4;
            b.right = 6;
        }
        // Now selected centers: 1, 5, 13. first=1, last=13,
        // n=3, gap = (13-1)/2 = 6. targets: 1, 7, 13.
        let moved = s.distribute_selection(DistributeAxis::Horizontal);
        assert_eq!(moved, 1, "only the selected middle moves");
        let centers: Vec<i32> = s
            .document
            .objects
            .iter()
            .filter_map(|o| {
                if let DrawObject::Box(b) = o {
                    Some(i32::midpoint(b.left, b.right))
                } else {
                    None
                }
            })
            .collect();
        // Selected leftmost stays at 1; selected rightmost stays
        // at 13; selected middle moves from 5 to 7. Unselected
        // boxes (centers 4 and 10) are untouched.
        assert_eq!(centers, vec![1, 4, 7, 10, 13], "unselected stay put");
    }

    // ---- group / ungroup ----
    //
    // ponytail: parent_id is metadata, not a transform parent.
    // The tests lock the surface contract (parent_id gets set,
    // gets cleared, undo round-trips) without claiming any
    // transform propagation — that's a future design decision.

    /// Seed three boxes with **distinct, explicit ids** —
    /// `seed_three_boxes()` (used elsewhere) commits three drafts
    /// in immediate succession, and `new_object_id` keys off
    /// nanoseconds since UNIX_EPOCH, so the three boxes can
    /// collide on fast hardware. Distinct ids are required for
    /// group tests so a `selected_ids.insert(a)` doesn't also
    /// match `b` / `c`.
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
    fn group_selection_sets_parent_id_on_every_selected_object() {
        let mut s = seed_three_boxes_with_distinct_ids();
        s.selected_ids.insert("box-a".into());
        s.selected_ids.insert("box-b".into());
        s.mark_saved();

        let parent = s
            .group_selection()
            .expect("non-empty selection returns parent id");
        assert!(
            parent.starts_with("g-"),
            "parent id should be g-prefixed: {parent}"
        );
        // Both selected objects share the new parent id.
        assert_eq!(s.document.objects[0].parent_id(), Some(parent.as_str()));
        assert_eq!(s.document.objects[1].parent_id(), Some(parent.as_str()));
        // The third (unselected) object is untouched.
        assert_eq!(s.document.objects[2].parent_id(), None);
        assert!(s.is_dirty());
    }

    #[test]
    fn group_selection_pushes_one_undo_step_for_batch() {
        let mut s = seed_three_boxes_with_distinct_ids();
        s.selected_ids.insert("box-a".into());
        s.selected_ids.insert("box-b".into());
        s.mark_saved();

        let undo_before = s.undo_stack.len();
        s.group_selection();
        assert_eq!(s.undo_stack.len(), undo_before + 1);
        // One Ctrl-Z reverts both selections back to no parent.
        assert!(s.undo());
        assert_eq!(s.document.objects[0].parent_id(), None);
        assert_eq!(s.document.objects[1].parent_id(), None);
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
    fn ungroup_selection_clears_parent_id_on_every_selected_grouped_object() {
        let mut s = seed_three_boxes_with_distinct_ids();
        // Group A and B together (push one undo step).
        s.selected_ids.insert("box-a".into());
        s.selected_ids.insert("box-b".into());
        s.group_selection();
        assert!(s.document.objects[0].parent_id().is_some());
        // Now ungroup the same selection.
        let cleared = s.ungroup_selection();
        assert_eq!(cleared, 2);
        assert_eq!(s.document.objects[0].parent_id(), None);
        assert_eq!(s.document.objects[1].parent_id(), None);
        assert!(s.is_dirty());
    }

    #[test]
    fn add_to_selection_inserts_known_id() {
        let mut s = seed_three_boxes_with_distinct_ids();
        assert!(s.add_to_selection("box-a"));
        assert_eq!(s.selected_count(), 1);
        assert_eq!(s.selected()[0].id(), "box-a");
        // Adding the same id again is a no-op on count.
        assert!(s.add_to_selection("box-a"));
        assert_eq!(s.selected_count(), 1);
    }

    #[test]
    fn add_to_selection_unknown_id_is_noop() {
        let mut s = seed_three_boxes_with_distinct_ids();
        assert!(!s.add_to_selection("nope"));
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn add_to_selection_preserves_other_picks() {
        let mut s = seed_three_boxes_with_distinct_ids();
        s.select_id("box-a");
        assert!(s.add_to_selection("box-b"));
        assert_eq!(s.selected_count(), 2);
        let ids: Vec<&str> = s.selected().iter().map(|o| o.id()).collect();
        assert!(ids.contains(&"box-a"), "box-a retained from select_id");
        assert!(ids.contains(&"box-b"), "box-b added via add_to_selection");
    }

    #[test]
    fn toggle_selection_flips_membership() {
        let mut s = seed_three_boxes_with_distinct_ids();
        // First toggle: insert.
        assert!(s.toggle_selection("box-a"));
        assert_eq!(s.selected_count(), 1);
        assert_eq!(s.selected()[0].id(), "box-a");
        // Second toggle: remove.
        assert!(s.toggle_selection("box-a"));
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn toggle_selection_unknown_id_is_noop() {
        let mut s = seed_three_boxes_with_distinct_ids();
        assert!(!s.toggle_selection("nope"));
        assert_eq!(s.selected_count(), 0);
    }

    // Helper: seed a state with two lines and one elbow so restyle
    // tests can exercise all three DrawObject variants that carry a
    // LineStyle.
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
    fn restyle_selection_changes_only_line_and_elbow() {
        // Add a Box to the selection — the restyle must NOT touch the
        // box's BoxStyle (a separate enum) and must not count it in
        // `changed`.
        let mut s = seed_two_lines_one_elbow();
        s.set_tool(DrawMode::Box);
        s.begin_draft(Point { x: 10, y: 0 });
        s.update_draft(Point { x: 14, y: 4 });
        let box_id = s.commit_draft().unwrap();
        s.clear_selection();
        // Multi-select: 2 lines + 1 elbow + 1 box.
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let box_style_before = if let DrawObject::Box(b) = &s.document.objects[3] {
            b.style
        } else {
            panic!("expected box at index 3");
        };

        let changed = s.restyle_selection(LineStyle::Double);
        assert_eq!(changed, 3, "two lines + one elbow");
        assert!(s.is_dirty());
        // Box style untouched (BoxStyle is a separate enum).
        if let DrawObject::Box(b) = &s.document.objects[3] {
            assert_eq!(b.style, box_style_before);
            assert_eq!(b.id.as_str(), box_id);
        } else {
            panic!("expected box");
        }
        // All three styled objects flipped.
        for o in &s.document.objects {
            match o {
                DrawObject::Line(l) => assert_eq!(l.style, LineStyle::Double),
                DrawObject::Elbow(e) => assert_eq!(e.style, LineStyle::Double),
                // ponytail: Box / Paint / Text carry no LineStyle;
                // the assertion intentionally skips them. A new
                // LineStyle-bearing kind would need its own arm.
                _ => {}
            }
        }
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
    fn restyle_boxes_selection_changes_every_selected_box() {
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let undo_before = s.undo_stack.len();
        let changed = s.restyle_boxes_selection(BoxStyle::Heavy);
        assert_eq!(changed, 3);
        assert!(s.is_dirty());
        // One undo step for the whole batch.
        assert_eq!(s.undo_stack.len(), undo_before + 1);
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.style, BoxStyle::Heavy);
            }
        }
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
    fn restyle_boxes_selection_undo_restores_prior_style() {
        // Cycle from Light → Heavy, then Ctrl-Z should restore Light
        // (and clear the dirty flag).
        let mut s = seed_three_boxes();
        s.clear_selection();
        for o in &s.document.objects {
            s.selected_ids.insert(o.id().to_string());
        }
        let undo_before = s.undo_stack.len();
        s.restyle_boxes_selection(BoxStyle::Heavy);
        assert!(s.is_dirty());
        assert_eq!(s.undo_stack.len(), undo_before + 1);
        s.undo();
        // ponytail: undo() restores the document snapshot but
        // does NOT clear the dirty flag on its own — the bin
        // wires Ctrl-Z through handle_undo which calls
        // mark_saved on success. Here we just check the
        // document state, not the dirty flag.
        for o in &s.document.objects {
            if let DrawObject::Box(b) = o {
                assert_eq!(b.style, BoxStyle::Light, "undo should restore Light");
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

    // -- Marquee selection -----------------------------------------

    /// Helper: seed three non-overlapping boxes for marquee tests.
    /// Each test calls `clear_selection` then drives `select_in_rect`
    /// itself.
    fn seed_marquee_boxes() -> (DrawState, Vec<String>) {
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2); // a
        make_box_at(&mut s, 5, 0, 7, 2); // b
        make_box_at(&mut s, 10, 0, 12, 2); // c
        let ids: Vec<String> = s
            .document
            .objects
            .iter()
            .map(|o| o.id().to_string())
            .collect();
        (s, ids)
    }

    #[test]
    fn select_in_rect_replace_selects_only_intersecting() {
        // Marquee covering box b only (rect 4..8, -1..4). Replace
        // mode must drop everything else and leave b selected.
        let (mut s, ids) = seed_marquee_boxes();
        let n = s.select_in_rect(
            Rect {
                left: 4,
                top: -1,
                right: 8,
                bottom: 4,
            },
            SelectionMode::Replace,
        );
        assert_eq!(n, 1);
        assert_eq!(s.selected_count(), 1);
        assert!(s.selected_ids.contains(&ids[1]));
        assert!(!s.selected_ids.contains(&ids[0]));
        assert!(!s.selected_ids.contains(&ids[2]));
    }

    #[test]
    fn select_in_rect_add_preserves_existing_selection() {
        // Pre-select box a; marquee over box b in Add mode must
        // leave a selected AND add b. Total 2.
        let (mut s, ids) = seed_marquee_boxes();
        // commit_draft leaves the last-committed box selected, so
        // clear before seeding our pre-selection.
        s.clear_selection();
        s.selected_ids.insert(ids[0].clone());
        assert_eq!(s.selected_count(), 1);

        let n = s.select_in_rect(
            Rect {
                left: 4,
                top: -1,
                right: 8,
                bottom: 4,
            },
            SelectionMode::Add,
        );
        assert_eq!(n, 2);
        assert!(s.selected_ids.contains(&ids[0]));
        assert!(s.selected_ids.contains(&ids[1]));
        assert!(!s.selected_ids.contains(&ids[2]));
    }

    #[test]
    fn select_in_rect_toggle_flips_membership() {
        // Pre-select box b; marquee over b only in Toggle mode
        // must drop b (was selected → now unselected). Net 0.
        let (mut s, ids) = seed_marquee_boxes();
        s.clear_selection();
        s.selected_ids.insert(ids[1].clone());
        assert_eq!(s.selected_count(), 1);

        let n = s.select_in_rect(
            Rect {
                left: 4,
                top: -1,
                right: 8,
                bottom: 4,
            },
            SelectionMode::Toggle,
        );
        assert_eq!(n, 0);
        assert_eq!(s.selected_count(), 0);
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
    fn select_all_picks_every_object() {
        // 3 objects, all distinct ids. select_all must
        // collect every id into the selection, regardless of
        // draw order.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        make_box_at(&mut s, 10, 0, 12, 2);
        s.clear_selection();
        assert_eq!(s.selected_count(), 0);
        let n = s.select_all();
        assert_eq!(n, 3);
        assert_eq!(s.selected_count(), 3);
    }

    #[test]
    fn select_all_replaces_prior_selection() {
        // Pre-seed a single selection; select_all must wipe
        // it before adding the full set (the "Replace" mode
        // of select_in_rect, not Add). Catches a future
        // regression where select_all calls add_to_selection
        // instead of clearing first.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        // commit_draft auto-selects the just-committed box;
        // s.selected_count() == 1 here.
        assert_eq!(s.selected_count(), 1);
        let n = s.select_all();
        assert_eq!(n, 2);
        assert_eq!(s.selected_count(), 2);
    }

    #[test]
    fn select_all_is_idempotent() {
        // Calling select_all twice must produce the same
        // selection (a 2nd call shouldn't grow the count by
        // accident — common bug when "all" is implemented as
        // "insert every id into the existing set" without
        // clearing first).
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        s.clear_selection();
        let n1 = s.select_all();
        let n2 = s.select_all();
        assert_eq!(n1, 2);
        assert_eq!(n2, 2);
        assert_eq!(s.selected_count(), 2);
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
    fn invert_selection_with_empty_selection_selects_everything() {
        // Empty selection → invert → all 3 selected.
        // Mirrors select_all's count contract.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        make_box_at(&mut s, 0, 5, 2, 7);
        s.clear_selection();
        let n = s.invert_selection();
        assert_eq!(n, 3);
        assert_eq!(s.selected_count(), 3);
    }

    #[test]
    fn invert_selection_with_everything_selected_empties() {
        // The Ctrl-A then Ctrl-Shift-I workflow: grab
        // everything, then flip back to an empty selection.
        let mut s = DrawState::new();
        make_box_at(&mut s, 0, 0, 2, 2);
        make_box_at(&mut s, 5, 0, 7, 2);
        let _ = s.select_all();
        let n = s.invert_selection();
        assert_eq!(n, 0);
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn invert_selection_with_partial_flips_membership() {
        // 4 boxes total, 2 selected (the even ids). Invert:
        // the 2 selected become unselected, the 2
        // unselected become selected. Membership flip is
        // a pure set op.
        let mut s = DrawState::new();
        let ids = [
            make_box_at(&mut s, 0, 0, 2, 2),
            make_box_at(&mut s, 5, 0, 7, 2),
            make_box_at(&mut s, 0, 5, 2, 7),
            make_box_at(&mut s, 5, 5, 7, 7),
        ];
        s.clear_selection();
        s.add_to_selection(&ids[0]);
        s.add_to_selection(&ids[2]);
        let n = s.invert_selection();
        assert_eq!(n, 2, "the 2 unselected ids should now be selected");
        // Original 2 dropped, the other 2 picked up.
        assert!(!s.selected_ids.contains(&ids[0]));
        assert!(s.selected_ids.contains(&ids[1]));
        assert!(!s.selected_ids.contains(&ids[2]));
        assert!(s.selected_ids.contains(&ids[3]));
    }

    #[test]
    fn invert_selection_pushes_one_undo_step() {
        // Inversion is one edit, one undo step. The
        // selection state itself is not in the undo
        // snapshot (push_undo clones `document`, not
        // `selected_ids`; undo's `reconcile_selection`
        // trims ids that no longer reference existing
        // objects but does not replay prior selection
        // membership), so this test only verifies the
        // undo-stack bookkeeping.
        let mut s = DrawState::new();
        let id0 = make_box_at(&mut s, 0, 0, 2, 2);
        let _id1 = make_box_at(&mut s, 5, 0, 7, 2);
        s.clear_selection();
        s.add_to_selection(&id0);
        let undo_before = s.undo_stack.len();
        let n = s.invert_selection();
        assert_eq!(n, 1, "only the unselected id should be selected now");
        assert_eq!(s.undo_stack.len(), undo_before + 1);
    }

    #[test]
    fn invert_selection_twice_returns_to_start() {
        // Inverting twice is the identity — a regression
        // guard against a flip that's only one-way (e.g.,
        // forgetting to re-include the prior unselected
        // set on the second pass).
        let mut s = DrawState::new();
        let ids = [
            make_box_at(&mut s, 0, 0, 2, 2),
            make_box_at(&mut s, 5, 0, 7, 2),
            make_box_at(&mut s, 0, 5, 2, 7),
        ];
        s.clear_selection();
        s.add_to_selection(&ids[1]);
        let before: std::collections::HashSet<String> = s.selected_ids.iter().cloned().collect();
        s.invert_selection();
        s.invert_selection();
        let after: std::collections::HashSet<String> = s.selected_ids.iter().cloned().collect();
        assert_eq!(before, after);
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
    fn select_in_rect_edge_touching_counts_as_intersect() {
        // Marquee that exactly meets a box edge must still select
        // it — matches the existing `rects_intersect` convention.
        let (mut s, ids) = seed_marquee_boxes();
        let n = s.select_in_rect(
            Rect {
                left: 0,
                top: 0,
                right: 2,  // touches the right edge of box a (right=2)
                bottom: 2, // touches the bottom edge of box a
            },
            SelectionMode::Replace,
        );
        assert_eq!(n, 1);
        assert!(s.selected_ids.contains(&ids[0]));
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
    fn z_order_round_trip_through_undo() {
        let mut s = seed_three_boxes();
        let id_a = s.document.objects[0].id().to_string();
        let id_b = s.document.objects[1].id().to_string();
        let id_c = s.document.objects[2].id().to_string();
        s.clear_selection();
        s.select_at(Point { x: 6, y: 1 });

        assert!(s.bring_to_front());
        s.undo();
        assert_eq!(
            doc_ids(&s),
            vec![id_a.as_str(), id_b.as_str(), id_c.as_str()]
        );
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

    #[test]
    fn resize_drag_updates_box_in_place() {
        let (mut s, id) = seeded_box_state();
        assert!(s.begin_resize(BoxResizeHandle::BottomRight));
        s.update_resize(Point { x: 30, y: 25 });
        assert!(s.is_resizing());
        assert_eq!(box_bounds(&s, &id), Some((10, 10, 30, 25)));
        assert!(s.commit_resize());
        assert!(!s.is_resizing());
        assert_eq!(box_bounds(&s, &id), Some((10, 10, 30, 25)));
    }

    #[test]
    fn resize_drag_is_one_undo_step() {
        let (mut s, id) = seeded_box_state();
        s.begin_resize(BoxResizeHandle::BottomRight);
        s.update_resize(Point { x: 30, y: 25 });
        s.update_resize(Point { x: 31, y: 26 });
        s.update_resize(Point { x: 32, y: 27 });
        s.commit_resize();
        assert_eq!(box_bounds(&s, &id), Some((10, 10, 32, 27)));
        s.undo();
        assert_eq!(box_bounds(&s, &id), Some((10, 10, 20, 20)));
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
    fn undo_during_resize_preserves_prior_history() {
        // Regression for the undo/redo-during-resize double-pop bug.
        // `undo` pops its own snapshot, then `cancel_all` →
        // `cancel_resize` pops a second time. With prior history
        // behind the resize, that second pop silently destroys the
        // pre-commit snapshot. After this fix, pressing undo mid-resize
        // still leaves `can_undo()` true so the user can reach the
        // pre-commit state.
        let (mut s, id) = seeded_box_state();

        // First commit so there's something on the undo stack behind
        // the resize.
        s.set_tool(DrawMode::Box);
        s.begin_draft(Point { x: 0, y: 0 });
        s.update_draft(Point { x: 5, y: 5 });
        s.commit_draft().unwrap();
        assert!(s.can_undo(), "commit must populate undo_stack");
        // After one commit there should be exactly one undoable
        // action (the commit) and nothing to redo.
        assert!(!s.can_redo());

        // Begin a resize on the seeded box. Committing a draft clears
        // the selection in this state, so re-select the seeded box.
        s.clear_selection();
        s.selected_ids.insert(id.clone());
        assert!(s.begin_resize(BoxResizeHandle::BottomRight));

        // Drag.
        s.update_resize(Point { x: 30, y: 25 });

        // Undo mid-resize: should pop the begin_resize snapshot and
        // restore the seeded bounds. The prior commit's snapshot must
        // remain on the stack so the user can still reach the pre-
        // commit state.
        assert!(s.undo());
        assert_eq!(box_bounds(&s, &id), Some((10, 10, 20, 20)));
        assert!(
            s.can_undo(),
            "undo mid-resize must not destroy prior history"
        );

        // Second undo must reach the pre-commit state: just the seeded
        // box (no draft box on top of it).
        assert!(s.undo());
        assert_eq!(
            s.document.objects.len(),
            1,
            "second undo must reach pre-commit state (only the seeded box), got {:?}",
            s.document.objects
        );
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
    fn selection_bounds_returns_union_of_selected() {
        let (mut s, _id) = seeded_box_state();
        // The seeded box is at (10,10)-(20,20) and pre-selected.
        let r = s.selection_bounds().expect("selection has bounds");
        assert_eq!((r.left, r.top, r.right, r.bottom), (10, 10, 20, 20));

        // Empty selection → None.
        s.clear_selection();
        assert!(s.selection_bounds().is_none());
    }
}
