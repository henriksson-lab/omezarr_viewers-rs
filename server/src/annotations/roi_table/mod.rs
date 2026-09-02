//! Reading and writing an **ngio/Fractal ROI table** inside an OME-Zarr store.
//!
//! This is the one place in the repo that *writes* annotation data. What it
//! writes is not an OME-Zarr specification — there is no annotation spec beyond
//! `labels` (`info_roi.md`) — it is the convention every tool in the ecosystem
//! actually reads: a `tables/` group beside `labels/`, one subgroup per table,
//! each carrying its own attributes and a payload.
//!
//! ```text
//! image.zarr
//! ├── 0 … N              multiscale levels
//! ├── labels/            label images        (in the spec)
//! └── tables/            zattrs: {"tables": ["my_boxes"]}
//!     └── my_boxes/      zattrs: {"type": "roi_table", "backend": "csv", …}
//!         └── table.csv
//! ```
//!
//! # Backends
//!
//! ngio names four: AnnData (its default), Parquet, CSV and JSON. **This module
//! writes CSV and reads all four**, which is the asymmetry the situation calls
//! for — one form to write is a decision, four forms to read is interoperation,
//! and ngio's own default being AnnData means a read-only-CSV viewer cannot open
//! anybody else's table.
//!
//! Every backend is funnelled through [`Columns`] and then through
//! [`rows_from_columns`], so "which columns make an ROI table" is decided in
//! exactly one place regardless of which bytes it came out of.
//!
//! CSV is what gets written: three of the four are a single extra key inside the
//! table's group, which `WritableStorageTraits::set` writes directly, and of
//! those CSV is the one a person can check by eye. Speed on a few hundred
//! hand-drawn rows is not the constraint.
//!
//! # Boxes only
//!
//! An ROI table row is an axis-aligned bounding box and nothing else. A shape
//! written here goes as [`Annotation::bounds`], and a caller that has drawn a
//! polygon should be told what it is about to lose — [`lossy_rows`] counts
//! them. The lossless form is the GeoJSON beside this module.
//!
//! # `*_micrometer` and `t_second`
//!
//! The ROI table's columns are named for micrometres and seconds, and the only
//! statement OME-Zarr makes about either is the `coordinateTransformations`
//! scale on the reference image. [`world_scale`] reads it; where a store says
//! nothing the factor is 1 and a written "micrometre" is a pixel. Either way the
//! factors actually used are recorded in the table's own attributes, so a reader
//! — this one included — can undo them exactly rather than guess.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{
    Annotation, CoordinateTransformation, DatasetInfo, DatasetMetadata, TableColumn, TableInfo,
    WorldScale,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zarrs::group::{Group, GroupBuilder, GroupMetadata};
use zarrs::metadata::v2::GroupMetadataV2;
use zarrs::storage::{AsyncWritableStorageTraits, StoreKey};

/// The version this writes into `table_version`.
const TABLE_VERSION: &str = "1";
/// The column ngio uses to identify a row, declared as `index_key`.
const INDEX_KEY: &str = "FieldIndex";
/// Our own attributes, under a key nothing else claims.
const OURS: &str = "omezarr_viewer";
/// The payload's name inside the table group, per byte-payload backend.
const CSV_PAYLOAD: &str = "table.csv";
const JSON_PAYLOAD: &str = "table.json";
const PARQUET_PAYLOAD: &str = "table.parquet";

/// The header, in the order the columns are written.
///
/// `class` rather than `label`: ngio treats a `label` column as an index key
/// tying rows to a label image, which is not what a free-text class is. An
/// unrecognised column is carried through unchanged, which is exactly the
/// treatment wanted.
///
/// `t_second` and `len_t_second` are optional in the spec and always written
/// here. A column of zeros costs nothing and says "frame 0" out loud, where an
/// absent column leaves a reader to decide for itself what a missing timepoint
/// means.
const HEADER: [&str; 10] = [
    INDEX_KEY,
    "x_micrometer",
    "y_micrometer",
    "z_micrometer",
    "len_x_micrometer",
    "len_y_micrometer",
    "len_z_micrometer",
    "t_second",
    "len_t_second",
    "class",
];

