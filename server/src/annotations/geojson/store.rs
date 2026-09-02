//! Where an annotation set lives, and how it gets there.
//!
//! Separate from the codec because it depends on an entirely different half of
//! the crate — `zarrs`, the source registry, and async I/O — while the codec
//! needs only `serde_json` and an [`Annotation`]. The behaviour here is covered
//! end to end by `server/tests/annotations.rs` rather than by unit tests: what
//! matters about a save is that a *session* can read it back.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::Annotation;
use serde_json::{json, Value};

use super::read::parse;
use super::write::write;

// ---------------------------------------------------------------------------
// Where it lives in the store
// ---------------------------------------------------------------------------
//
// Nothing in OME-Zarr says, so this mirrors `labels/` and `tables/` — the two
// patterns the format and its conventions already use:
//
//     image.zarr
//     ├── 0 … N              multiscale levels
//     ├── labels/            label images            (in the spec)
//     ├── tables/            ngio ROI tables         (convention)
//     └── annotations/       zattrs: {"annotations": ["my_regions"]}
//         └── my_regions/    zattrs: type, version, coordinate_space
//             └── annotations.geojson

use std::path::{Path, PathBuf};
use std::sync::Arc;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;
use zarrs::storage::{ReadableStorageTraits, StoreKey, WritableStorageTraits};

use crate::annotations::roi_table::{
    attributes_at_async, group_for, is_remote, remote, remote_is_v3, store_is_v3,
};
use crate::source::SourceRegistry;
use zarrs::storage::{AsyncReadableStorageTraits, AsyncWritableStorageTraits};

/// The group that indexes annotation sets, and the payload inside each one.
const GROUP: &str = "annotations";
const PAYLOAD: &str = "annotations.geojson";
/// Bumped when the layout below changes in a way a reader must notice.
const VERSION: &str = "1";

/// One annotation set, as it sits on disk.
#[derive(Debug, Clone)]
pub struct AnnotationFile {
    pub rows: Vec<Annotation>,
    /// Whether the file declared the coordinate space this viewer assumes.
    pub declared_space: bool,
}

/// The attributes an annotation group carries.
///
/// The coordinate space is the part that matters: GeoJSON's own convention is
/// WGS84 longitude and latitude, and every bioimaging user of it — QuPath
/// included — silently means pixels instead. RFC 7946 removed the `crs` member,
/// so there is nowhere *inside* the file to say so. Saying it here costs six
/// keys and is the difference between a file that can be read back exactly and
/// one that relies on the reader guessing the same convention.
fn attributes() -> serde_json::Value {
    json!({
        "type": "geojson_annotations",
        "version": VERSION,
        // The foreign members `plane` and `isEllipse` are honoured, and the
        // properties are QuPath's.
        "dialect": "qupath",
        "coordinate_space": {
            "axes": ["x", "y"],
            "units": "pixel",
            "level": 0,
            "origin": "top-left",
            "y_axis": "down",
        },
        // Ours, and a deviation from both QuPath and OME-XML, which give a shape
        // exactly one plane. Declared so a reader is told rather than surprised.
        "extensions": ["zExtent", "tExtent", "strokeWidth", "denseRegion"],
        // How to read the pixels a shape does *not* cover. Sparse is the safe
        // default: a scribble asserts something about the pixels within its own
        // width and nothing about any other, so an uncovered pixel is
        // unexamined rather than background. Inside a shape marked
        // `denseRegion` that flips — there, uncovered means background, because
        // the curator has said they marked every instance in it.
        //
        // A trainer needs this to exist. Without it, sparse annotation read as
        // dense teaches that every unmarked object is background, which is
        // worse training data than none.
        "supervision": {
            "default": "sparse",
            "dense_within": "denseRegion",
        },
        // What a stored shape means in pixels, so that two rasterisers agree.
        // Left undeclared, "a stroke of width 11" is an intention rather than a
        // set of voxels, and two curators' work stops being comparable at the
        // last step.
        //
        // The supersample-and-threshold rule is ilastik's, which resolves the
        // same question the same way for the same reason: an edge pixel is in or
        // out, and antialiasing has to be resolved somewhere.
        "rasterisation": {
            "stroke": "pixels within strokeWidth/2 of the path",
            "cap": "round",
            "join": "round",
            "region": "even-odd over the rings; ring 0 is the exterior",
            "sampling": "4x4 subsamples per pixel, on at 7 of 16 or more",
            // Where a pixel *is*. Writing the rule without this leaves half a
            // voxel of freedom on every axis, which is the whole disagreement
            // the block exists to prevent. The integer is the centre because
            // this viewer routes a vertex to a voxel by rounding, and rounding
            // is the map to the nearest sample; on a corner convention the
            // containing pixel would be a floor and the key would name the
            // wrong voxel for half of every axis.
            "pixel_centre": "the integer coordinate",
            // `sampling` governs the stroke as well as the region. The two
            // agree wherever it matters — a straight stroke selects
            // 2*floor(h)+1 rows under a centre test and 2*floor(h+1/16*2)+1
            // under sixteen subsamples, which is the same for every stroke
            // width whose half has a fractional part below 7/8.
            "sampling_applies_to": ["stroke", "region"],
            // A closed ring with a width is both: filled, and its outline
            // fattened. At zero width that degrades to a plain fill. An open
            // path with no width covers nothing and is refused rather than
            // written as an empty shape.
            "fill_and_stroke": "union",
            // Where two shapes cover one voxel, the higher `shape` wins. An
            // order-free rule, because the alternative is that the answer
            // depends on the order blocks happened to be gathered in.
            "collision": "highest shape id",
            "level": 0,
        },
        "written_by": concat!("omezarr-viewer ", env!("CARGO_PKG_VERSION")),
    })
}

