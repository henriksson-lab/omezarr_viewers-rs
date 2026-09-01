//! Routes that edit annotation layers — the only mutable part of the API.
//!
//! Adding, updating, removing, re-nesting and detaching a shape, plus the save
//! that writes a layer out as QuPath GeoJSON or as an ngio ROI table. Which
//! form a save takes is decided by the shape of the target path, and a remote
//! target needs `--allow-remote-writes`.

use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use serde::Deserialize;

use omezarr_viewer_common::Annotation;

use crate::annotations::{geojson, roi_table, AnnotationSet};

use super::{layer_annotations, layer_annotations_mut, AppState};

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
    match layer_annotations(&session, &id) {
        Ok(set) => HttpResponse::Ok().json(set.items()),
        Err(res) => res,
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
    match layer_annotations_mut(&mut session, &id) {
        // Nested, not merely appended: a shape drawn inside a region becomes a
        // child of it, which is how QuPath's hierarchy works.
        Ok(set) => HttpResponse::Ok().json(set.add_nested(body.into_inner())),
        Err(res) => res,
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
    let set = match layer_annotations_mut(&mut session, &layer) {
        Ok(set) => set,
        Err(res) => return res,
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
    let set = match layer_annotations_mut(&mut session, &layer) {
        Ok(set) => set,
        Err(res) => return res,
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
    match layer_annotations_mut(&mut session, &id) {
        Ok(set) => {
            set.renest();
            HttpResponse::Ok().json(set.items())
        }
        Err(res) => res,
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
    let set = match layer_annotations_mut(&mut session, &layer) {
        Ok(set) => set,
        Err(res) => return res,
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
/// Remember where a layer last saved, so an argument-less save goes back there.
///
/// Beside every successful write rather than inside one branch: a save path that
/// forgot it would leave the set looking unsaved and send the next save
/// somewhere else. Both forms — the GeoJSON set and the ROI table — record it
/// the same way.
async fn record_target(data: &web::Data<AppState>, layer: &str, written: &str) {
    let mut session = data.session.write().await;
    if let Some(set) = session.annotations_mut(layer) {
        set.set_target(written.to_string());
    }
}

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
        let set = match layer_annotations(&session, &layer) {
            Ok(set) => set,
            Err(res) => return res,
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
                record_target(&data, &layer, &written).await;
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

    record_target(&data, &layer, &written).await;
    HttpResponse::Ok().json(serde_json::json!({
        "target": written,
        "rows": rows.len(),
        "format": "roi_table",
        "flattened": flattened,
        "voxel": scale.voxel,
        "seconds": scale.seconds,
    }))
}
