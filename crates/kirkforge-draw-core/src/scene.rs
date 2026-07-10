//! Scene composer.
//!
//! Takes a flat list of `DrawObject`s and produces a per-cell glyph + color
//! grid sized to a view rect. Box perimeter cells are rendered with
//! connection-aware glyphs so a line that meets a box edge produces a
//! T-junction (┬, ├, ┤, ┴) or a cross (┼) where the connections meet.
//!
//! Mirrors `termdraw`'s `draw-state/scene.ts`. The output is just data —
//! the binary crate's ratatui renderer reads it cell-by-cell.

use crate::geometry::{get_rect_perimeter_points, rect_contains_point};
use crate::line::{get_elbow_render_characters, get_line_render_characters};
use crate::text_util::{get_text_content_origin, get_text_render_rect};
use crate::types::{
    BoxObject, BoxStyle, DrawObject, ElbowObject, InkColor, LineObject, LineStyle, PaintObject,
    Point, Rect, TextObject,
};

// Connection bitmasks for a single cell. N=top neighbor, E=right, S=bottom,
// W=left. A box's top-left corner has N=1 and W=1 (frame continues up and
// left) and E=0, S=0 (the cell IS the corner — nothing to the south/east
// of the corner on the frame line).
pub const CONNECTION_N: u8 = 0b0001;
pub const CONNECTION_E: u8 = 0b0010;
pub const CONNECTION_S: u8 = 0b0100;
pub const CONNECTION_W: u8 = 0b1000;

/// A single cell of the composed scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneCell {
    pub glyph: char,
    pub color: Option<InkColor>,
}

impl Default for SceneCell {
    fn default() -> Self {
        Self {
            glyph: ' ',
            color: None,
        }
    }
}

/// The composed scene grid.
#[derive(Debug, Clone)]
pub struct Scene {
    pub width: i32,
    pub height: i32,
    /// Document-space coordinate of the scene's top-left cell. Use this
    /// to map a cell index back to a document `Point`.
    pub origin: Point,
    pub cells: Vec<Vec<SceneCell>>,
    /// 4-bit neighbor-content bitmask per cell, same indexing as `cells`.
    pub connections: Vec<Vec<u8>>,
}

impl Scene {
    /// Convert a document point to a scene cell index. Returns `None` if
    /// the point lies outside the scene rect.
    pub fn point_to_cell(&self, p: Point) -> Option<(usize, usize)> {
        let x = p.x - self.origin.x;
        let y = p.y - self.origin.y;
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        Some((x as usize, y as usize))
    }

    /// Get the bitmask at the cell containing the given document point.
    pub fn connection_at(&self, p: Point) -> u8 {
        if let Some((x, y)) = self.point_to_cell(p) {
            self.connections[y][x]
        } else {
            0
        }
    }
}

/// Build an empty scene of the given size, anchored at `origin` in
/// document space. `width` and `height` are the scene dimensions in
/// cells; `origin` is the top-left document coordinate.
pub fn create_scene(width: i32, height: i32, origin: Point) -> Scene {
    let cells = vec![vec![SceneCell::default(); width as usize]; height as usize];
    let connections = vec![vec![0u8; width as usize]; height as usize];
    Scene {
        width,
        height,
        origin,
        cells,
        connections,
    }
}

/// Add `delta` to the connection bitmask at `point` in the scene. Used
/// while stamping objects: each perimeter cell of a box adds N/E/S/W
/// bits pointing toward the box's other frame cells.
pub fn adjust_connection(scene: &mut Scene, p: Point, delta: u8) {
    if let Some((x, y)) = scene.point_to_cell(p) {
        scene.connections[y][x] |= delta;
    }
}

/// Paint the cell at `p` with `color`. Does not overwrite a higher-z
/// color — the caller is expected to invoke this in document order so
/// later (higher z) objects win.
pub fn paint_connection_color(scene: &mut Scene, p: Point, color: InkColor) {
    if let Some((x, y)) = scene.point_to_cell(p) {
        scene.cells[y][x].color = Some(color);
    }
}

