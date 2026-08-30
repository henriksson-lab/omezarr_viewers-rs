//! The annotation routes, over HTTP — the only part of the API that writes.
//!
//! The claim: a shape drawn in the viewer round-trips through POST/GET/PUT/
//! DELETE with a server-assigned id, the hierarchy behaves the way QuPath's
//! does (deleting a parent *lifts* its children), a save writes whichever of
//! the two on-disk forms the target's shape names and says what a table
//! flattened — and a save to a remote store is refused unless the operator
//! started the server with `--allow-remote-writes`. That last one is a
//! security control, and it is the reason this file exists.

mod api_harness;

use actix_web::http::StatusCode;
use api_harness::Api;
use omezarr_viewer_common::{Annotation, Geometry, Plane};
use serde_json::{json, Value};

// -- fixtures ---------------------------------------------------------------

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Annotation {
    Annotation::rect(x0, y0, x1, y1, Plane::default())
}

fn point(x: f64, y: f64) -> Annotation {
    Annotation::point(x, y, Plane::default())
}

/// A polygon that is not its own bounding box, and so cannot survive an ROI
/// table: this is what `flattened` counts.
fn triangle() -> Annotation {
    Annotation {
        geometry: Geometry::Polygon(vec![vec![
            [0.0, 0.0],
            [20.0, 0.0],
            [10.0, 20.0],
            [0.0, 0.0],
        ]]),
        ..Default::default()
    }
}

fn body(annotation: &Annotation) -> Value {
    serde_json::to_value(annotation).expect("an annotation as JSON")
}

/// An `Api::image_writable` with an annotation layer on it, since the harness
/// only offers the pairing for a default server.
async fn writable_with_annotations() -> Api {
    let api = Api::image_writable().await;
    let res = api
        .post("/api/annotations/layers", json!({"name": "drawn"}))
        .await;
    assert!(res.is_ok(), "new layer: {} {}", res.status, res.text());
    api
}

/// POST one annotation and return the id the server gave it.
async fn add(api: &Api, layer: &str, annotation: &Annotation) -> u64 {
    let res = api
        .post(&format!("/api/annotations/{layer}"), body(annotation))
        .await;
    assert!(res.is_ok(), "add: {} {}", res.status, res.text());
    res.json()["id"]
        .as_u64()
        .unwrap_or_else(|| panic!("no server-assigned id in {}", res.text()))
}

/// Every row of a layer, as the GET reports them.
async fn rows(api: &Api, layer: &str) -> Vec<Value> {
    let res = api.get(&format!("/api/annotations/{layer}")).await;
    assert!(res.is_ok(), "list: {} {}", res.status, res.text());
    res.json()
        .as_array()
        .unwrap_or_else(|| panic!("expected a list, got {}", res.text()))
        .clone()
}

fn row(rows: &[Value], id: u64) -> &Value {
    rows.iter()
        .find(|r| r["id"].as_u64() == Some(id))
        .unwrap_or_else(|| panic!("no annotation {id} in {rows:?}"))
}

/// The `target` an annotation layer reports it would save to.
async fn recorded_target(api: &Api, layer: &str) -> Value {
    let session = api.get("/api/session").await.json();
    session["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .find(|l| l["id"] == layer)
        .unwrap_or_else(|| panic!("no layer {layer} in the session"))["kind"]["target"]
        .clone()
}

// -- the remote-writes gate -------------------------------------------------
//
// Credentials handed to a viewer so it can *read* a bucket must not silently
// become write access to it. The gate is one `if`, so it is exactly the kind of
// line a refactor drops without anything noticing.

/// Every remote scheme, in both save formats: the gate is upstream of the
/// GeoJSON/ROI-table split, and a hole in either half is the same hole.
const REMOTE_TARGETS: [&str; 6] = [
    "s3://bucket/img.zarr/annotations/hand",
    "s3://bucket/img.zarr/tables/hand",
    "http://127.0.0.1:9/img.zarr/annotations/hand",
    "http://127.0.0.1:9/img.zarr/tables/hand",
    "https://example.invalid/img.zarr/annotations/hand",
    "https://example.invalid/img.zarr/tables/hand",
];

#[actix_web::test]
async fn a_remote_save_is_refused_unless_the_operator_allowed_it() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &rect(0.0, 0.0, 10.0, 10.0)).await;

    for target in REMOTE_TARGETS {
        let res = api
            .post(
                &format!("/api/annotations/{layer}/save"),
                json!({ "target": target }),
            )
            .await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "{target} was not refused: {} {}",
            res.status,
            res.text()
        );
        assert!(
            res.text().contains("--allow-remote-writes"),
            "the refusal must say how to lift it, got {}",
            res.text()
        );
    }

    // A refused save is a save that did not happen: it must not leave the layer
    // pointing at the bucket, or the next targetless save would try again.
    assert_eq!(
        recorded_target(&api, &layer).await,
        Value::Null,
        "a refused save recorded a target"
    );
}

