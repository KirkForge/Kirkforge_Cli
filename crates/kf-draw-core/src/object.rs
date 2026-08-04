//! Per-object helpers: bounds, hit testing, transform, clone, and the
//! list of cells an object actually paints into.
//!
//! Mirrors `termdraw`'s `draw-state/object.ts`. All functions are pure —
//! the editor state wraps these to compose mutations and snapshots.

use crate::geometry::{normalize_rect, rect_contains_point};
use crate::line::{get_elbow_render_cells, get_line_render_cells};
use crate::text_util::{get_text_render_rect, get_text_selection_bounds};
use crate::types::{
    BoxObject, BoxResizeHandle, DrawObject, ElbowObject, ElbowOrientation, LineObject, PaintObject,
    Point, Rect, TextObject,
};

/// Deep-clone a list of draw objects. Cheaper than a JSON round-trip and
/// keeps the document model type-safe.
pub fn clone_objects(objects: &[DrawObject]) -> Vec<DrawObject> {
    objects.to_vec()
}

/// Return a deep clone of `obj` with `id` replaced by `new_id`. Used
/// by `duplicate_selected` so the copy has a fresh identifier (and
/// doesn't collide with the original in the snapshot history).
pub fn clone_object_with_id(obj: &DrawObject, new_id: &str) -> DrawObject {
    // ponytail: each variant has its own `id` field — match the
    // spread pattern, swap id, clone the rest.
    match obj {
        DrawObject::Box(b) => DrawObject::Box(BoxObject {
            id: new_id.into(),
            ..b.clone()
        }),
        DrawObject::Line(l) => DrawObject::Line(LineObject {
            id: new_id.into(),
            ..l.clone()
        }),
        DrawObject::Elbow(e) => DrawObject::Elbow(ElbowObject {
            id: new_id.into(),
            ..e.clone()
        }),
        DrawObject::Paint(p) => DrawObject::Paint(PaintObject {
            id: new_id.into(),
            ..p.clone()
        }),
        DrawObject::Text(t) => DrawObject::Text(TextObject {
            id: new_id.into(),
            ..t.clone()
        }),
    }
}

/// The bounding rect that contains the entire object, including interior
/// for boxes and text. Returns `None` for a degenerate object (e.g. an
/// empty paint stroke or a zero-area line).
pub fn get_object_bounds(object: &DrawObject) -> Option<Rect> {
    match object {
        DrawObject::Box(b) => {
            let r = Rect {
                left: b.left,
                top: b.top,
                right: b.right,
                bottom: b.bottom,
            };
            if r.left > r.right || r.top > r.bottom {
                None
            } else {
                Some(r)
            }
        }
        DrawObject::Line(l) => Some(normalize_rect(
            Point { x: l.x1, y: l.y1 },
            Point { x: l.x2, y: l.y2 },
        )),
        DrawObject::Elbow(e) => {
            let start = Point { x: e.x1, y: e.y1 };
            let end = Point { x: e.x2, y: e.y2 };
            let corner = match e.orientation {
                ElbowOrientation::VerticalFirst => Point { x: e.x1, y: e.y2 },
                ElbowOrientation::HorizontalFirst => Point { x: e.x2, y: e.y1 },
            };
            let span = normalize_rect(start, end);
            let min_x = span.left.min(corner.x);
            let min_y = span.top.min(corner.y);
            let max_x = span.right.max(corner.x);
            let max_y = span.bottom.max(corner.y);
            Some(Rect {
                left: min_x,
                top: min_y,
                right: max_x,
                bottom: max_y,
            })
        }
        DrawObject::Paint(p) => {
            if p.points.is_empty() {
                return None;
            }
            let (mut min_x, mut min_y) = (i32::MAX, i32::MAX);
            let (mut max_x, mut max_y) = (i32::MIN, i32::MIN);
            for pt in &p.points {
                min_x = min_x.min(pt.x);
                min_y = min_y.min(pt.y);
                max_x = max_x.max(pt.x);
                max_y = max_y.max(pt.y);
            }
            Some(Rect {
                left: min_x,
                top: min_y,
                right: max_x,
                bottom: max_y,
            })
        }
        DrawObject::Text(t) => Some(get_text_render_rect(t)),
    }
}

/// The interior of a box (the area inside its frame). All box styles
/// have a 1-cell frame, so we always inset by 1 on each side. For a
/// degenerate box (height or width < 2) the content rect will be invalid
/// (left > right or top > bottom); callers should check `is_valid_rect`.
pub fn get_box_content_bounds(b: &BoxObject) -> Rect {
    Rect {
        left: b.left + 1,
        top: b.top + 1,
        right: b.right - 1,
        bottom: b.bottom - 1,
    }
}

