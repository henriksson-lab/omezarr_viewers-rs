use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;

use omezarr_viewer_common::Annotation;

use crate::annotations::{geojson, roi_table, AnnotationSet};
use crate::cache::{TileCache, TileKey};
use crate::objects::{ObjectQuery, ObjectSpace};
use crate::ontology::{Ontology, RegionCount};
use crate::project::{Project, ProjectLayer};
use crate::session::{LayerRole, Session};
use crate::source::{SourceRegistry, SourceSpec};
use crate::zarr_reader::{
    self, PlaneAxis, PlaneRequest, Projection, S3Config, TileEncoding, TileRequest,
};

/// Shared application state: the open session, how sources are resolved, and
/// the tile cache.
pub struct AppState {
    pub session: RwLock<Session>,
    pub registry: SourceRegistry,
    pub cache: TileCache,
    pub s3_config: Option<S3Config>,
    /// Region names for label ids, when an atlas ontology was given.
    pub ontology: Option<Arc<Ontology>>,
    /// May annotations be written to `s3://` and `http(s)://` targets?
    ///
    /// Off unless the operator said otherwise: the credentials this server holds
    /// were given to it for reading.
    pub allow_remote_writes: bool,
}

/// Register every route, in the order actix must see them.
///
/// One list, shared by `main` and by the HTTP tests, because the *order* is
/// part of the behaviour: `/tables` and `/layers` are literal segments that
/// would also match `/{layer}`, and actix takes the first route that matches,
/// not the most specific. A test that registered its own list in its own order
/// would be testing a server nobody runs.
pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(info)
        .service(session_info)
        .service(stats)
        .service(tile)
        .service(slice)
        .service(voxel_value)
        .service(objects)
        .service(object_at)
        .service(regions)
        .service(datasets)
        .service(open_dataset)
        .service(save_project)
        .service(open_project)
        .service(add_layer)
        .service(remove_layer)
        .service(list_tables)
        .service(table_rows)
        .service(table_column)
        .service(add_annotation_layer)
        .service(annotations)
        .service(add_annotation)
        .service(save_annotations)
        .service(renest_annotations)
        .service(detach_annotation)
        .service(update_annotation)
        .service(remove_annotation);
}

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
    match session.default_layer().and_then(|l| l.data.store()) {
        Some(store) => HttpResponse::Ok().json(store.metadata()),
        None => HttpResponse::NotFound().body("No dataset loaded"),
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

/// Query parameters for the /api/tile endpoint.
#[derive(Deserialize)]
pub struct TileQuery {
    level: usize,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
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
    /// `max` or `mean` to project through z instead of taking one slice.
    #[serde(default)]
    zproj: Option<String>,
    /// How many z planes the projection covers, starting at `z`.
    #[serde(default)]
    depth: Option<u64>,
}

/// Handle GET /api/tile — raw tile bytes, f32 by default, the array's own
/// dtype under `encoding=raw`.
#[get("/api/tile")]
pub async fn tile(data: web::Data<AppState>, query: web::Query<TileQuery>) -> impl Responder {
    let q = query.into_inner();
    let encoding = TileEncoding::parse(q.encoding.as_deref());
    let projection = Projection::parse(q.zproj.as_deref());
    let depth = q.depth.unwrap_or(1).max(1);

    let (layer_id, store) = {
        let session = data.session.read().await;
        match session.resolve(q.layer.as_deref()) {
            Some(layer) => match layer.data.store() {
                Some(store) => (layer.id.clone(), store.clone()),
                None => {
                    return HttpResponse::BadRequest()
                        .body(format!("layer {} holds no image data", layer.id))
                }
            },
            None => return HttpResponse::NotFound().body("No such layer"),
        }
    };

    let key = TileKey {
        layer: layer_id,
        level: q.level,
        t: q.t,
        c: q.c,
        z: q.z,
        y: q.y,
        x: q.x,
        h: q.h,
        w: q.w,
        encoding: encoding.as_str(),
        // Saturating: `z` and `depth` are whatever the query string said, and
        // `u64::MAX + 1` is a panic in a debug build rather than a bad request.
        projection: projection.map(|p| (p.as_str(), q.z, q.z.saturating_add(depth))),
    };

    if let Some(bytes) = data.cache.get(&key) {
        let dtype = match projection {
            // A projection is f32 whatever the array holds; see `read_tile_bytes`.
            Some(_) => "float32".to_string(),
            None => wire_dtype(&store, q.level, encoding),
        };
        return tile_response(&bytes, &dtype, q.w, q.h, true);
    }

    let request = TileRequest::new(q.level, q.y, q.x, q.h, q.w)
        .at(q.t, q.c, q.z)
        .encoded(encoding)
        .projected(projection, depth);
    match store.read_tile_bytes(&request).await {
        Ok(tile) => {
            let bytes = Arc::new(tile.bytes);
            data.cache.put(key, bytes.clone());
            tile_response(&bytes, &tile.dtype, q.w, q.h, false)
        }
        Err(e) => {
            log::error!("Tile read error: {}", e);
            HttpResponse::InternalServerError().body(format!("Error: {}", e))
        }
    }
}

/// The dtype a cached tile is in, without re-reading it.
fn wire_dtype(store: &crate::volume::Volume, level: usize, encoding: TileEncoding) -> String {
    match encoding {
        TileEncoding::F32 => "float32".to_string(),
        TileEncoding::Raw => store
            .level_dtype(level)
            .unwrap_or_else(|_| "float32".to_string()),
    }
}

fn tile_response(bytes: &[u8], dtype: &str, w: u64, h: u64, cached: bool) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/octet-stream")
        .insert_header(("X-Dtype", dtype.to_string()))
        .insert_header(("X-Width", w.to_string()))
        .insert_header(("X-Height", h.to_string()))
        .insert_header(("X-Cache", if cached { "hit" } else { "miss" }))
        .body(bytes.to_vec())
}