#[actix_web::test]
async fn allowing_remote_writes_lets_a_remote_save_reach_the_writer() {
    // The other half of the gate: with the flag on, the request gets past the
    // check and fails (or succeeds) on its own merits. These targets have no S3
    // profile and no reachable host, so what is asserted is *not 403* and not
    // the refusal text — that the policy, and only the policy, changed.
    let api = writable_with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &rect(0.0, 0.0, 10.0, 10.0)).await;

    for target in REMOTE_TARGETS {
        let res = api
            .post(
                &format!("/api/annotations/{layer}/save"),
                json!({ "target": target }),
            )
            .await;
        assert_ne!(
            res.status,
            StatusCode::FORBIDDEN,
            "{target} was refused on a server started with --allow-remote-writes: {}",
            res.text()
        );
        assert!(
            !res.text().contains("--allow-remote-writes"),
            "{target} got the refusal body with the flag on: {}",
            res.text()
        );
        // Evidence that it is the *writer* that failed and not the policy: an
        // unconfigured profile and an unwritable http store are both errors
        // raised past the gate, on the way to the store.
        let text = res.text();
        assert!(
            text.contains("S3 profile") || text.contains("not supported"),
            "{target} did not reach the writer: {} {text}",
            res.status
        );
    }
}

#[actix_web::test]
async fn whitespace_does_not_smuggle_a_remote_target_past_the_gate() {
    // `is_remote` trims before it matches, and so must every path that decides
    // what the target is: a leading space that made the check say "local" and
    // the writer say "s3" would be the whole exploit.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &point(1.0, 1.0)).await;

    for target in [
        "  s3://bucket/img.zarr/tables/hand",
        " https://h/x.zarr/annotations/a",
    ] {
        let res = api
            .post(
                &format!("/api/annotations/{layer}/save"),
                json!({ "target": target }),
            )
            .await;
        assert_eq!(
            res.status,
            StatusCode::FORBIDDEN,
            "`{target}` was not refused: {} {}",
            res.status,
            res.text()
        );
    }
}

#[actix_web::test]
async fn a_local_save_works_on_a_server_that_forbids_remote_writes() {
    // The gate is about *where*, not about saving: a viewer with the flag off
    // still writes to its own disk, and a gate that broke that would be found
    // only by somebody trying to save their work.
    let strict = Api::with_annotations().await;
    let permissive = writable_with_annotations().await;

    for api in [&strict, &permissive] {
        let layer = api.layer_of_kind("annotations").await;
        add(api, &layer, &rect(0.0, 0.0, 10.0, 10.0)).await;
        let target = format!("{}/annotations/hand", api.store.display());
        let res = api
            .post(
                &format!("/api/annotations/{layer}/save"),
                json!({ "target": target }),
            )
            .await;
        assert!(
            res.is_ok(),
            "local save (allow_remote_writes={}): {} {}",
            api.state.allow_remote_writes,
            res.status,
            res.text()
        );
        assert!(
            api.store
                .join("annotations/hand/annotations.geojson")
                .is_file(),
            "nothing was written under {}",
            api.store.display()
        );
    }
}

