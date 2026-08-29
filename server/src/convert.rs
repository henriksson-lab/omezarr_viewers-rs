//! `.npy` to OME-Zarr, with a pyramid.
//!
//! A `.npy` is fine on local disk and bad over object storage: it has no chunk
//! grid, so reading one tile means reading (or ranging over) a flat buffer with
//! no index, and it has no pyramid, so a zoomed-out view reads every pixel to
//! show a thousandth of them. Both are properties of the format, not of the
//! reader — which is why the answer is a conversion rather than a cleverer
//! `NpyVolume`.
//!
//! What comes out is a `(c, z, y, x)` OME-Zarr with `multiscales` metadata and
//! one channel, downsampled by two in y and x per level until the coarsest
//! level fits in a screen. Levels are **mean**-reduced for intensity data and
//! **nearest**-sampled for labels, because averaging two ids invents a third.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::Arc;
use zarrs::array::{ArrayBuilder, DataType, FillValue};
use zarrs::array_subset::ArraySubset;
use zarrs::filesystem::FilesystemStore;
use zarrs::group::GroupBuilder;

use crate::npy_volume::NpyVolume;
use crate::zarr_reader::{PlaneAxis, PlaneRequest, TileEncoding};

/// How a level is derived from the one above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    /// Average the 2x2 block — intensity.
    Mean,
    /// Take one of the four — labels, where an average is not a value.
    Nearest,
}

impl Reduce {
    /// The right reduction for a dtype, unless the caller says otherwise:
    /// integers wide enough to be ids are sampled, everything else averaged.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value {
            Some("mean") => Some(Reduce::Mean),
            Some("nearest") | Some("labels") => Some(Reduce::Nearest),
            _ => None,
        }
    }
}

/// Convert `input` (a `.npy`) into an OME-Zarr store at `output`.
///
/// Returns the shape of each level written.
pub fn npy_to_zarr(
    input: &Path,
    output: &Path,
    reduce: Option<Reduce>,
    max_levels: usize,
    chunk: u64,
) -> Result<Vec<[u64; 4]>> {
    let volume = NpyVolume::open_local(input)?;
    let dtype = volume.level_dtype(0)?;
    let reduce = reduce.unwrap_or(match dtype.as_str() {
        "uint32" | "uint64" | "int32" | "int64" => Reduce::Nearest,
        _ => Reduce::Mean,
    });

    let depth = volume.axis_extent(0, "z")?;
    let height = volume.axis_extent(0, "y")?;
    let width = volume.axis_extent(0, "x")?;
    if depth == 0 || height == 0 || width == 0 {
        bail!("{} holds no voxels", input.display());
    }

    if output.exists() {
        bail!("{} already exists", output.display());
    }
    std::fs::create_dir_all(output)
        .with_context(|| format!("creating {}", output.display()))?;
    let store = Arc::new(
        FilesystemStore::new(output)
            .with_context(|| format!("opening {} as a store", output.display()))?,
    );

    // Level 0 is read plane by plane out of the `.npy`, so a volume larger than
    // memory converts without ever being resident.
    let mut planes: Vec<Vec<f64>> = Vec::with_capacity(depth as usize);
    for z in 0..depth {
        let plane = volume.read_plane(&PlaneRequest {
            level: 0,
            t: 0,
            c: 0,
            axis: PlaneAxis::Z,
            index: z,
            encoding: TileEncoding::F32,
        })?;
        planes.push(
            plane
                .bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
                .collect(),
        );
    }

    let data_type = data_type_of(&dtype)?;
    let mut shapes = Vec::new();
    let mut level_shape = [1u64, depth, height, width];
    let mut level_planes = planes;
    let mut datasets = Vec::new();
    let mut step = 1u64;

    for level in 0..max_levels {
        write_level(
            &store,
            &level.to_string(),
            data_type.clone(),
            &dtype,
            level_shape,
            &level_planes,
            chunk,
        )?;
        datasets.push(serde_json::json!({
            "path": level.to_string(),
            "coordinateTransformations": [
                {"type": "scale", "scale": [1.0, 1.0, step as f64, step as f64]}
            ],
        }));
        shapes.push(level_shape);

        let next = [
            1,
            level_shape[1],
            level_shape[2].div_ceil(2),
            level_shape[3].div_ceil(2),
        ];
        // Stop when halving would gain nothing, or when the level already fits
        // a screen: the point of the pyramid is the zoomed-out view.
        if next[2] == level_shape[2] && next[3] == level_shape[3] {
            break;
        }
        if level_shape[2].max(level_shape[3]) <= 512 {
            break;
        }
        level_planes = level_planes
            .iter()
            .map(|plane| downsample(plane, level_shape[2], level_shape[3], reduce))
            .collect();
        level_shape = next;
        step *= 2;
    }

    let attributes = serde_json::json!({
        "multiscales": [{
            "name": input.file_stem().map(|s| s.to_string_lossy().into_owned()),
            "axes": [
                {"name": "c", "type": "channel"},
                {"name": "z", "type": "space"},
                {"name": "y", "type": "space"},
                {"name": "x", "type": "space"},
            ],
            "datasets": datasets,
        }],
    });
    let group = GroupBuilder::new()
        .attributes(attributes.as_object().unwrap().clone())
        .build(store.clone(), "/")
        .context("building the group")?;
    group.store_metadata().context("writing group metadata")?;

    Ok(shapes)
}

