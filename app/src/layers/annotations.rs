//! An annotation layer's state, and the buffers it is drawn from.
//!
//! Batching is the point: shapes are grouped by colour so the renderer makes
//! one draw call per colour rather than one per shape, and points, outlines and
//! fills go to separate buffers because each is a different shader program.

use std::collections::HashMap;

use omezarr_viewer_common::{Annotation, Geometry, ObjectType, Point};

use super::LayerStyle;

/// UI state for an annotation layer — the one kind the viewer writes.
///
/// The rows themselves live here rather than in a separate map, unlike an
/// object layer's: there are as many as somebody drew by hand, every edit
/// replaces the whole list anyway, and holding them beside the colour they are
/// drawn in means one place to look when a shape is in the wrong spot.
#[derive(Clone, PartialEq)]
pub struct AnnotUiState {
    pub annotations: Vec<Annotation>,
    /// Where a save with no explicit target would write.
    pub target: Option<String>,
    /// How it is drawn.
    pub style: LayerStyle,
    /// Colour each shape by its class instead of by `color`.
    pub color_by_class: bool,
    /// Size points by a radius in *world* pixels rather than by `style.size` in
    /// screen pixels.
    ///
    /// Off by default, which keeps a plain point annotation the marker it has
    /// always been. On, a point is a **pick**: a circle whose size is a claim
    /// about the image, so it grows and shrinks with the zoom and the question
    /// "does this enclose the particle" has the same answer at every zoom.
    pub world_radius: bool,
    /// The radius a class with no radius of its own draws at, in world pixels.
    pub radius: f32,
    /// The radius per class, where one has been set.
    ///
    /// Per class because that is the granularity the fact has: in cryo-EM a
    /// dataset has one box size per particle type, so a radius per shape would
    /// be a thousand copies of one number and a thousand chances to disagree
    /// with it. Stored rather than derived — unlike `class_color`, which can be
    /// hashed from the name, a box size is something only the annotator knows.
    pub class_radii: HashMap<String, f32>,
    /// Fill regions as well as outlining them, as QuPath's "Fill annotations"
    /// does. Off by default, which is QuPath's default too: a fill hides the
    /// pixels the shape was drawn around.
    pub filled: bool,
    /// The annotation the last click selected.
    pub selected: Option<u64>,
    /// The class the *next* shape drawn into this layer gets.
    pub class: String,
    /// The object type the *next* shape gets.
    ///
    /// QuPath's `objectType` is a processing role, not a semantic kind — the
    /// kind is the classification above. It is set here because it is a property
    /// of the *source* of the shapes ("these are hand-drawn regions", "these are
    /// detector output"), and because QuPath treats detections as bulk data:
    /// marking machine output as such is what keeps it out of QuPath's
    /// annotation list and fast with thousands of objects.
    pub object_type: ObjectType,
    /// Show only this class, when one is chosen.
    pub filter: Option<String>,
    /// The stroke width the *next* open path drawn here gets, in world pixels.
    ///
    /// `None` is the default and is not "zero": it is a *geometric* line, a
    /// curve of no area covering no pixels, which is what GeoJSON and QuPath
    /// mean by a `LineString`. `Some(w)` makes the path a **scribble** — the
    /// pixels within `w / 2` of it — which is the form partial supervision
    /// takes: it says something about the pixels it covers and nothing about
    /// any other, which a closed region cannot say.
    ///
    /// World pixels, like every other coordinate here, so the assertion does
    /// not change with the zoom the curator happened to be at.
    pub stroke_width: Option<f64>,
    /// What the save box holds, which starts as the target and is editable.
    pub save_target: String,
    /// True between asking to save and hearing back.
    pub saving: bool,
    /// What the last save said, good or bad.
    pub status: Option<String>,
    /// Has anything changed since the last save?
    ///
    /// Tracked rather than derived: comparing against the rows on disk would
    /// mean holding a second copy of them, and the question this answers — "is
    /// there work here that closing the tab would lose" — is about edits, not
    /// about whether the edits happened to cancel out.
    pub dirty: bool,
}

impl AnnotUiState {
    pub fn new(annotations: Vec<Annotation>, target: Option<String>) -> Self {
        Self {
            annotations,
            save_target: target.clone().unwrap_or_default(),
            target,
            style: LayerStyle {
                color: [0.2, 0.9, 1.0],
                opacity: 0.95,
                size: 11.0,
                slab: 8.0,
            },
            color_by_class: false,
            world_radius: false,
            radius: 20.0,
            class_radii: HashMap::new(),
            filled: false,
            selected: None,
            class: String::new(),
            object_type: ObjectType::Annotation,
            filter: None,
            stroke_width: None,
            saving: false,
            status: None,
            dirty: false,
        }
    }

    /// Adopt another state's *view* settings, keeping these rows.
    ///
    /// Split from a wholesale clone because an annotation layer's rows arrive
    /// with the session while its colours and filters do not: reusing the old
    /// state entirely would discard whatever the server just said, and reusing
    /// none of it would reset the panel on every reload.
    pub fn keep_view_of(&mut self, old: &Self) {
        self.style = old.style;
        self.color_by_class = old.color_by_class;
        self.world_radius = old.world_radius;
        self.radius = old.radius;
        self.class_radii = old.class_radii.clone();
        self.filled = old.filled;
        self.class = old.class.clone();
        self.object_type = old.object_type;
        self.filter = old.filter.clone();
        self.stroke_width = old.stroke_width;
        self.save_target = old.save_target.clone();
        self.status = old.status.clone();
        self.dirty = old.dirty;
        // A selection only survives if the row it names did.
        self.selected = old
            .selected
            .filter(|id| self.annotations.iter().any(|a| a.id == *id));
    }

