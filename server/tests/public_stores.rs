//! The remote read path, against **real public OME-Zarr stores**.
//!
//! Everything else in this suite reads from a temp directory, so
//! `zarrs::filesystem` is exercised and `AsyncOpendalStore` is not — and this
//! codebase has two paths rather than one almost everywhere, which means half
//! the reader had no positive coverage at all. Every remote reference in the
//! other test files is a *negative* one: that a URL classifies as remote, that
//! a write is refused without `--allow-remote-writes`, that a dead host errors.
//! Nothing had ever opened a store over HTTP and got pixels back.
//!
//! # These do not run in CI, on purpose
//!
//! They reach the public internet, so running them in CI would import the
//! IDR's uptime into our build and make a red tick mean "somebody else's
//! server is slow". They are `#[ignore]`d, which `cargo test` reports as
//! ignored rather than skipping silently:
//!
//! ```sh
//! make test-network
//! ```
//!
//! # The stores
//!
//! From OME's own catalogue (`ome/ome-zarr-catalog`, all CC BY 4.0) plus the
//! Open SciVis set, chosen to cover what our local fixtures cannot:
//!
//! * an ordinary **0.4** image, as `omero-zarr` writes one;
//! * a **`bioformats2raw` container**, which is the layout `img2omezarr`
//!   writes for everything it produces — and the async half of that resolution
//!   shipped with no test;
//! * an **0.5 / zarr v3** store, whose metadata lives under `ome` in
//!   `zarr.json` — a path no local fixture takes.
//!
//! A failure here is as likely to be the world moving as a bug: a dataset
//! withdrawn, a bucket renamed, the spec's examples restructured. That is the
//! point of having it, and the reason it is not a gate.

use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::zarr_reader::{TileEncoding, TileRequest, ZarrStore};

/// An ordinary NGFF 0.4 image: IDR study idr0062, written by `omero-zarr`.
const IDR_IMAGE: &str = "https://uk1s3.embassy.ebi.ac.uk/idr/zarr/v0.4/idr0062A/6001240.zarr";

/// A `bioformats2raw` container: the root holds only the layout key, and the
/// image is in the `0` subgroup.
const IDR_CONTAINER: &str = "https://uk1s3.embassy.ebi.ac.uk/idr/zarr/v0.4/idr0048A/9846151.zarr";

/// NGFF 0.5, zarr v3: metadata under `ome` in `zarr.json`.
const SCIVIS_V05: &str =
    "https://ome-zarr-scivis.s3.us-east-1.amazonaws.com/v0.5/96x2/backpack.ome.zarr";

async fn open(uri: &str) -> ZarrStore {
    let registry = SourceRegistry::new();
    let spec = SourceSpec::parse(uri).expect("a URL");
    ZarrStore::open_spec(&registry, &spec)
        .await
        .unwrap_or_else(|e| panic!("opening {uri}: {e:#}"))
}

#[tokio::test]
#[ignore = "reaches the public internet; run with `make test-network`"]
async fn a_real_idr_image_opens_over_https() {
    let store = open(IDR_IMAGE).await;
    let arrays = &store.metadata().arrays;
    assert!(!arrays.is_empty(), "a pyramid with no levels");

    // `(c, z, y, x)`, as this study is published.
    assert_eq!(arrays[0].shape.len(), 4, "shape {:?}", arrays[0].shape);
    assert_eq!(arrays[0].shape[0], 2, "two channels");

    // Levels get smaller. Trivial, and it is what makes a pyramid a pyramid —
    // a reader that mixed up `chunk_shape` and `chunk_grid_shape` (which this
    // one once did) produces levels that do not.
    for pair in arrays.windows(2) {
        assert!(
            pair[1].shape[3] < pair[0].shape[3],
            "levels do not shrink: {:?} then {:?}",
            pair[0].shape,
            pair[1].shape
        );
    }

    // Real rendering settings, which no synthetic fixture here writes.
    let omero = store
        .metadata()
        .metadata
        .omero
        .as_ref()
        .expect("the IDR publishes omero settings");
    assert_eq!(omero.channels.len(), 2);
}

/// The async half of the container resolution, which shipped with no test
/// because this repository has no remote fixture. This *is* the remote fixture.
#[tokio::test]
#[ignore = "reaches the public internet; run with `make test-network`"]
async fn a_real_bioformats2raw_container_resolves_to_its_series() {
    let store = open(IDR_CONTAINER).await;

    // The container root has no pixels at all. Getting a pyramid back means the
    // series was found, the `OME/.zattrs` index was read, and the image group
    // beneath it was opened — over the network, asynchronously.
    let arrays = &store.metadata().arrays;
    assert!(arrays.len() > 1, "a real slide is pyramidal");
    assert_eq!(arrays[0].shape.len(), 5, "(t, c, z, y, x)");
    assert_eq!(arrays[0].shape[1], 3, "three channels");

    // And the attributes are the *image's*, not the container's — the
    // container carries `bioformats2raw.layout` and nothing else.
    assert!(
        store.attributes().contains_key("multiscales"),
        "got the container's attributes: {:?}",
        store.attributes().keys().collect::<Vec<_>>()
    );
}

/// 0.5 keeps the same fields one level down, under `ome`. Both branches of
/// `parse_multiscales` exist for this, and only one was ever taken.
#[tokio::test]
#[ignore = "reaches the public internet; run with `make test-network`"]
async fn a_real_ngff_0_5_store_opens_from_under_ome() {
    let store = open(SCIVIS_V05).await;
    let multiscales = &store.metadata().metadata.multiscales;
    assert_eq!(multiscales.len(), 1);
    assert!(
        !multiscales[0].axes.is_empty(),
        "axes came back empty, so the 0.5 lookup found the wrong thing"
    );
    assert!(!store.metadata().arrays.is_empty());
}

/// Metadata is the easy half. This reads **pixels** from a public store,
/// through the container indirection, at a pyramid level.
#[tokio::test]
#[ignore = "reaches the public internet; run with `make test-network`"]
async fn real_pixels_come_back_from_a_public_store() {
    let store = open(IDR_CONTAINER).await;
    // A coarse level, so the test moves a few hundred kilobytes rather than
    // asking the IDR for a full-resolution plane.
    let level = store.metadata().arrays.len() - 1;
    let shape = store.metadata().arrays[level].shape.clone();
    let (height, width) = (shape[3].min(64), shape[4].min(64));

    let tile = store
        .read_tile_bytes(&TileRequest {
            level,
            t: 0,
            c: 0,
            z: shape[2] / 2,
            y: 0,
            x: 0,
            h: height,
            w: width,
            encoding: TileEncoding::F32,
            projection: None,
            depth: 1,
        })
        .await
        .expect("a tile from the IDR");

    assert_eq!(tile.dtype, "float32");
    assert_eq!(tile.bytes.len(), (height * width * 4) as usize);

    let values: Vec<f32> = tile
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert!(
        values.iter().any(|v| *v != values[0]),
        "every pixel identical — a fill value rather than data"
    );
    assert!(
        values.iter().all(|v| v.is_finite()),
        "a decode that produced non-finite pixels"
    );
}