/// Halve a plane in y and x.
fn downsample(plane: &[f64], height: u64, width: u64, reduce: Reduce) -> Vec<f64> {
    let (h, w) = (height as usize, width as usize);
    let (nh, nw) = (h.div_ceil(2), w.div_ceil(2));
    let mut out = Vec::with_capacity(nh * nw);
    for row in 0..nh {
        for column in 0..nw {
            let (y0, x0) = (row * 2, column * 2);
            let mut values = Vec::with_capacity(4);
            for y in y0..(y0 + 2).min(h) {
                for x in x0..(x0 + 2).min(w) {
                    values.push(plane[y * w + x]);
                }
            }
            out.push(match reduce {
                Reduce::Mean => values.iter().sum::<f64>() / values.len().max(1) as f64,
                // The first of the block, always — a fixed rule, so two runs of
                // the converter produce the same ids.
                Reduce::Nearest => values.first().copied().unwrap_or(0.0),
            });
        }
    }
    out
}

fn write_level(
    store: &Arc<FilesystemStore>,
    path: &str,
    data_type: DataType,
    dtype: &str,
    shape: [u64; 4],
    planes: &[Vec<f64>],
    chunk: u64,
) -> Result<()> {
    let chunk_shape = vec![1, 1, shape[2].min(chunk).max(1), shape[3].min(chunk).max(1)];
    let array = ArrayBuilder::new(
        shape.to_vec(),
        data_type,
        chunk_shape.try_into().map_err(|e| anyhow::anyhow!("{e:?}"))?,
        fill_value_of(dtype)?,
    )
    .build(store.clone(), &format!("/{path}"))
    .context("building the array")?;
    array.store_metadata().context("writing array metadata")?;

    for (z, plane) in planes.iter().enumerate() {
        let subset = ArraySubset::new_with_ranges(&[
            0..1,
            z as u64..z as u64 + 1,
            0..shape[2],
            0..shape[3],
        ]);
        store_plane(&array, &subset, dtype, plane)?;
    }
    Ok(())
}

/// Write one plane in the array's own dtype.
fn store_plane(
    array: &zarrs::array::Array<FilesystemStore>,
    subset: &ArraySubset,
    dtype: &str,
    plane: &[f64],
) -> Result<()> {
    macro_rules! store_as {
        ($ty:ty) => {{
            let values: Vec<$ty> = plane.iter().map(|&v| v as $ty).collect();
            array
                .store_array_subset_elements(subset, &values)
                .context("writing a plane")?;
        }};
    }
    match dtype {
        "uint8" => store_as!(u8),
        "uint16" => store_as!(u16),
        "uint32" => store_as!(u32),
        "uint64" => store_as!(u64),
        "int8" => store_as!(i8),
        "int16" => store_as!(i16),
        "int32" => store_as!(i32),
        "int64" => store_as!(i64),
        "float32" => store_as!(f32),
        "float64" => store_as!(f64),
        other => bail!("no writer for dtype {other}"),
    }
    Ok(())
}

fn data_type_of(dtype: &str) -> Result<DataType> {
    Ok(match dtype {
        "uint8" => DataType::UInt8,
        "uint16" => DataType::UInt16,
        "uint32" => DataType::UInt32,
        "uint64" => DataType::UInt64,
        "int8" => DataType::Int8,
        "int16" => DataType::Int16,
        "int32" => DataType::Int32,
        "int64" => DataType::Int64,
        "float32" => DataType::Float32,
        "float64" => DataType::Float64,
        other => bail!("no zarr data type for {other}"),
    })
}