/// Set the glyph at `p`. The same z-order rule as colors applies.
pub fn stamp_glyph(scene: &mut Scene, p: Point, glyph: char) {
    if let Some((x, y)) = scene.point_to_cell(p) {
        scene.cells[y][x].glyph = glyph;
    }
}

/// Look up the box-drawing glyph for a single perimeter cell, given the
/// cell's connection bitmask and the box's style. Mirrors
/// `getBoxBorderGlyphs` in termdraw.
///
/// The bitmask uses four bits: N=1, E=2, S=4, W=8. A bit is set when the
/// cell has a connection arm pointing in that direction. For a box
/// perimeter, a cell's arms are exactly the directions toward adjacent
/// perimeter cells — e.g. a top-edge cell (between corners) has only
/// E+W arms; the NW corner cell has E+S arms.
///
/// We index the lookup by raw bitmask value rather than a 16-arm match
/// because the mapping is a 1:1 number→glyph dictionary and the
/// number-based form is much easier to keep correct.
pub fn get_box_border_glyph(style: BoxStyle, connections: u8) -> char {
    let is_double = matches!(style, BoxStyle::Double);
    let solid = match connections & 0x0F {
        0b0000 => ' ',
        0b0001 => '╵', // N
        0b0010 => '╶', // E
        0b0011 => '└', // N+E = SW corner glyph (arms N+E)
        0b0100 => '╷', // S
        0b0101 => '│', // N+S
        0b0110 => '┌', // E+S = NW corner glyph (arms E+S)
        0b0111 => '├', // N+E+S
        0b1000 => '╴', // W
        0b1001 => '┘', // N+W = SE corner glyph
        0b1010 => '─', // E+W
        0b1011 => '┴', // N+E+W = T pointing up
        0b1100 => '┐', // S+W = NE corner glyph
        0b1101 => '┤', // N+S+W
        0b1110 => '┬', // E+S+W = T pointing down
        0b1111 => '┼', // cross
        _ => ' ',
    };
    if !is_double {
        return solid;
    }
    // Double-line equivalents for the same bitmasks.
    match connections & 0x0F {
        0b0000 => ' ',
        0b0001 => '╨',
        0b0010 => '╴',
        0b0011 => '╚',
        0b0100 => '╥',
        0b0101 => '║',
        0b0110 => '╔',
        0b0111 => '╠',
        0b1000 => '╶',
        0b1001 => '╝',
        0b1010 => '═',
        0b1011 => '╩',
        0b1100 => '╗',
        0b1101 => '╣',
        0b1110 => '╦',
        0b1111 => '╋',
        _ => ' ',
    }
}

/// Look up the glyph for a line cell, given the connection bitmask. Used
/// when a line meets a box or another line and we need a junction glyph
/// instead of a plain dash or pipe. Same bitmask convention as
/// `get_box_border_glyph`.
pub fn get_connection_glyph(connections: u8, style: LineStyle) -> char {
    let is_double = style == LineStyle::Double;
    let solid = match connections & 0x0F {
        0b0000 => ' ',
        0b0001 => '╵',
        0b0010 => '╶',
        0b0011 => '└',
        0b0100 => '╷',
        0b0101 => '│',
        0b0110 => '┌',
        0b0111 => '├',
        0b1000 => '╴',
        0b1001 => '┘',
        0b1010 => '─',
        0b1011 => '┴',
        0b1100 => '┐',
        0b1101 => '┤',
        0b1110 => '┬',
        0b1111 => '┼',
        _ => ' ',
    };
    if !is_double {
        return solid;
    }
    match connections & 0x0F {
        0b0000 => ' ',
        0b0001 => '╨',
        0b0010 => '╴',
        0b0011 => '╚',
        0b0100 => '╥',
        0b0101 => '║',
        0b0110 => '╔',
        0b0111 => '╠',
        0b1000 => '╶',
        0b1001 => '╝',
        0b1010 => '═',
        0b1011 => '╩',
        0b1100 => '╗',
        0b1101 => '╣',
        0b1110 => '╦',
        0b1111 => '╋',
        _ => ' ',
    }
}

