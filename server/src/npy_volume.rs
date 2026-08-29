//! A `.npy` array as a volume layer.
//!
//! This is what `clearmap-ng` writes: `Workspace` puts every mask, skeleton and
//! density on disk as a C-ordered `.npy`, and that is the only volume form the
//! pipeline produces today (PLAN.md §3). A `.npy` has no chunk grid and no
//! pyramid, so this reader is deliberately plain — it slices a flat buffer —
//! and the answer to "why is this slow over S3" is the converter in phase 4,
//! not a cleverer reader.
//!
//! Local files are memory-mapped: a tile is then a strided copy out of the page
//! cache, and the kernel decides what stays resident. Remote sources are read
//! whole, once, because a `.npy` has no structure that would let a range read
//! fetch less.

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{
    ArrayInfo, Axis, DatasetInfo, DatasetMetadata, Multiscale, MultiscaleDataset,
};
use std::path::Path;

use crate::source::{SourceRegistry, SourceSpec};
use crate::zarr_reader::{
    bytes_to_f32, PlaneAxis, PlaneBytes, PlaneRequest, Projection, TileBytes, TileEncoding,
    TileRequest,
};

/// What a `.npy` file holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpyKind {
    /// A 2D or 3D array of pixels — a mask, a density, a plane.
    Volume,
    /// A table of objects: a structured array, or `(N, k)` with a small `k`.
    Objects,
}

/// Decide which reader a `.npy` belongs to, from its header alone.
///
/// The distinction is real and cannot be guessed from the extension:
/// `clearmap-ng` writes masks and cell tables as `.npy` alike. The rules, in
/// order:
///
/// * a **structured** dtype names fields — that is a table, always;
/// * `(N,)` is a table of one column;
/// * `(N, k)` with `k <= 4` is a point list — a volume that narrow is not a
///   picture of anything;
/// * anything else is a volume.
pub fn classify(header_bytes: &[u8]) -> Result<NpyKind> {
    let text = header_text(header_bytes)?;
    let descr = value_of(&text, "descr").context("the header names no dtype")?;
    if descr.trim_start().starts_with('[') {
        return Ok(NpyKind::Objects);
    }
    let dims: Vec<u64> = value_of(&text, "shape")
        .context("the header names no shape")?
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .collect();
    Ok(match dims.as_slice() {
        [_] => NpyKind::Objects,
        [_, k] if *k <= 4 => NpyKind::Objects,
        _ => NpyKind::Volume,
    })
}

/// Where the array's bytes live.
#[derive(Debug)]
enum Storage {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl Storage {
    fn as_slice(&self) -> &[u8] {
        match self {
            Storage::Mapped(map) => &map[..],
            Storage::Owned(bytes) => bytes,
        }
    }
}

/// An opened `.npy` volume.
#[derive(Debug)]
pub struct NpyVolume {
    storage: Storage,
    /// Byte offset of the first element.
    offset: usize,
    /// `(z, y, x)`; a 2D array is one plane.
    shape: [u64; 3],
    dtype: String,
    /// Bytes per element.
    width: usize,
    little_endian: bool,
    metadata: DatasetInfo,
}

impl NpyVolume {
    /// Open a `.npy` from any source.
    pub async fn open(registry: &SourceRegistry, spec: &SourceSpec) -> Result<Self> {
        match registry.operator(spec)? {
            None => {
                let SourceSpec::File(path) = spec else {
                    bail!("source {} has no operator and is not a file", spec.uri());
                };
                Self::open_local(path)
            }
            Some(op) => {
                let key = match spec {
                    SourceSpec::S3 { .. } => String::new(),
                    SourceSpec::Http(url) => {
                        url.rsplit('/').next().unwrap_or_default().to_string()
                    }
                    SourceSpec::File(_) => unreachable!("file sources have no operator"),
                };
                let data = op
                    .read(&key)
                    .await
                    .with_context(|| format!("reading {}", spec.uri()))?;
                Self::from_bytes(Storage::Owned(data.to_vec()))
            }
        }
    }

    /// Open a local `.npy`, memory-mapped.
    pub fn open_local(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: the same guarantee every mmap-backed reader makes — the file
        // must not be truncated under us. These are pipeline outputs, written
        // once and then read.
        let map = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("mapping {}", path.display()))?;
        Self::from_bytes(Storage::Mapped(map))
    }