// -- the CRUD round trip ----------------------------------------------------

#[actix_web::test]
async fn an_annotation_survives_post_get_put_and_delete() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    assert!(rows(&api, &layer).await.is_empty(), "a new layer is empty");

    // POST: the id is the server's to assign, not the client's.
    let sent = Annotation {
        id: 999,
        label: "region".into(),
        ..rect(10.0, 20.0, 30.0, 40.0)
    };
    let created = api
        .post(&format!("/api/annotations/{layer}"), body(&sent))
        .await;
    assert!(created.is_ok(), "{} {}", created.status, created.text());
    let id = created.json()["id"].as_u64().expect("an id");
    assert_eq!(
        id, 1,
        "the first annotation of a layer is 1, not the client's 999"
    );

    // GET: it is there, with what was sent on it.
    let listed = rows(&api, &layer).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(row(&listed, id)["label"], "region");
    assert_eq!(row(&listed, id)["geometry"]["type"], "Polygon");

    // PUT: the geometry and class change, the id does not.
    let edited = Annotation {
        label: "tumour".into(),
        ..point(55.0, 66.0)
    };
    let res = api
        .put(&format!("/api/annotations/{layer}/{id}"), body(&edited))
        .await;
    assert!(res.is_ok(), "update: {} {}", res.status, res.text());
    assert_eq!(
        res.json()["id"].as_u64(),
        Some(id),
        "an update keeps the id"
    );
    let listed = rows(&api, &layer).await;
    assert_eq!(listed.len(), 1, "an update is not an insert: {listed:?}");
    assert_eq!(row(&listed, id)["label"], "tumour");
    assert_eq!(
        row(&listed, id)["geometry"],
        json!({"type": "Point", "coordinates": [55.0, 66.0]})
    );

    // DELETE: gone, and reported by the id it removed.
    let res = api.delete(&format!("/api/annotations/{layer}/{id}")).await;
    assert!(res.is_ok(), "delete: {} {}", res.status, res.text());
    assert_eq!(res.json()["removed"].as_u64(), Some(id));
    assert!(rows(&api, &layer).await.is_empty(), "it is still there");
}

#[actix_web::test]
async fn a_shape_drawn_inside_another_becomes_its_child() {
    // The hierarchy is spatial and nobody says so: this is what `add_nested`
    // buys over a plain append, and it is only visible through the POST.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;

    let outer = add(&api, &layer, &rect(0.0, 0.0, 100.0, 100.0)).await;
    let middle = add(&api, &layer, &rect(10.0, 10.0, 50.0, 50.0)).await;
    let inner = add(&api, &layer, &point(20.0, 20.0)).await;
    let outside = add(&api, &layer, &point(400.0, 400.0)).await;

    let listed = rows(&api, &layer).await;
    assert_eq!(row(&listed, outer)["parent"], Value::Null);
    assert_eq!(row(&listed, middle)["parent"].as_u64(), Some(outer));
    assert_eq!(
        row(&listed, inner)["parent"].as_u64(),
        Some(middle),
        "the smallest covering shape wins, not the first"
    );
    assert_eq!(row(&listed, outside)["parent"], Value::Null);
}

#[actix_web::test]
async fn deleting_a_parent_lifts_its_children_rather_than_deleting_them() {
    // Deleting a region must not silently delete every cell inside it; the
    // children keep existing, one level further out.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;

    let outer = add(&api, &layer, &rect(0.0, 0.0, 100.0, 100.0)).await;
    let middle = add(&api, &layer, &rect(10.0, 10.0, 50.0, 50.0)).await;
    let inner = add(&api, &layer, &point(20.0, 20.0)).await;

    let res = api
        .delete(&format!("/api/annotations/{layer}/{middle}"))
        .await;
    assert!(res.is_ok(), "delete: {} {}", res.status, res.text());

    let listed = rows(&api, &layer).await;
    assert_eq!(
        listed.len(),
        2,
        "the child was deleted with its parent: {listed:?}"
    );
    assert_eq!(
        row(&listed, inner)["parent"].as_u64(),
        Some(outer),
        "the child inherits its grandparent, it does not keep a dangling link"
    );

    // And a top-level parent's children become top-level, not orphans pointing
    // at an id nothing has.
    let res = api
        .delete(&format!("/api/annotations/{layer}/{outer}"))
        .await;
    assert!(res.is_ok(), "delete: {} {}", res.status, res.text());
    let listed = rows(&api, &layer).await;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(row(&listed, inner)["parent"], Value::Null);
}