fn set_path(name: &str) -> String {
    format!("/{GROUP}/{name}")
}

fn payload_key(name: &str) -> Result<StoreKey> {
    StoreKey::new(format!("{GROUP}/{name}/{PAYLOAD}")).map_err(|e| anyhow::anyhow!("{e}"))
}

fn filesystem(root: &Path) -> Result<Arc<FilesystemStore>> {
    Ok(Arc::new(
        FilesystemStore::new(root).with_context(|| format!("opening {}", root.display()))?,
    ))
}

fn attributes_at(store: &Arc<FilesystemStore>, path: &str) -> serde_json::Map<String, Value> {
    Group::open(store.clone(), path)
        .map(|group| group.attributes().clone())
        .unwrap_or_default()
}

/// Split a `…/store.zarr/annotations/<name>` target, keeping the store's scheme.
pub fn split_uri_target(target: &str) -> Result<(String, String)> {
    let trimmed = target.trim().trim_end_matches(['/', '\\']);
    let (parent, name) = trimmed
        .rsplit_once(['/', '\\'])
        .context("target names no annotation set")?;
    let (root, group) = parent
        .rsplit_once(['/', '\\'])
        .context("target has no `annotations` parent")?;
    if group != GROUP || name.is_empty() || root.is_empty() {
        bail!("an annotation set lives at <store>/{GROUP}/<name>, got `{target}`");
    }
    Ok((root.to_string(), name.to_string()))
}

/// [`split_uri_target`] for a local target, as a path.
pub fn split_target(target: &str) -> Result<(PathBuf, String)> {
    let (store, name) = split_uri_target(target)?;
    Ok((PathBuf::from(store.trim_start_matches("file://")), name))
}

/// Join a store root and a set name back into a target.
pub fn make_target(root: &Path, name: &str) -> String {
    root.join(GROUP).join(name).display().to_string()
}

/// Join a store URI and a set name back into a target.
pub fn make_uri_target(store: &str, name: &str) -> String {
    format!("{}/{GROUP}/{name}", store.trim_end_matches('/'))
}

/// Does this path name an annotation set rather than an ROI table?
pub fn is_annotation_target(target: &str) -> bool {
    split_uri_target(target).is_ok()
}

/// Is this annotation target one only the async path can reach?
pub fn target_is_remote(target: &str) -> bool {
    is_remote(target)
}

/// The set names an `annotations` group's attributes list.
fn listed(attributes: &serde_json::Map<String, Value>) -> Vec<String> {
    attributes
        .get(GROUP)
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
}

/// A group that declares itself something else is refused by name rather than
/// parsed hopefully — the payload might be anything.
fn check_kind(attributes: &serde_json::Map<String, Value>, name: &str) -> Result<()> {
    match attributes.get("type").and_then(Value::as_str) {
        Some(kind) if kind != "geojson_annotations" => {
            bail!("`{name}` declares itself a `{kind}`, not a GeoJSON annotation set")
        }
        _ => Ok(()),
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        bail!("`{name}` is not an annotation set name");
    }
    Ok(())
}

/// Every annotation set the store's `annotations` group lists.
pub fn list(root: &Path) -> Result<Vec<String>> {
    Ok(listed(&attributes_at(
        &filesystem(root)?,
        &format!("/{GROUP}"),
    )))
}

