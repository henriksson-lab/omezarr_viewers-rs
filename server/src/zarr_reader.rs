//! Reading OME-Zarr arrays, from disk or from an object store.
//!
//! Two backends, and the split is sync vs async rather than local vs remote: a
//! filesystem store is read on the calling thread through `zarrs::filesystem`,
//! and everything else goes through `opendal`. Which one a source gets is
//! [`crate::source::SourceRegistry`]'s answer, not this module's.
//!
//! Array handles are opened once per level and kept. Opening one re-reads the
//! array metadata, which is a file read on disk and a request over S3, and a
//! viewer asks for tiles from the same level thousands of times in a row.

use anyhow::{Context, Result};
use omezarr_viewer_common::{ArrayInfo, DatasetInfo, DatasetMetadata, Multiscale, OmeroMetadata};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zarrs::array::Array;
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;
use zarrs_opendal::AsyncOpendalStore;

use crate::source::{SourceRegistry, SourceSpec};

/// How tile bytes are handed to the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileEncoding {
    /// Normalised to `float32`. The intensity path, and lossy for wide
    /// integers by construction.
    F32,
    /// The array's own dtype, unconverted.
    ///
    /// The label path. An id above `2^24` does not survive an f32 round trip,
    /// and nearest-sampling an averaged id yields an id that does not exist, so
    /// a label layer must never travel as f32.
    Raw,
}

impl TileEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            TileEncoding::F32 => "f32",
            TileEncoding::Raw => "raw",
        }
    }

    /// Parse the `encoding=` query parameter; anything unrecognised is f32,
    /// which is what a client that does not know about encodings wants.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("raw") => TileEncoding::Raw,
            _ => TileEncoding::F32,
        }
    }
}

/// A projection through the z axis, replacing a single slice.
///
/// Computed here rather than on the client: a 32-plane maximum over a tile is
/// 32 tiles' worth of bytes over the wire and one tile's worth after the
/// reduction, so the only sensible place to do it is where the bytes already
/// are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Projection {
    Max,
    Mean,
}

impl Projection {
    pub fn as_str(self) -> &'static str {
        match self {
            Projection::Max => "max",
            Projection::Mean => "mean",
        }
    }

    /// Parse `zproj=`; anything else means no projection.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("max") => Some(Projection::Max),
            Some("mean") => Some(Projection::Mean),
            _ => None,
        }
    }
}

/// Which axis a plane is taken across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneAxis {
    /// The ordinary view: a `(y, x)` plane at one z.
    Z,
    /// A `(z, x)` plane at one y — the pane below the main view.
    Y,
    /// A `(z, y)` plane at one x — the pane beside the main view.
    X,
}

impl PlaneAxis {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaneAxis::Z => "z",
            PlaneAxis::Y => "y",
            PlaneAxis::X => "x",
        }
    }

    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("y") => PlaneAxis::Y,
            Some("x") => PlaneAxis::X,
            _ => PlaneAxis::Z,
        }
    }
}

/// A whole plane through the volume, at one level.
///
/// Planes are read whole rather than tiled, and that is a deliberate limit
/// rather than an oversight: the orthogonal panes are secondary views a few
/// hundred pixels tall, the client picks a level that fits them, and one
/// request per pane is far less machinery than a second tile grid whose axes
/// are not the ones the store is chunked along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneRequest {
    pub level: usize,
    pub t: u64,
    pub c: u64,
    pub axis: PlaneAxis,
    /// The index along `axis`.
    pub index: u64,
    pub encoding: TileEncoding,
}

/// A plane's bytes and the shape they came back in.
pub struct PlaneBytes {
    pub bytes: Vec<u8>,
    pub dtype: String,
    /// Rows, columns — `(z, x)` for a y plane, `(z, y)` for an x plane.
    pub height: u64,
    pub width: u64,
}