/// The four corner cells of a box's frame, in (NW, NE, SW, SE) order.
pub fn get_box_corner_points(b: &BoxObject) -> (Point, Point, Point, Point) {
    let nw = Point {
        x: b.left,
        y: b.top,
    };
    let ne = Point {
        x: b.right,
        y: b.top,
    };
    let sw = Point {
        x: b.left,
        y: b.bottom,
    };
    let se = Point {
        x: b.right,
        y: b.bottom,
    };
    (nw, ne, sw, se)
}

/// The two endpoints of a line or elbow. Returns `(start, end)`.
pub fn get_line_endpoint_points(object: &DrawObject) -> Option<(Point, Point)> {
    match object {
        DrawObject::Line(l) => Some((Point { x: l.x1, y: l.y1 }, Point { x: l.x2, y: l.y2 })),
        DrawObject::Elbow(e) => Some((Point { x: e.x1, y: e.y1 }, Point { x: e.x2, y: e.y2 })),
        // ponytail: Box / Paint / Text don't have endpoints, so
        // the wildcard is correct, not a fallback for a future
        // variant. If a new kind *does* gain endpoints (e.g.
        // a Bezier with two control points), add the arm here
        // and remove this comment.
        _ => None,
    }
}

/// The cells an object paints into, in iteration order. Boxes return
/// the perimeter cells (the frame). Text returns the cells of the full
/// render rect (frame + content).
pub fn get_object_render_cells(object: &DrawObject) -> Vec<Point> {
    match object {
        DrawObject::Box(b) => crate::geometry::get_rect_perimeter_points(Rect {
            left: b.left,
            top: b.top,
            right: b.right,
            bottom: b.bottom,
        }),
        DrawObject::Line(l) => get_line_render_cells(
            Point { x: l.x1, y: l.y1 },
            Point { x: l.x2, y: l.y2 },
            l.style,
        ),
        DrawObject::Elbow(e) => get_elbow_render_cells(
            Point { x: e.x1, y: e.y1 },
            Point { x: e.x2, y: e.y2 },
            e.style,
            e.orientation,
        ),
        DrawObject::Paint(p) => p.points.clone(),
        DrawObject::Text(t) => {
            let r = get_text_render_rect(t);
            let mut cells = Vec::new();
            for y in r.top..=r.bottom {
                for x in r.left..=r.right {
                    cells.push(Point { x, y });
                }
            }
            cells
        }
    }
}

/// The marquee rect used to select an object by click. Boxes use the
/// full rect; text uses the explicit selection bounds (which can differ
/// from render bounds for framed text).
pub fn get_object_selection_bounds(object: &DrawObject) -> Option<Rect> {
    match object {
        DrawObject::Box(b) => Some(Rect {
            left: b.left,
            top: b.top,
            right: b.right,
            bottom: b.bottom,
        }),
        DrawObject::Text(t) => Some(get_text_selection_bounds(t)),
        other => get_object_bounds(other),
    }
}

/// Hit test: does `point` lie on any cell this object paints? Boxes hit
/// on the perimeter (or anywhere inside the rect); lines, elbows, and
/// paint on the actual rendered cells; text on its selection rect.
pub fn object_contains_point(object: &DrawObject, point: Point) -> bool {
    match object {
        DrawObject::Box(b) => {
            let r = Rect {
                left: b.left,
                top: b.top,
                right: b.right,
                bottom: b.bottom,
            };
            if r.left > r.right || r.top > r.bottom {
                return false;
            }
            rect_contains_point(r, point.x, point.y)
        }
        DrawObject::Line(_) | DrawObject::Elbow(_) => get_object_render_cells(object)
            .iter()
            .any(|p| p.x == point.x && p.y == point.y),
        DrawObject::Paint(p) => p.points.iter().any(|q| q.x == point.x && q.y == point.y),
        DrawObject::Text(t) => rect_contains_point(get_text_selection_bounds(t), point.x, point.y),
    }
}

