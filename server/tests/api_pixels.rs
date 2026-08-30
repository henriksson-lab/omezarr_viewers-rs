//! The pixel routes, over HTTP: `/api/tile`, `/api/slice`, `/api/value` and
//! `/api/regions`.
//!
//! The claim is that these four answer with *bytes and headers a client can
//! act on* — an octet stream of exactly the asked-for size, the dtype it is
//! really in, an id that survived the wire — and that a request they cannot
//! serve is refused with a status that says whose fault it was, rather than
//! with a 200 holding something else or a panic.

mod api_harness;

use std::path::{Path, PathBuf};

use api_harness::{Api, Res, SHAPE};
use omezarr_viewer_server::session::LayerRole;

/// An id that does not survive an f32 round trip: `2^24 + 1` is the first
/// integer f32 cannot hold, and a label id is exactly the kind of number that
/// lands there.
const BIG_ID: u32 = 16_777_217;

fn u32_pixels(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect()
}

fn f32_pixels(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Assert a response is a tile of `w * h` four-byte pixels in `dtype`.
fn assert_tile(res: &Res, dtype: &str, w: u64, h: u64) {
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(
        res.header("content-type").as_deref(),
        Some("application/octet-stream"),
        "a tile is bytes, not JSON: {}",
        res.text()
    );
    assert_eq!(res.header("x-dtype").as_deref(), Some(dtype));
    assert_eq!(
        res.header("x-width").as_deref(),
        Some(w.to_string().as_str())
    );
    assert_eq!(
        res.header("x-height").as_deref(),
        Some(h.to_string().as_str())
    );
    assert_eq!(
        res.body.len() as u64,
        w * h * 4,
        "{dtype} tile of {w}x{h} is the wrong length"
    );
}

/// Write a `.npy` label volume holding one very large id.
///
/// A hand-written fixture rather than the synthetic store: `synthetic::blobs`
/// numbers its ids from 1, so nothing it writes can show whether a wide id
/// survives the wire — which is the whole point of `encoding=raw`.
fn write_big_id_labels(root: &Path) -> PathBuf {
    let (z, y, x) = (2usize, 4usize, 4usize);
    let mut values = vec![0u32; z * y * x];
    // One id, at (z=1, y=2, x=3), so a tile that transposed anything would
    // report zero rather than a truncated id.
    values[(y * x) + (2 * x) + 3] = BIG_ID;

    let dict = format!("{{'descr': '<u4', 'fortran_order': False, 'shape': ({z}, {y}, {x}), }}");
    let mut header = dict.into_bytes();
    while !(10 + header.len() + 1).is_multiple_of(64) {
        header.push(b' ');
    }
    header.push(b'\n');

    let mut npy = Vec::new();
    npy.extend_from_slice(b"\x93NUMPY\x01\x00");
    npy.extend_from_slice(&(header.len() as u16).to_le_bytes());
    npy.extend_from_slice(&header);
    for value in &values {
        npy.extend_from_slice(&value.to_le_bytes());
    }

    let path = root.join("big_ids.npy");
    std::fs::write(&path, npy).expect("write big_ids.npy");
    path
}

/// An image, a label layer and an object layer, which is what `/api/regions`
/// needs and no single harness fixture offers.
async fn regions_fixture() -> (Api, String, String) {
    let api = Api::with_labels().await;
    let blobs = omezarr_viewer_server::synthetic::blobs(SHAPE, 3);
    omezarr_viewer_server::synthetic::write_objects(api.dir.path(), &blobs).expect("write objects");
    let objects = api
        .open(&api.dir.path().join("cells.csv"), LayerRole::Objects)
        .await;
    let labels = api.layer_of_kind("labels").await;
    (api, labels, objects)
}

// -- /api/tile ---------------------------------------------------------------

#[actix_web::test]
async fn a_tile_is_an_octet_stream_of_exactly_the_pixels_that_were_asked_for() {
    let api = Api::image().await;
    let res = api.get("/api/tile?level=0&z=0&y=0&x=0&h=32&w=48").await;
    // The client uploads the body straight into a texture, so a body that is
    // one pixel short or one row long is a corrupt picture, not a slow one.
    assert_tile(&res, "float32", 48, 32);
    assert_eq!(res.header("x-cache").as_deref(), Some("miss"));
}

#[actix_web::test]
async fn the_same_tile_asked_for_twice_comes_back_from_the_cache_unchanged() {
    let api = Api::image().await;
    let uri = "/api/tile?level=0&z=2&y=16&x=16&h=32&w=32";
    let first = api.get(uri).await;
    let second = api.get(uri).await;
    assert_eq!(first.header("x-cache").as_deref(), Some("miss"));
    assert_eq!(second.header("x-cache").as_deref(), Some("hit"));
    // The cached branch reconstructs the dtype header without re-reading the
    // array; getting that wrong would tell the client to decode raw bytes as
    // floats on the second request only.
    assert_eq!(first.header("x-dtype"), second.header("x-dtype"));
    assert_eq!(first.body, second.body);
}

#[actix_web::test]
async fn a_label_tile_comes_back_as_raw_integers() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;
    let res = api
        .get(&format!(
            "/api/tile?layer={labels}&level=0&z=1&y=0&x=0&h=64&w=64&encoding=raw"
        ))
        .await;
    // uint32 in, uint32 out: the ids are the answer, not a picture of it.
    assert_tile(&res, "uint32", 64, 64);

    let ids = u32_pixels(&res.body);
    let (at, id) = ids
        .iter()
        .enumerate()
        .find(|(_, id)| **id != 0)
        .map(|(at, id)| (at, *id))
        .expect("the label plane at z=1 holds at least one blob");

    // The same voxel through /api/value must say the same id, because the
    // viewer picks ids off the tile and then asks this route about them.
    let (y, x) = (at as u64 / 64, at as u64 % 64);
    let value = api
        .get(&format!(
            "/api/value?layer={labels}&level=0&z=1&y={y}&x={x}"
        ))
        .await;
    assert!(value.is_ok(), "{} {}", value.status, value.text());
    assert_eq!(value.json()["id"].as_u64(), Some(id as u64));
    assert_eq!(value.json()["dtype"], "uint32");
}

