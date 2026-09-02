//! Tile reads against a fixture whose every value is known by arithmetic.
//!
//! The claim each test makes is about *which value came back for which pixel* —
//! not that a read succeeded. A tile read that transposed y and x, or that lost
//! the channel offset, returns numbers `value_at` does not predict.

mod fixture;

use fixture::value_at;
use omezarr_viewer_server::objects::ObjectSpace;
use omezarr_viewer_server::session::{LayerRole, Session};
use omezarr_viewer_server::source::{SourceRegistry, SourceSpec};
use omezarr_viewer_server::zarr_reader::{
    PlaneAxis, PlaneRequest, Projection, TileEncoding, TileRequest, ZarrStore,
};

const SHAPE: [u64; 4] = [2, 4, 16, 16];

fn f32_tile(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

#[actix_web::test]
async fn every_dtype_round_trips_through_the_f32_path() {
    for dtype in [
        "uint8", "uint16", "uint32", "uint64", "int16", "int32", "float32", "float64",
    ] {
        let fx = fixture::write(dtype, SHAPE);
        let store = ZarrStore::open_local(fx.path()).expect("open fixture");

        let (h, w) = (3, 4);
        let (y0, x0) = (1, 2);
        let tile = store
            .read_tile_bytes(&TileRequest::new(0, y0, x0, h, w).at(0, 0, 2))
            .await
            .unwrap_or_else(|e| panic!("{dtype}: {e:#}"));
        assert_eq!(tile.dtype, "float32", "{dtype} is f32 on the wire");
        let pixels = f32_tile(&tile.bytes);
        assert_eq!(pixels.len() as u64, h * w, "{dtype} tile size");

        for row in 0..h {
            for col in 0..w {
                let expected = value_at(0, 2, y0 + row, x0 + col);
                let expected = if dtype == "uint8" {
                    (expected % 256) as f32
                } else {
                    expected as f32
                };
                let got = pixels[(row * w + col) as usize];
                assert_eq!(got, expected, "{dtype} at ({row},{col})");
            }
        }
    }
}

#[actix_web::test]
async fn raw_encoding_preserves_the_arrays_own_dtype() {
    let fx = fixture::write("uint32", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");

    let tile = store
        .read_tile_bytes(
            &TileRequest::new(0, 0, 0, 2, 2)
                .at(0, 1, 3)
                .encoded(TileEncoding::Raw),
        )
        .await
        .expect("raw tile");
    assert_eq!(tile.dtype, "uint32");
    assert_eq!(tile.bytes.len(), 4 * 4);

    let ids: Vec<u32> = tile
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    for row in 0..2u64 {
        for col in 0..2u64 {
            assert_eq!(
                ids[(row * 2 + col) as usize] as u64,
                value_at(1, 3, row, col),
                "raw id at ({row},{col})"
            );
        }
    }
}

/// Channel 1 of the fixture holds ids above 2^24. Through the f32 path they
/// come back as *different* ids; raw is the only encoding a label layer can use.
#[actix_web::test]
async fn wide_ids_survive_raw_and_not_f32() {
    let fx = fixture::write_wide(SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");

    let truth = fixture::wide_value_at(1, 0, 0, 1);
    assert!(truth > (1 << 24), "the fixture's ids exercise the range");

    let raw = store
        .read_tile_bytes(
            &TileRequest::new(0, 0, 1, 1, 1)
                .at(0, 1, 0)
                .encoded(TileEncoding::Raw),
        )
        .await
        .expect("raw tile");
    let id = u32::from_le_bytes([raw.bytes[0], raw.bytes[1], raw.bytes[2], raw.bytes[3]]);
    assert_eq!(id as u64, truth);

    let lossy = store
        .read_tile_bytes(&TileRequest::new(0, 0, 1, 1, 1).at(0, 1, 0))
        .await
        .expect("f32 tile");
    assert_ne!(f32_tile(&lossy.bytes)[0] as u64, truth);
}

#[actix_web::test]
async fn level_one_is_the_decimated_grid() {
    let fx = fixture::write("uint16", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");

    let info = store.metadata();
    assert_eq!(info.arrays.len(), 2);
    assert_eq!(info.arrays[1].shape, vec![2, 4, 8, 8]);

    let tile = store
        .read_tile_bytes(&TileRequest::new(1, 0, 0, 2, 2))
        .await
        .expect("level 1 tile");
    let pixels = f32_tile(&tile.bytes);
    // Level 1 sample (row, col) is level 0 pixel (2*row, 2*col).
    assert_eq!(pixels[3], value_at(0, 0, 2, 2) as f32);
}

#[actix_web::test]
async fn reading_past_the_last_level_is_an_error_not_a_panic() {
    let fx = fixture::write("uint8", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");
    assert!(store
        .read_tile_bytes(&TileRequest::new(9, 0, 0, 2, 2))
        .await
        .is_err());
}

#[actix_web::test]
async fn a_session_resolves_named_and_default_layers() {
    let image = fixture::write("uint16", SHAPE);
    let labels = fixture::write("uint32", SHAPE);
    fixture::mark_as_labels(&labels);

    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let label_id = session
        .add(
            &registry,
            SourceSpec::File(labels.path().to_path_buf()),
            LayerRole::Auto,
            None,
            ObjectSpace::default(),
        )
        .await
        .map(only)
        .expect("add labels");
    let image_id = session
        .add(
            &registry,
            SourceSpec::File(image.path().to_path_buf()),
            LayerRole::Auto,
            Some("intensity".into()),
            ObjectSpace::default(),
        )
        .await
        .map(only)
        .expect("add image");

    // The label store was added first, but the default layer is the image one:
    // that is what keeps `/api/tile` with no `layer=` answering about pixels.
    assert_eq!(session.default_layer().unwrap().id, image_id);
    assert_eq!(session.resolve(Some(&label_id)).unwrap().id, label_id);
    assert!(session.resolve(Some("nope")).is_none());

    let info = session.info();
    assert_eq!(info.layers.len(), 2);
    assert!(matches!(
        info.layers[0].kind,
        omezarr_viewer_common::LayerKind::Labels { .. }
    ));
    assert!(matches!(
        info.layers[1].kind,
        omezarr_viewer_common::LayerKind::Image { .. }
    ));
    assert_eq!(info.layers[1].name, "intensity");

    // `image-label` colours travel with the layer.
    let omezarr_viewer_common::LayerKind::Labels { colors, .. } = &info.layers[0].kind else {
        unreachable!()
    };
    assert_eq!(colors.as_ref().expect("colours").len(), 2);

    assert!(session.remove(&label_id));
    assert_eq!(session.info().layers.len(), 1);
}

#[actix_web::test]
async fn an_object_source_becomes_an_object_layer() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("spots.csv");
    std::fs::write(
        &path,
        "id,x,y,confidence,class\n1,10,20,0.9,0\n2,30,40,0.5,1\n",
    )
    .expect("write csv");

    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let id = session
        .add(
            &registry,
            SourceSpec::File(path),
            LayerRole::Auto,
            None,
            ObjectSpace::default(),
        )
        .await
        .map(only)
        .expect("add objects");

    let layer = session.get(&id).expect("layer");
    let store = layer.data.objects().expect("object store");
    assert_eq!(store.len(), 2);

    let omezarr_viewer_common::LayerKind::Objects { schema, count } = &layer.info().kind else {
        panic!("a csv is an object layer");
    };
    assert_eq!(*count, 2);
    let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["id", "confidence", "class"]);
    assert!(!schema.has_z, "a 2D detector's rows have no z");
}

#[actix_web::test]
async fn an_unreadable_object_format_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("cells.parquet");
    std::fs::write(&path, b"PAR1").expect("write");

    let registry = SourceRegistry::new();
    let mut session = Session::new();
    let err = session
        .add(
            &registry,
            SourceSpec::File(path),
            LayerRole::Objects,
            None,
            ObjectSpace::default(),
        )
        .await
        .map(only)
        .expect_err("refused");
    assert!(format!("{err:#}").contains("parquet"), "{err:#}");
}

#[actix_web::test]
async fn a_max_projection_is_the_brightest_plane_in_the_slab() {
    let fx = fixture::write("uint16", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");

    // The fixture's value grows with z, so the maximum over a slab is the top
    // plane of it and the mean is the arithmetic mean — both known without
    // reading anything back.
    let (h, w) = (2, 2);
    let tile = store
        .read_tile_bytes(
            &TileRequest::new(0, 4, 6, h, w)
                .at(0, 0, 1)
                .projected(Some(Projection::Max), 3),
        )
        .await
        .expect("max projection");
    assert_eq!(tile.dtype, "float32");
    let pixels = f32_tile(&tile.bytes);
    assert_eq!(pixels[0], value_at(0, 3, 4, 6) as f32);

    let tile = store
        .read_tile_bytes(
            &TileRequest::new(0, 4, 6, h, w)
                .at(0, 0, 1)
                .projected(Some(Projection::Mean), 3),
        )
        .await
        .expect("mean projection");
    let pixels = f32_tile(&tile.bytes);
    let expected = (1..4).map(|z| value_at(0, z, 4, 6) as f32).sum::<f32>() / 3.0;
    assert_eq!(pixels[0], expected);
}

/// A slab that runs off the top of the stack projects the planes that exist.
#[actix_web::test]
async fn a_projection_past_the_last_plane_uses_what_is_there() {
    let fx = fixture::write("uint16", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");
    let tile = store
        .read_tile_bytes(
            &TileRequest::new(0, 0, 0, 1, 1)
                .at(0, 0, 3)
                .projected(Some(Projection::Max), 8),
        )
        .await
        .expect("clamped projection");
    // SHAPE has four z planes, so the slab covers only the last one.
    assert_eq!(f32_tile(&tile.bytes)[0], value_at(0, 3, 0, 0) as f32);
}

/// A projection of label ids would be meaningless, so it never travels as raw.
#[actix_web::test]
async fn a_projection_is_f32_even_when_raw_was_asked_for() {
    let fx = fixture::write("uint32", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");
    let tile = store
        .read_tile_bytes(
            &TileRequest::new(0, 0, 0, 1, 1)
                .at(0, 0, 0)
                .encoded(TileEncoding::Raw)
                .projected(Some(Projection::Max), 2),
        )
        .await
        .expect("projection");
    assert_eq!(tile.dtype, "float32");
    assert_eq!(tile.bytes.len(), 4);
}

#[actix_web::test]
async fn orthogonal_planes_hold_the_axes_they_claim() {
    let fx = fixture::write("uint16", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");

    // A y plane is (z, x): four planes of sixteen columns.
    let plane = store
        .read_plane(&PlaneRequest {
            level: 0,
            t: 0,
            c: 0,
            axis: PlaneAxis::Y,
            index: 5,
            encoding: TileEncoding::F32,
        })
        .await
        .expect("y plane");
    assert_eq!((plane.height, plane.width), (SHAPE[1], SHAPE[3]));
    let pixels = f32_tile(&plane.bytes);
    for z in 0..SHAPE[1] {
        for x in 0..SHAPE[3] {
            assert_eq!(
                pixels[(z * SHAPE[3] + x) as usize],
                value_at(0, z, 5, x) as f32,
                "y plane at (z={z}, x={x})"
            );
        }
    }

    // An x plane is (z, y).
    let plane = store
        .read_plane(&PlaneRequest {
            level: 0,
            t: 0,
            c: 1,
            axis: PlaneAxis::X,
            index: 7,
            encoding: TileEncoding::F32,
        })
        .await
        .expect("x plane");
    assert_eq!((plane.height, plane.width), (SHAPE[1], SHAPE[2]));
    let pixels = f32_tile(&plane.bytes);
    assert_eq!(
        pixels[(2 * SHAPE[2] + 3) as usize],
        value_at(1, 2, 3, 7) as f32
    );

    // And a z plane is the ordinary view, full size.
    let plane = store
        .read_plane(&PlaneRequest {
            level: 0,
            t: 0,
            c: 0,
            axis: PlaneAxis::Z,
            index: 2,
            encoding: TileEncoding::F32,
        })
        .await
        .expect("z plane");
    assert_eq!((plane.height, plane.width), (SHAPE[2], SHAPE[3]));
    assert_eq!(f32_tile(&plane.bytes)[0], value_at(0, 2, 0, 0) as f32);
}

#[actix_web::test]
async fn a_plane_index_past_the_edge_is_clamped_rather_than_refused() {
    let fx = fixture::write("uint8", SHAPE);
    let store = ZarrStore::open_local(fx.path()).expect("open fixture");
    let plane = store
        .read_plane(&PlaneRequest {
            level: 0,
            t: 0,
            c: 0,
            axis: PlaneAxis::X,
            index: 9999,
            encoding: TileEncoding::F32,
        })
        .await
        .expect("clamped plane");
    assert_eq!((plane.height, plane.width), (SHAPE[1], SHAPE[2]));
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
