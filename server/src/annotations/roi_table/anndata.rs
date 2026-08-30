//! The AnnData backend: `X`, `var`, `obs` and `obsm`.
//!
//! The table group *is* an AnnData store, so there is no payload file to
//! decode — the columns are assembled from arrays. Fetching differs between
//! the local and remote paths; the assembly does not, which is what
//! [`AnnDataParts`] exists to keep true.

use anyhow::{bail, Context, Result};
use std::sync::Arc;
use zarrs::array::Array;
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;

use super::*;

/// What an AnnData read has to fetch, whichever store it came from.
///
/// The assembly below is the same either way; only the fetching differs, which
/// is the whole reason this is a struct rather than two copies of the logic.
struct AnnDataParts {
    /// `X`, row-major `(n_obs, n_vars)`.
    x: Vec<f64>,
    rows: u64,
    width: u64,
    /// `var/_index` — the names of X's columns.
    names: Vec<String>,
    /// `obs` columns already decoded, numeric or text.
    obs: Vec<(String, ObsColumn)>,
    /// `obsm["spatial"]`, flattened, with its width.
    spatial: Option<(Vec<f64>, usize)>,
}

enum ObsColumn {
    Numbers(Vec<f64>),
    Text(Vec<String>),
}

/// Turn fetched AnnData parts into columns.
fn assemble_anndata(parts: AnnDataParts) -> Result<Columns> {
    let AnnDataParts {
        x,
        rows,
        width,
        names,
        obs,
        spatial,
    } = parts;
    if names.len() as u64 != width {
        bail!(
            "AnnData `var/_index` names {} column(s) but `X` has {width}",
            names.len()
        );
    }
    let mut columns = Columns::default();
    // X is row-major `(obs, var)`, and a table is a set of columns, so this
    // transposes once here rather than striding on every later lookup.
    for (column, name) in names.iter().enumerate() {
        let mut down = Vec::with_capacity(rows as usize);
        for row in 0..rows {
            down.push(
                x.get((row * width + column as u64) as usize)
                    .copied()
                    .unwrap_or(0.0),
            );
        }
        columns.push_numeric(name.clone(), down);
    }
    columns.rows = columns.rows.max(rows as usize);
    for (name, values) in obs {
        match values {
            ObsColumn::Numbers(values) => columns.push_numeric(name, values),
            ObsColumn::Text(values) => columns.push_text(name, values),
        }
    }
    columns.spatial = spatial;
    Ok(columns)
}

/// The AnnData backend: the table group *is* an AnnData store.
///
/// ngio's normalisation puts float and boolean columns in `X` — a 2D array, one
/// column per `var` entry — and categorical and integer columns in `obs`, one
/// 1D array each, with the row index in `obs/_index`. Every geometry column an
/// ROI table needs is a float, so they all arrive through `X`; `obs` is where
/// the class is, and is read best-effort.
pub(crate) fn anndata_columns(store: &Arc<FilesystemStore>, table: &str) -> Result<Columns> {
    let prefix = table_path(table);

    let x = Array::open(store.clone(), &format!("{prefix}/X"))
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("opening the AnnData `X` array")?;
    let shape = x.shape().to_vec();
    if shape.len() != 2 {
        bail!("AnnData `X` is {}-dimensional, expected 2", shape.len());
    }
    let values =
        read_numbers(store, &format!("{prefix}/X")).context("reading the AnnData `X` array")?;
    let names = read_strings(store, &format!("{prefix}/var/_index"))
        .context("reading AnnData `var/_index` (the names of X's columns)")?;

    // `obsm["spatial"]` — the scverse convention, and the only positions a
    // spatial-omics table has. Optional: an ngio ROI table carries its
    // coordinates as ordinary columns instead.
    let spatial = match Array::open(store.clone(), &format!("{prefix}/obsm/spatial")) {
        Ok(array) => {
            let shape = array.shape().to_vec();
            match spatial_shape(&shape).and_then(|width| {
                Ok((
                    read_numbers(store, &format!("{prefix}/obsm/spatial"))?,
                    width,
                ))
            }) {
                Ok(spatial) => Some(spatial),
                Err(e) => {
                    log::warn!("obsm[\"spatial\"]: {e:#}");
                    None
                }
            }
        }
        Err(_) => None,
    };

    // obs, best-effort: the geometry is already in hand, and a class this reader
    // cannot decode is worth losing rather than failing the whole table over.
    // Whatever is lost is named in the log rather than silently dropped.
    let mut obs = Vec::new();
    for name in obs_columns(store, &format!("{prefix}/obs")) {
        let path = format!("{prefix}/obs/{name}");
        if let Ok(values) = read_categorical(store, &path) {
            obs.push((name, ObsColumn::Text(values)));
        } else if let Ok(values) = read_strings(store, &path) {
            obs.push((name, ObsColumn::Text(values)));
        } else if let Ok(values) = read_numbers(store, &path) {
            obs.push((name, ObsColumn::Numbers(values)));
        } else {
            log::warn!("AnnData column `{name}` is in a form this reader does not decode");
        }
    }

    assemble_anndata(AnnDataParts {
        x: values,
        rows: shape[0],
        width: shape[1],
        names,
        obs,
        spatial,
    })
}

