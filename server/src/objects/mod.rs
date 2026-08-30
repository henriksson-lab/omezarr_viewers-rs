//! Object layers: a position per row, plus typed columns.
//!
//! This is where a run's *annotation* lives — one row per detected cell, spot
//! or instance — and the two pipelines that produce it disagree about the byte
//! form. `blockflow`'s YOLO path writes a CSV; its `model_segment` path writes
//! a `table` blob; `clearmap-ng`'s points are arrays. Each gets a reader
//! ([`csv`], [`table`], [`npy`]) and they all produce the same [`ObjectStore`],
//! so nothing downstream knows which one it came from.
//!
//! # Coordinates
//!
//! Positions are held in the *source volume's* pixel coordinates and mapped
//! into the viewer's world by a per-layer [`ObjectSpace`]. A detector that ran
//! on level 2 of a pyramid writes level-2 coordinates, and nothing in the file
//! says so — the scale is a fact about the run, which is why it is a layer
//! setting rather than something guessed here.

pub mod csv;
pub mod npy;
pub mod table;

use anyhow::{bail, Context, Result};
use omezarr_viewer_common::{ObjectColumn, ObjectSchema};

use crate::source::{SourceRegistry, SourceSpec};

/// A column's values, in the type the source declared.
///
/// A `U64` column stays exact all the way to the inspector: an object id or a
/// voxel count is not a float, and rounding one to show it is a lie about the
/// data rather than a rendering detail.
#[derive(Debug, Clone)]
pub enum ColumnData {
    U64(Vec<u64>),
    F64(Vec<f64>),
}

impl ColumnData {
    pub fn len(&self) -> usize {
        match self {
            ColumnData::U64(values) => values.len(),
            ColumnData::F64(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ColumnData::U64(_) => "u64",
            ColumnData::F64(_) => "f64",
        }
    }

    /// The value at `row` as an `f64` — what the wire and the filters use.
    pub fn at(&self, row: usize) -> Option<f64> {
        match self {
            ColumnData::U64(values) => values.get(row).map(|&v| v as f64),
            ColumnData::F64(values) => values.get(row).copied(),
        }
    }

    /// The value at `row` as JSON, in its own type.
    pub fn json_at(&self, row: usize) -> serde_json::Value {
        match self {
            ColumnData::U64(values) => values
                .get(row)
                .map(|&v| serde_json::json!(v))
                .unwrap_or(serde_json::Value::Null),
            ColumnData::F64(values) => values
                .get(row)
                .map(|&v| serde_json::json!(v))
                .unwrap_or(serde_json::Value::Null),
        }
    }

    fn range(&self) -> Option<[f64; 2]> {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let n = self.len();
        for row in 0..n {
            let Some(v) = self.at(row) else { continue };
            if v.is_nan() {
                continue;
            }
            lo = lo.min(v);
            hi = hi.max(v);
        }
        (lo <= hi).then_some([lo, hi])
    }
}

/// One named column.
#[derive(Debug, Clone)]
pub struct NamedColumn {
    pub name: String,
    pub data: ColumnData,
}

/// How a layer's own coordinates map into the viewer's world.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectSpace {
    /// World pixels per source unit, `(z, y, x)`.
    pub scale: [f64; 3],
    /// World offset added after scaling, `(z, y, x)`.
    pub offset: [f64; 3],
}

impl Default for ObjectSpace {
    fn default() -> Self {
        Self {
            scale: [1.0, 1.0, 1.0],
            offset: [0.0, 0.0, 0.0],
        }
    }
}

impl ObjectSpace {
    /// Parse `z,y,x` triples from query parameters.
    pub fn parse(scale: Option<&str>, offset: Option<&str>) -> Result<Self> {
        let mut space = ObjectSpace::default();
        if let Some(text) = scale {
            space.scale = triple(text).context("parsing scale")?;
        }
        if let Some(text) = offset {
            space.offset = triple(text).context("parsing offset")?;
        }
        Ok(space)
    }

