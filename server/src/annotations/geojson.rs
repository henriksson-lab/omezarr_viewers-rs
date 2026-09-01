//! QuPath's GeoJSON dialect, read and written faithfully.
//!
//! This is the viewer's **native** annotation format — the ROI table beside it
//! (`roi_table.rs`) is for interoperating with the ngio/Fractal world, and can
//! only hold axis-aligned boxes. Here the geometry is whatever was drawn.
//!
//! `info_annotation_formats.md` has the analysis; the short version is that
//! OME-Zarr specifies no vector annotation, OME-XML's ROI model cannot express
//! a polygon with a hole, and QuPath's dialect is both a real standard
//! underneath (RFC 7946) and the thing the tool we mean to replace reads.
//!
//! # The dialect, as QuPath writes it
//!
//! A `FeatureCollection` of `Feature`s. Each feature's `geometry` is a plain
//! RFC 7946 geometry with two **foreign members** QuPath adds:
//!
//! * `plane` — `{"c": -1, "z": 3, "t": 0}`, omitted when it is the default.
//! * `isEllipse` — the geometry is a polygonised ellipse; QuPath rebuilds one
//!   from the bounding box on read. Load-bearing, because a polygonised ellipse
//!   is not recoverable from its vertices. A *rectangle* needs no such flag:
//!   QuPath recognises one by inspecting the ring, and so does this reader.
//!
//! and `properties` carries `objectType`, `name`, `color`, `classification`,
//! `isLocked`, `measurements`, `metadata` and nested `childObjects`.
//!
//! # Coordinates
//!
//! Full-resolution pixels, origin top-left, y downwards — which is exactly this
//! viewer's world, so nothing is converted on the way in or out. That is the
//! single biggest practical advantage over the ROI table, whose `*_micrometer`
//! columns are unrecoverable unless the scale used is recorded alongside.
//!
//! # Our one deviation
//!
//! QuPath and OME-XML both give a shape exactly one plane. An annotation here
//! may span a range ([`Annotation::z_extent`]), which is written as
//! `zExtent`/`tExtent` in `properties` and declared in the group attributes.
//! QuPath ignores members it does not know, so a file with these still opens
//! there — as a shape on the first plane of its range, which is the honest
//! degradation.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{Annotation, Geometry, ObjectType, Plane};
use serde_json::{json, Map, Value};

/// The `properties` members this viewer adds beyond QuPath's.
const Z_EXTENT: &str = "zExtent";
const T_EXTENT: &str = "tExtent";
/// The width a stroke covers, in world pixels. See `Annotation::stroke_width`
/// for why an absent one is a geometric line rather than a zero-width stroke.
const STROKE_WIDTH: &str = "strokeWidth";
/// Marks a shape as asserting that everything inside it is annotated. See
/// `Annotation::dense_region` for why sparse cannot be the only mode.
const DENSE_REGION: &str = "denseRegion";

/// Parse a `FeatureCollection` — or a bare feature, or an array of them.
///
/// All three are in the wild: QuPath writes a collection by default but can
/// write an array, and hand-made files are often a single feature. Accepting
/// each costs one match and saves the user finding out by error message.
pub fn parse(bytes: &[u8]) -> Result<Vec<Annotation>> {
    let value: Value = serde_json::from_slice(bytes).context("parsing the annotations as JSON")?;
    let features = match &value {
        Value::Object(map) => match map.get("type").and_then(Value::as_str) {
            Some("FeatureCollection") => map
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .context("a FeatureCollection with no `features` array")?,
            Some("Feature") => vec![value.clone()],
            _ => bail!("not GeoJSON: the root object is neither a Feature nor a FeatureCollection"),
        },
        Value::Array(items) => items.clone(),
        _ => bail!("not GeoJSON: the root is neither an object nor an array"),
    };

    let mut out = Vec::new();
    for feature in &features {
        read_feature(feature, None, &mut out)?;
    }
    Ok(out)
}

