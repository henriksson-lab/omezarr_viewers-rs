//! What a drawing gesture means, and what a drag does to a shape.
//!
//! Every function here is pure: geometry in, geometry out, no `App` and no DOM.
//! That is the whole reason they live apart — it is what lets them be tested on
//! the host with a plain `#[cfg(test)] mod tests` rather than in a browser.

use omezarr_viewer_common::{Annotation, Geometry};

use crate::viewer_canvas::{Drawn, EditKind, Editing, Handle, Tool};

/// The geometry a finished drawing gesture means, and whether it is an ellipse.
///
/// The tool decides the shape, not the number of points: a two-point drag is a
/// rectangle under one tool and an ellipse under another, and a traced path is
/// a closed region or an open one depending on which pencil was in hand.
pub(super) fn geometry_of(drawn: &Drawn) -> Option<(Geometry, bool)> {
    let points: Vec<[f64; 2]> = drawn
        .points
        .iter()
        .map(|(x, y)| [*x as f64, *y as f64])
        .collect();
    let corners = drawn.corners();
    Some(match drawn.tool {
        Tool::Point => (Geometry::Point(*points.first()?), false),
        Tool::Box => {
            let (x0, y0, x1, y1) = corners?;
            (
                Geometry::rect(x0 as f64, y0 as f64, x1 as f64, y1 as f64),
                false,
            )
        }
        Tool::Ellipse => {
            // GeoJSON has no ellipse, so it is stored as the polygon QuPath
            // would store, plus the `isEllipse` flag that lets QuPath — and
            // this viewer — rebuild the real thing from the bounding box.
            let (x0, y0, x1, y1) = corners?;
            let ring: Vec<[f64; 2]> = crate::viewer_canvas::ellipse_path(x0, y0, x1, y1)
                .iter()
                .map(|(x, y)| [*x as f64, *y as f64])
                .collect();
            (Geometry::Polygon(vec![ring]), true)
        }
        Tool::Polygon | Tool::Freehand => {
            let mut ring = simplify(points);
            if ring.len() < 3 {
                return None;
            }
            if ring.first() != ring.last() {
                ring.push(*ring.first()?);
            }
            (Geometry::Polygon(vec![ring]), false)
        }
        Tool::Polyline | Tool::Line => {
            let path = simplify(points);
            if path.len() < 2 {
                return None;
            }
            (Geometry::LineString(path), false)
        }
        Tool::Pan => return None,
    })
}

/// Drop points a freehand trace put on top of each other.
///
/// A mouse-move fires far more often than the hand moves a pixel, so a traced
/// ring arrives with runs of near-identical vertices; keeping them makes a
/// hundred-point shape out of a ten-point one and gives the vertex editor
/// handles nobody can tell apart.
fn simplify(points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    const MIN_STEP: f64 = 1.0;
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(points.len());
    for point in points {
        match out.last() {
            Some(last)
                if (point[0] - last[0]).abs() < MIN_STEP
                    && (point[1] - last[1]).abs() < MIN_STEP => {}
            _ => out.push(point),
        }
    }
    out
}

/// Apply a finished drag to an annotation. Returns false if it changed nothing.
///
/// Every case here is a geometry transform rather than a coordinate assignment,
/// which is what lets one function serve a point, a rectangle, an ellipse and a
/// hundred-vertex freehand ring.
pub(super) fn apply_edit(item: &mut Annotation, editing: &Editing) -> bool {
    let (dx, dy) = editing.delta();
    let (dx, dy) = (dx as f64, dy as f64);

    match (editing.kind, editing.handle) {
        (EditKind::Drag, Handle::Body) => {
            item.geometry.translate(dx, dy);
            true
        }
        (EditKind::Drag, Handle::Corner(west, north)) => {
            // Scale about the corner *opposite* the one grabbed, so that corner
            // stays where it is — which is what a resize handle means.
            let Some([x0, y0, x1, y1]) = item.bounds() else {
                return false;
            };
            let (w, h) = (x1 - x0, y1 - y0);
            if w <= 0.0 || h <= 0.0 {
                return false;
            }
            let (anchor_x, sx) = if west {
                (x1, (w - dx) / w)
            } else {
                (x0, (w + dx) / w)
            };
            let (anchor_y, sy) = if north {
                (y1, (h - dy) / h)
            } else {
                (y0, (h + dy) / h)
            };
            // A drag that would turn the shape inside out is refused rather than
            // mirrored: nobody drags a corner past its opposite on purpose, and
            // a mirrored polygon is very hard to undo by hand.
            if sx <= 0.0 || sy <= 0.0 {
                return false;
            }
            item.geometry.scale_about(anchor_x, anchor_y, sx, sy);
            true
        }
        (EditKind::Drag, Handle::Vertex(path, vertex)) => {
            item.geometry.move_vertex(path, vertex, dx, dy)
        }
        (EditKind::DeleteVertex, Handle::Vertex(path, vertex)) => {
            item.geometry.remove_vertex(path, vertex)
        }
        (EditKind::InsertVertex, Handle::Vertex(path, vertex)) => item.geometry.insert_vertex(
            path,
            vertex,
            [editing.from.0 as f64, editing.from.1 as f64],
        ),
        _ => false,
    }
}