fn perimeter_connections_for(b: &BoxObject, p: Point) -> u8 {
    // A neighbor is "on the perimeter" iff it lies inside the box rect
    // and lies on one of the four edges. The `inside` check is what
    // makes the corner case work — `(0, -1)` is outside the box even
    // though `x == b.left` is true.
    let on_perim = |x: i32, y: i32| {
        x >= b.left
            && x <= b.right
            && y >= b.top
            && y <= b.bottom
            && (x == b.left || x == b.right || y == b.top || y == b.bottom)
    };
    let mut bits = 0u8;
    if on_perim(p.x, p.y - 1) {
        bits |= CONNECTION_N;
    }
    if on_perim(p.x, p.y + 1) {
        bits |= CONNECTION_S;
    }
    if on_perim(p.x + 1, p.y) {
        bits |= CONNECTION_E;
    }
    if on_perim(p.x - 1, p.y) {
        bits |= CONNECTION_W;
    }
    bits
}

fn normalize_brush(brush: &str) -> char {
    use unicode_segmentation::UnicodeSegmentation;
    brush
        .graphemes(true)
        .next()
        .unwrap_or(" ")
        .chars()
        .next()
        .unwrap_or(' ')
}

fn stamp_text(scene: &mut Scene, t: &TextObject) {
    let rect = get_text_render_rect(t);
    let content_origin = get_text_content_origin(t);
    let content = &t.content;
    let widest = crate::text_util::widest_line(content) as i32;
    use unicode_segmentation::UnicodeSegmentation;
    // Iterate per `\n`-separated line so multi-line content
    // stacks. `str::lines()` drops the trailing empty line for
    // strings that end with `\n`, matching `get_text_render_rect` —
    // a doc-ending newline doesn't paint an empty row.
    for (idx, line) in content.lines().enumerate() {
        let y = content_origin.y + idx as i32;
        let mut col = content_origin.x;
        for g in line.graphemes(true) {
            let w = unicode_width::UnicodeWidthStr::width(g) as i32;
            if col > rect.right || col >= scene.origin.x + scene.width {
                break;
            }
            if w == 0 {
                continue;
            }
            let p = Point { x: col, y };
            if rect_contains_point(rect, col, y) {
                // For double-width graphemes, occupy both cells. The second
                // cell must also carry the object color so the renderer's
                // spacer auto-paint matches the glyph's fg.
                for dx in 0..w {
                    let cell_p = Point {
                        x: col + dx,
                        y: p.y,
                    };
                    stamp_glyph(
                        scene,
                        cell_p,
                        if dx == 0 {
                            g.chars().next().unwrap_or(' ')
                        } else {
                            ' '
                        },
                    );
                    paint_connection_color(scene, cell_p, t.color);
                }
            }
            col += w;
        }
    }
    if widest == 0 {
        return;
    }
    stamp_text_frame(scene, t, rect);
}

