//! Group metadata and I/O: the local filesystem path and the remote one.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{Annotation, WorldScale};
use std::path::Path;
use std::sync::Arc;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;
use zarrs::storage::{
    AsyncReadableStorageTraits, ReadableStorageTraits, StoreKey, WritableStorageTraits,
};
use zarrs_opendal::AsyncOpendalStore;

use crate::source::{SourceRegistry, SourceSpec};

use super::*;

// ---------------------------------------------------------------------------
// The local path: a filesystem store, read and written synchronously
// ---------------------------------------------------------------------------

/// Does this store hold its metadata as zarr v3 (`zarr.json`) or v2
/// (`.zgroup`)?
///
/// A table written in the other version than the store it sits in is a table
/// half the readers will walk straight past, so this follows the host rather
/// than picking a favourite. An empty or unrecognised directory gets v2, which
/// is what NGFF 0.4 — still the version most tools implement — uses.
pub(crate) fn store_is_v3(root: &Path) -> bool {
    root.join("zarr.json").exists()
}

pub(crate) fn filesystem(root: &Path) -> Result<Arc<FilesystemStore>> {
    Ok(Arc::new(
        FilesystemStore::new(root).with_context(|| format!("opening {}", root.display()))?,
    ))
}

/// Read a group's attributes, or an empty map when the group is not there.
///
/// `Group::open` finds either version and merges a v2 `.zattrs` into the
/// metadata, so nothing here has to know which one it is looking at.
pub(crate) fn attributes_at(
    store: &Arc<FilesystemStore>,
    path: &str,
) -> serde_json::Map<String, serde_json::Value> {
    Group::open(store.clone(), path)
        .map(|group| group.attributes().clone())
        .unwrap_or_default()
}

pub(crate) fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        bail!("`{name}` is not a table name");
    }
    Ok(())
}

/// Write `rows` as `<root>/tables/<name>`, creating the `tables` group if the
/// store has none, and return the target.
///
/// Rewriting a table that already exists replaces its rows.
pub fn write(root: &Path, name: &str, rows: &[Annotation], scale: WorldScale) -> Result<String> {
    check_name(name)?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let v3 = store_is_v3(root);
    let store = filesystem(root)?;

    let index = merged_index(&attributes_at(&store, "/tables"), name);
    group_for(store.clone(), v3, "/tables", index)?
        .store_metadata()
        .context("writing the tables group")?;
    group_for(
        store.clone(),
        v3,
        &table_path(name),
        table_attributes(scale),
    )?
    .store_metadata()
    .context("writing the table group")?;
    store
        .set(
            &payload_key(name, CSV_PAYLOAD)?,
            encode_csv(rows, scale)?.into(),
        )
        .context("writing the table payload")?;

    Ok(make_target(root, name))
}

/// Read `<root>/tables/<name>` back into world-coordinate annotations.
pub fn read(root: &Path, name: &str) -> Result<RoiTable> {
    let store = filesystem(root)?;
    let attributes = attributes_at(&store, &table_path(name));
    let backend = backend_of(&attributes);
    let scale = scale_from(&attributes);

    let columns = match payload_name(&backend) {
        Some(payload) => {
            let bytes = store
                .get(&payload_key(name, payload)?)
                .context("reading the table payload")?
                .with_context(|| format!("no {payload} in table `{name}`"))?;
            columns_from_payload(&backend, name, &bytes)?
        }
        None if backend == "anndata" => anndata_columns(&store, name)?,
        None => bail!("table `{name}` declares the unknown backend `{backend}`"),
    };

    finish(columns, scale, backend, &attributes)
}

/// Assemble what was read into a table, deciding whether it is geometry.
///
/// A table with no positions of any kind is not an error: a feature table is
/// per-object measurements keyed to a label image, and a condition table is
/// experiment metadata. Both are worth opening; neither has anywhere to be
/// drawn on its own. So `rows` is empty for those, and `columns` is what the
/// caller shows.
fn finish(
    columns: Columns,
    scale: WorldScale,
    backend: String,
    attributes: &serde_json::Map<String, serde_json::Value>,
) -> Result<RoiTable> {
    let from_obsm = !columns.has("x_micrometer") && columns.spatial.is_some();
    let table_type = type_of(attributes);
    // A table that *declares* itself an ROI table and carries no coordinates is
    // a broken file, and saying so is better than opening it as something else.
    // A feature or condition table having none is the spec working as intended.
    let must_have_geometry = matches!(table_type.as_str(), "roi_table" | "masking_roi_table");
    let rows = if columns.has_positions() || must_have_geometry {
        rows_from_columns(&columns, scale)?
    } else {
        Vec::new()
    };
    Ok(RoiTable {
        rows,
        scale,
        backend,
        table_type,
        region: region_from(attributes),
        columns,
        from_obsm,
    })
}

impl RoiTable {
    /// Does this table have geometry of its own, or is it only rows?
    pub fn is_geometry(&self) -> bool {
        !self.rows.is_empty()
    }

