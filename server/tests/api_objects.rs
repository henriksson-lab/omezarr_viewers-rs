//! The object routes, over HTTP: `/api/objects` and `/api/objects/at`.
//!
//! The claim under test is the one the frontend's "showing N of M" line rests
//! on: a capped read reports the total it decimated *from*, and the rows it
//! keeps are a fixed stride over the canonical order — the same query twice is
//! the same picture, not a fresh sample. The rest pins the wire format a client
//! has to decode by hand, and that a bad request is a 4xx rather than a 500.

mod api_harness;

use api_harness::{Api, Res, SHAPE};
use omezarr_viewer_server::synthetic;

/// The fixture's rows: `Api::with_objects` writes `cells.csv` from exactly
/// these blobs, so every count and position below is arithmetic, not a re-read.
fn blobs() -> Vec<synthetic::Blob> {
    synthetic::blobs(SHAPE, 3)
}

/// A rectangle covering the whole image, so nothing is excluded by geometry.
fn whole_world(layer: &str) -> String {
    format!(
        "/api/objects?layer={layer}&y0=0&y1={h}&x0=0&x1={w}",
        h = SHAPE.1,
        w = SHAPE.2
    )
}

/// The packed `OBJS` buffer, taken apart the way the client does.
struct Objs {
    positions: Vec<[f32; 3]>,
    rows: Vec<u32>,
    /// One `f32` per row per requested column, in the order they were asked for.
    values: Vec<Vec<f32>>,
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn f32_at(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

/// Decode a response body, checking the framing on the way through: a client
/// that cannot find the magic and the version has no way to read the rest.
fn decode(res: &Res) -> Objs {
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let bytes = &res.body;
    assert!(
        bytes.len() >= 16,
        "a header is 16 bytes, got {}",
        bytes.len()
    );
    assert_eq!(&bytes[..4], b"OBJS", "magic");
    assert_eq!(u32_at(bytes, 4), 1, "version");
    let count = u32_at(bytes, 8) as usize;
    let columns = u32_at(bytes, 12) as usize;

    let expected = 16 + count * 16 + columns * count * 4;
    assert_eq!(
        bytes.len(),
        expected,
        "{count} row(s) and {columns} column(s) do not account for the body"
    );

    let positions = (0..count)
        .map(|i| {
            let at = 16 + i * 12;
            [
                f32_at(bytes, at),
                f32_at(bytes, at + 4),
                f32_at(bytes, at + 8),
            ]
        })
        .collect();
    let rows_at = 16 + count * 12;
    let rows = (0..count).map(|i| u32_at(bytes, rows_at + i * 4)).collect();
    let values_at = 16 + count * 16;
    let values = (0..columns)
        .map(|c| {
            (0..count)
                .map(|i| f32_at(bytes, values_at + (c * count + i) * 4))
                .collect()
        })
        .collect();

    Objs {
        positions,
        rows,
        values,
    }
}

/// A header as a number, so a missing header fails loudly rather than as `0`.
fn count_header(res: &Res, name: &str) -> usize {
    res.header(name)
        .unwrap_or_else(|| panic!("no {name} header on {} {}", res.status, res.text()))
        .parse()
        .unwrap_or_else(|e| panic!("{name} is not a number: {e}"))
}

// -- the wire format ---------------------------------------------------------

#[actix_web::test]
async fn a_read_carries_a_packed_buffer_a_client_can_take_apart() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let res = api.get(&whole_world(&layer)).await;

    assert!(res.is_ok(), "{} {}", res.status, res.text());
    // The body is bytes, not JSON: a client reads it with a DataView, and a
    // content type that says otherwise is what makes it try `.json()` instead.
    assert_eq!(
        res.header("content-type").as_deref(),
        Some("application/octet-stream")
    );

    let objs = decode(&res);
    assert_eq!(objs.rows.len(), blobs().len(), "one row per blob");
    // Positions are world coordinates, z/y/x in that order — the same order
    // `world_position` returns, and the order the client's shader expects.
    let first = objs.positions[0];
    let blob = blobs()[0];
    assert!(
        (first[0] - blob.z as f32).abs() < 0.01
            && (first[1] - blob.y as f32).abs() < 0.01
            && (first[2] - blob.x as f32).abs() < 0.01,
        "row 0 is at {first:?}, not {:?}",
        [blob.z, blob.y, blob.x]
    );
}

#[actix_web::test]
async fn columns_come_back_as_f32_in_the_order_they_were_asked_for() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // Deliberately not the schema's order: the client indexes the buffer by
    // the position it asked for, so a server that reordered would mislabel
    // every point it coloured.
    let res = api
        .get(&format!("{}&columns=intensity,size", whole_world(&layer)))
        .await;
    let objs = decode(&res);
    assert_eq!(objs.values.len(), 2, "two columns were requested");

