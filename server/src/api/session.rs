//! Routes that describe or replace the session itself.
//!
//! `/api/session`, `/api/info` and `/api/stats` report what is open; the rest
//! change it — listing and opening an S3 dataset, saving and reopening a
//! project file, and adding or closing one layer. Everything here that writes
//! clears the tile cache, because cache keys carry a layer id.

use actix_web::{delete, get, post, web, HttpResponse, Responder};
use serde::Deserialize;

use crate::objects::ObjectSpace;
use crate::project::{Project, ProjectLayer};
use crate::session::LayerRole;
use crate::source::SourceSpec;
use crate::zarr_reader;

use super::{resolve_store, AppState};

/// Handle GET /api/session — every open layer, in draw order.
#[get("/api/session")]
pub async fn session_info(data: web::Data<AppState>) -> impl Responder {
    HttpResponse::Ok().json(data.session.read().await.info())
}

/// Handle GET /api/info — the default image layer's metadata.
///
/// Kept as it was so a client that predates the session model still works; it
/// is `/api/session`'s first image layer.
#[get("/api/info")]
pub async fn info(data: web::Data<AppState>) -> impl Responder {
    let session = data.session.read().await;
    match resolve_store(&session, None) {
        Ok((_, store)) => HttpResponse::Ok().json(store.metadata()),
        Err(res) => res,
    }
}

