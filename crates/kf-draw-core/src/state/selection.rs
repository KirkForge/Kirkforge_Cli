//! Selection: the set of currently-picked object ids.
//!
//! Single-id, marquee, and hit-test entry points. The set is kept
//! private and mutated only through these methods so the invariant
//! "an id in `selected_ids` refers to a real object" holds.

use std::collections::HashSet;

use crate::object::{get_object_selection_bounds, object_contains_point};
use crate::types::{DrawObject, Point, Rect, SelectionMode};

use super::helpers::o_id;

impl super::DrawState {
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

    pub(super) fn reconcile_selection(&mut self) {
        let live: HashSet<String> = self
            .document
            .objects
            .iter()
            .map(|o| o_id(o).to_string())
            .collect();
        self.selected_ids.retain(|id| live.contains(id));
    }
}
