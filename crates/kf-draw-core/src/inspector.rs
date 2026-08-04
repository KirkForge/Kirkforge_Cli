//! Properties inspector: pure summary of the current selection.
//!
//! The bin's status bar / future side panel renders the
//! [`SelectionSummary`] without touching the document directly —
//! this module is the only place that walks `DrawObject` to read
//! kind-specific fields (`BoxStyle`, `LineStyle`, `TextBorderMode`,
//! content length). Keeping the discrimination here means the UI
//! layer can stay dumb: it just formats fields.
//!
//! ponytail: single-selection only today. Multi-selection
//! inspection ("3 objects, mixed colors") is a separate tick —
//! folding it in now would force the struct to carry a list
//! variant for every field. Add when a user actually asks.

use crate::doc::ObjectKind;
use crate::state::DrawState;
use crate::text_util::visible_cell_count;
use crate::types::{BoxStyle, DrawObject, InkColor, LineStyle, Rect, TextBorderMode};

/// Total cells the renderer will stamp for a piece of text
/// content. Equal to `visible_cell_count(content)` minus one
/// per `\n` — the renderer splits on `\n` and stamps each
/// line on its own row, so a `\n` never paints a cell of its
/// own. `visible_cell_count` alone would over-count because
/// `unicode-width` reports control characters as width 1.
///
/// ponytail: `str::lines()` drops the trailing empty line for
/// strings ending in `\n`, matching `line_count`'s contract
/// — a doc-ending newline doesn't paint an empty row, so it
/// shouldn't add to the cell total either.
fn text_content_cells(content: &str) -> usize {
    content.lines().map(visible_cell_count).sum()
}

/// Read-only view of one selected object's inspector-relevant
/// fields. `None` slots indicate "not applicable to this kind"
/// rather than "unset" — the renderer treats them as "don't
/// show this row".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionSummary {
    /// Object kind (Box / Line / Elbow / Paint / Text).
    pub kind: ObjectKind,
    /// Display id — useful when the inspector is the only place
    /// the user sees the id (e.g., to copy into a script).
    pub id: String,
    /// Z-order value. Higher renders on top.
    pub z: i32,
    /// Ink color used by every kind today.
    pub color: InkColor,
    /// Document-space bounds, when defined. `None` for shapes
    /// whose bounds helper returns None (degenerate rectangles,
    /// zero-length lines, etc.).
    pub bounds: Option<Rect>,
    /// Box border style (Box only). `None` for non-Box kinds.
    pub box_style: Option<BoxStyle>,
    /// Line / elbow stroke style (Line + Elbow only). `None`
    /// for non-stroke kinds.
    pub line_style: Option<LineStyle>,
    /// Text border mode (Text only). `None` for non-Text kinds.
    pub text_border: Option<TextBorderMode>,
    /// Text content length in visible terminal cells, for Text
    /// only. `None` for non-Text kinds.
    ///
    /// `visible_cell_count` is the same helper the renderer
    /// uses to stamp glyphs, so the inspector number matches
    /// what the user sees on screen: newlines count as 0
    /// (they don't paint a cell), wide graphemes count as 2
    /// (they paint two). A multi-line text with `"ab\ncd"`
    /// reports `4` — the four cells the renderer will stamp.
    pub text_len: Option<usize>,
    /// Parent id when this object is part of a group. `None`
    /// when the object is ungrouped. Surfaced in the
    /// inspector so the user can confirm a Ctrl-G took
    /// without expanding the layers panel.
    pub parent_id: Option<String>,
}

/// Build a `SelectionSummary` for `state`. Returns `None` when
/// the selection isn't exactly one object — multi-select
/// inspection is a future tick.
pub fn selection_summary(state: &DrawState) -> Option<SelectionSummary> {
    if state.selected_count() != 1 {
        return None;
    }
    // selected() returns refs to the selected DrawObjects. We
    // already know there's exactly one, so the first element
    // is the only one we need.
    let obj = state.selected().into_iter().next()?;
    Some(summarize(obj, state.selection_bounds()))
}