    /// Every class in the layer, in first-seen order, with the unclassified
    /// shapes under an empty name.
    pub fn classes(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for item in &self.annotations {
            if !seen.iter().any(|c| c == &item.label) {
                seen.push(item.label.clone());
            }
        }
        seen
    }

    /// Is this annotation drawn at all, given the class filter and the plane?
    pub fn shows(&self, item: &Annotation, z: i32, t: i32) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|class| &item.label == class)
            && item.at_plane(z, t)
    }

    /// How many shapes here assert that everything inside them is annotated.
    ///
    /// Surfaced in the panel because it is the layer's *supervision state*: with
    /// none of these, every pixel nothing covers is unexamined, and a trainer
    /// that read the set as exhaustive would learn that every unmarked object is
    /// background. A count is the cheapest honest way to say which it is.
    pub fn dense_count(&self) -> usize {
        self.annotations.iter().filter(|a| a.dense_region).count()
    }

    /// How many shapes carry a stroke width — how many are scribbles rather
    /// than geometric curves.
    pub fn scribble_count(&self) -> usize {
        self.annotations
            .iter()
            .filter(|a| a.stroke_width.is_some_and(|w| w > 0.0))
            .count()
    }

    /// The annotation with this id.
    pub fn get(&self, id: u64) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    /// How many shapes are drawn right now, of how many there are.
    pub fn visible_count(&self, z: i32, t: i32) -> (usize, usize) {
        (
            self.annotations
                .iter()
                .filter(|item| self.shows(item, z, t))
                .count(),
            self.annotations.len(),
        )
    }

    /// The colour a shape is drawn in.
    ///
    /// The object's own colour, then its class's, then — when colouring by
    /// class — a stable hash of the class name, then the layer's. A hash rather
    /// than a palette walked in order, because two sessions that loaded the same
    /// file in a different order would otherwise disagree about which colour
    /// "vessel" is, and the name is the only thing both have.
    pub fn color_of(&self, item: &Annotation) -> [f32; 3] {
        if let Some([r, g, b]) = item.effective_color() {
            return [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
        }
        self.class_color(&item.label)
    }

    /// The colour a *class* draws in, for the key and for shapes with no colour
    /// of their own.
    pub fn class_color(&self, class: &str) -> [f32; 3] {
        if !self.color_by_class || class.is_empty() {
            return self.style.color;
        }
        super::class_color(class)
    }

    /// The world radius a *class*'s points draw at, or 0 while the layer sizes
    /// its markers in screen pixels.
    ///
    /// The class's own radius, then the layer's default — the same fallback
    /// `class_color` makes, and for the same reason: most layers hold one kind
    /// of thing and should not have to say so once per class.
    pub fn class_radius(&self, class: &str) -> f32 {
        if !self.world_radius {
            return 0.0;
        }
        self.class_radii
            .get(class)
            .copied()
            .unwrap_or(self.radius)
            .max(0.0)
    }

    /// The radius the *next* shape drawn here would get — what the control
    /// shows, and what it edits.
    pub fn current_radius(&self) -> f32 {
        self.class_radii
            .get(&self.class)
            .copied()
            .unwrap_or(self.radius)
    }

    /// Set the radius for the class new shapes get, or the layer's default when
    /// no class is named.
    ///
    /// One control, two destinations, decided by the class box beside it: with
    /// a particle type named it is that type's box size, with none it is what
    /// every unnamed class falls back to.
    pub fn set_radius(&mut self, radius: f32) {
        if self.class.is_empty() {
            self.radius = radius;
        } else {
            self.class_radii.insert(self.class.clone(), radius);
        }
    }

    /// The shapes to draw, grouped into one batch per colour *and radius*.
    ///
    /// One draw call per colour rather than a per-vertex colour attribute: the
    /// colour count is what a person typed, and the point program is shared with
    /// object layers, which supply no such attribute. The radius joins the key
    /// for the same reason — it is a uniform, not an attribute — and costs
    /// nothing while a layer has one radius, which is the usual case.
    pub fn batches(&self, z: i32, t: i32) -> Vec<AnnotBatch> {
        let mut batches: Vec<AnnotBatch> = Vec::new();
        for item in &self.annotations {
            if !self.shows(item, z, t) {
                continue;
            }
            let color = self.color_of(item);
            let radius = self.class_radius(&item.label);
            let batch = match batches
                .iter_mut()
                .find(|b| b.color == color && b.radius == radius)
            {
                Some(batch) => batch,
                None => {
                    batches.push(AnnotBatch {
                        color,
                        radius,
                        points: Vec::new(),
                        markers: Vec::new(),
                        lines: Vec::new(),
                        fills: Vec::new(),
                    });
                    batches.last_mut().expect("just pushed")
                }
            };
            batch.push(item, self.selected == Some(item.id), self.filled);
        }
        batches
    }
}

