//! Opening a **bioformats2raw container**: a store whose root holds no pixels.
//!
//! This is not an exotic interop case. `bioformats2raw` is the conversion path
//! from CZI, ND2, LIF and the rest into OME-Zarr, and `img2omezarr` — the
//! converter in the repository next door — writes the same layout for
//! everything it produces. Until this existed, none of its output could be
//! opened without the user knowing to append `/0`, and the layer was then
//! called `0`.
//!
//! Nothing caught it for the same reason nothing caught the class-numbering
//! disagreement with `blockflow`: every image fixture here is written by
//! `synthetic.rs`, so the reader had only ever been shown the shape this repo's
//! own writer produces.

use omezarr_viewer_server::objects::ObjectSpace;
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::synthetic;
use omezarr_viewer_server::zarr_reader::{TileEncoding, TileRequest, ZarrStore};
use std::path::{Path, PathBuf};

const SHAPE: (u64, u64, u64) = (4, 64, 64);

/// A container laid out the way `bioformats2raw` and `img2omezarr` write one:
/// a root carrying only the layout key, an `OME/` group indexing the series,
/// and one ordinary NGFF image per numbered subgroup.
fn container(root: &Path, series: &[&str], index: Option<serde_json::Value>) -> PathBuf {
    let store = root.join("container.zarr");
    std::fs::create_dir_all(store.join("OME")).expect("OME group");
    write(
        &store.join(".zgroup"),
        &serde_json::json!({"zarr_format": 2}),
    );
    write(
        &store.join(".zattrs"),
        &serde_json::json!({"bioformats2raw.layout": 3}),
    );
    write(
        &store.join("OME").join(".zgroup"),
        &serde_json::json!({"zarr_format": 2}),
    );
    if let Some(index) = index {
        write(&store.join("OME").join(".zattrs"), &index);
    }
    for name in series {
        let blobs = synthetic::blobs(SHAPE, 3);
        synthetic::write_image(&store.join(name), SHAPE, &blobs).expect("a series");
    }
    store
}

fn write(path: &Path, value: &serde_json::Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).expect("json")).expect("write");
}

#[test]
fn a_container_with_one_series_opens_as_that_image() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0"],
        Some(serde_json::json!({"series": ["0"]})),
    );

    let opened = ZarrStore::open_local(&store).expect("the container opens");
    let level = &opened.metadata().arrays[0];
    assert_eq!(
        level.shape[level.shape.len() - 2..],
        [SHAPE.1, SHAPE.2],
        "the pixels are the series', not the container's — it has none"
    );
    // The attributes must be the *image's*: `image-label` detection and the
    // omero window are read from them, and the container carries neither.
    assert!(
        opened.attributes().contains_key("multiscales"),
        "got the container's attributes instead of the series': {:?}",
        opened.attributes().keys().collect::<Vec<_>>()
    );
}

/// `img2omezarr` writes the 0.5 form, with both the layout key and the series
/// list nested under `ome`. Half its output is that shape.
#[test]
fn the_nested_form_that_img2omezarr_writes_opens_too() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0"],
        Some(serde_json::json!({"ome": {"version": "0.5", "series": ["0"]}})),
    );
    write(
        &store.join(".zattrs"),
        &serde_json::json!({"ome": {"version": "0.5", "bioformats2raw.layout": 3}}),
    );
    assert!(ZarrStore::open_local(&store).is_ok());
}

/// The index is a `SHOULD` in the spec, and a container without one still has
/// its first series where bioformats2raw always puts it.
#[test]
fn a_container_with_no_index_still_finds_the_first_series() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(dir.path(), &["0"], None);
    assert!(ZarrStore::open_local(&store).is_ok());
}

/// Several series are refused by name rather than silently reduced to the
/// first. A slide with three scenes opened as one scene is a wrong picture that
/// looks like a right one.
#[test]
fn several_series_are_refused_rather_than_silently_reduced_to_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0", "1"],
        Some(serde_json::json!({"series": ["0", "1"]})),
    );
    let error = ZarrStore::open_local(&store)
        .err()
        .expect("two series cannot be one image")
        .to_string();
    assert!(error.contains("container"), "{error}");
    assert!(error.contains('0') && error.contains('1'), "{error}");

    // And naming one of them opens it, which is what the message asks for.
    assert!(ZarrStore::open_local(&store.join("1")).is_ok());
}

/// The ordinary case must be untouched: a plain image store is not a container,
/// and nothing about this may add a level of indirection to it.
#[test]
fn an_ordinary_image_store_is_unaffected() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plain.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(&path, SHAPE, &blobs).expect("write image");

    let opened = ZarrStore::open_local(&path).expect("opens as it always did");
    let level = &opened.metadata().arrays[0];
    assert_eq!(level.shape[level.shape.len() - 2..], [SHAPE.1, SHAPE.2]);
}

// -- opening a container as a session, which is where the expansion happens --

async fn open_into_session(store: &Path) -> (Session, Vec<String>) {
    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let ids = session
        .add(
            &registry,
            SourceSpec::File(store.to_path_buf()),
            LayerRole::Auto,
            None,
            ObjectSpace::default(),
        )
        .await
        .expect("the container opens");
    (session, ids)
}

