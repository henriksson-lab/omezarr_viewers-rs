//! What a table's columns are, and the ROI rows they make.
//!
//! Every backend funnels through [`Columns`] and then through
//! [`rows_from_columns`], so "which columns make an ROI table" is decided
//! in one place regardless of which bytes it came out of.

use anyhow::{bail, Result};
use omezarr_viewer_common::{Annotation, Geometry, Plane, WorldScale};
use std::collections::BTreeMap;

use super::*;

// ---------------------------------------------------------------------------
// Columns: what every backend produces, and what rows are made of
// ---------------------------------------------------------------------------

/// One table's columns, whatever bytes they came out of.
///
/// The point of the detour: four backends produce this, and exactly one
/// function turns it into annotations. Without it, "an ROI table needs
/// x/y/z_micrometer" would be written four times and would drift three ways.
#[derive(Debug, Default, Clone)]
pub struct Columns {
    pub(crate) numeric: BTreeMap<String, Vec<f64>>,
    pub(crate) text: BTreeMap<String, Vec<String>>,
    pub(crate) rows: usize,
    /// The order the columns were read in, so a table view shows what the
    /// writer wrote rather than what a sorted map happens to produce.
    pub(crate) order: Vec<String>,
    /// `obsm["spatial"]`, when the file had one: `(n_obs, 2)` or `(n_obs, 3)`
    /// coordinates, flattened row-major.
    ///
    /// The scverse convention, and the default key for scanpy and squidpy — it
    /// is where a spatial-omics table keeps its positions, since such a table
    /// has no `*_micrometer` columns at all.
    pub(crate) spatial: Option<(Vec<f64>, usize)>,
}

impl Columns {
    pub(crate) fn push_numeric(&mut self, name: impl Into<String>, values: Vec<f64>) {
        let name = name.into();
        self.rows = self.rows.max(values.len());
        self.remember(&name);
        self.numeric.insert(name, values);
    }

    pub(crate) fn push_text(&mut self, name: impl Into<String>, values: Vec<String>) {
        let name = name.into();
        self.rows = self.rows.max(values.len());
        self.remember(&name);
        self.text.insert(name, values);
    }

    fn remember(&mut self, name: &str) {
        if !self.order.iter().any(|n| n == name) {
            self.order.push(name.to_string());
        }
    }

    pub(crate) fn number(&self, name: &str, row: usize) -> Option<f64> {
        self.numeric.get(name).and_then(|v| v.get(row).copied())
    }

    /// A value as text, whichever kind of column holds it.
    ///
    /// Both kinds, because `label` is a number in a masking ROI table and a
    /// string in a hand-written one, and a class is a class either way.
    pub fn string(&self, name: &str, row: usize) -> Option<String> {
        if let Some(text) = self.text.get(name).and_then(|v| v.get(row)) {
            return Some(text.clone());
        }
        self.number(name, row).map(|v| {
            if v.fract() == 0.0 {
                format!("{v:.0}")
            } else {
                v.to_string()
            }
        })
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.numeric.contains_key(name) || self.text.contains_key(name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.order.iter().map(String::as_str).collect()
    }

    pub fn row_count(&self) -> usize {
        self.rows
    }

    /// One column as `(name, values)`, numbers as numbers and text as text.
    pub fn column(&self, name: &str) -> Option<ColumnValues<'_>> {
        if let Some(values) = self.numeric.get(name) {
            return Some(ColumnValues::Numbers(values));
        }
        self.text.get(name).map(|v| ColumnValues::Text(v))
    }

    /// Does this table carry positions of any kind?
    pub(crate) fn has_positions(&self) -> bool {
        (self.has("x_micrometer") && self.has("y_micrometer")) || self.spatial.is_some()
    }
}

