use anyhow::{Context, Result};
use omezarr_viewer_common::{
    ArrayInfo, Axis, DatasetInfo, DatasetMetadata, Multiscale, OmeroMetadata,
};
use std::path::Path;
use std::sync::Arc;
use zarrs::array::Array;
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::Group;
use zarrs_opendal::AsyncOpendalStore;

/// Abstraction over local filesystem and HTTP zarr stores.
/// For local stores, uses sync APIs. For HTTP stores, uses async APIs via opendal.
enum StoreBackend {
    Local(Arc<FilesystemStore>),
    Http(Arc<AsyncOpendalStore>),
}

/// Opened OME-Zarr store with parsed metadata and array handles.
pub struct ZarrStore {
    backend: StoreBackend,
    metadata: DatasetInfo,
}

impl ZarrStore {
    /// Open a zarr store from a local filesystem path.
    pub fn open_local(path: &Path) -> Result<Self> {
        let store = Arc::new(
            FilesystemStore::new(path).context("Failed to open filesystem store")?,
        );
        let metadata = read_metadata_local(&store)?;
        Ok(Self {
            backend: StoreBackend::Local(store),
            metadata,
        })
    }

    /// Open a zarr store from an HTTP URL.
    pub async fn open_http(url: &str) -> Result<Self> {
        let http_op = opendal::Operator::new(
            opendal::services::Http::default().endpoint(url),
        )?
        .finish();
        let store = Arc::new(AsyncOpendalStore::new(http_op));
        let metadata = read_metadata_http(&store).await?;
        Ok(Self {
            backend: StoreBackend::Http(store),
            metadata,
        })
    }

    /// Open a zarr store, auto-detecting local path vs HTTP URL.
    pub async fn open(source: &str) -> Result<Self> {
        if source.starts_with("http://") || source.starts_with("https://") {
            Self::open_http(source).await
        } else {
            Self::open_local(Path::new(source))
        }
    }

    /// Return a reference to the parsed dataset metadata.
    pub fn metadata(&self) -> &DatasetInfo {
        &self.metadata
    }

    /// Read a rectangular tile region from a specific resolution level and channel.
    /// Returns pixel data normalized to f32.
    pub async fn read_tile(
        &self,
        level: usize,
        t: u64,
        c: u64,
        z: u64,
        y: u64,
        x: u64,
        h: u64,
        w: u64,
    ) -> Result<Vec<f32>> {
        let dataset_path = &self.metadata.metadata.multiscales[0].datasets[level].path;
        let array_path = format!("/{}", dataset_path);
        let axes = &self.metadata.metadata.multiscales[0].axes;
        let dtype = &self.metadata.arrays[level].dtype;

        match &self.backend {
            StoreBackend::Local(store) => {
                let array = Array::open(store.clone(), &array_path)
                    .context(format!("Failed to open array at {}", array_path))?;
                let subset = build_subset(axes, array.shape().len(), t, c, z, y, x, h, w)?;
                let bytes = array
                    .retrieve_array_subset(&subset)
                    .context("Failed to retrieve array subset")?;
                let raw = bytes.into_fixed().map_err(|e| anyhow::anyhow!("{:?}", e))?;
                bytes_to_f32(&raw, dtype)
            }
            StoreBackend::Http(store) => {
                let array = Array::async_open(store.clone(), &array_path)
                    .await
                    .context(format!("Failed to open array at {}", array_path))?;
                let subset = build_subset(axes, array.shape().len(), t, c, z, y, x, h, w)?;
                let bytes = array
                    .async_retrieve_array_subset(&subset)
                    .await
                    .context("Failed to retrieve array subset")?;
                let raw = bytes.into_fixed().map_err(|e| anyhow::anyhow!("{:?}", e))?;
                bytes_to_f32(&raw, dtype)
            }
        }
    }
}

/// Build an ArraySubset from axis names and tile coordinates.
fn build_subset(
    axes: &[Axis],
    _ndim: usize,
    t: u64,
    c: u64,
    z: u64,
    y: u64,
    x: u64,
    h: u64,
    w: u64,
) -> Result<ArraySubset> {
    let mut ranges = Vec::with_capacity(axes.len());
    for axis in axes {
        match axis.name.as_str() {
            "t" => ranges.push(t..t + 1),
            "c" => ranges.push(c..c + 1),
            "z" => ranges.push(z..z + 1),
            "y" => ranges.push(y..y + h),
            "x" => ranges.push(x..x + w),
            _ => ranges.push(0..1),
        }
    }
    Ok(ArraySubset::new_with_ranges(&ranges))
}

