//! Line, elbow, and paint-stroke rendering.
//!
//! Mirrors `termdraw`'s `draw-state/line.ts`. Single-cell line glyphs come
//! from a small lookup table per style; smooth (default) diagonals fall back
//! to Braille cells for shallow angles so terminal output tracks the
//! intended vector line more closely. Elbow connectors are orthogonal: one
//! horizontal + one vertical segment + a corner glyph + an arrowhead.

use std::collections::BTreeMap;

use crate::geometry::{clamp, normalize_rect};
use crate::types::{ElbowOrientation, LineStyle, Point};

/// Glyph set for orthogonal (light / double / dashed) lines.
struct OrthogonalGlyphs {
    horizontal: char,
    vertical: char,
    corner_ne: char,
    corner_nw: char,
    corner_se: char,
    corner_sw: char,
}

fn get_orthogonal_line_glyphs(style: LineStyle) -> OrthogonalGlyphs {
    match style {
        LineStyle::Double => OrthogonalGlyphs {
            horizontal: '═',
            vertical: '║',
            corner_ne: '╚',
            corner_nw: '╝',
            corner_se: '╔',
            corner_sw: '╗',
        },
        LineStyle::Dashed => OrthogonalGlyphs {
            horizontal: '┄',
            vertical: '┆',
            corner_ne: '└',
            corner_nw: '┘',
            corner_se: '┌',
            corner_sw: '┐',
        },
        // Light, Smooth, or unknown → single-line.
        _ => OrthogonalGlyphs {
            horizontal: '─',
            vertical: '│',
            corner_ne: '└',
            corner_nw: '┘',
            corner_se: '┌',
            corner_sw: '┐',
        },
    }
}

/// Pick the best single-cell glyph for a non-Braille line segment.
fn get_line_character(start: Point, end: Point, style: LineStyle) -> char {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let abs_dx = dx.abs();
    let abs_dy = dy.abs();

    match style {
        LineStyle::Light | LineStyle::Smooth => {
            if dx == 0 && dy == 0 {
                return '•';
            }
            if dx == 0 {
                return '│';
            }
            if dy == 0 {
                return '─';
            }
            if abs_dx >= abs_dy * 2 {
                return '─';
            }
            if abs_dy >= abs_dx * 2 {
                return '│';
            }
            if dx.signum() == dy.signum() {
                '╲'
            } else {
                '╱'
            }
        }
        LineStyle::Double => {
            if dx == 0 && dy == 0 {
                return '•';
            }
            if dx == 0 {
                return '║';
            }
            if dy == 0 {
                return '═';
            }
            if abs_dx >= abs_dy * 2 {
                return '═';
            }
            if abs_dy >= abs_dx * 2 {
                return '║';
            }
            if dx.signum() == dy.signum() {
                '╲'
            } else {
                '╱'
            }
        }
        LineStyle::Dashed => {
            if dx == 0 && dy == 0 {
                return '•';
            }
            if dx == 0 {
                return '┆';
            }
            if dy == 0 {
                return '┄';
            }
            if abs_dx >= abs_dy * 2 {
                return '┄';
            }
            if abs_dy >= abs_dx * 2 {
                return '┆';
            }
            if dx.signum() == dy.signum() {
                '╲'
            } else {
                '╱'
            }
        }
    }
}

/// True for shallow smooth diagonals — those that look bad as a single
/// diagonal glyph and benefit from Braille sub-cell rendering.
fn should_render_line_as_braille(start: Point, end: Point, style: LineStyle) -> bool {
    if style != LineStyle::Smooth {
        return false;
    }
    let dx = (end.x - start.x).abs();
    let dy = (end.y - start.y).abs();
    dx != 0 && dy != 0 && dx != dy
}

