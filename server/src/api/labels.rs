//! Classing the ids in a label image.
//!
//! The label image itself is never written. A class is an assertion *about*
//! somebody else's raster — usually a segmentation a model produced — so it is
//! held beside the layer and saved to a feature table joined by label id. That
//! is also what makes it cheap: the instances already exist, so there is no
//! brush, no rasterisation and nothing to resample.
//!
//! # Route order
//!
//! `/classes/save` and `/classes/{id}` are the literal-versus-parameter shape
//! that `configure`'s doc warns about. They are safe here only because the
//! methods differ — `POST` for the save, `PUT`/`DELETE` for an id — and a test
//! in `api_labels.rs` pins that rather than trusting it, because the day
//! somebody adds `POST /classes/{id}` the save silently becomes an id named
//! "save".

use actix_web::{delete, get, post, put, web, HttpResponse, Responder};
use serde::Deserialize;

use crate::annotations::roi_table::classes;

use super::{no_such_layer, wrong_kind, AppState};

/// Resolve a label layer, or say which of the two things went wrong.
fn label_layer(session: &crate::session::Session, id: &str) -> Result<(), HttpResponse> {
    match session.get(id) {
        None => Err(no_such_layer(Some(id))),
        Some(layer) if session.label_classes(id).is_none() => {
            Err(wrong_kind(id, layer.role(), "is not a label image"))
        }
        Some(_) => Ok(()),
    }
}

/// Handle GET /api/labels/{layer}/classes — every id that has been classed.
#[get("/api/labels/{layer}/classes")]
pub async fn label_classes(data: web::Data<AppState>, path: web::Path<String>) -> impl Responder {
    let layer = path.into_inner();
    let session = data.session.read().await;
    if let Err(res) = label_layer(&session, &layer) {
        return res;
    }
    let classes = session.label_classes(&layer).expect("checked above");
    HttpResponse::Ok().json(serde_json::json!({
        "assigned": classes
            .iter()
            .map(|(id, class)| serde_json::json!({"id": id, "class": class}))
            .collect::<Vec<_>>(),
        "classes": classes.classes(),
    }))
}

#[derive(Deserialize)]
pub struct SetClass {
    /// The empty string is a class: "looked at, nothing in particular". To say
    /// "not looked at", delete the assignment instead.
    class: String,
}

/// Handle PUT /api/labels/{layer}/classes/{id} — class one id.
#[put("/api/labels/{layer}/classes/{id}")]
pub async fn set_label_class(
    data: web::Data<AppState>,
    path: web::Path<(String, u64)>,
    body: web::Json<SetClass>,
) -> impl Responder {
    let (layer, id) = path.into_inner();
    let mut session = data.session.write().await;
    if let Err(res) = label_layer(&session, &layer) {
        return res;
    }
    let classes = session.label_classes_mut(&layer).expect("checked above");
    classes.set(id, body.into_inner().class);
    HttpResponse::Ok().json(serde_json::json!({"id": id, "assigned": classes.len()}))
}

/// Handle DELETE /api/labels/{layer}/classes/{id} — back to unexamined.
#[delete("/api/labels/{layer}/classes/{id}")]
pub async fn clear_label_class(
    data: web::Data<AppState>,
    path: web::Path<(String, u64)>,
) -> impl Responder {
    let (layer, id) = path.into_inner();
    let mut session = data.session.write().await;
    if let Err(res) = label_layer(&session, &layer) {
        return res;
    }
    let classes = session.label_classes_mut(&layer).expect("checked above");
    classes.clear(id);
    HttpResponse::Ok().json(serde_json::json!({"id": id, "assigned": classes.len()}))
}

#[derive(Deserialize)]
pub struct SaveClasses {
    /// `<store>.zarr/tables/<name>`, the same shape a table save takes.
    target: String,
    /// The label image the ids belong to, relative to the table group — e.g.
    /// `../labels/nuclei`. Without it the table is a column of numbers joined
    /// to nothing.
    region: String,
}

/// Handle POST /api/labels/{layer}/classes/save — write the feature table.
#[post("/api/labels/{layer}/classes/save")]
pub async fn save_label_classes(
    data: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<SaveClasses>,
) -> impl Responder {
    let layer = path.into_inner();
    let SaveClasses { target, region } = body.into_inner();
    let session = data.session.read().await;
    if let Err(res) = label_layer(&session, &layer) {
        return res;
    }
    if target.trim().is_empty() {
        return HttpResponse::BadRequest().body("a save needs somewhere to write");
    }
    // The same gate the annotation saves pass: credentials given to read a
    // bucket must not silently become write access to it.
    if crate::annotations::roi_table::is_remote(&target) && !data.allow_remote_writes {
        return HttpResponse::Forbidden().body(format!(
            "{target} is remote, and this server was not started with --allow-remote-writes"
        ));
    }
    let assigned = session.label_classes(&layer).expect("checked above");
    let (root, name) = match crate::annotations::roi_table::split_target(&target) {
        Ok(split) => split,
        Err(e) => return HttpResponse::BadRequest().body(format!("{e:#}")),
    };
    match classes::write(&root, &name, &region, assigned) {
        Ok(written) => HttpResponse::Ok().json(serde_json::json!({
            "target": written,
            "rows": assigned.len(),
            "format": "feature_table",
            "region": region,
        })),
        Err(e) => {
            log::error!("saving label classes to {target}: {e:#}");
            HttpResponse::InternalServerError().body(format!("{e:#}"))
        }
    }
}