/// One colour's worth of an annotation layer, ready to upload.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AnnotBatch {
    pub color: [f32; 3],
    /// The world radius these points draw at, or 0 for a screen-space marker.
    pub radius: f32,
    /// Interleaved `(z, y, x, value, row)`, for the point program.
    pub points: Vec<f32>,
    /// The same points as `(x, y, z, selected)`, kept on the CPU.
    ///
    /// Only filled when `radius > 0`. It is what a circle is built from on the
    /// frames where the radius has outgrown the device's point-sprite cap and
    /// the ring has to be drawn as real geometry instead — which needs the
    /// positions, and the GPU buffer above cannot be read back.
    pub markers: Vec<[f32; 4]>,
    /// Interleaved `(x, y, z0, z1, selected)`, for the line program.
    pub lines: Vec<f32>,
    /// Interleaved `(x, y, z0, z1)`, three vertices per triangle.
    pub fills: Vec<f32>,
}

impl AnnotBatch {
    fn push(&mut self, item: &Annotation, selected: bool, filled: bool) {
        let (z0, z1) = item.z_range();
        let (z0, z1) = (z0 as f32, z1 as f32);
        let flag = f32::from(selected);

        // Point geometries become sprites; `row` carries the annotation *id* so
        // the selection highlight survives a deletion further up the list.
        for [x, y] in item.geometry.markers() {
            self.points
                .extend_from_slice(&[z0, y as f32, x as f32, 0.0, item.id as f32]);
            if self.radius > 0.0 {
                self.markers.push([x as f32, y as f32, z0, flag]);
            }
        }
        // Every ring and line becomes segments. `GL_LINE_LOOP` would need one
        // draw call per shape, which is the whole reason a colour is one buffer.
        // A cell's nucleus is a second outline inside the first — QuPath draws
        // both, and a cell drawn with only its membrane is half a cell.
        let outlines = item
            .geometry
            .outlines()
            .into_iter()
            .chain(item.nucleus.iter().flat_map(|n| n.outlines()));
        for path in outlines {
            for pair in path.windows(2) {
                for point in pair {
                    self.lines
                        .extend_from_slice(&[point[0] as f32, point[1] as f32, z0, z1, flag]);
                }
            }
        }
        // A stroke is a claim about *pixels*, so it is drawn as the band it
        // covers rather than as a wider line: `lineWidth` above 1 is not
        // portable in WebGL and is screen-space where it works at all, and a
        // scribble whose apparent width changed with the zoom would be showing
        // an assertion nobody made. The band is real geometry in world
        // coordinates — the same answer `draw_pick_circles` reaches for a
        // radius too big to be a point sprite — with the round caps and joins
        // the group attributes declare the rasteriser will use.
        //
        // The centre line is stroked as well, above: at a zoom where the band
        // is under a pixel wide it would otherwise vanish, and a scribble that
        // disappears reads as a scribble that was never drawn.
        if let Some(half) = item.stroke_width.filter(|w| *w > 0.0).map(|w| w / 2.0) {
            for path in item.geometry.outlines() {
                for triangle in stroke_band(&path, half) {
                    for [x, y] in triangle {
                        self.fills.extend_from_slice(&[x as f32, y as f32, z0, z1]);
                    }
                }
            }
        }
        // A dense region means something an ordinary shape does not — inside
        // it, a pixel nothing covers is *background* rather than unexamined —
        // so it cannot look like an ordinary shape. Hatched, which reads at a
        // glance, survives the fill being off, and needs no second program:
        // the strokes go in the line buffer with everything else.
        if item.dense_region {
            for [a, b] in hatch(&item.geometry) {
                for point in [a, b] {
                    self.lines
                        .extend_from_slice(&[point[0] as f32, point[1] as f32, z0, z1, flag]);
                }
            }
        }
        if filled {
            for triangle in triangulate(&item.geometry) {
                for [x, y] in triangle {
                    self.fills.extend_from_slice(&[x as f32, y as f32, z0, z1]);
                }
            }
        }
    }
}

/// Segments a full turn of a round cap or join is drawn with.
///
/// Twelve is enough that a cap reads as round at the zooms a stroke is judged
/// at, and cheap enough that a freehand trace of a few hundred vertices is
/// still a few thousand triangles. A cap or a join takes only its own share of
/// this, so a gentle bend costs one triangle.
const CAP_SEGMENTS: usize = 12;

/// A wedge of a disc, as triangles: `sweep` radians from `from`, either way.
fn fan(centre: Point, radius: f64, from: f64, sweep: f64, out: &mut Vec<[Point; 3]>) {
    let per = std::f64::consts::TAU / CAP_SEGMENTS as f64;
    let steps = ((sweep.abs() / per).ceil() as usize).max(1);
    let step = sweep / steps as f64;
    let at = |angle: f64| {
        [
            centre[0] + radius * angle.cos(),
            centre[1] + radius * angle.sin(),
        ]
    };
    for index in 0..steps {
        out.push([
            centre,
            at(from + step * index as f64),
            at(from + step * (index + 1) as f64),
        ]);
    }
}

