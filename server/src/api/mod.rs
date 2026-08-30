//! The HTTP API: shared state, the route table, and the layer lookups every
//! route opens with.
//!
//! The handlers themselves live one module down, grouped by what they serve —
//! the session, pixels, objects, annotations, tables — and are re-exported here
//! so `api::tile` and friends stay where `main` and the desktop shell expect
//! them.

use actix_web::{web, HttpResponse};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::annotations::roi_table::RoiTable;
use crate::annotations::AnnotationSet;
use crate::cache::TileCache;
use crate::objects::ObjectStore;
use crate::ontology::Ontology;
use crate::session::Session;
use crate::source::SourceRegistry;
use crate::volume::Volume;
use crate::zarr_reader::S3Config;

// Two of these are named for their route group with a suffix rather than
// outright: an actix route macro expands to a struct of the handler's name, and
// `objects` and `annotations` are both handlers *and* groups. A module and a
// struct share the type namespace, so a `mod objects` would shadow the
// re-exported `objects` route and `api::objects` would stop naming a service.
mod annotation_routes;
mod object_routes;
mod pixels;
mod session;
mod tables;

pub use annotation_routes::*;
pub use object_routes::*;
pub use pixels::*;
pub use session::*;
pub use tables::*;

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

// ---------------------------------------------------------------------------
// Resolving a layer
//
// Nearly every route opens the same way: read the session, find the layer the
// request names, ask it for the kind of data the route serves. Written out per
// handler, the copies drifted — a layer that existed but held no pixels was a
// 400 from `/api/tile` and a 404 from `/api/value`, for the same mistake — and
// a status that means different things on different routes is one no client can
// act on. So the decision lives here, once:
//
//   * an id no layer has            -> 404, naming the id
//   * a layer of the wrong kind     -> 400, naming the layer and what it lacks
//   * a caller's value out of range -> 400, before anything is read
//   * anything else                 -> 500
//
// The first two are what a client most needs to tell apart: "that layer is
// gone, stop asking" against "that layer cannot answer this, ask another".
// ---------------------------------------------------------------------------

/// No layer by that id — or, on a route that falls back to the default layer,
/// no layer at all.
fn no_such_layer(id: Option<&str>) -> HttpResponse {
    match id.filter(|id| !id.is_empty()) {
        Some(id) => HttpResponse::NotFound().body(format!("no layer {id}")),
        None => HttpResponse::NotFound().body("no layer loaded"),
    }
}

/// The layer is open; it is the wrong kind for this route.
///
/// Takes the id and kind rather than the layer, because the annotation routes
/// cannot hold a borrow of the layer for its error message and a mutable borrow
/// of its contents for the edit at the same time.
fn wrong_kind(id: &str, kind: &str, lacks: &str) -> HttpResponse {
    HttpResponse::BadRequest().body(format!("layer {id} {lacks} (layer kind: {kind})"))
}

/// A level the volume does not have.
///
/// The caller chose the level, so this is a bad request. Letting the reader's
/// error become a blanket 500 is what sends a frontend retrying a request that
/// can never succeed.
fn bad_level(id: &str, e: anyhow::Error) -> HttpResponse {
    HttpResponse::BadRequest().body(format!("layer {id}: {e:#}"))
}

/// Refuse a level before a single chunk is read.
fn check_level(store: &Volume, id: &str, level: usize) -> Result<(), HttpResponse> {
    store
        .level_dtype(level)
        .map(|_| ())
        .map_err(|e| bad_level(id, e))
}

/// The pixels behind `layer=`, and the id they came from.
///
/// The id comes back because it is the tile cache key, and the default layer's
/// id is exactly what the request did not say.
fn resolve_store(session: &Session, id: Option<&str>) -> Result<(String, Volume), HttpResponse> {
    let layer = session.resolve(id).ok_or_else(|| no_such_layer(id))?;
    match layer.data.store() {
        Some(store) => Ok((layer.id.clone(), store.clone())),
        None => Err(wrong_kind(&layer.id, layer.role(), "holds no image data")),
    }
}

/// The pixels behind a layer named outright, where there is no default to fall
/// back on.
fn layer_store(session: &Session, id: &str) -> Result<Volume, HttpResponse> {
    Ok(resolve_store(session, Some(id))?.1)
}

/// The rows behind an object layer.
fn layer_objects(session: &Session, id: &str) -> Result<Arc<ObjectStore>, HttpResponse> {
    let layer = session.get(id).ok_or_else(|| no_such_layer(Some(id)))?;
    match layer.data.objects() {
        Some(store) => Ok(store.clone()),
        None => Err(wrong_kind(&layer.id, layer.role(), "holds no objects")),
    }
}

/// The rows behind a table layer — a feature or condition table, which carries
/// no geometry and so is neither an image nor an annotation set.
fn layer_table<'a>(session: &'a Session, id: &str) -> Result<&'a RoiTable, HttpResponse> {
    let layer = session.get(id).ok_or_else(|| no_such_layer(Some(id)))?;
    session
        .table(id)
        .ok_or_else(|| wrong_kind(&layer.id, layer.role(), "is not a table"))
}

/// The annotations of a layer, for reading.
fn layer_annotations<'a>(
    session: &'a Session,
    id: &str,
) -> Result<&'a AnnotationSet, HttpResponse> {
    let layer = session.get(id).ok_or_else(|| no_such_layer(Some(id)))?;
    layer
        .data
        .annotations()
        .ok_or_else(|| wrong_kind(&layer.id, layer.role(), "holds no annotations"))
}

/// The annotations of a layer, for editing.
fn layer_annotations_mut<'a>(
    session: &'a mut Session,
    id: &str,
) -> Result<&'a mut AnnotationSet, HttpResponse> {
    // The kind is read first, and kept as a `&'static str`: the message needs
    // the layer and the edit needs it mutably, and those two borrows cannot
    // overlap.
    let kind = match session.get(id) {
        Some(layer) => layer.role(),
        None => return Err(no_such_layer(Some(id))),
    };
    session
        .annotations_mut(id)
        .ok_or_else(|| wrong_kind(id, kind, "holds no annotations"))
}
