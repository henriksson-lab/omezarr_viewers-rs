//! Object (detection) layers: the rows in view, and one row inspected.
//!
//! `/api/objects` strides over the canonical order and reports `X-Total`, so a
//! decimated answer can say "showing N of M" rather than presenting a subset as
//! though it were everything.

use gloo_net::http::Request;

use super::get_host_url;

/// One object query's answer: rows, their columns, and what was left out.
#[derive(Debug, Default)]
pub struct ObjectBatch {
    pub positions: Vec<[f32; 3]>,
    pub rows: Vec<u32>,
    /// One array per requested column, in the order they were asked for.
    pub columns: Vec<Vec<f32>>,
    /// How many rows matched on the server, before any cap.
    pub total: usize,
}

/// The rectangle and slab an object query covers, in world pixels.
#[derive(Clone, Copy, Debug)]
pub struct ObjectRegion {
    pub y0: f32,
    pub y1: f32,
    pub x0: f32,
    pub x1: f32,
    pub z0: f32,
    pub z1: f32,
    /// The most rows to accept. The server decimates deterministically above it.
    pub max: usize,
}

/// Fetch the objects in a region, with the named columns.
pub async fn fetch_objects(
    layer: &str,
    region: &ObjectRegion,
    columns: &[String],
) -> Result<ObjectBatch, String> {
    let url = format!(
        "{}/api/objects?layer={}&y0={}&y1={}&x0={}&x1={}&z0={}&z1={}&max={}&columns={}",
        get_host_url(),
        layer,
        region.y0,
        region.y1,
        region.x0,
        region.x1,
        region.z0,
        region.z1,
        region.max,
        columns.join(",")
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch objects: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch objects: status {}", resp.status()));
    }
    let total = resp
        .headers()
        .get("X-Total")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read objects: {}", e))?;
    let mut batch = decode_objects(&bytes)?;
    batch.total = total;
    Ok(batch)
}

/// Decode the packed object buffer: a header, positions, row ids, columns.
pub fn decode_objects(bytes: &[u8]) -> Result<ObjectBatch, String> {
    if bytes.len() < 16 || &bytes[0..4] != b"OBJS" {
        return Err("this is not an object buffer".into());
    }
    let read_u32 = |at: usize| -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    };
    let version = read_u32(4);
    if version != 1 {
        return Err(format!(
            "object buffer version {version} is not readable here"
        ));
    }
    let count = read_u32(8) as usize;
    let column_count = read_u32(12) as usize;

    let needed = 16 + count * 12 + count * 4 + count * column_count * 4;
    if bytes.len() < needed {
        return Err(format!(
            "an object buffer of {count} row(s) and {column_count} column(s) needs {needed} bytes, got {}",
            bytes.len()
        ));
    }

    let mut at = 16;
    let mut positions = Vec::with_capacity(count);
    for _ in 0..count {
        let mut p = [0.0f32; 3];
        for value in p.iter_mut() {
            *value = f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
            at += 4;
        }
        positions.push(p);
    }
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        rows.push(read_u32(at));
        at += 4;
    }
    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            values.push(f32::from_le_bytes([
                bytes[at],
                bytes[at + 1],
                bytes[at + 2],
                bytes[at + 3],
            ]));
            at += 4;
        }
        columns.push(values);
    }

    Ok(ObjectBatch {
        positions,
        rows,
        columns,
        total: count,
    })
}

/// The row nearest a world point, with every column in its own type.
pub async fn fetch_object_at(
    layer: &str,
    z: f32,
    y: f32,
    x: f32,
    radius: f32,
) -> Result<Option<serde_json::Value>, String> {
    let url = format!(
        "{}/api/objects/at?layer={}&z={}&y={}&x={}&r={}",
        get_host_url(),
        layer,
        z,
        y,
        x,
        radius
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch object: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch object: status {}", resp.status()));
    }
    let value = resp
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("parse object: {}", e))?;
    Ok((!value.is_null()).then_some(value))
}
