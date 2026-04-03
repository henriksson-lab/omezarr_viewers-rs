use serde::{Deserialize, Serialize};

/// Metadata for an OME-Zarr dataset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetMetadata {
    /// Multiscale metadata (from .zattrs)
    pub multiscales: Vec<Multiscale>,
    /// OMERO rendering metadata if present
    pub omero: Option<OmeroMetadata>,
}

/// OME-NGFF multiscale specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Multiscale {
    pub axes: Vec<Axis>,
    pub datasets: Vec<MultiscaleDataset>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A single axis in the multiscale specification (e.g. x, y, z, c, t).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Axis {
    pub name: String,
    #[serde(rename = "type")]
    pub axis_type: Option<String>,
    pub unit: Option<String>,
}

/// Reference to a single resolution level within a multiscale pyramid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiscaleDataset {
    pub path: String,
    #[serde(default)]
    #[serde(rename = "coordinateTransformations")]
    pub coordinate_transformations: Option<Vec<CoordinateTransformation>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoordinateTransformation {
    #[serde(rename = "scale")]
    Scale { scale: Vec<f64> },
    #[serde(rename = "translation")]
    Translation { translation: Vec<f64> },
}

/// OMERO rendering metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmeroMetadata {
    pub channels: Vec<OmeroChannel>,
    #[serde(default)]
    pub rdefs: Option<OmeroRdefs>,
}

/// OMERO rendering settings for a single channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelWindow {
    pub start: f64,
    pub end: f64,
    pub min: f64,
    pub max: f64,
}

/// OMERO rendering defaults (default z/t indices, color model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmeroRdefs {
    #[serde(default)]
    pub model: Option<String>,
}

/// Information about a single resolution level array
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrayInfo {
    pub shape: Vec<u64>,
    pub chunks: Vec<u64>,
    pub dtype: String,
    /// Byte order: "<" little-endian, ">" big-endian
    pub order: Option<String>,
    pub compressor: Option<serde_json::Value>,
}

/// Full info returned by the /api/info endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub metadata: DatasetMetadata,
    pub arrays: Vec<ArrayInfo>,
}

/// Request for a specific chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRequest {
    /// Resolution level index
    pub level: usize,
    /// Chunk coordinates (e.g. [0, 0, 0, 2, 3] for t,c,z,y,x)
    pub chunk_coords: Vec<u64>,
}

/// Viewer state shared between frontend components
#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelState {
    pub index: usize,
    pub visible: bool,
    pub color: [f32; 3],
    pub contrast_min: f64,
    pub contrast_max: f64,
    pub opacity: f32,
}

/// Configuration for the server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Path to the OME-Zarr store directory
    pub store: String,
    /// Bind address (e.g. "127.0.0.1:8080")
    pub bind: String,
}