    fn to_world(self, position: [f32; 3]) -> [f32; 3] {
        [
            (position[0] as f64 * self.scale[0] + self.offset[0]) as f32,
            (position[1] as f64 * self.scale[1] + self.offset[1]) as f32,
            (position[2] as f64 * self.scale[2] + self.offset[2]) as f32,
        ]
    }
}

fn triple(text: &str) -> Result<[f64; 3]> {
    let parts: Vec<f64> = text
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .context("expected three comma-separated numbers")?;
    match parts.len() {
        3 => Ok([parts[0], parts[1], parts[2]]),
        2 => Ok([1.0, parts[0], parts[1]]),
        _ => bail!("expected `z,y,x` or `y,x`, got `{text}`"),
    }
}

/// The rows of one object layer, columnar, with a coarse spatial index.
#[derive(Debug)]
pub struct ObjectStore {
    /// `(z, y, x)` per row, in source coordinates.
    positions: Vec<[f32; 3]>,
    columns: Vec<NamedColumn>,
    has_z: bool,
    space: ObjectSpace,
    index: GridIndex,
    bounds: Option<[f64; 6]>,
}

/// A uniform bucket grid over `(y, x)`, with `z` kept per row.
///
/// Coarse on purpose: the query this serves is "everything in the visible
/// rectangle, in a z slab", and a bucket that holds a few hundred rows costs
/// one linear scan of a few hundred rows to filter exactly.
#[derive(Debug)]
struct GridIndex {
    origin: [f32; 2],
    cell: [f32; 2],
    shape: [usize; 2],
    /// Row indices per bucket, row-major over `shape`.
    buckets: Vec<Vec<u32>>,
}

impl GridIndex {
    fn build(positions: &[[f32; 3]]) -> Self {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        for p in positions {
            for axis in 0..2 {
                min[axis] = min[axis].min(p[axis + 1]);
                max[axis] = max[axis].max(p[axis + 1]);
            }
        }
        if !min[0].is_finite() {
            min = [0.0, 0.0];
            max = [1.0, 1.0];
        }

        // Aim for ~64 rows per bucket, and never fewer than one bucket.
        let target = ((positions.len() as f64 / 64.0).sqrt().ceil() as usize).clamp(1, 512);
        let extent = [(max[0] - min[0]).max(1.0), (max[1] - min[1]).max(1.0)];
        let cell = [extent[0] / target as f32, extent[1] / target as f32];
        let shape = [target, target];
        let mut buckets = vec![Vec::new(); shape[0] * shape[1]];
        for (row, p) in positions.iter().enumerate() {
            let (by, bx) = Self::bucket_of(min, cell, shape, p[1], p[2]);
            buckets[by * shape[1] + bx].push(row as u32);
        }
        Self {
            origin: min,
            cell,
            shape,
            buckets,
        }
    }

    fn bucket_of(
        origin: [f32; 2],
        cell: [f32; 2],
        shape: [usize; 2],
        y: f32,
        x: f32,
    ) -> (usize, usize) {
        let by = (((y - origin[0]) / cell[0]).floor().max(0.0) as usize).min(shape[0] - 1);
        let bx = (((x - origin[1]) / cell[1]).floor().max(0.0) as usize).min(shape[1] - 1);
        (by, bx)
    }

    /// Every row whose bucket overlaps the rectangle.
    fn candidates(&self, y0: f32, y1: f32, x0: f32, x1: f32) -> impl Iterator<Item = u32> + '_ {
        let (by0, bx0) = Self::bucket_of(self.origin, self.cell, self.shape, y0, x0);
        let (by1, bx1) = Self::bucket_of(self.origin, self.cell, self.shape, y1, x1);
        (by0..=by1).flat_map(move |by| {
            (bx0..=bx1).flat_map(move |bx| self.buckets[by * self.shape[1] + bx].iter().copied())
        })
    }
}

/// A region query: a rectangle, a z slab, and a cap on how many rows come back.
///
/// The shared crate's type, because the client declared the same seven fields
/// under the name `ObjectRegion`: this is an API contract, and one side
/// silently gaining a field is exactly what the shared crate exists to stop.
/// Re-exported under the server's own word for it.
pub use omezarr_viewer_common::ObjectRegion as ObjectQuery;

