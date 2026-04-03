use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;
use std::sync::Arc;

use crate::zarr_reader::ZarrStore;

/// Shared application state holding the ZarrStore for request handlers.
pub struct AppState {
    pub store: Arc<ZarrStore>,
}

/// Handle GET /api/info — return dataset metadata as JSON.
#[get("/api/info")]
pub async fn info(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(data.store.metadata())
}

/// Query parameters for the /api/tile endpoint.
#[derive(Deserialize)]
pub struct TileQuery {
    level: usize,
    #[serde(default)]
    t: u64,
    #[serde(default)]
    c: u64,
    #[serde(default)]
    z: u64,
    y: u64,
    x: u64,
    h: u64,
    w: u64,
}

/// Handle GET /api/tile — return raw float32 tile data.
#[get("/api/tile")]
pub async fn tile(data: web::Data<AppState>, query: web::Query<TileQuery>) -> impl Responder {
    let q = query.into_inner();

    match data
        .store
        .read_tile(q.level, q.t, q.c, q.z, q.y, q.x, q.h, q.w)
        .await
    {
        Ok(pixels) => {
            let bytes: Vec<u8> = pixels.iter().flat_map(|f| f.to_le_bytes()).collect();
            HttpResponse::Ok()
                .content_type("application/octet-stream")
                .insert_header(("X-Dtype", "float32"))
                .insert_header(("X-Width", q.w.to_string()))
                .insert_header(("X-Height", q.h.to_string()))
                .body(bytes)
        }
        Err(e) => {
            log::error!("Tile read error: {}", e);
            HttpResponse::InternalServerError().body(format!("Error: {}", e))
        }
    }
}
