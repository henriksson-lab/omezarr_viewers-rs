//! An annotation layer's state, and the buffers it is drawn from.
//!
//! Batching is the point: shapes are grouped by colour so the renderer makes
//! one draw call per colour rather than one per shape, and points, outlines and
//! fills go to separate buffers because each is a different shader program.

use std::collections::HashMap;

use omezarr_viewer_common::{Annotation, Geometry, ObjectType};

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
        if filled {
            for triangle in triangulate(&item.geometry) {
                for [x, y] in triangle {
                    self.fills.extend_from_slice(&[x as f32, y as f32, z0, z1]);
                }
            }
        }
    }
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
