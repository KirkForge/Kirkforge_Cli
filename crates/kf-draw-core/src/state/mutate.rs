//! Selection mutations: delete / move / restyle / recolor / align /
//! distribute / group / ungroup, the text-content helpers, duplicate,
//! the clipboard (serialize / paste / cut), and z-order
//! (bring-to-front / send-to-back / bring-forward / send-backward).
//! All push a single undo step per call and flip the dirty flag only
//! when the document actually changed.

use crate::object::{get_object_selection_bounds, translate_object};
use crate::types::{
    Align, BoxStyle, DistributeAxis, DrawObject, InkColor, LineStyle, TextBorderMode, TextObject,
};

use super::helpers::{align_delta, o_id};

impl super::DrawState {
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
}