#[actix_web::test]
async fn an_id_above_two_to_the_twentyfourth_survives_raw_and_is_destroyed_by_f32() {
    let api = Api::image().await;
    let path = write_big_id_labels(api.dir.path());
    let layer = api.open(&path, LayerRole::Labels).await;
    let at = 2 * 4 + 3; // (y=2, x=3) of the 4x4 plane at z=1

    let raw = api
        .get(&format!(
            "/api/tile?layer={layer}&level=0&z=1&y=0&x=0&h=4&w=4&encoding=raw"
        ))
        .await;
    assert_tile(&raw, "uint32", 4, 4);
    assert_eq!(
        u32_pixels(&raw.body)[at],
        BIG_ID,
        "raw is the only encoding a wide id survives"
    );

    // The other half of the rule: the f32 path really does lose it, so a
    // client that forgets `encoding=raw` filters on an id that never existed.
    let lossy = api
        .get(&format!(
            "/api/tile?layer={layer}&level=0&z=1&y=0&x=0&h=4&w=4"
        ))
        .await;
    assert_tile(&lossy, "float32", 4, 4);
    assert_eq!(f32_pixels(&lossy.body)[at], 16_777_216.0);

    // /api/value reads the array in its own dtype whatever the tile asked for,
    // which is why clicking a label is trustworthy when the picture is not.
    let value = api
        .get(&format!("/api/value?layer={layer}&level=0&z=1&y=2&x=3"))
        .await;
    assert!(value.is_ok(), "{} {}", value.status, value.text());
    assert_eq!(value.json()["id"].as_u64(), Some(BIG_ID as u64));
    assert_eq!(value.json()["value"].as_f64(), Some(16_777_216.0));
}

#[actix_web::test]
async fn a_projection_is_f32_even_on_a_label_layer_that_asked_for_raw() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;
    let res = api
        .get(&format!(
            "/api/tile?layer={labels}&level=0&z=0&y=0&x=0&h=16&w=16&encoding=raw&zproj=max&depth=4"
        ))
        .await;
    // The maximum of a set of ids is not an id, so `raw` is not honoured
    // silently: the dtype header says float32 and the client can see that the
    // bytes it got are not the ones it asked for.
    assert_tile(&res, "float32", 16, 16);
}

#[actix_web::test]
async fn a_projection_running_past_the_top_of_the_stack_reduces_what_is_there() {
    let api = Api::image().await;
    // The slab is a viewing choice; the end of the volume is not a mistake.
    let res = api
        .get("/api/tile?level=0&z=6&y=0&x=0&h=8&w=8&zproj=mean&depth=64")
        .await;
    assert_tile(&res, "float32", 8, 8);
    assert!(
        f32_pixels(&res.body).iter().all(|v| v.is_finite()),
        "a projection over a truncated slab must not divide by zero planes"
    );
}