/// Is this shape exactly the axis-aligned rectangle of its own bounds?
///
/// Not a stored flag but an inspection, which is how QuPath recognises one too:
/// a rectangle polygonised and read back has no marker on it, only four corners
/// that happen to be the corners of its bounding box.
pub(super) fn is_axis_aligned_rect(item: &Annotation) -> bool {
    let Geometry::Polygon(rings) = &item.geometry else {
        return false;
    };
    if rings.len() != 1 {
        return false;
    }
    let Some([x0, y0, x1, y1]) = item.bounds() else {
        return false;
    };
    let mut corners = rings[0].clone();
    if corners.len() > 1 && corners.first() == corners.last() {
        corners.pop();
    }
    corners.len() == 4
        && corners
            .iter()
            .all(|p| (p[0] == x0 || p[0] == x1) && (p[1] == y0 || p[1] == y1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer_canvas::ELLIPSE_SEGMENTS;

    fn drawn(tool: Tool, points: &[(f32, f32)]) -> Drawn {
        Drawn {
            tool,
            points: points.to_vec(),
        }
    }

    #[test]
    fn each_tool_makes_the_geometry_it_claims() {
        /// A tool, the points a hand gave it, and the GeoJSON type it owes.
        type Case = (Tool, Vec<(f32, f32)>, &'static str);
        let cases: Vec<Case> = vec![
            (Tool::Point, vec![(5.0, 6.0)], "Point"),
            (Tool::Box, vec![(0.0, 0.0), (10.0, 10.0)], "Polygon"),
            (Tool::Ellipse, vec![(0.0, 0.0), (10.0, 6.0)], "Polygon"),
            (
                Tool::Polygon,
                vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)],
                "Polygon",
            ),
            (
                Tool::Freehand,
                vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)],
                "Polygon",
            ),
            (Tool::Polyline, vec![(0.0, 0.0), (10.0, 5.0)], "LineString"),
            (Tool::Line, vec![(0.0, 0.0), (10.0, 5.0)], "LineString"),
        ];
        for (tool, points, want) in cases {
            let (geometry, _) = geometry_of(&drawn(tool, &points))
                .unwrap_or_else(|| panic!("{tool:?} produced nothing"));
            let got = match geometry {
                Geometry::Point(_) => "Point",
                Geometry::Polygon(_) => "Polygon",
                Geometry::LineString(_) => "LineString",
                _ => "other",
            };
            assert_eq!(got, want, "{tool:?}");
        }
        // The pan tool draws nothing at all.
        assert!(geometry_of(&drawn(Tool::Pan, &[(0.0, 0.0), (1.0, 1.0)])).is_none());
    }

    #[test]
    fn only_the_ellipse_is_flagged_as_one() {
        // A polygonised ellipse cannot be recovered from its vertices, which is
        // why it needs the flag; a rectangle is recognised by inspection.
        let (_, ellipse) = geometry_of(&drawn(Tool::Ellipse, &[(0.0, 0.0), (10.0, 6.0)])).unwrap();
        let (_, rectangle) = geometry_of(&drawn(Tool::Box, &[(0.0, 0.0), (10.0, 6.0)])).unwrap();
        assert!(ellipse);
        assert!(!rectangle);
    }

    #[test]
    fn a_rectangle_comes_out_the_right_way_round_and_closed() {
        // Drawn right-to-left and bottom-to-top, as half of all drags are.
        let (geometry, _) = geometry_of(&drawn(Tool::Box, &[(30.0, 40.0), (10.0, 20.0)])).unwrap();
        let Geometry::Polygon(rings) = geometry else {
            panic!("not a polygon")
        };
        assert_eq!(rings[0][0], [10.0, 20.0]);
        assert_eq!(rings[0].len(), 5);
        assert_eq!(rings[0].first(), rings[0].last());
    }

    #[test]
    fn an_ellipse_is_a_closed_ring_of_the_segment_count() {
        let (geometry, _) =
            geometry_of(&drawn(Tool::Ellipse, &[(0.0, 0.0), (20.0, 10.0)])).unwrap();
        let Geometry::Polygon(rings) = geometry else {
            panic!("not a polygon")
        };
        assert_eq!(rings[0].len(), ELLIPSE_SEGMENTS + 1, "closed");
        // Inscribed in the drag: the extremes are the corners it was dragged to.
        let xs: Vec<f64> = rings[0].iter().map(|p| p[0]).collect();
        assert!(xs.iter().cloned().fold(f64::MIN, f64::max) <= 20.001);
        assert!(xs.iter().cloned().fold(f64::MAX, f64::min) >= -0.001);
    }

    #[test]
    fn a_shape_that_went_nowhere_is_refused() {
        // Too few vertices to be the thing the tool makes.
        assert!(geometry_of(&drawn(Tool::Polygon, &[(0.0, 0.0), (1.0, 1.0)])).is_none());
        assert!(geometry_of(&drawn(Tool::Polyline, &[(0.0, 0.0)])).is_none());
        // …but a point is a single click by nature.
        assert!(geometry_of(&drawn(Tool::Point, &[(0.0, 0.0)])).is_some());
    }

    #[test]
    fn a_traced_path_drops_the_points_the_hand_did_not_move_between() {
        // A mouse-move fires far more often than a hand moves a pixel, so a
        // freehand ring arrives with runs of near-identical vertices; keeping
        // them gives the vertex editor handles nobody can tell apart.
        let mut points = vec![(0.0f32, 0.0f32)];
        for i in 0..20 {
            points.push((i as f32 * 0.1, 0.0));
        }
        points.push((50.0, 0.0));
        points.push((25.0, 50.0));
        let (geometry, _) = geometry_of(&drawn(Tool::Freehand, &points)).unwrap();
        let Geometry::Polygon(rings) = geometry else {
            panic!()
        };
        assert!(
            rings[0].len() <= 5,
            "twenty sub-pixel steps collapsed: {:?}",
            rings[0]
        );
    }

    #[test]
    fn a_rectangle_is_recognised_by_inspection_and_a_triangle_is_not() {
        // This decides corner handles versus vertex handles, so a wrong answer
        // is a shape that cannot be edited the way it looks like it should.
        let rect = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            ..Default::default()
        };
        assert!(is_axis_aligned_rect(&rect));

        let triangle = Annotation {
            geometry: Geometry::Polygon(vec![vec![
                [0.0, 0.0],
                [10.0, 0.0],
                [5.0, 10.0],
                [0.0, 0.0],
            ]]),
            ..Default::default()
        };
        assert!(!is_axis_aligned_rect(&triangle));

        // A rotated square has four corners but they are not the bounding box's.
        let diamond = Annotation {
            geometry: Geometry::Polygon(vec![vec![
                [5.0, 0.0],
                [10.0, 5.0],
                [5.0, 10.0],
                [0.0, 5.0],
                [5.0, 0.0],
            ]]),
            ..Default::default()
        };
        assert!(!is_axis_aligned_rect(&diamond));

        // A rectangle with a hole is not a rectangle to edit by its corners.
        let holed = Annotation {
            geometry: Geometry::Polygon(vec![
                vec![
                    [0.0, 0.0],
                    [10.0, 0.0],
                    [10.0, 10.0],
                    [0.0, 10.0],
                    [0.0, 0.0],
                ],
                vec![[2.0, 2.0], [4.0, 2.0], [4.0, 4.0], [2.0, 4.0], [2.0, 2.0]],
            ]),
            ..Default::default()
        };
        assert!(!is_axis_aligned_rect(&holed));

        let point = Annotation::default();
        assert!(!is_axis_aligned_rect(&point));
    }

    #[test]
    fn a_corner_drag_that_would_turn_a_shape_inside_out_is_refused() {
        // Nobody drags a corner past its opposite on purpose, and a mirrored
        // polygon is very hard to undo by hand.
        let mut item = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            ..Default::default()
        };
        let editing = Editing {
            id: 0,
            handle: Handle::Corner(false, false),
            kind: EditKind::Drag,
            from: (10.0, 10.0),
            to: (-50.0, -50.0),
        };
        assert!(!apply_edit(&mut item, &editing));
        assert_eq!(item.bounds(), Some([0.0, 0.0, 10.0, 10.0]), "unchanged");
    }

    #[test]
    fn a_body_drag_moves_every_coordinate() {
        let mut item = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            ..Default::default()
        };
        let editing = Editing {
            id: 0,
            handle: Handle::Body,
            kind: EditKind::Drag,
            from: (5.0, 5.0),
            to: (15.0, 25.0),
        };
        assert!(apply_edit(&mut item, &editing));
        assert_eq!(item.bounds(), Some([10.0, 20.0, 20.0, 30.0]));
    }
}
