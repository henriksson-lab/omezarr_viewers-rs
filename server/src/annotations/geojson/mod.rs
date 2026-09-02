//! QuPath's GeoJSON dialect, read and written faithfully.
//!
//! This is the viewer's **native** annotation format — the ROI table beside it
//! (`roi_table.rs`) is for interoperating with the ngio/Fractal world, and can
//! only hold axis-aligned boxes. Here the geometry is whatever was drawn.
//!
//! `info_annotation_formats.md` has the analysis; the short version is that
//! OME-Zarr specifies no vector annotation, OME-XML's ROI model cannot express
//! a polygon with a hole, and QuPath's dialect is both a real standard
//! underneath (RFC 7946) and the thing the tool we mean to replace reads.
//!
//! # The dialect, as QuPath writes it
//!
//! A `FeatureCollection` of `Feature`s. Each feature's `geometry` is a plain
//! RFC 7946 geometry with two **foreign members** QuPath adds:
//!
//! * `plane` — `{"c": -1, "z": 3, "t": 0}`, omitted when it is the default.
//! * `isEllipse` — the geometry is a polygonised ellipse; QuPath rebuilds one
//!   from the bounding box on read. Load-bearing, because a polygonised ellipse
//!   is not recoverable from its vertices. A *rectangle* needs no such flag:
//!   QuPath recognises one by inspecting the ring, and so does this reader.
//!
//! and `properties` carries `objectType`, `name`, `color`, `classification`,
//! `isLocked`, `measurements`, `metadata` and nested `childObjects`.
//!
//! # Coordinates
//!
//! Full-resolution pixels, origin top-left, y downwards — which is exactly this
//! viewer's world, so nothing is converted on the way in or out. That is the
//! single biggest practical advantage over the ROI table, whose `*_micrometer`
//! columns are unrecoverable unless the scale used is recorded alongside.
//!
//! # Our one deviation
//!
//! QuPath and OME-XML both give a shape exactly one plane. An annotation here
//! may span a range ([`Annotation::z_extent`]), which is written as
//! `zExtent`/`tExtent` in `properties` and declared in the group attributes.
//! QuPath ignores members it does not know, so a file with these still opens
//! there — as a shape on the first plane of its range, which is the honest
//! degradation.

mod read;
mod store;
mod write;

pub use read::parse;
pub use store::{
    is_annotation_target, list, list_async, load, load_async, load_file, make_target,
    make_uri_target, save, save_async, save_file, split_target, split_uri_target, target_is_remote,
    AnnotationFile,
};
pub use write::write;

/// The `properties` members this viewer adds beyond QuPath's.
const Z_EXTENT: &str = "zExtent";
const T_EXTENT: &str = "tExtent";
/// The width a stroke covers, in world pixels. See `Annotation::stroke_width`
/// for why an absent one is a geometric line rather than a zero-width stroke.
const STROKE_WIDTH: &str = "strokeWidth";
/// Marks a shape as asserting that everything inside it is annotated. See
/// `Annotation::dense_region` for why sparse cannot be the only mode.
const DENSE_REGION: &str = "denseRegion";

/// Sample files the read and write halves both build on.
///
/// Shared rather than copied: `QUPATH` is a real QuPath export, and two
/// divergent copies of it would let one half be tested against a file the other
/// half never sees.
#[cfg(test)]
mod fixtures {
    pub(super) fn square(x: f64, y: f64, size: f64) -> Vec<[f64; 2]> {
        vec![
            [x, y],
            [x + size, y],
            [x + size, y + size],
            [x, y + size],
            [x, y],
        ]
    }

    /// A feature collection as QuPath 0.4+ actually writes one.
    pub(super) const QUPATH: &str = r#"{
          "type": "FeatureCollection",
          "features": [
            {
              "type": "Feature",
              "id": "e3b0c442-98fc-1c14-9afb-4c8996fb9242",
              "geometry": {
                "type": "Polygon",
                "coordinates": [
                  [[100,200],[300,200],[300,400],[100,400],[100,200]],
                  [[150,250],[200,250],[200,300],[150,300],[150,250]]
                ],
                "plane": {"c": -1, "z": 3, "t": 1}
              },
              "properties": {
                "objectType": "annotation",
                "name": "Region 1",
                "color": [255, 0, 0],
                "classification": {"names": ["Tumor", "Positive"], "color": [200, 0, 0]},
                "isLocked": true,
                "measurements": {"Area": 1234.5, "Bad": "NaN"},
                "metadata": {"note": "checked"},
                "childObjects": [
                  {
                    "type": "Feature",
                    "geometry": {"type": "Point", "coordinates": [150, 260]},
                    "nucleusGeometry": {
                      "type": "Polygon",
                      "coordinates": [[[148,258],[152,258],[152,262],[148,262],[148,258]]]
                    },
                    "properties": {"objectType": "cell", "classification": {"name": "Cell"}}
                  }
                ]
              }
            },
            {
              "type": "Feature",
              "geometry": {
                "type": "Polygon",
                "coordinates": [[[0,0],[10,0],[10,10],[0,10],[0,0]]],
                "isEllipse": true
              },
              "properties": {"objectType": "annotation"}
            }
          ]
        }"#;
}

