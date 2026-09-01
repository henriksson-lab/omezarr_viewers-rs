//! Annotations as `blockflow` table rows — the form a rasteriser consumes.
//!
//! One row per **vertex**, not per shape, because a table's columns are scalars
//! (`u64` and `f64`) and a shape is a variable-length path. The shape is
//! reassembled from `shape`, `ring` and `vertex`, which is the same
//! decomposition the rasteriser needs anyway: an op is plannable only when the
//! blocks that can affect a given block are known before the run, and that is
//! true of a vertex with a bounded half-width and untrue of a path of unbounded
//! length.
//!
//! # Why vertices and not pixels
//!
//! Resampling a stroke to points here would bake in a resolution. The whole
//! argument for storing geometry is that whoever rasterises does so at the level
//! they train at — a mask rasterised at a downsampled level and scaled up
//! teaches a boundary-regressing model to reproduce the staircase. So this emits
//! what the curator drew, and the resampling belongs to the rasteriser.
//!
//! # The position key is an address, not the value
//!
//! A table is keyed by whole voxels, and that key is what routes a row to a
//! block. Our coordinates are fractional world pixels, so the exact position
//! travels in the `x` and `y` columns and the key is the rounded one. A
//! rasteriser must read the columns; a consumer that reads only the key gets
//! the right block and the wrong sub-voxel position.

use anyhow::Result;
use omezarr_viewer_common::{Annotation, Geometry};

use crate::objects::{table, ColumnData, NamedColumn};

/// What a rasteriser is told about every vertex.
///
/// `half_width` is the one that carries the supervision: a positive one is a
/// stroke covering the pixels within it of the path, and zero is a boundary of a
/// region to be filled. `dense` says how to read the pixels a shape does *not*
/// cover — see [`Annotation::dense_region`].
pub struct Fragments {
    pub positions: Vec<[u64; 3]>,
    pub columns: Vec<NamedColumn>,
    /// Class names, indexed by the `class` column. Names cannot live in a table
    /// — its columns are numbers — so the mapping travels beside it, in the
    /// group attributes.
    pub classes: Vec<String>,
}

impl Fragments {
    pub fn encode(&self) -> Result<Vec<u8>> {
        table::write(&self.positions, &self.columns)
    }
}

/// Flatten annotations into vertex rows.
pub fn fragments(annotations: &[Annotation]) -> Fragments {
    let mut classes: Vec<String> = Vec::new();
    let mut positions = Vec::new();
    let (mut shape, mut ring_of, mut vertex_of) = (Vec::new(), Vec::new(), Vec::new());
    let (mut class, mut closed, mut dense) = (Vec::new(), Vec::new(), Vec::new());
    let (mut xs, mut ys, mut half_width, mut z_extent) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());

    for annotation in annotations {
        let class_id = match classes.iter().position(|c| c == &annotation.label) {
            Some(at) => at,
            None => {
                classes.push(annotation.label.clone());
                classes.len() - 1
            }
        } as u64;
        // Half, because the rasterised set is the pixels within `w / 2` of the
        // path. Stored halved so the rasteriser's reach is the column itself.
        let half = annotation.stroke_width.map(|w| w / 2.0).unwrap_or(0.0);

        for (ring_index, ring, is_closed) in rings(&annotation.geometry) {
            for (vertex_index, point) in ring.iter().enumerate() {
                positions.push([
                    annotation.plane.z.max(0) as u64,
                    point[1].max(0.0).round() as u64,
                    point[0].max(0.0).round() as u64,
                ]);
                shape.push(annotation.id);
                ring_of.push(ring_index);
                vertex_of.push(vertex_index as u64);
                class.push(class_id);
                closed.push(u64::from(is_closed));
                dense.push(u64::from(annotation.dense_region));
                xs.push(point[0]);
                ys.push(point[1]);
                half_width.push(half);
                z_extent.push(u64::from(annotation.z_extent));
            }
        }
    }

    let columns = vec![
        NamedColumn {
            name: "shape".into(),
            data: ColumnData::U64(shape),
        },
        NamedColumn {
            name: "ring".into(),
            data: ColumnData::U64(ring_of),
        },
        NamedColumn {
            name: "vertex".into(),
            data: ColumnData::U64(vertex_of),
        },
        NamedColumn {
            name: "class".into(),
            data: ColumnData::U64(class),
        },
        NamedColumn {
            name: "closed".into(),
            data: ColumnData::U64(closed),
        },
        NamedColumn {
            name: "dense".into(),
            data: ColumnData::U64(dense),
        },
        NamedColumn {
            name: "z_extent".into(),
            data: ColumnData::U64(z_extent),
        },
        NamedColumn {
            name: "x".into(),
            data: ColumnData::F64(xs),
        },
        NamedColumn {
            name: "y".into(),
            data: ColumnData::F64(ys),
        },
        NamedColumn {
            name: "half_width".into(),
            data: ColumnData::F64(half_width),
        },
    ];
    Fragments {
        positions,
        columns,
        classes,
    }
}