    let blobs = blobs();
    for (i, &row) in objs.rows.iter().enumerate() {
        let blob = blobs[row as usize];
        let size = (4.0 / 3.0 * std::f64::consts::PI * blob.radius.powi(3)).round() as f32;
        assert!(
            (objs.values[0][i] - blob.intensity as f32).abs() < 1e-3,
            "row {row} intensity is {}, not {}",
            objs.values[0][i],
            blob.intensity
        );
        assert!(
            (objs.values[1][i] - size).abs() < 1.0,
            "row {row} size is {}, not {size}",
            objs.values[1][i]
        );
    }
}

#[actix_web::test]
async fn a_column_name_nothing_matches_is_refused_rather_than_dropped() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let res = api
        .get(&format!(
            "{}&columns=size,nosuchcolumn",
            whole_world(&layer)
        ))
        .await;
    // The unknown name used to be skipped, which left one plane where two were
    // asked for — and the client indexes planes *positionally*, so every column
    // after the typo arrived under the wrong label. A name that matches nothing
    // is a caller's value out of range, so it is refused before anything is
    // encoded, and the refusal names both the miss and what is on offer.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("nosuchcolumn"), "{}", res.text());
    assert!(
        res.text().contains("size"),
        "the refusal lists the columns there are: {}",
        res.text()
    );
}

#[actix_web::test]
async fn one_bad_name_refuses_the_whole_request_rather_than_the_columns_around_it() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // Nothing partial: a buffer holding the good columns would still be read
    // positionally, which is the failure the refusal exists to prevent.
    let res = api
        .get(&format!("{}&columns=nope,size", whole_world(&layer)))
        .await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        !res.body.starts_with(b"OBJS"),
        "no partial buffer came back: {}",
        res.text()
    );
}

#[actix_web::test]
async fn a_trailing_comma_names_nothing_and_is_not_an_error() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // An empty segment adds no plane, so it shifts nothing and the client that
    // wrote it was not counting one. Refusing it would turn a stray comma into
    // a failed read for no gain.
    let res = api
        .get(&format!("{}&columns=size,", whole_world(&layer)))
        .await;
    let objs = decode(&res);
    assert_eq!(objs.values.len(), 1, "{}", res.status);
}

// -- decimation: the headline claim ------------------------------------------

#[actix_web::test]
async fn an_uncapped_read_returns_every_matching_row() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let total = blobs().len();

    for uri in [
        whole_world(&layer),
        format!("{}&max=0", whole_world(&layer)),
    ] {
        // `max=0` is the documented "no cap", not a request for no rows.
        let res = api.get(&uri).await;
        let objs = decode(&res);
        assert_eq!(count_header(&res, "X-Total"), total, "{uri}");
        assert_eq!(count_header(&res, "X-Returned"), total, "{uri}");
        assert_eq!(res.header("X-Truncated").as_deref(), Some("false"), "{uri}");
        assert_eq!(objs.rows.len(), total, "{uri}");
    }
}

#[actix_web::test]
async fn a_capped_read_reports_the_total_it_decimated_from() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let total = blobs().len();
    assert!(total > 10, "the fixture must exceed the cap to decimate");

    let res = api.get(&format!("{}&max=10", whole_world(&layer))).await;
    let objs = decode(&res);

    // This is the whole point: X-Total is what MATCHED, not what came back,
    // so the client can say "showing N of M" instead of showing a decimated
    // set as if it were everything.
    assert_eq!(
        count_header(&res, "X-Total"),
        total,
        "X-Total must be the full match count, not the returned count: {}",
        res.text().len()
    );
    assert_eq!(count_header(&res, "X-Returned"), objs.rows.len());
    assert!(objs.rows.len() <= 10, "the cap was {}", objs.rows.len());
    assert!(objs.rows.len() < total, "something must have been dropped");
    assert_eq!(
        res.header("X-Truncated").as_deref(),
        Some("true"),
        "a client that only looks at one header must still be warned"
    );
}

#[actix_web::test]
async fn a_cap_keeps_a_fixed_stride_over_the_canonical_order() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let total = blobs().len();
    let max = 10usize;

    let res = api.get(&format!("{}&max={max}", whole_world(&layer))).await;
    let objs = decode(&res);

    // Not "some ten rows": every stride-th row of the sorted row order. A
    // random sample would pass a count assertion and still make the picture
    // flicker as the client panned.
    let stride = total.div_ceil(max);
    let expected: Vec<u32> = (0..total as u32).step_by(stride).collect();
    assert_eq!(
        objs.rows, expected,
        "decimation must stride, got {:?}",
        objs.rows
    );
}