    fn from_bytes(storage: Storage) -> Result<Self> {
        let header = Header::parse(storage.as_slice())?;
        let elements: u64 = header.shape.iter().product();
        let needed = header.offset + elements as usize * header.width;
        if storage.as_slice().len() < needed {
            bail!(
                "this .npy claims {elements} element(s) of {} byte(s) and holds {} byte(s) of data",
                header.width,
                storage.as_slice().len().saturating_sub(header.offset)
            );
        }
        let metadata = describe(&header);
        Ok(Self {
            storage,
            offset: header.offset,
            shape: header.shape,
            dtype: header.dtype,
            width: header.width,
            little_endian: header.little_endian,
            metadata,
        })
    }

    pub fn metadata(&self) -> &DatasetInfo {
        &self.metadata
    }

    pub fn level_dtype(&self, _level: usize) -> Result<String> {
        Ok(self.dtype.clone())
    }

    /// The length of a named axis. A `.npy` volume has one level.
    pub fn axis_extent(&self, _level: usize, name: &str) -> Result<u64> {
        Ok(match name {
            "z" => self.shape[0],
            "y" => self.shape[1],
            "x" => self.shape[2],
            _ => 1,
        })
    }

    pub fn plane_shape(&self, _level: usize, axis: PlaneAxis) -> Result<(u64, u64)> {
        Ok(match axis {
            PlaneAxis::Z => (self.shape[1], self.shape[2]),
            PlaneAxis::Y => (self.shape[0], self.shape[2]),
            PlaneAxis::X => (self.shape[0], self.shape[1]),
        })
    }

    /// A contiguous run of `count` elements starting at `(z, y, x)`.
    ///
    /// A C-ordered array's rows are contiguous, so a tile row is one slice
    /// rather than a loop over elements.
    fn run(&self, z: u64, y: u64, x: u64, count: u64) -> &[u8] {
        let index = (z * self.shape[1] * self.shape[2] + y * self.shape[2] + x) as usize;
        let at = self.offset + index * self.width;
        let end = at + count as usize * self.width;
        &self.storage.as_slice()[at..end]
    }

    /// Copy a rectangle out of one plane, in the array's own dtype.
    fn read_rect(&self, z: u64, y: u64, x: u64, h: u64, w: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity((h * w) as usize * self.width);
        for row in y..y + h {
            if row >= self.shape[1] {
                out.resize(out.len() + (w as usize) * self.width, 0);
                continue;
            }
            let columns = w.min(self.shape[2].saturating_sub(x));
            if columns > 0 {
                let plane = z.min(self.shape[0].saturating_sub(1));
                out.extend_from_slice(self.run(plane, row, x, columns));
            }
            // Past the right edge, pad with zeros rather than shortening the
            // tile: the client's texture upload wants the shape it asked for.
            out.resize(
                out.len() + ((w - columns) as usize) * self.width,
                0,
            );
        }
        self.to_little_endian(out)
    }

    /// The wire wants little-endian; a big-endian array is byte-swapped here.
    fn to_little_endian(&self, mut bytes: Vec<u8>) -> Vec<u8> {
        if !self.little_endian && self.width > 1 {
            for element in bytes.chunks_exact_mut(self.width) {
                element.reverse();
            }
        }
        bytes
    }

