//! The harness itself works.

mod api_harness;

use api_harness::Api;

#[actix_web::test]
async fn a_server_with_no_layers_still_answers() {
    let api = Api::empty().await;
    let res = api.get("/api/session").await;
    assert!(res.is_ok(), "{} {}", res.status, res.text());
    assert_eq!(res.json()["layers"].as_array().map(Vec::len), Some(0));
}

#[actix_web::test]
async fn an_open_image_is_reported_by_the_session() {
    let api = Api::image().await;
    let session = api.get("/api/session").await.json();
    assert_eq!(session["layers"].as_array().map(Vec::len), Some(1));
    assert_eq!(session["layers"][0]["kind"]["kind"], "image");
}

#[actix_web::test]
async fn every_fixture_the_harness_offers_opens() {
    assert_eq!(Api::with_labels().await.layer_ids().await.len(), 2);
    assert_eq!(Api::with_objects().await.layer_ids().await.len(), 2);

    let annotated = Api::with_annotations().await;
    assert_eq!(annotated.layer_of_kind("annotations").await, "L1");
    assert!(Api::image_writable().await.state.allow_remote_writes);
}
