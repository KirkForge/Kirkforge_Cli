//! Read-only queries: `selection_bounds`, `document_bounds`,
//! `all_objects` (draft + committed, for the renderer), and `next_z`
//! (the next z value for a new object).

use crate::object::{clone_objects, get_object_bounds, get_object_selection_bounds};
use crate::types::DrawObject;

use super::helpers::{o_id, o_z};

impl super::DrawState {
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

    pub(super) fn next_z(&self) -> i32 {
        self.document
            .objects
            .iter()
            .map(|o| o_z(o) + 1)
            .max()
            .unwrap_or(1)
    }
}
