//! Low-level rectangle and point math.
//!
//! Mirrors `termdraw`'s `draw-state/geometry.ts`. Pure functions, no I/O,
//! no terminal. Safe to call from any thread.

use crate::types::{Point, Rect};

/// Clamp `value` into the inclusive `[min, max]` range.
#[inline]
pub fn clamp(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

/// Build a normalized rect whose edges are ordered regardless of drag direction.
#[inline]
pub fn normalize_rect(start: Point, end: Point) -> Rect {
    Rect {
        left: start.x.min(end.x),
        right: start.x.max(end.x),
        top: start.y.min(end.y),
        bottom: start.y.max(end.y),
    }
}

/// Returns whether the point lies within the inclusive rect bounds.
#[inline]
pub fn rect_contains_point(rect: Rect, x: i32, y: i32) -> bool {
    x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
}

/// Returns the unique perimeter cells of a rect.
///
/// A one-cell or one-row rect still produces a sensible (degenerate) set:
/// the four corner points collapse to the same coordinates but the map
/// key dedupes them.
pub fn get_rect_perimeter_points(rect: Rect) -> Vec<Point> {
    let mut cells: Vec<Point> = Vec::new();
    let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
    let add = |cells: &mut Vec<Point>, seen: &mut std::collections::HashSet<(i32, i32)>, x, y| {
        if seen.insert((x, y)) {
            cells.push(Point { x, y });
        }
    };

    for x in rect.left..=rect.right {
        add(&mut cells, &mut seen, x, rect.top);
        add(&mut cells, &mut seen, x, rect.bottom);
    }
    for y in rect.top..=rect.bottom {
        add(&mut cells, &mut seen, rect.left, y);
        add(&mut cells, &mut seen, rect.right, y);
    }
    cells
}

/// A rect is valid if width and height are non-negative.
#[inline]
pub fn is_valid_rect(rect: Rect) -> bool {
    rect.left <= rect.right && rect.top <= rect.bottom
}

/// True if `inner` is fully contained inside `outer`. Test-only
/// helper (gated under `cfg(test)` below) — no production callers.
/// Kept as a named helper so the geometry test expresses intent
/// rather than inlining the inequality chain twice.
#[cfg(test)]
fn rect_contains_rect(outer: Rect, inner: Rect) -> bool {
    if !is_valid_rect(outer) {
        return false;
    }
    inner.left >= outer.left
        && inner.right <= outer.right
        && inner.top >= outer.top
        && inner.bottom <= outer.bottom
}

/// True if two inclusive rects overlap at all. Test-only helper.
#[cfg(test)]
#[inline]
fn rects_intersect(a: Rect, b: Rect) -> bool {
    a.left <= b.right && a.right >= b.left && a.top <= b.bottom && a.bottom >= b.top
}

/// Inclusive cell area of a rect.
#[inline]
pub fn get_rect_area(rect: Rect) -> i32 {
    if !is_valid_rect(rect) {
        return 0;
    }
    (rect.right - rect.left + 1) * (rect.bottom - rect.top + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_clamps_below_and_above() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(11, 0, 10), 10);
        assert_eq!(clamp(0, 0, 10), 0);
        assert_eq!(clamp(10, 0, 10), 10);
    }

    #[test]
    fn normalize_rect_orders_edges() {
        let r = normalize_rect(Point { x: 5, y: 5 }, Point { x: 0, y: 0 });
        assert_eq!(
            r,
            Rect {
                left: 0,
                top: 0,
                right: 5,
                bottom: 5
            }
        );
    }

    #[test]
    fn rect_contains_point_inclusive_edges() {
        let r = Rect {
            left: 0,
            top: 0,
            right: 3,
            bottom: 3,
        };
        assert!(rect_contains_point(r, 0, 0));
        assert!(rect_contains_point(r, 3, 3));
        assert!(rect_contains_point(r, 1, 2));
        assert!(!rect_contains_point(r, 4, 0));
        assert!(!rect_contains_point(r, 0, 4));
    }

    #[test]
    fn perimeter_of_one_cell_rect_has_one_point() {
        let r = Rect {
            left: 2,
            top: 2,
            right: 2,
            bottom: 2,
        };
        let p = get_rect_perimeter_points(r);
        assert_eq!(p, vec![Point { x: 2, y: 2 }]);
    }

    #[test]
    fn perimeter_of_one_row_rect_dedupes() {
        let r = Rect {
            left: 0,
            top: 1,
            right: 3,
            bottom: 1,
        };
        let p = get_rect_perimeter_points(r);
        // 4 unique cells: (0,1), (1,1), (2,1), (3,1)
        assert_eq!(p.len(), 4);
    }

    #[test]
    fn perimeter_of_three_by_three_rect_has_eight_corners_and_edges() {
        let r = Rect {
            left: 0,
            top: 0,
            right: 2,
            bottom: 2,
        };
        let p = get_rect_perimeter_points(r);
        // 3 top + 3 bottom + 1 left-mid + 1 right-mid = 8
        assert_eq!(p.len(), 8);
    }

    #[test]
    fn is_valid_rect_distinguishes_orientation() {
        assert!(is_valid_rect(Rect {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0
        }));
        assert!(is_valid_rect(Rect {
            left: 0,
            top: 0,
            right: 5,
            bottom: 3
        }));
        assert!(!is_valid_rect(Rect {
            left: 5,
            top: 0,
            right: 0,
            bottom: 3
        }));
    }

    #[test]
    fn rect_contains_rect_requires_strict_containment() {
        let outer = Rect {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        let inner = Rect {
            left: 2,
            top: 2,
            right: 8,
            bottom: 8,
        };
        assert!(rect_contains_rect(outer, inner));
        assert!(!rect_contains_rect(inner, outer));
        let touching = Rect {
            left: 0,
            top: 0,
            right: 10,
            bottom: 10,
        };
        assert!(rect_contains_rect(outer, touching));
    }

    #[test]
    fn rects_intersect_includes_edge_touching() {
        let a = Rect {
            left: 0,
            top: 0,
            right: 5,
            bottom: 5,
        };
        let b = Rect {
            left: 5,
            top: 0,
            right: 10,
            bottom: 5,
        };
        assert!(rects_intersect(a, b));
        let c = Rect {
            left: 6,
            top: 0,
            right: 10,
            bottom: 5,
        };
        assert!(!rects_intersect(a, c));
    }

    #[test]
    fn area_handles_degenerate_rects() {
        assert_eq!(
            get_rect_area(Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0
            }),
            1
        );
        assert_eq!(
            get_rect_area(Rect {
                left: 0,
                top: 0,
                right: 3,
                bottom: 2
            }),
            12
        );
        assert_eq!(
            get_rect_area(Rect {
                left: 5,
                top: 0,
                right: 0,
                bottom: 2
            }),
            0
        );
    }
}