/// Query parameters for /api/slice.
#[derive(Deserialize)]
pub struct SliceQuery {
    level: usize,
    #[serde(default)]
    layer: Option<String>,
    /// `z`, `y` or `x` — the axis held constant.
    #[serde(default)]
    axis: Option<String>,
    /// The index along that axis.
    index: u64,
    #[serde(default)]
    t: u64,
    #[serde(default)]
    c: u64,
    #[serde(default)]
    encoding: Option<String>,
}

/// Handle GET /api/slice — a whole plane across one axis.
///
/// This is what the orthogonal panes read. The shape comes back in the headers
/// rather than in the body, so the answer is still a bare array of pixels and
/// the client uploads it to a texture without unpacking anything.
#[get("/api/slice")]
pub async fn slice(data: web::Data<AppState>, query: web::Query<SliceQuery>) -> impl Responder {
    let q = query.into_inner();
    let encoding = TileEncoding::parse(q.encoding.as_deref());
    let axis = PlaneAxis::parse(q.axis.as_deref());

    let (layer_id, store) = {
        let session = data.session.read().await;
        match session.resolve(q.layer.as_deref()) {
            Some(layer) => match layer.data.store() {
                Some(store) => (layer.id.clone(), store.clone()),
                None => {
                    return HttpResponse::BadRequest()
                        .body(format!("layer {} holds no image data", layer.id))
                }
            },
            None => return HttpResponse::NotFound().body("No such layer"),
        }
    };

    let (height, width) = match store.plane_shape(q.level, axis) {
        Ok(shape) => shape,
        Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
    };

    // Planes are keyed like tiles: the axis rides in the projection slot, which
    // is free for a plane and keeps one cache rather than two.
    let key = TileKey {
        layer: layer_id,
        level: q.level,
        t: q.t,
        c: q.c,
        z: q.index,
        y: 0,
        x: 0,
        h: height,
        w: width,
        encoding: encoding.as_str(),
        projection: Some((axis.as_str(), q.index, q.index.saturating_add(1))),
    };
    if let Some(bytes) = data.cache.get(&key) {
        let dtype = wire_dtype(&store, q.level, encoding);
        return plane_response(&bytes, &dtype, width, height, true);
    }

    let request = PlaneRequest {
        level: q.level,
        t: q.t,
        c: q.c,
        axis,
        index: q.index,
        encoding,
    };
    match store.read_plane(&request).await {
        Ok(plane) => {
            let bytes = Arc::new(plane.bytes);
            data.cache.put(key, bytes.clone());
            plane_response(&bytes, &plane.dtype, plane.width, plane.height, false)
        }
        Err(e) => {
            log::error!("Slice read error: {e:#}");
            HttpResponse::InternalServerError().body(format!("Error: {e:#}"))
        }
    }
}

