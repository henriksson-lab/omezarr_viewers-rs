//! Routes over an object layer — a row per detection.
//!
//! `/api/objects` answers a visible rectangle as a packed binary buffer and
//! says how many rows *matched* as well as how many it sent; `/api/objects/at`
//! answers one row, with its exact values, for a click.

use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;

use crate::objects::ObjectQuery;

use super::{layer_objects, AppState};

/// Query parameters for /api/objects.
#[derive(Deserialize)]
pub struct ObjectsQuery {
    layer: String,
    /// The visible rectangle, in world pixels.
    y0: f32,
    y1: f32,
    x0: f32,
    x1: f32,
    /// The z slab. Absent means every z.
    #[serde(default)]
    z0: Option<f32>,
    #[serde(default)]
    z1: Option<f32>,
    /// The most rows to return; 0 or absent means no cap.
    #[serde(default)]
    max: Option<usize>,
    /// Comma-separated column names to send values for.
    #[serde(default)]
    columns: Option<String>,
}

/// Handle GET /api/objects — the rows in a region, as a packed binary buffer.
///
/// The response says how many rows *matched* as well as how many it carries,
/// so a client that asked for a cap can say "showing 50k of 1.2M" rather than
/// showing a decimated set as if it were everything.
#[get("/api/objects")]
pub async fn objects(data: web::Data<AppState>, query: web::Query<ObjectsQuery>) -> impl Responder {
    let q = query.into_inner();
    let store = {
        let session = data.session.read().await;
        match layer_objects(&session, &q.layer) {
            Ok(store) => store,
            Err(res) => return res,
        }
    };

    let requested: Vec<usize> = match &q.columns {
        Some(names) if !names.is_empty() => names
            .split(',')
            .filter_map(|name| {
                store
                    .columns()
                    .iter()
                    .position(|column| column.name == name.trim())
            })
            .collect(),
        _ => Vec::new(),
    };

    let selection = store.query(&ObjectQuery {
        y0: q.y0,
        y1: q.y1,
        x0: q.x0,
        x1: q.x1,
        z0: q.z0.unwrap_or(f32::NEG_INFINITY),
        z1: q.z1.unwrap_or(f32::INFINITY),
        max: q.max.unwrap_or(0),
    });
    let total = selection.total;
    let returned = selection.rows.len();
    let bytes = store.encode(&selection, &requested);

    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .insert_header(("X-Total", total.to_string()))
        .insert_header(("X-Returned", returned.to_string()))
        .insert_header(("X-Truncated", (returned < total).to_string()))
        .body(bytes)
}

/// Query parameters for /api/objects/at.
#[derive(Deserialize)]
pub struct ObjectAtQuery {
    layer: String,
    y: f32,
    x: f32,
    #[serde(default)]
    z: f32,
    /// Search radius in world pixels.
    #[serde(default = "default_radius")]
    r: f32,
}

fn default_radius() -> f32 {
    12.0
}

/// Handle GET /api/objects/at — the row nearest a point, with exact values.
#[get("/api/objects/at")]
pub async fn object_at(
    data: web::Data<AppState>,
    query: web::Query<ObjectAtQuery>,
) -> impl Responder {
    let q = query.into_inner();
    let store = {
        let session = data.session.read().await;
        match layer_objects(&session, &q.layer) {
            Ok(store) => store,
            Err(res) => return res,
        }
    };
    match store
        .nearest(q.z, q.y, q.x, q.r)
        .and_then(|row| store.row_json(row))
    {
        Some(row) => HttpResponse::Ok().json(row),
        None => HttpResponse::Ok().json(serde_json::Value::Null),
    }
}
