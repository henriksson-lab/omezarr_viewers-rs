//! Numbers a client can make absurd.
//!
//! Every one of these is a query-string value that reaches arithmetic in
//! `api.rs`. The claim is narrow and worth having: an out-of-range number is a
//! bad request or an empty answer, never a crash. `?offset=18446744073709551615`
//! panicked with `attempt to add with overflow` in a debug build until the
//! `saturating_add` these tests pin — found only once `api.rs` had any tests at
//! all.

mod api_harness;

use api_harness::{Api, Res};

/// The claim every test here makes: a verdict came back at all.
///
/// Not *which* verdict — an absurd number is a bad request or an empty answer
/// depending on the route, and pinning which would be pinning an accident. What
/// matters is that the handler returned rather than panicking, because a panic
/// is a dropped connection with no status at all.
fn assert_answered(res: &Res, what: &str) {
    assert!(
        res.status.is_success() || res.status.is_client_error(),
        "{what}: {} {}",
        res.status,
        res.text()
    );
}

/// The largest value that fits the `usize`/`u64` these queries deserialise into.
const HUGE: u64 = u64::MAX;

#[actix_web::test]
async fn a_table_offset_at_the_integer_ceiling_is_an_empty_page_not_a_panic() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;

    let res = api
        .get(&format!("/api/tables/{layer}/rows?offset={HUGE}&limit=100"))
        .await;
    assert_answered(&res, "a table offset at the ceiling");
}

#[actix_web::test]
async fn a_table_limit_at_the_ceiling_is_capped_rather_than_added_to_the_offset() {
    let api = Api::with_annotations().await;
    let layer = api.layer_of_kind("annotations").await;

    let res = api
        .get(&format!("/api/tables/{layer}/rows?offset=1&limit={HUGE}"))
        .await;
    assert_answered(&res, "a table limit at the ceiling");
}

#[actix_web::test]
async fn a_projection_depth_at_the_ceiling_does_not_overflow_the_z_range() {
    let api = Api::image().await;
    let layer = api.layer_ids().await[0].clone();

    // `z + depth` is the top of the projected range, and both halves are the
    // client's to choose.
    let res = api
        .get(&format!(
            "/api/tile?layer={layer}&level=0&z={HUGE}&y=0&x=0&h=8&w=8&zproj=max&depth={HUGE}"
        ))
        .await;
    assert_answered(&res, "a projection depth at the ceiling");
}

#[actix_web::test]
async fn a_slice_index_at_the_ceiling_does_not_overflow_its_one_plane_range() {
    let api = Api::image().await;
    let layer = api.layer_ids().await[0].clone();

    // The slice reader projects over `index .. index + 1`.
    let res = api
        .get(&format!("/api/slice?layer={layer}&level=0&index={HUGE}"))
        .await;
    assert_answered(&res, "a slice index at the ceiling");
}