/// The band a stroke of half-width `half` covers, as triangles in world pixels.
///
/// A quad per segment, a round cap at each end and a round wedge on the
/// *outside* of every bend — which is the rasterisation rule the group
/// attributes declare, so the picture and whatever eventually rasterises this
/// agree about which pixels the scribble claims.
///
/// The wedge is only on the outer side, and the caps are half discs rather than
/// whole ones, because these triangles are drawn translucent in one pass: a
/// disc at every vertex would overlap the quads either side of it and blend
/// twice, beading the path at exactly the joins that are supposed to be
/// invisible.
pub(crate) fn stroke_band(path: &[Point], half: f64) -> Vec<[Point; 3]> {
    let mut out = Vec::new();
    if half <= 0.0 {
        return out;
    }
    // A repeated vertex has no direction, and one in the middle of a path would
    // leave the joins either side of it without one either.
    let mut points: Vec<Point> = Vec::with_capacity(path.len());
    for point in path {
        if points.last().is_none_or(|last: &Point| {
            (point[0] - last[0]).hypot(point[1] - last[1]) > f64::EPSILON
        }) {
            points.push(*point);
        }
    }
    let Some(first) = points.first().copied() else {
        return out;
    };
    if points.len() == 1 {
        // What a stroke of no length covers is the disc under its cap, which is
        // the right answer rather than an empty one.
        fan(first, half, 0.0, std::f64::consts::TAU, &mut out);
        return out;
    }

    let heading = |a: Point, b: Point| (b[1] - a[1]).atan2(b[0] - a[0]);
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let angle = heading(a, b);
        let (nx, ny) = (-angle.sin() * half, angle.cos() * half);
        let corners = [
            [a[0] + nx, a[1] + ny],
            [b[0] + nx, b[1] + ny],
            [b[0] - nx, b[1] - ny],
            [a[0] - nx, a[1] - ny],
        ];
        out.push([corners[0], corners[1], corners[2]]);
        out.push([corners[0], corners[2], corners[3]]);
    }

    // The caps: half a disc each, facing away from the path.
    let half_turn = std::f64::consts::PI;
    let start = heading(points[0], points[1]);
    fan(first, half, start + half_turn / 2.0, half_turn, &mut out);
    let last = points[points.len() - 1];
    let end = heading(points[points.len() - 2], last);
    fan(last, half, end - half_turn / 2.0, half_turn, &mut out);

    // The joins: the wedge the bend opens on its outer side. The inner side
    // needs nothing — the two quads already overlap there.
    for index in 1..points.len() - 1 {
        let incoming = heading(points[index - 1], points[index]);
        let outgoing = heading(points[index], points[index + 1]);
        // Which way the path turns decides which side the gap is on.
        let turn = wrap(outgoing - incoming);
        if turn.abs() <= f64::EPSILON {
            continue;
        }
        let side = if turn > 0.0 {
            -half_turn / 2.0
        } else {
            half_turn / 2.0
        };
        fan(points[index], half, incoming + side, turn, &mut out);
    }
    out
}

/// An angle folded into `(-pi, pi]`, so a turn is the short way round.
fn wrap(angle: f64) -> f64 {
    let turn = std::f64::consts::TAU;
    let wrapped = angle % turn;
    if wrapped > std::f64::consts::PI {
        wrapped - turn
    } else if wrapped <= -std::f64::consts::PI {
        wrapped + turn
    } else {
        wrapped
    }
}

/// How many hatch lines cross a dense region, at its widest.
///
/// A count rather than a spacing, so the hatch is the same texture on a
/// thumbnail-sized crop and on a region covering the slide — and so a shape can
/// never cost more than this many lines per triangle.
const HATCH_LINES: usize = 10;

/// Diagonal hatch across the inside of a shape, in world pixels.
///
/// Clipped against the same triangulation the fill uses, so holes are not
/// hatched and a multi-part region is hatched in every part. The lines are
/// `x + y = k`, which is a 45° hatch, and `k` is shared across the whole shape
/// so the pieces line up rather than each triangle carrying its own phase.
///
/// Empty for anything with no area: "everything inside me is annotated" is a
/// statement only a region can make.
fn hatch(geometry: &Geometry) -> Vec<[Point; 2]> {
    let triangles = triangulate(geometry);
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for triangle in &triangles {
        for point in triangle {
            lo = lo.min(point[0] + point[1]);
            hi = hi.max(point[0] + point[1]);
        }
    }
    let span = hi - lo;
    if !span.is_finite() || span <= 0.0 {
        return Vec::new();
    }
    let step = span / (HATCH_LINES + 1) as f64;
    let mut out = Vec::new();
    for triangle in &triangles {
        let u = [
            triangle[0][0] + triangle[0][1],
            triangle[1][0] + triangle[1][1],
            triangle[2][0] + triangle[2][1],
        ];
        let first = (((u[0].min(u[1]).min(u[2]) - lo) / step).ceil() as i64).max(1);
        let last =
            (((u[0].max(u[1]).max(u[2]) - lo) / step).floor() as i64).min(HATCH_LINES as i64);
        for line in first..=last {
            let value = lo + line as f64 * step;
            // A line meets a triangle in at most two edges, and the segment
            // between those two crossings is the part of it that is inside.
            let mut hits: Vec<Point> = Vec::new();
            for edge in 0..3 {
                let (p, q) = (triangle[edge], triangle[(edge + 1) % 3]);
                let (up, uq) = (u[edge], u[(edge + 1) % 3]);
                if (up < value) == (uq < value) {
                    continue;
                }
                let fraction = (value - up) / (uq - up);
                hits.push([
                    p[0] + (q[0] - p[0]) * fraction,
                    p[1] + (q[1] - p[1]) * fraction,
                ]);
            }
            if let [a, b] = hits[..] {
                out.push([a, b]);
            }
        }
    }
    out
}

