//! Undo / redo stacks + the document dirty flag.
//!
//! `push_undo` clones the document onto the undo stack and clears
//! the redo stack; `undo` / `redo` pop and swap. `is_dirty` /
//! `mark_saved` / `mark_dirty` track the unsaved badge.

use super::helpers::MAX_UNDO;

impl super::DrawState {
    // -- Undo / redo -------------------------------------------------

    pub(super) fn push_undo(&mut self) {
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
}