fn summarize(obj: &DrawObject, bounds: Option<Rect>) -> SelectionSummary {
    // Pull kind-specific fields up-front so the dispatch below
    // is a flat match — easier to read than five nested `if let`s.
    let (box_style, line_style, text_border, text_len) = match obj {
        DrawObject::Box(o) => (Some(o.style), None, None, None),
        DrawObject::Line(o) => (None, Some(o.style), None, None),
        DrawObject::Elbow(o) => (None, Some(o.style), None, None),
        DrawObject::Paint(_) => (None, None, None, None),
        DrawObject::Text(o) => (
            None,
            None,
            Some(o.border),
            Some(text_content_cells(&o.content)),
        ),
    };
    SelectionSummary {
        kind: ObjectKind::of(obj),
        id: obj.id().to_string(),
        z: obj.z(),
        color: obj.color(),
        bounds,
        box_style,
        line_style,
        text_border,
        text_len,
        parent_id: obj.parent_id().map(|s| s.to_string()),
    }
}

/// Format a `SelectionSummary` for a single-line status-bar /
/// side-panel display. Pure: no allocation beyond the returned
/// `String`, no clock / rand / IO. The bin calls this and
/// writes the result verbatim; the core crate owns the field
/// ordering and abbreviation choices so a future "rich
/// inspector" panel doesn't have to re-derive them.
///
/// ponytail: single-line today. A multi-line / table inspector
/// is a separate tick (the formatter would either branch on
/// layout or grow into a Vec<String>).
pub fn format_summary(s: &SelectionSummary) -> String {
    // Color name reused in two arms; pull it once so the
    // template strings below read flat.
    let color = ink_color_name(s.color);
    // Build the kind-specific suffix as its own String so the
    // header (kind/id/z/color) and the suffix join cleanly.
    let suffix = match s.kind {
        ObjectKind::Box => format!(
            " | style={}",
            box_style_name(s.box_style.unwrap_or(BoxStyle::Light))
        ),
        ObjectKind::Line | ObjectKind::Elbow => format!(
            " | style={}",
            line_style_name(s.line_style.unwrap_or(LineStyle::Smooth))
        ),
        ObjectKind::Paint => String::new(),
        ObjectKind::Text => format!(
            " | border={} | len={}",
            text_border_name(s.text_border.unwrap_or(TextBorderMode::None)),
            s.text_len.unwrap_or(0)
        ),
    };
    let bounds = match s.bounds {
        Some(b) => format!(
            " | bounds=({},{})..({},{})",
            b.left, b.top, b.right, b.bottom
        ),
        None => String::new(),
    };
    // parent_id is None for ungrouped objects — the formatter
    // emits nothing for that case so the default status line
    // doesn't gain a dangling "parent=" chunk for the common
    // ungrouped selection.
    let parent = match s.parent_id.as_deref() {
        Some(p) => format!(" | parent={p}"),
        None => String::new(),
    };
    format!(
        "{kind:?} {id} | z={z} | color={color}{bounds}{parent}{suffix}",
        kind = s.kind,
        id = s.id,
        z = s.z,
        color = color,
    )
}

/// Format a `SelectionSummary` as one row per field, intended
/// for the bin's properties inspector panel. The side panel
/// has the vertical room to lay out each field on its own
/// line; the status bar still uses `format_summary` because it
/// only has one line. Returns a `Vec<String>` of rows in
/// display order: id, kind, z, color, then optional bounds,
/// kind-specific (style / border / len), and parent.
///
/// Always-on rows come first so the panel renders the
/// identifying fields at the top regardless of kind. Optional
/// rows are appended in a fixed order (bounds → kind-specific
/// → parent) so a single tick of paint redraws the same
/// layout each time, which makes the panel easier to scan
/// than "the field order depends on the kind".
///
/// ponytail: matches the per-field labels in `format_summary`
/// (`color`, `style`, `border`, `bounds`, `parent`) so users
/// moving from the status bar to the panel see the same
/// vocabulary. Status bar keeps its single-line shape; the
/// panel gets the row layout because the panel has the room.
pub fn format_summary_rows(s: &SelectionSummary) -> Vec<String> {
    // Always-on header: 4 rows. Box capacity of 8 leaves room
    // for the typical 1-4 optional rows (bounds, style/border
    // /len, parent) without reallocating.
    let mut rows: Vec<String> = Vec::with_capacity(8);
    rows.push(format!("id: {}", s.id));
    rows.push(format!("kind: {:?}", s.kind));
    rows.push(format!("z: {}", s.z));
    rows.push(format!("color: {}", ink_color_name(s.color)));
    if let Some(b) = s.bounds {
        rows.push(format!(
            "bounds: ({},{})..({},{})",
            b.left, b.top, b.right, b.bottom
        ));
    }
    match s.kind {
        ObjectKind::Box => {
            let style = box_style_name(s.box_style.unwrap_or(BoxStyle::Light));
            rows.push(format!("style: {style}"));
        }
        ObjectKind::Line | ObjectKind::Elbow => {
            let style = line_style_name(s.line_style.unwrap_or(LineStyle::Smooth));
            rows.push(format!("style: {style}"));
        }
        ObjectKind::Text => {
            let border = text_border_name(s.text_border.unwrap_or(TextBorderMode::None));
            rows.push(format!("border: {border}"));
            rows.push(format!("len: {}", s.text_len.unwrap_or(0)));
        }
        ObjectKind::Paint => {}
    }
    if let Some(p) = s.parent_id.as_deref() {
        rows.push(format!("parent: {p}"));
    }
    rows
}