#[actix_web::test]
async fn a_z_past_the_end_of_the_volume_is_clamped_to_the_last_plane() {
    let api = Api::image().await;
    let last = api
        .get(&format!(
            "/api/tile?level=0&z={}&y=0&x=0&h=8&w=8",
            SHAPE.0 - 1
        ))
        .await;
    let past = api.get("/api/tile?level=0&z=9999&y=0&x=0&h=8&w=8").await;
    assert!(past.is_ok(), "{} {}", past.status, past.text());
    // Scrolling one notch past the end must show the end, not an error page:
    // the client's z slider and the store's extent are separately rounded.
    assert_eq!(past.body, last.body);
}

#[actix_web::test]
async fn a_tile_of_an_unknown_layer_is_a_404() {
    let api = Api::image().await;
    let res = api
        .get("/api/tile?layer=L99&level=0&z=0&y=0&x=0&h=8&w=8")
        .await;
    assert_eq!(res.status, 404, "{}", res.text());
    assert!(res.text().contains("No such layer"), "{}", res.text());
}

#[actix_web::test]
async fn a_tile_of_a_layer_that_holds_no_pixels_is_a_400() {
    let api = Api::with_objects().await;
    let objects = api.layer_of_kind("objects").await;
    let res = api
        .get(&format!(
            "/api/tile?layer={objects}&level=0&z=0&y=0&x=0&h=8&w=8"
        ))
        .await;
    // A table is open and named; asking it for pixels is the caller's mistake
    // and not a missing layer, so the status distinguishes the two.
    assert_eq!(res.status, 400, "{}", res.text());
    assert!(res.text().contains("holds no image data"), "{}", res.text());
}

#[actix_web::test]
async fn a_tile_on_an_empty_session_is_a_404() {
    let api = Api::empty().await;
    let res = api.get("/api/tile?level=0&z=0&y=0&x=0&h=8&w=8").await;
    assert_eq!(res.status, 404, "{}", res.text());
}

#[actix_web::test]
async fn a_tile_with_missing_or_unparseable_geometry_is_a_400_not_a_500() {
    let api = Api::image().await;
    for uri in [
        "/api/tile",                                // nothing at all
        "/api/tile?level=0&z=0&y=0&x=0&h=8",        // no width
        "/api/tile?level=0&z=0&y=0&h=8&w=8",        // no x
        "/api/tile?level=frog&z=0&y=0&x=0&h=8&w=8", // level is not a number
        "/api/tile?level=0&z=0&y=-1&x=0&h=8&w=8",   // y is unsigned
        "/api/tile?level=0&z=0&y=0&x=0&h=8&w=1e9",  // and so is w
    ] {
        let res = api.get(uri).await;
        // A malformed query is the client's error. A 500 here would send the
        // frontend retrying a request that can never succeed.
        assert_eq!(res.status, 400, "{uri}: {} {}", res.status, res.text());
    }
}

#[actix_web::test]
async fn an_unrecognised_encoding_or_projection_falls_back_rather_than_failing() {
    let api = Api::image().await;
    // A client that knows nothing about encodings must still get a picture,
    // so anything unparseable means "the default", not "no".
    let res = api
        .get("/api/tile?level=0&z=0&y=0&x=0&h=8&w=8&encoding=jpeg&zproj=median")
        .await;
    assert_tile(&res, "float32", 8, 8);
    let plain = api.get("/api/tile?level=0&z=0&y=0&x=0&h=8&w=8").await;
    assert_eq!(res.body, plain.body, "the fallback is the plain f32 slice");
}

#[actix_web::test]
async fn a_tile_past_the_edge_of_the_volume_is_padded_rather_than_refused() {
    let api = Api::image().await;
    // A tile grid does not divide the volume evenly, so the last tile of a row
    // legitimately overhangs it; padding is the contract, not an accident.
    let res = api.get("/api/tile?level=0&z=0&y=9000&x=9000&h=8&w=8").await;
    assert_tile(&res, "float32", 8, 8);
    assert!(f32_pixels(&res.body).iter().all(|v| *v == 0.0));
}

