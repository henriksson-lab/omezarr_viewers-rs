//! An object (detection) layer: how it is drawn, and the rows behind it.

use omezarr_viewer_common::ObjectSchema;

use super::LayerStyle;

/// UI state for an object layer.
#[derive(Clone, PartialEq)]
pub struct ObjectUiState {
    pub schema: ObjectSchema,
    pub count: u64,
    /// How it is drawn.
    pub style: LayerStyle,
    /// Rings rather than discs, so the pixels underneath stay visible.
    pub hollow: bool,
    /// Which column colours the points, if any.
    pub color_by: Option<usize>,
    /// Per-column `(min, max)` filter, when one is set.
    pub filters: Vec<Option<(f32, f32)>>,
    /// The row the last click selected.
    pub selected_row: Option<u32>,
    /// What the last fetch returned, and how much matched before the cap.
    pub loaded: usize,
    pub total: usize,
    /// Rows filtered out on the client, of `loaded`.
    pub shown: usize,
}

impl ObjectUiState {
    pub fn new(schema: ObjectSchema, count: u64) -> Self {
        let filters = vec![None; schema.columns.len()];
        let slab = if schema.has_z { 8.0 } else { 0.0 };
        Self {
            schema,
            count,
            style: LayerStyle {
                color: [1.0, 0.85, 0.2],
                opacity: 0.9,
                size: 9.0,
                slab,
            },
            hollow: false,
            color_by: None,
            filters,
            selected_row: None,
            loaded: 0,
            total: 0,
            shown: 0,
        }
    }
}

/// The rows one object layer currently has on the client.
///
/// Held whole rather than only as a GPU buffer so that filtering and
/// colour-by are instant: they rebuild the buffer from these arrays without
/// another round trip.
#[derive(Clone, Default, PartialEq)]
pub struct ObjectData {
    pub positions: Vec<[f32; 3]>,
    pub rows: Vec<u32>,
    /// One array per schema column, in schema order.
    pub columns: Vec<Vec<f32>>,
}

impl ObjectData {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Build the interleaved `(z, y, x, value, row)` buffer the point shader
    /// reads, applying the layer's filters.
    pub fn to_vertices(&self, state: &ObjectUiState) -> (Vec<f32>, usize) {
        let mut out = Vec::with_capacity(self.len() * 5);
        let mut shown = 0;
        for row in 0..self.len() {
            if !self.passes(state, row) {
                continue;
            }
            let position = self.positions[row];
            out.extend_from_slice(&[position[0], position[1], position[2]]);
            let value = state
                .color_by
                .and_then(|column| self.columns.get(column))
                .and_then(|values| values.get(row))
                .copied()
                .unwrap_or(0.0);
            out.push(value);
            out.push(self.rows.get(row).copied().unwrap_or(row as u32) as f32);
            shown += 1;
        }
        (out, shown)
    }

    fn passes(&self, state: &ObjectUiState, row: usize) -> bool {
        for (column, filter) in state.filters.iter().enumerate() {
            let Some((lo, hi)) = filter else { continue };
            let Some(value) = self.columns.get(column).and_then(|v| v.get(row)) else {
                continue;
            };
            if value.is_nan() || *value < *lo || *value > *hi {
                return false;
            }
        }
        true
    }
}
