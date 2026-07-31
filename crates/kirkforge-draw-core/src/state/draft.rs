//! Draft objects: the in-progress shape between `begin_draft` and
//! `commit_draft` / `cancel_draft`. Also holds `cancel_resize` /
//! `cancel_all` (the abort helpers) because they share the
//! "drop in-flight gesture" lifecycle.

use crate::doc::new_object_id;
use crate::line::{append_paint_segment, constrain_line_point};
use crate::types::{
    BoxObject, DrawMode, DrawObject, ElbowObject, ElbowOrientation, LineObject, PaintObject, Point,
    TextObject,
};

use super::helpers::{is_degenerate, o_id};

impl super::DrawState {
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
}
