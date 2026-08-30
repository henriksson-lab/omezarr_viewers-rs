//! Annotations, and the tables that sit beside them in a store.

use omezarr_viewer_common::{Annotation, SessionInfo};

use gloo_net::http::Request;

use super::get_host_url;

/// Create an empty annotation layer, returning the new session.
pub async fn add_annotation_layer(name: &str) -> Result<SessionInfo, String> {
    let url = format!("{}/api/annotations/layers", get_host_url());
    let resp = Request::post(&url)
        .json(&serde_json::json!({ "name": name }))
        .map_err(|e| format!("new annotation layer body: {e}"))?
        .send()
        .await
        .map_err(|e| format!("new annotation layer: {e}"))?;
    if !resp.ok() {
        return Err(format!(
            "new annotation layer: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    resp.json::<SessionInfo>()
        .await
        .map_err(|e| format!("parse session: {e}"))
}

/// Add one annotation, returning it with the id the server assigned.
pub async fn add_annotation(layer: &str, annotation: &Annotation) -> Result<Annotation, String> {
    let url = format!("{}/api/annotations/{}", get_host_url(), layer);
    let resp = Request::post(&url)
        .json(annotation)
        .map_err(|e| format!("add annotation body: {e}"))?
        .send()
        .await
        .map_err(|e| format!("add annotation: {e}"))?;
    if !resp.ok() {
        return Err(format!(
            "add annotation: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    resp.json::<Annotation>()
        .await
        .map_err(|e| format!("parse annotation: {e}"))
}

/// Replace one annotation's geometry and class, keeping its id.
pub async fn update_annotation(layer: &str, annotation: &Annotation) -> Result<Annotation, String> {
    let url = format!(
        "{}/api/annotations/{}/{}",
        get_host_url(),
        layer,
        annotation.id
    );
    let resp = Request::put(&url)
        .json(annotation)
        .map_err(|e| format!("update annotation body: {e}"))?
        .send()
        .await
        .map_err(|e| format!("update annotation: {e}"))?;
    if !resp.ok() {
        return Err(format!(
            "update annotation: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    resp.json::<Annotation>()
        .await
        .map_err(|e| format!("parse annotation: {e}"))
}

/// Drop one annotation.
pub async fn remove_annotation(layer: &str, id: u64) -> Result<(), String> {
    let url = format!("{}/api/annotations/{}/{}", get_host_url(), layer, id);
    let resp = Request::delete(&url)
        .send()
        .await
        .map_err(|e| format!("remove annotation: {e}"))?;
    if !resp.ok() {
        return Err(format!(
            "remove annotation: {}",
            resp.text().await.unwrap_or_default()
        ));
    }
    Ok(())
}

/// What a save reports back.
///
/// The scale fields are optional because only an ROI table has one: GeoJSON is
/// written in world pixels unconverted, which is the point of it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SavedAnnotations {
    /// Where it actually went, which is what a later save with no target uses.
    pub target: String,
    pub rows: usize,
    /// `"geojson"` or `"roi_table"` — which form the target's shape asked for.
    #[serde(default)]
    pub format: String,
    /// How many shapes the format could not hold and stored as bounding boxes.
    #[serde(default)]
    pub flattened: usize,
    /// The world-pixel-to-micrometre factor an ROI table was written with.
    #[serde(default)]
    pub voxel: Option<[f64; 3]>,
    /// The frame-to-second factor, likewise.
    #[serde(default)]
    pub seconds: Option<f64>,
}

/// Write one annotation layer into a store as an ROI table.
pub async fn save_annotations(
    layer: &str,
    target: Option<&str>,
) -> Result<SavedAnnotations, String> {
    let url = format!("{}/api/annotations/{}/save", get_host_url(), layer);
    let resp = Request::post(&url)
        .json(&serde_json::json!({ "target": target }))
        .map_err(|e| format!("save body: {e}"))?
        .send()
        .await
        .map_err(|e| format!("save annotations: {e}"))?;
    if !resp.ok() {
        return Err(resp.text().await.unwrap_or_else(|_| "save failed".into()));
    }
    resp.json::<SavedAnnotations>()
        .await
        .map_err(|e| format!("parse save result: {e}"))
}

/// The ROI tables a store already holds, and the store they were looked for in.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StoreTables {
    #[serde(default)]
    pub store: Option<String>,
    /// ngio ROI tables — boxes, and the interop form.
    #[serde(default)]
    pub tables: Vec<String>,
    /// GeoJSON annotation sets — the native form, and the one this viewer
    /// writes by default. Listing them is what makes a saved set reopenable
    /// without retyping its path.
    #[serde(default)]
    pub annotations: Vec<String>,
}

/// List the ROI tables in a store — absent `store` means the session's own.
pub async fn fetch_tables(store: Option<&str>) -> Result<StoreTables, String> {
    let url = match store {
        Some(store) => format!("{}/api/annotations/tables?store={}", get_host_url(), store),
        None => format!("{}/api/annotations/tables", get_host_url()),
    };
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("list tables: {e}"))?;
    if !resp.ok() {
        return Err(format!("list tables: status {}", resp.status()));
    }
    resp.json::<StoreTables>()
        .await
        .map_err(|e| format!("parse tables: {e}"))
}

/// Rebuild a layer's hierarchy from where its shapes now are.
pub async fn renest_annotations(layer: &str) -> Result<Vec<Annotation>, String> {
    post_rows(&format!(
        "{}/api/annotations/{}/renest",
        get_host_url(),
        layer
    ))
    .await
}

/// Lift one annotation out of its parent.
pub async fn detach_annotation(layer: &str, id: u64) -> Result<Vec<Annotation>, String> {
    post_rows(&format!(
        "{}/api/annotations/{}/{}/detach",
        get_host_url(),
        layer,
        id
    ))
    .await
}

/// A POST that answers with the layer's rows.
async fn post_rows(url: &str) -> Result<Vec<Annotation>, String> {
    let resp = Request::post(url)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !resp.ok() {
        return Err(resp.text().await.unwrap_or_else(|_| "failed".into()));
    }
    resp.json::<Vec<Annotation>>()
        .await
        .map_err(|e| format!("parse rows: {e}"))
}

/// A page of a table layer's rows, as text.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TablePage {
    pub offset: usize,
    pub rows: Vec<Vec<String>>,
}

/// Fetch rows `offset..offset+limit` of a table layer.
pub async fn fetch_table_rows(
    layer: &str,
    offset: usize,
    limit: usize,
) -> Result<TablePage, String> {
    let url = format!(
        "{}/api/tables/{}/rows?offset={}&limit={}",
        get_host_url(),
        layer,
        offset,
        limit
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("table rows: {e}"))?;
    if !resp.ok() {
        return Err(format!("table rows: status {}", resp.status()));
    }
    resp.json::<TablePage>()
        .await
        .map_err(|e| format!("parse table rows: {e}"))
}

/// One numeric column paired with the label id of each row.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct TableColumnValues {
    pub labels: Vec<u64>,
    pub values: Vec<f64>,
}

/// Fetch the column that colours a label image.
pub async fn fetch_table_column(layer: &str, name: &str) -> Result<TableColumnValues, String> {
    let url = format!(
        "{}/api/tables/{}/column?name={}",
        get_host_url(),
        layer,
        name
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("table column: {e}"))?;
    if !resp.ok() {
        return Err(resp.text().await.unwrap_or_else(|_| "failed".into()));
    }
    resp.json::<TableColumnValues>()
        .await
        .map_err(|e| format!("parse table column: {e}"))
}