/// Every path in a geometry, as `(ring index, points, closed)`.
///
/// Ring 0 of a polygon is its exterior and the rest are holes, which is the one
/// thing that cannot be recovered from the vertices alone — a hole and an
/// exterior are both closed rings — so the index carries it.
fn rings(geometry: &Geometry) -> Vec<(u64, &[[f64; 2]], bool)> {
    match geometry {
        Geometry::Point(point) => vec![(0, std::slice::from_ref(point), false)],
        Geometry::MultiPoint(points) => vec![(0, points.as_slice(), false)],
        Geometry::LineString(points) => vec![(0, points.as_slice(), false)],
        Geometry::MultiLineString(paths) => paths
            .iter()
            .enumerate()
            .map(|(at, path)| (at as u64, path.as_slice(), false))
            .collect(),
        Geometry::Polygon(rings) => rings
            .iter()
            .enumerate()
            .map(|(at, ring)| (at as u64, ring.as_slice(), true))
            .collect(),
        // Each part restarts at ring 0, so a consumer groups by `shape` then by
        // the run of rings between one exterior and the next.
        Geometry::MultiPolygon(parts) => parts
            .iter()
            .flat_map(|part| {
                part.iter()
                    .enumerate()
                    .map(|(at, ring)| (at as u64, ring.as_slice(), true))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omezarr_viewer_common::Plane;

    fn column<'a>(fragments: &'a Fragments, name: &str) -> &'a ColumnData {
        &fragments
            .columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column `{name}`"))
            .data
    }

    fn u64s(fragments: &Fragments, name: &str) -> Vec<u64> {
        match column(fragments, name) {
            ColumnData::U64(values) => values.clone(),
            _ => panic!("`{name}` is not integer"),
        }
    }

    fn f64s(fragments: &Fragments, name: &str) -> Vec<f64> {
        match column(fragments, name) {
            ColumnData::F64(values) => values.clone(),
            _ => panic!("`{name}` is not float"),
        }
    }

    #[test]
    fn a_hole_is_told_from_an_exterior_by_its_ring_index() {
        // The one thing vertices alone cannot say: both are closed rings, and
        // filling a hole is the difference between a doughnut and a disc.
        let outer = vec![
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
            [0.0, 0.0],
        ];
        let hole = vec![[4.0, 4.0], [6.0, 4.0], [6.0, 6.0], [4.0, 6.0], [4.0, 4.0]];
        let doughnut = Annotation {
            id: 7,
            geometry: Geometry::Polygon(vec![outer, hole]),
            ..Default::default()
        };
        let fragments = fragments(&[doughnut]);
        let rings = u64s(&fragments, "ring");
        assert_eq!(&rings[..5], &[0, 0, 0, 0, 0], "the exterior is ring 0");
        assert_eq!(&rings[5..], &[1, 1, 1, 1, 1], "the hole is ring 1");
        assert!(u64s(&fragments, "closed").iter().all(|&c| c == 1));
        assert!(u64s(&fragments, "shape").iter().all(|&s| s == 7));
    }

    #[test]
    fn a_stroke_carries_half_its_width_and_a_region_carries_none() {
        // Halved here so the rasteriser's reach is the column itself rather than
        // a division it has to remember to do.
        let scribble = Annotation {
            geometry: Geometry::LineString(vec![[0.0, 0.0], [9.0, 0.0]]),
            stroke_width: Some(11.0),
            ..Default::default()
        };
        let region = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 4.0, 4.0),
            ..Default::default()
        };
        let fragments = fragments(&[scribble, region]);
        let widths = f64s(&fragments, "half_width");
        assert_eq!(widths[0], 5.5);
        assert_eq!(*widths.last().unwrap(), 0.0, "a boundary is not a stroke");
        assert_eq!(u64s(&fragments, "closed")[0], 0, "a scribble is open");
    }

    #[test]
    fn the_exact_coordinate_survives_and_the_key_is_only_an_address() {
        // The position key is whole voxels because that is what routes a row to
        // a block. Truncating the geometry to it would move every vertex by up
        // to half a voxel, which is exactly the silent drift this project has
        // already been bitten by once.
        let annotation = Annotation {
            geometry: Geometry::LineString(vec![[379.59344482421875, 12.25]]),
            plane: Plane::at(3, 0),
            ..Default::default()
        };
        let fragments = fragments(&[annotation]);
        assert_eq!(fragments.positions[0], [3, 12, 380], "rounded, for routing");
        assert_eq!(
            f64s(&fragments, "x")[0],
            379.59344482421875,
            "exact, for drawing"
        );
        assert_eq!(f64s(&fragments, "y")[0], 12.25);
    }

    #[test]
    fn classes_become_indices_with_the_names_carried_alongside() {
        let of = |label: &str| Annotation {
            geometry: Geometry::Point([0.0, 0.0]),
            label: label.to_string(),
            ..Default::default()
        };
        let fragments = fragments(&[of("cell"), of("vessel"), of("cell")]);
        assert_eq!(u64s(&fragments, "class"), vec![0, 1, 0]);
        assert_eq!(fragments.classes, vec!["cell", "vessel"]);
    }

    #[test]
    fn a_dense_region_says_so_on_every_one_of_its_vertices() {
        let dense = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            dense_region: true,
            ..Default::default()
        };
        let fragments = fragments(&[dense]);
        assert!(u64s(&fragments, "dense").iter().all(|&d| d == 1));
    }

    #[test]
    fn the_rows_survive_the_blob() {
        let fragments = fragments(&[Annotation {
            geometry: Geometry::rect(1.5, 2.5, 3.5, 4.5),
            stroke_width: Some(3.0),
            ..Default::default()
        }]);
        let back = crate::objects::table::read(&fragments.encode().unwrap()).unwrap();
        assert_eq!(back.len(), fragments.positions.len());
        assert_eq!(back.columns().len(), fragments.columns.len());
    }
}