/// What a table says about the label image its rows describe.
///
/// A feature table and a masking ROI table both carry this: `region.path`
/// points at a label image, and `instance_key` names the column holding the id
/// each row belongs to. It is the whole of how a table with no coordinates —
/// which is what a feature table is — knows where its rows are.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Relative to the table group, e.g. `../labels/nuclei`.
    pub path: String,
    /// The column holding the label id; `label` by convention.
    pub instance_key: String,
}

/// One table, as it sits on disk.
#[derive(Debug, Clone)]
pub struct RoiTable {
    pub rows: Vec<Annotation>,
    /// The factors the file's `*_micrometer` and `t_second` columns were
    /// written with, already undone: `rows` are back in world coordinates.
    pub scale: WorldScale,
    /// The backend the rows were actually read out of.
    pub backend: String,
    /// What the table declares itself: `roi_table`, `feature_table`,
    /// `masking_roi_table`, `condition_table`, or whatever a foreign writer put
    /// there.
    pub table_type: String,
    /// The label image its rows describe, when it names one.
    pub region: Option<Region>,
    /// Every column, kept for the tables that are not geometry at all.
    pub columns: Columns,
    /// True when the positions came from `obsm["spatial"]` rather than from
    /// `*_micrometer` columns — so a caller can say which convention it read.
    pub from_obsm: bool,
}

// ---------------------------------------------------------------------------
// The scale a table is written in
// ---------------------------------------------------------------------------

/// How world coordinates convert to file units, from the reference image's own
/// metadata.
///
/// The first `scale` in a list of transformations, if there is one. A
/// translation moves a level; it does not change how big a voxel is.
fn scale_of(transforms: &Option<Vec<CoordinateTransformation>>) -> Option<&Vec<f64>> {
    transforms.as_ref()?.iter().find_map(|t| match t {
        CoordinateTransformation::Scale { scale } => Some(scale),
        CoordinateTransformation::Translation { .. } => None,
    })
}

/// One axis's factor from one scale, or 1 if it does not usefully say.
///
/// A value that is not finite and positive is not a size, and it must not
/// discard the *other* transformation's word on the same axis — so it reads as
/// "says nothing here" rather than as a reason to abandon the whole lookup.
fn factor(scale: Option<&Vec<f64>>, axis: usize) -> f64 {
    match scale.and_then(|s| s.get(axis)) {
        Some(value) if value.is_finite() && *value > 0.0 => *value,
        _ => 1.0,
    }
}

/// The size of one world voxel, in micrometres, as the store declares it.
///
/// Two transformations compose here, and both are in the spec:
///
/// * `coordinateTransformations` on the *first* dataset — level 0 — which is
///   the one that speaks about full-resolution pixels, which is what the world
///   is; and
/// * `coordinateTransformations` on the **multiscale**, which applies to every
///   dataset on top of its own.
///
/// They multiply. The specification's own example pairs a dataset `[1, 1]` with
/// a multiscale `[10, 10]` and means ten — reading only the first was a silent
/// wrong number in every `*_micrometer` column this viewer wrote for such a
/// store, and it is pinned now by `the_spec_example_that_found_this_bug_reads_ten`.
///
/// An axis neither one mentions, or a store with no transformation at all, is
/// 1: a pixel and a frame, stated as such rather than invented.
pub fn world_scale(metadata: &DatasetMetadata) -> WorldScale {
    let mut size = WorldScale::default();
    let Some(multiscale) = metadata.multiscales.first() else {
        return size;
    };
    let per_dataset = scale_of(
        &multiscale
            .datasets
            .first()
            .and_then(|d| d.coordinate_transformations.clone()),
    )
    .cloned();
    let for_all = scale_of(&multiscale.coordinate_transformations).cloned();
    if per_dataset.is_none() && for_all.is_none() {
        return size;
    }
    for (axis_index, axis) in multiscale.axes.iter().enumerate() {
        let value = factor(per_dataset.as_ref(), axis_index) * factor(for_all.as_ref(), axis_index);
        match axis.name.as_str() {
            "z" => size.voxel[0] = value,
            "y" => size.voxel[1] = value,
            "x" => size.voxel[2] = value,
            "t" => size.seconds = value,
            _ => {}
        }
    }
    size
}