/// The answer to a region query.
pub struct ObjectSelection {
    /// Row indices, in store order.
    pub rows: Vec<u32>,
    /// How many rows matched before decimation.
    pub total: usize,
}

impl ObjectStore {
    pub fn new(positions: Vec<[f32; 3]>, columns: Vec<NamedColumn>, has_z: bool) -> Result<Self> {
        for column in &columns {
            if column.data.len() != positions.len() {
                bail!(
                    "column `{}` has {} value(s) for {} row(s)",
                    column.name,
                    column.data.len(),
                    positions.len()
                );
            }
        }
        let index = GridIndex::build(&positions);
        let bounds = bounds_of(&positions);
        Ok(Self {
            positions,
            columns,
            has_z,
            space: ObjectSpace::default(),
            index,
            bounds,
        })
    }

    pub fn with_space(mut self, space: ObjectSpace) -> Self {
        self.space = space;
        self
    }

    pub fn space(&self) -> ObjectSpace {
        self.space
    }

    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub fn columns(&self) -> &[NamedColumn] {
        &self.columns
    }

    /// The world position of a row.
    pub fn world_position(&self, row: usize) -> Option<[f32; 3]> {
        self.positions.get(row).map(|&p| self.space.to_world(p))
    }

    /// The schema as the API reports it, in world units.
    pub fn schema(&self) -> ObjectSchema {
        ObjectSchema {
            columns: self
                .columns
                .iter()
                .map(|column| ObjectColumn {
                    name: column.name.clone(),
                    kind: column.data.kind().to_string(),
                    range: column.data.range(),
                })
                .collect(),
            has_z: self.has_z,
            bounds: self.bounds.map(|b| {
                let lo = self.space.to_world([b[0] as f32, b[1] as f32, b[2] as f32]);
                let hi = self.space.to_world([b[3] as f32, b[4] as f32, b[5] as f32]);
                [
                    lo[0] as f64,
                    lo[1] as f64,
                    lo[2] as f64,
                    hi[0] as f64,
                    hi[1] as f64,
                    hi[2] as f64,
                ]
            }),
        }
    }

    /// Rows inside a world-coordinate query.
    pub fn query(&self, query: &ObjectQuery) -> ObjectSelection {
        // The index is in source coordinates, so the query comes back the
        // other way rather than the whole store going forwards.
        let inv = |value: f32, axis: usize| -> f32 {
            let scale = self.space.scale[axis];
            if scale == 0.0 {
                return value;
            }
            ((value as f64 - self.space.offset[axis]) / scale) as f32
        };
        let (y0, y1) = (inv(query.y0, 1), inv(query.y1, 1));
        let (x0, x1) = (inv(query.x0, 2), inv(query.x1, 2));
        let (z0, z1) = (inv(query.z0, 0), inv(query.z1, 0));

        let mut rows: Vec<u32> = self
            .index
            .candidates(y0.min(y1), y0.max(y1), x0.min(x1), x0.max(x1))
            .filter(|&row| {
                let p = self.positions[row as usize];
                p[1] >= y0.min(y1)
                    && p[1] <= y0.max(y1)
                    && p[2] >= x0.min(x1)
                    && p[2] <= x0.max(x1)
                    && (!self.has_z || (p[0] >= z0.min(z1) && p[0] <= z0.max(z1)))
            })
            .collect();
        rows.sort_unstable();
        let total = rows.len();

        if query.max > 0 && rows.len() > query.max {
            // A stride, not a random sample: the same query returns the same
            // rows, so panning back and forth does not reshuffle the picture.
            let stride = rows.len().div_ceil(query.max);
            rows = rows.into_iter().step_by(stride).collect();
        }
        ObjectSelection { rows, total }
    }