#[actix_web::test]
async fn the_same_capped_request_twice_returns_the_same_rows() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let uri = format!("{}&max=7&columns=size", whole_world(&layer));

    let first = api.get(&uri).await;
    let second = api.get(&uri).await;
    assert!(
        first.is_ok() && second.is_ok(),
        "{} {}",
        first.status,
        second.status
    );

    // Byte-for-byte, not just row-for-row: the positions and column values
    // that go with a row have to be the same ones too.
    assert_eq!(
        first.body,
        second.body,
        "a repeated query returned a different {} vs {} byte body",
        first.body.len(),
        second.body.len()
    );
    assert_eq!(
        decode(&first).rows,
        decode(&second).rows,
        "decimation is sampled, not strided"
    );
}

#[actix_web::test]
async fn a_cap_larger_than_the_match_leaves_it_alone() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let total = blobs().len();

    let res = api
        .get(&format!("{}&max={}", whole_world(&layer), total * 10))
        .await;
    let objs = decode(&res);
    assert_eq!(objs.rows.len(), total);
    // Nothing was dropped, so nothing should claim it was.
    assert_eq!(res.header("X-Truncated").as_deref(), Some("false"));
}

// -- the region query --------------------------------------------------------

#[actix_web::test]
async fn a_rectangle_excludes_the_rows_outside_it() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // The top-left quadrant. The blobs sit on a 3x3 grid at 1/6, 1/2 and 5/6
    // of each axis, so this holds exactly the one column-and-row nearest the
    // origin, at every z.
    let half = SHAPE.1 as f32 / 2.0;
    let res = api
        .get(&format!(
            "/api/objects?layer={layer}&y0=0&y1={half}&x0=0&x1={half}"
        ))
        .await;
    let objs = decode(&res);

    let total = count_header(&res, "X-Total");
    assert!(
        total > 0 && total < blobs().len(),
        "the quadrant should hold some but not all of {} rows, got {total}",
        blobs().len()
    );
    for p in &objs.positions {
        assert!(
            p[1] >= 0.0 && p[1] <= half && p[2] >= 0.0 && p[2] <= half,
            "{p:?} is outside the rectangle that was asked for"
        );
    }
}

#[actix_web::test]
async fn a_z_slab_selects_only_the_planes_it_names() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // The fixture's blobs sit on four z planes; this slab covers the first.
    let first_z = blobs()[0].z as f32;
    let res = api
        .get(&format!(
            "{}&z0={}&z1={}",
            whole_world(&layer),
            first_z - 0.5,
            first_z + 0.5
        ))
        .await;
    let objs = decode(&res);

    assert!(!objs.rows.is_empty(), "the slab holds the first plane");
    assert!(
        objs.rows.len() < blobs().len(),
        "a slab that keeps everything is not filtering z"
    );
    for p in &objs.positions {
        assert!(
            (p[0] - first_z).abs() <= 0.5,
            "{p:?} is not on the plane that was asked for"
        );
    }
}

#[actix_web::test]
async fn an_empty_region_is_an_empty_buffer_not_an_error() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // Panning off the data is normal; it must not look like a failure, or the
    // client shows an error where it should show nothing.
    let res = api
        .get(&format!(
            "/api/objects?layer={layer}&y0=-100&y1=-50&x0=-100&x1=-50"
        ))
        .await;
    let objs = decode(&res);
    assert!(objs.rows.is_empty(), "{:?}", objs.rows);
    assert_eq!(count_header(&res, "X-Total"), 0);
    assert_eq!(res.header("X-Truncated").as_deref(), Some("false"));
}

// -- bad requests ------------------------------------------------------------

#[actix_web::test]
async fn an_unknown_layer_id_is_not_found() {
    let api = Api::with_objects().await;
    let res = api
        .get("/api/objects?layer=L99&y0=0&y1=10&x0=0&x1=10")
        .await;
    assert_eq!(res.status, 404, "{} {}", res.status, res.text());
}

#[actix_web::test]
async fn asking_an_image_layer_for_objects_is_a_bad_request_not_a_missing_one() {
    let api = Api::with_objects().await;
    let image = api.layer_of_kind("image").await;
    // The layer exists but carries no rows. That is the client asking the wrong
    // layer, which is a different thing from naming one that is not there — and
    // both used to be the same 404, so nothing downstream could tell them apart.
    let res = api.get(&whole_world(&image)).await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        res.text().contains(&image) && res.text().contains("holds no objects"),
        "the body should name the layer and what it lacks: {}",
        res.text()
    );
}

#[actix_web::test]
async fn a_malformed_number_is_refused_before_any_store_is_read() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    for bad in [
        format!("/api/objects?layer={layer}&y0=nope&y1=10&x0=0&x1=10"),
        format!("/api/objects?layer={layer}&y0=0&y1=10&x0=0&x1=10&max=-1"),
        format!("/api/objects?layer={layer}&y0=0&y1=10&x0=0&x1=10&z0=x"),
    ] {
        let res = api.get(&bad).await;
        assert_eq!(res.status, 400, "{bad}: {} {}", res.status, res.text());
    }
}