#[actix_web::test]
async fn renest_rebuilds_a_hierarchy_that_editing_made_wrong() {
    // An update deliberately keeps the stored parent — a client editing a shape
    // must not re-parent it by omission — so moving a shape into a region does
    // *not* nest it. Renest is the offered, explicit correction.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;

    let outer = add(&api, &layer, &rect(0.0, 0.0, 100.0, 100.0)).await;
    let stray = add(&api, &layer, &point(400.0, 400.0)).await;

    let res = api
        .put(
            &format!("/api/annotations/{layer}/{stray}"),
            body(&point(20.0, 20.0)),
        )
        .await;
    assert!(res.is_ok(), "update: {} {}", res.status, res.text());
    assert_eq!(
        row(&rows(&api, &layer).await, stray)["parent"],
        Value::Null,
        "an edit must not re-nest under the pointer"
    );

    let res = api
        .post_empty(&format!("/api/annotations/{layer}/renest"))
        .await;
    assert!(res.is_ok(), "renest: {} {}", res.status, res.text());
    let listed = res.json();
    let listed = listed.as_array().expect("renest answers with the rows");
    assert_eq!(
        row(listed, stray)["parent"].as_u64(),
        Some(outer),
        "renest did not pick the region up"
    );
    // Answering with the rows is the contract: the client redraws from this.
    assert_eq!(listed.len(), 2, "{listed:?}");
}

#[actix_web::test]
async fn detach_makes_a_child_top_level_and_says_so_twice() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    let outer = add(&api, &layer, &rect(0.0, 0.0, 100.0, 100.0)).await;
    let inner = add(&api, &layer, &point(20.0, 20.0)).await;
    assert_eq!(
        row(&rows(&api, &layer).await, inner)["parent"].as_u64(),
        Some(outer)
    );

    let res = api
        .post_empty(&format!("/api/annotations/{layer}/{inner}/detach"))
        .await;
    assert!(res.is_ok(), "detach: {} {}", res.status, res.text());
    assert_eq!(
        row(res.json().as_array().expect("rows"), inner)["parent"],
        Value::Null
    );

    // Detaching something already top-level is not an error: the caller asked
    // for it to have no parent, and it has none.
    let res = api
        .post_empty(&format!("/api/annotations/{layer}/{inner}/detach"))
        .await;
    assert!(res.is_ok(), "second detach: {} {}", res.status, res.text());
    assert_eq!(
        rows(&api, &layer).await.len(),
        2,
        "detach deleted something"
    );
}

// -- layers -----------------------------------------------------------------

#[actix_web::test]
async fn a_new_annotation_layer_is_appended_and_answers_with_the_session() {
    // `layers` is a literal segment that `/{layer}` would also match, so route
    // order is behaviour: this must create a layer, not add an annotation to a
    // layer called "layers".
    let api = Api::image().await;
    let before = api.layer_ids().await.len();

    let res = api
        .post("/api/annotations/layers", json!({"name": "drawn"}))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let session = res.json();
    let layers = session["layers"].as_array().expect("the session's layers");
    assert_eq!(layers.len(), before + 1, "no layer was appended: {session}");
    assert_eq!(
        layers.last().expect("the new layer")["kind"]["kind"],
        "annotations"
    );
    assert_eq!(layers.last().expect("the new layer")["name"], "drawn");

    // A blank name is no name: the layer still opens, unnamed.
    let res = api
        .post("/api/annotations/layers", json!({"name": "  "}))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(
        res.json()["layers"].as_array().map(Vec::len),
        Some(before + 2)
    );
    let res = api.post("/api/annotations/layers", json!({})).await;
    assert!(
        res.is_ok(),
        "a nameless layer: {} {}",
        res.status,
        res.text()
    );
}