/// The round trip — the contract *between* the two halves, and so the property
/// a split most endangers.
///
/// These live here rather than under `read` or `write` because neither owns
/// them: what they assert is that anything the reader understood survives being
/// written and read again, including the members nothing in this viewer
/// displays. Filed under either half they would read as that half's tests and
/// could be moved or weakened with it.
#[cfg(test)]
mod tests {
    use super::fixtures::{square, QUPATH};
    use super::*;
    use omezarr_viewer_common::{Annotation, Geometry, Plane};
    use serde_json::Value;

    #[test]
    fn a_round_trip_preserves_everything_it_read() {
        let rows = parse(QUPATH.as_bytes()).unwrap();
        let written = write(&rows).unwrap();
        let back = parse(&written).unwrap();
        assert_eq!(back.len(), rows.len());
        for (before, after) in rows.iter().zip(&back) {
            assert_eq!(before.geometry, after.geometry);
            assert_eq!(before.plane, after.plane);
            assert_eq!(before.label, after.label);
            assert_eq!(before.class_color, after.class_color);
            assert_eq!(before.name, after.name);
            assert_eq!(before.color, after.color);
            assert_eq!(before.object_type, after.object_type);
            assert_eq!(before.locked, after.locked);
            assert_eq!(before.is_ellipse, after.is_ellipse);
            assert_eq!(before.measurements, after.measurements);
            assert_eq!(before.metadata, after.metadata);
            assert_eq!(before.nucleus, after.nucleus);
            assert_eq!(before.missing, after.missing);
            assert_eq!(before.uuid, after.uuid);
            assert_eq!(before.parent, after.parent);
        }
    }
    #[test]
    fn a_stroke_carries_its_width_and_a_bare_line_carries_none() {
        // The distinction this pins is the whole point of storing a width: a
        // `LineString` with no width is a mathematical curve covering no pixels,
        // and the same path with a width is a scribble covering a capsule. A
        // reader that collapsed the two would turn "I painted here" into "I drew
        // an infinitely thin line", which asserts nothing about any pixel.
        let scribble = Annotation {
            geometry: Geometry::LineString(vec![[0.0, 0.0], [10.0, 4.0]]),
            stroke_width: Some(11.0),
            ..Default::default()
        };
        let bare = Annotation {
            geometry: Geometry::LineString(vec![[0.0, 0.0], [10.0, 4.0]]),
            ..Default::default()
        };
        let written = write(&[scribble.clone(), bare.clone()]).unwrap();
        let value: Value = serde_json::from_slice(&written).unwrap();
        assert_eq!(value["features"][0]["properties"]["strokeWidth"], 11.0);
        assert!(
            value["features"][1]["properties"]
                .get("strokeWidth")
                .is_none(),
            "a line with no width writes no member, rather than a zero that reads\
             back as a stroke covering nothing"
        );

        let back = parse(&written).unwrap();
        assert_eq!(back[0].stroke_width, Some(11.0));
        assert_eq!(back[1].stroke_width, None);
    }
    #[test]
    fn a_z_range_is_written_as_our_own_member_and_nothing_else_changes() {
        let annotation = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            plane: Plane::at(2, 0),
            z_extent: 5,
            ..Default::default()
        };
        let value: Value = serde_json::from_slice(&write(&[annotation]).unwrap()).unwrap();
        let properties = &value["features"][0]["properties"];
        assert_eq!(properties["zExtent"], 5);
        assert!(properties.get("tExtent").is_none(), "zero is not written");
        // The geometry itself stays plain GeoJSON, so QuPath opens the file and
        // simply ignores the member it does not know.
        assert_eq!(value["features"][0]["geometry"]["type"], "Polygon");
        assert_eq!(value["features"][0]["geometry"]["plane"]["z"], 2);

        let back = parse(
            &write(&[Annotation {
                geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
                plane: Plane::at(2, 0),
                z_extent: 5,
                t_extent: 3,
                ..Default::default()
            }])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back[0].z_extent, 5);
        assert_eq!(back[0].t_extent, 3);
    }
    #[test]
    fn every_geometry_type_survives_a_round_trip() {
        let geometries = [
            Geometry::Point([1.0, 2.0]),
            Geometry::MultiPoint(vec![[1.0, 2.0], [3.0, 4.0]]),
            Geometry::LineString(vec![[0.0, 0.0], [10.0, 10.0]]),
            Geometry::MultiLineString(vec![vec![[0.0, 0.0], [1.0, 1.0]]]),
            Geometry::Polygon(vec![square(0.0, 0.0, 10.0), square(2.0, 2.0, 3.0)]),
            Geometry::MultiPolygon(vec![
                vec![square(0.0, 0.0, 5.0)],
                vec![square(20.0, 20.0, 5.0)],
            ]),
        ];
        let rows: Vec<Annotation> = geometries
            .iter()
            .enumerate()
            .map(|(i, geometry)| Annotation {
                id: i as u64,
                geometry: geometry.clone(),
                ..Default::default()
            })
            .collect();
        let back = parse(&write(&rows).unwrap()).unwrap();
        assert_eq!(back.len(), geometries.len());
        for (expected, actual) in geometries.iter().zip(&back) {
            assert_eq!(*expected, actual.geometry);
        }
    }
}
