//! Scene → ratatui buffer renderer.
//!
//! For each frame the editor calls `render_scene_into(&scene, &mut buf,
//! viewport, scroll)`. We walk the scene grid one cell at a time so
//! per-cell color from `SceneCell` is preserved. Wide graphemes
//! (CJK, emoji) are rendered as two cells by the scene composer; the
//! second cell gets a space + matching color so terminal cursor
//! advance stays correct.
//!
//! Coordinates: the scene's `(origin.x, origin.y)` is a document
//! point. The viewport is a rect in terminal coordinates. We map
//! `(scene_x, scene_y) → (vp_x + scene_x - scroll_x, vp_y + scene_y
//! - scroll_y)`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use kirkforge_draw_core::Scene;

/// Draw `scene` into `buf`, clipped to `viewport`, scrolled by
/// `(scroll_x, scroll_y)` in scene-cell coordinates. Cells outside
/// the scene bounds are left untouched (the buffer is normally
/// cleared by the ratatui frame before this is called).
pub fn render_scene_into(
    scene: &Scene,
    buf: &mut Buffer,
    viewport: Rect,
    scroll_x: i32,
    scroll_y: i32,
) {
    for sy in 0..scene.height {
        let by = viewport.y as i32 + sy - scroll_y;
        if by < viewport.y as i32 || by >= viewport.bottom() as i32 {
            continue;
        }
        for sx in 0..scene.width {
            let bx = viewport.x as i32 + sx - scroll_x;
            if bx < viewport.x as i32 || bx >= viewport.right() as i32 {
                continue;
            }
            let cell = scene.cells[sy as usize][sx as usize];
            if cell.glyph == ' ' {
                continue; // leave the buffer's cleared space as-is
            }
            let style = ink_style(cell.color);
            buf.set_stringn(bx as u16, by as u16, cell.glyph.to_string(), 1, style);
        }
    }
}

/// Map `InkColor` to a ratatui `Color`. `None` means "use the
/// terminal default"; we approximate that with `Color::Reset`.
fn ink_style(c: Option<kirkforge_draw_core::InkColor>) -> Style {
    Style::default().fg(match c {
        Some(kirkforge_draw_core::InkColor::White) => Color::White,
        Some(kirkforge_draw_core::InkColor::Red) => Color::Red,
        Some(kirkforge_draw_core::InkColor::Orange) => Color::Indexed(208),
        Some(kirkforge_draw_core::InkColor::Yellow) => Color::Yellow,
        Some(kirkforge_draw_core::InkColor::Green) => Color::Green,
        Some(kirkforge_draw_core::InkColor::Cyan) => Color::Cyan,
        Some(kirkforge_draw_core::InkColor::Blue) => Color::Blue,
        Some(kirkforge_draw_core::InkColor::Magenta) => Color::Magenta,
        None => Color::Reset,
    })
}

/// Style for selection outline dots. Distinct from regular scene
/// cells so the user can see the marquee even on top of drawn
/// objects (we still leave existing glyphs in place, but the dots
/// fill blank cells inside the bounds).
fn marquee_style() -> Style {
    Style::default()
        .fg(Color::Indexed(244))
        .add_modifier(ratatui::style::Modifier::DIM)
}

/// Map a document point to a viewport coordinate under the given
/// scene origin and scroll offsets. Returns `None` when the point
/// falls outside the viewport (including scrolled-off-screen); the
/// callers skip drawing in that case.
///
/// ponytail: shared by the three overlay painters (selection
/// marquee, resize handles, text-edit cursor). Each used to
/// inline the same 5 lines of arithmetic + bounds-check; this
/// helper makes "project, then clip" read as one step. The math
/// matches the per-cell loop in `render_scene_into` (scene x →
/// viewport.x + sx − scroll_x) — a future tick could unify both
/// if a fourth caller shows up.
fn project_to_viewport(
    doc_point: kirkforge_draw_core::Point,
    scene_origin: kirkforge_draw_core::Point,
    viewport: Rect,
    scroll_x: i32,
    scroll_y: i32,
) -> Option<(u16, u16)> {
    let sx = doc_point.x - scene_origin.x;
    let sy = doc_point.y - scene_origin.y;
    let bx = viewport.x as i32 + sx - scroll_x;
    let by = viewport.y as i32 + sy - scroll_y;
    let inside = bx >= viewport.x as i32
        && bx < viewport.right() as i32
        && by >= viewport.y as i32
        && by < viewport.bottom() as i32;
    inside.then_some((bx as u16, by as u16))
}