#[actix_web::test]
async fn a_channel_that_does_not_exist_comes_back_black_rather_than_refused() {
    let api = Api::image().await;
    // The fixture has two channels. Asking for the tenth is answered with the
    // fill value and a 200 — pinned as current behaviour, and reported: unlike
    // an overhanging tile, a channel index has no legitimate out-of-range case,
    // so a black tile here is indistinguishable from real black data.
    let res = api.get("/api/tile?level=0&c=9&z=0&y=0&x=0&h=8&w=8").await;
    assert_tile(&res, "float32", 8, 8);
    assert!(f32_pixels(&res.body).iter().all(|v| *v == 0.0));
}

#[actix_web::test]
async fn a_time_index_is_ignored_by_a_volume_that_has_no_time_axis() {
    let api = Api::image().await;
    // The fixture is (c, z, y, x). `t` names an axis it does not have, and the
    // reader skips it rather than indexing something else with it.
    let timeless = api.get("/api/tile?level=0&z=0&y=0&x=0&h=8&w=8").await;
    let timed = api.get("/api/tile?level=0&t=5&z=0&y=0&x=0&h=8&w=8").await;
    assert_tile(&timed, "float32", 8, 8);
    assert_eq!(timed.body, timeless.body);
}

#[actix_web::test]
async fn a_level_outside_the_dataset_is_a_500_from_tile_and_value_but_a_400_from_slice() {
    let api = Api::image().await;
    // Pinned, not endorsed. All three are the same caller error — a level the
    // dataset does not have — and only /api/slice, which checks the level
    // before reading, calls it one.
    for uri in [
        "/api/tile?level=42&z=0&y=0&x=0&h=8&w=8",
        "/api/value?level=42&z=0&y=0&x=0",
    ] {
        let res = api.get(uri).await;
        assert_eq!(res.status, 500, "{uri}: {} {}", res.status, res.text());
        assert!(
            res.text().contains("outside this dataset"),
            "{}",
            res.text()
        );
    }
    assert_eq!(api.get("/api/slice?level=42&index=0").await.status, 400);
}

// -- /api/slice --------------------------------------------------------------

#[actix_web::test]
async fn a_z_slice_is_the_whole_plane_and_says_its_shape_in_the_headers() {
    let api = Api::image().await;
    let res = api.get("/api/slice?level=0&index=3").await;
    // The shape rides in headers so the body stays a bare array of pixels the
    // client can upload without unpacking anything.
    assert_tile(&res, "float32", SHAPE.2, SHAPE.1);
}

#[actix_web::test]
async fn the_orthogonal_slices_have_the_shapes_their_axes_imply() {
    let api = Api::image().await;
    // A (z, x) plane and a (z, y) plane: the panes below and beside the main
    // view, and the reason a slice is not just a full-width tile.
    let y = api.get("/api/slice?level=0&axis=y&index=64").await;
    assert_tile(&y, "float32", SHAPE.2, SHAPE.0);
    let x = api.get("/api/slice?level=0&axis=x&index=64").await;
    assert_tile(&x, "float32", SHAPE.1, SHAPE.0);
}

#[actix_web::test]
async fn an_unrecognised_axis_is_the_z_plane() {
    let api = Api::image().await;
    let odd = api.get("/api/slice?level=0&axis=diagonal&index=3").await;
    let z = api.get("/api/slice?level=0&axis=z&index=3").await;
    assert!(odd.is_ok(), "{} {}", odd.status, odd.text());
    assert_eq!(odd.body, z.body);
}

#[actix_web::test]
async fn a_label_slice_keeps_its_own_dtype_under_raw() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;
    // The orthogonal panes draw labels too, and an id averaged into a float
    // there is as wrong as it is in the main view.
    let res = api
        .get(&format!(
            "/api/slice?layer={labels}&level=0&index=1&encoding=raw"
        ))
        .await;
    assert_tile(&res, "uint32", SHAPE.2 / 2, SHAPE.1 / 2);
    assert!(
        u32_pixels(&res.body).iter().any(|id| *id != 0),
        "the label plane at z=1 should hold blobs"
    );
}

#[actix_web::test]
async fn a_slice_index_past_the_end_of_its_axis_is_clamped() {
    let api = Api::image().await;
    let last = api
        .get(&format!("/api/slice?level=0&index={}", SHAPE.0 - 1))
        .await;
    let past = api.get("/api/slice?level=0&index=9999").await;
    assert!(past.is_ok(), "{} {}", past.status, past.text());
    assert_eq!(past.body, last.body);
}