/// Serialise as a `FeatureCollection`, nesting children under their parents.
pub fn write(annotations: &[Annotation]) -> Result<Vec<u8>> {
    // Roots first, then each child under the parent it names. A parent that is
    // not in the set at all leaves its children at the top level rather than
    // dropping them: losing an annotation because its parent was deleted would
    // be the worst possible reading of a dangling id.
    let known: Vec<u64> = annotations.iter().map(|a| a.id).collect();
    let features: Vec<Value> = annotations
        .iter()
        .filter(|a| !a.parent.is_some_and(|p| known.contains(&p)))
        .map(|a| write_feature(a, annotations))
        .collect::<Result<_>>()?;

    let collection = json!({
        "type": "FeatureCollection",
        "features": features,
    });
    serde_json::to_vec_pretty(&collection).context("serialising the annotations")
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

fn read_feature(feature: &Value, parent: Option<u64>, out: &mut Vec<Annotation>) -> Result<()> {
    let Some(object) = feature.as_object() else {
        bail!("a feature that is not an object");
    };
    // A `root` object is QuPath's hierarchy anchor and has no geometry of its
    // own; its children are what matter, so it is descended into and dropped.
    let geometry_value = object.get("geometry");
    let is_root = object
        .get("properties")
        .and_then(|p| p.get("objectType"))
        .and_then(Value::as_str)
        == Some("root");

    let mut id = None;
    if let Some(geometry_value) = geometry_value.filter(|v| !v.is_null()) {
        if !is_root {
            let mut annotation = read_geometry(geometry_value)?;
            // A cell object's nucleus rides beside its main geometry. Its plane
            // and ellipse flag are the object's, not its own.
            annotation.nucleus = object
                .get("nucleusGeometry")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            annotation.parent = parent;
            annotation.uuid = object.get("id").and_then(Value::as_str).map(str::to_string);
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                read_properties(properties, &mut annotation);
            }
            // The index within this batch stands in for an id until the set
            // assigns real ones; `AnnotationSet::from_rows` rewrites both this
            // and the children's `parent` to match.
            annotation.id = out.len() as u64;
            id = Some(annotation.id);
            out.push(annotation);
        }
    }

    if let Some(children) = object
        .get("properties")
        .and_then(|p| p.get("childObjects"))
        .and_then(Value::as_array)
    {
        for child in children {
            read_feature(child, id.or(parent), out)?;
        }
    }
    Ok(())
}

/// One GeoJSON geometry, plus QuPath's two foreign members.
fn read_geometry(value: &Value) -> Result<Annotation> {
    let geometry: Geometry = serde_json::from_value(value.clone()).with_context(|| match value
        .get("type")
        .and_then(Value::as_str)
    {
        Some(kind) => format!("`{kind}` is not a geometry this viewer draws"),
        None => "a geometry with no `type`".to_string(),
    })?;

    let plane = value
        .get("plane")
        .and_then(|p| {
            Some(Plane {
                c: p.get("c").and_then(Value::as_i64).unwrap_or(-1) as i32,
                z: p.get("z").and_then(Value::as_i64)? as i32,
                t: p.get("t").and_then(Value::as_i64).unwrap_or(0) as i32,
            })
        })
        .unwrap_or_default();

    Ok(Annotation {
        is_ellipse: value
            .get("isEllipse")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        geometry,
        plane,
        ..Default::default()
    })
}

fn read_properties(properties: &Map<String, Value>, into: &mut Annotation) {
    if let Some(kind) = properties.get("objectType").and_then(Value::as_str) {
        into.object_type = ObjectType::parse(kind);
    }
    into.name = properties
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    into.color = properties.get("color").and_then(read_color);
    into.locked = properties
        .get("isLocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    into.missing = properties
        .get("isMissing")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Some(classification) = properties.get("classification") {
        // QuPath writes a simple class as `name` and a derived one as `names`;
        // joining with ": " is how QuPath itself renders the derived form, so
        // the round trip through a single text field is exact.
        into.label = classification
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let names = classification.get("names")?.as_array()?;
                Some(
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(": "),
                )
            })
            .unwrap_or_default();
        into.class_color = classification.get("color").and_then(read_color);
    }

    if let Some(measurements) = properties.get("measurements") {
        into.measurements = read_measurements(measurements);
    }
    if let Some(metadata) = properties.get("metadata").and_then(Value::as_object) {
        into.metadata = metadata
            .iter()
            .map(|(key, value)| {
                let text = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (key.clone(), text)
            })
            .collect();
    }
    into.z_extent = properties
        .get(Z_EXTENT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    into.t_extent = properties
        .get(T_EXTENT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    // Only a positive, finite width is a stroke. A zero or a NaN in the file is
    // a line that says nothing about area, which is exactly what `None` means,
    // so it is not carried through as a width nobody can rasterise.
    into.dense_region = properties
        .get(DENSE_REGION)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    into.stroke_width = properties
        .get(STROKE_WIDTH)
        .and_then(Value::as_f64)
        .filter(|w| w.is_finite() && *w > 0.0);
}

/// `[r, g, b]` or `[r, g, b, a]`; the alpha is dropped, since nothing here
/// draws a per-object alpha.
fn read_color(value: &Value) -> Option<[u8; 3]> {
    let array = value.as_array()?;
    let channel = |i: usize| -> u8 {
        array
            .get(i)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 255) as u8
    };
    (array.len() >= 3).then(|| [channel(0), channel(1), channel(2)])
}