    /// The table as a layer: its schema, and a first page of it.
    ///
    /// `preview` is capped because a feature table has a row per segmented
    /// object and there can be a hundred thousand of them; the rest is paged.
    pub fn info(&self, preview_rows: usize) -> TableInfo {
        let names: Vec<String> = self.columns.names().iter().map(|n| n.to_string()).collect();
        let columns = names
            .iter()
            .map(|name| match self.columns.column(name) {
                Some(ColumnValues::Numbers(values)) => TableColumn {
                    name: name.clone(),
                    kind: "number".into(),
                    range: range_of(values),
                },
                _ => TableColumn {
                    name: name.clone(),
                    kind: "text".into(),
                    range: None,
                },
            })
            .collect();
        let preview = (0..self.columns.row_count().min(preview_rows))
            .map(|row| {
                names
                    .iter()
                    .map(|name| self.columns.string(name, row).unwrap_or_default())
                    .collect()
            })
            .collect();
        TableInfo {
            table_type: self.table_type.clone(),
            columns,
            rows: self.columns.row_count(),
            region: self.region.as_ref().map(|r| r.path.clone()),
            instance_key: self.region.as_ref().map(|r| r.instance_key.clone()),
            preview,
        }
    }

    /// One numeric column paired with the label id of each row.
    ///
    /// This is what colours a label image by a measurement: the ids come from
    /// the table's `instance_key` column, the values from the named one, and
    /// the pairing is the join the `region` link exists to make.
    pub fn column_by_label(&self, name: &str) -> Option<(Vec<u64>, Vec<f64>)> {
        let key = self
            .region
            .as_ref()
            .map(|r| r.instance_key.as_str())
            .unwrap_or("label");
        let Some(ColumnValues::Numbers(values)) = self.columns.column(name) else {
            return None;
        };
        let ids: Vec<u64> = (0..self.columns.row_count())
            .map(|row| {
                self.columns
                    .number(key, row)
                    .map(|v| v.max(0.0) as u64)
                    .unwrap_or(0)
            })
            .collect();
        Some((ids, values.to_vec()))
    }
}

pub(crate) fn range_of(values: &[f64]) -> Option<[f64; 2]> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for value in values.iter().filter(|v| v.is_finite()) {
        lo = lo.min(*value);
        hi = hi.max(*value);
    }
    (lo <= hi).then_some([lo, hi])
}

/// Every table the store's `tables` group lists.
pub fn list(root: &Path) -> Result<Vec<String>> {
    Ok(listed_tables(&attributes_at(&filesystem(root)?, "/tables")))
}

// ---------------------------------------------------------------------------
// The remote path: an opendal store, read and written asynchronously
// ---------------------------------------------------------------------------

/// An opendal store rooted at a source, for the schemes that have one.
pub(crate) fn remote(registry: &SourceRegistry, store: &str) -> Result<Arc<AsyncOpendalStore>> {
    let spec = SourceSpec::parse(store)?;
    let operator = registry
        .operator(&spec)?
        .with_context(|| format!("{store} is not a remote source"))?;
    Ok(Arc::new(AsyncOpendalStore::new(operator)))
}

pub(crate) async fn attributes_at_async(
    store: &Arc<AsyncOpendalStore>,
    path: &str,
) -> serde_json::Map<String, serde_json::Value> {
    Group::async_open(store.clone(), path)
        .await
        .map(|group| group.attributes().clone())
        .unwrap_or_default()
}

/// Does the remote store hold v3 metadata?
///
/// One `get` rather than a listing: object storage has no directories, and
/// `zarr.json` either answers or it does not.
pub(crate) async fn remote_is_v3(store: &Arc<AsyncOpendalStore>) -> bool {
    let Ok(key) = StoreKey::new("zarr.json") else {
        return false;
    };
    matches!(store.get(&key).await, Ok(Some(_)))
}

/// [`write`], for an `s3://` or `http(s)://` store.
pub async fn write_async(
    registry: &SourceRegistry,
    store_uri: &str,
    name: &str,
    rows: &[Annotation],
    scale: WorldScale,
) -> Result<String> {
    check_name(name)?;
    let store = remote(registry, store_uri)?;
    let v3 = remote_is_v3(&store).await;

    let index = merged_index(&attributes_at_async(&store, "/tables").await, name);
    group_for(store.clone(), v3, "/tables", index)?
        .async_store_metadata()
        .await
        .context("writing the tables group")?;
    group_for(
        store.clone(),
        v3,
        &table_path(name),
        table_attributes(scale),
    )?
    .async_store_metadata()
    .await
    .context("writing the table group")?;
    store
        .set(
            &payload_key(name, CSV_PAYLOAD)?,
            encode_csv(rows, scale)?.into(),
        )
        .await
        .context("writing the table payload")?;

    Ok(make_uri_target(store_uri, name))
}

/// [`read`], for an `s3://` or `http(s)://` store.
///
/// Every byte-payload backend works here. AnnData does not: its rows live in
/// zarr arrays rather than in one object, and reading those asynchronously is a
/// second implementation of the same decoder — worth writing when a remote
/// AnnData table actually turns up, and not before.
pub async fn read_async(
    registry: &SourceRegistry,
    store_uri: &str,
    name: &str,
) -> Result<RoiTable> {
    let store = remote(registry, store_uri)?;
    let attributes = attributes_at_async(&store, &table_path(name)).await;
    let backend = backend_of(&attributes);
    let scale = scale_from(&attributes);

    let Some(payload) = payload_name(&backend) else {
        bail!("table `{name}` uses the `{backend}` backend, which this viewer reads locally only");
    };
    let bytes = store
        .get(&payload_key(name, payload)?)
        .await
        .context("reading the table payload")?
        .with_context(|| format!("no {payload} in table `{name}`"))?;

    finish(
        columns_from_payload(&backend, name, &bytes)?,
        scale,
        backend,
        &attributes,
    )
}

/// [`list`], for an `s3://` or `http(s)://` store.
pub async fn list_async(registry: &SourceRegistry, store_uri: &str) -> Result<Vec<String>> {
    Ok(listed_tables(
        &attributes_at_async(&remote(registry, store_uri)?, "/tables").await,
    ))
}