    /// The row nearest a world point within `radius`, if any.
    pub fn nearest(&self, z: f32, y: f32, x: f32, radius: f32) -> Option<usize> {
        let query = ObjectQuery {
            y0: y - radius,
            y1: y + radius,
            x0: x - radius,
            x1: x + radius,
            z0: f32::NEG_INFINITY,
            z1: f32::INFINITY,
            max: 0,
        };
        let selection = self.query(&query);
        let mut best: Option<(f32, usize)> = None;
        for row in selection.rows {
            let p = self.world_position(row as usize)?;
            let dz = if self.has_z { p[0] - z } else { 0.0 };
            let distance = ((p[1] - y).powi(2) + (p[2] - x).powi(2) + dz.powi(2)).sqrt();
            if distance <= radius && best.is_none_or(|(d, _)| distance < d) {
                best = Some((distance, row as usize));
            }
        }
        best.map(|(_, row)| row)
    }

    /// One row as JSON, with every column in its own type.
    pub fn row_json(&self, row: usize) -> Option<serde_json::Value> {
        let world = self.world_position(row)?;
        let mut object = serde_json::Map::new();
        object.insert("row".into(), serde_json::json!(row));
        object.insert("z".into(), serde_json::json!(world[0]));
        object.insert("y".into(), serde_json::json!(world[1]));
        object.insert("x".into(), serde_json::json!(world[2]));
        let mut columns = serde_json::Map::new();
        for column in &self.columns {
            columns.insert(column.name.clone(), column.data.json_at(row));
        }
        object.insert("columns".into(), serde_json::Value::Object(columns));
        Some(serde_json::Value::Object(object))
    }

    /// The wire form of a selection: a small header, then positions, then one
    /// `f32` per requested column per row.
    ///
    /// Columns travel as `f32` because their only job on the client is to
    /// colour and filter points; the exact value is what `/api/objects/at`
    /// returns, and that one keeps a `u64` a `u64`.
    pub fn encode(&self, selection: &ObjectSelection, columns: &[usize]) -> Vec<u8> {
        let count = selection.rows.len();
        let mut bytes = Vec::with_capacity(16 + count * (12 + 4 + columns.len() * 4));
        bytes.extend_from_slice(b"OBJS");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&(count as u32).to_le_bytes());
        bytes.extend_from_slice(&(columns.len() as u32).to_le_bytes());