/// An object of name → number (QuPath ≥ 0.4), or the older array of
/// `{"name":…, "value":…}`. Both are accepted, as QuPath accepts both.
///
/// Non-finite values arrive as the *strings* `"NaN"`, `"Infinity"`,
/// `"-Infinity"`, which is what QuPath writes; they are dropped rather than
/// turned into a number that would be wrong in a different way.
fn read_measurements(value: &Value) -> std::collections::BTreeMap<String, f64> {
    let mut out = std::collections::BTreeMap::new();
    match value {
        Value::Object(map) => {
            for (name, value) in map {
                if let Some(number) = value.as_f64().filter(|n| n.is_finite()) {
                    out.insert(name.clone(), number);
                }
            }
        }
        Value::Array(rows) => {
            for row in rows {
                let (Some(name), Some(number)) = (
                    row.get("name").and_then(Value::as_str),
                    row.get("value").and_then(Value::as_f64),
                ) else {
                    continue;
                };
                if number.is_finite() {
                    out.insert(name.to_string(), number);
                }
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn write_feature(annotation: &Annotation, all: &[Annotation]) -> Result<Value> {
    let mut geometry =
        serde_json::to_value(&annotation.geometry).context("serialising a geometry")?;
    if let Some(object) = geometry.as_object_mut() {
        // Foreign members, in QuPath's spelling. `plane` is omitted when it is
        // the default, exactly as QuPath omits it.
        if !annotation.plane.is_default() {
            object.insert(
                "plane".into(),
                json!({
                    "c": annotation.plane.c,
                    "z": annotation.plane.z,
                    "t": annotation.plane.t,
                }),
            );
        }
        if annotation.is_ellipse {
            object.insert("isEllipse".into(), json!(true));
        }
    }

    let mut properties = Map::new();
    properties.insert("objectType".into(), json!(annotation.object_type.as_str()));
    if let Some(name) = &annotation.name {
        properties.insert("name".into(), json!(name));
    }
    if let Some([r, g, b]) = annotation.color {
        properties.insert("color".into(), json!([r, g, b]));
    }
    if let Some(width) = annotation.stroke_width {
        properties.insert(STROKE_WIDTH.into(), json!(width));
    }
    if annotation.dense_region {
        properties.insert(DENSE_REGION.into(), json!(true));
    }
    if !annotation.label.is_empty() {
        let mut classification = Map::new();
        // A derived class round-trips as QuPath writes it: `names` when the
        // label has parts, `name` when it is one.
        let parts: Vec<&str> = annotation.label.split(": ").collect();
        if parts.len() > 1 {
            classification.insert("names".into(), json!(parts));
        } else {
            classification.insert("name".into(), json!(annotation.label));
        }
        if let Some([r, g, b]) = annotation.class_color {
            classification.insert("color".into(), json!([r, g, b]));
        }
        properties.insert("classification".into(), Value::Object(classification));
    }
    if annotation.locked {
        // Written only when true, as QuPath does.
        properties.insert("isLocked".into(), json!(true));
    }
    if annotation.missing {
        properties.insert("isMissing".into(), json!(true));
    }
    if !annotation.measurements.is_empty() {
        properties.insert(
            "measurements".into(),
            Value::Object(
                annotation
                    .measurements
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
    }
    if !annotation.metadata.is_empty() {
        properties.insert(
            "metadata".into(),
            Value::Object(
                annotation
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );
    }
    // Ours. Written only when set, so a file of ordinary one-plane shapes is
    // byte-for-byte what QuPath would have written.
    if annotation.z_extent > 0 {
        properties.insert(Z_EXTENT.into(), json!(annotation.z_extent));
    }
    if annotation.t_extent > 0 {
        properties.insert(T_EXTENT.into(), json!(annotation.t_extent));
    }

    let children: Vec<Value> = all
        .iter()
        .filter(|child| child.parent == Some(annotation.id))
        .map(|child| write_feature(child, all))
        .collect::<Result<_>>()?;
    if !children.is_empty() {
        properties.insert("childObjects".into(), Value::Array(children));
    }

    let mut feature = Map::new();
    feature.insert("type".into(), json!("Feature"));
    // QuPath's own UUID when the annotation came from there, so a round trip
    // does not renumber somebody else's objects.
    if let Some(uuid) = &annotation.uuid {
        feature.insert("id".into(), json!(uuid));
    }
    feature.insert("geometry".into(), geometry);
    if let Some(nucleus) = &annotation.nucleus {
        feature.insert(
            "nucleusGeometry".into(),
            serde_json::to_value(nucleus).context("serialising a nucleus")?,
        );
    }
    feature.insert("properties".into(), Value::Object(properties));
    Ok(Value::Object(feature))
}

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

use super::roi_table::{
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

#[cfg(test)]
mod tests {
    use super::*;
    use omezarr_viewer_common::Geometry;

    fn square(x: f64, y: f64, size: f64) -> Vec<[f64; 2]> {
        vec![
            [x, y],
            [x + size, y],
            [x + size, y + size],
            [x, y + size],
            [x, y],
        ]
    }

    /// A feature collection as QuPath 0.4+ actually writes one.
    const QUPATH: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {
          "type": "Feature",
          "id": "e3b0c442-98fc-1c14-9afb-4c8996fb9242",
          "geometry": {
            "type": "Polygon",
            "coordinates": [
              [[100,200],[300,200],[300,400],[100,400],[100,200]],
              [[150,250],[200,250],[200,300],[150,300],[150,250]]
            ],
            "plane": {"c": -1, "z": 3, "t": 1}
          },
          "properties": {
            "objectType": "annotation",
            "name": "Region 1",
            "color": [255, 0, 0],
            "classification": {"names": ["Tumor", "Positive"], "color": [200, 0, 0]},
            "isLocked": true,
            "measurements": {"Area": 1234.5, "Bad": "NaN"},
            "metadata": {"note": "checked"},
            "childObjects": [
              {
                "type": "Feature",
                "geometry": {"type": "Point", "coordinates": [150, 260]},
                "nucleusGeometry": {
                  "type": "Polygon",
                  "coordinates": [[[148,258],[152,258],[152,262],[148,262],[148,258]]]
                },
                "properties": {"objectType": "cell", "classification": {"name": "Cell"}}
              }
            ]
          }
        },
        {
          "type": "Feature",
          "geometry": {
            "type": "Polygon",
            "coordinates": [[[0,0],[10,0],[10,10],[0,10],[0,0]]],
            "isEllipse": true
          },
          "properties": {"objectType": "annotation"}
        }
      ]
    }"#;

    #[test]
    fn reads_everything_qupath_writes() {
        let rows = parse(QUPATH.as_bytes()).unwrap();
        assert_eq!(rows.len(), 3, "two features plus a nested child");

        let region = &rows[0];
        assert_eq!(
            region.uuid.as_deref(),
            Some("e3b0c442-98fc-1c14-9afb-4c8996fb9242")
        );
        assert_eq!(region.plane, Plane { c: -1, z: 3, t: 1 });
        assert_eq!(region.name.as_deref(), Some("Region 1"));
        assert_eq!(region.color, Some([255, 0, 0]));
        assert_eq!(region.label, "Tumor: Positive", "a derived class joins");
        assert_eq!(region.class_color, Some([200, 0, 0]));
        assert!(region.locked);
        assert_eq!(
            region.metadata.get("note").map(String::as_str),
            Some("checked")
        );
        assert_eq!(region.measurements.get("Area"), Some(&1234.5));
        assert!(
            !region.measurements.contains_key("Bad"),
            "`NaN` is a string in this format and is not a measurement"
        );
        // The hole survived, which is the whole reason for this format.
        let Geometry::Polygon(rings) = &region.geometry else {
            panic!("not a polygon");
        };
        assert_eq!(rings.len(), 2);
        assert!(!region.geometry.contains(175.0, 275.0, 0.0), "in the hole");
        assert!(region.geometry.contains(280.0, 380.0, 0.0), "in the ring");

        // The child came through as a child.
        let cell = &rows[1];
        assert_eq!(cell.parent, Some(region.id));
        assert_eq!(cell.object_type, ObjectType::Cell);
        assert_eq!(cell.label, "Cell");
        assert!(matches!(cell.geometry, Geometry::Point(_)));
        // A cell's second geometry. Dropping it would lose half of every cell
        // a QuPath segmentation produced.
        let nucleus = cell.nucleus.as_ref().expect("the nucleus came through");
        assert_eq!(nucleus.bounds(), Some([148.0, 258.0, 152.0, 262.0]));

        // And the ellipse flag, which cannot be recovered from the vertices.
        assert!(rows[2].is_ellipse);
        assert_eq!(rows[2].plane, Plane::default());
    }

    #[test]
    fn a_round_trip_preserves_everything_it_read() {
        let rows = parse(QUPATH.as_bytes()).unwrap();
        let written = write(&rows).unwrap();
        let back = parse(&written).unwrap();
        assert_eq!(back.len(), rows.len());
        for (before, after) in rows.iter().zip(&back) {
            assert_eq!(before.geometry, after.geometry);
            assert_eq!(before.plane, after.plane);
            assert_eq!(before.label, after.label);
            assert_eq!(before.class_color, after.class_color);
            assert_eq!(before.name, after.name);
            assert_eq!(before.color, after.color);
            assert_eq!(before.object_type, after.object_type);
            assert_eq!(before.locked, after.locked);
            assert_eq!(before.is_ellipse, after.is_ellipse);
            assert_eq!(before.measurements, after.measurements);
            assert_eq!(before.metadata, after.metadata);
            assert_eq!(before.nucleus, after.nucleus);
            assert_eq!(before.missing, after.missing);
            assert_eq!(before.uuid, after.uuid);
            assert_eq!(before.parent, after.parent);
        }
    }

    #[test]
    fn the_written_shape_is_the_dialect_qupath_reads() {
        let mut region = Annotation {
            geometry: Geometry::Polygon(vec![square(0.0, 0.0, 10.0)]),
            label: "Tumor".into(),
            ..Default::default()
        };
        region.id = 7;
        let child = Annotation {
            id: 8,
            geometry: Geometry::Point([5.0, 5.0]),
            object_type: ObjectType::Detection,
            parent: Some(7),
            ..Default::default()
        };
        let value: Value = serde_json::from_slice(&write(&[region, child]).unwrap()).unwrap();

        assert_eq!(value["type"], "FeatureCollection");
        let features = value["features"].as_array().unwrap();
        assert_eq!(features.len(), 1, "the child nests, it does not sit beside");
        let feature = &features[0];
        assert_eq!(feature["type"], "Feature");
        assert_eq!(feature["geometry"]["type"], "Polygon");
        assert!(
            feature["geometry"].get("plane").is_none(),
            "the default plane is omitted, as QuPath omits it"
        );
        assert_eq!(feature["properties"]["objectType"], "annotation");
        assert_eq!(feature["properties"]["classification"]["name"], "Tumor");
        assert!(
            feature["properties"].get("isLocked").is_none(),
            "written only when true"
        );
        let children = feature["properties"]["childObjects"].as_array().unwrap();
        assert_eq!(children[0]["properties"]["objectType"], "detection");
    }

    #[test]
    fn a_stroke_carries_its_width_and_a_bare_line_carries_none() {
        // The distinction this pins is the whole point of storing a width: a
        // `LineString` with no width is a mathematical curve covering no pixels,
        // and the same path with a width is a scribble covering a capsule. A
        // reader that collapsed the two would turn "I painted here" into "I drew
        // an infinitely thin line", which asserts nothing about any pixel.
        let scribble = Annotation {
            geometry: Geometry::LineString(vec![[0.0, 0.0], [10.0, 4.0]]),
            stroke_width: Some(11.0),
            ..Default::default()
        };
        let bare = Annotation {
            geometry: Geometry::LineString(vec![[0.0, 0.0], [10.0, 4.0]]),
            ..Default::default()
        };
        let written = write(&[scribble.clone(), bare.clone()]).unwrap();
        let value: Value = serde_json::from_slice(&written).unwrap();
        assert_eq!(value["features"][0]["properties"]["strokeWidth"], 11.0);
        assert!(
            value["features"][1]["properties"]
                .get("strokeWidth")
                .is_none(),
            "a line with no width writes no member, rather than a zero that reads\
             back as a stroke covering nothing"
        );

        let back = parse(&written).unwrap();
        assert_eq!(back[0].stroke_width, Some(11.0));
        assert_eq!(back[1].stroke_width, None);
    }

    #[test]
    fn a_width_that_cannot_be_rasterised_is_not_a_width() {
        // Zero and negative reach us from files this viewer did not write, and
        // each is a line rather than a stroke: there is no set of pixels within
        // `w / 2` of the path for either. Non-finite is not tested because JSON
        // cannot express one — `NaN` and `Infinity` are not JSON numbers, so a
        // width that survives parsing is already finite.
        for bad in [json!(0), json!(-3.5)] {
            let feature = json!({
                "type": "Feature",
                "geometry": {"type": "LineString", "coordinates": [[0, 0], [5, 5]]},
                "properties": {"strokeWidth": bad},
            });
            let back = parse(feature.to_string().as_bytes()).unwrap();
            assert_eq!(back[0].stroke_width, None, "width {bad} is not a stroke");
        }
    }

    #[test]
    fn a_z_range_is_written_as_our_own_member_and_nothing_else_changes() {
        let annotation = Annotation {
            geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
            plane: Plane::at(2, 0),
            z_extent: 5,
            ..Default::default()
        };
        let value: Value = serde_json::from_slice(&write(&[annotation]).unwrap()).unwrap();
        let properties = &value["features"][0]["properties"];
        assert_eq!(properties["zExtent"], 5);
        assert!(properties.get("tExtent").is_none(), "zero is not written");
        // The geometry itself stays plain GeoJSON, so QuPath opens the file and
        // simply ignores the member it does not know.
        assert_eq!(value["features"][0]["geometry"]["type"], "Polygon");
        assert_eq!(value["features"][0]["geometry"]["plane"]["z"], 2);

        let back = parse(
            &write(&[Annotation {
                geometry: Geometry::rect(0.0, 0.0, 10.0, 10.0),
                plane: Plane::at(2, 0),
                z_extent: 5,
                t_extent: 3,
                ..Default::default()
            }])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back[0].z_extent, 5);
        assert_eq!(back[0].t_extent, 3);
    }

    #[test]
    fn a_bare_feature_and_an_array_of_features_are_both_accepted() {
        let one = r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
                      "properties":{}}"#;
        assert_eq!(parse(one.as_bytes()).unwrap().len(), 1);
        let array = format!("[{one},{one}]");
        assert_eq!(parse(array.as_bytes()).unwrap().len(), 2);
    }

    #[test]
    fn a_root_object_contributes_its_children_and_not_itself() {
        // QuPath's hierarchy export wraps everything in a geometry-less root.
        let text = r#"{"type":"FeatureCollection","features":[{
            "type":"Feature","geometry":null,
            "properties":{"objectType":"root","childObjects":[
              {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
               "properties":{"objectType":"annotation"}}
            ]}}]}"#;
        let rows = parse(text.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].parent, None, "the root is not a parent to keep");
        assert!(matches!(rows[0].geometry, Geometry::Point(_)));
    }

    #[test]
    fn something_that_is_not_geojson_is_refused_by_name() {
        let error = parse(br#"{"tables":["boxes"]}"#).unwrap_err().to_string();
        assert!(error.contains("Feature"), "{error}");

        let bad_geometry = r#"{"type":"Feature","properties":{},
            "geometry":{"type":"Sphere","coordinates":[1,2,3]}}"#;
        let error = parse(bad_geometry.as_bytes()).unwrap_err().to_string();
        assert!(error.contains("Sphere"), "{error}");
    }

    #[test]
    fn every_geometry_type_survives_a_round_trip() {
        let geometries = [
            Geometry::Point([1.0, 2.0]),
            Geometry::MultiPoint(vec![[1.0, 2.0], [3.0, 4.0]]),
            Geometry::LineString(vec![[0.0, 0.0], [10.0, 10.0]]),
            Geometry::MultiLineString(vec![vec![[0.0, 0.0], [1.0, 1.0]]]),
            Geometry::Polygon(vec![square(0.0, 0.0, 10.0), square(2.0, 2.0, 3.0)]),
            Geometry::MultiPolygon(vec![
                vec![square(0.0, 0.0, 5.0)],
                vec![square(20.0, 20.0, 5.0)],
            ]),
        ];
        let rows: Vec<Annotation> = geometries
            .iter()
            .enumerate()
            .map(|(i, geometry)| Annotation {
                id: i as u64,
                geometry: geometry.clone(),
                ..Default::default()
            })
            .collect();
        let back = parse(&write(&rows).unwrap()).unwrap();
        assert_eq!(back.len(), geometries.len());
        for (expected, actual) in geometries.iter().zip(&back) {
            assert_eq!(*expected, actual.geometry);
        }
    }
}