/// One column's values, in the type it was stored as.
#[derive(Debug, Clone, Copy)]
pub enum ColumnValues<'a> {
    Numbers(&'a [f64]),
    Text(&'a [String]),
}

/// Turn a table's columns into world-coordinate annotations.
pub(crate) fn rows_from_columns(columns: &Columns, scale: WorldScale) -> Result<Vec<Annotation>> {
    // A spatial-omics table has no `*_micrometer` columns at all: its positions
    // are in `obsm["spatial"]`, already in the pixels of the image they were
    // measured from — so they are taken as world coordinates unscaled, where a
    // micrometre column has to be divided by the factor it was written with.
    if !(columns.has("x_micrometer") && columns.has("y_micrometer")) {
        if let Some((values, width)) = &columns.spatial {
            return Ok(spatial_rows(values, *width, columns));
        }
    }

    // The position columns are what makes it an ROI table; without them it is
    // some other table that happens to live under `tables/`.
    if !(columns.has("x_micrometer") && columns.has("y_micrometer") && columns.has("z_micrometer"))
    {
        bail!(
            "not an ROI table: no x/y/z_micrometer columns and no obsm[\"spatial\"] (found {})",
            columns.names().join(", ")
        );
    }

    let [sz, sy, sx] = scale.voxel;
    // A factor of zero would divide the file's own numbers away; a store that
    // declared one is broken, and 1 is the honest reading.
    let back = |value: f64, factor: f64| if factor > 0.0 { value / factor } else { value };

    let mut rows = Vec::with_capacity(columns.rows);
    for row in 0..columns.rows {
        let at = |name: &str| columns.number(name, row).unwrap_or(0.0);
        let (x, y) = (back(at("x_micrometer"), sx), back(at("y_micrometer"), sy));
        let (w, h) = (
            back(at("len_x_micrometer"), sx),
            back(at("len_y_micrometer"), sy),
        );
        // A zero-size box is a point, and saying so is what keeps a point drawn
        // here, written to a table and read back from being a degenerate
        // rectangle nothing can select.
        let geometry = if w <= 0.0 && h <= 0.0 {
            Geometry::Point([x, y])
        } else {
            Geometry::rect(x, y, x + w, y + h)
        };
        rows.push(Annotation {
            id: 0,
            geometry,
            plane: Plane::at(
                back(at("z_micrometer"), sz).round() as i32,
                back(at("t_second"), scale.seconds).round() as i32,
            ),
            z_extent: back(at("len_z_micrometer"), sz).round().max(0.0) as u32,
            t_extent: back(at("len_t_second"), scale.seconds).round().max(0.0) as u32,
            // `label` as a fallback: a masking ROI table names its label image's
            // id there, and showing that beats showing nothing.
            label: columns
                .string("class", row)
                .or_else(|| columns.string("label", row))
                .unwrap_or_default(),
            ..Default::default()
        });
    }
    Ok(rows)
}

/// Points from an `obsm["spatial"]` array.
///
/// `(n_obs, 2)` is `(x, y)` and `(n_obs, 3)` is `(x, y, z)` — scanpy's and
/// squidpy's order, which is the order the array is written in and not the
/// `(z, y, x)` an OME-Zarr axis list uses.
pub(crate) fn spatial_rows(values: &[f64], width: usize, columns: &Columns) -> Vec<Annotation> {
    let label = |row: usize| {
        columns
            .string("class", row)
            .or_else(|| columns.string("label", row))
            .unwrap_or_default()
    };
    (0..values.len() / width.max(1))
        .map(|row| {
            let at = |axis: usize| values.get(row * width + axis).copied().unwrap_or(0.0);
            Annotation {
                geometry: Geometry::Point([at(0), at(1)]),
                plane: Plane::at(if width > 2 { at(2).round() as i32 } else { 0 }, 0),
                label: label(row),
                ..Default::default()
            }
        })
        .collect()
}

/// World coordinates to the file's units.
pub(crate) fn to_units(value: f64, factor: f64) -> f64 {
    value * factor
}

/// How many of these rows lose shape by being written as bounding boxes.
///
/// An ROI table has no way to hold a polygon, so a caller about to write one
/// should be able to say so rather than discovering it on the round trip.
pub fn lossy_rows(rows: &[Annotation]) -> usize {
    rows.iter()
        .filter(|row| !matches!(row.geometry, Geometry::Point(_)) && !is_rect(&row.geometry))
        .count()
}

/// Is this geometry exactly the axis-aligned rectangle of its own bounds?
pub(crate) fn is_rect(geometry: &Geometry) -> bool {
    let Geometry::Polygon(rings) = geometry else {
        return false;
    };
    let [Some(ring)] = [rings.first()] else {
        return false;
    };
    if rings.len() != 1 {
        return false;
    }
    let Some([x0, y0, x1, y1]) = geometry.bounds() else {
        return false;
    };
    // Four distinct corners, each of them a corner of the bounding box.
    let corners: Vec<[f64; 2]> = {
        let mut open = ring.clone();
        if open.len() > 1 && open.first() == open.last() {
            open.pop();
        }
        open
    };
    corners.len() == 4
        && corners
            .iter()
            .all(|p| (p[0] == x0 || p[0] == x1) && (p[1] == y0 || p[1] == y1))
}

pub(crate) fn encode_csv(rows: &[Annotation], scale: WorldScale) -> Result<Vec<u8>> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(HEADER)?;
    let [sz, sy, sx] = scale.voxel;
    for row in rows {
        // Every shape goes as its bounding box: that is all the format has.
        let [x0, y0, x1, y1] = row.bounds().unwrap_or([0.0; 4]);
        writer.write_record([
            // The index is a string, as `index_type` declares. `roi_<id>` rather
            // than a bare number so it stays a string through a reader that
            // infers types from values.
            format!("roi_{}", row.id),
            to_units(x0, sx).to_string(),
            to_units(y0, sy).to_string(),
            to_units(row.plane.z as f64, sz).to_string(),
            to_units(x1 - x0, sx).to_string(),
            to_units(y1 - y0, sy).to_string(),
            to_units(row.z_extent as f64, sz).to_string(),
            to_units(row.plane.t as f64, scale.seconds).to_string(),
            to_units(row.t_extent as f64, scale.seconds).to_string(),
            row.label.clone(),
        ])?;
    }
    Ok(writer.into_inner()?)
}