/// One tile read: which array, which region, and how to encode the answer.
///
/// A struct rather than nine positional arguments, because phase 3 adds a
/// projection to the same request and a tenth positional `u64` is a bug waiting
/// for a caller to transpose two of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileRequest {
    pub level: usize,
    pub t: u64,
    pub c: u64,
    pub z: u64,
    pub y: u64,
    pub x: u64,
    pub h: u64,
    pub w: u64,
    pub encoding: TileEncoding,
    /// A z projection over `z .. z + depth`, when the viewer asked for one.
    pub projection: Option<Projection>,
    /// How many z planes the projection covers. Ignored without one.
    pub depth: u64,
}

impl TileRequest {
    /// A tile at the origin of `level`, sized `h` x `w`, in f32.
    pub fn new(level: usize, y: u64, x: u64, h: u64, w: u64) -> Self {
        Self {
            level,
            t: 0,
            c: 0,
            z: 0,
            y,
            x,
            h,
            w,
            encoding: TileEncoding::F32,
            projection: None,
            depth: 1,
        }
    }

    pub fn at(mut self, t: u64, c: u64, z: u64) -> Self {
        self.t = t;
        self.c = c;
        self.z = z;
        self
    }

    pub fn encoded(mut self, encoding: TileEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Project over `depth` planes starting at this request's `z`.
    pub fn projected(mut self, projection: Option<Projection>, depth: u64) -> Self {
        self.projection = projection;
        self.depth = depth.max(1);
        self
    }

    /// The z planes this request covers.
    fn z_range(&self) -> std::ops::Range<u64> {
        match self.projection {
            Some(_) => self.z..self.z + self.depth.max(1),
            None => self.z..self.z + 1,
        }
    }
}

/// Encoded tile bytes and the dtype they are in.
pub struct TileBytes {
    pub bytes: Vec<u8>,
    /// The dtype on the wire — `"float32"` for [`TileEncoding::F32`], the
    /// array's own for [`TileEncoding::Raw`].
    pub dtype: String,
}

/// S3 connection configuration for the dataset-listing UI.
#[derive(Clone)]
pub struct S3Config {
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub prefix: String,
}

/// Abstraction over local filesystem and opendal-backed zarr stores, each with
/// its own per-level handle cache.
enum StoreBackend {
    Local {
        store: Arc<FilesystemStore>,
        arrays: Mutex<HashMap<usize, Arc<Array<FilesystemStore>>>>,
    },
    Async {
        store: Arc<AsyncOpendalStore>,
        arrays: Mutex<HashMap<usize, Arc<Array<AsyncOpendalStore>>>>,
    },
}

/// Opened OME-Zarr store with parsed metadata.
pub struct ZarrStore {
    backend: StoreBackend,
    metadata: DatasetInfo,
    /// Raw group attributes, kept so a layer can ask questions this struct does
    /// not answer — `image-label` colours, for one.
    attributes: serde_json::Map<String, serde_json::Value>,
}

impl ZarrStore {
    /// Open a store named by a source spec, resolving S3 profiles through the
    /// registry.
    pub async fn open_spec(registry: &SourceRegistry, spec: &SourceSpec) -> Result<Self> {
        match registry.operator(spec)? {
            None => {
                let SourceSpec::File(path) = spec else {
                    anyhow::bail!("source {} has no operator and is not a file", spec.uri());
                };
                Self::open_local(path)
            }
            Some(op) => {
                let store = Arc::new(AsyncOpendalStore::new(op));
                let (metadata, attributes) = read_metadata_async(&store).await?;
                Ok(Self {
                    backend: StoreBackend::Async {
                        store,
                        arrays: Mutex::new(HashMap::new()),
                    },
                    metadata,
                    attributes,
                })
            }
        }
    }

    /// Open a zarr store from a local filesystem path.
    pub fn open_local(path: &std::path::Path) -> Result<Self> {
        let store =
            Arc::new(FilesystemStore::new(path).context("Failed to open filesystem store")?);
        let (metadata, attributes) = read_metadata_local(&store)?;
        Ok(Self {
            backend: StoreBackend::Local {
                store,
                arrays: Mutex::new(HashMap::new()),
            },
            metadata,
            attributes,
        })
    }

    /// Return a reference to the parsed dataset metadata.
    pub fn metadata(&self) -> &DatasetInfo {
        &self.metadata
    }

    /// The zarr group attributes this store was opened with.
    pub fn attributes(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.attributes
    }