/// Snap a free endpoint onto the dominant horizontal or vertical axis
/// relative to `anchor`. Used by line and elbow constrained drags.
pub fn constrain_line_point(anchor: Point, point: Point) -> Point {
    let dx = point.x - anchor.x;
    let dy = point.y - anchor.y;
    if dx.abs() >= dy.abs() {
        Point {
            x: point.x,
            y: anchor.y,
        }
    } else {
        Point {
            x: anchor.x,
            y: point.y,
        }
    }
}

/// Squared distance from `point` to the segment `start`–`end`.
fn get_distance_to_segment_squared(point: Point, start: Point, end: Point) -> i32 {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_squared = dx * dx + dy * dy;

    if length_squared == 0 {
        let ox = point.x - start.x;
        let oy = point.y - start.y;
        return ox * ox + oy * oy;
    }

    // Standard projection: t = dot(p - s, d) / |d|², clamped to [0, 1].
    let t_num = (point.x - start.x) * dx + (point.y - start.y) * dy;
    let t = clamp(t_num / length_squared.max(1), 0, 1);
    let projected_x = start.x + t * dx;
    let projected_y = start.y + t * dy;
    let ox = point.x - projected_x;
    let oy = point.y - projected_y;
    ox * ox + oy * oy
}

// Braille dot masks, indexed by [row][col]. Matches the Unicode Braille
// block (U+2800–U+28FF): column-major, top-to-bottom.
//
//   0x01  0x08
//   0x02  0x10
//   0x04  0x20
//   0x40  0x80
const BRAILLE_DOT_MASKS: [[u32; 2]; 4] = [[0x1, 0x8], [0x2, 0x10], [0x4, 0x20], [0x40, 0x80]];

// Half-cell coordinates of each braille dot. We compare in half-cells
// (i.e. x*2, y*2) so these become integer offsets:
//   col 0 (x=0.25) → 1 half-cell (after the cell's left edge at 0)
//   col 1 (x=0.75) → 3 half-cells
//   row 0 (y=0.125) → 1 half-cell
//   row 1 (y=0.375) → 3 half-cells
//   row 2 (y=0.625) → 5 half-cells
//   row 3 (y=0.875) → 7 half-cells
const BRAILLE_X_OFFSETS: [i32; 2] = [1, 3];
const BRAILLE_Y_OFFSETS: [i32; 4] = [1, 3, 5, 7];
// 0.22 cells → 0.88 half-cells → 0.88² = 0.7744. We round up to 1.
const BRAILLE_LINE_THRESHOLD_SQ: i32 = 1;

/// Render the cells a smooth line covers as Braille dots.
fn render_braille_line(start: Point, end: Point) -> BTreeMap<(i32, i32), char> {
    let mut rendered: BTreeMap<(i32, i32), char> = BTreeMap::new();

    // Sample at half-cell precision (centered on each cell).
    let sample_start = Point {
        x: start.x * 2 + 1,
        y: start.y * 2 + 1,
    };
    let sample_end = Point {
        x: end.x * 2 + 1,
        y: end.y * 2 + 1,
    };

    let rect = normalize_rect(start, end);

    for y in rect.top..=rect.bottom {
        for x in rect.left..=rect.right {
            // The cell's half-cell top-left is (x*2, y*2). Sample each dot
            // at (cell_origin + dot_offset) and light up the ones that fall
            // within `BRAILLE_LINE_THRESHOLD_SQ` of the segment.
            let cell_ox = x * 2;
            let cell_oy = y * 2;
            let mut mask: u32 = 0;
            for (row, &yoff) in BRAILLE_Y_OFFSETS.iter().enumerate() {
                for (col, &xoff) in BRAILLE_X_OFFSETS.iter().enumerate() {
                    let sample = Point {
                        x: cell_ox + xoff,
                        y: cell_oy + yoff,
                    };
                    if get_distance_to_segment_squared(sample, sample_start, sample_end)
                        <= BRAILLE_LINE_THRESHOLD_SQ
                    {
                        mask |= BRAILLE_DOT_MASKS[row][col];
                    }
                }
            }
            if mask != 0 {
                let ch = char::from_u32(0x2800 + mask).unwrap_or(' ');
                rendered.insert((x, y), ch);
            }
        }
    }
    rendered
}