#[actix_web::test]
async fn a_request_missing_the_rectangle_is_refused() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // Every bound is required: defaulting a missing one would silently query
    // a rectangle nobody asked for.
    for bad in [
        format!("/api/objects?layer={layer}"),
        format!("/api/objects?layer={layer}&y0=0&y1=10&x0=0"),
        "/api/objects?y0=0&y1=10&x0=0&x1=10".to_string(),
    ] {
        let res = api.get(&bad).await;
        assert_eq!(res.status, 400, "{bad}: {} {}", res.status, res.text());
    }
}

// -- /api/objects/at ---------------------------------------------------------

#[actix_web::test]
async fn a_click_on_a_row_returns_its_exact_values() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let blob = blobs()[0];
    let res = api
        .get(&format!(
            "/api/objects/at?layer={layer}&z={}&y={}&x={}&r=2",
            blob.z, blob.y, blob.x
        ))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let row = res.json();
    assert!(!row.is_null(), "a click on a blob found nothing: {row}");

    assert_eq!(row["row"], 0, "the nearest row is the one clicked: {row}");
    assert!(
        (row["y"].as_f64().expect("y") - blob.y).abs() < 0.01
            && (row["x"].as_f64().expect("x") - blob.x).abs() < 0.01,
        "{row} is not at {:?}",
        [blob.y, blob.x]
    );
    // The inspector's whole reason to exist: exact values, in their own type,
    // not the f32 the packed buffer carries.
    assert_eq!(
        row["columns"]["id"], blob.id,
        "an id must stay an integer: {row}"
    );
    assert!(
        (row["columns"]["intensity"].as_f64().expect("intensity") - blob.intensity).abs() < 1e-3,
        "{row}"
    );
}

#[actix_web::test]
async fn a_click_on_nothing_is_a_null_body_not_a_404() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    // Clicking empty canvas is the common case, and a 404 for it would put an
    // error in the client's log on every miss.
    let res = api
        .get(&format!("/api/objects/at?layer={layer}&z=0&y=0&x=0&r=1"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert!(
        res.json().is_null(),
        "a miss should be null, got {}",
        res.text()
    );
}

#[actix_web::test]
async fn the_search_radius_decides_what_counts_as_a_hit() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let blob = blobs()[0];
    // Eight pixels off a blob: inside a generous radius, outside a tight one.
    let near = format!(
        "/api/objects/at?layer={layer}&z={}&y={}&x={}",
        blob.z,
        blob.y + 8.0,
        blob.x
    );

    let tight = api.get(&format!("{near}&r=2")).await;
    assert!(tight.is_ok(), "{} {}", tight.status, tight.text());
    assert!(tight.json().is_null(), "r=2 should miss: {}", tight.text());

    let loose = api.get(&format!("{near}&r=20")).await;
    assert!(loose.is_ok(), "{} {}", loose.status, loose.text());
    assert_eq!(loose.json()["row"], 0, "r=20 should hit: {}", loose.text());
}

#[actix_web::test]
async fn a_click_needs_no_radius_of_its_own() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    let blob = blobs()[0];
    // `r` and `z` both default, so an older client that sends neither still
    // gets an answer rather than a 400.
    let res = api
        .get(&format!(
            "/api/objects/at?layer={layer}&y={}&x={}",
            blob.y, blob.x
        ))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert!(!res.json().is_null(), "the default radius found nothing");
}

#[actix_web::test]
async fn a_click_tells_an_unknown_layer_from_one_that_holds_no_rows() {
    let api = Api::with_objects().await;
    let image = api.layer_of_kind("image").await;

    let unknown = api.get("/api/objects/at?layer=L99&z=0&y=1&x=1").await;
    assert_eq!(unknown.status, 404, "{} {}", unknown.status, unknown.text());
    assert!(unknown.text().contains("L99"), "{}", unknown.text());

    let no_rows = api
        .get(&format!("/api/objects/at?layer={image}&z=0&y=1&x=1"))
        .await;
    assert_eq!(no_rows.status, 400, "{} {}", no_rows.status, no_rows.text());
    assert!(
        no_rows.text().contains("holds no objects"),
        "{}",
        no_rows.text()
    );
}

#[actix_web::test]
async fn a_click_with_a_malformed_coordinate_is_refused() {
    let api = Api::with_objects().await;
    let layer = api.layer_of_kind("objects").await;
    for bad in [
        format!("/api/objects/at?layer={layer}&y=nope&x=1"),
        format!("/api/objects/at?layer={layer}&y=1"),
    ] {
        let res = api.get(&bad).await;
        assert_eq!(res.status, 400, "{bad}: {} {}", res.status, res.text());
    }
}