    /// Read a rectangular tile and encode it for the wire.
    pub async fn read_tile_bytes(&self, request: &TileRequest) -> Result<TileBytes> {
        let dtype = self.level_dtype(request.level)?;
        let (raw, planes) = self.read_raw(request).await?;

        match (request.projection, request.encoding) {
            (None, TileEncoding::Raw) => Ok(TileBytes { bytes: raw, dtype }),
            (None, TileEncoding::F32) => {
                let pixels = bytes_to_f32(&raw, &dtype)?;
                Ok(TileBytes {
                    bytes: f32_bytes(&pixels),
                    dtype: "float32".to_string(),
                })
            }
            (Some(projection), _) => {
                // A projection is always f32 on the wire: the maximum of a set
                // of label ids is not a label id, so `raw` has no meaning here
                // and is not honoured silently.
                let pixels = bytes_to_f32(&raw, &dtype)?;
                let plane = (request.h * request.w) as usize;
                let reduced = project(&pixels, plane, planes.max(1), projection);
                Ok(TileBytes {
                    bytes: f32_bytes(&reduced),
                    dtype: "float32".to_string(),
                })
            }
        }
    }

    /// Read a whole plane across one axis.
    ///
    /// The `(y, x)` case is an ordinary full-size tile; the other two are the
    /// orthogonal panes, and they are the reason this exists — a `(z, x)` plane
    /// crosses every chunk row of the store, so it is worth reading once at a
    /// level that fits the pane rather than tiling it.
    pub async fn read_plane(&self, request: &PlaneRequest) -> Result<PlaneBytes> {
        let dtype = self.level_dtype(request.level)?;
        let (height, width) = self.plane_shape(request.level, request.axis)?;
        let tile = TileRequest {
            level: request.level,
            t: request.t,
            c: request.c,
            z: 0,
            y: 0,
            x: 0,
            h: 0,
            w: 0,
            encoding: request.encoding,
            projection: None,
            depth: 1,
        };
        let subset = self.plane_subset(&tile, request)?;
        let raw = self.read_subset(request.level, &subset).await?;

        let bytes = match request.encoding {
            TileEncoding::Raw => raw,
            TileEncoding::F32 => f32_bytes(&bytes_to_f32(&raw, &dtype)?),
        };
        Ok(PlaneBytes {
            bytes,
            dtype: match request.encoding {
                TileEncoding::Raw => dtype,
                TileEncoding::F32 => "float32".to_string(),
            },
            height,
            width,
        })
    }

    /// `(rows, columns)` of a plane across `axis` at `level`.
    pub fn plane_shape(&self, level: usize, axis: PlaneAxis) -> Result<(u64, u64)> {
        let z = self.axis_extent(level, "z")?;
        let y = self.axis_extent(level, "y")?;
        let x = self.axis_extent(level, "x")?;
        Ok(match axis {
            PlaneAxis::Z => (y, x),
            PlaneAxis::Y => (z, x),
            PlaneAxis::X => (z, y),
        })
    }

    /// The length of a named axis at one level, or 1 when the axis is absent.
    pub fn axis_extent(&self, level: usize, name: &str) -> Result<u64> {
        let array = self
            .metadata
            .arrays
            .get(level)
            .with_context(|| format!("level {level} is outside this dataset"))?;
        let axes = &self.metadata.metadata.multiscales[0].axes;
        Ok(axes
            .iter()
            .position(|axis| axis.name == name)
            .and_then(|i| array.shape.get(i).copied())
            .unwrap_or(1))
    }

    /// The dtype of one level's array.
    pub fn level_dtype(&self, level: usize) -> Result<String> {
        Ok(self
            .metadata
            .arrays
            .get(level)
            .with_context(|| format!("level {level} is outside this dataset"))?
            .dtype
            .clone())
    }