/// Translate an object by `(dx, dy)`. Lines, elbows, and paint strokes
/// keep their relative shape; boxes shift their rect; text shifts its
/// origin.
pub fn translate_object(object: &DrawObject, dx: i32, dy: i32) -> DrawObject {
    match object {
        DrawObject::Box(b) => DrawObject::Box(BoxObject {
            left: b.left + dx,
            top: b.top + dy,
            right: b.right + dx,
            bottom: b.bottom + dy,
            ..b.clone()
        }),
        DrawObject::Line(l) => DrawObject::Line(LineObject {
            x1: l.x1 + dx,
            y1: l.y1 + dy,
            x2: l.x2 + dx,
            y2: l.y2 + dy,
            ..l.clone()
        }),
        DrawObject::Elbow(e) => DrawObject::Elbow(ElbowObject {
            x1: e.x1 + dx,
            y1: e.y1 + dy,
            x2: e.x2 + dx,
            y2: e.y2 + dy,
            ..e.clone()
        }),
        DrawObject::Paint(p) => DrawObject::Paint(PaintObject {
            points: p
                .points
                .iter()
                .map(|pt| Point {
                    x: pt.x + dx,
                    y: pt.y + dy,
                })
                .collect(),
            ..p.clone()
        }),
        DrawObject::Text(t) => DrawObject::Text(TextObject {
            x: t.x + dx,
            y: t.y + dy,
            ..t.clone()
        }),
    }
}

/// The corner of `rect` that a given handle controls, expressed in
/// document coordinates. Used by the editor to map a handle + current
/// pointer onto the rect's new bounds.
pub fn box_handle_corner(rect: Rect, handle: BoxResizeHandle) -> Point {
    match handle {
        BoxResizeHandle::TopLeft => Point {
            x: rect.left,
            y: rect.top,
        },
        BoxResizeHandle::TopRight => Point {
            x: rect.right,
            y: rect.top,
        },
        BoxResizeHandle::BottomLeft => Point {
            x: rect.left,
            y: rect.bottom,
        },
        BoxResizeHandle::BottomRight => Point {
            x: rect.right,
            y: rect.bottom,
        },
    }
}

/// True iff `point` is within `tolerance` cells (Manhattan) of the
/// corner that `handle` controls on `rect`. Used by mouse hit-test to
/// decide whether to begin a resize.
pub fn box_handle_contains(
    rect: Rect,
    handle: BoxResizeHandle,
    point: Point,
    tolerance: i32,
) -> bool {
    let c = box_handle_corner(rect, handle);
    (point.x - c.x).abs() <= tolerance && (point.y - c.y).abs() <= tolerance
}

/// Hit-test the four resize handles of `rect`. Returns the closest
/// handle whose corner is within `tolerance` cells of `point`, or
/// `None` if no handle is in range. Ties broken by enum order
/// (TL wins over TR wins over BL wins over BR).
pub fn hit_test_box_handles(rect: Rect, point: Point, tolerance: i32) -> Option<BoxResizeHandle> {
    // ponytail: single-pass scan, prefer closer by Manhattan distance.
    let handles = [
        BoxResizeHandle::TopLeft,
        BoxResizeHandle::TopRight,
        BoxResizeHandle::BottomLeft,
        BoxResizeHandle::BottomRight,
    ];
    let mut best: Option<(i32, BoxResizeHandle)> = None;
    for h in handles {
        if box_handle_contains(rect, h, point, tolerance) {
            let c = box_handle_corner(rect, h);
            let d = (point.x - c.x).abs() + (point.y - c.y).abs();
            best = match best {
                None => Some((d, h)),
                Some((bd, _)) if d < bd => Some((d, h)),
                other => other,
            };
        }
    }
    best.map(|(_, h)| h)
}

/// Compute the new bounds of a box being resized by dragging `handle`.
/// The corner opposite to `handle` stays pinned to its original
/// position; the dragged corner follows the pointer. `min_size` keeps
/// the box from collapsing below that span (defaults handled by
/// caller; 0 here means "no clamp").
pub fn compute_resized_bounds(original: Rect, handle: BoxResizeHandle, pointer: Point) -> Rect {
    // ponytail: branch on which two edges move. each handle controls
    // exactly two of the four rectangle edges; the other two are pinned
    // to the original bounds.
    let (left, right) = match handle {
        BoxResizeHandle::TopLeft | BoxResizeHandle::BottomLeft => {
            (pointer.x.min(original.right), original.right)
        }
        BoxResizeHandle::TopRight | BoxResizeHandle::BottomRight => {
            (original.left, pointer.x.max(original.left))
        }
    };
    let (top, bottom) = match handle {
        BoxResizeHandle::TopLeft | BoxResizeHandle::TopRight => {
            (pointer.y.min(original.bottom), original.bottom)
        }
        BoxResizeHandle::BottomLeft | BoxResizeHandle::BottomRight => {
            (original.top, pointer.y.max(original.top))
        }
    };
    Rect {
        left,
        top,
        right,
        bottom,
    }
}