    /// Read a tile, with the same contract as the zarr path.
    pub fn read_tile_bytes(&self, request: &TileRequest) -> Result<TileBytes> {
        let z_extent = self.shape[0];
        let z_start = request.z.min(z_extent.saturating_sub(1));
        let planes = match request.projection {
            Some(_) => (z_start + request.depth.max(1)).min(z_extent) - z_start,
            None => 1,
        };

        let mut raw = Vec::new();
        for plane in 0..planes {
            raw.extend_from_slice(&self.read_rect(
                z_start + plane,
                request.y,
                request.x,
                request.h,
                request.w,
            ));
        }

        match (request.projection, request.encoding) {
            (None, TileEncoding::Raw) => Ok(TileBytes {
                bytes: raw,
                dtype: self.dtype.clone(),
            }),
            (None, TileEncoding::F32) => {
                let pixels = bytes_to_f32(&raw, &self.dtype)?;
                Ok(TileBytes {
                    bytes: f32_bytes(&pixels),
                    dtype: "float32".to_string(),
                })
            }
            (Some(projection), _) => {
                let pixels = bytes_to_f32(&raw, &self.dtype)?;
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
    pub fn read_plane(&self, request: &PlaneRequest) -> Result<PlaneBytes> {
        let (height, width) = self.plane_shape(request.level, request.axis)?;
        let raw = match request.axis {
            PlaneAxis::Z => {
                let z = request.index.min(self.shape[0].saturating_sub(1));
                self.read_rect(z, 0, 0, self.shape[1], self.shape[2])
            }
            PlaneAxis::Y => {
                let y = request.index.min(self.shape[1].saturating_sub(1));
                let mut out = Vec::with_capacity((height * width) as usize * self.width);
                for z in 0..self.shape[0] {
                    out.extend_from_slice(&self.read_rect(z, y, 0, 1, self.shape[2]));
                }
                out
            }
            PlaneAxis::X => {
                let x = request.index.min(self.shape[2].saturating_sub(1));
                let mut out = Vec::with_capacity((height * width) as usize * self.width);
                for z in 0..self.shape[0] {
                    for y in 0..self.shape[1] {
                        out.extend_from_slice(&self.read_rect(z, y, x, 1, 1));
                    }
                }
                out
            }
        };

        let bytes = match request.encoding {
            TileEncoding::Raw => raw,
            TileEncoding::F32 => f32_bytes(&bytes_to_f32(&raw, &self.dtype)?),
        };
        Ok(PlaneBytes {
            bytes,
            dtype: match request.encoding {
                TileEncoding::Raw => self.dtype.clone(),
                TileEncoding::F32 => "float32".to_string(),
            },
            height,
            width,
        })
    }
}

/// Reduce `planes` consecutive planes. Mirrors the zarr path's own reducer.
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

fn f32_bytes(pixels: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for value in pixels {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The parsed `.npy` header.
struct Header {
    offset: usize,
    shape: [u64; 3],
    dtype: String,
    width: usize,
    little_endian: bool,
}

/// The header dictionary and where the data starts.
fn header_span(bytes: &[u8]) -> Result<(String, usize)> {
    if bytes.len() < 12 || &bytes[..6] != b"\x93NUMPY" {
        bail!("this file does not start with the NumPy magic");
    }
    let (header_len, start) = if bytes[6] == 1 {
        (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10usize)
    } else {
        (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12usize,
        )
    };
    let end = start + header_len;
    if bytes.len() < end {
        bail!("the .npy header runs past the end of the file");
    }
    Ok((String::from_utf8_lossy(&bytes[start..end]).into_owned(), end))
}

fn header_text(bytes: &[u8]) -> Result<String> {
    Ok(header_span(bytes)?.0)
}

impl Header {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let (text, end) = header_span(bytes)?;

        if value_of(&text, "fortran_order")
            .map(|v| v.contains("True"))
            .unwrap_or(false)
        {
            bail!("this array is Fortran-ordered; write it C-ordered (np.ascontiguousarray)");
        }
        let descr = value_of(&text, "descr").context("the header names no dtype")?;
        let descr = descr.trim().trim_matches(|c| c == '\'' || c == '"');
        let (dtype, width, little_endian) = scalar(descr)?;

        let dims: Vec<u64> = value_of(&text, "shape")
            .context("the header names no shape")?
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split(',')
            .filter_map(|part| part.trim().parse::<u64>().ok())
            .collect();
        let shape = match dims.as_slice() {
            [z, y, x] => [*z, *y, *x],
            [y, x] => [1, *y, *x],
            other => bail!("a volume is 2D or 3D; this array is {other:?}"),
        };

        Ok(Self {
            offset: end,
            shape,
            dtype,
            width,
            little_endian,
        })
    }
}

/// A NumPy scalar descriptor as `(dtype name, width, little-endian)`.
fn scalar(descr: &str) -> Result<(String, usize, bool)> {
    let (little_endian, rest) = match descr.chars().next() {
        Some('>') => (false, &descr[1..]),
        Some('<') | Some('|') | Some('=') => (true, &descr[1..]),
        _ => (true, descr),
    };
    let kind = rest.chars().next().context("dtype names no kind")?;
    let width: usize = rest[1..].parse().context("dtype names no width")?;
    let name = match (kind, width) {
        ('u', 1) => "uint8",
        ('u', 2) => "uint16",
        ('u', 4) => "uint32",
        ('u', 8) => "uint64",
        ('i', 1) => "int8",
        ('i', 2) => "int16",
        ('i', 4) => "int32",
        ('i', 8) => "int64",
        ('f', 4) => "float32",
        ('f', 8) => "float64",
        ('b', 1) => "uint8",
        _ => bail!("dtype `{descr}` is not one this reads"),
    };
    Ok((name.to_string(), width, little_endian))
}

fn value_of(header: &str, key: &str) -> Option<String> {
    let needle = format!("'{key}':");
    let at = header.find(&needle)? + needle.len();
    let rest = header[at..].trim_start();
    let end = match rest.chars().next()? {
        '(' => rest.find(')')? + 1,
        '[' => rest.rfind(']')? + 1,
        '\'' => rest[1..].find('\'')? + 2,
        _ => rest.find(',').unwrap_or(rest.len()),
    };
    Some(rest[..end].trim().to_string())
}

/// Describe the array as a one-level OME-Zarr dataset, so the frontend needs
/// to know nothing about where it came from.
fn describe(header: &Header) -> DatasetInfo {
    let axes = ["c", "z", "y", "x"]
        .iter()
        .map(|name| Axis {
            name: name.to_string(),
            axis_type: Some(
                match *name {
                    "c" => "channel",
                    _ => "space",
                }
                .to_string(),
            ),
            unit: None,
        })
        .collect();
    DatasetInfo {
        metadata: DatasetMetadata {
            multiscales: vec![Multiscale {
                axes,
                datasets: vec![MultiscaleDataset {
                    path: "0".to_string(),
                    coordinate_transformations: None,
                }],
                name: Some("npy".to_string()),
            }],
            omero: None,
        },
        arrays: vec![ArrayInfo {
            shape: vec![1, header.shape[0], header.shape[1], header.shape[2]],
            // No chunk grid: one "chunk" the size of a plane is what the tile
            // grid should be cut from.
            chunks: vec![1, 1, header.shape[1].min(512), header.shape[2].min(512)],
            dtype: header.dtype.clone(),
            order: Some(if header.little_endian { "<" } else { ">" }.to_string()),
            compressor: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn npy(descr: &str, shape: &str, data: &[u8]) -> Vec<u8> {
        let dict = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape}, }}");
        let mut header = dict.into_bytes();
        while !(10 + header.len() + 1).is_multiple_of(64) {
            header.push(b' ');
        }
        header.push(b'\n');
        let mut out = Vec::new();
        out.extend_from_slice(b"\x93NUMPY\x01\x00");
        out.extend_from_slice(&(header.len() as u16).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out
    }

    /// `value_at(z, y, x)` for the fixture below.
    fn value(z: u64, y: u64, x: u64) -> u16 {
        (x + 10 * y + 100 * z) as u16
    }

    fn volume() -> NpyVolume {
        let (dz, dy, dx) = (3u64, 4u64, 5u64);
        let mut data = Vec::new();
        for z in 0..dz {
            for y in 0..dy {
                for x in 0..dx {
                    data.extend_from_slice(&value(z, y, x).to_le_bytes());
                }
            }
        }
        let bytes = npy("<u2", "(3, 4, 5)", &data);
        NpyVolume::from_bytes(Storage::Owned(bytes)).expect("volume")
    }

    fn f32s(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    #[test]
    fn describes_itself_as_a_one_level_dataset() {
        let volume = volume();
        let info = volume.metadata();
        assert_eq!(info.arrays.len(), 1);
        assert_eq!(info.arrays[0].shape, vec![1, 3, 4, 5]);
        assert_eq!(info.arrays[0].dtype, "uint16");
        assert_eq!(info.metadata.multiscales[0].axes.len(), 4);
    }

    #[test]
    fn reads_a_tile_out_of_the_middle() {
        let volume = volume();
        let tile = volume
            .read_tile_bytes(&TileRequest::new(0, 1, 2, 2, 3).at(0, 0, 2))
            .expect("tile");
        let pixels = f32s(&tile.bytes);
        assert_eq!(pixels.len(), 6);
        for row in 0..2u64 {
            for column in 0..3u64 {
                assert_eq!(
                    pixels[(row * 3 + column) as usize],
                    value(2, 1 + row, 2 + column) as f32,
                    "at ({row}, {column})"
                );
            }
        }
    }

    #[test]
    fn a_tile_past_the_edge_is_padded_rather_than_short() {
        let volume = volume();
        let tile = volume
            .read_tile_bytes(&TileRequest::new(0, 3, 4, 2, 2))
            .expect("tile");
        let pixels = f32s(&tile.bytes);
        assert_eq!(pixels.len(), 4, "the tile is the size it asked for");
        assert_eq!(pixels[0], value(0, 3, 4) as f32);
        assert_eq!(pixels[1], 0.0, "past the right edge");
        assert_eq!(pixels[2], 0.0, "past the bottom edge");
    }

    #[test]
    fn raw_encoding_keeps_the_arrays_own_dtype() {
        let volume = volume();
        let tile = volume
            .read_tile_bytes(
                &TileRequest::new(0, 0, 0, 1, 2)
                    .at(0, 0, 1)
                    .encoded(TileEncoding::Raw),
            )
            .expect("tile");
        assert_eq!(tile.dtype, "uint16");
        assert_eq!(
            u16::from_le_bytes([tile.bytes[0], tile.bytes[1]]),
            value(1, 0, 0)
        );
    }

    #[test]
    fn a_max_projection_takes_the_brightest_plane() {
        let volume = volume();
        let tile = volume
            .read_tile_bytes(
                &TileRequest::new(0, 1, 1, 1, 1)
                    .at(0, 0, 0)
                    .projected(Some(Projection::Max), 3),
            )
            .expect("projection");
        assert_eq!(f32s(&tile.bytes)[0], value(2, 1, 1) as f32);
    }

    #[test]
    fn planes_hold_the_axes_they_claim() {
        let volume = volume();
        let plane = volume
            .read_plane(&PlaneRequest {
                level: 0,
                t: 0,
                c: 0,
                axis: PlaneAxis::Y,
                index: 2,
                encoding: TileEncoding::F32,
            })
            .expect("y plane");
        assert_eq!((plane.height, plane.width), (3, 5));
        let pixels = f32s(&plane.bytes);
        assert_eq!(pixels[5 + 3], value(1, 2, 3) as f32);

        let plane = volume
            .read_plane(&PlaneRequest {
                level: 0,
                t: 0,
                c: 0,
                axis: PlaneAxis::X,
                index: 4,
                encoding: TileEncoding::F32,
            })
            .expect("x plane");
        assert_eq!((plane.height, plane.width), (3, 4));
        let pixels = f32s(&plane.bytes);
        assert_eq!(pixels[4 + 1], value(1, 1, 4) as f32);
    }

    #[test]
    fn a_two_dimensional_array_is_one_plane() {
        let data: Vec<u8> = (0..6u8).flat_map(|v| (v as u16).to_le_bytes()).collect();
        let bytes = npy("<u2", "(2, 3)", &data);
        let volume = NpyVolume::from_bytes(Storage::Owned(bytes)).expect("volume");
        assert_eq!(volume.metadata().arrays[0].shape, vec![1, 1, 2, 3]);
    }

    #[test]
    fn big_endian_arrays_come_back_little_endian() {
        let data: Vec<u8> = [258u16, 3].iter().flat_map(|v| v.to_be_bytes()).collect();
        let bytes = npy(">u2", "(1, 2)", &data);
        let volume = NpyVolume::from_bytes(Storage::Owned(bytes)).expect("volume");
        let tile = volume
            .read_tile_bytes(&TileRequest::new(0, 0, 0, 1, 2))
            .expect("tile");
        assert_eq!(f32s(&tile.bytes), vec![258.0, 3.0]);
    }

    #[test]
    fn a_truncated_file_is_refused_by_name() {
        let bytes = npy("<u2", "(3, 4, 5)", &[0u8; 4]);
        let err = NpyVolume::from_bytes(Storage::Owned(bytes)).expect_err("refused");
        assert!(format!("{err}").contains("element"), "{err}");
    }

    #[test]
    fn fortran_order_is_refused_by_name() {
        let dict = "{'descr': '<u2', 'fortran_order': True, 'shape': (2, 2), }";
        let mut header = dict.as_bytes().to_vec();
        header.push(b'\n');
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x93NUMPY\x01\x00");
        bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&[0u8; 8]);
        let err = NpyVolume::from_bytes(Storage::Owned(bytes)).expect_err("refused");
        assert!(format!("{err}").contains("Fortran"), "{err}");
    }
}