    /// The array's own bytes for a tile region, and how many z planes they hold.
    async fn read_raw(&self, request: &TileRequest) -> Result<(Vec<u8>, u64)> {
        let axes = self.metadata.metadata.multiscales[0].axes.clone();
        let z_extent = self.axis_extent(request.level, "z")?;
        let z_range = request.z_range();
        // A projection near the top of the stack asks for planes that are not
        // there; it gets the ones that are rather than an error, because the
        // slab is a viewing choice and the volume's end is not a mistake.
        let z_start = z_range.start.min(z_extent.saturating_sub(1));
        let z_end = z_range.end.min(z_extent).max(z_start + 1);
        let planes = z_end - z_start;

        let mut ranges = Vec::with_capacity(axes.len());
        for axis in &axes {
            match axis.name.as_str() {
                "t" => ranges.push(request.t..request.t + 1),
                "c" => ranges.push(request.c..request.c + 1),
                "z" => ranges.push(z_start..z_end),
                "y" => ranges.push(request.y..request.y + request.h),
                "x" => ranges.push(request.x..request.x + request.w),
                _ => ranges.push(0..1),
            }
        }
        let subset = ArraySubset::new_with_ranges(&ranges);
        let bytes = self.read_subset(request.level, &subset).await?;
        Ok((bytes, planes))
    }

    /// The subset one plane request covers.
    fn plane_subset(&self, tile: &TileRequest, request: &PlaneRequest) -> Result<ArraySubset> {
        let axes = &self.metadata.metadata.multiscales[0].axes;
        let z = self.axis_extent(request.level, "z")?;
        let y = self.axis_extent(request.level, "y")?;
        let x = self.axis_extent(request.level, "x")?;
        let mut ranges = Vec::with_capacity(axes.len());
        for axis in axes {
            let range = match (axis.name.as_str(), request.axis) {
                ("t", _) => tile.t..tile.t + 1,
                ("c", _) => tile.c..tile.c + 1,
                ("z", PlaneAxis::Z) => {
                    let index = request.index.min(z.saturating_sub(1));
                    index..index + 1
                }
                ("z", _) => 0..z,
                ("y", PlaneAxis::Y) => {
                    let index = request.index.min(y.saturating_sub(1));
                    index..index + 1
                }
                ("y", _) => 0..y,
                ("x", PlaneAxis::X) => {
                    let index = request.index.min(x.saturating_sub(1));
                    index..index + 1
                }
                ("x", _) => 0..x,
                _ => 0..1,
            };
            ranges.push(range);
        }
        Ok(ArraySubset::new_with_ranges(&ranges))
    }