/// Draw a dotted marquee around `bounds` (in scene-cell coords) onto
/// `buf`. Cells that already contain a non-blank glyph in the
/// buffer are left alone — the marquee fills empty cells around the
/// selection so existing frames stay readable.
pub fn render_selection_marquee(
    buf: &mut Buffer,
    viewport: Rect,
    scroll_x: i32,
    scroll_y: i32,
    scene_origin: kirkforge_draw_core::Point,
    bounds: kirkforge_draw_core::Rect,
) {
    let style = marquee_style();
    for y in bounds.top..=bounds.bottom {
        for x in bounds.left..=bounds.right {
            // Only on perimeter cells.
            if x != bounds.left && x != bounds.right && y != bounds.top && y != bounds.bottom {
                continue;
            }
            let Some((bx, by)) = project_to_viewport(
                kirkforge_draw_core::Point { x, y },
                scene_origin,
                viewport,
                scroll_x,
                scroll_y,
            ) else {
                continue;
            };
            let cell = &buf[(bx, by)];
            if cell.symbol() == " " {
                buf.set_stringn(bx, by, '·'.to_string(), 1, style);
            }
        }
    }
}

/// Inverted block markers at the four corners of `bounds` so the
/// user can see where to grab the box for a resize. Overwrites
/// whatever is at the corner cell (the box frame glyph) — the
/// inverted color makes the grab point obvious even when the box is
/// empty.
pub fn render_resize_handles(
    buf: &mut Buffer,
    viewport: Rect,
    scroll_x: i32,
    scroll_y: i32,
    scene_origin: kirkforge_draw_core::Point,
    bounds: kirkforge_draw_core::Rect,
) {
    // ponytail: four corners, two chars per marker — keep it dense,
    // users find the corner visually rather than reading a label.
    let style = Style::default()
        .bg(Color::White)
        .fg(Color::Black)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let corners = [
        (bounds.left, bounds.top),
        (bounds.right, bounds.top),
        (bounds.left, bounds.bottom),
        (bounds.right, bounds.bottom),
    ];
    for (x, y) in corners {
        let Some((bx, by)) = project_to_viewport(
            kirkforge_draw_core::Point { x, y },
            scene_origin,
            viewport,
            scroll_x,
            scroll_y,
        ) else {
            continue;
        };
        buf.set_stringn(bx, by, '▪'.to_string(), 1, style);
    }
}