fn stamp_text_frame(scene: &mut Scene, t: &TextObject, rect: Rect) {
    use crate::types::TextBorderMode;
    match t.border {
        TextBorderMode::None => {}
        TextBorderMode::Underline => {
            // Single horizontal line at the bottom of the rect.
            for x in rect.left..=rect.right {
                let p = Point { x, y: rect.bottom };
                adjust_connection(scene, p, CONNECTION_E | CONNECTION_W);
                paint_connection_color(scene, p, t.color);
                stamp_glyph(scene, p, '─');
            }
        }
        TextBorderMode::Single => {
            let conns_corner_nw = CONNECTION_N | CONNECTION_W;
            let conns_corner_ne = CONNECTION_N | CONNECTION_E;
            let conns_corner_sw = CONNECTION_S | CONNECTION_W;
            let conns_corner_se = CONNECTION_S | CONNECTION_E;
            let conns_top = CONNECTION_E | CONNECTION_W;
            let conns_bottom = CONNECTION_E | CONNECTION_W;
            let conns_left = CONNECTION_N | CONNECTION_S;
            let conns_right = CONNECTION_N | CONNECTION_S;
            let (top, bottom, left, right) = (rect.top, rect.bottom, rect.left, rect.right);
            // Corners.
            for (p, conns) in [
                (Point { x: left, y: top }, conns_corner_nw),
                (Point { x: right, y: top }, conns_corner_ne),
                (Point { x: left, y: bottom }, conns_corner_sw),
                (
                    Point {
                        x: right,
                        y: bottom,
                    },
                    conns_corner_se,
                ),
            ] {
                adjust_connection(scene, p, conns);
                paint_connection_color(scene, p, t.color);
                stamp_glyph(scene, p, get_connection_glyph(conns, LineStyle::Light));
            }
            // Top + bottom edges.
            if right > left + 1 {
                for x in (left + 1)..(right) {
                    let p = Point { x, y: top };
                    adjust_connection(scene, p, conns_top);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '─');
                }
                for x in (left + 1)..(right) {
                    let p = Point { x, y: bottom };
                    adjust_connection(scene, p, conns_bottom);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '─');
                }
            }
            // Left + right edges.
            if bottom > top + 1 {
                for y in (top + 1)..(bottom) {
                    let p = Point { x: left, y };
                    adjust_connection(scene, p, conns_left);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '│');
                    let p = Point { x: right, y };
                    adjust_connection(scene, p, conns_right);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '│');
                }
            }
        }
        TextBorderMode::Double => {
            let conns_top = CONNECTION_E | CONNECTION_W;
            let conns_bottom = CONNECTION_E | CONNECTION_W;
            let conns_left = CONNECTION_N | CONNECTION_S;
            let conns_right = CONNECTION_N | CONNECTION_S;
            let (top, bottom, left, right) = (rect.top, rect.bottom, rect.left, rect.right);
            for (p, ch) in [
                (Point { x: left, y: top }, '╔'),
                (Point { x: right, y: top }, '╗'),
                (Point { x: left, y: bottom }, '╚'),
                (
                    Point {
                        x: right,
                        y: bottom,
                    },
                    '╝',
                ),
            ] {
                adjust_connection(
                    scene,
                    p,
                    CONNECTION_N | CONNECTION_S | CONNECTION_E | CONNECTION_W,
                );
                paint_connection_color(scene, p, t.color);
                stamp_glyph(scene, p, ch);
            }
            if right > left + 1 {
                for x in (left + 1)..(right) {
                    let p = Point { x, y: top };
                    adjust_connection(scene, p, conns_top);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '═');
                    let p = Point { x, y: bottom };
                    adjust_connection(scene, p, conns_bottom);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '═');
                }
            }
            if bottom > top + 1 {
                for y in (top + 1)..(bottom) {
                    let p = Point { x: left, y };
                    adjust_connection(scene, p, conns_left);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '║');
                    let p = Point { x: right, y };
                    adjust_connection(scene, p, conns_right);
                    paint_connection_color(scene, p, t.color);
                    stamp_glyph(scene, p, '║');
                }
            }
        }
    }
}

/// Compose a scene from a list of objects, stamping each into the grid.
/// Later objects in the list overwrite earlier ones at the same cell
/// (so document order = z-order from low to high).
pub fn compose_scene(scene: &mut Scene, objects: &[DrawObject]) {
    // First pass: stamp connection bits and colors from all objects, so
    // boxes know which line cells meet their perimeter before we resolve
    // the box perimeter glyph.
    for obj in objects {
        match obj {
            DrawObject::Box(b) => stamp_box_connections(scene, b),
            DrawObject::Line(l) => stamp_line_connections(scene, l),
            DrawObject::Elbow(e) => stamp_elbow_connections(scene, e),
            DrawObject::Paint(p) => stamp_paint_connections(scene, p),
            DrawObject::Text(_) => {} // text doesn't participate in connection grid
        }
    }
    // Second pass: stamp glyphs.
    for obj in objects {
        match obj {
            DrawObject::Box(b) => stamp_box_glyphs(scene, b),
            DrawObject::Line(l) => stamp_line_glyphs(scene, l),
            DrawObject::Elbow(e) => stamp_elbow_glyphs(scene, e),
            DrawObject::Paint(p) => stamp_paint_glyphs(scene, p),
            DrawObject::Text(t) => stamp_text(scene, t),
        }
    }
}