/// A slide with three scenes is three images, and a viewer that showed one of
/// them would be showing a wrong picture that looks like a right one. So the
/// container expands into a layer per series.
#[tokio::test]
async fn a_container_of_several_series_opens_as_a_layer_each() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0", "1", "2"],
        Some(serde_json::json!({"series": ["0", "1", "2"]})),
    );
    let (session, ids) = open_into_session(&store).await;
    assert_eq!(ids.len(), 3, "one layer per series");

    // Distinguishable, or the panel shows three rows called `container.zarr`.
    let names: Vec<&str> = session.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "container.zarr[0]",
            "container.zarr[1]",
            "container.zarr[2]"
        ]
    );
}

/// Each layer's **spec is the series**, not the container. That is what decides
/// where its annotations are written: a coordinate space declared at a
/// container root is a claim about pixels that are not there.
#[tokio::test]
async fn each_layer_points_at_its_own_series_so_annotations_land_there() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0", "1"],
        Some(serde_json::json!({"series": ["0", "1"]})),
    );
    let (session, _) = open_into_session(&store).await;
    for (layer, series) in session.layers().iter().zip(["0", "1"]) {
        assert!(
            layer
                .spec
                .uri()
                .ends_with(&format!("container.zarr/{series}")),
            "layer {} points at {}",
            layer.name,
            layer.spec.uri()
        );
    }
}

/// One series needs no disambiguating, so it keeps the store's own name — but
/// its spec is still the series, for the same annotation reason.
#[tokio::test]
async fn a_single_series_container_keeps_the_stores_plain_name() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0"],
        Some(serde_json::json!({"series": ["0"]})),
    );
    let (session, ids) = open_into_session(&store).await;
    assert_eq!(ids.len(), 1);
    assert_eq!(session.layers()[0].name, "container.zarr");
    assert!(session.layers()[0].spec.uri().ends_with("container.zarr/0"));
}

/// And an ordinary store is still one layer pointing at itself. `.zarr` is a
/// suffix on a *directory*, not a file extension — gating the container probe
/// on "has no extension" is how the first version of this never ran at all.
#[tokio::test]
async fn an_ordinary_store_is_one_layer_that_points_at_itself() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plain.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(&path, SHAPE, &blobs).expect("write image");

    let (session, ids) = open_into_session(&path).await;
    assert_eq!(ids.len(), 1);
    assert_eq!(session.layers()[0].name, "plain.zarr");
    assert!(session.layers()[0].spec.uri().ends_with("plain.zarr"));
}

/// Series are **alternatives, not overlays**, and only the first arrives shown.
///
/// Stacked image layers composite additively — only the bottom-most visible one
/// replaces — so two visible series of one container sum two unrelated pictures
/// into one that means nothing. Measured before this existed: two identical
/// series rendered 1.75x the brightness of one. They do not share a coordinate
/// space either; the world is the first image's, and a second scene is rarely
/// the same size.
#[tokio::test]
async fn only_the_first_series_arrives_visible() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0", "1", "2"],
        Some(serde_json::json!({"series": ["0", "1", "2"]})),
    );
    let (session, _) = open_into_session(&store).await;

    let shown: Vec<bool> = session.layers().iter().map(|l| l.visible).collect();
    assert_eq!(shown, [true, false, false]);

    // And it reaches the client, which is the half that decides what is drawn.
    let reported: Vec<bool> = session.info().layers.iter().map(|l| l.visible).collect();
    assert_eq!(reported, [true, false, false]);
}

/// A store that is not a container is untouched: one layer, shown.
#[tokio::test]
async fn an_ordinary_layer_is_still_visible() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("plain.zarr");
    let blobs = synthetic::blobs(SHAPE, 3);
    synthetic::write_image(&path, SHAPE, &blobs).expect("write image");
    let (session, _) = open_into_session(&path).await;
    assert!(session.layers()[0].visible);
    assert!(session.info().layers[0].visible);
}

/// A single-series container has nothing to hide behind, so it is shown.
#[tokio::test]
async fn a_lone_series_is_not_hidden_by_the_rule_meant_for_its_siblings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0"],
        Some(serde_json::json!({"series": ["0"]})),
    );
    let (session, _) = open_into_session(&store).await;
    assert!(session.layers()[0].visible);
}

/// **Pixels**, not just metadata, from a store opened at its container root.
///
/// Metadata resolution and tile reading take different routes to an array, and
/// for a while only the first knew about the series: the pyramid was described
/// correctly and no tile could be fetched at all, because the read path asked
/// for `/5` where the array is `/0/5`. A real container over the network is
/// what found it, and `public_stores.rs` does not run in CI — so it is pinned
/// here, where it does.
#[tokio::test]
async fn pixels_come_back_from_a_store_opened_at_its_container_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = container(
        dir.path(),
        &["0"],
        Some(serde_json::json!({"series": ["0"]})),
    );
    let opened = ZarrStore::open_local(&store).expect("the container opens");

    // The deepest level, so the request goes through the same level -> path
    // lookup a real pyramid does rather than only ever asking for level 0.
    let level = opened.metadata().arrays.len() - 1;
    let shape = opened.metadata().arrays[level].shape.clone();
    let (h, w) = (shape[shape.len() - 2], shape[shape.len() - 1]);

    let tile = opened
        .read_tile_bytes(&TileRequest {
            level,
            t: 0,
            c: 0,
            z: 0,
            y: 0,
            x: 0,
            h,
            w,
            encoding: TileEncoding::F32,
            projection: None,
            depth: 1,
        })
        .await
        .expect("a tile from inside the container");

    assert_eq!(tile.bytes.len(), (h * w * 4) as usize);
    let values: Vec<f32> = tile
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert!(
        values.iter().any(|v| *v != values[0]),
        "every pixel identical — the synthetic image has structure in it"
    );
}