        for &row in &selection.rows {
            let world = self.world_position(row as usize).unwrap_or([0.0; 3]);
            for value in world {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        for &row in &selection.rows {
            bytes.extend_from_slice(&row.to_le_bytes());
        }
        for &column in columns {
            for &row in &selection.rows {
                let value = self
                    .columns
                    .get(column)
                    .and_then(|c| c.data.at(row as usize))
                    .unwrap_or(f64::NAN) as f32;
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        bytes
    }
}

fn bounds_of(positions: &[[f32; 3]]) -> Option<[f64; 6]> {
    if positions.is_empty() {
        return None;
    }
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in positions {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    Some([
        lo[0] as f64,
        lo[1] as f64,
        lo[2] as f64,
        hi[0] as f64,
        hi[1] as f64,
        hi[2] as f64,
    ])
}

/// Read an object source, choosing the reader by extension.
///
/// The extension is the only signal available before the bytes are read, and
/// each reader validates its own magic or header afterwards — so a `.csv` that
/// is really a table blob is refused by name rather than parsed into nonsense.
pub async fn open(registry: &SourceRegistry, spec: &SourceSpec) -> Result<ObjectStore> {
    let bytes = read_all(registry, spec).await?;
    let extension = spec.extension().unwrap_or_default();
    match extension.as_str() {
        "csv" | "tsv" => csv::read(&bytes, extension == "tsv"),
        "npy" => npy::read(&bytes),
        "bin" | "blob" | "table" | "" => table::read(&bytes),
        other => bail!("`{other}` is not an object format this build reads (csv, tsv, npy, or a blockflow table blob)"),
    }
}

/// Read a whole source into memory.
///
/// Object sources are small next to the volumes beside them — a million rows
/// of five columns is tens of megabytes — and every reader here needs the whole
/// thing to sort it anyway.
async fn read_all(registry: &SourceRegistry, spec: &SourceSpec) -> Result<Vec<u8>> {
    crate::source::read_bytes(registry, spec, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ObjectStore {
        let positions = vec![
            [0.0, 10.0, 10.0],
            [1.0, 20.0, 20.0],
            [2.0, 30.0, 30.0],
            [3.0, 40.0, 40.0],
        ];
        let columns = vec![
            NamedColumn {
                name: "id".into(),
                data: ColumnData::U64(vec![1, 2, 3, 4]),
            },
            NamedColumn {
                name: "size".into(),
                data: ColumnData::F64(vec![1.5, 2.5, 3.5, 4.5]),
            },
        ];
        ObjectStore::new(positions, columns, true).expect("store")
    }

    #[test]
    fn queries_return_rows_in_the_region_and_nothing_else() {
        let store = store();
        let selection = store.query(&ObjectQuery {
            y0: 15.0,
            y1: 35.0,
            x0: 15.0,
            x1: 35.0,
            z0: f32::NEG_INFINITY,
            z1: f32::INFINITY,
            max: 0,
        });
        assert_eq!(selection.rows, vec![1, 2]);
        assert_eq!(selection.total, 2);
    }

    #[test]
    fn the_z_slab_excludes_rows_outside_it() {
        let store = store();
        let selection = store.query(&ObjectQuery {
            y0: 0.0,
            y1: 100.0,
            x0: 0.0,
            x1: 100.0,
            z0: 1.5,
            z1: 2.5,
            max: 0,
        });
        assert_eq!(selection.rows, vec![2]);
    }

    #[test]
    fn decimation_is_deterministic_and_reports_the_true_total() {
        let store = store();
        let query = ObjectQuery {
            y0: 0.0,
            y1: 100.0,
            x0: 0.0,
            x1: 100.0,
            z0: f32::NEG_INFINITY,
            z1: f32::INFINITY,
            max: 2,
        };
        let first = store.query(&query);
        let second = store.query(&query);
        assert_eq!(first.rows, second.rows);
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.total, 4, "the answer says how much it left out");
    }

    #[test]
    fn a_scale_moves_positions_and_the_query_with_them() {
        let store = store().with_space(ObjectSpace {
            scale: [1.0, 2.0, 2.0],
            offset: [0.0, 0.0, 0.0],
        });
        assert_eq!(store.world_position(1).unwrap(), [1.0, 40.0, 40.0]);
        let selection = store.query(&ObjectQuery {
            y0: 35.0,
            y1: 45.0,
            x0: 35.0,
            x1: 45.0,
            z0: f32::NEG_INFINITY,
            z1: f32::INFINITY,
            max: 0,
        });
        assert_eq!(selection.rows, vec![1], "the query is in world units");
    }

    #[test]
    fn nearest_finds_the_row_under_a_click_and_nothing_far_away() {
        let store = store();
        assert_eq!(store.nearest(1.0, 21.0, 21.0, 5.0), Some(1));
        assert_eq!(store.nearest(1.0, 100.0, 100.0, 5.0), None);
    }

    #[test]
    fn the_encoding_carries_positions_rows_and_columns() {
        let store = store();
        let selection = store.query(&ObjectQuery {
            y0: 0.0,
            y1: 100.0,
            x0: 0.0,
            x1: 100.0,
            z0: f32::NEG_INFINITY,
            z1: f32::INFINITY,
            max: 0,
        });
        let bytes = store.encode(&selection, &[1]);
        assert_eq!(&bytes[0..4], b"OBJS");
        let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        assert_eq!(count, 4);
        let columns = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(columns, 1);

        let positions_at = 16;
        let y = f32::from_le_bytes(
            bytes[positions_at + 4..positions_at + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(y, 10.0);

        let column_at = positions_at + count * 12 + count * 4;
        let size = f32::from_le_bytes(bytes[column_at..column_at + 4].try_into().unwrap());
        assert_eq!(size, 1.5);
    }
}