/// Render the cells a line covers. The map is keyed by `(x, y)`.
pub fn get_line_render_characters(
    start: Point,
    end: Point,
    style: LineStyle,
) -> BTreeMap<(i32, i32), char> {
    let mut rendered: BTreeMap<(i32, i32), char> = BTreeMap::new();

    if should_render_line_as_braille(start, end, style) {
        let braille = render_braille_line(start, end);
        if !braille.is_empty() {
            return braille;
        }
    }

    let ch = get_line_character(start, end, style);
    for p in get_line_points(start.x, start.y, end.x, end.y) {
        rendered.insert((p.x, p.y), ch);
    }
    rendered
}

/// Render the cells an elbow connector covers. The corner is the cell at
/// (start.x, end.y) for `vertical-first` or (end.x, start.y) for
/// `horizontal-first`. The endpoint cell is the arrowhead.
pub fn get_elbow_render_characters(
    start: Point,
    end: Point,
    style: LineStyle,
    orientation: ElbowOrientation,
) -> BTreeMap<(i32, i32), char> {
    let mut rendered: BTreeMap<(i32, i32), char> = BTreeMap::new();
    let g = get_orthogonal_line_glyphs(style);

    let corner = match orientation {
        ElbowOrientation::VerticalFirst => Point {
            x: start.x,
            y: end.y,
        },
        ElbowOrientation::HorizontalFirst => Point {
            x: end.x,
            y: start.y,
        },
    };
    let first_glyph = match orientation {
        ElbowOrientation::VerticalFirst => g.vertical,
        ElbowOrientation::HorizontalFirst => g.horizontal,
    };
    let second_glyph = match orientation {
        ElbowOrientation::VerticalFirst => g.horizontal,
        ElbowOrientation::HorizontalFirst => g.vertical,
    };

    for p in get_line_points(start.x, start.y, corner.x, corner.y) {
        rendered.insert((p.x, p.y), first_glyph);
    }
    for p in get_line_points(corner.x, corner.y, end.x, end.y) {
        rendered.insert((p.x, p.y), second_glyph);
    }

    if start.x != end.x && start.y != end.y {
        let connects_north = start.y < corner.y || end.y < corner.y;
        let connects_south = start.y > corner.y || end.y > corner.y;
        let connects_east = start.x > corner.x || end.x > corner.x;
        let connects_west = start.x < corner.x || end.x < corner.x;
        let corner_glyph = if connects_north {
            if connects_east {
                g.corner_ne
            } else {
                g.corner_nw
            }
        } else if connects_south {
            if connects_east {
                g.corner_se
            } else {
                g.corner_sw
            }
        } else if connects_east || connects_west {
            g.horizontal
        } else {
            g.vertical
        };
        rendered.insert((corner.x, corner.y), corner_glyph);
    }

    // Arrowhead points along the dominant last-segment axis. Falls back
    // to the start→end direction when start/end share an axis with corner.
    let arrow = if corner.x != end.x {
        if end.x > corner.x {
            '>'
        } else {
            '<'
        }
    } else if corner.y != end.y {
        if end.y > corner.y {
            'v'
        } else {
            '^'
        }
    } else if end.x != start.x {
        if end.x > start.x {
            '>'
        } else {
            '<'
        }
    } else if end.y > start.y {
        'v'
    } else {
        '^'
    };
    rendered.insert((end.x, end.y), arrow);
    rendered.insert(
        (start.x, start.y),
        if start.x == corner.x {
            g.vertical
        } else {
            g.horizontal
        },
    );
    rendered
}

/// Parse a "x,y" map key back into a point.
pub fn point_from_key(key: &(i32, i32)) -> Point {
    Point { x: key.0, y: key.1 }
}

