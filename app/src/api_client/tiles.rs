//! Pixels: image tiles, label tiles, one voxel, and an orthogonal slice.
//!
//! Label ids never travel as `f32` — an id above 2^24 does not survive the
//! round trip, and a filtered id that does not exist is worse than no filter at
//! all — so label tiles come back as raw bytes and are widened here.

use gloo_net::http::Request;

use super::get_host_url;

/// Where a tile is, on the wire.
#[derive(Clone, Copy, Debug)]
pub struct TileAddress {
    pub level: usize,
    pub t: u64,
    pub c: u64,
    pub z: u64,
    pub y: u64,
    pub x: u64,
    pub h: u64,
    pub w: u64,
    /// `Some((kind, depth))` to project through z instead of taking one slice.
    pub projection: Option<(&'static str, u64)>,
}

fn tile_url(layer: &str, at: &TileAddress, encoding: &str) -> String {
    let mut url = format!(
        "{}/api/tile?layer={}&encoding={}&level={}&t={}&c={}&z={}&y={}&x={}&h={}&w={}",
        get_host_url(),
        layer,
        encoding,
        at.level,
        at.t,
        at.c,
        at.z,
        at.y,
        at.x,
        at.h,
        at.w
    );
    if let Some((kind, depth)) = at.projection {
        url.push_str(&format!("&zproj={kind}&depth={depth}"));
    }
    url
}

/// Fetch a rectangular tile region as float32 pixel data.
pub async fn fetch_tile(layer: &str, at: &TileAddress) -> Result<Vec<f32>, String> {
    let resp = Request::get(&tile_url(layer, at, "f32"))
        .send()
        .await
        .map_err(|e| format!("fetch tile: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch tile: status {}", resp.status()));
    }
    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read tile bytes: {}", e))?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}

/// Fetch a tile of label ids, in the array's own dtype.
///
/// The ids are widened to `u32` here rather than on the server: that is the
/// texture format they are going into, and it is the widest exact integer a
/// WebGL2 integer texture holds. A `uint64` array whose ids exceed `u32` is
/// reported rather than silently wrapped.
pub async fn fetch_label_tile(layer: &str, at: &TileAddress) -> Result<Vec<u32>, String> {
    let resp = Request::get(&tile_url(layer, at, "raw"))
        .send()
        .await
        .map_err(|e| format!("fetch labels: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch labels: status {}", resp.status()));
    }
    let dtype = resp
        .headers()
        .get("X-Dtype")
        .unwrap_or_else(|| "uint32".to_string());
    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read label bytes: {}", e))?;
    ids_from_bytes(&bytes, &dtype)
}

/// Widen raw label bytes to `u32`.
pub fn ids_from_bytes(bytes: &[u8], dtype: &str) -> Result<Vec<u32>, String> {
    match dtype {
        "uint8" => Ok(bytes.iter().map(|&b| b as u32).collect()),
        "int8" => Ok(bytes.iter().map(|&b| (b as i8).max(0) as u32).collect()),
        "uint16" => Ok(bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c) as u32)
            .collect()),
        "int16" => Ok(bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c).max(0) as u32)
            .collect()),
        "uint32" => Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect()),
        "int32" => Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c).max(0) as u32)
            .collect()),
        "uint64" => {
            let mut overflowed = false;
            let ids: Vec<u32> = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| {
                    let wide = u64::from_le_bytes(*c);
                    if wide > u32::MAX as u64 {
                        overflowed = true;
                    }
                    wide as u32
                })
                .collect();
            if overflowed {
                return Err("label ids exceed 2^32 and cannot be drawn as a u32 texture".into());
            }
            Ok(ids)
        }
        other => Err(format!("{other} is not an integer label dtype")),
    }
}

/// One voxel's value — what a click on a label layer asks for.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct VoxelValue {
    pub dtype: String,
    /// The exact integer, for integer arrays.
    pub id: Option<u64>,
    /// The value as a float, for every array.
    pub value: Option<f32>,
    /// The region this id names, when an atlas ontology is loaded.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub acronym: Option<String>,
}

/// Read one voxel of a layer.
pub async fn fetch_value(
    layer: &str,
    level: usize,
    t: u64,
    c: u64,
    z: u64,
    y: u64,
    x: u64,
) -> Result<VoxelValue, String> {
    let url = format!(
        "{}/api/value?layer={}&level={}&t={}&c={}&z={}&y={}&x={}",
        get_host_url(),
        layer,
        level,
        t,
        c,
        z,
        y,
        x
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch value: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch value: status {}", resp.status()));
    }
    resp.json::<VoxelValue>()
        .await
        .map_err(|e| format!("parse value: {}", e))
}

/// A plane read from one axis: pixels and the shape they came back in.
pub struct PlaneData {
    pub pixels: Vec<f32>,
    pub width: u32,
    pub height: u32,
}

/// Fetch a whole plane across an axis, as float32.
///
/// The orthogonal panes read this rather than tiles: a `(z, x)` plane crosses
/// every chunk row of the store, so it is read once at a level that fits the
/// pane instead of being assembled from a second tile grid.
pub async fn fetch_slice(
    layer: &str,
    axis: &str,
    index: u64,
    level: usize,
    t: u64,
    c: u64,
) -> Result<PlaneData, String> {
    let url = format!(
        "{}/api/slice?layer={}&axis={}&index={}&level={}&t={}&c={}&encoding=f32",
        get_host_url(),
        layer,
        axis,
        index,
        level,
        t,
        c
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch slice: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch slice: status {}", resp.status()));
    }
    let width = resp
        .headers()
        .get("X-Width")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let height = resp
        .headers()
        .get("X-Height")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read slice: {}", e))?;
    let pixels = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect();
    Ok(PlaneData {
        pixels,
        width,
        height,
    })
}