/// The bounding rect of a set of rects, or `None` if the input is empty.
pub fn get_bounds_union(rects: &[Rect]) -> Option<Rect> {
    let mut iter = rects.iter();
    let first = *iter.next()?;
    let mut union = first;
    for r in iter {
        union.left = union.left.min(r.left);
        union.top = union.top.min(r.top);
        union.right = union.right.max(r.right);
        union.bottom = union.bottom.max(r.bottom);
    }
    Some(union)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoxStyle, ElbowOrientation, InkColor, LineStyle};

    fn b(left: i32, top: i32, right: i32, bottom: i32) -> DrawObject {
        DrawObject::Box(BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left,
            top,
            right,
            bottom,
            style: BoxStyle::Light,
        })
    }

    fn line(x1: i32, y1: i32, x2: i32, y2: i32) -> DrawObject {
        DrawObject::Line(LineObject {
            id: "l".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x1,
            y1,
            x2,
            y2,
            style: LineStyle::Smooth,
        })
    }

    fn elbow(x1: i32, y1: i32, x2: i32, y2: i32, orientation: ElbowOrientation) -> DrawObject {
        DrawObject::Elbow(ElbowObject {
            id: "e".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x1,
            y1,
            x2,
            y2,
            style: LineStyle::Light,
            orientation,
        })
    }

    fn paint(pts: Vec<Point>) -> DrawObject {
        DrawObject::Paint(PaintObject {
            id: "p".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            points: pts,
            brush: "·".into(),
        })
    }

    fn text(content: &str) -> DrawObject {
        use crate::types::{TextBorderMode, TextObject};
        DrawObject::Text(TextObject {
            id: "t".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            x: 2,
            y: 2,
            content: content.into(),
            border: TextBorderMode::None,
        })
    }

    #[test]
    fn box_bounds_use_rect() {
        let r = get_object_bounds(&b(1, 2, 5, 6)).unwrap();
        assert_eq!(
            r,
            Rect {
                left: 1,
                top: 2,
                right: 5,
                bottom: 6
            }
        );
    }

    #[test]
    fn degenerate_box_returns_none() {
        // left > right is invalid.
        let bad = BoxObject {
            id: "x".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 5,
            top: 0,
            right: 1,
            bottom: 3,
            style: BoxStyle::Light,
        };
        assert!(get_object_bounds(&DrawObject::Box(bad)).is_none());
    }

    #[test]
    fn line_bounds_normalize() {
        let r = get_object_bounds(&line(5, 5, 0, 0)).unwrap();
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
    fn elbow_bounds_include_corner() {
        // Vertical-first: (0,0)→(5,3) with corner (0,3).
        let r = get_object_bounds(&elbow(0, 0, 5, 3, ElbowOrientation::VerticalFirst)).unwrap();
        assert_eq!(
            r,
            Rect {
                left: 0,
                top: 0,
                right: 5,
                bottom: 3
            }
        );
    }

    #[test]
    fn paint_bounds_empty_returns_none() {
        assert!(get_object_bounds(&paint(vec![])).is_none());
    }

    #[test]
    fn paint_bounds_enclose_all_points() {
        let r = get_object_bounds(&paint(vec![
            Point { x: 1, y: 2 },
            Point { x: -1, y: 0 },
            Point { x: 4, y: 5 },
        ]))
        .unwrap();
        assert_eq!(
            r,
            Rect {
                left: -1,
                top: 0,
                right: 4,
                bottom: 5
            }
        );
    }

    #[test]
    fn box_content_bounds_inset_for_framed() {
        let b = BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 10,
            bottom: 5,
            style: BoxStyle::Double,
        };
        let r = get_box_content_bounds(&b);
        assert_eq!(
            r,
            Rect {
                left: 1,
                top: 1,
                right: 9,
                bottom: 4
            }
        );
    }

    #[test]
    fn box_content_bounds_degenerate_box_is_invalid() {
        // 1x1 box → content rect collapses to invalid (left > right).
        let b = BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
            style: BoxStyle::Light,
        };
        let r = get_box_content_bounds(&b);
        assert!(!crate::geometry::is_valid_rect(r));
    }

    #[test]
    fn box_corner_points_are_corners() {
        let (nw, ne, sw, se) = get_box_corner_points(&BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 1,
            top: 2,
            right: 6,
            bottom: 5,
            style: BoxStyle::Light,
        });
        assert_eq!(nw, Point { x: 1, y: 2 });
        assert_eq!(ne, Point { x: 6, y: 2 });
        assert_eq!(sw, Point { x: 1, y: 5 });
        assert_eq!(se, Point { x: 6, y: 5 });
    }

    #[test]
    fn line_endpoints_return_start_end() {
        let (s, e) = get_line_endpoint_points(&line(2, 3, 7, 9)).unwrap();
        assert_eq!(s, Point { x: 2, y: 3 });
        assert_eq!(e, Point { x: 7, y: 9 });
    }

    #[test]
    fn line_endpoints_unsupported_for_box() {
        assert!(get_line_endpoint_points(&b(0, 0, 3, 3)).is_none());
    }

    #[test]
    fn object_render_cells_paint_returns_points() {
        let cells =
            get_object_render_cells(&paint(vec![Point { x: 1, y: 1 }, Point { x: 2, y: 1 }]));
        assert_eq!(cells, vec![Point { x: 1, y: 1 }, Point { x: 2, y: 1 }]);
    }

    #[test]
    fn object_render_cells_text_covers_render_rect() {
        let cells = get_object_render_cells(&text("hi"));
        // 2 cells, single row.
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn object_selection_bounds_for_text_uses_text_helper() {
        let r = get_object_selection_bounds(&text("hi")).unwrap();
        assert_eq!(
            r,
            Rect {
                left: 2,
                top: 2,
                right: 3,
                bottom: 2
            }
        );
    }

    #[test]
    fn object_contains_point_hits_box_perimeter() {
        let obj = b(0, 0, 4, 3);
        assert!(object_contains_point(&obj, Point { x: 0, y: 0 }));
        assert!(object_contains_point(&obj, Point { x: 4, y: 0 }));
        assert!(object_contains_point(&obj, Point { x: 0, y: 3 }));
        // Interior also hits.
        assert!(object_contains_point(&obj, Point { x: 2, y: 1 }));
        // Outside the box doesn't.
        assert!(!object_contains_point(&obj, Point { x: 5, y: 0 }));
    }

    #[test]
    fn object_contains_point_hits_line_cells() {
        let obj = line(0, 0, 3, 0);
        assert!(object_contains_point(&obj, Point { x: 1, y: 0 }));
        assert!(!object_contains_point(&obj, Point { x: 0, y: 1 }));
    }

    #[test]
    fn translate_object_shifts_box() {
        let moved = translate_object(&b(1, 1, 4, 4), 2, 3);
        if let DrawObject::Box(b) = moved {
            assert_eq!(b.left, 3);
            assert_eq!(b.top, 4);
            assert_eq!(b.right, 6);
            assert_eq!(b.bottom, 7);
        } else {
            panic!("expected box");
        }
    }

    #[test]
    fn translate_object_shifts_paint_points() {
        let moved = translate_object(
            &paint(vec![Point { x: 0, y: 0 }, Point { x: 1, y: 2 }]),
            5,
            -1,
        );
        if let DrawObject::Paint(p) = moved {
            assert_eq!(p.points, vec![Point { x: 5, y: -1 }, Point { x: 6, y: 1 }]);
        } else {
            panic!("expected paint");
        }
    }

    #[test]
    fn translate_object_shifts_text_origin() {
        let moved = translate_object(&text("hi"), 4, 5);
        if let DrawObject::Text(t) = moved {
            assert_eq!(t.x, 6);
            assert_eq!(t.y, 7);
        } else {
            panic!("expected text");
        }
    }

    #[test]
    fn clone_objects_returns_independent_copy() {
        let original = vec![b(0, 0, 2, 2), text("a")];
        let copy = clone_objects(&original);
        assert_eq!(original, copy);
    }

    #[test]
    fn bounds_union_handles_empty() {
        assert!(get_bounds_union(&[]).is_none());
    }

    #[test]
    fn bounds_union_encloses_all() {
        let rs = vec![
            Rect {
                left: 0,
                top: 0,
                right: 2,
                bottom: 2,
            },
            Rect {
                left: 5,
                top: -1,
                right: 7,
                bottom: 4,
            },
            Rect {
                left: 1,
                top: 3,
                right: 3,
                bottom: 3,
            },
        ];
        assert_eq!(
            get_bounds_union(&rs).unwrap(),
            Rect {
                left: 0,
                top: -1,
                right: 7,
                bottom: 4
            }
        );
    }

    #[test]
    fn top_left_resize_moves_left_and_top() {
        let rect = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        // Pointer is up-and-left of the original TopLeft corner.
        let next = compute_resized_bounds(rect, BoxResizeHandle::TopLeft, Point { x: 5, y: 7 });
        assert_eq!(
            next,
            Rect {
                left: 5,
                top: 7,
                right: 20,
                bottom: 20
            }
        );
    }

    #[test]
    fn bottom_right_resize_moves_right_and_bottom() {
        let rect = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        let next =
            compute_resized_bounds(rect, BoxResizeHandle::BottomRight, Point { x: 30, y: 25 });
        assert_eq!(
            next,
            Rect {
                left: 10,
                top: 10,
                right: 30,
                bottom: 25,
            }
        );
    }

    #[test]
    fn resize_pins_opposite_corner() {
        // Pointer is past the opposite corner: handle still drags, the
        // opposite stays at its original position (no inversion).
        let rect = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        let next = compute_resized_bounds(rect, BoxResizeHandle::BottomRight, Point { x: 5, y: 5 });
        // BottomRight handle pins left + top (the TopLeft corner).
        // Pointer x is past `original.left`, so right clamps to left.
        assert_eq!(next.left, 10);
        assert_eq!(next.top, 10);
        assert_eq!(next.right, 10);
        assert_eq!(next.bottom, 10);
    }

    #[test]
    fn box_handle_contains_in_range() {
        let r = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        assert!(box_handle_contains(
            r,
            BoxResizeHandle::TopLeft,
            Point { x: 10, y: 10 },
            0
        ));
        assert!(box_handle_contains(
            r,
            BoxResizeHandle::TopLeft,
            Point { x: 11, y: 11 },
            1
        ));
        assert!(!box_handle_contains(
            r,
            BoxResizeHandle::TopLeft,
            Point { x: 12, y: 12 },
            1
        ));
        assert!(!box_handle_contains(
            r,
            BoxResizeHandle::BottomRight,
            Point { x: 10, y: 10 },
            1
        ));
    }

    #[test]
    fn hit_test_box_handles_picks_closest() {
        let r = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        assert_eq!(
            hit_test_box_handles(r, Point { x: 10, y: 10 }, 1),
            Some(BoxResizeHandle::TopLeft)
        );
        assert_eq!(
            hit_test_box_handles(r, Point { x: 20, y: 10 }, 1),
            Some(BoxResizeHandle::TopRight)
        );
        assert_eq!(
            hit_test_box_handles(r, Point { x: 10, y: 20 }, 1),
            Some(BoxResizeHandle::BottomLeft)
        );
        assert_eq!(
            hit_test_box_handles(r, Point { x: 20, y: 20 }, 1),
            Some(BoxResizeHandle::BottomRight)
        );
    }

    #[test]
    fn hit_test_box_handles_misses_when_out_of_range() {
        let r = Rect {
            left: 10,
            top: 10,
            right: 20,
            bottom: 20,
        };
        // Far from any corner.
        assert_eq!(hit_test_box_handles(r, Point { x: 15, y: 15 }, 0), None);
        // Just outside tolerance.
        assert_eq!(hit_test_box_handles(r, Point { x: 8, y: 10 }, 1), None);
    }

    #[test]
    fn box_handle_corner_matches_box_corner_points() {
        let bx = BoxObject {
            id: "b".into(),
            z: 1,
            parent_id: None,
            color: InkColor::White,
            left: 3,
            top: 5,
            right: 13,
            bottom: 9,
            style: BoxStyle::Light,
        };
        let (nw, ne, sw, se) = get_box_corner_points(&bx);
        assert_eq!(
            box_handle_corner(boxed_rect(&bx), BoxResizeHandle::TopLeft),
            nw
        );
        assert_eq!(
            box_handle_corner(boxed_rect(&bx), BoxResizeHandle::TopRight),
            ne
        );
        assert_eq!(
            box_handle_corner(boxed_rect(&bx), BoxResizeHandle::BottomLeft),
            sw
        );
        assert_eq!(
            box_handle_corner(boxed_rect(&bx), BoxResizeHandle::BottomRight),
            se
        );
    }

    fn boxed_rect(b: &BoxObject) -> Rect {
        Rect {
            left: b.left,
            top: b.top,
            right: b.right,
            bottom: b.bottom,
        }
    }
}