#[actix_web::test]
async fn a_slice_level_outside_the_dataset_is_a_400() {
    let api = Api::image().await;
    let res = api.get("/api/slice?level=42&index=0").await;
    // The level is checked before the read here, so this route names the
    // caller's mistake — unlike /api/tile, which discovers it in the reader.
    assert_eq!(res.status, 400, "{}", res.text());
}

#[actix_web::test]
async fn a_slice_with_no_index_or_no_level_is_a_400() {
    let api = Api::image().await;
    for uri in ["/api/slice", "/api/slice?level=0", "/api/slice?index=0"] {
        let res = api.get(uri).await;
        assert_eq!(res.status, 400, "{uri}: {} {}", res.status, res.text());
    }
}

#[actix_web::test]
async fn a_slice_of_an_unknown_layer_is_a_404_and_of_a_table_layer_a_400() {
    let api = Api::with_objects().await;
    let objects = api.layer_of_kind("objects").await;
    let unknown = api.get("/api/slice?layer=L99&level=0&index=0").await;
    assert_eq!(unknown.status, 404, "{}", unknown.text());
    let table = api
        .get(&format!("/api/slice?layer={objects}&level=0&index=0"))
        .await;
    assert_eq!(table.status, 400, "{}", table.text());
}

// -- /api/value --------------------------------------------------------------

#[actix_web::test]
async fn an_image_voxel_reports_its_dtype_its_integer_and_its_float() {
    let api = Api::image().await;
    let res = api.get("/api/value?level=0&z=0&y=100&x=100").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert_eq!(body["dtype"], "uint16");
    // The two forms must agree: the inspector shows one and filters on the
    // other, and a uint16 has no fractional part to disagree about.
    let id = body["id"].as_u64().expect("a uint16 voxel has an integer");
    let value = body["value"].as_f64().expect("and a float");
    assert_eq!(id as f64, value);
    assert!(id > 0, "the fixture is not black at (100, 100)");
    assert_eq!(body["y"], 100);
    assert_eq!(body["x"], 100);
    assert_eq!(body["z"], 0);
}