/// Ear-clip every polygon in a geometry into triangles, holes included.
///
/// `earcut` wants one flat coordinate list plus the index each hole starts at,
/// which is exactly how a GeoJSON polygon's rings are already laid out — the
/// exterior first, then the holes.
fn triangulate(geometry: &Geometry) -> Vec<[[f64; 2]; 3]> {
    let mut out = Vec::new();
    let mut earcut = earcut::Earcut::new();
    for rings in geometry.polygons() {
        let mut flat: Vec<[f64; 2]> = Vec::new();
        let mut holes: Vec<u32> = Vec::new();
        for (index, ring) in rings.iter().enumerate() {
            // A ring arrives closed from some writers and open from others;
            // earcut wants it open, and a repeated vertex makes a zero-area ear.
            let mut ring = ring.clone();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() < 3 {
                continue;
            }
            if index > 0 {
                holes.push(flat.len() as u32);
            }
            flat.extend(ring);
        }
        if flat.len() < 3 {
            continue;
        }
        let mut indices: Vec<u32> = Vec::new();
        earcut.earcut(flat.iter().copied(), &holes, &mut indices);
        for triangle in indices.as_chunks::<3>().0 {
            let corner = |i: u32| flat.get(i as usize).copied().unwrap_or([0.0, 0.0]);
            out.push([
                corner(triangle[0]),
                corner(triangle[1]),
                corner(triangle[2]),
            ]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use omezarr_viewer_common::Plane;

    fn ring(x: f64, y: f64, size: f64) -> Vec<[f64; 2]> {
        vec![
            [x, y],
            [x + size, y],
            [x + size, y + size],
            [x, y + size],
            [x, y],
        ]
    }

    fn state(annotations: Vec<Annotation>) -> AnnotUiState {
        AnnotUiState::new(annotations, None)
    }

    fn shape(id: u64, geometry: Geometry, label: &str) -> Annotation {
        Annotation {
            id,
            geometry,
            label: label.to_string(),
            ..Default::default()
        }
    }

    // -- triangulation -------------------------------------------------------

    #[test]
    fn a_square_triangulates_into_two_triangles() {
        let triangles = triangulate(&Geometry::Polygon(vec![ring(0.0, 0.0, 10.0)]));
        assert_eq!(triangles.len(), 2);
        // Together they cover the square exactly: 100 square units.
        let area: f64 = triangles.iter().map(triangle_area).sum();
        assert!((area - 100.0).abs() < 1e-9, "{area}");
    }

    #[test]
    fn a_hole_is_cut_out_of_the_fill() {
        // The whole reason fills go through earcut rather than a fan: a hole
        // that is filled over is a hole nobody can see.
        let with_hole = Geometry::Polygon(vec![ring(0.0, 0.0, 10.0), ring(4.0, 4.0, 2.0)]);
        let area: f64 = triangulate(&with_hole).iter().map(triangle_area).sum();
        assert!((area - (100.0 - 4.0)).abs() < 1e-9, "{area}");
    }

    #[test]
    fn a_multipolygon_fills_every_part() {
        let two = Geometry::MultiPolygon(vec![
            vec![ring(0.0, 0.0, 10.0)],
            vec![ring(50.0, 50.0, 4.0)],
        ]);
        let area: f64 = triangulate(&two).iter().map(triangle_area).sum();
        assert!((area - (100.0 + 16.0)).abs() < 1e-9, "{area}");
    }

    #[test]
    fn nothing_that_has_no_inside_produces_triangles() {
        assert!(triangulate(&Geometry::Point([0.0, 0.0])).is_empty());
        assert!(triangulate(&Geometry::LineString(vec![[0.0, 0.0], [1.0, 1.0]])).is_empty());
        // A ring of two points has no area either, and must not panic.
        assert!(triangulate(&Geometry::Polygon(vec![vec![[0.0, 0.0], [1.0, 1.0]]])).is_empty());
        assert!(triangulate(&Geometry::Polygon(vec![])).is_empty());
    }

    #[test]
    fn an_open_ring_fills_the_same_as_a_closed_one() {
        // Some writers close a ring and some do not; a fill that depended on
        // which would be right half the time.
        let closed = triangulate(&Geometry::Polygon(vec![ring(0.0, 0.0, 10.0)]));
        let mut open = ring(0.0, 0.0, 10.0);
        open.pop();
        let open = triangulate(&Geometry::Polygon(vec![open]));
        let a: f64 = closed.iter().map(triangle_area).sum();
        let b: f64 = open.iter().map(triangle_area).sum();
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    fn triangle_area(t: &[[f64; 2]; 3]) -> f64 {
        ((t[1][0] - t[0][0]) * (t[2][1] - t[0][1]) - (t[2][0] - t[0][0]) * (t[1][1] - t[0][1]))
            .abs()
            / 2.0
    }

    // -- batching ------------------------------------------------------------

    #[test]
    fn one_batch_per_colour_and_each_primitive_in_its_own_buffer() {
        let mut ui = state(vec![
            shape(1, Geometry::Point([1.0, 1.0]), "cell"),
            shape(2, Geometry::Polygon(vec![ring(0.0, 0.0, 10.0)]), "cell"),
            shape(
                3,
                Geometry::LineString(vec![[0.0, 0.0], [5.0, 5.0]]),
                "cell",
            ),
        ]);
        let batches = ui.batches(0, 0);
        assert_eq!(batches.len(), 1, "one colour until classes colour them");
        let batch = &batches[0];
        assert_eq!(batch.points.len(), 5, "one point, five floats");
        assert!(!batch.lines.is_empty(), "the ring and the line are stroked");
        assert!(batch.fills.is_empty(), "fill is off by default");

        ui.filled = true;
        assert!(!ui.batches(0, 0)[0].fills.is_empty(), "and on when asked");
    }

    #[test]
    fn colouring_by_class_splits_the_batches() {
        let mut ui = state(vec![
            shape(1, Geometry::Point([1.0, 1.0]), "cell"),
            shape(2, Geometry::Point([2.0, 2.0]), "vessel"),
        ]);
        assert_eq!(ui.batches(0, 0).len(), 1);
        ui.color_by_class = true;
        assert_eq!(ui.batches(0, 0).len(), 2, "one draw call per colour");
    }

    #[test]
    fn a_filtered_class_contributes_nothing() {
        let mut ui = state(vec![
            shape(1, Geometry::Point([1.0, 1.0]), "cell"),
            shape(2, Geometry::Point([2.0, 2.0]), "vessel"),
        ]);
        ui.filter = Some("cell".into());
        let batches = ui.batches(0, 0);
        assert_eq!(batches.iter().map(|b| b.points.len()).sum::<usize>(), 5);
        assert_eq!(
            ui.visible_count(0, 0),
            (1, 2),
            "drawn, of how many there are"
        );
    }

    #[test]
    fn a_shape_on_another_plane_is_not_drawn() {
        let mut deep = shape(1, Geometry::Point([1.0, 1.0]), "");
        deep.plane = Plane::at(5, 0);
        deep.z_extent = 2;
        let ui = state(vec![deep]);
        assert_eq!(ui.visible_count(4, 0), (0, 1), "before the span");
        assert_eq!(ui.visible_count(5, 0), (1, 1));
        assert_eq!(ui.visible_count(7, 0), (1, 1), "the far end is inclusive");
        assert_eq!(ui.visible_count(8, 0), (0, 1), "past it");
        assert_eq!(
            ui.visible_count(5, 1),
            (0, 1),
            "another frame is another picture"
        );
    }

    #[test]
    fn a_cells_nucleus_is_stroked_as_well_as_its_membrane() {
        // A cell drawn with only its membrane is half a cell.
        let plain = shape(1, Geometry::Polygon(vec![ring(0.0, 0.0, 10.0)]), "");
        let mut celled = plain.clone();
        celled.nucleus = Some(Geometry::Polygon(vec![ring(3.0, 3.0, 3.0)]));
        let without = state(vec![plain]).batches(0, 0)[0].lines.len();
        let with = state(vec![celled]).batches(0, 0)[0].lines.len();
        assert!(with > without, "{without} -> {with}");
    }

    // -- stroke width and dense regions --------------------------------------

    #[test]
    fn a_geometric_line_covers_no_pixels_and_a_scribble_covers_a_band() {
        // The whole distinction: `None` is a curve of no area, `Some(w)` is an
        // assertion about the pixels within w/2 of it. A width of zero is not
        // how "no width" is said — it is refused, so the two cannot collide.
        let path = Geometry::LineString(vec![[0.0, 0.0], [10.0, 0.0]]);
        let plain = shape(1, path.clone(), "");
        assert!(state(vec![plain.clone()]).batches(0, 0)[0].fills.is_empty());

        let mut scribble = plain.clone();
        scribble.stroke_width = Some(4.0);
        let batches = state(vec![scribble]).batches(0, 0);
        let batch = &batches[0];
        assert!(!batch.fills.is_empty(), "a band of covered pixels");
        // And the centre line is still stroked, so a scribble narrower than a
        // screen pixel is still visible.
        assert_eq!(
            batch.lines.len(),
            state(vec![plain.clone()]).batches(0, 0)[0].lines.len()
        );

        let mut zero = plain;
        zero.stroke_width = Some(0.0);
        assert!(
            state(vec![zero]).batches(0, 0)[0].fills.is_empty(),
            "zero is not a width"
        );
    }

    #[test]
    fn a_strokes_band_is_the_width_it_says_and_in_world_pixels() {
        // A 10-long segment at half-width 2 covers a 10x4 rectangle plus the
        // two end caps, which together are one disc of radius 2. Every number
        // here is a world coordinate: the band is world geometry, so it grows
        // with the zoom instead of being a screen-space line width.
        let band = stroke_band(&[[0.0, 0.0], [10.0, 0.0]], 2.0);
        let area: f64 = band
            .iter()
            .map(|t| triangle_area(&[t[0], t[1], t[2]]))
            .sum();
        let expected = 10.0 * 4.0 + std::f64::consts::PI * 4.0;
        // The caps are polygonal, so they fall a couple of percent short of a
        // circle; anything further off is a band of the wrong width. The sum is
        // the covered area only because the pieces do not overlap, which is
        // what keeps the join from blending twice.
        assert!((area - expected).abs() / expected < 0.03, "{area}");
        // And it reaches exactly half a width either side, no further.
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for triangle in &band {
            for point in triangle {
                for axis in 0..2 {
                    lo[axis] = lo[axis].min(point[axis]);
                    hi[axis] = hi[axis].max(point[axis]);
                }
            }
        }
        assert!(
            (lo[1] + 2.0).abs() < 1e-9 && (hi[1] - 2.0).abs() < 1e-9,
            "{lo:?} {hi:?}"
        );
        assert!(
            (lo[0] + 2.0).abs() < 1e-9 && (hi[0] - 12.0).abs() < 1e-9,
            "{lo:?} {hi:?}"
        );
    }

    #[test]
    fn a_bend_is_filled_once_rather_than_twice() {
        // Translucent triangles drawn in one pass blend where they overlap, so
        // a disc at every vertex would bead the path at its joins. The wedge
        // goes on the outside of the turn, where there is a gap, and the area
        // stays close to the band's true area rather than exceeding it.
        let corner = stroke_band(&[[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]], 1.0);
        let area: f64 = corner
            .iter()
            .map(|t| triangle_area(&[t[0], t[1], t[2]]))
            .sum();
        // Two 10x2 arms, a quarter-disc outside the corner, the square inside
        // it counted twice by the two arms, and the two caps.
        let expected = 40.0 + std::f64::consts::PI * 0.25 + 1.0 + std::f64::consts::PI * 0.5;
        assert!((area - expected).abs() / expected < 0.05, "{area}");
    }

    #[test]
    fn a_single_point_path_strokes_as_a_disc_rather_than_as_nothing() {
        assert!(!stroke_band(&[[3.0, 3.0]], 1.5).is_empty());
        assert!(stroke_band(&[[3.0, 3.0]], 0.0).is_empty());
    }

    #[test]
    fn a_dense_region_is_drawn_differently_from_an_ordinary_one() {
        // Its meaning is completely different — inside it, unlabelled means
        // background — so it cannot be told apart only by reading the list.
        let plain = shape(1, Geometry::Polygon(vec![ring(0.0, 0.0, 100.0)]), "");
        let mut dense = plain.clone();
        dense.dense_region = true;
        let ordinary = state(vec![plain]).batches(0, 0)[0].lines.len();
        let hatched = state(vec![dense]).batches(0, 0)[0].lines.len();
        assert!(hatched > ordinary, "{ordinary} -> {hatched}");
    }

    #[test]
    fn hatching_stays_inside_the_shape_and_out_of_its_holes() {
        let square = Geometry::Polygon(vec![ring(0.0, 0.0, 100.0)]);
        let lines = hatch(&square);
        assert!(!lines.is_empty());
        for [a, b] in &lines {
            for p in [a, b] {
                assert!((-1e-6..=100.0 + 1e-6).contains(&p[0]), "{p:?}");
                assert!((-1e-6..=100.0 + 1e-6).contains(&p[1]), "{p:?}");
            }
        }
        // A hole is not part of the region, so no line crosses it.
        let holed = Geometry::Polygon(vec![ring(0.0, 0.0, 100.0), ring(30.0, 30.0, 40.0)]);
        let total = |lines: &[[Point; 2]]| -> f64 {
            lines
                .iter()
                .map(|[a, b]| (b[0] - a[0]).hypot(b[1] - a[1]))
                .sum()
        };
        assert!(total(&hatch(&holed)) < total(&lines));
    }

    #[test]
    fn nothing_with_an_inside_to_claim_hatches_nothing() {
        // "Everything inside me is annotated" is a statement only a region can
        // make; a line marked dense simply has no interior to hatch.
        assert!(hatch(&Geometry::LineString(vec![[0.0, 0.0], [5.0, 5.0]])).is_empty());
        assert!(hatch(&Geometry::Point([1.0, 1.0])).is_empty());
    }

    #[test]
    fn the_panel_can_say_what_the_layer_asserts() {
        let mut dense = shape(1, Geometry::Polygon(vec![ring(0.0, 0.0, 10.0)]), "");
        dense.dense_region = true;
        let mut scribble = shape(2, Geometry::LineString(vec![[0.0, 0.0], [5.0, 0.0]]), "");
        scribble.stroke_width = Some(6.0);
        let plain = shape(3, Geometry::Point([1.0, 1.0]), "");
        let ui = state(vec![dense, scribble, plain]);
        assert_eq!(ui.dense_count(), 1);
        assert_eq!(ui.scribble_count(), 1);
    }

    #[test]
    fn a_reload_keeps_the_stroke_width_the_panel_was_set_to() {
        let mut old = state(vec![]);
        old.stroke_width = Some(11.0);
        let mut fresh = state(vec![]);
        fresh.keep_view_of(&old);
        assert_eq!(fresh.stroke_width, Some(11.0));
    }

    // -- colour --------------------------------------------------------------

    #[test]
    fn a_class_colour_does_not_depend_on_the_order_classes_were_seen() {
        // A hash rather than a palette walked in order: two sessions that
        // loaded the same file differently would otherwise disagree about which
        // colour "vessel" is.
        let mut first = state(vec![
            shape(1, Geometry::Point([0.0, 0.0]), "cell"),
            shape(2, Geometry::Point([0.0, 0.0]), "vessel"),
        ]);
        let mut second = state(vec![
            shape(1, Geometry::Point([0.0, 0.0]), "vessel"),
            shape(2, Geometry::Point([0.0, 0.0]), "cell"),
        ]);
        first.color_by_class = true;
        second.color_by_class = true;
        assert_eq!(first.class_color("vessel"), second.class_color("vessel"));
        assert_ne!(first.class_color("vessel"), first.class_color("cell"));
        // The unclassified take the layer's own colour, whatever it is.
        assert_eq!(first.class_color(""), first.style.color);
    }

    // -- radius --------------------------------------------------------------

    #[test]
    fn a_screen_space_layer_has_no_radius_at_all() {
        // The old behaviour is the default: a point is a marker until somebody
        // says it is a pick.
        let ui = state(vec![shape(1, Geometry::Point([1.0, 1.0]), "cell")]);
        assert_eq!(ui.class_radius("cell"), 0.0);
        let batches = ui.batches(0, 0);
        assert_eq!(batches[0].radius, 0.0);
        assert!(
            batches[0].markers.is_empty(),
            "nothing to build a ring from"
        );
    }

    #[test]
    fn a_class_takes_its_own_radius_and_otherwise_the_layers() {
        let mut ui = state(vec![
            shape(1, Geometry::Point([1.0, 1.0]), "ribosome"),
            shape(2, Geometry::Point([2.0, 2.0]), "proteasome"),
        ]);
        ui.world_radius = true;
        ui.radius = 20.0;
        ui.class_radii.insert("ribosome".into(), 75.0);
        assert_eq!(ui.class_radius("ribosome"), 75.0);
        assert_eq!(ui.class_radius("proteasome"), 20.0, "the layer's default");
        assert_eq!(ui.class_radius(""), 20.0);
    }

    #[test]
    fn two_radii_are_two_batches_even_in_one_colour() {
        // The radius is a uniform, not a vertex attribute, so it splits the
        // draw the same way the colour does.
        let mut ui = state(vec![
            shape(1, Geometry::Point([1.0, 1.0]), "ribosome"),
            shape(2, Geometry::Point([2.0, 2.0]), "proteasome"),
        ]);
        ui.world_radius = true;
        assert_eq!(ui.batches(0, 0).len(), 1, "one radius, one batch");
        ui.class_radii.insert("ribosome".into(), 75.0);
        let batches = ui.batches(0, 0);
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().any(|b| b.radius == 75.0));
        assert!(batches.iter().all(|b| b.color == batches[0].color));
    }

    #[test]
    fn a_pick_keeps_its_position_on_the_cpu_for_the_geometry_path() {
        // The circle drawn past the point-sprite cap is built from these; the
        // GPU buffer beside them cannot be read back.
        let mut ui = state(vec![shape(1, Geometry::Point([3.0, 4.0]), "cell")]);
        ui.world_radius = true;
        let batches = ui.batches(0, 0);
        assert_eq!(batches[0].markers, vec![[3.0, 4.0, 0.0, 0.0]]);
        ui.selected = Some(1);
        assert_eq!(ui.batches(0, 0)[0].markers[0][3], 1.0, "selected travels");
    }

    #[test]
    fn the_radius_control_edits_the_class_in_the_box_or_the_default() {
        let mut ui = state(vec![]);
        ui.world_radius = true;
        ui.set_radius(30.0);
        assert_eq!(ui.radius, 30.0, "no class named: the layer's default");
        assert!(ui.class_radii.is_empty());

        ui.class = "ribosome".into();
        ui.set_radius(75.0);
        assert_eq!(ui.radius, 30.0, "the default is left alone");
        assert_eq!(ui.current_radius(), 75.0);
        assert_eq!(ui.class_radius("proteasome"), 30.0);
    }

    #[test]
    fn a_reload_keeps_the_radii_the_panel_was_set_to() {
        // An annotation layer's rows arrive with the session and its style does
        // not; a radius lost on every reload is a box size retyped every time.
        let mut old = state(vec![]);
        old.world_radius = true;
        old.radius = 30.0;
        old.class_radii.insert("ribosome".into(), 75.0);
        let mut fresh = state(vec![shape(1, Geometry::Point([1.0, 1.0]), "ribosome")]);
        fresh.keep_view_of(&old);
        assert!(fresh.world_radius);
        assert_eq!(fresh.class_radius("ribosome"), 75.0);
        assert_eq!(fresh.class_radius("other"), 30.0);
    }

    #[test]
    fn an_objects_own_colour_beats_its_classs() {
        let mut item = shape(1, Geometry::Point([0.0, 0.0]), "cell");
        item.class_color = Some([10, 20, 30]);
        let mut ui = state(vec![item.clone()]);
        ui.color_by_class = true;
        let by_class = ui.color_of(&item);
        assert!((by_class[0] - 10.0 / 255.0).abs() < 1e-6);

        item.color = Some([200, 100, 50]);
        let own = ui.color_of(&item);
        assert!((own[0] - 200.0 / 255.0).abs() < 1e-6, "{own:?}");
    }
}
