//! The session, layer and project routes, over HTTP.
//!
//! The claim is about the *contract*: the field names `/api/session`,
//! `/api/info`, `/api/stats` and `/api/project` promise the frontend, and the
//! status code each route answers with when it is asked for something that
//! is not there. A handler that returned the right JSON with the wrong status
//! is a handler the client's error handling never sees.

mod api_harness;

use api_harness::{write_image, Api};
use omezarr_viewer_server::session::LayerRole;
use omezarr_viewer_server::synthetic;
use serde_json::json;

// ---------------------------------------------------------------------------
// GET /api/session
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn the_session_lists_every_layer_with_the_fields_the_frontend_reads() {
    let api = Api::with_labels().await;
    let res = api.get("/api/session").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let layers = res.json()["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 2, "{}", res.text());
    for layer in &layers {
        // Every one of these is dereferenced by the layer panel without a
        // guard; a missing one is a blank row rather than an error.
        for field in ["id", "name", "source", "kind"] {
            assert!(layer.get(field).is_some(), "no `{field}` in {layer}");
        }
    }
    // Draw order is bottom-to-top and the image was opened first, so the label
    // layer must come second: reversing this hides the stain under its mask.
    assert_eq!(layers[0]["kind"]["kind"], "image");
    assert_eq!(layers[1]["kind"]["kind"], "labels");
    assert_eq!(layers[0]["id"], "L0");
    assert_eq!(layers[1]["id"], "L1");
}

// ---------------------------------------------------------------------------
// GET /api/info
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn info_is_the_first_image_layers_multiscale_metadata() {
    let api = Api::image().await;
    let res = api.get("/api/info").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let info = res.json();
    // The pre-session client reads `metadata.multiscales` and `arrays`; this
    // route exists only to keep answering it.
    assert!(
        info["metadata"]["multiscales"][0]["axes"].is_array(),
        "{}",
        res.text()
    );
    assert!(info["arrays"].is_array(), "{}", res.text());
    assert!(
        !info["arrays"].as_array().unwrap().is_empty(),
        "a store with no levels: {}",
        res.text()
    );
}

#[actix_web::test]
async fn info_without_a_dataset_is_a_not_found() {
    let api = Api::empty().await;
    let res = api.get("/api/info").await;
    assert_eq!(res.status, 404, "{} {}", res.status, res.text());
}

// ---------------------------------------------------------------------------
// GET /api/stats
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn stats_reports_the_cache_and_the_layer_count() {
    let api = Api::with_labels().await;
    let res = api.get("/api/stats").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let stats = res.json();
    for counter in ["entries", "bytes", "hits", "misses"] {
        assert!(
            stats["cache"][counter].is_u64(),
            "no numeric `cache.{counter}`: {}",
            res.text()
        );
    }
    // The layer count is what makes this route worth calling while a pane is
    // slow: a cache miss rate means nothing without knowing how many layers
    // are competing for it.
    assert_eq!(stats["layers"], 2, "{}", res.text());
}

// ---------------------------------------------------------------------------
// GET /api/datasets, POST /api/open
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn datasets_is_an_empty_list_when_no_bucket_is_configured() {
    let api = Api::empty().await;
    let res = api.get("/api/datasets").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    // An empty list, not a 404: the dataset picker asks unconditionally and
    // renders nothing when there is nothing to pick.
    assert_eq!(res.json(), json!([]), "{}", res.text());
}

#[actix_web::test]
async fn opening_a_dataset_without_an_s3_config_is_refused() {
    let api = Api::image().await;
    let res = api.post_empty("/api/open?dataset=some.zarr").await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    // The refusal must come *before* the session is cleared, or a click on a
    // picker that cannot work would close the layers already open.
    assert_eq!(api.layer_ids().await.len(), 1);
}

#[actix_web::test]
async fn open_without_a_dataset_name_is_rejected() {
    let api = Api::empty().await;
    let res = api.post_empty("/api/open").await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
}

// ---------------------------------------------------------------------------
// GET /api/project
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn the_project_records_each_layers_source_role_and_name() {
    let api = Api::with_labels().await;
    let res = api.get("/api/project").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let project = res.json();
    let layers = project["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 2, "{}", res.text());
    // Roles are written explicitly rather than left to auto-detection, so that
    // reopening the file cannot re-classify a layer differently.
    assert_eq!(layers[0]["role"], "image");
    assert_eq!(layers[1]["role"], "labels");
    for layer in &layers {
        let source = layer["source"].as_str().unwrap_or_default();
        assert!(
            source.starts_with("file://"),
            "a project source must be a URI a client can hand back: {source}"
        );
        assert!(layer["name"].is_string(), "no name in {layer}");
    }
}

#[actix_web::test]
async fn an_unsaved_annotation_layer_is_left_out_of_the_project() {
    let api = Api::with_annotations().await;
    let res = api.get("/api/project").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    // The drawn layer lives nowhere on disk, so an entry for it would be a
    // source the file can never reopen.
    let layers = res.json()["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 1, "{}", res.text());
    assert_eq!(layers[0]["role"], "image");
}

// ---------------------------------------------------------------------------
// POST /api/project
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn posting_a_project_replaces_the_open_layers() {
    let api = Api::with_labels().await;
    let res = api
        .post(
            "/api/project",
            json!({"layers": [{
                "source": format!("file://{}", api.store.display()),
                "role": "image",
                "name": "only",
            }]}),
        )
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    // The response is the new session, so the client need not re-fetch it.
    let layers = res.json()["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 1, "{}", res.text());
    assert_eq!(layers[0]["name"], "only");
    assert_eq!(api.layer_ids().await.len(), 1);
}

#[actix_web::test]
async fn a_project_layer_that_cannot_be_opened_is_skipped() {
    let api = Api::empty().await;
    let missing = api.dir.path().join("gone.zarr");
    let res = api
        .post(
            "/api/project",
            json!({"layers": [{"source": format!("file://{}", missing.display())}]}),
        )
        .await;
    // Deliberate: a run directory with one truncated output should still open
    // the rest, so a layer that fails is reported and skipped, not fatal.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(res.json()["layers"], json!([]), "{}", res.text());
}

#[actix_web::test]
async fn a_project_body_that_is_not_a_project_is_rejected() {
    let api = Api::empty().await;
    let res = api.post("/api/project", json!({"layers": 5})).await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    // A rejected body must leave the session alone rather than half-clearing it.
    assert!(api.layer_ids().await.is_empty());
}

// ---------------------------------------------------------------------------
// POST /api/layers
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn adding_a_layer_returns_the_session_including_it() {
    let api = Api::image().await;
    let labels = api.dir.path().join("added.zarr");
    let blobs = synthetic::blobs(api_harness::SHAPE, 3);
    synthetic::write_labels(&labels, api_harness::SHAPE, &blobs).expect("write labels");

    let res = api
        .post(
            "/api/layers",
            json!({
                "source": format!("file://{}", labels.display()),
                "role": "labels",
                "name": "mask",
            }),
        )
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let layers = res.json()["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 2, "{}", res.text());
    // `role` decides the kind, so asking for labels must not yield an image
    // even though the store would open as one.
    assert_eq!(layers[1]["kind"]["kind"], "labels");
    assert_eq!(layers[1]["name"], "mask");
}

#[actix_web::test]
async fn a_source_with_no_scheme_is_taken_as_a_path() {
    let api = Api::empty().await;
    write_image(&api.store);
    let res = api
        .post(
            "/api/layers",
            json!({"source": api.store.display().to_string()}),
        )
        .await;
    // Typing a path into the layer box is what a user does; requiring the
    // `file://` prefix would make that a silent failure.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(res.json()["layers"][0]["kind"]["kind"], "image");
}

#[actix_web::test]
async fn a_layer_whose_source_does_not_exist_is_a_bad_request() {
    let api = Api::empty().await;
    let missing = api.dir.path().join("not-here.zarr");
    let res = api
        .post(
            "/api/layers",
            json!({"source": format!("file://{}", missing.display())}),
        )
        .await;
    // A path a user mistyped is their error, not the server's: it must come
    // back as a 400 with a message, never as a 500 or a panicked worker.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        !res.text().is_empty(),
        "a 400 with nothing to show the user"
    );
    assert!(api.layer_ids().await.is_empty());
}

#[actix_web::test]
async fn a_source_uri_that_names_no_bucket_is_rejected() {
    let api = Api::empty().await;
    let res = api
        .post("/api/layers", json!({"source": "s3:///key"}))
        .await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        res.text().contains("bad source"),
        "the message must say which half of the request was wrong: {}",
        res.text()
    );
}

#[actix_web::test]
async fn an_unparsable_scale_is_rejected() {
    let api = Api::with_objects().await;
    let res = api
        .post(
            "/api/layers",
            json!({
                "source": format!("file://{}", api.dir.path().join("cells.csv").display()),
                "role": "objects",
                "scale": "not a number",
            }),
        )
        .await;
    // Rejected before the source is read: a scale nothing can parse would
    // otherwise put every row at an unpredictable place in the world.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("scale"), "{}", res.text());
}

#[actix_web::test]
async fn a_layer_body_with_no_source_is_rejected() {
    let api = Api::empty().await;
    let res = api.post("/api/layers", json!({"role": "image"})).await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
}

#[actix_web::test]
async fn a_directory_opened_as_a_project_becomes_its_layers() {
    let api = Api::empty().await;
    write_image(&api.store);
    let res = api
        .post(
            "/api/layers",
            json!({
                "source": format!("file://{}", api.dir.path().display()),
                "role": "project",
            }),
        )
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    // `role=project` is the "open a run" path: one request, every store under
    // the directory, rather than one request per asset.
    let layers = res.json()["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 1, "{}", res.text());
    assert_eq!(layers[0]["kind"]["kind"], "image");
}

#[actix_web::test]
async fn scanning_something_that_is_not_a_directory_is_a_bad_request() {
    let api = Api::empty().await;
    let res = api
        .post(
            "/api/layers",
            json!({
                "source": format!("file://{}", api.dir.path().join("nowhere").display()),
                "role": "project",
            }),
        )
        .await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        !res.text().is_empty(),
        "a 400 with nothing to show the user"
    );
}

// ---------------------------------------------------------------------------
// DELETE /api/layers/{id}
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn a_removed_layer_is_gone_from_the_session() {
    let api = Api::with_labels().await;
    let labels = api.layer_of_kind("labels").await;

    let res = api.delete(&format!("/api/layers/{labels}")).await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    // The response is the session, and so is the next GET: a client that
    // trusted the first and a client that re-fetched must see the same thing.
    assert_eq!(res.json()["layers"].as_array().map(Vec::len), Some(1));

    let session = api.get("/api/session").await;
    let remaining = session.json()["layers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(remaining.len(), 1, "{}", session.text());
    assert!(
        remaining.iter().all(|l| l["id"] != labels.as_str()),
        "layer {labels} survived its delete: {}",
        session.text()
    );
}

#[actix_web::test]
async fn removing_an_unknown_layer_is_a_not_found() {
    let api = Api::image().await;
    let res = api.delete("/api/layers/L99").await;
    assert_eq!(res.status, 404, "{} {}", res.status, res.text());
    assert!(
        res.text().contains("L99"),
        "the message must name the id that was not found: {}",
        res.text()
    );
    assert_eq!(api.layer_ids().await.len(), 1);
}

#[actix_web::test]
async fn removing_the_last_layer_leaves_a_session_that_still_answers() {
    let api = Api::image().await;
    assert!(api.delete("/api/layers/L0").await.is_ok());

    // An empty session is a state the viewer starts in, not an error state:
    // the session route keeps answering and only /api/info has nothing to say.
    let session = api.get("/api/session").await;
    assert!(session.is_ok(), "{} {}", session.status, session.text());
    assert_eq!(session.json()["layers"], json!([]), "{}", session.text());
    assert_eq!(api.get("/api/info").await.status, 404);
}

#[actix_web::test]
async fn a_layer_id_is_not_reused_after_a_delete() {
    let api = Api::image().await;
    assert!(api.delete("/api/layers/L0").await.is_ok());
    // Ids must not be reused: tile cache keys carry them, and a client may
    // still hold a request for the layer that was just closed.
    let id = api.open(&api.store.clone(), LayerRole::Image).await;
    assert_eq!(id, "L1");
}

// ---------------------------------------------------------------------------
// Resolving a layer, across every route that names one
//
// These two sweep the whole API rather than one route, because the bug they
// pin was a disagreement *between* routes: the same pair of requests — an id
// nothing answers to, and an open layer of the wrong kind — came back 404/400
// from /api/tile and 404/404 from /api/value, so no client could read one rule
// off the API. One helper decides it now; these say what it decided.
// ---------------------------------------------------------------------------

/// Every route that names a layer, with `LAYER` standing in for the id, and the
/// kind of layer that is *wrong* for it.
const LAYER_ROUTES: [(&str, &str); 8] = [
    (
        "/api/tile?layer=LAYER&level=0&z=0&y=0&x=0&h=8&w=8",
        "objects",
    ),
    ("/api/slice?layer=LAYER&level=0&index=0", "objects"),
    ("/api/value?layer=LAYER&level=0&z=0&y=0&x=0", "objects"),
    ("/api/objects?layer=LAYER&y0=0&y1=9&x0=0&x1=9", "image"),
    ("/api/objects/at?layer=LAYER&y=1&x=1", "image"),
    ("/api/tables/LAYER/rows", "image"),
    ("/api/tables/LAYER/column?name=area", "image"),
    ("/api/annotations/LAYER", "image"),
];

#[actix_web::test]
async fn an_unknown_layer_id_is_a_404_that_names_it_on_every_route() {
    let api = Api::with_objects().await;
    for (route, _) in LAYER_ROUTES {
        let res = api.get(&route.replace("LAYER", "L99")).await;
        assert_eq!(res.status, 404, "{route}: {} {}", res.status, res.text());
        // The handler's 404, not the router's: an unmatched route answers with
        // an empty body and would pass the status check while proving nothing.
        assert!(
            res.text().contains("L99"),
            "{route}: the body must name the id: `{}`",
            res.text()
        );
    }
}

#[actix_web::test]
async fn a_layer_of_the_wrong_kind_is_a_400_that_names_it_on_every_route() {
    let api = Api::with_objects().await;
    for (route, wrong) in LAYER_ROUTES {
        let layer = api.layer_of_kind(wrong).await;
        let res = api.get(&route.replace("LAYER", &layer)).await;
        assert_eq!(res.status, 400, "{route}: {} {}", res.status, res.text());
        assert!(
            res.text().contains(&layer),
            "{route}: the body must name the layer: `{}`",
            res.text()
        );
    }
}