/// Write `rows` as `<root>/annotations/<name>`, and return the target.
pub fn save(root: &Path, name: &str, rows: &[Annotation]) -> Result<String> {
    check_name(name)?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let v3 = store_is_v3(root);
    let store = filesystem(root)?;

    // The index is merged, never replaced: a store may hold sets this viewer
    // knows nothing about, and dropping their names would hide them from every
    // reader that trusts it.
    let mut index = listed(&attributes_at(&store, &format!("/{GROUP}")));
    if !index.iter().any(|s| s == name) {
        index.push(name.to_string());
    }
    group_for(
        store.clone(),
        v3,
        &format!("/{GROUP}"),
        json!({ GROUP: index }),
    )?
    .store_metadata()
    .context("writing the annotations group")?;
    group_for(store.clone(), v3, &set_path(name), attributes())?
        .store_metadata()
        .context("writing the annotation set group")?;
    store
        .set(&payload_key(name)?, write(rows)?.into())
        .context("writing the annotations")?;

    Ok(make_target(root, name))
}

/// Read `<root>/annotations/<name>`.
pub fn load(root: &Path, name: &str) -> Result<AnnotationFile> {
    let store = filesystem(root)?;
    let attributes = attributes_at(&store, &set_path(name));
    check_kind(&attributes, name)?;
    let declared_space = attributes.contains_key("coordinate_space");

    let bytes = store
        .get(&payload_key(name)?)
        .context("reading the annotations")?
        .with_context(|| format!("no {PAYLOAD} in annotation set `{name}`"))?;

    Ok(AnnotationFile {
        rows: parse(&bytes)?,
        declared_space,
    })
}

// ---------------------------------------------------------------------------
// The remote path: an opendal store, read and written asynchronously
//
// The whole set is one object — `annotations.geojson` — so unlike an AnnData
// table this needs no second decoder, only the same bytes through a different
// store. That is why the format is fully available remotely and AnnData is not.
// ---------------------------------------------------------------------------

/// Every annotation set an `s3://` or `http(s)://` store lists.
pub async fn list_async(registry: &SourceRegistry, store_uri: &str) -> Result<Vec<String>> {
    Ok(listed(
        &attributes_at_async(&remote(registry, store_uri)?, &format!("/{GROUP}")).await,
    ))
}

/// [`save`], for an `s3://` or `http(s)://` store.
pub async fn save_async(
    registry: &SourceRegistry,
    store_uri: &str,
    name: &str,
    rows: &[Annotation],
) -> Result<String> {
    check_name(name)?;
    let store = remote(registry, store_uri)?;
    let v3 = remote_is_v3(&store).await;

    let mut index = listed(&attributes_at_async(&store, &format!("/{GROUP}")).await);
    if !index.iter().any(|s| s == name) {
        index.push(name.to_string());
    }
    group_for(
        store.clone(),
        v3,
        &format!("/{GROUP}"),
        json!({ GROUP: index }),
    )?
    .async_store_metadata()
    .await
    .context("writing the annotations group")?;
    group_for(store.clone(), v3, &set_path(name), attributes())?
        .async_store_metadata()
        .await
        .context("writing the annotation set group")?;
    store
        .set(&payload_key(name)?, write(rows)?.into())
        .await
        .context("writing the annotations")?;

    Ok(make_uri_target(store_uri, name))
}

/// [`load`], for an `s3://` or `http(s)://` store.
pub async fn load_async(
    registry: &SourceRegistry,
    store_uri: &str,
    name: &str,
) -> Result<AnnotationFile> {
    let store = remote(registry, store_uri)?;
    let attributes = attributes_at_async(&store, &set_path(name)).await;
    check_kind(&attributes, name)?;

    let bytes = store
        .get(&payload_key(name)?)
        .await
        .context("reading the annotations")?
        .with_context(|| format!("no {PAYLOAD} in annotation set `{name}`"))?;

    Ok(AnnotationFile {
        rows: parse(&bytes)?,
        declared_space: attributes.contains_key("coordinate_space"),
    })
}

/// Read a bare `.geojson` file — what QuPath's export writes.
///
/// Kept separate from [`load`] because a file somebody exported from QuPath has
/// no group and no declared coordinate space; it is read on the understanding
/// that QuPath's convention and this viewer's world are the same, which
/// `info_annotation_formats.md` establishes and which is the whole reason for
/// choosing the format.
pub fn load_file(path: &Path) -> Result<AnnotationFile> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(AnnotationFile {
        rows: parse(&bytes)?,
        declared_space: false,
    })
}

/// Write a bare `.geojson` file, for handing to QuPath.
pub fn save_file(path: &Path, rows: &[Annotation]) -> Result<()> {
    std::fs::write(path, write(rows)?).with_context(|| format!("writing {}", path.display()))
}