/// Cells a line covers, in iteration order.
pub fn get_line_render_cells(start: Point, end: Point, style: LineStyle) -> Vec<Point> {
    get_line_render_characters(start, end, style)
        .keys()
        .map(point_from_key)
        .collect()
}

/// Cells an elbow connector covers, in iteration order.
pub fn get_elbow_render_cells(
    start: Point,
    end: Point,
    style: LineStyle,
    orientation: ElbowOrientation,
) -> Vec<Point> {
    get_elbow_render_characters(start, end, style, orientation)
        .keys()
        .map(point_from_key)
        .collect()
}

/// Bresenham line points between (x0, y0) and (x1, y1).
pub fn get_line_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<Point> {
    let mut points: Vec<Point> = Vec::new();
    let mut current_x = x0;
    let mut current_y = y0;
    let delta_x = (x1 - x0).abs();
    let delta_y = (y1 - y0).abs();
    let step_x = if x0 < x1 { 1 } else { -1 };
    let step_y = if y0 < y1 { 1 } else { -1 };
    let mut err = delta_x - delta_y;

    loop {
        points.push(Point {
            x: current_x,
            y: current_y,
        });
        if current_x == x1 && current_y == y1 {
            break;
        }
        let twice_err = err * 2;
        if twice_err > -delta_y {
            err -= delta_y;
            current_x += step_x;
        }
        if twice_err < delta_x {
            err += delta_x;
            current_y += step_y;
        }
    }
    points
}

/// Merge `next` into `existing`, preserving the first occurrence of each
/// cell. Returns a fresh `Vec<Point>`.
pub fn merge_unique_points(existing: &[Point], next: &[Point]) -> Vec<Point> {
    let mut merged: Vec<Point> = existing.to_vec();
    let mut seen: std::collections::HashSet<(i32, i32)> =
        existing.iter().map(|p| (p.x, p.y)).collect();
    for p in next {
        if seen.insert((p.x, p.y)) {
            merged.push(*p);
        }
    }
    merged
}

/// Append a Bresenham-rasterized segment from `from` to `to` to a paint
/// stroke. Dedupes cells so a back-and-forth drag doesn't double-stamp.
pub fn append_paint_segment(points: &[Point], from: Point, to: Point) -> Vec<Point> {
    merge_unique_points(points, &get_line_points(from.x, from.y, to.x, to.y))
}

