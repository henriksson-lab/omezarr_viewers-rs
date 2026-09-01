//! Classing label ids over HTTP.
//!
//! The claim: a class can be put on an id, taken off again, and written to a
//! feature table joined to the label image by that id — and the three ways this
//! can quietly go wrong are each pinned. Those are the route-order collision
//! between `/classes/save` and `/classes/{id}`, the difference between an
//! unassigned id and one classed as nothing, and the remote-write gate.

mod api_harness;

use api_harness::Api;
use serde_json::json;

async fn labels_layer(api: &Api) -> String {
    api.layer_of_kind("labels").await
}

#[actix_web::test]
async fn a_class_can_be_put_on_an_id_and_taken_off_again() {
    let api = Api::with_labels().await;
    let layer = labels_layer(&api).await;

    let res = api
        .put(
            &format!("/api/labels/{layer}/classes/7"),
            json!({"class": "tumour"}),
        )
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());

    let listed = api
        .get(&format!("/api/labels/{layer}/classes"))
        .await
        .json();
    assert_eq!(listed["assigned"][0]["id"], 7);
    assert_eq!(listed["assigned"][0]["class"], "tumour");
    assert_eq!(listed["classes"], json!(["tumour"]));

    let res = api.delete(&format!("/api/labels/{layer}/classes/7")).await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let listed = api
        .get(&format!("/api/labels/{layer}/classes"))
        .await
        .json();
    assert_eq!(listed["assigned"].as_array().map(Vec::len), Some(0));
}

#[actix_web::test]
async fn an_id_classed_as_nothing_is_not_an_id_nobody_looked_at() {
    // The distinction the whole table rests on. An empty class is a decision;
    // no row is an absence of one, and a trainer must be able to tell them
    // apart or "I have not started" reads as "none of these are cells".
    let api = Api::with_labels().await;
    let layer = labels_layer(&api).await;

    api.put(
        &format!("/api/labels/{layer}/classes/3"),
        json!({"class": ""}),
    )
    .await;
    let listed = api
        .get(&format!("/api/labels/{layer}/classes"))
        .await
        .json();
    assert_eq!(
        listed["assigned"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
    assert_eq!(listed["assigned"][0]["class"], "");
}

#[actix_web::test]
async fn the_literal_save_route_is_not_read_as_an_id_named_save() {
    // `/classes/save` and `/classes/{id}` collide by shape and are told apart
    // only by method. If someone adds `POST /classes/{id}`, this fails — which
    // is the point of pinning it.
    let api = Api::with_labels().await;
    let layer = labels_layer(&api).await;

    let res = api
        .post(
            &format!("/api/labels/{layer}/classes/save"),
            json!({"target": "", "region": "../labels/nuclei"}),
        )
        .await;
    // Reaching the handler at all is the claim: it refuses an empty target,
    // where the `{id}` route would have refused to parse "save" as a number.
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains("somewhere to write"), "{}", res.text());
}

#[actix_web::test]
async fn classes_are_saved_as_a_feature_table_joined_to_the_labels() {
    let api = Api::with_labels().await;
    let layer = labels_layer(&api).await;
    api.put(
        &format!("/api/labels/{layer}/classes/1"),
        json!({"class": "tumour"}),
    )
    .await;
    api.put(
        &format!("/api/labels/{layer}/classes/2"),
        json!({"class": "stroma"}),
    )
    .await;

    let target = format!("{}/tables/cell_types", api.store.display());
    let res = api
        .post(
            &format!("/api/labels/{layer}/classes/save"),
            json!({"target": target, "region": "../labels/nuclei"}),
        )
        .await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    let body = res.json();
    assert_eq!(body["rows"], 2);
    assert_eq!(body["format"], "feature_table");
    assert!(
        api.store.join("tables/cell_types/table.csv").is_file(),
        "the payload is on disk"
    );
    let csv = std::fs::read_to_string(api.store.join("tables/cell_types/table.csv")).unwrap();
    assert_eq!(csv, "label,class\n1,tumour\n2,stroma\n", "{csv}");
}

#[actix_web::test]
async fn a_remote_save_is_refused_unless_the_operator_allowed_it() {
    // The same gate the annotation saves pass, for the same reason: credentials
    // given so this server can read a bucket must not become write access.
    let api = Api::with_labels().await;
    let layer = labels_layer(&api).await;
    let res = api
        .post(
            &format!("/api/labels/{layer}/classes/save"),
            json!({"target": "s3://bucket/run.zarr/tables/x", "region": "../labels/n"}),
        )
        .await;
    assert_eq!(res.status, 403, "{} {}", res.status, res.text());
    assert!(res.text().contains("allow-remote-writes"), "{}", res.text());
}

#[actix_web::test]
async fn an_image_layer_is_not_a_label_image() {
    let api = Api::with_labels().await;
    let image = api.layer_of_kind("image").await;
    let res = api.get(&format!("/api/labels/{image}/classes")).await;
    assert_eq!(res.status, 400, "{} {}", res.status, res.text());
    assert!(res.text().contains(&image), "{}", res.text());

    let missing = api.get("/api/labels/L99/classes").await;
    assert_eq!(missing.status, 404, "{}", missing.text());
    assert!(missing.text().contains("L99"), "{}", missing.text());
}
