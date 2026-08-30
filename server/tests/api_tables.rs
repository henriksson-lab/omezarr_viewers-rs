//! The table routes, and the route ordering they depend on.
//!
//! Two claims. First, that a table layer can be read over HTTP: its columns in
//! the file's own order, a page at a time with the total reported, and one
//! numeric column joined to the label ids the table is keyed by. Second, that
//! `/api/annotations/tables` and `/api/annotations/layers` reach their own
//! handlers rather than the `/{layer}` routes that also match them — actix
//! takes the first route that matches, so `configure`'s order is behaviour.

mod api_harness;

use std::fs;
use std::path::Path;

use api_harness::Api;
use omezarr_viewer_server::session::LayerRole;

/// How many rows the big fixture has: more than the handler's default page of
/// 200, so a request that asks for no page still proves there is one.
const ROWS: usize = 250;

/// Write an ngio feature table as plain files, the way ngio would leave one.
///
/// Written here rather than through `roi_table::write`, because the point of
/// these tests is reading a table this viewer did not write: a v2 `tables`
/// group, a `.zattrs` naming the backend and the label image, and a CSV.
fn write_feature_table(store: &Path, name: &str, instance_key: &str, csv: &str) {
    let tables = store.join("tables");
    let group = tables.join(name);
    fs::create_dir_all(&group).expect("create the table group");
    fs::write(tables.join(".zgroup"), r#"{"zarr_format":2}"#).expect("tables/.zgroup");
    fs::write(
        tables.join(".zattrs"),
        format!(r#"{{"tables":["{name}"]}}"#),
    )
    .expect("tables/.zattrs");
    fs::write(group.join(".zgroup"), r#"{"zarr_format":2}"#).expect("table/.zgroup");
    fs::write(
        group.join(".zattrs"),
        format!(
            r#"{{"type":"feature_table","table_version":"1","backend":"csv",
                 "region":{{"path":"../labels/nuclei"}},"instance_key":"{instance_key}",
                 "index_key":"{instance_key}","index_type":"int"}}"#
        ),
    )
    .expect("table/.zattrs");
    fs::write(group.join("table.csv"), csv).expect("table.csv");
}

/// The big fixture's CSV: an id, a fractional number, an integral number and a
/// text column, so both column kinds are exercised by one table.
fn feature_csv() -> String {
    let mut csv = String::from("label,area,intensity_mean,cell_type\n");
    for i in 1..=ROWS {
        let kind = if i % 3 == 0 { "stroma" } else { "tumour" };
        csv.push_str(&format!("{i},{}.5,{},{kind}\n", 100 + i, (i % 60) + 20));
    }
    csv
}

/// A server with an image and a feature table over it, and the table's id.
async fn with_feature_table() -> (Api, String) {
    let api = Api::image().await;
    write_feature_table(&api.store, "features", "label", &feature_csv());
    let id = api
        .open(
            &api.store.join("tables").join("features"),
            LayerRole::Annotations,
        )
        .await;
    // A table with no coordinates is a layer of its own, not an annotation
    // layer: if it came back as annotations the table routes would 404 and
    // every assertion below would be about the wrong thing.
    assert_eq!(
        api.layer_of_kind("table").await,
        id,
        "the feature table opened as something other than a table layer"
    );
    (api, id)
}

// ---------------------------------------------------------------------------
// GET /api/tables/{layer}/rows
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn rows_come_back_with_the_file_s_own_columns_in_the_file_s_own_order() {
    let (api, id) = with_feature_table().await;
    let res = api.get(&format!("/api/tables/{id}/rows")).await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();

    // Not sorted: `Columns` keeps a separate order list precisely so a table
    // view shows what the writer wrote rather than what a BTreeMap produces.
    assert_eq!(
        body["columns"],
        serde_json::json!(["label", "area", "intensity_mean", "cell_type"]),
        "{}",
        res.text()
    );
    assert_eq!(body["total"], ROWS, "{}", res.text());
    assert_eq!(body["offset"], 0, "{}", res.text());
    // Every cell is text on the wire, whichever kind of column holds it.
    assert_eq!(
        body["rows"][0],
        serde_json::json!(["1", "101.5", "21", "tumour"]),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn rows_stop_at_the_default_page_but_report_the_whole_table() {
    let (api, id) = with_feature_table().await;
    let res = api.get(&format!("/api/tables/{id}/rows")).await;
    let body = res.json();
    // A feature table has a row per segmented object; a client that asked for
    // no page must still not be handed a hundred thousand of them, and must
    // still be told how many there are so it can ask for the rest.
    assert_eq!(
        body["rows"].as_array().map(Vec::len),
        Some(200),
        "{}",
        res.text()
    );
    assert_eq!(body["total"], ROWS, "{}", res.text());
}

#[actix_web::test]
async fn offset_and_limit_return_the_slice_they_name() {
    let (api, id) = with_feature_table().await;
    let res = api
        .get(&format!("/api/tables/{id}/rows?offset=200&limit=100"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    // The window is clamped to the table, not to the limit: 200..300 over 250
    // rows is 50 rows, and the 201st row is the first of them.
    assert_eq!(
        body["rows"].as_array().map(Vec::len),
        Some(50),
        "{}",
        res.text()
    );
    assert_eq!(body["offset"], 200, "{}", res.text());
    assert_eq!(body["total"], ROWS, "{}", res.text());
    assert_eq!(
        body["rows"][0],
        serde_json::json!(["201", "301.5", "41", "stroma"]),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn an_offset_past_the_end_is_empty_rather_than_an_error() {
    let (api, id) = with_feature_table().await;
    let res = api.get(&format!("/api/tables/{id}/rows?offset=1000")).await;
    // A client paging by a stale total asks for rows that are no longer there;
    // that is a normal race, not a failure, and the fresh total is the answer.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert_eq!(
        body["rows"].as_array().map(Vec::len),
        Some(0),
        "{}",
        res.text()
    );
    assert_eq!(body["total"], ROWS, "{}", res.text());
}

#[actix_web::test]
async fn an_unknown_layer_has_no_rows() {
    let (api, _) = with_feature_table().await;
    let res = api.get("/api/tables/L99/rows").await;
    assert_eq!(res.status, 404, "{} {}", res.status, res.text());
    assert!(res.text().contains("L99"), "{}", res.text());
}

#[actix_web::test]
async fn asking_an_image_layer_for_rows_fails_cleanly() {
    let (api, _) = with_feature_table().await;
    let image = api.layer_of_kind("image").await;
    let res = api.get(&format!("/api/tables/{image}/rows")).await;
    // The layer exists and is the wrong kind: a 400 that names it, not the 404
    // an id nobody has gets, and certainly not a 500 or an empty table
    // pretending to be one.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(
        res.text().contains(&image) && res.text().contains("is not a table"),
        "{}",
        res.text()
    );
}

// ---------------------------------------------------------------------------
// GET /api/tables/{layer}/column
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn a_column_comes_back_paired_with_the_label_ids_of_its_rows() {
    let (api, id) = with_feature_table().await;
    let res = api.get(&format!("/api/tables/{id}/column?name=area")).await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert_eq!(body["column"], "area", "{}", res.text());
    // Not paged, unlike /rows: this is the join that colours a label image, so
    // a missing id is a label drawn in the wrong colour.
    assert_eq!(
        body["labels"].as_array().map(Vec::len),
        Some(ROWS),
        "{}",
        res.text()
    );
    assert_eq!(
        body["values"].as_array().map(Vec::len),
        Some(ROWS),
        "{}",
        res.text()
    );
    assert_eq!(body["labels"][0], 1, "{}", res.text());
    assert_eq!(body["values"][0], 101.5, "{}", res.text());
    assert_eq!(body["labels"][249], 250, "{}", res.text());
}

#[actix_web::test]
async fn the_ids_come_from_the_table_s_instance_key_not_from_a_column_named_label() {
    let api = Api::image().await;
    // No column called `label` at all: the ids have to come from the key the
    // group's attributes name, or the join is to nothing.
    write_feature_table(
        &api.store,
        "keyed",
        "cell_id",
        "cell_id,score\n7,0.5\n8,1.5\n9,2.5\n",
    );
    let id = api
        .open(
            &api.store.join("tables").join("keyed"),
            LayerRole::Annotations,
        )
        .await;
    let res = api
        .get(&format!("/api/tables/{id}/column?name=score"))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert_eq!(
        body["labels"],
        serde_json::json!([7, 8, 9]),
        "{}",
        res.text()
    );
    assert_eq!(
        body["values"],
        serde_json::json!([0.5, 1.5, 2.5]),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn a_column_that_is_not_there_is_refused() {
    let (api, id) = with_feature_table().await;
    let res = api.get(&format!("/api/tables/{id}/column?name=nope")).await;
    // NOTE: 400, not 404 — the handler answers a missing column and a
    // non-numeric one with the same status and message. Asserting what it
    // does; see the report for why this is arguably the wrong code.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("nope"), "{}", res.text());
}

#[actix_web::test]
async fn a_text_column_cannot_colour_a_label_image() {
    let (api, id) = with_feature_table().await;
    let res = api
        .get(&format!("/api/tables/{id}/column?name=cell_type"))
        .await;
    // `cell_type` is a real column, and still not an answer: the endpoint
    // exists to produce numbers a colour ramp can use.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("cell_type"), "{}", res.text());
}

#[actix_web::test]
async fn an_unknown_layer_has_no_columns_either() {
    let (api, _) = with_feature_table().await;
    let res = api.get("/api/tables/L99/column?name=area").await;
    assert_eq!(res.status, 404, "{} {}", res.status, res.text());
}

#[actix_web::test]
async fn asking_an_image_layer_for_a_column_fails_cleanly() {
    let (api, _) = with_feature_table().await;
    let image = api.layer_of_kind("image").await;
    let res = api
        .get(&format!("/api/tables/{image}/column?name=area"))
        .await;
    // Wrong kind of layer, not a missing one — the same 400 the rows route
    // gives, so a client reads one rule off both.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("is not a table"), "{}", res.text());
}

// ---------------------------------------------------------------------------
// GET /api/annotations/tables
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn the_listing_names_the_tables_a_store_already_holds() {
    let (api, _) = with_feature_table().await;
    let res = api.get("/api/annotations/tables").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    // With no `store=`, the store is the reference layer's own source — which
    // is what makes this a list the user can pick from rather than a path they
    // have to remember.
    assert_eq!(
        body["store"],
        api.store.display().to_string(),
        "{}",
        res.text()
    );
    assert_eq!(
        body["tables"],
        serde_json::json!(["features"]),
        "{}",
        res.text()
    );
    assert_eq!(body["annotations"], serde_json::json!([]), "{}", res.text());
    assert_eq!(body["writable"], true, "{}", res.text());
    assert_eq!(body["error"], serde_json::Value::Null, "{}", res.text());
}

#[actix_web::test]
async fn a_store_with_no_tables_group_lists_nothing_and_is_not_an_error() {
    let api = Api::image().await;
    let res = api.get("/api/annotations/tables").await;
    // The normal case for a fresh store. An error status here would make every
    // client special-case the situation it will meet most often.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(
        res.json()["tables"],
        serde_json::json!([]),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn the_listing_looks_inside_the_store_the_query_names() {
    let (api, _) = with_feature_table().await;
    let elsewhere = api.dir.path().join("empty.zarr");
    std::fs::create_dir_all(&elsewhere).expect("create a second store");
    let res = api
        .get(&format!(
            "/api/annotations/tables?store={}",
            elsewhere.display()
        ))
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    // The named store, not the session's: this is how a user browses a store
    // nothing has opened yet.
    assert_eq!(
        body["store"],
        elsewhere.display().to_string(),
        "{}",
        res.text()
    );
    assert_eq!(body["tables"], serde_json::json!([]), "{}", res.text());
}

// ---------------------------------------------------------------------------
// Route ordering
//
// `configure` registers `/api/annotations/tables` and
// `/api/annotations/layers` before `/api/annotations/{layer}`, and actix takes
// the first route that matches rather than the most specific one. Nothing else
// in the tree notices if that order is disturbed: the literal segments would
// simply arrive at the parameter handlers as a layer id spelled "tables".
// ---------------------------------------------------------------------------

#[actix_web::test]
async fn the_literal_tables_route_is_not_swallowed_by_the_layer_route() {
    let api = Api::empty().await;
    let res = api.get("/api/annotations/tables").await;
    // With no layer open at all, `GET /api/annotations/{layer}` could only
    // answer 404 "no layer tables". A 200 carrying the listing's own keys is
    // proof the literal route matched first.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert!(
        body.is_object(),
        "the listing is an object, got {}",
        res.text()
    );
    assert_eq!(body["tables"], serde_json::json!([]), "{}", res.text());
    // With nothing open there is no store to look inside, and the listing says
    // so rather than failing — the empty session is still the listing's
    // answer, not the layer route's.
    assert_eq!(body["store"], serde_json::Value::Null, "{}", res.text());
}

#[actix_web::test]
async fn an_open_annotation_layer_does_not_shadow_the_tables_listing() {
    let (api, _) = with_feature_table().await;
    api.post(
        "/api/annotations/layers",
        serde_json::json!({"name": "drawn"}),
    )
    .await;
    let res = api.get("/api/annotations/tables").await;
    // The other side of the same order: with a layer route that would happily
    // answer for some id, the listing still wins. `annotations` returns a JSON
    // *array* of shapes, so the shape of the body tells the two apart.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert!(
        res.json().is_object(),
        "the layer route answered instead: {}",
        res.text()
    );
    assert_eq!(
        res.json()["tables"],
        serde_json::json!(["features"]),
        "{}",
        res.text()
    );
}

#[actix_web::test]
async fn the_literal_layers_route_is_not_swallowed_by_the_add_annotation_route() {
    let api = Api::empty().await;
    let res = api
        .post(
            "/api/annotations/layers",
            serde_json::json!({"name": "drawn"}),
        )
        .await;
    // `POST /api/annotations/{layer}` would read this body as an `Annotation`
    // for a layer called "layers" and answer 400 or 404. A session that grew a
    // layer is proof the literal route matched first.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(
        res.json()["layers"].as_array().map(Vec::len),
        Some(1),
        "{}",
        res.text()
    );
    assert_eq!(res.json()["layers"][0]["name"], "drawn", "{}", res.text());
    assert_eq!(api.layer_ids().await.len(), 1);
}

#[actix_web::test]
async fn a_real_layer_id_still_reaches_the_parameter_route() {
    let api = Api::with_annotations().await;
    let id = api.layer_of_kind("annotations").await;
    let res = api.get(&format!("/api/annotations/{id}")).await;
    // The complement of the two tests above: putting the literals first must
    // not make them greedy. An empty layer's shapes are an empty array.
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(res.json(), serde_json::json!([]), "{}", res.text());
}