fn fill_value_of(dtype: &str) -> Result<FillValue> {
    Ok(match dtype {
        "uint8" => FillValue::from(0u8),
        "uint16" => FillValue::from(0u16),
        "uint32" => FillValue::from(0u32),
        "uint64" => FillValue::from(0u64),
        "int8" => FillValue::from(0i8),
        "int16" => FillValue::from(0i16),
        "int32" => FillValue::from(0i32),
        "int64" => FillValue::from(0i64),
        "float32" => FillValue::from(0.0f32),
        "float64" => FillValue::from(0.0f64),
        other => bail!("no fill value for {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zarr_reader::{TileRequest, ZarrStore};

    /// Write a `(z, y, x)` `uint16` `.npy` whose values are known.
    fn write_npy(path: &Path, shape: (u64, u64, u64), value: impl Fn(u64, u64, u64) -> u16) {
        let mut data = Vec::new();
        for z in 0..shape.0 {
            for y in 0..shape.1 {
                for x in 0..shape.2 {
                    data.extend_from_slice(&value(z, y, x).to_le_bytes());
                }
            }
        }
        let dict = format!(
            "{{'descr': '<u2', 'fortran_order': False, 'shape': ({}, {}, {}), }}",
            shape.0, shape.1, shape.2
        );
        let mut header = dict.into_bytes();
        while !(10 + header.len() + 1).is_multiple_of(64) {
            header.push(b' ');
        }
        header.push(b'\n');
        let mut out = Vec::new();
        out.extend_from_slice(b"\x93NUMPY\x01\x00");
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&data);
        std::fs::write(path, out).expect("write npy");
    }

    #[actix_web::test]
    async fn a_converted_volume_reads_back_the_same_voxels() {
        let dir = tempfile::tempdir().expect("temp dir");
        let npy = dir.path().join("mask.npy");
        let value = |z: u64, y: u64, x: u64| (x + 100 * y + 10_000 * z) as u16;
        write_npy(&npy, (2, 40, 60), value);

        let zarr = dir.path().join("mask.zarr");
        let shapes = npy_to_zarr(&npy, &zarr, None, 4, 16).expect("convert");
        assert_eq!(shapes[0], [1, 2, 40, 60]);

        let store = ZarrStore::open_local(&zarr).expect("open converted");
        let tile = store
            .read_tile_bytes(&TileRequest::new(0, 3, 4, 2, 3).at(0, 0, 1))
            .await
            .expect("tile");
        let pixels: Vec<f32> = tile
            .bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        for row in 0..2u64 {
            for column in 0..3u64 {
                assert_eq!(
                    pixels[(row * 3 + column) as usize],
                    value(1, 3 + row, 4 + column) as f32,
                    "at ({row}, {column})"
                );
            }
        }
    }

    #[actix_web::test]
    async fn a_pyramid_is_written_and_halves_each_level() {
        let dir = tempfile::tempdir().expect("temp dir");
        let npy = dir.path().join("big.npy");
        write_npy(&npy, (1, 1200, 1200), |_, y, x| (y + x) as u16);
        let zarr = dir.path().join("big.zarr");
        let shapes = npy_to_zarr(&npy, &zarr, None, 8, 64).expect("convert");
        assert!(shapes.len() >= 2, "a big volume gets a pyramid: {shapes:?}");
        assert_eq!(shapes[1], [1, 1, 600, 600]);
        assert!(
            shapes.last().unwrap()[2] <= 512,
            "the coarsest level fits a screen: {shapes:?}"
        );
    }

    #[test]
    fn labels_are_sampled_rather_than_averaged() {
        // Two ids in a 2x2 block: an average would invent a third.
        let plane = vec![10.0, 20.0, 30.0, 40.0];
        assert_eq!(downsample(&plane, 2, 2, Reduce::Nearest), vec![10.0]);
        assert_eq!(downsample(&plane, 2, 2, Reduce::Mean), vec![25.0]);
    }

    #[test]
    fn a_wide_integer_dtype_defaults_to_sampling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let npy = dir.path().join("ids.npy");
        // uint32 is the label dtype; the converter must not average it.
        let mut data = Vec::new();
        for value in [1u32, 2, 3, 4] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let dict = "{'descr': '<u4', 'fortran_order': False, 'shape': (1, 2, 2), }";
        let mut header = dict.as_bytes().to_vec();
        while !(10 + header.len() + 1).is_multiple_of(64) {
            header.push(b' ');
        }
        header.push(b'\n');
        let mut out = Vec::new();
        out.extend_from_slice(b"\x93NUMPY\x01\x00");
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&data);
        std::fs::write(&npy, out).unwrap();

        let zarr = dir.path().join("ids.zarr");
        npy_to_zarr(&npy, &zarr, None, 4, 8).expect("convert");
        // One level only: 2x2 already fits, and nothing was averaged.
        let store = ZarrStore::open_local(&zarr).expect("open");
        assert_eq!(store.metadata().arrays.len(), 1);
        assert_eq!(store.metadata().arrays[0].dtype, "uint32");
    }

    #[test]
    fn converting_onto_an_existing_directory_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let npy = dir.path().join("x.npy");
        write_npy(&npy, (1, 2, 2), |_, _, _| 1);
        let out = dir.path().join("out.zarr");
        std::fs::create_dir_all(&out).unwrap();
        assert!(npy_to_zarr(&npy, &out, None, 2, 8).is_err());
    }
}
