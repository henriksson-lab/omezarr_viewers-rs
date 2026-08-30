//! Annotations, and the tables that sit beside them in a store.

use omezarr_viewer_common::{Annotation, SessionInfo};

use super::{delete_ok, get_host_url, get_json, post_empty_json, post_json, put_json};

/// Create an empty annotation layer, returning the new session.
pub async fn add_annotation_layer(name: &str) -> Result<SessionInfo, String> {
    let url = format!("{}/api/annotations/layers", get_host_url());
    let body = serde_json::json!({ "name": name });
    post_json(&url, &body, "new annotation layer", "parse session").await
}

/// Add one annotation, returning it with the id the server assigned.
pub async fn add_annotation(layer: &str, annotation: &Annotation) -> Result<Annotation, String> {
    let url = format!("{}/api/annotations/{}", get_host_url(), layer);
    post_json(&url, annotation, "add annotation", "parse annotation").await
}

/// Replace one annotation's geometry and class, keeping its id.
pub async fn update_annotation(layer: &str, annotation: &Annotation) -> Result<Annotation, String> {
    let url = format!(
        "{}/api/annotations/{}/{}",
        get_host_url(),
        layer,
        annotation.id
    );
    put_json(&url, annotation, "update annotation", "parse annotation").await
}

/// Drop one annotation.
pub async fn remove_annotation(layer: &str, id: u64) -> Result<(), String> {
    let url = format!("{}/api/annotations/{}/{}", get_host_url(), layer, id);
    delete_ok(&url, "remove annotation").await
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
    let body = serde_json::json!({ "target": target });
    post_json(&url, &body, "save annotations", "parse save result").await
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
    get_json(&url, "list tables", "parse tables").await
}

/// Rebuild a layer's hierarchy from where its shapes now are.
pub async fn renest_annotations(layer: &str) -> Result<Vec<Annotation>, String> {
    let url = format!("{}/api/annotations/{}/renest", get_host_url(), layer);
    post_empty_json(&url, "renest annotations", "parse rows").await
}

/// Lift one annotation out of its parent.
pub async fn detach_annotation(layer: &str, id: u64) -> Result<Vec<Annotation>, String> {
    let url = format!("{}/api/annotations/{}/{}/detach", get_host_url(), layer, id);
    post_empty_json(&url, "detach annotation", "parse rows").await
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
    get_json(&url, "table rows", "parse table rows").await
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
    get_json(&url, "table column", "parse table column").await
}