fn plane_response(bytes: &[u8], dtype: &str, w: u64, h: u64, cached: bool) -> HttpResponse {
    tile_response(bytes, dtype, w, h, cached)
}

/// Query parameters for /api/value.
#[derive(Deserialize)]
pub struct ValueQuery {
    level: usize,
    #[serde(default)]
    layer: Option<String>,
    #[serde(default)]
    t: u64,
    #[serde(default)]
    c: u64,
    #[serde(default)]
    z: u64,
    y: u64,
    x: u64,
}

/// Handle GET /api/value — the value of one voxel.
///
/// This is what clicking a label answers: the id under the cursor, read from
/// the array in its own dtype. Doing it here rather than reading back from the
/// GPU keeps the client from having to hold every label tile in memory, and it
/// is one cached 1x1 read.
#[get("/api/value")]
pub async fn voxel_value(
    data: web::Data<AppState>,
    query: web::Query<ValueQuery>,
) -> impl Responder {
    let q = query.into_inner();
    let store = {
        let session = data.session.read().await;
        match session
            .resolve(q.layer.as_deref())
            .and_then(|l| l.data.store())
        {
            Some(store) => store.clone(),
            None => return HttpResponse::NotFound().body("No such layer"),
        }
    };

    let request = TileRequest::new(q.level, q.y, q.x, 1, 1)
        .at(q.t, q.c, q.z)
        .encoded(TileEncoding::Raw);
    match store.read_tile_bytes(&request).await {
        Ok(voxel) => {
            let integer = integer_value(&voxel.bytes, &voxel.dtype);
            let float = crate::zarr_reader::bytes_to_f32(&voxel.bytes, &voxel.dtype)
                .ok()
                .and_then(|v| v.first().copied());
            let region = integer
                .and_then(|id| data.ontology.as_ref().and_then(|o| o.get(id)))
                .cloned();
            HttpResponse::Ok().json(serde_json::json!({
                "dtype": voxel.dtype,
                "id": integer,
                "value": float,
                "y": q.y,
                "x": q.x,
                "z": q.z,
                "name": region.as_ref().map(|r| r.name.clone()),
                "acronym": region.as_ref().and_then(|r| r.acronym.clone()),
            }))
        }
        Err(e) => HttpResponse::InternalServerError().body(format!("Error: {e:#}")),
    }
}