fn stamp_box_connections(scene: &mut Scene, b: &BoxObject) {
    for p in get_rect_perimeter_points(Rect {
        left: b.left,
        top: b.top,
        right: b.right,
        bottom: b.bottom,
    }) {
        let bits = perimeter_connections_for(b, p);
        adjust_connection(scene, p, bits);
        paint_connection_color(scene, p, b.color);
    }
}

fn stamp_box_glyphs(scene: &mut Scene, b: &BoxObject) {
    for p in get_rect_perimeter_points(Rect {
        left: b.left,
        top: b.top,
        right: b.right,
        bottom: b.bottom,
    }) {
        let conns = scene.connection_at(p);
        stamp_glyph(scene, p, get_box_border_glyph(b.style, conns));
    }
}

fn stamp_line_connections(scene: &mut Scene, l: &LineObject) {
    let start = Point { x: l.x1, y: l.y1 };
    let end = Point { x: l.x2, y: l.y2 };
    let cells = get_line_render_characters(start, end, l.style);
    let cell_set: std::collections::HashSet<(i32, i32)> = cells.keys().copied().collect();
    let has = |x: i32, y: i32| cell_set.contains(&(x, y));
    for key in cells.keys() {
        let p = Point { x: key.0, y: key.1 };
        let mut bits = 0u8;
        if has(key.0 - 1, key.1) {
            bits |= CONNECTION_W;
        }
        if has(key.0 + 1, key.1) {
            bits |= CONNECTION_E;
        }
        if has(key.0, key.1 - 1) {
            bits |= CONNECTION_N;
        }
        if has(key.0, key.1 + 1) {
            bits |= CONNECTION_S;
        }
        adjust_connection(scene, p, bits);
        paint_connection_color(scene, p, l.color);
    }
}

fn stamp_line_glyphs(scene: &mut Scene, l: &LineObject) {
    let start = Point { x: l.x1, y: l.y1 };
    let end = Point { x: l.x2, y: l.y2 };
    let cells = get_line_render_characters(start, end, l.style);
    for (key, glyph) in &cells {
        let conns = scene.connection_at(Point { x: key.0, y: key.1 });
        // Use the connection glyph so lines crossing boxes or other
        // lines resolve to T-junctions and crosses.
        let g = if conns == (CONNECTION_E | CONNECTION_W) || conns == (CONNECTION_N | CONNECTION_S)
        {
            *glyph
        } else {
            get_connection_glyph(conns, l.style)
        };
        stamp_glyph(scene, Point { x: key.0, y: key.1 }, g);
    }
}

fn stamp_elbow_connections(scene: &mut Scene, e: &ElbowObject) {
    let start = Point { x: e.x1, y: e.y1 };
    let end = Point { x: e.x2, y: e.y2 };
    let cells = get_elbow_render_characters(start, end, e.style, e.orientation);
    for key in cells.keys() {
        let p = Point { x: key.0, y: key.1 };
        // The corner cell is a junction; the segment cells carry the
        // horizontal or vertical arms; the arrowhead cell ends the
        // last segment. The simple rule: every cell is on at least one
        // segment, so it has E+W or N+S; the corner additionally has
        // the perpendicular pair. We OR in all four bits — overlap
        // with a box's bitmask will produce the right T/cross glyph
        // because the cell-level bitmask is the union of all
        // contributors.
        let bits = CONNECTION_E | CONNECTION_W | CONNECTION_N | CONNECTION_S;
        adjust_connection(scene, p, bits);
        paint_connection_color(scene, p, e.color);
    }
}

