use serde::{Deserialize, Serialize};

/// Metadata for an OME-Zarr dataset
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetMetadata {
    /// Multiscale metadata (from .zattrs)
    pub multiscales: Vec<Multiscale>,
    /// OMERO rendering metadata if present
    pub omero: Option<OmeroMetadata>,
}

/// OME-NGFF multiscale specification
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Multiscale {
    pub axes: Vec<Axis>,
    pub datasets: Vec<MultiscaleDataset>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A single axis in the multiscale specification (e.g. x, y, z, c, t).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    pub name: String,
    #[serde(rename = "type")]
    pub axis_type: Option<String>,
    pub unit: Option<String>,
}

/// Reference to a single resolution level within a multiscale pyramid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultiscaleDataset {
    pub path: String,
    #[serde(default)]
    #[serde(rename = "coordinateTransformations")]
    pub coordinate_transformations: Option<Vec<CoordinateTransformation>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoordinateTransformation {
    #[serde(rename = "scale")]
    Scale { scale: Vec<f64> },
    #[serde(rename = "translation")]
    Translation { translation: Vec<f64> },
}

/// OMERO rendering metadata
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OmeroMetadata {
    pub channels: Vec<OmeroChannel>,
    #[serde(default)]
    pub rdefs: Option<OmeroRdefs>,
}

/// OMERO rendering settings for a single channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OmeroChannel {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub window: Option<ChannelWindow>,
}

/// Contrast window (min/max display range) for a channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelWindow {
    pub start: f64,
    pub end: f64,
    pub min: f64,
    pub max: f64,
}

/// OMERO rendering defaults (default z/t indices, color model).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OmeroRdefs {
    #[serde(default)]
    pub model: Option<String>,
}

/// Information about a single resolution level array
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrayInfo {
    pub shape: Vec<u64>,
    pub chunks: Vec<u64>,
    pub dtype: String,
    /// Byte order: "<" little-endian, ">" big-endian
    pub order: Option<String>,
    pub compressor: Option<serde_json::Value>,
}

/// Full info returned by the /api/info endpoint
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub metadata: DatasetMetadata,
    pub arrays: Vec<ArrayInfo>,
}

/// Request for a specific chunk
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkRequest {
    /// Resolution level index
    pub level: usize,
    /// Chunk coordinates (e.g. [0, 0, 0, 2, 3] for t,c,z,y,x)
    pub chunk_coords: Vec<u64>,
}

/// Viewer state shared between frontend components
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewerState {
    /// Which channels are visible
    pub channels: Vec<ChannelState>,
    /// Current zoom level
    pub zoom: f64,
    /// Pan offset (x, y)
    pub pan: (f64, f64),
    /// Current resolution level being viewed
    pub current_level: usize,
    /// Current Z slice
    pub z_slice: u32,
    /// Current T (time) index
    pub t_index: u32,
}

/// Per-channel state in the shared viewer state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    pub index: usize,
    pub visible: bool,
    pub color: [f32; 3],
    pub contrast_min: f64,
    pub contrast_max: f64,
    pub opacity: f32,
}

/// Configuration for the server
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Path to the OME-Zarr store directory
    pub store: String,
    /// Bind address (e.g. "127.0.0.1:8080")
    pub bind: String,
}

// ---------------------------------------------------------------------------
// Session / layer model (PLAN.md phase 0)
//
// A session is an ordered list of layers, bottom to top. A layer names a source
// (a URI the server can resolve) and a kind, and the kind carries whatever
// metadata that kind of data has: an image layer carries the multiscale
// `DatasetInfo` the viewer already speaks, a label layer carries that plus its
// id colouring, an object layer carries a column schema instead of an array.
// ---------------------------------------------------------------------------

/// Everything the frontend needs to render a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub layers: Vec<LayerInfo>,
}

/// One layer: an identity, a source and a kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerInfo {
    /// Stable id, used as `layer=` in tile/object requests.
    pub id: String,
    /// Human-readable name, shown in the layer list.
    pub name: String,
    /// The source URI as given (`file:///…`, `http(s)://…`, `s3://…`).
    pub source: String,
    pub kind: LayerKind,
}

/// What a layer holds, and the metadata that kind of data carries.
///
/// A flat `.npy` volume is an [`LayerKind::Image`] with exactly one level:
/// there is no third kind of pixel layer, only a source that happens to have no
/// pyramid, and the viewer draws it the same way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerKind {
    /// A multiscale intensity image — the path this viewer started with.
    Image { dataset: DatasetInfo },
    /// A multiscale volume of integer instance/region ids.
    ///
    /// Rendered from the array's own dtype (never through f32, which cannot
    /// hold ids above 2^24 exactly) with nearest sampling.
    Labels {
        dataset: DatasetInfo,
        /// OME-NGFF `image-label` colours, when the store declares them.
        #[serde(default)]
        colors: Option<Vec<LabelColor>>,
        /// Id of an object layer whose rows describe these ids, when one is loaded.
        #[serde(default)]
        properties: Option<String>,
    },
    /// A set of objects: a position per row plus typed columns.
    Objects { schema: ObjectSchema, count: u64 },
}

/// One entry of an OME-NGFF `image-label` colour table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LabelColor {
    #[serde(rename = "label-value")]
    pub label_value: f64,
    /// RGBA, 0-255.
    pub rgba: Option<[i32; 4]>,
}

/// The columns an object layer carries, beside the position every row has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectSchema {
    pub columns: Vec<ObjectColumn>,
    /// True when rows carry a meaningful z (a 2D detector's rows do not).
    pub has_z: bool,
    /// Axis-aligned bounds of the set, `[z0, y0, x0, z1, y1, x1]`, in the
    /// coordinate space of the layer the objects were detected in.
    pub bounds: Option<[f64; 6]>,
}

/// One column of an object table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectColumn {
    pub name: String,
    /// `"u64"` or `"f64"`. A u64 column is exact end to end and is never
    /// converted to a float on the way to the client.
    #[serde(rename = "type")]
    pub kind: String,
    /// Observed range, for auto-scaling a filter slider.
    #[serde(default)]
    pub range: Option<[f64; 2]>,
}