/// [`world_scale`] for a whole layer.
pub fn world_scale_of(dataset: &DatasetInfo) -> WorldScale {
    world_scale(&dataset.metadata)
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

/// Where a table lives inside a store, as a store key prefix.
fn table_prefix(name: &str) -> String {
    format!("tables/{name}")
}

pub(crate) fn payload_key(name: &str, payload: &str) -> Result<StoreKey> {
    StoreKey::new(format!("{}/{payload}", table_prefix(name))).map_err(|e| anyhow::anyhow!("{e}"))
}

pub(crate) fn table_path(name: &str) -> String {
    format!("/{}", table_prefix(name))
}

/// Split a local `…/store.zarr/tables/<name>` target into its two halves.
///
/// A target names a table, not a store, because that is what a person types and
/// what `list` hands back. Accepting the store plus a name separately would mean
/// two fields everywhere the target travels.
pub fn split_target(target: &str) -> Result<(PathBuf, String)> {
    let (store, name) = split_uri_target(target)?;
    Ok((PathBuf::from(store.trim_start_matches("file://")), name))
}

/// [`split_target`] for any scheme: the store half stays a URI.
pub fn split_uri_target(target: &str) -> Result<(String, String)> {
    let trimmed = target.trim().trim_end_matches(['/', '\\']);
    let (store, name) = trimmed
        .rsplit_once(['/', '\\'])
        .context("target names no table")?;
    if name.is_empty() {
        bail!("target names no table: `{target}`");
    }
    let (root, tables) = store
        .rsplit_once(['/', '\\'])
        .context("target has no `tables` parent")?;
    if tables != "tables" {
        bail!("an ROI table lives at <store>/tables/<name>, got `{target}`");
    }
    if root.is_empty() {
        bail!("target has no store root: `{target}`");
    }
    Ok((root.to_string(), name.to_string()))
}

/// Join a store root and a table name back into a target.
pub fn make_target(root: &Path, name: &str) -> String {
    root.join("tables").join(name).display().to_string()
}

/// Join a store URI and a table name back into a target.
pub fn make_uri_target(store: &str, name: &str) -> String {
    format!("{}/tables/{name}", store.trim_end_matches('/'))
}

/// Is this target one only the async path can reach?
pub fn is_remote(target: &str) -> bool {
    let target = target.trim();
    target.starts_with("s3://") || target.starts_with("http://") || target.starts_with("https://")
}

// ---------------------------------------------------------------------------
// Group metadata
// ---------------------------------------------------------------------------

/// A group at the store's own zarr version, ready to have its metadata stored.
///
/// The v2 branch builds [`GroupMetadataV2`] rather than converting a v3 group
/// down: `GroupBuilder` is v3-only, and `store_metadata` writes whichever
/// version the metadata carries — `.zgroup` plus `.zattrs` for v2, `zarr.json`
/// for v3 — so choosing the type here is the whole of the choice.
pub(crate) fn group_for<T: ?Sized>(
    store: Arc<T>,
    v3: bool,
    path: &str,
    attributes: serde_json::Value,
) -> Result<Group<T>> {
    let attributes = attributes
        .as_object()
        .context("group attributes must be an object")?
        .clone();
    if v3 {
        let mut builder = GroupBuilder::new();
        builder.attributes(attributes);
        return builder
            .build(store, path)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }
    Group::new_with_metadata(
        store,
        path,
        GroupMetadata::V2(GroupMetadataV2::new().with_attributes(attributes)),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

/// The attributes a table group carries: the ngio contract, plus the factors
/// this particular write used.
fn table_attributes(scale: WorldScale) -> serde_json::Value {
    serde_json::json!({
        "type": "roi_table",
        "table_version": TABLE_VERSION,
        "backend": "csv",
        "index_key": INDEX_KEY,
        "index_type": "str",
        // Ours, so the pixel-to-micrometre and frame-to-second factors above are
        // recoverable rather than folded irreversibly into the numbers.
        OURS: {
            "world_pixel_size_zyx": scale.voxel,
            "world_seconds_per_frame": scale.seconds,
            "written_by": concat!("omezarr-viewer ", env!("CARGO_PKG_VERSION")),
        },
    })
}

/// The `tables` index, with `name` added to whatever is already listed.
///
/// Merged, never replaced: a store may well hold tables this viewer knows
/// nothing about, and dropping their names from the index would make them
/// invisible to every reader that trusts it.
pub(crate) fn merged_index(
    existing: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> serde_json::Value {
    let mut listed = listed_tables(existing);
    if !listed.iter().any(|t| t == name) {
        listed.push(name.to_string());
    }
    serde_json::json!({ "tables": listed })
}

fn listed_tables(attributes: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    attributes
        .get("tables")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
}

/// The scale a table was written with, from its own attributes.
/// The label image a table describes, from its attributes.
fn region_from(attributes: &serde_json::Map<String, serde_json::Value>) -> Option<Region> {
    let path = attributes.get("region")?.get("path")?.as_str()?.to_string();
    Some(Region {
        path,
        instance_key: attributes
            .get("instance_key")
            .or_else(|| attributes.get("index_key"))
            .and_then(|v| v.as_str())
            .unwrap_or("label")
            .to_string(),
    })
}

/// What a table declares itself to be.
fn type_of(attributes: &serde_json::Map<String, serde_json::Value>) -> String {
    attributes
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("roi_table")
        .to_string()
}

fn scale_from(attributes: &serde_json::Map<String, serde_json::Value>) -> WorldScale {
    let mut scale = WorldScale::default();
    let Some(ours) = attributes.get(OURS) else {
        return scale;
    };
    if let Some(voxel) = ours
        .get("world_pixel_size_zyx")
        .and_then(|v| serde_json::from_value::<[f64; 3]>(v.clone()).ok())
    {
        scale.voxel = voxel;
    }
    if let Some(seconds) = ours
        .get("world_seconds_per_frame")
        .and_then(|v| v.as_f64())
        .filter(|s| *s > 0.0)
    {
        scale.seconds = seconds;
    }
    scale
}

/// Which backend a table's attributes declare, normalised.
///
/// ngio records a backend as `{name}_v{version}` and keeps `experimental_*_v1`
/// aliases for tables its older releases wrote. An absent backend is CSV,
/// because a table with no attributes at all is one somebody dropped in by hand.
fn backend_of(attributes: &serde_json::Map<String, serde_json::Value>) -> String {
    let raw = attributes
        .get("backend")
        .and_then(|v| v.as_str())
        .unwrap_or("csv");
    match raw {
        "csv" | "experimental_csv_v1" => "csv",
        "json" | "experimental_json_v1" => "json",
        "parquet" | "experimental_parquet_v1" => "parquet",
        "anndata" | "anndata_v1" => "anndata",
        other => other,
    }
    .to_string()
}

mod anndata;
mod backends;
pub mod classes;
mod columns;
mod store;

pub(crate) use anndata::*;
pub(crate) use backends::*;
pub use columns::*;
pub use store::*;

#[cfg(test)]
mod tests;
