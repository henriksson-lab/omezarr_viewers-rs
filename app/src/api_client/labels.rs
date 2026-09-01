//! Classing the ids in a label image.
//!
//! The label image is never written: a class is an assertion *about* somebody
//! else's raster, so it is held beside the layer and saved to a feature table
//! joined by label id. Which is why these are their own calls rather than part
//! of the annotation ones — there is no geometry here, only an id.

use super::{delete_json, get_host_url, get_json, post_json, put_json};

/// One id and what it was classed as.
///
/// `class` may be the empty string, and that is a *decision*: the id was looked
/// at and is nothing in particular. An id with no entry at all has not been
/// looked at. Nothing here may collapse the two.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct LabelClass {
    pub id: u64,
    pub class: String,
}

/// Every id a label layer has a class for, and the classes in use.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
pub struct LabelClasses {
    #[serde(default)]
    pub assigned: Vec<LabelClass>,
    /// The distinct classes, in the server's order — including the empty one
    /// when some id carries it.
    #[serde(default)]
    pub classes: Vec<String>,
}

/// What a single-id edit reports back.
///
/// The count is the server's own, and the client compares it with what it just
/// did to itself: two clicks racing, or an edit that never landed, show up as a
/// disagreement rather than as a panel that quietly drifts from the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct ClassCount {
    pub id: u64,
    pub assigned: usize,
}

/// What a label layer's ids have been classed as.
pub async fn fetch_label_classes(layer: &str) -> Result<LabelClasses, String> {
    let url = format!("{}/api/labels/{}/classes", get_host_url(), layer);
    get_json(&url, "fetch label classes", "parse label classes").await
}

/// Class one id. An empty `class` says "looked at, nothing in particular".
pub async fn set_label_class(layer: &str, id: u64, class: &str) -> Result<ClassCount, String> {
    let url = format!("{}/api/labels/{}/classes/{}", get_host_url(), layer, id);
    let body = serde_json::json!({ "class": class });
    put_json(&url, &body, "class label id", "parse class count").await
}

/// Forget an id, which is not the same as classing it as nothing.
pub async fn clear_label_class(layer: &str, id: u64) -> Result<ClassCount, String> {
    let url = format!("{}/api/labels/{}/classes/{}", get_host_url(), layer, id);
    delete_json(&url, "unclass label id", "parse class count").await
}

/// What a class save wrote.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SavedClasses {
    /// Where it actually went.
    pub target: String,
    pub rows: usize,
    /// `"feature_table"` — a class table carries no coordinates.
    #[serde(default)]
    pub format: String,
    /// The label image the ids were joined to.
    #[serde(default)]
    pub region: String,
}

/// Write a label layer's classes as an ngio feature table.
pub async fn save_label_classes(
    layer: &str,
    target: &str,
    region: &str,
) -> Result<SavedClasses, String> {
    let url = format!("{}/api/labels/{}/classes/save", get_host_url(), layer);
    let body = serde_json::json!({ "target": target, "region": region });
    post_json(&url, &body, "save label classes", "parse save result").await
}