/// F2 text-edit cursor: a single inverted block at the
/// `TextEditState.cursor_offset` cell so the user can see where
/// their next keystroke will land. No blink — the cursor is
/// static per frame; a future tick can layer a timer on top.
///
/// ponytail: byte-offset cursor, not grapheme. The caller passes
/// the doc-space cell that the core helper computed from
/// `cursor_offset`; `render_text_cursor` is purely a paint. Left
/// / Right / Home / End / Up / Down / Delete / Backspace all
/// mutate `cursor_offset` in `app.rs` before this runs. ASCII
/// stepping is 1-byte splices; multi-byte graphemes (CJK, emoji)
/// are 3–4 bytes — Left/Right steps one byte at a time and can
/// briefly land mid-grapheme until the cursor walks out of the
/// multi-byte span. Visually fine today; grapheme-aware
/// stepping is a future tick (would need `unicode-segmentation`
/// to walk grapheme cluster boundaries from the offset).
pub fn render_text_cursor(
    buf: &mut Buffer,
    viewport: Rect,
    scroll_x: i32,
    scroll_y: i32,
    scene_origin: kirkforge_draw_core::Point,
    doc_point: kirkforge_draw_core::Point,
) {
    let style = Style::default()
        .bg(Color::White)
        .fg(Color::Black)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let Some((bx, by)) = project_to_viewport(doc_point, scene_origin, viewport, scroll_x, scroll_y)
    else {
        return;
    };
    buf.set_stringn(bx, by, '█'.to_string(), 1, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirkforge_draw_core::types::{BoxObject, BoxStyle, InkColor};
    use kirkforge_draw_core::{compose_scene, create_scene, types::DrawObject};
    use kirkforge_draw_core::{Point, Rect as KRect};
    use ratatui::layout::Rect;

    fn doc_with_box(left: i32, top: i32, right: i32, bottom: i32) -> Vec<DrawObject> {
        vec![DrawObject::Box(BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left,
            top,
            right,
            bottom,
            style: BoxStyle::Light,
        })]
    }

    fn empty_buffer(width: u16, height: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, width, height))
    }

    #[test]
    fn renders_box_top_left_corner() {
        let mut scene = create_scene(10, 6, Point { x: 0, y: 0 });
        compose_scene(&mut scene, &doc_with_box(0, 0, 5, 3));
        let mut buf = empty_buffer(10, 6);
        render_scene_into(&scene, &mut buf, Rect::new(0, 0, 10, 6), 0, 0);
        let (x, y) = (0, 0);
        let s = buf[(x, y)].symbol();
        assert_eq!(s, "┌", "expected ┌ at (0,0), got {s:?}");
    }

    #[test]
    fn honors_scroll_offset() {
        // Box starts at scene origin; scroll right by 2 cells, top-left
        // corner should appear at viewport x=0.
        let mut scene = create_scene(10, 6, Point { x: 0, y: 0 });
        compose_scene(&mut scene, &doc_with_box(2, 1, 7, 4));
        let mut buf = empty_buffer(10, 6);
        render_scene_into(&scene, &mut buf, Rect::new(0, 0, 10, 6), 2, 1);
        let s = buf[(0, 0)].symbol();
        assert_eq!(s, "┌");
    }

    #[test]
    fn clips_outside_viewport() {
        // Buffer is 20x20; viewport is 5x5 at (15,15). Cells outside
        // the viewport bounds must be left untouched (still space).
        let mut scene = create_scene(10, 10, Point { x: 0, y: 0 });
        compose_scene(&mut scene, &doc_with_box(0, 0, 5, 3));
        let mut buf = empty_buffer(20, 20);
        render_scene_into(&scene, &mut buf, Rect::new(15, 15, 5, 5), 0, 0);
        // Cell (0,0) is outside the viewport — stays as space.
        assert_eq!(buf[(0, 0)].symbol(), " ");
        // Cell (14,14) is outside the viewport — stays as space.
        assert_eq!(buf[(14, 14)].symbol(), " ");
    }

    #[test]
    fn blank_cells_are_left_as_space() {
        let mut scene = create_scene(10, 6, Point { x: 0, y: 0 });
        // No objects — all cells are blank.
        compose_scene(&mut scene, &[]);
        let mut buf = empty_buffer(10, 6);
        render_scene_into(&scene, &mut buf, Rect::new(0, 0, 10, 6), 0, 0);
        assert_eq!(buf[(0, 0)].symbol(), " ");
    }

    #[test]
    fn renders_box_with_origin_offset() {
        // Document bounds at (-1, -1) → (3, 2); scene origin = (-2, -2).
        let mut scene = create_scene(6, 5, Point { x: -2, y: -2 });
        compose_scene(&mut scene, &doc_with_box(-1, -1, 3, 2));
        let mut buf = empty_buffer(8, 6);
        render_scene_into(&scene, &mut buf, Rect::new(0, 0, 8, 6), 0, 0);
        // Scene cell (1, 1) is the box top-left corner — it should
        // land at viewport (1, 1).
        assert_eq!(buf[(1, 1)].symbol(), "┌");
    }

    #[test]
    fn marquee_fills_blank_perimeter_cells() {
        let mut buf = empty_buffer(10, 6);
        let bounds = KRect {
            left: 0,
            top: 0,
            right: 5,
            bottom: 3,
        };
        render_selection_marquee(
            &mut buf,
            Rect::new(0, 0, 10, 6),
            0,
            0,
            Point { x: 0, y: 0 },
            bounds,
        );
        // Corners and edge midpoints should be dotted.
        assert_eq!(buf[(0, 0)].symbol(), "·");
        assert_eq!(buf[(5, 0)].symbol(), "·");
        assert_eq!(buf[(0, 3)].symbol(), "·");
        assert_eq!(buf[(5, 3)].symbol(), "·");
        assert_eq!(buf[(2, 0)].symbol(), "·"); // top edge
                                               // Interior cell stays untouched.
        assert_eq!(buf[(2, 1)].symbol(), " ");
    }

    #[test]
    fn marquee_does_not_overwrite_existing_glyphs() {
        let mut buf = empty_buffer(10, 6);
        // Pretend a box corner was already drawn at (0, 0).
        buf[(0, 0)].set_symbol("┌");
        render_selection_marquee(
            &mut buf,
            Rect::new(0, 0, 10, 6),
            0,
            0,
            Point { x: 0, y: 0 },
            KRect {
                left: 0,
                top: 0,
                right: 5,
                bottom: 3,
            },
        );
        // The '┌' stays.
        assert_eq!(buf[(0, 0)].symbol(), "┌");
        // An adjacent blank cell gets a dot.
        assert_eq!(buf[(1, 0)].symbol(), "·");
    }

    #[test]
    fn marquee_respects_scroll_and_origin() {
        let mut buf = empty_buffer(10, 6);
        // Selection in doc coords; scene origin offset and scroll
        // both push it.
        render_selection_marquee(
            &mut buf,
            Rect::new(0, 0, 10, 6),
            2,                      // scroll_x
            1,                      // scroll_y
            Point { x: -1, y: -1 }, // scene origin
            KRect {
                left: 1,
                top: 1,
                right: 4,
                bottom: 3,
            },
        );
        // Doc (1, 1) → scene (2, 2) → viewport (0, 1).
        assert_eq!(buf[(0, 1)].symbol(), "·");
        // Doc (4, 1) → scene (5, 2) → viewport (3, 1).
        assert_eq!(buf[(3, 1)].symbol(), "·");
    }

    #[test]
    fn resize_handles_stamp_four_corners() {
        let mut buf = empty_buffer(10, 6);
        render_resize_handles(
            &mut buf,
            Rect::new(0, 0, 10, 6),
            0,
            0,
            Point { x: 0, y: 0 },
            KRect {
                left: 2,
                top: 1,
                right: 7,
                bottom: 4,
            },
        );
        assert_eq!(buf[(2, 1)].symbol(), "▪");
        assert_eq!(buf[(7, 1)].symbol(), "▪");
        assert_eq!(buf[(2, 4)].symbol(), "▪");
        assert_eq!(buf[(7, 4)].symbol(), "▪");
        // Empty corners untouched.
        assert_eq!(buf[(3, 3)].symbol(), " ");
    }

    #[test]
    fn resize_handles_clip_outside_viewport() {
        let mut buf = empty_buffer(5, 5);
        render_resize_handles(
            &mut buf,
            Rect::new(0, 0, 5, 5),
            0,
            0,
            Point { x: 0, y: 0 },
            KRect {
                left: 100,
                top: 100,
                right: 110,
                bottom: 110,
            },
        );
        // All four corners are out of the viewport; buffer stays blank.
        for y in 0..5 {
            for x in 0..5 {
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn text_cursor_stamps_block_at_doc_point() {
        // Doc (3, 2) with no scroll + zero origin → viewport (3, 2).
        let mut buf = empty_buffer(8, 6);
        render_text_cursor(
            &mut buf,
            Rect::new(0, 0, 8, 6),
            0,
            0,
            Point { x: 0, y: 0 },
            Point { x: 3, y: 2 },
        );
        assert_eq!(buf[(3, 2)].symbol(), "█");
        // Adjacent cells stay blank.
        assert_eq!(buf[(2, 2)].symbol(), " ");
        assert_eq!(buf[(4, 2)].symbol(), " ");
        assert_eq!(buf[(3, 1)].symbol(), " ");
        assert_eq!(buf[(3, 3)].symbol(), " ");
    }

    #[test]
    fn text_cursor_honors_scene_origin_and_scroll() {
        // Doc (5, 1), scene origin (2, 0), scroll (1, 0):
        // sx = 3, sy = 1, bx = 0 + 3 - 1 = 2, by = 0 + 1 - 0 = 1.
        let mut buf = empty_buffer(8, 6);
        render_text_cursor(
            &mut buf,
            Rect::new(0, 0, 8, 6),
            1, // scroll_x
            0, // scroll_y
            Point { x: 2, y: 0 },
            Point { x: 5, y: 1 },
        );
        assert_eq!(buf[(2, 1)].symbol(), "█");
    }

    #[test]
    fn text_cursor_clips_outside_viewport() {
        // Doc point well outside the viewport; buffer stays blank.
        let mut buf = empty_buffer(5, 5);
        render_text_cursor(
            &mut buf,
            Rect::new(0, 0, 5, 5),
            0,
            0,
            Point { x: 0, y: 0 },
            Point { x: 100, y: 100 },
        );
        for y in 0..5 {
            for x in 0..5 {
                assert_eq!(buf[(x, y)].symbol(), " ");
            }
        }
    }

    // Pins the three-overlay projection helper so a math tweak can
    // be made deliberately (and the three renderers' tests catch
    // a regression if it isn't).

    #[test]
    fn project_to_viewport_inside_no_scroll_no_origin() {
        // Doc (3, 2), origin (0, 0), scroll (0, 0), viewport (0,0,10,10)
        // — lands at viewport (3, 2).
        let p = super::project_to_viewport(
            Point { x: 3, y: 2 },
            Point { x: 0, y: 0 },
            Rect::new(0, 0, 10, 10),
            0,
            0,
        );
        assert_eq!(p, Some((3, 2)));
    }

    #[test]
    fn project_to_viewport_applies_origin_and_scroll() {
        // Doc (5, 1), origin (2, 0), scroll (1, 0):
        //   sx = 5 - 2 = 3, sy = 1 - 0 = 1
        //   bx = 0 + 3 - 1 = 2, by = 0 + 1 - 0 = 1
        let p = super::project_to_viewport(
            Point { x: 5, y: 1 },
            Point { x: 2, y: 0 },
            Rect::new(0, 0, 8, 6),
            1,
            0,
        );
        assert_eq!(p, Some((2, 1)));
    }

    #[test]
    fn project_to_viewport_returns_none_when_scrolled_offscreen() {
        // Doc point lives inside the scene but scroll_x has pushed
        // it past the viewport's right edge.
        let p = super::project_to_viewport(
            Point { x: 5, y: 2 },
            Point { x: 0, y: 0 },
            Rect::new(0, 0, 5, 5),
            10,
            0,
        );
        assert_eq!(p, None);
    }

    #[test]
    fn project_to_viewport_returns_none_on_negative_origin_drift() {
        // Doc point inside the scene bounds but origin has drifted
        // so far that the cell lands behind the viewport.
        let p = super::project_to_viewport(
            Point { x: 0, y: 0 },
            Point { x: 100, y: 0 },
            Rect::new(0, 0, 5, 5),
            0,
            0,
        );
        assert_eq!(p, None);
    }
}
