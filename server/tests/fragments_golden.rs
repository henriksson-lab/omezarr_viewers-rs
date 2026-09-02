//! The bytes this viewer hands a rasteriser, pinned.
//!
//! The rasteriser is `blockflow`'s `ops::rasterise`, in another repository,
//! reached through a table blob rather than a dependency — that crate pulls
//! burn, candle and a CUDA toolchain behind it, which is why `objects::table`
//! reimplements sixty lines of little-endian words instead of importing them.
//!
//! That decision has a cost, and this file is the payment. `table.rs`'s own
//! header names it: "every consumer writes its own parser against a layout
//! that is documented, at best, in the producer's header — and the layouts
//! drift, because nothing can compare them." Two hand-written statements of one
//! format cannot be checked against each other by a compiler, so they are
//! checked against a **committed artefact** instead.
//!
//! `server/tests/data/fragments.bftable` is that artefact. This test asserts
//! the writer still produces it; `blockflow`'s `tests/viewer_fragments.rs`
//! reads the same bytes and rasterises them. A change to either side that the
//! other does not expect fails one of the two.
//!
//! Regenerate with `UPDATE_FIXTURES=1 cargo test -p server --test
//! fragments_golden`, then copy the file to blockflow's `tests/data/`. The
//! copy is deliberate: a shared path would tie two repositories to a directory
//! layout neither controls.

use omezarr_viewer_common::{Annotation, Geometry, Plane};
use omezarr_viewer_server::annotations::fragments::fragments;

const GOLDEN: &str = "tests/data/fragments.bftable";

/// A scene chosen for what it exercises, not for looking like real work.
///
/// * a polygon **with a hole** — the case that forced a real rasteriser, since
///   outline-plus-flood-fill closes the hole rather than keeping it;
/// * an **open stroke with a width** — partial supervision, the thing a closed
///   region cannot express;
/// * a **dense region** — the assertion that flips what an uncovered pixel
///   means inside it;
/// * a plane and a z extent, so the axes are not all zero.
fn scene() -> Vec<Annotation> {
    let ring = |x: f64, y: f64, size: f64| {
        vec![
            [x, y],
            [x + size, y],
            [x + size, y + size],
            [x, y + size],
            [x, y],
        ]
    };
    vec![
        Annotation {
            id: 1,
            geometry: Geometry::Polygon(vec![ring(10.0, 10.0, 40.0), ring(24.0, 24.0, 12.0)]),
            label: "vessel".into(),
            plane: Plane::at(2, 0),
            z_extent: 3,
            ..Default::default()
        },
        Annotation {
            id: 2,
            geometry: Geometry::LineString(vec![[60.5, 12.25], [72.0, 30.0], [88.75, 26.5]]),
            label: "cell".into(),
            stroke_width: Some(7.0),
            ..Default::default()
        },
        Annotation {
            id: 3,
            geometry: Geometry::rect(0.0, 60.0, 96.0, 96.0),
            label: "cell".into(),
            dense_region: true,
            ..Default::default()
        },
    ]
}

#[test]
fn the_columns_are_the_ones_the_rasteriser_reads() {
    // Names, order *and* types. The consumer reads columns positionally, so a
    // reordering is not a rename — it silently swaps two meanings. This list is
    // the counterpart of `blockflow`'s `ops::rasterise::vertex_schema()`, and
    // the two are only ever equal because both are written down.
    let fragments = fragments(&scene());
    let columns: Vec<(&str, &str)> = fragments
        .columns
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                match c.data {
                    omezarr_viewer_server::objects::ColumnData::U64(_) => "u64",
                    omezarr_viewer_server::objects::ColumnData::F64(_) => "f64",
                },
            )
        })
        .collect();
    assert_eq!(
        columns,
        vec![
            ("shape", "u64"),
            ("ring", "u64"),
            ("vertex", "u64"),
            ("class", "u64"),
            ("closed", "u64"),
            ("dense", "u64"),
            ("z_extent", "u64"),
            ("x", "f64"),
            ("y", "f64"),
            ("half_width", "f64"),
        ]
    );
}

#[test]
fn the_blob_is_byte_for_byte_what_the_rasteriser_was_given() {
    let blob = fragments(&scene()).encode().expect("encoding the scene");

    if std::env::var("UPDATE_FIXTURES").is_ok() {
        std::fs::create_dir_all("tests/data").expect("tests/data");
        std::fs::write(GOLDEN, &blob).expect("writing the fixture");
        eprintln!(
            "wrote {GOLDEN} ({} bytes) — copy it to blockflow",
            blob.len()
        );
        return;
    }

    let golden = std::fs::read(GOLDEN)
        .unwrap_or_else(|e| panic!("{GOLDEN} is missing ({e}); regenerate with UPDATE_FIXTURES=1"));
    assert_eq!(
        blob, golden,
        "the fragment layout changed. If that was deliberate, regenerate with \
         UPDATE_FIXTURES=1 and copy the file to blockflow's tests/data/ — the \
         rasteriser reads these bytes and will not be told otherwise."
    );
}
