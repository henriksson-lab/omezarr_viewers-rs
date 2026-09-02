//! The three object readers, against one set of rows.
//!
//! `synthetic::write_objects` writes the *same* blobs as a CSV, a structured
//! `.npy` and a `blockflow` table blob. The claim these tests make is that the
//! three come back describing the same objects — which is the property the
//! viewer relies on and the one a reader bug breaks first.

use omezarr_viewer_server::objects::{self, ObjectQuery, ObjectSpace};
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::synthetic;

fn written() -> (tempfile::TempDir, Vec<synthetic::Blob>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let blobs = synthetic::blobs((8, 512, 512), 6);
    synthetic::write_objects(dir.path(), &blobs).expect("write objects");
    (dir, blobs)
}

#[actix_web::test]
async fn every_reader_finds_the_same_objects() {
    let (dir, blobs) = written();
    let registry = SourceRegistry::new();

    for name in ["cells.csv", "cells.npy", "cells.blob"] {
        let spec = SourceSpec::File(dir.path().join(name));
        let store = objects::open(&registry, &spec)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));

        assert_eq!(store.len(), blobs.len(), "{name} row count");

        // The second blob, by arithmetic rather than by another read.
        let blob = blobs[1];
        let row = store
            .nearest(blob.z as f32, blob.y as f32, blob.x as f32, 2.0)
            .unwrap_or_else(|| panic!("{name} has no row at blob 2"));
        let position = store.world_position(row).expect("position");
        assert!(
            (position[2] - blob.x as f32).abs() <= 1.0
                && (position[1] - blob.y as f32).abs() <= 1.0
                && (position[0] - blob.z as f32).abs() <= 1.0,
            "{name} puts blob 2 at {position:?}, not {:?}",
            [blob.z, blob.y, blob.x]
        );

        // Every reader carries a size-like column, whatever it is called.
        let size = store
            .columns()
            .iter()
            .find(|column| matches!(column.name.as_str(), "size" | "count"))
            .unwrap_or_else(|| panic!("{name} carries no size column"));
        let expected = 4.0 / 3.0 * std::f64::consts::PI * blob.radius.powi(3);
        let got = size.data.at(row).expect("a size for blob 2");
        assert!(
            (got - expected).abs() <= 1.0,
            "{name} says blob 2's size is {got}, not {expected}"
        );
    }
}

#[actix_web::test]
async fn a_region_query_returns_the_blobs_in_it() {
    let (dir, _) = written();
    let registry = SourceRegistry::new();
    let spec = SourceSpec::File(dir.path().join("cells.csv"));
    let store = objects::open(&registry, &spec).await.expect("open");

    // The blob lattice is 6x6 per z-plane over 512 px, so a 100 px square in
    // the corner holds exactly one column-row pair's worth of blobs — one per
    // z plane, of which there are four.
    let selection = store.query(&ObjectQuery {
        y0: 0.0,
        y1: 100.0,
        x0: 0.0,
        x1: 100.0,
        z0: f32::NEG_INFINITY,
        z1: f32::INFINITY,
        max: 0,
    });
    assert_eq!(selection.total, 4, "one blob per z plane");

    // The same query, restricted to one plane.
    let one_plane = store.query(&ObjectQuery {
        z0: 0.5,
        z1: 1.5,
        ..ObjectQuery {
            y0: 0.0,
            y1: 100.0,
            x0: 0.0,
            x1: 100.0,
            z0: 0.0,
            z1: 0.0,
            max: 0,
        }
    });
    assert_eq!(one_plane.total, 1);
}

#[actix_web::test]
async fn an_object_layer_reports_its_schema_and_bounds() {
    let (dir, blobs) = written();
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let id = session
        .add(
            &registry,
            SourceSpec::File(dir.path().join("cells.blob")),
            LayerRole::Auto,
            None,
            ObjectSpace::default(),
        )
        .await
        .map(only)
        .expect("add layer");

    let layer = session.get(&id).expect("layer");
    let omezarr_viewer_common::LayerKind::Objects { schema, count } = &layer.info().kind else {
        panic!("a table blob is an object layer");
    };
    assert_eq!(*count as usize, blobs.len());
    let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "count", "intensity"]);
    assert!(schema.has_z);

    let bounds = schema.bounds.expect("bounds");
    let max_x = blobs.iter().map(|b| b.x).fold(f64::MIN, f64::max);
    assert!((bounds[5] - max_x).abs() <= 1.0, "bounds cover the rows");
}

#[actix_web::test]
async fn a_scale_maps_a_downsampled_detectors_coordinates_into_the_world() {
    let (dir, blobs) = written();
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    // As if the detector had run on a 2x-downsampled volume.
    let id = session
        .add(
            &registry,
            SourceSpec::File(dir.path().join("cells.csv")),
            LayerRole::Objects,
            None,
            ObjectSpace {
                scale: [1.0, 2.0, 2.0],
                offset: [0.0, 0.0, 0.0],
            },
        )
        .await
        .map(only)
        .expect("add layer");

    let store = session
        .get(&id)
        .and_then(|layer| layer.data.objects())
        .expect("object store");
    let blob = blobs[1];
    let row = store
        .nearest(blob.z as f32, blob.y as f32 * 2.0, blob.x as f32 * 2.0, 2.0)
        .expect("the row at twice the coordinates");
    let position = store.world_position(row).expect("position");
    assert!((position[2] - blob.x as f32 * 2.0).abs() <= 1.0);
}

/// The id of the single layer a source opened as.
///
/// `Session::add` returns a list because a `bioformats2raw` container expands
/// into one layer per series. Every fixture here is one image, so this says so
/// and fails loudly if that ever stops being true.
fn only(ids: Vec<String>) -> String {
    assert_eq!(ids.len(), 1, "expected one layer, got {ids:?}");
    ids.into_iter().next().expect("one layer")
}