fn stamp_elbow_glyphs(scene: &mut Scene, e: &ElbowObject) {
    let start = Point { x: e.x1, y: e.y1 };
    let end = Point { x: e.x2, y: e.y2 };
    let cells = get_elbow_render_characters(start, end, e.style, e.orientation);
    for (key, glyph) in &cells {
        stamp_glyph(scene, Point { x: key.0, y: key.1 }, *glyph);
    }
}

fn stamp_paint_connections(scene: &mut Scene, p: &PaintObject) {
    for (i, pt) in p.points.iter().enumerate() {
        if i + 1 < p.points.len() {
            let next = p.points[i + 1];
            if next.x > pt.x {
                adjust_connection(scene, *pt, CONNECTION_E);
            } else if next.x < pt.x {
                adjust_connection(scene, *pt, CONNECTION_W);
            }
            if next.y > pt.y {
                adjust_connection(scene, *pt, CONNECTION_S);
            } else if next.y < pt.y {
                adjust_connection(scene, *pt, CONNECTION_N);
            }
        }
        if i > 0 {
            let prev = p.points[i - 1];
            if prev.x > pt.x {
                adjust_connection(scene, *pt, CONNECTION_E);
            } else if prev.x < pt.x {
                adjust_connection(scene, *pt, CONNECTION_W);
            }
            if prev.y > pt.y {
                adjust_connection(scene, *pt, CONNECTION_S);
            } else if prev.y < pt.y {
                adjust_connection(scene, *pt, CONNECTION_N);
            }
        }
        paint_connection_color(scene, *pt, p.color);
    }
}

