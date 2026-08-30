//! Routes over the tables a store holds.
//!
//! A page of a table layer's rows, one of its columns paired with the label ids
//! it measures, and the listing that tells a client which ROI tables and
//! annotation sets a store already carries.

use actix_web::{get, web, HttpResponse, Responder};
use serde::Deserialize;

use crate::annotations::{geojson, roi_table};

use super::{layer_table, AppState};

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
    let table = match layer_table(&session, &id) {
        Ok(table) => table,
        Err(res) => return res,
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
    let table = match layer_table(&session, &id) {
        Ok(table) => table,
        Err(res) => return res,
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
