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
//!
//! The state machine is split by domain across the submodules of this
//! file: `tool` (ink setters), `history` (undo / redo / dirty),
//! `selection`, `draft`, `resize`, `mutate` (delete / move / restyle /
//! align / distribute / group / text / clipboard / z-order), and
//! `query` (read-only bounds). `helpers` holds the free functions
//! shared across domains. Each `impl DrawState` block below lives in
//! its own submodule; this file owns the struct + the constructors.

mod draft;
mod helpers;
mod history;
mod mutate;
mod query;
mod resize;
mod selection;
mod tool;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

// Only the types the struct + constructors + `find_object_index`
// reference directly. The test module (`tests.rs`) pulls the rest of
// the draw types + free helpers it needs via its own `use` lines.
use crate::types::{
    BoxResizeHandle, BoxStyle, DrawDocument, DrawMode, DrawObject, InkColor, LineStyle, Point,
    Rect, TextBorderMode,
};
use helpers::o_id;

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

    pub(super) selected_ids: HashSet<String>,
    /// The drag anchor of an in-progress draft. `Some` only between
    /// `begin_draft` and `commit_draft` / `cancel_draft`.
    pub(super) draft_anchor: Option<Point>,
    pub(super) draft_object: Option<DrawObject>,
    /// The most recent constrained point (e.g. line endpoint) so
    /// re-renders between pointer events can use it.
    pub(super) draft_pointer: Option<Point>,

    /// Active resize drag of an already-committed box: the box id, the
    /// pre-drag bounds (kept so undo only rolls back to one snapshot),
    /// and the corner the user grabbed. `None` when not resizing.
    pub(super) resize_target: Option<(String, Rect, BoxResizeHandle)>,

    pub(super) undo_stack: Vec<DrawDocument>,
    pub(super) redo_stack: Vec<DrawDocument>,
    /// True when the document has been mutated since the last
    /// `mark_saved()`. Read by the UI to render a `*` badge on the
    /// status line; cleared by the save handler. `false` for a
    /// freshly-loaded / freshly-built state.
    pub(super) dirty: bool,
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
    /// ponytail: O(n) is fine because normal documents hold a few, upgrade to HashMap index if layer count exceeds 100
    /// dozen objects — adding a `HashMap<String, usize>` would
    /// buy nothing at this scale and would force a parallel-write
    /// discipline on every insert / remove.
    pub(super) fn find_object_index(&self, id: impl AsRef<str>) -> Option<usize> {
        self.document
            .objects
            .iter()
            .position(|o| o_id(o) == id.as_ref())
    }
}