/// How wide an `obsm["spatial"]` array is, if it is a position at all.
///
/// Two columns is `(x, y)` and three is `(x, y, z)`; anything else is some other
/// embedding stored under the same name — a UMAP, say — and is not a position.
pub(crate) fn spatial_shape(shape: &[u64]) -> Result<usize> {
    if shape.len() != 2 {
        bail!("obsm/spatial is {}-dimensional, expected 2", shape.len());
    }
    let width = shape[1] as usize;
    if !(2..=3).contains(&width) {
        bail!("obsm/spatial has {width} columns, which is not a position");
    }
    Ok(width)
}

/// The names under an AnnData `obs` group, from its `column-order` attribute or
/// failing that from the group's own children.
pub(crate) fn obs_columns(store: &Arc<FilesystemStore>, path: &str) -> Vec<String> {
    let Ok(group) = Group::open(store.clone(), path) else {
        return Vec::new();
    };
    let named = named_obs_columns(group.attributes());
    if !named.is_empty() {
        return named;
    }
    let mut found: Vec<String> = Vec::new();
    for child in group
        .child_paths(false)
        .unwrap_or_default()
        .iter()
        .chain(group.child_group_paths(false).unwrap_or_default().iter())
    {
        if let Some(name) = child.as_str().rsplit('/').next() {
            if name != "_index" && !found.iter().any(|f| f == name) {
                found.push(name.to_string());
            }
        }
    }
    found
}

pub(crate) fn read_strings(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<String>> {
    let array = Array::open(store.clone(), path).map_err(|e| anyhow::anyhow!("{e}"))?;
    array
        .retrieve_array_subset_elements::<String>(&ArraySubset::new_with_shape(
            array.shape().to_vec(),
        ))
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Read a numeric array of any width as `f64`.
///
/// `retrieve_array_subset_elements` is exact about its element type — asking an
/// `int8` array for `f64` is an error, not a conversion — and an AnnData table
/// picks whatever width pandas happened to use. Categorical codes in
/// particular are `int8` whenever there are fewer than 128 categories, which is
/// every hand-written class column there will ever be.
pub(crate) fn read_numbers(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<f64>> {
    use zarrs::array::DataType;
    let array = Array::open(store.clone(), path).map_err(|e| anyhow::anyhow!("{e}"))?;
    let subset = ArraySubset::new_with_shape(array.shape().to_vec());

    macro_rules! widen {
        ($ty:ty) => {
            array
                .retrieve_array_subset_elements::<$ty>(&subset)
                .map(|values| values.into_iter().map(|v| v as f64).collect())
                .map_err(|e| anyhow::anyhow!("{e}"))
        };
    }
    match array.data_type() {
        DataType::Float64 => widen!(f64),
        DataType::Float32 => widen!(f32),
        DataType::Int8 => widen!(i8),
        DataType::Int16 => widen!(i16),
        DataType::Int32 => widen!(i32),
        DataType::Int64 => widen!(i64),
        DataType::UInt8 => widen!(u8),
        DataType::UInt16 => widen!(u16),
        DataType::UInt32 => widen!(u32),
        DataType::UInt64 => widen!(u64),
        DataType::Bool => array
            .retrieve_array_subset_elements::<bool>(&subset)
            .map(|values| values.into_iter().map(f64::from).collect())
            .map_err(|e| anyhow::anyhow!("{e}")),
        other => bail!("`{path}` holds {other}, which is not a number"),
    }
}

/// A pandas categorical, as AnnData stores it: `categories` plus integer `codes`
/// indexing into them.
pub(crate) fn read_categorical(store: &Arc<FilesystemStore>, path: &str) -> Result<Vec<String>> {
    let categories = read_strings(store, &format!("{path}/categories"))?;
    let codes = read_numbers(store, &format!("{path}/codes"))?;
    Ok(codes
        .iter()
        .map(|code| category_at(&categories, *code))
        .collect())
}

/// One pandas categorical code resolved against its categories.
///
/// A negative code is pandas' NaN and an out-of-range one is a file that
/// disagrees with itself; both read as no class, rather than as a panic or as a
/// neighbouring category.
pub(crate) fn category_at(categories: &[String], code: f64) -> String {
    usize::try_from(code as i64)
        .ok()
        .and_then(|index| categories.get(index))
        .cloned()
        .unwrap_or_default()
}

/// The column names an AnnData `obs` group's attributes declare.
///
/// Remote reads cannot list children, so a remote `obs` is read from
/// `column-order` alone — which AnnData always writes.
pub(crate) fn named_obs_columns(
    attributes: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    attributes
        .get("column-order")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
}