    /// Read one subset of a level, through whichever backend this store has.
    async fn read_subset(&self, level: usize, subset: &ArraySubset) -> Result<Vec<u8>> {
        let dataset_path = self.metadata.metadata.multiscales[0]
            .datasets
            .get(level)
            .with_context(|| format!("level {level} is outside this dataset"))?
            .path
            .clone();
        let array_path = format!("/{}", dataset_path);

        match &self.backend {
            StoreBackend::Local { store, arrays } => {
                let cached = arrays.lock().unwrap().get(&level).cloned();
                let array = match cached {
                    Some(array) => array,
                    None => {
                        let array = Arc::new(
                            Array::open(store.clone(), &array_path)
                                .with_context(|| format!("Failed to open array at {array_path}"))?,
                        );
                        arrays.lock().unwrap().insert(level, array.clone());
                        array
                    }
                };
                let bytes = array
                    .retrieve_array_subset(subset)
                    .context("Failed to retrieve array subset")?;
                bytes
                    .into_fixed()
                    .map_err(|e| anyhow::anyhow!("{:?}", e))
                    .map(|b| b.to_vec())
            }
            StoreBackend::Async { store, arrays } => {
                let cached = arrays.lock().unwrap().get(&level).cloned();
                let array = match cached {
                    Some(array) => array,
                    None => {
                        let array = Arc::new(
                            Array::async_open(store.clone(), &array_path)
                                .await
                                .with_context(|| format!("Failed to open array at {array_path}"))?,
                        );
                        arrays.lock().unwrap().insert(level, array.clone());
                        array
                    }
                };
                let bytes = array
                    .async_retrieve_array_subset(subset)
                    .await
                    .context("Failed to retrieve array subset")?;
                bytes
                    .into_fixed()
                    .map_err(|e| anyhow::anyhow!("{:?}", e))
                    .map(|b| b.to_vec())
            }
        }
    }
}

/// Reduce `planes` consecutive planes of `plane` pixels each.
fn project(pixels: &[f32], plane: usize, planes: u64, projection: Projection) -> Vec<f32> {
    let planes = planes as usize;
    if plane == 0 || planes <= 1 {
        return pixels.to_vec();
    }
    let mut out = vec![
        match projection {
            Projection::Max => f32::NEG_INFINITY,
            Projection::Mean => 0.0,
        };
        plane
    ];
    for index in 0..planes {
        let at = index * plane;
        let Some(slice) = pixels.get(at..at + plane) else {
            break;
        };
        for (accumulator, value) in out.iter_mut().zip(slice) {
            match projection {
                Projection::Max => *accumulator = accumulator.max(*value),
                Projection::Mean => *accumulator += *value,
            }
        }
    }
    if projection == Projection::Mean {
        for value in out.iter_mut() {
            *value /= planes as f32;
        }
    }
    out
}

/// Little-endian `f32` bytes, the wire form of every intensity answer.
fn f32_bytes(pixels: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for value in pixels {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// List datasets (top-level directories) in an S3 bucket under the given prefix.
pub async fn list_s3_datasets(config: &S3Config) -> Result<Vec<String>> {
    log::info!(
        "Listing S3 datasets: bucket={}, endpoint={}, prefix={}",
        config.bucket,
        config.endpoint,
        config.prefix
    );

    let profile = crate::source::S3Profile {
        endpoint: config.endpoint.clone(),
        region: config.region.clone(),
        access_key: config.access_key.clone(),
        secret_key: config.secret_key.clone(),
    };
    let op = profile.operator(&config.bucket, "")?;

    let prefix = if config.prefix.is_empty() {
        "/".to_string()
    } else {
        let p = config.prefix.trim_start_matches('/');
        if p.ends_with('/') {
            p.to_string()
        } else {
            format!("{}/", p)
        }
    };

    log::info!("Listing prefix: '{}'", prefix);
    let entries = op
        .list(&prefix)
        .await
        .with_context(|| format!("Failed to list S3 bucket at prefix '{prefix}'"))?;

    let datasets: Vec<String> = entries
        .into_iter()
        .filter(|e| e.metadata().is_dir())
        .map(|e| e.name().trim_end_matches('/').to_string())
        .filter(|name| !name.is_empty())
        .collect();

    log::info!("Found {} datasets", datasets.len());
    Ok(datasets)
}

type Attributes = serde_json::Map<String, serde_json::Value>;

/// Read OME-Zarr metadata from a local filesystem store.
fn read_metadata_local(store: &Arc<FilesystemStore>) -> Result<(DatasetInfo, Attributes)> {
    let group = Group::open(store.clone(), "/").context("Failed to open zarr group")?;
    let attrs = group.attributes().clone();
    let multiscales = parse_multiscales(&attrs)?;
    let omero: Option<OmeroMetadata> = attrs
        .get("omero")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let mut arrays = Vec::new();
    for dataset in &multiscales[0].datasets {
        let array_path = format!("/{}", dataset.path);
        let array = Array::open(store.clone(), &array_path)
            .with_context(|| format!("Failed to open array at {array_path}"))?;
        arrays.push(array_info(array.shape(), &array));
    }

    Ok((
        DatasetInfo {
            metadata: DatasetMetadata { multiscales, omero },
            arrays,
        },
        attrs,
    ))
}

/// Read OME-Zarr metadata from an opendal-backed store.
async fn read_metadata_async(store: &Arc<AsyncOpendalStore>) -> Result<(DatasetInfo, Attributes)> {
    let group = Group::async_open(store.clone(), "/")
        .await
        .context("Failed to open zarr group")?;
    let attrs = group.attributes().clone();

    let multiscales = parse_multiscales(&attrs)?;
    let omero: Option<OmeroMetadata> = attrs
        .get("omero")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let mut arrays = Vec::new();
    for dataset in &multiscales[0].datasets {
        let array_path = format!("/{}", dataset.path);
        let array = Array::async_open(store.clone(), &array_path)
            .await
            .with_context(|| format!("Failed to open array at {array_path}"))?;
        arrays.push(array_info(array.shape(), &array));
    }

    Ok((
        DatasetInfo {
            metadata: DatasetMetadata { multiscales, omero },
            arrays,
        },
        attrs,
    ))
}

/// Summarise one opened array for the API.
fn array_info<T: ?Sized>(shape: &[u64], array: &Array<T>) -> ArrayInfo {
    ArrayInfo {
        shape: shape.to_vec(),
        chunks: array
            .chunk_grid_shape()
            .map(|s| s.to_vec())
            .unwrap_or_default(),
        dtype: format!("{}", array.data_type()),
        order: None,
        compressor: None,
    }
}

/// Parse the multiscales array from zarr group attributes.
fn parse_multiscales(attrs: &Attributes) -> Result<Vec<Multiscale>> {
    if let Some(ms) = attrs.get("multiscales") {
        serde_json::from_value(ms.clone()).context("Failed to parse multiscales")
    } else if let Some(ome) = attrs.get("ome") {
        let ms = ome
            .get("multiscales")
            .context("No multiscales in ome attributes")?;
        serde_json::from_value(ms.clone()).context("Failed to parse ome.multiscales")
    } else {
        anyhow::bail!("No multiscales metadata found in zarr attributes");
    }
}

/// Convert raw bytes to f32 based on the array dtype.
pub fn bytes_to_f32(raw: &[u8], dtype: &str) -> Result<Vec<f32>> {
    match dtype {
        "uint8" => Ok(raw.iter().map(|&b| b as f32).collect()),
        "int8" => Ok(raw.iter().map(|&b| b as i8 as f32).collect()),
        "uint16" => Ok(raw
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]) as f32)
            .collect()),
        "int16" => Ok(raw
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as f32)
            .collect()),
        "uint32" => Ok(raw
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32)
            .collect()),
        "int32" => Ok(raw
            .chunks_exact(4)
            .map(|chunk| i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32)
            .collect()),
        "uint64" => Ok(raw
            .chunks_exact(8)
            .map(|chunk| {
                u64::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ]) as f32
            })
            .collect()),
        "float32" => Ok(raw
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()),
        "float64" => Ok(raw
            .chunks_exact(8)
            .map(|chunk| {
                f64::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ]) as f32
            })
            .collect()),
        _ => anyhow::bail!("Unsupported dtype: {}", dtype),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_dtype_converts() {
        assert_eq!(bytes_to_f32(&[7], "uint8").unwrap(), vec![7.0]);
        assert_eq!(bytes_to_f32(&[0xff], "int8").unwrap(), vec![-1.0]);
        assert_eq!(
            bytes_to_f32(&1234u16.to_le_bytes(), "uint16").unwrap(),
            vec![1234.0]
        );
        assert_eq!(
            bytes_to_f32(&(-5i16).to_le_bytes(), "int16").unwrap(),
            vec![-5.0]
        );
        assert_eq!(
            bytes_to_f32(&70000u32.to_le_bytes(), "uint32").unwrap(),
            vec![70000.0]
        );
        assert_eq!(
            bytes_to_f32(&(-9i32).to_le_bytes(), "int32").unwrap(),
            vec![-9.0]
        );
        assert_eq!(
            bytes_to_f32(&5u64.to_le_bytes(), "uint64").unwrap(),
            vec![5.0]
        );
        assert_eq!(
            bytes_to_f32(&1.5f32.to_le_bytes(), "float32").unwrap(),
            vec![1.5]
        );
        assert_eq!(
            bytes_to_f32(&2.5f64.to_le_bytes(), "float64").unwrap(),
            vec![2.5]
        );
        assert!(bytes_to_f32(&[0], "complex64").is_err());
    }

    /// The reason [`TileEncoding::Raw`] exists, stated as a test: an id past
    /// f32's exact range comes back as a *different* id through the f32 path.
    #[test]
    fn wide_ids_do_not_survive_f32() {
        let id: u32 = (1 << 24) + 1;
        let through_f32 = bytes_to_f32(&id.to_le_bytes(), "uint32").unwrap()[0] as u32;
        assert_ne!(through_f32, id);
    }
}