// -- saving -----------------------------------------------------------------

#[actix_web::test]
async fn a_geojson_save_writes_the_set_and_remembers_where() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &rect(10.0, 10.0, 20.0, 20.0)).await;
    add(&api, &layer, &triangle()).await;

    let target = format!("{}/annotations/hand", api.store.display());
    let res = api
        .post(
            &format!("/api/annotations/{layer}/save"),
            json!({ "target": target }),
        )
        .await;
    assert!(res.is_ok(), "save: {} {}", res.status, res.text());
    let report = res.json();
    assert_eq!(report["format"], "geojson");
    assert_eq!(report["rows"].as_u64(), Some(2));
    assert_eq!(
        report["flattened"].as_u64(),
        Some(0),
        "GeoJSON is the lossless form; nothing is flattened"
    );
    let written = api.store.join("annotations/hand/annotations.geojson");
    assert!(written.is_file(), "{} was not written", written.display());
    let text = std::fs::read_to_string(&written).expect("read back");
    assert!(text.contains("FeatureCollection"), "not GeoJSON: {text}");

    // The target is remembered, which is what lets a re-save take no argument.
    let recorded = recorded_target(&api, &layer).await;
    assert_eq!(
        recorded, report["target"],
        "the layer forgot where it saved"
    );
    let again = api
        .post(&format!("/api/annotations/{layer}/save"), json!({}))
        .await;
    assert!(again.is_ok(), "re-save: {} {}", again.status, again.text());
    assert_eq!(again.json()["target"], report["target"]);
}

#[actix_web::test]
async fn a_table_save_reports_what_it_had_to_flatten() {
    // An ROI table holds axis-aligned boxes and nothing else. A save that would
    // flatten a polygon says how many, rather than doing it quietly and letting
    // the user find out on the round trip.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &point(5.0, 5.0)).await;
    add(&api, &layer, &rect(10.0, 10.0, 20.0, 20.0)).await;
    add(&api, &layer, &triangle()).await;

    let target = format!("{}/tables/hand", api.store.display());
    let res = api
        .post(
            &format!("/api/annotations/{layer}/save"),
            json!({ "target": target, "voxel": [2.0, 0.5, 0.5] }),
        )
        .await;
    assert!(res.is_ok(), "save: {} {}", res.status, res.text());
    let report = res.json();
    assert_eq!(report["format"], "roi_table");
    assert_eq!(report["rows"].as_u64(), Some(3));
    assert_eq!(
        report["flattened"].as_u64(),
        Some(1),
        "only the triangle is not its own bounding box: {report}"
    );
    assert_eq!(
        report["voxel"],
        json!([2.0, 0.5, 0.5]),
        "the scale a save used is reported back, so it is recoverable"
    );
    assert!(
        api.store.join("tables/hand").is_dir(),
        "no table under {}",
        api.store.display()
    );
}

#[actix_web::test]
async fn a_save_with_nowhere_to_write_is_a_bad_request() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    add(&api, &layer, &point(1.0, 1.0)).await;

    for target in [json!({}), json!({"target": "   "})] {
        let res = api
            .post(&format!("/api/annotations/{layer}/save"), target.clone())
            .await;
        assert_eq!(
            res.status,
            StatusCode::BAD_REQUEST,
            "{target} : {} {}",
            res.status,
            res.text()
        );
        assert!(res.text().contains("no target"), "{}", res.text());
    }

    // A path that names neither an annotation set nor a table is refused too:
    // the format is decided by the target's shape, so a shapeless one has none.
    let res = api
        .post(
            &format!("/api/annotations/{layer}/save"),
            json!({ "target": format!("{}/hand", api.store.display()) }),
        )
        .await;
    assert_eq!(
        res.status,
        StatusCode::BAD_REQUEST,
        "{} {}",
        res.status,
        res.text()
    );
}

// -- what is not there ------------------------------------------------------

