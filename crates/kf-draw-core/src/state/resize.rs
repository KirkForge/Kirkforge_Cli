//! Box resize drag: `begin_resize` pushes an undo snapshot, then
//! `update_resize` mutates the box in place while the pre-drag
//! bounds stay stashed; `commit_resize` clears the drag state.
//! One undo step covers the whole gesture.

use crate::object::{compute_resized_bounds, get_object_bounds};
use crate::types::{BoxResizeHandle, DrawObject, Point};

use super::helpers::{is_degenerate, o_id};

impl super::DrawState {
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
}