/// The voxel as an exact integer, for the dtypes where that is what it is.
///
/// `None` for a float array: a float id is not an id, and reporting one as an
/// integer would invent precision the array does not have.
fn integer_value(bytes: &[u8], dtype: &str) -> Option<u64> {
    let value = match dtype {
        "uint8" => *bytes.first()? as u64,
        "int8" => (*bytes.first()? as i8).max(0) as u64,
        "uint16" => u16::from_le_bytes(bytes.get(..2)?.try_into().ok()?) as u64,
        "int16" => i16::from_le_bytes(bytes.get(..2)?.try_into().ok()?).max(0) as u64,
        "uint32" => u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as u64,
        "int32" => i32::from_le_bytes(bytes.get(..4)?.try_into().ok()?).max(0) as u64,
        "uint64" => u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?),
        _ => return None,
    };
    Some(value)
}

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
        match session.get(&q.layer).and_then(|layer| layer.data.objects()) {
            Some(store) => store.clone(),
            None => return HttpResponse::NotFound().body("no such object layer"),
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
        match session.get(&q.layer).and_then(|layer| layer.data.objects()) {
            Some(store) => store.clone(),
            None => return HttpResponse::NotFound().body("no such object layer"),
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

/// Query parameters for /api/regions.
#[derive(Deserialize)]
pub struct RegionsQuery {
    /// The label layer whose ids name the regions.
    labels: String,
    /// The object layer whose rows are counted.
    objects: String,
    /// The label level to sample. Coarser is faster and, for whole regions,
    /// says the same thing.
    #[serde(default)]
    level: Option<usize>,
    /// The most rows to return, most populous first.
    #[serde(default)]
    limit: Option<usize>,
}

/// Handle GET /api/regions — how many objects fall in each region.
///
/// One label plane is read per z the objects occupy, not one voxel per object:
/// a million cells over a few hundred planes is a few hundred reads, and the
/// other way round is a million.
#[get("/api/regions")]
pub async fn regions(data: web::Data<AppState>, query: web::Query<RegionsQuery>) -> impl Responder {
    let q = query.into_inner();
    let (labels, rows_of, world) = {
        let session = data.session.read().await;
        let labels = session.get(&q.labels).and_then(|l| l.data.store()).cloned();
        let table = session
            .get(&q.objects)
            .and_then(|l| l.data.objects())
            .cloned();
        // Object positions are in the *session's* world — the reference image
        // layer's full-resolution grid — not in the label layer's own. A label
        // volume at half resolution is sampled at half the coordinate, and
        // taking its own extent as the world would cancel exactly that.
        let reference = session
            .default_layer()
            .and_then(|l| l.data.store())
            .or(labels.as_ref())
            .cloned();
        match (labels, table, reference) {
            (Some(labels), Some(table), Some(reference)) => {
                let world = [
                    reference.axis_extent(0, "z").unwrap_or(1).max(1),
                    reference.axis_extent(0, "y").unwrap_or(1).max(1),
                    reference.axis_extent(0, "x").unwrap_or(1).max(1),
                ];
                (labels, table, world)
            }
            _ => return HttpResponse::NotFound().body("need a label layer and an object layer"),
        }
    };

    let level = q.level.unwrap_or(0);
    let (Ok(label_z), Ok(label_y), Ok(label_x)) = (
        labels.axis_extent(level, "z"),
        labels.axis_extent(level, "y"),
        labels.axis_extent(level, "x"),
    ) else {
        return HttpResponse::BadRequest().body("that level is outside the label layer");
    };

    let [world_z, world_y, world_x] = world;

    let mut by_plane: std::collections::BTreeMap<u64, Vec<(u64, u64)>> =
        std::collections::BTreeMap::new();
    for row in 0..rows_of.len() {
        let Some(position) = rows_of.world_position(row) else {
            continue;
        };
        let z = ((position[0] as f64 / world_z as f64) * label_z as f64) as u64;
        let y = ((position[1] as f64 / world_y as f64) * label_y as f64) as u64;
        let x = ((position[2] as f64 / world_x as f64) * label_x as f64) as u64;
        by_plane
            .entry(z.min(label_z.saturating_sub(1)))
            .or_default()
            .push((
                y.min(label_y.saturating_sub(1)),
                x.min(label_x.saturating_sub(1)),
            ));
    }

    let mut counts: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    for (z, points) in by_plane {
        let plane = match labels
            .read_plane(&PlaneRequest {
                level,
                t: 0,
                c: 0,
                axis: PlaneAxis::Z,
                index: z,
                encoding: TileEncoding::Raw,
            })
            .await
        {
            Ok(plane) => plane,
            Err(e) => return HttpResponse::InternalServerError().body(format!("Error: {e:#}")),
        };
        let width = plane.width.max(1);
        for (y, x) in points {
            let at = (y * width + x) as usize;
            if let Some(id) = id_at(&plane.bytes, &plane.dtype, at) {
                *counts.entry(id).or_insert(0) += 1;
            }
        }
    }

    let mut rows: Vec<RegionCount> = counts
        .into_iter()
        .map(|(id, count)| {
            let region = data.ontology.as_ref().and_then(|o| o.get(id));
            RegionCount {
                id,
                name: region.map(|r| r.name.clone()),
                acronym: region.and_then(|r| r.acronym.clone()),
                count,
            }
        })
        .collect();
    // Most populous first, and ties broken by id so the answer is stable.
    rows.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
    if let Some(limit) = q.limit {
        rows.truncate(limit);
    }
    HttpResponse::Ok().json(rows)
}

/// One id out of a raw plane.
fn id_at(bytes: &[u8], dtype: &str, index: usize) -> Option<u64> {
    let width = match dtype {
        "uint8" | "int8" => 1,
        "uint16" | "int16" => 2,
        "uint32" | "int32" => 4,
        "uint64" | "int64" => 8,
        _ => return None,
    };
    let at = index * width;
    integer_value(bytes.get(at..at + width)?, dtype)
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

// ---------------------------------------------------------------------------
// Annotations
//
// The only mutable thing in a session, and so the only part of the API that is
// not a read. Annotations live in memory until `save` writes them into a store
// as an ngio ROI table; nothing here writes on its own, because a viewer that
// silently edits the data it was pointed at is a viewer nobody can trust with a
// dataset.
// ---------------------------------------------------------------------------

/// Body of POST /api/annotations/layers.
#[derive(Deserialize)]
pub struct NewAnnotationLayer {
    #[serde(default)]
    pub name: Option<String>,
}

/// Handle POST /api/annotations/layers — append an empty annotation layer.
#[post("/api/annotations/layers")]
pub async fn add_annotation_layer(
    data: web::Data<AppState>,
    body: web::Json<NewAnnotationLayer>,
) -> impl Responder {
    let mut session = data.session.write().await;
    let name = body.into_inner().name.filter(|n| !n.trim().is_empty());
    let id = session.add_annotations(name, AnnotationSet::new());
    log::info!("added annotation layer {id}");
    HttpResponse::Ok().json(session.info())
}

/// Handle GET /api/annotations/{layer} — every annotation in one layer.
#[get("/api/annotations/{layer}")]
pub async fn annotations(data: web::Data<AppState>, path: web::Path<String>) -> impl Responder {
    let id = path.into_inner();
    let session = data.session.read().await;
    match session.get(&id).and_then(|layer| layer.data.annotations()) {
        Some(set) => HttpResponse::Ok().json(set.items()),
        None => HttpResponse::NotFound().body(format!("no annotation layer {id}")),
    }
}

/// Handle POST /api/annotations/{layer} — add one annotation, id assigned here.
#[post("/api/annotations/{layer}")]
pub async fn add_annotation(
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<Annotation>,
) -> impl Responder {
    let id = path.into_inner();
    let mut session = data.session.write().await;
    match session.annotations_mut(&id) {
        // Nested, not merely appended: a shape drawn inside a region becomes a
        // child of it, which is how QuPath's hierarchy works.
        Some(set) => HttpResponse::Ok().json(set.add_nested(body.into_inner())),
        None => HttpResponse::NotFound().body(format!("no annotation layer {id}")),
    }
}

/// Handle PUT /api/annotations/{layer}/{id} — replace one annotation's geometry
/// and class, keeping its id.
#[put("/api/annotations/{layer}/{id}")]
pub async fn update_annotation(
    data: web::Data<AppState>,
    path: web::Path<(String, u64)>,
    body: web::Json<Annotation>,
) -> impl Responder {
    let (layer, annotation) = path.into_inner();
    let mut session = data.session.write().await;
    let Some(set) = session.annotations_mut(&layer) else {
        return HttpResponse::NotFound().body(format!("no annotation layer {layer}"));
    };
    match set.update(annotation, body.into_inner()) {
        Ok(updated) => HttpResponse::Ok().json(updated),
        Err(e) => HttpResponse::NotFound().body(format!("Error: {e:#}")),
    }
}

/// Handle DELETE /api/annotations/{layer}/{id} — drop one annotation.
#[delete("/api/annotations/{layer}/{id}")]
pub async fn remove_annotation(
    data: web::Data<AppState>,
    path: web::Path<(String, u64)>,
) -> impl Responder {
    let (layer, annotation) = path.into_inner();
    let mut session = data.session.write().await;
    let Some(set) = session.annotations_mut(&layer) else {
        return HttpResponse::NotFound().body(format!("no annotation layer {layer}"));
    };
    if set.remove(annotation) {
        HttpResponse::Ok().json(serde_json::json!({ "removed": annotation }))
    } else {
        HttpResponse::NotFound().body(format!("no annotation {annotation}"))
    }
}

/// Handle POST /api/annotations/{layer}/renest — rebuild the hierarchy from
/// where the shapes now are.
#[post("/api/annotations/{layer}/renest")]
pub async fn renest_annotations(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let id = path.into_inner();
    let mut session = data.session.write().await;
    match session.annotations_mut(&id) {
        Some(set) => {
            set.renest();
            HttpResponse::Ok().json(set.items())
        }
        None => HttpResponse::NotFound().body(format!("no annotation layer {id}")),
    }
}

/// Handle POST /api/annotations/{layer}/{id}/detach — make it top-level.
#[post("/api/annotations/{layer}/{id}/detach")]
pub async fn detach_annotation(
    data: web::Data<AppState>,
    path: web::Path<(String, u64)>,
) -> impl Responder {
    let (layer, annotation) = path.into_inner();
    let mut session = data.session.write().await;
    let Some(set) = session.annotations_mut(&layer) else {
        return HttpResponse::NotFound().body(format!("no annotation layer {layer}"));
    };
    // Detaching something already top-level is not an error: the caller asked
    // for it to have no parent, and it has none. Either way the answer is the
    // layer as it now stands.
    set.detach(annotation);
    HttpResponse::Ok().json(set.items())
}

/// Body of POST /api/annotations/{layer}/save.
#[derive(Deserialize)]
pub struct SaveAnnotations {
    /// `<store>.zarr/tables/<name>`, or absent to rewrite where this layer was
    /// read from or last saved.
    #[serde(default)]
    pub target: Option<String>,
    /// World pixels to file units, `z,y,x`. Absent takes the reference image's
    /// own `coordinateTransformations` scale — see [`roi_table::world_scale`].
    #[serde(default)]
    pub voxel: Option<[f64; 3]>,
    /// Seconds per frame, for `t_second`. Absent takes the same source.
    #[serde(default)]
    pub seconds: Option<f64>,
}

/// Handle POST /api/annotations/{layer}/save — write the layer out.
///
/// **Which format is decided by the target's shape**, the same way opening one
/// is: `<store>/annotations/<name>` or a `.geojson` path writes GeoJSON, and
/// `<store>/tables/<name>` writes an ngio ROI table. The two are not
/// interchangeable — a table holds axis-aligned boxes and nothing else — so a
/// save that would flatten a polygon says how many it flattened rather than
/// doing it quietly.
///
/// A remote target needs `--allow-remote-writes`. Credentials handed to a viewer
/// so it can *read* a bucket must not silently become write access to it: the
/// operator who configured the profile said "show me this", not "change this",
/// and only they can say otherwise.
#[post("/api/annotations/{layer}/save")]
pub async fn save_annotations(
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<SaveAnnotations>,
) -> impl Responder {
    let layer = path.into_inner();
    let body = body.into_inner();

    let (rows, target) = {
        let session = data.session.read().await;
        let Some(set) = session.get(&layer).and_then(|l| l.data.annotations()) else {
            return HttpResponse::NotFound().body(format!("no annotation layer {layer}"));
        };
        let target = body
            .target
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| set.target().map(str::to_string));
        (set.items().to_vec(), target)
    };
    let Some(target) = target else {
        return HttpResponse::BadRequest().body(
            "no target: say where to write, as <store>.zarr/annotations/<name>, \
             a .geojson path, or <store>.zarr/tables/<name> for an ROI table",
        );
    };
    let remote = roi_table::is_remote(&target);
    if remote && !data.allow_remote_writes {
        return HttpResponse::Forbidden().body(
            "this server will not write to a remote store; start it with --allow-remote-writes",
        );
    }

    // GeoJSON first: it is the lossless form, and the ROI table is the one that
    // has to be asked for by naming a `tables/` path.
    let is_geojson = geojson::is_annotation_target(&target)
        || target.trim_end().ends_with(".geojson")
        || target.trim_end().ends_with(".json");
    if is_geojson {
        let written = if geojson::is_annotation_target(&target) {
            if remote {
                match geojson::split_uri_target(&target) {
                    Ok((store, name)) => {
                        geojson::save_async(&data.registry, &store, &name, &rows).await
                    }
                    Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
                }
            } else {
                match geojson::split_target(&target) {
                    Ok((root, name)) => geojson::save(&root, &name, &rows),
                    Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
                }
            }
        } else if remote {
            // A bare `.geojson` path is a *file*, and a bucket has no files —
            // only objects inside a store. Naming the set is what makes it
            // addressable, so the error says how.
            return HttpResponse::BadRequest().body(
                "a remote target must name an annotation set, as \
                 <store>.zarr/annotations/<name>",
            );
        } else {
            let path = std::path::PathBuf::from(target.trim_start_matches("file://"));
            geojson::save_file(&path, &rows).map(|()| path.display().to_string())
        };
        return match written {
            Ok(written) => {
                log::info!("wrote {} annotation(s) to {written}", rows.len());
                let mut session = data.session.write().await;
                if let Some(set) = session.annotations_mut(&layer) {
                    set.set_target(written.clone());
                }
                HttpResponse::Ok().json(serde_json::json!({
                    "target": written,
                    "rows": rows.len(),
                    "format": "geojson",
                    "flattened": 0,
                }))
            }
            Err(e) => {
                log::error!("saving annotations to {target}: {e:#}");
                HttpResponse::InternalServerError().body(format!("Error: {e:#}"))
            }
        };
    }

    let scale = {
        let session = data.session.read().await;
        let mut scale = session
            .reference_dataset()
            .map(roi_table::world_scale_of)
            .unwrap_or_default();
        if let Some(voxel) = body.voxel {
            scale.voxel = voxel;
        }
        if let Some(seconds) = body.seconds.filter(|s| *s > 0.0) {
            scale.seconds = seconds;
        }
        scale
    };

    let written = if remote {
        let (store, name) = match roi_table::split_uri_target(&target) {
            Ok(split) => split,
            Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
        };
        roi_table::write_async(&data.registry, &store, &name, &rows, scale).await
    } else {
        match roi_table::split_target(&target) {
            Ok((root, name)) => roi_table::write(&root, &name, &rows, scale),
            Err(e) => return HttpResponse::BadRequest().body(format!("Error: {e:#}")),
        }
    };
    let written = match written {
        Ok(written) => written,
        Err(e) => {
            log::error!("saving annotations to {target}: {e:#}");
            return HttpResponse::InternalServerError().body(format!("Error: {e:#}"));
        }
    };
    // How many shapes an ROI table could not hold, so the client can say so
    // rather than letting the user find out on the round trip.
    let flattened = roi_table::lossy_rows(&rows);
    log::info!(
        "wrote {} annotation(s) to {written}, {flattened} as bounding boxes",
        rows.len()
    );

    let mut session = data.session.write().await;
    if let Some(set) = session.annotations_mut(&layer) {
        set.set_target(written.clone());
    }
    HttpResponse::Ok().json(serde_json::json!({
        "target": written,
        "rows": rows.len(),
        "format": "roi_table",
        "flattened": flattened,
        "voxel": scale.voxel,
        "seconds": scale.seconds,
    }))
}

// ---------------------------------------------------------------------------
// Table layers
//
// A feature or condition table has no geometry, so it is read rather than
// drawn — and where it names a label image, one of its columns can colour that
// image's ids.
// ---------------------------------------------------------------------------

/// Query parameters for /api/tables/{layer}/rows.
#[derive(Deserialize)]
pub struct TableRowsQuery {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_page")]
    limit: usize,
}

fn default_page() -> usize {
    200
}

/// Handle GET /api/tables/{layer}/rows — a page of a table, as text.
///
/// Paged because a feature table has a row per segmented object, and a hundred
/// thousand of them is not something to push through a session read.
#[get("/api/tables/{layer}/rows")]
pub async fn table_rows(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<TableRowsQuery>,
) -> impl Responder {
    let id = path.into_inner();
    let q = query.into_inner();
    let session = data.session.read().await;
    let Some(table) = session.table(&id) else {
        return HttpResponse::NotFound().body(format!("no table layer {id}"));
    };
    let names: Vec<String> = table
        .columns
        .names()
        .iter()
        .map(|n| n.to_string())
        .collect();
    let total = table.columns.row_count();
    // Saturating for the same reason: `offset` comes off the query string, so
    // `?offset=18446744073709551615` would otherwise panic here.
    let end = q.offset.saturating_add(q.limit.min(5000)).min(total);
    let rows: Vec<Vec<String>> = (q.offset.min(total)..end)
        .map(|row| {
            names
                .iter()
                .map(|name| table.columns.string(name, row).unwrap_or_default())
                .collect()
        })
        .collect();
    HttpResponse::Ok().json(serde_json::json!({
        "columns": names,
        "offset": q.offset,
        "total": total,
        "rows": rows,
    }))
}

/// Query parameters for /api/tables/{layer}/column.
#[derive(Deserialize)]
pub struct TableColumnQuery {
    name: String,
}

/// Handle GET /api/tables/{layer}/column — one column paired with label ids.
///
/// This is the join a feature table exists for: the ids come from the table's
/// `instance_key`, the values from the named column, and together they colour a
/// label image by a measurement.
#[get("/api/tables/{layer}/column")]
pub async fn table_column(
    data: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<TableColumnQuery>,
) -> impl Responder {
    let id = path.into_inner();
    let name = query.into_inner().name;
    let session = data.session.read().await;
    let Some(table) = session.table(&id) else {
        return HttpResponse::NotFound().body(format!("no table layer {id}"));
    };
    match table.column_by_label(&name) {
        Some((labels, values)) => HttpResponse::Ok().json(serde_json::json!({
            "column": name,
            "labels": labels,
            "values": values,
        })),
        None => HttpResponse::BadRequest()
            .body(format!("`{name}` is not a numeric column of this table")),
    }
}

/// Query parameters for /api/annotations/tables.
#[derive(Deserialize)]
pub struct TablesQuery {
    /// A store to look inside. Absent uses the reference layer's source.
    #[serde(default)]
    store: Option<String>,
}

/// Handle GET /api/annotations/tables — the ROI tables a store already holds.
///
/// This is what turns "open an annotation layer" from a path the user must
/// remember into a list they can pick from. A store with no `tables` group is
/// not an error — it is the normal case — so this answers with an empty list
/// rather than a status the client would have to special-case.
#[get("/api/annotations/tables")]
pub async fn list_tables(
    data: web::Data<AppState>,
    query: web::Query<TablesQuery>,
) -> impl Responder {
    let store = match query.into_inner().store.filter(|s| !s.trim().is_empty()) {
        Some(store) => store,
        None => {
            let session = data.session.read().await;
            match session.default_layer().map(|layer| layer.spec.uri()) {
                Some(uri) => uri,
                None => {
                    return HttpResponse::Ok()
                        .json(serde_json::json!({"store": null, "tables": []}))
                }
            }
        }
    };

    let (tables, sets, shown, error) = if roi_table::is_remote(&store) {
        let tables = roi_table::list_async(&data.registry, &store).await;
        let sets = geojson::list_async(&data.registry, &store).await;
        let error = tables
            .as_ref()
            .err()
            .or(sets.as_ref().err())
            .map(|e| format!("{e:#}"));
        (
            tables.unwrap_or_default(),
            sets.unwrap_or_default(),
            store.clone(),
            error,
        )
    } else {
        let root = std::path::PathBuf::from(store.trim_start_matches("file://"));
        let shown = root.display().to_string();
        let tables = roi_table::list(&root);
        let sets = geojson::list(&root);
        let error = tables
            .as_ref()
            .err()
            .or(sets.as_ref().err())
            .map(|e| format!("{e:#}"));
        (
            tables.unwrap_or_default(),
            sets.unwrap_or_default(),
            shown,
            error,
        )
    };

    HttpResponse::Ok().json(serde_json::json!({
        "store": shown,
        "tables": tables,
        "annotations": sets,
        "writable": !roi_table::is_remote(&shown) || data.allow_remote_writes,
        "error": error,
    }))
}