#[actix_web::test]
async fn a_voxel_outside_every_blob_is_the_background_id() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;
    let res = api
        .get(&format!("/api/value?layer={labels}&level=0&z=0&y=0&x=0"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    // Zero is a real answer — "no object here" — not a missing one.
    assert_eq!(res.json()["id"].as_u64(), Some(0));
    assert_eq!(res.json()["name"], serde_json::Value::Null);
}

#[actix_web::test]
async fn a_value_request_names_no_region_when_no_ontology_is_loaded() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;
    let res = api
        .get(&format!("/api/value?layer={labels}&level=0&z=1&y=10&x=10"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    // The keys are always present: the client reads them unconditionally, and
    // an absent key and a null one are different things in JS.
    assert!(body.get("name").is_some() && body.get("acronym").is_some());
    assert_eq!(body["name"], serde_json::Value::Null);
}

#[actix_web::test]
async fn a_value_of_an_unknown_or_pixel_less_layer_is_a_404() {
    let api = Api::with_objects().await;
    let objects = api.layer_of_kind("objects").await;
    for layer in ["L99", objects.as_str()] {
        let res = api
            .get(&format!("/api/value?layer={layer}&level=0&z=0&y=0&x=0"))
            .await;
        assert_eq!(res.status, 404, "{layer}: {} {}", res.status, res.text());
    }
}

#[actix_web::test]
async fn a_value_with_no_coordinates_is_a_400_not_a_500() {
    let api = Api::image().await;
    for uri in [
        "/api/value",
        "/api/value?level=0&y=1",
        "/api/value?level=0&y=1&x=nope",
    ] {
        let res = api.get(uri).await;
        assert_eq!(res.status, 400, "{uri}: {} {}", res.status, res.text());
    }
}

#[actix_web::test]
async fn a_value_past_the_edge_of_the_volume_reports_the_fill_value() {
    let api = Api::image().await;
    let res = api.get("/api/value?level=0&z=0&y=100000&x=0").await;
    // The same padding the tile path relies on: a click that lands off the
    // volume reads as empty rather than as an error page in the inspector.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(res.json()["id"].as_u64(), Some(0));
    assert_eq!(res.json()["value"].as_f64(), Some(0.0));
}

// -- /api/regions ------------------------------------------------------------

#[actix_web::test]
async fn regions_counts_every_object_row_into_the_label_it_lands_in() {
    let (api, labels, objects) = regions_fixture().await;
    let res = api
        .get(&format!("/api/regions?labels={labels}&objects={objects}"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let rows = res.json();
    let rows = rows.as_array().expect("regions is an array");
    assert!(!rows.is_empty(), "the fixture's objects sit on its blobs");

    let total: u64 = rows.iter().map(|r| r["count"].as_u64().unwrap()).sum();
    // Every row is counted exactly once, background included: a route that
    // reports fewer has dropped detections without saying so.
    assert_eq!(total, 36, "{rows:?}");
    assert!(
        rows.iter().any(|r| r["id"].as_u64() != Some(0)),
        "objects drawn on blobs must land on ids: {rows:?}"
    );

    // Most populous first, ties by id, so the panel's order is stable across
    // reloads rather than a HashMap's.
    let order: Vec<(u64, u64)> = rows
        .iter()
        .map(|r| (r["count"].as_u64().unwrap(), r["id"].as_u64().unwrap()))
        .collect();
    let mut sorted = order.clone();
    sorted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    assert_eq!(order, sorted, "regions must come back sorted");
}

#[actix_web::test]
async fn a_regions_limit_keeps_the_most_populous_rows() {
    let (api, labels, objects) = regions_fixture().await;
    let all = api
        .get(&format!("/api/regions?labels={labels}&objects={objects}"))
        .await;
    let limited = api
        .get(&format!(
            "/api/regions?labels={labels}&objects={objects}&limit=2"
        ))
        .await;
    assert!(limited.is_ok(), "{} {}", limited.status, limited.text());
    let all = all.json();
    let limited = limited.json();
    let expected = all.as_array().unwrap().len().min(2);
    assert_eq!(limited.as_array().unwrap().len(), expected);
    // Truncation happens after the sort, so the head of the list is the same.
    assert_eq!(limited[0], all[0]);
}

#[actix_web::test]
async fn a_coarser_label_level_counts_the_same_objects() {
    let (api, labels, objects) = regions_fixture().await;
    let coarse = api
        .get(&format!(
            "/api/regions?labels={labels}&objects={objects}&level=1"
        ))
        .await;
    assert!(coarse.is_ok(), "{} {}", coarse.status, coarse.text());
    // A coarser level is a speed choice: which id a point falls in may change
    // at the edges, but no point may be lost, because the positions are scaled
    // into the level rather than assumed to be in it.
    let total: u64 = coarse
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["count"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 36, "{}", coarse.text());
}

#[actix_web::test]
async fn regions_without_both_a_label_and_an_object_layer_is_a_404() {
    let api = Api::with_objects().await;
    let objects = api.layer_of_kind("objects").await;
    let image = api.layer_of_kind("image").await;
    for uri in [
        // no label layer by that name
        format!("/api/regions?labels=L99&objects={objects}"),
        // no object layer by that name
        format!("/api/regions?labels={image}&objects=L99"),
        // an object layer named as the labels holds no ids to count into
        format!("/api/regions?labels={objects}&objects={objects}"),
    ] {
        let res = api.get(&uri).await;
        assert_eq!(res.status, 404, "{uri}: {} {}", res.status, res.text());
        assert!(
            res.text()
                .contains("need a label layer and an object layer"),
            "{uri}: {}",
            res.text()
        );
    }
}

#[actix_web::test]
async fn a_regions_level_outside_the_label_layer_is_a_400() {
    let (api, labels, objects) = regions_fixture().await;
    let res = api
        .get(&format!(
            "/api/regions?labels={labels}&objects={objects}&level=42"
        ))
        .await;
    // The level is the caller's, and it is checked before a single plane is
    // read, so this is a 400 rather than a read failure halfway through.
    assert_eq!(res.status, 400, "{}", res.text());
    assert!(
        res.text().contains("outside the label layer"),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn regions_with_a_missing_layer_name_is_a_400() {
    let (api, labels, objects) = regions_fixture().await;
    for uri in [
        "/api/regions".to_string(),
        format!("/api/regions?labels={labels}"),
        format!("/api/regions?objects={objects}"),
        format!("/api/regions?labels={labels}&objects={objects}&level=deep"),
    ] {
        let res = api.get(&uri).await;
        assert_eq!(res.status, 400, "{uri}: {} {}", res.status, res.text());
    }
}