fn ink_color_name(c: InkColor) -> &'static str {
    match c {
        InkColor::White => "white",
        InkColor::Red => "red",
        InkColor::Orange => "orange",
        InkColor::Yellow => "yellow",
        InkColor::Green => "green",
        InkColor::Cyan => "cyan",
        InkColor::Blue => "blue",
        InkColor::Magenta => "magenta",
    }
}

fn box_style_name(s: BoxStyle) -> &'static str {
    match s {
        BoxStyle::Auto => "auto",
        BoxStyle::Light => "light",
        BoxStyle::Heavy => "heavy",
        BoxStyle::Double => "double",
        BoxStyle::Dashed => "dashed",
    }
}

fn line_style_name(s: LineStyle) -> &'static str {
    match s {
        LineStyle::Smooth => "smooth",
        LineStyle::Light => "light",
        LineStyle::Double => "double",
        LineStyle::Dashed => "dashed",
    }
}

fn text_border_name(b: TextBorderMode) -> &'static str {
    match b {
        TextBorderMode::None => "none",
        TextBorderMode::Single => "single",
        TextBorderMode::Double => "double",
        TextBorderMode::Underline => "underline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        BoxObject, BoxStyle, DrawMode, ElbowObject, ElbowOrientation, InkColor, LineObject,
        LineStyle, TextBorderMode, TextObject,
    };

    fn state_with(obj: DrawObject) -> DrawState {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Select);
        let id = obj.id().to_string();
        s.document.objects.push(obj);
        // select_id is the public seed-by-id helper — it
        // bypasses the hit-test logic that varies per kind
        // (lines need a cell on the rasterized path, paint needs
        // an interior point, text needs the bounds, etc.).
        s.select_id(&id);
        s
    }

    fn box_obj() -> DrawObject {
        DrawObject::Box(BoxObject {
            id: "b1".into(),
            z: 0,
            parent_id: None,
            color: InkColor::Red,
            left: 0,
            top: 0,
            right: 5,
            bottom: 3,
            style: BoxStyle::Double,
        })
    }

    fn line_obj() -> DrawObject {
        DrawObject::Line(LineObject {
            id: "l1".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Green,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 0,
            style: LineStyle::Dashed,
        })
    }

    fn elbow_obj() -> DrawObject {
        DrawObject::Elbow(ElbowObject {
            id: "e1".into(),
            z: 2,
            parent_id: None,
            color: InkColor::Cyan,
            x1: 0,
            y1: 0,
            x2: 4,
            y2: 4,
            style: LineStyle::Light,
            orientation: ElbowOrientation::HorizontalFirst,
        })
    }

    fn text_obj() -> DrawObject {
        DrawObject::Text(TextObject {
            id: "t1".into(),
            z: 3,
            parent_id: None,
            color: InkColor::Yellow,
            x: 0,
            y: 0,
            content: "hi world".into(),
            border: TextBorderMode::Underline,
        })
    }

    fn paint_obj() -> DrawObject {
        // Paint objects always carry ≥ 2 points by construction in
        // the editor; the summary helper doesn't enforce that.
        use crate::types::PaintObject;
        DrawObject::Paint(PaintObject {
            id: "p1".into(),
            z: 4,
            parent_id: None,
            color: InkColor::Magenta,
            points: vec![crate::types::Point { x: 0, y: 0 }],
            brush: "round".into(),
        })
    }

    #[test]
    fn empty_selection_returns_none() {
        let s = DrawState::new();
        assert!(selection_summary(&s).is_none());
    }

    #[test]
    fn multi_selection_returns_none() {
        let mut s = DrawState::new();
        s.set_tool(DrawMode::Select);
        s.document.objects.push(box_obj());
        s.document.objects.push(line_obj());
        // Seed by id, then add the second via select_in_rect.
        // select_id replaces, so the Add-mode rect adds the
        // second id on top of the first.
        s.select_id("b1");
        use crate::types::{Rect, SelectionMode};
        s.select_in_rect(
            Rect {
                left: 0,
                top: 0,
                right: 5,
                bottom: 3,
            },
            SelectionMode::Add,
        );
        assert_eq!(s.selected_count(), 2);
        assert!(selection_summary(&s).is_none());
    }

    #[test]
    fn box_summary_populates_box_style_only() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.kind, ObjectKind::Box);
        assert_eq!(r.id, "b1");
        assert_eq!(r.z, 0);
        assert_eq!(r.color, InkColor::Red);
        assert_eq!(r.box_style, Some(BoxStyle::Double));
        assert!(r.line_style.is_none());
        assert!(r.text_border.is_none());
        assert!(r.text_len.is_none());
    }

    #[test]
    fn line_summary_populates_line_style_only() {
        let s = state_with(line_obj());
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.kind, ObjectKind::Line);
        assert_eq!(r.color, InkColor::Green);
        assert_eq!(r.line_style, Some(LineStyle::Dashed));
        assert!(r.box_style.is_none());
        assert!(r.text_border.is_none());
        assert!(r.text_len.is_none());
    }

    #[test]
    fn elbow_summary_uses_line_style_slot() {
        let s = state_with(elbow_obj());
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.kind, ObjectKind::Elbow);
        assert_eq!(r.line_style, Some(LineStyle::Light));
        assert!(r.box_style.is_none());
    }

    #[test]
    fn text_summary_populates_text_fields() {
        let s = state_with(text_obj());
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.kind, ObjectKind::Text);
        assert_eq!(r.text_border, Some(TextBorderMode::Underline));
        assert_eq!(r.text_len, Some(8)); // "hi world"
        assert!(r.box_style.is_none());
        assert!(r.line_style.is_none());
    }

    #[test]
    fn paint_summary_has_no_kind_specific_slots() {
        // Paint carries only color + bounds today. The summary
        // should populate those and leave every style slot None.
        let s = state_with(paint_obj());
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.kind, ObjectKind::Paint);
        assert_eq!(r.color, InkColor::Magenta);
        assert!(r.box_style.is_none());
        assert!(r.line_style.is_none());
        assert!(r.text_border.is_none());
        assert!(r.text_len.is_none());
    }

    #[test]
    fn bounds_populated_when_selection_bounds_helper_returns_some() {
        // The 5×3 box from `box_obj` produces a non-None bounds
        // via DrawState::selection_bounds. Lock it so a regression
        // in either helper surfaces here.
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        assert!(r.bounds.is_some());
        let b = r.bounds.unwrap();
        assert_eq!(b.left, 0);
        assert_eq!(b.top, 0);
        assert_eq!(b.right, 5);
        assert_eq!(b.bottom, 3);
    }

    #[test]
    fn summary_is_cloneable_for_inspector_history() {
        // Future inspector may want to keep the last summary in
        // memory after the selection changes — Clone lets us do
        // that without re-querying the document.
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        let _copy = r.clone();
        let _copy2 = r.clone();
    }

    // ---- format_summary ----
    //
    // Locks the user-visible string so the bin / future side panel
    // can rely on the format without re-deriving it. Any reformat
    // is a breaking change for downstream tooling that greps the
    // status bar — bump intentionally.

    #[test]
    fn format_summary_box_includes_kind_id_color_z_bounds_and_style() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        // Locked substring set — reformat needs an explicit
        // update here AND in any user-facing docs that quote
        // the status line.
        assert!(out.contains("Box"), "kind label missing: {out}");
        assert!(out.contains("b1"), "id missing: {out}");
        assert!(out.contains("color=red"), "color missing: {out}");
        assert!(out.contains("z=0"), "z missing: {out}");
        assert!(out.contains("style=double"), "box style missing: {out}");
        assert!(out.contains("bounds="), "bounds missing: {out}");
    }

    #[test]
    fn format_summary_line_uses_line_style_label() {
        let s = state_with(line_obj());
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        assert!(out.contains("Line"));
        assert!(out.contains("color=green"));
        assert!(out.contains("style=dashed"));
    }

    #[test]
    fn format_summary_text_shows_border_and_len() {
        let s = state_with(text_obj());
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        assert!(out.contains("Text"));
        assert!(out.contains("border=underline"));
        assert!(
            out.contains("len=8"),
            "expected 'len=8' for 'hi world', got: {out}"
        );
    }

    #[test]
    fn format_summary_paint_omits_style_suffix() {
        // Paint has no kind-specific slots — the formatter
        // shouldn't emit a dangling 'style=…' / 'border=…' /
        // 'len=…' chunk.
        let s = state_with(paint_obj());
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        assert!(out.contains("Paint"));
        assert!(out.contains("color=magenta"));
        assert!(!out.contains("style="), "paint must not emit style: {out}");
        assert!(
            !out.contains("border="),
            "paint must not emit border: {out}"
        );
        assert!(!out.contains("len="), "paint must not emit len: {out}");
    }

    #[test]
    fn summary_carries_parent_id_when_object_is_grouped() {
        // A grouped Box's summary should surface the parent id
        // so the user can see the group tag without opening the
        // layers panel.
        let mut obj = box_obj();
        if let DrawObject::Box(o) = &mut obj {
            o.parent_id = Some("g-test01".into());
        }
        let s = state_with(obj);
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.parent_id.as_deref(), Some("g-test01"));
    }

    #[test]
    fn summary_parent_id_is_none_for_ungrouped_object() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        assert!(r.parent_id.is_none());
    }

    #[test]
    fn format_summary_emits_parent_chunk_when_grouped() {
        let mut obj = box_obj();
        if let DrawObject::Box(o) = &mut obj {
            o.parent_id = Some("g-abc123".into());
        }
        let s = state_with(obj);
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        assert!(
            out.contains("parent=g-abc123"),
            "expected parent id in summary, got: {out}"
        );
    }

    #[test]
    fn format_summary_omits_parent_chunk_when_ungrouped() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        let out = format_summary(&r);
        assert!(
            !out.contains("parent="),
            "ungrouped objects must not emit parent=, got: {out}"
        );
    }

    // ---- format_summary_rows ----
    //
    // The bin's side panel uses these rows directly (one
    // `Line` per `String`), so each row must be self-contained
    // and short enough to fit a 22-cell panel without
    // wrapping. The tests below pin the row count and the
    // substring vocabulary so a future format change surfaces
    // here, not in the panel renderer's diff.

    #[test]
    fn format_summary_rows_box_has_id_kind_z_color_bounds_style() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        // 4 always-on + bounds + style = 6 rows for a Box.
        assert_eq!(rows.len(), 6, "got: {rows:?}");
        assert_eq!(rows[0], "id: b1");
        assert_eq!(rows[1], "kind: Box");
        assert_eq!(rows[2], "z: 0");
        assert_eq!(rows[3], "color: red");
        assert!(rows[4].starts_with("bounds: "), "row 4: {}", rows[4]);
        assert_eq!(rows[5], "style: double");
    }

    #[test]
    fn format_summary_rows_line_emits_line_style_label() {
        let s = state_with(line_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        // 4 always-on + bounds + style = 6 rows.
        assert_eq!(rows.len(), 6, "got: {rows:?}");
        assert_eq!(rows[1], "kind: Line");
        assert_eq!(rows[3], "color: green");
        assert_eq!(rows[5], "style: dashed");
    }

    #[test]
    fn format_summary_rows_elbow_emits_line_style_label() {
        let s = state_with(elbow_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        assert_eq!(rows[1], "kind: Elbow");
        assert_eq!(rows[3], "color: cyan");
        assert_eq!(rows[5], "style: light");
    }

    #[test]
    fn format_summary_rows_text_emits_border_and_len_rows() {
        let s = state_with(text_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        // 4 always-on + bounds + border + len = 7 rows.
        assert_eq!(rows.len(), 7, "got: {rows:?}");
        assert_eq!(rows[1], "kind: Text");
        assert_eq!(rows[3], "color: yellow");
        assert_eq!(rows[5], "border: underline");
        assert_eq!(rows[6], "len: 8");
    }

    #[test]
    fn format_summary_rows_paint_emits_only_always_on_plus_bounds() {
        // Paint has no kind-specific slot — the formatter
        // shouldn't emit a dangling style/border/len row.
        let s = state_with(paint_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        // 4 always-on + bounds = 5 rows.
        assert_eq!(rows.len(), 5, "got: {rows:?}");
        assert_eq!(rows[1], "kind: Paint");
        assert_eq!(rows[3], "color: magenta");
        assert!(!rows.iter().any(|r| r.starts_with("style:")));
        assert!(!rows.iter().any(|r| r.starts_with("border:")));
        assert!(!rows.iter().any(|r| r.starts_with("len:")));
    }

    #[test]
    fn format_summary_rows_appends_parent_when_grouped() {
        let mut obj = box_obj();
        if let DrawObject::Box(o) = &mut obj {
            o.parent_id = Some("g-rows01".into());
        }
        let s = state_with(obj);
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        // Box's 6 rows + parent = 7.
        assert_eq!(rows.len(), 7, "got: {rows:?}");
        assert_eq!(rows[6], "parent: g-rows01");
    }

    #[test]
    fn format_summary_rows_omits_parent_when_ungrouped() {
        let s = state_with(box_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        assert!(
            !rows.iter().any(|r| r.starts_with("parent:")),
            "ungrouped must not emit a parent row, got: {rows:?}"
        );
    }

    #[test]
    fn format_summary_rows_each_row_fits_panel_inner_width() {
        // Panel inner width = 22 cells - 1 left border = 21
        // cells. Rows must be ≤ 21 chars or the panel will
        // wrap, defeating the row layout. Pinned at compile
        // time of the panel so a longer id or bounds can't
        // sneak past.
        use crate::text_util::visible_cell_count;
        let s = state_with(text_obj());
        let r = selection_summary(&s).unwrap();
        let rows = format_summary_rows(&r);
        const INNER: usize = 21;
        for (i, row) in rows.iter().enumerate() {
            let cells = visible_cell_count(row);
            assert!(
                cells <= INNER,
                "row {i} is {cells} cells (> {INNER}): {row:?}"
            );
        }
    }

    // -- text_len cell-count contract ------------------------------
    //
    // visible_cell_count is the same helper the renderer uses
    // to stamp glyphs, so the inspector number must match
    // what's on screen. Pins: \n counts as 0 cells; wide
    // graphemes count as 2 cells; ASCII counts as 1 cell
    // per character.

    fn text_obj_with(content: &str) -> DrawObject {
        DrawObject::Text(TextObject {
            id: "t-cnt".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 0,
            content: content.into(),
            border: TextBorderMode::None,
        })
    }

    #[test]
    fn inspector_text_len_is_visible_cell_count_for_single_line_ascii() {
        // 8 ASCII chars, no newlines, no wide graphemes: 8 cells.
        let s = state_with(text_obj_with("hi world"));
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.text_len, Some(8));
    }

    #[test]
    fn inspector_text_len_excludes_newlines() {
        // Pre-fix, "ab\ncd" reported 5 (chars().count()).
        // Post-fix, the \n contributes 0 cells so it's 4 —
        // matching what the renderer actually stamps.
        let s = state_with(text_obj_with("ab\ncd"));
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.text_len, Some(4));
    }

    #[test]
    fn inspector_text_len_counts_wide_graphemes_as_two_cells() {
        // '你' is a CJK East Asian Wide char: 1 grapheme, 2 cells.
        // Pre-fix, chars().count() returned 1; the renderer paints
        // 2 cells, so the inspector was lying.
        let s = state_with(text_obj_with("你"));
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.text_len, Some(2));
    }

    #[test]
    fn inspector_text_len_mixed_ascii_and_wide_and_newline() {
        // 'a你\nb' = 1 (a) + 2 (你) + 0 (\n) + 1 (b) = 4 cells.
        let s = state_with(text_obj_with("a你\nb"));
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.text_len, Some(4));
    }

    #[test]
    fn inspector_text_len_for_empty_text_is_zero() {
        // Empty string: 0 cells. Stays zero so the format_summary
        // pipe-through `len=0` is a clean signal rather than a
        // panic on unwrap.
        let s = state_with(text_obj_with(""));
        let r = selection_summary(&s).unwrap();
        assert_eq!(r.text_len, Some(0));
    }
}