/// Read OME-Zarr metadata from a local filesystem store.
fn read_metadata_local(store: &Arc<FilesystemStore>) -> Result<DatasetInfo> {
    let group = Group::open(store.clone(), "/").context("Failed to open zarr group")?;
    let attrs = group.attributes();
    parse_metadata(attrs, |dataset_path| {
        let array_path = format!("/{}", dataset_path);
        let array = Array::open(store.clone(), &array_path)
            .context(format!("Failed to open array at {}", array_path))?;
        Ok(ArrayInfo {
            shape: array.shape().to_vec(),
            chunks: array.chunk_grid_shape().map(|s| s.to_vec()).unwrap_or_default(),
            dtype: format!("{}", array.data_type()),
            order: None,
            compressor: None,
        })
    })
}

/// Read OME-Zarr metadata from an HTTP-backed store.
async fn read_metadata_http(store: &Arc<AsyncOpendalStore>) -> Result<DatasetInfo> {
    let group = Group::async_open(store.clone(), "/")
        .await
        .context("Failed to open zarr group")?;
    let attrs = group.attributes();

    let multiscales = parse_multiscales(attrs)?;
    let omero: Option<OmeroMetadata> = attrs
        .get("omero")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let mut arrays = Vec::new();
    for dataset in &multiscales[0].datasets {
        let array_path = format!("/{}", dataset.path);
        let array = Array::async_open(store.clone(), &array_path)
            .await
            .context(format!("Failed to open array at {}", array_path))?;
        arrays.push(ArrayInfo {
            shape: array.shape().to_vec(),
            chunks: array.chunk_grid_shape().map(|s| s.to_vec()).unwrap_or_default(),
            dtype: format!("{}", array.data_type()),
            order: None,
            compressor: None,
        });
    }

    Ok(DatasetInfo {
        metadata: DatasetMetadata { multiscales, omero },
        arrays,
    })
}

/// Parse the multiscales array from zarr group attributes.
fn parse_multiscales(attrs: &serde_json::Map<String, serde_json::Value>) -> Result<Vec<Multiscale>> {
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

/// Parse dataset metadata and open arrays for each resolution level.
fn parse_metadata<F>(
    attrs: &serde_json::Map<String, serde_json::Value>,
    open_array: F,
) -> Result<DatasetInfo>
where
    F: Fn(&str) -> Result<ArrayInfo>,
{
    let multiscales = parse_multiscales(attrs)?;
    let omero: Option<OmeroMetadata> = attrs
        .get("omero")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    let mut arrays = Vec::new();
    for dataset in &multiscales[0].datasets {
        arrays.push(open_array(&dataset.path)?);
    }

    Ok(DatasetInfo {
        metadata: DatasetMetadata { multiscales, omero },
        arrays,
    })
}

/// Convert raw bytes to f32 based on the array dtype.
fn bytes_to_f32(raw: &[u8], dtype: &str) -> Result<Vec<f32>> {
    match dtype {
        "uint8" => Ok(raw.iter().map(|&b| b as f32).collect()),
        "uint16" => {
            let pixels: Vec<f32> = raw
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]) as f32)
                .collect();
            Ok(pixels)
        }
        "int16" => {
            let pixels: Vec<f32> = raw
                .chunks_exact(2)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as f32)
                .collect();
            Ok(pixels)
        }
        "uint32" => {
            let pixels: Vec<f32> = raw
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32)
                .collect();
            Ok(pixels)
        }
        "float32" => {
            let pixels: Vec<f32> = raw
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();
            Ok(pixels)
        }
        "float64" => {
            let pixels: Vec<f32> = raw
                .chunks_exact(8)
                .map(|chunk| {
                    let v = f64::from_le_bytes([
                        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6],
                        chunk[7],
                    ]);
                    v as f32
                })
                .collect();
            Ok(pixels)
        }
        _ => anyhow::bail!("Unsupported dtype: {}", dtype),
    }
}