/// Handle GET /api/stats — cache occupancy, for diagnosing slow panes.
#[get("/api/stats")]
pub async fn stats(data: web::Data<AppState>) -> impl Responder {
    let (entries, held, hits, misses) = data.cache.stats();
    HttpResponse::Ok().json(serde_json::json!({
        "cache": { "entries": entries, "bytes": held, "hits": hits, "misses": misses },
        "layers": data.session.read().await.layers().len(),
    }))
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

/// Handle POST /api/open — replace the session with one image layer from the
/// configured S3 bucket.
#[post("/api/open")]
pub async fn open_dataset(
    data: web::Data<AppState>,
    query: web::Query<OpenQuery>,
) -> impl Responder {
    let config = match &data.s3_config {
        Some(c) => c,
        None => return HttpResponse::BadRequest().body("No S3 config — cannot switch datasets"),
    };

    log::info!("Opening dataset: {}", query.dataset);

    let spec = SourceSpec::S3 {
        profile: "default".to_string(),
        bucket: config.bucket.clone(),
        key: format!("{}{}", config.prefix, query.dataset),
    };

    let mut session = data.session.write().await;
    session.clear();
    data.cache.clear();
    match session
        .add(
            &data.registry,
            spec,
            LayerRole::Image,
            Some(query.dataset.clone()),
            ObjectSpace::default(),
        )
        .await
    {
        Ok(_) => {
            let opened = session
                .default_layer()
                .and_then(|l| l.data.store())
                .map(|s| s.metadata().clone());
            match opened {
                Some(dataset) => HttpResponse::Ok().json(dataset),
                None => HttpResponse::InternalServerError().body("Layer opened with no metadata"),
            }
        }
        Err(e) => {
            log::error!("Failed to open dataset '{}': {}", query.dataset, e);
            HttpResponse::InternalServerError().body(format!("Error: {}", e))
        }
    }
}

/// Handle GET /api/project — the open session as a project file.
///
/// This is the "share a view" unit: the layer list with each source's URI, the
/// role it was opened as and the name it is shown under. Saving it is the
/// client's business — the server hands back JSON and does not write files on
/// behalf of a browser.
#[get("/api/project")]
pub async fn save_project(data: web::Data<AppState>) -> impl Responder {
    let session = data.session.read().await;
    let layers = session
        .layers()
        .iter()
        // A project file records where things live. An annotation layer that
        // has never been saved lives nowhere, so writing an entry for it would
        // put a source in the file that can never be opened — better to leave
        // it out than to promise a layer the file cannot deliver.
        .filter(|layer| !layer.spec.is_unsaved())
        .map(|layer| ProjectLayer {
            source: layer.spec.uri(),
            role: Some(layer.role().to_string()),
            name: Some(layer.name.clone()),
            scale: layer.object_scale(),
            offset: None,
        })
        .collect();
    HttpResponse::Ok().json(Project { name: None, layers })
}

/// Handle POST /api/project — replace the session with a project's layers.
#[post("/api/project")]
pub async fn open_project(data: web::Data<AppState>, body: web::Json<Project>) -> impl Responder {
    let wanted = body.into_inner();
    let mut session = data.session.write().await;
    session.clear();
    data.cache.clear();
    match wanted.open(&data.registry, &mut session).await {
        Ok(opened) => {
            log::info!("opened {opened} of {} layer(s)", wanted.layers.len());
            HttpResponse::Ok().json(session.info())
        }
        Err(e) => HttpResponse::BadRequest().body(format!("Error: {e:#}")),
    }
}

/// Body of POST /api/layers.
#[derive(Deserialize)]
pub struct AddLayer {
    /// A source URI: `file:///…`, `http(s)://…`, `s3://[profile@]bucket/key`.
    pub source: String,
    /// `image`, `labels`, `objects`, `project` (scan a directory), or absent
    /// for auto-detection.
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// For object layers: world pixels per source unit, `z,y,x`.
    ///
    /// A detector that ran on a downsampled volume writes coordinates in that
    /// volume's pixels, and nothing in its output says so — which is why this
    /// is a layer setting rather than something inferred.
    #[serde(default)]
    pub scale: Option<String>,
    /// For object layers: world offset added after scaling, `z,y,x`.
    #[serde(default)]
    pub offset: Option<String>,
}

/// Handle POST /api/layers — open a source and append it to the session.
#[post("/api/layers")]
pub async fn add_layer(data: web::Data<AppState>, body: web::Json<AddLayer>) -> impl Responder {
    let body = body.into_inner();
    let spec = match SourceSpec::parse(&body.source) {
        Ok(spec) => spec,
        Err(e) => return HttpResponse::BadRequest().body(format!("bad source: {e}")),
    };
    // A directory of outputs is opened as a *run*: every store, volume and
    // table under it becomes a layer, which is what a `clearmap-ng` workspace
    // is and what typing each asset by hand would otherwise cost.
    if body.role.as_deref() == Some("project") {
        let path = std::path::PathBuf::from(body.source.trim_start_matches("file://"));
        let scanned = match Project::scan(&path) {
            Ok(project) => project,
            Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
        };
        let mut session = data.session.write().await;
        return match scanned.open(&data.registry, &mut session).await {
            Ok(opened) => {
                log::info!("scanned {}: {opened} layer(s)", path.display());
                HttpResponse::Ok().json(session.info())
            }
            Err(e) => HttpResponse::BadRequest().body(format!("Error: {e:#}")),
        };
    }

    let role = LayerRole::parse(body.role.as_deref());
    let space = match ObjectSpace::parse(body.scale.as_deref(), body.offset.as_deref()) {
        Ok(space) => space,
        Err(e) => return HttpResponse::BadRequest().body(format!("bad scale/offset: {e:#}")),
    };
    let mut session = data.session.write().await;
    match session
        .add(&data.registry, spec, role, body.name, space)
        .await
    {
        Ok(id) => {
            log::info!("Added layer {id}");
            HttpResponse::Ok().json(session.info())
        }
        Err(e) => {
            log::error!("Failed to add layer: {e:#}");
            HttpResponse::BadRequest().body(format!("Error: {e:#}"))
        }
    }
}

/// Handle DELETE /api/layers/{id} — close a layer.
#[delete("/api/layers/{id}")]
pub async fn remove_layer(data: web::Data<AppState>, path: web::Path<String>) -> impl Responder {
    let id = path.into_inner();
    let mut session = data.session.write().await;
    if session.remove(&id) {
        // Cache keys carry the layer id, so a removed layer's tiles are dead
        // weight; and an id is never reused, so nothing can collide with them.
        data.cache.clear();
        HttpResponse::Ok().json(session.info())
    } else {
        HttpResponse::NotFound().body(format!("no layer {id}"))
    }
}