fn stamp_paint_glyphs(scene: &mut Scene, p: &PaintObject) {
    for pt in &p.points {
        stamp_glyph(scene, *pt, normalize_brush(&p.brush));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoxStyle, InkColor, LineStyle, TextBorderMode, TextObject};

    fn empty_box(left: i32, top: i32, right: i32, bottom: i32) -> BoxObject {
        BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left,
            top,
            right,
            bottom,
            style: BoxStyle::Light,
        }
    }

    fn horizontal_line(x1: i32, y1: i32, x2: i32) -> LineObject {
        LineObject {
            id: "l".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x1,
            y1,
            x2,
            y2: y1,
            style: LineStyle::Light,
        }
    }

    #[test]
    fn create_scene_initializes_blank_grid() {
        let s = create_scene(3, 2, Point { x: 0, y: 0 });
        assert_eq!(s.width, 3);
        assert_eq!(s.height, 2);
        assert_eq!(s.cells.len(), 2);
        assert!(s.cells.iter().all(|row| row.iter().all(|c| c.glyph == ' ')));
        assert!(s.connections.iter().all(|row| row.iter().all(|b| *b == 0)));
    }

    #[test]
    fn point_to_cell_maps_document_to_index() {
        let s = create_scene(5, 5, Point { x: 10, y: 20 });
        assert_eq!(s.point_to_cell(Point { x: 10, y: 20 }), Some((0, 0)));
        assert_eq!(s.point_to_cell(Point { x: 14, y: 24 }), Some((4, 4)));
        assert_eq!(s.point_to_cell(Point { x: 15, y: 20 }), None);
        assert_eq!(s.point_to_cell(Point { x: 9, y: 20 }), None);
    }

    #[test]
    fn get_box_border_glyph_picks_corners_and_edges() {
        // NW corner of a box: arms E+S → bitmask 0b0110 = 6 → '┌'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_E | CONNECTION_S),
            '┌'
        );
        // NE corner: arms S+W → 0b1100 = 12 → '┐'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_S | CONNECTION_W),
            '┐'
        );
        // SE corner: arms N+W → 0b1001 = 9 → '┘'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_N | CONNECTION_W),
            '┘'
        );
        // SW corner: arms N+E → 0b0011 = 3 → '└'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_N | CONNECTION_E),
            '└'
        );
        // Horizontal edge: E+W → 0b1010 = 10 → '─'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_E | CONNECTION_W),
            '─'
        );
        // Vertical edge: N+S → 0b0101 = 5 → '│'.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Light, CONNECTION_N | CONNECTION_S),
            '│'
        );
        // Cross: all four → '┼'.
        assert_eq!(
            get_box_border_glyph(
                BoxStyle::Light,
                CONNECTION_N | CONNECTION_E | CONNECTION_S | CONNECTION_W,
            ),
            '┼'
        );
    }

    #[test]
    fn get_box_border_glyph_picks_double_for_double_style() {
        // Same bitmask conventions, double-line glyphs.
        assert_eq!(
            get_box_border_glyph(BoxStyle::Double, CONNECTION_E | CONNECTION_S),
            '╔'
        );
        assert_eq!(
            get_box_border_glyph(BoxStyle::Double, CONNECTION_N | CONNECTION_S),
            '║'
        );
        assert_eq!(
            get_box_border_glyph(
                BoxStyle::Double,
                CONNECTION_N | CONNECTION_E | CONNECTION_S | CONNECTION_W,
            ),
            '╋'
        );
    }

    #[test]
    fn compose_scene_box_perimeter_uses_corner_glyphs() {
        let mut s = create_scene(6, 4, Point { x: 0, y: 0 });
        compose_scene(&mut s, &[DrawObject::Box(empty_box(0, 0, 5, 3))]);
        assert_eq!(s.cells[0][0].glyph, '┌');
        assert_eq!(s.cells[0][5].glyph, '┐');
        assert_eq!(s.cells[3][0].glyph, '└');
        assert_eq!(s.cells[3][5].glyph, '┘');
        // Top middle should be a horizontal edge.
        assert_eq!(s.cells[0][2].glyph, '─');
        // Right middle should be a vertical edge.
        assert_eq!(s.cells[1][5].glyph, '│');
    }

    #[test]
    fn compose_scene_line_meets_box_edge_forms_t_junction() {
        // A line runs east from the right edge of a box at y=2. The
        // right-edge cell at (5, 2) gets the line's E arm plus the
        // frame's N and S arms → bitmask 7 → '├' (T pointing right,
        // since the line emerges from the east side of the box).
        let mut s = create_scene(10, 5, Point { x: 0, y: 0 });
        let objects = vec![
            DrawObject::Box(empty_box(0, 0, 5, 3)),
            DrawObject::Line(horizontal_line(5, 2, 9)),
        ];
        compose_scene(&mut s, &objects);
        assert_eq!(s.cells[2][5].glyph, '├');
        // The first line cell past the box should remain '─'.
        assert_eq!(s.cells[2][6].glyph, '─');
    }

    #[test]
    fn compose_scene_paint_stamps_brush_glyphs() {
        let mut s = create_scene(4, 2, Point { x: 0, y: 0 });
        let p = PaintObject {
            id: "p".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            points: vec![
                Point { x: 0, y: 0 },
                Point { x: 1, y: 0 },
                Point { x: 2, y: 0 },
            ],
            brush: "·".into(),
        };
        compose_scene(&mut s, &[DrawObject::Paint(p)]);
        for x in 0..3 {
            assert_eq!(s.cells[0][x].glyph, '·');
        }
    }

    #[test]
    fn compose_scene_text_writes_content_characters() {
        let mut s = create_scene(6, 4, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 1,
            y: 1,
            content: "hi".into(),
            border: TextBorderMode::None,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        assert_eq!(s.cells[1][1].glyph, 'h');
        assert_eq!(s.cells[1][2].glyph, 'i');
    }

    #[test]
    fn compose_scene_text_multiline_stacks_lines_on_increasing_y() {
        // Three lines via \n separators. Each line stamps at
        // content_origin.y + line_index. No border so the top
        // of the rect sits at y.
        let mut s = create_scene(8, 5, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 1,
            y: 1,
            content: "ab\ncd\nef".into(),
            border: TextBorderMode::None,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        assert_eq!(s.cells[1][1].glyph, 'a');
        assert_eq!(s.cells[1][2].glyph, 'b');
        assert_eq!(s.cells[2][1].glyph, 'c');
        assert_eq!(s.cells[2][2].glyph, 'd');
        assert_eq!(s.cells[3][1].glyph, 'e');
        assert_eq!(s.cells[3][2].glyph, 'f');
    }

    #[test]
    fn compose_scene_text_multiline_trailing_newline_doesnt_paint_empty_row() {
        // Trailing \n shouldn't add an empty row to the rect —
        // matches the render rect's height (line_count drops
        // the phantom line).
        let mut s = create_scene(8, 5, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 1,
            y: 1,
            content: "ab\n".into(),
            border: TextBorderMode::None,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        assert_eq!(s.cells[1][1].glyph, 'a');
        assert_eq!(s.cells[1][2].glyph, 'b');
        // y=2 should NOT be a glyph from this text. The scene
        // cell at that point was initialized to a space; we
        // assert it's still a space (the empty trailing line
        // didn't paint).
        assert_eq!(s.cells[2][1].glyph, ' ');
        assert_eq!(s.cells[2][2].glyph, ' ');
    }

    #[test]
    fn compose_scene_text_framed_double_draws_double_box() {
        let mut s = create_scene(6, 4, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 1,
            y: 0,
            content: "ok".into(),
            border: TextBorderMode::Double,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        // Top-left corner of the double frame.
        assert_eq!(s.cells[0][1].glyph, '╔');
        // Top-right corner.
        assert_eq!(s.cells[0][4].glyph, '╗');
    }

    #[test]
    fn compose_scene_text_stamps_wide_graphemes_as_two_cells() {
        // '中' is East Asian Wide (column width 2). stamp_text must
        // occupy two scene cells: first with the glyph, second with
        // a space, both with the object's color so ratatui can paint
        // the auto-spacer correctly.
        let mut s = create_scene(6, 4, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::Cyan,
            x: 1,
            y: 0,
            content: "中".into(),
            border: TextBorderMode::None,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        // First cell: the glyph itself.
        assert_eq!(s.cells[0][1].glyph, '中');
        assert_eq!(s.cells[0][1].color, Some(InkColor::Cyan));
        // Second cell: blank spacer, but with the same color so the
        // renderer's auto-spacer gets the correct fg.
        assert_eq!(s.cells[0][2].glyph, ' ');
        assert_eq!(s.cells[0][2].color, Some(InkColor::Cyan));
        // Third cell untouched (would have been "A" if naive stamping).
        assert_eq!(s.cells[0][3].glyph, ' ');
    }

    #[test]
    fn compose_scene_text_stamps_mixed_ascii_and_wide() {
        // "A中B" — A width 1, 中 width 2, B width 1. Total 4 cells.
        let mut s = create_scene(6, 4, Point { x: 0, y: 0 });
        let t = TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 0,
            y: 0,
            content: "A中B".into(),
            border: TextBorderMode::None,
        };
        compose_scene(&mut s, &[DrawObject::Text(t)]);
        assert_eq!(s.cells[0][0].glyph, 'A');
        assert_eq!(s.cells[0][1].glyph, '中');
        assert_eq!(s.cells[0][2].glyph, ' '); // wide-grapheme spacer
        assert_eq!(s.cells[0][3].glyph, 'B');
        assert_eq!(s.cells[0][4].glyph, ' '); // untouched
    }

    #[test]
    fn compose_scene_color_grid_records_object_color() {
        let mut s = create_scene(4, 4, Point { x: 0, y: 0 });
        compose_scene(&mut s, &[DrawObject::Box(empty_box(0, 0, 3, 3))]);
        for row in &s.cells {
            for c in row {
                if c.glyph != ' ' {
                    assert_eq!(c.color, Some(InkColor::White));
                }
            }
        }
    }
}
