use actix_web::{get, post, web, HttpResponse, Responder};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::zarr_reader::{self, S3Config, ZarrStore};

/// Shared application state holding the active ZarrStore and optional S3 config.
pub struct AppState {
    pub store: RwLock<Option<Arc<ZarrStore>>>,
    pub s3_config: Option<S3Config>,
}

/// Handle GET /api/info — return dataset metadata as JSON.
#[get("/api/info")]
pub async fn info(data: web::Data<AppState>) -> impl Responder {
    let store = data.store.read().await;
    match store.as_ref() {
        Some(s) => HttpResponse::Ok().json(s.metadata()),
        None => HttpResponse::NotFound().body("No dataset loaded"),
    }
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
    let store = data.store.read().await;
    let store = match store.as_ref() {
        Some(s) => s.clone(),
        None => return HttpResponse::NotFound().body("No dataset loaded"),
    };
    let q = query.into_inner();

    match store
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

/// Handle GET /api/datasets — list available datasets from S3.
#[get("/api/datasets")]
pub async fn datasets(data: web::Data<AppState>) -> impl Responder {
    let config = match &data.s3_config {
        Some(c) => c,
        None => return HttpResponse::Ok().json(Vec::<String>::new()),
    };
    match zarr_reader::list_s3_datasets(config).await {
        Ok(list) => HttpResponse::Ok().json(list),
        Err(e) => {
            log::error!("Failed to list datasets: {}", e);
            HttpResponse::InternalServerError().body(format!("Error: {}", e))
        }
    }
}

/// Query parameters for the /api/open endpoint.
#[derive(Deserialize)]
pub struct OpenQuery {
    dataset: String,
}

/// Handle POST /api/open — switch to a different dataset.
#[post("/api/open")]
pub async fn open_dataset(data: web::Data<AppState>, query: web::Query<OpenQuery>) -> impl Responder {
    let config = match &data.s3_config {
        Some(c) => c,
        None => return HttpResponse::BadRequest().body("No S3 config — cannot switch datasets"),
    };

    log::info!("Opening dataset: {}", query.dataset);

    match ZarrStore::open_s3(config, &query.dataset).await {
        Ok(store) => {
            let dataset_info = store.metadata().clone();
            let mut current = data.store.write().await;
            *current = Some(Arc::new(store));
            HttpResponse::Ok().json(dataset_info)
        }
        Err(e) => {
            log::error!("Failed to open dataset '{}': {}", query.dataset, e);
            HttpResponse::InternalServerError().body(format!("Error: {}", e))
        }
    }
}