#[actix_web::test]
async fn every_route_answers_404_for_a_layer_that_does_not_exist() {
    let api = Api::with_annotations().await;

    let missing = "L99";
    let cases: Vec<(&str, api_harness::Res)> = vec![
        ("get", api.get(&format!("/api/annotations/{missing}")).await),
        (
            "add",
            api.post(
                &format!("/api/annotations/{missing}"),
                body(&point(1.0, 1.0)),
            )
            .await,
        ),
        (
            "update",
            api.put(
                &format!("/api/annotations/{missing}/1"),
                body(&point(1.0, 1.0)),
            )
            .await,
        ),
        (
            "delete",
            api.delete(&format!("/api/annotations/{missing}/1")).await,
        ),
        (
            "renest",
            api.post_empty(&format!("/api/annotations/{missing}/renest"))
                .await,
        ),
        (
            "detach",
            api.post_empty(&format!("/api/annotations/{missing}/1/detach"))
                .await,
        ),
        (
            "save",
            api.post(
                &format!("/api/annotations/{missing}/save"),
                json!({"target": "/tmp/x.zarr/tables/hand"}),
            )
            .await,
        ),
    ];
    for (route, res) in cases {
        assert_eq!(
            res.status,
            StatusCode::NOT_FOUND,
            "{route} on a missing layer: {} {}",
            res.status,
            res.text()
        );
        // The handler's 404, not the router's: an unmatched route answers with
        // an empty body, and would pass the status check while proving nothing.
        assert!(
            res.text().contains(missing),
            "{route} 404 came from routing, not from the handler: `{}`",
            res.text()
        );
    }
}

#[actix_web::test]
async fn an_image_layer_is_not_an_annotation_layer() {
    // The id resolves; the layer just holds no annotations. That has to be a
    // 404 rather than an empty list, or a client would draw into a picture.
    let api = Api::with_annotations().await;
    let image = api.layer_of_kind("image").await;

    let res = api.get(&format!("/api/annotations/{image}")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
    let res = api
        .post(&format!("/api/annotations/{image}"), body(&point(1.0, 1.0)))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "{}", res.text());
}

#[actix_web::test]
async fn an_edit_to_an_annotation_that_is_gone_is_404() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    let id = add(&api, &layer, &point(1.0, 1.0)).await;
    assert!(api
        .delete(&format!("/api/annotations/{layer}/{id}"))
        .await
        .is_ok());

    let res = api
        .put(
            &format!("/api/annotations/{layer}/{id}"),
            body(&point(2.0, 2.0)),
        )
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "update: {}", res.text());
    let res = api.delete(&format!("/api/annotations/{layer}/{id}")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND, "delete: {}", res.text());
}

#[actix_web::test]
async fn a_body_that_is_not_an_annotation_is_a_bad_request() {
    // The extractor is the whole validation, so it is worth pinning: a geometry
    // nothing can draw must be refused at the door, not stored and drawn later.
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;
    let id = add(&api, &layer, &point(1.0, 1.0)).await;

    let junk = [
        json!({}),                                                     // no geometry
        json!({"geometry": {"type": "Blob", "coordinates": [1, 2]}}),  // no such geometry
        json!({"geometry": {"type": "Point", "coordinates": "here"}}), // not coordinates
        json!({"geometry": {"type": "Point", "coordinates": [0, 0]}, "z_extent": -1}),
    ];
    for bad in junk {
        let res = api
            .post(&format!("/api/annotations/{layer}"), bad.clone())
            .await;
        assert_eq!(
            res.status,
            StatusCode::BAD_REQUEST,
            "POST {bad} was accepted: {} {}",
            res.status,
            res.text()
        );
        let res = api
            .put(&format!("/api/annotations/{layer}/{id}"), bad.clone())
            .await;
        assert_eq!(
            res.status,
            StatusCode::BAD_REQUEST,
            "PUT {bad} was accepted: {} {}",
            res.status,
            res.text()
        );
    }
    assert_eq!(rows(&api, &layer).await.len(), 1, "junk was stored");
}