/// True if two point lists are identical in length and order.
pub fn points_equal(a: &[Point], b: &[Point]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(p, q)| p.x == q.x && p.y == q.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    #[test]
    fn bresenham_horizontal() {
        let pts = get_line_points(0, 0, 4, 0);
        assert_eq!(pts, vec![p(0, 0), p(1, 0), p(2, 0), p(3, 0), p(4, 0)]);
    }

    #[test]
    fn bresenham_vertical() {
        let pts = get_line_points(2, 1, 2, 4);
        assert_eq!(pts, vec![p(2, 1), p(2, 2), p(2, 3), p(2, 4)]);
    }

    #[test]
    fn bresenham_diagonal_45() {
        let pts = get_line_points(0, 0, 3, 3);
        assert_eq!(pts, vec![p(0, 0), p(1, 1), p(2, 2), p(3, 3)]);
    }

    #[test]
    fn bresenham_shallow_diagonal() {
        // dx=4, dy=2 → steps that drift through 6 cells.
        let pts = get_line_points(0, 0, 4, 2);
        assert_eq!(pts.first(), Some(&p(0, 0)));
        assert_eq!(pts.last(), Some(&p(4, 2)));
        assert!(pts.len() >= 5);
    }

    #[test]
    fn horizontal_smooth_uses_dash() {
        let m = get_line_render_characters(p(0, 0), p(4, 0), LineStyle::Smooth);
        assert_eq!(m.len(), 5);
        assert!(m.values().all(|c| *c == '─'));
    }

    #[test]
    fn vertical_light_uses_pipe() {
        let m = get_line_render_characters(p(2, 0), p(2, 3), LineStyle::Light);
        assert_eq!(m.len(), 4);
        assert!(m.values().all(|c| *c == '│'));
    }

    #[test]
    fn double_uses_equals_and_pipe() {
        let m = get_line_render_characters(p(0, 0), p(4, 0), LineStyle::Double);
        assert!(m.values().all(|c| *c == '═'));
    }

    #[test]
    fn smooth_shallow_diagonal_uses_braille() {
        // dx=4, dy=1 → shallow, should fall back to Braille.
        let m = get_line_render_characters(p(0, 0), p(4, 1), LineStyle::Smooth);
        // At least one cell should be a Braille character.
        assert!(m.values().any(|c| ('\u{2800}'..='\u{28FF}').contains(c)));
    }

    #[test]
    fn smooth_45_diagonal_uses_slash() {
        // dx == dy → not shallow; single diagonal glyph.
        let m = get_line_render_characters(p(0, 0), p(3, 3), LineStyle::Smooth);
        assert!(m.values().all(|c| *c == '╲' || *c == '╱'));
    }

    #[test]
    fn elbow_routes_two_segments_with_arrow() {
        // Vertical-first from (0,0) to (5,3): corner at (0,3). The last
        // segment is horizontal → arrow at end points east.
        let m = get_elbow_render_characters(
            p(0, 0),
            p(5, 3),
            LineStyle::Light,
            ElbowOrientation::VerticalFirst,
        );
        assert_eq!(m.get(&(5, 3)), Some(&'>'));
        assert!(m
            .values()
            .any(|c| *c == '└' || *c == '┌' || *c == '┘' || *c == '┐'));
    }

    #[test]
    fn elbow_vertical_first_uses_pipe_then_dash() {
        // Vertical-first from (0,0) to (5,3): corner at (0,3).
        let m = get_elbow_render_characters(
            p(0, 0),
            p(5, 3),
            LineStyle::Light,
            ElbowOrientation::VerticalFirst,
        );
        assert!(m.contains_key(&(0, 3)));
        assert!(m.values().any(|c| *c == '│'));
        assert!(m.values().any(|c| *c == '─'));
    }

    #[test]
    fn constrain_line_point_picks_dominant_axis() {
        assert_eq!(constrain_line_point(p(0, 0), p(5, 1)), p(5, 0));
        assert_eq!(constrain_line_point(p(0, 0), p(1, 5)), p(0, 5));
        assert_eq!(constrain_line_point(p(0, 0), p(5, 5)), p(5, 0));
    }

    #[test]
    fn merge_unique_points_dedupes_preserving_order() {
        let a = vec![p(0, 0), p(1, 0)];
        let b = vec![p(1, 0), p(2, 0), p(0, 0)];
        assert_eq!(merge_unique_points(&a, &b), vec![p(0, 0), p(1, 0), p(2, 0)]);
    }

    #[test]
    fn append_paint_segment_connects_via_bresenham() {
        let stroke = vec![p(0, 0)];
        let extended = append_paint_segment(&stroke, p(0, 0), p(2, 0));
        assert_eq!(extended, vec![p(0, 0), p(1, 0), p(2, 0)]);
    }

    #[test]
    fn points_equal_checks_length_and_order() {
        assert!(points_equal(&[p(1, 2), p(3, 4)], &[p(1, 2), p(3, 4)]));
        assert!(!points_equal(&[p(1, 2)], &[p(1, 2), p(3, 4)]));
        assert!(!points_equal(&[p(1, 2)], &[p(2, 1)]));
    }

    #[test]
    fn get_line_render_cells_returns_points_in_some_order() {
        let pts = get_line_render_cells(p(0, 0), p(3, 0), LineStyle::Smooth);
        assert_eq!(pts.len(), 4);
    }
}
