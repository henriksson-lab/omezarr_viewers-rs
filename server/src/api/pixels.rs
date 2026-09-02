//! Routes that read voxels: one tile, one whole plane, one value, and the
//! per-region counts that fall out of sampling a label volume.
//!
//! These are the hot paths, so they share the tile cache and answer with bare
//! bytes plus headers rather than JSON — the client uploads the body to a
//! texture without unpacking anything.

use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;
use std::sync::Arc;

use omezarr_viewer_common::TileCoords;

use crate::cache::TileKey;
use crate::ontology::RegionCount;
use crate::zarr_reader::{PlaneAxis, PlaneRequest, Projection, TileEncoding, TileRequest};

use super::{
    bad_level, check_channel, check_level, layer_objects, layer_store, resolve_store, AppState,
};

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
impl TileQuery {
    /// The eight numbers that say which tile, in the shared crate's spelling.
    ///
    /// A conversion rather than an embedded `TileCoords`: `web::Query` goes
    /// through `serde_urlencoded`, which cannot flatten a nested struct out of
    /// a flat query string.
    fn coords(&self) -> TileCoords {
        TileCoords {
            level: self.level,
            t: self.t,
            c: self.c,
            z: self.z,
            y: self.y,
            x: self.x,
            h: self.h,
            w: self.w,
        }
    }
}

#[get("/api/tile")]
pub async fn tile(data: web::Data<AppState>, query: web::Query<TileQuery>) -> impl Responder {
    let q = query.into_inner();
    let encoding = TileEncoding::parse(q.encoding.as_deref());
    let projection = Projection::parse(q.zproj.as_deref());
    let depth = q.depth.unwrap_or(1).max(1);

    let (layer_id, store) = {
        let session = data.session.read().await;
        match resolve_store(&session, q.layer.as_deref()) {
            Ok(found) => found,
            Err(res) => return res,
        }
    };
    // Checked here rather than left to the reader: the reader's error is an
    // `anyhow::Error` like any other, and the blanket 500 it used to land in
    // said "come back later" about a level that will never exist.
    if let Err(res) = check_level(&store, &layer_id, q.level) {
        return res;
    }
    if let Err(res) = check_channel(&store, &layer_id, q.level, q.c) {
        return res;
    }

    let coords = q.coords();
    let range = coords.z_range(depth);
    let key = TileKey {
        layer: layer_id,
        coords,
        encoding: encoding.as_str(),
        projection: projection.map(|p| (p.as_str(), range.start, range.end)),
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
        match resolve_store(&session, q.layer.as_deref()) {
            Ok(found) => found,
            Err(res) => return res,
        }
    };

    // The only way a plane has no shape is a level the volume does not have,
    // which is the caller's number and so the caller's error.
    let (height, width) = match store.plane_shape(q.level, axis) {
        Ok(shape) => shape,
        Err(e) => return bad_level(&layer_id, e),
    };
    if let Err(res) = check_channel(&store, &layer_id, q.level, q.c) {
        return res;
    }

    // Planes are keyed like tiles: the axis rides in the projection slot, which
    // is free for a plane and keeps one cache rather than two.
    // A plane is keyed as a one-deep tile at the axis index; `z_range` carries
    // the same saturating arithmetic the tile path uses.
    let coords = TileCoords {
        level: q.level,
        t: q.t,
        c: q.c,
        z: q.index,
        y: 0,
        x: 0,
        h: height,
        w: width,
    };
    let plane = coords.z_range(1);
    let key = TileKey {
        layer: layer_id,
        coords,
        encoding: encoding.as_str(),
        projection: Some((axis.as_str(), plane.start, plane.end)),
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
    let (layer_id, store) = {
        let session = data.session.read().await;
        match resolve_store(&session, q.layer.as_deref()) {
            Ok(found) => found,
            Err(res) => return res,
        }
    };
    if let Err(res) = check_level(&store, &layer_id, q.level) {
        return res;
    }
    if let Err(res) = check_channel(&store, &layer_id, q.level, q.c) {
        return res;
    }

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
        let labels = match layer_store(&session, &q.labels) {
            Ok(store) => store,
            Err(res) => return res,
        };
        let table = match layer_objects(&session, &q.objects) {
            Ok(store) => store,
            Err(res) => return res,
        };
        // Object positions are in the *session's* world — the reference image
        // layer's full-resolution grid — not in the label layer's own. A label
        // volume at half resolution is sampled at half the coordinate, and
        // taking its own extent as the world would cancel exactly that.
        let reference = session
            .default_layer()
            .and_then(|l| l.data.store())
            .cloned()
            .unwrap_or_else(|| labels.clone());
        let world = [
            reference.axis_extent(0, "z").unwrap_or(1).max(1),
            reference.axis_extent(0, "y").unwrap_or(1).max(1),
            reference.axis_extent(0, "x").unwrap_or(1).max(1),
        ];
        (labels, table, world)
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
