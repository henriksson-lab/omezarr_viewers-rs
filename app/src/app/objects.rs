//! Object (detection) layers: how they are drawn, and their rows.

use yew::prelude::*;

use crate::api_client::ObjectBatch;

use wasm_bindgen_futures::spawn_local;

use super::{App, AppMsg};
use omezarr_viewer_common::ObjectRegion;

use crate::api_client;
use crate::layers::{LayerState, LayerUi, ObjectData};

pub enum ObjectMsg {
    Color(usize, [f32; 3]),
    Opacity(usize, f32),
    Size(usize, f32),
    Hollow(usize, bool),
    ColorBy(usize, Option<usize>),
    Filter(usize, usize, Option<(f32, f32)>),
    Slab(usize, f32),
    Loaded {
        layer: String,
        batch: Box<ObjectBatch>,
        generation: u64,
    },
    Inspected(String, Option<serde_json::Value>),
}

impl From<ObjectMsg> for AppMsg {
    fn from(msg: ObjectMsg) -> Self {
        AppMsg::Object(msg)
    }
}

impl App {
    pub(super) fn update_objects(&mut self, ctx: &Context<Self>, msg: ObjectMsg) -> bool {
        match msg {
            ObjectMsg::Color(layer, color) => {
                if let Some(state) = self.object_mut(layer) {
                    state.color = color;
                }
                true
            }
            ObjectMsg::Opacity(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.opacity = value;
                }
                true
            }
            ObjectMsg::Size(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.size = value;
                }
                true
            }
            ObjectMsg::Hollow(layer, hollow) => {
                if let Some(state) = self.object_mut(layer) {
                    state.hollow = hollow;
                }
                true
            }
            ObjectMsg::ColorBy(layer, column) => {
                if let Some(state) = self.object_mut(layer) {
                    state.color_by = column;
                }
                self.rebuild_points(layer);
                true
            }
            ObjectMsg::Filter(layer, column, filter) => {
                if let Some(state) = self.object_mut(layer) {
                    if let Some(slot) = state.filters.get_mut(column) {
                        *slot = filter;
                    }
                }
                // Filtering happens over the rows already loaded, so it costs
                // a buffer rebuild rather than a round trip.
                self.rebuild_points(layer);
                true
            }
            ObjectMsg::Slab(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.slab = value;
                }
                self.load_tiles(ctx);
                true
            }
            ObjectMsg::Loaded {
                layer,
                batch,
                generation,
            } => {
                if generation != self.tile_generation {
                    return false;
                }
                self.tiles_pending = self.tiles_pending.saturating_sub(1);
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let batch = *batch;
                let loaded = batch.positions.len();
                let total = batch.total;
                self.objects.insert(
                    layer.clone(),
                    ObjectData {
                        positions: batch.positions,
                        rows: batch.rows,
                        columns: batch.columns,
                    },
                );
                if let Some(state) = self.object_mut(index) {
                    state.loaded = loaded;
                    state.total = total;
                }
                self.rebuild_points(index);
                true
            }
            ObjectMsg::Inspected(layer, row) => {
                match row {
                    Some(row) => {
                        if let Some(index) = self.layers.iter().position(|l| l.id == layer) {
                            let selected = row.get("row").and_then(|v| v.as_u64());
                            if let Some(state) = self.object_mut(index) {
                                state.selected_row = selected.map(|r| r as u32);
                            }
                        }
                        self.inspected.insert(layer, describe_row(&row));
                    }
                    None => {
                        if let Some(index) = self.layers.iter().position(|l| l.id == layer) {
                            if let Some(state) = self.object_mut(index) {
                                state.selected_row = None;
                            }
                        }
                        self.inspected.remove(&layer);
                    }
                }
                true
            }
        }
    }
}

impl App {
    fn object_mut(&mut self, layer: usize) -> Option<&mut crate::layers::ObjectUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Objects(state) => Some(state),
            _ => None,
        }
    }

    /// Rebuild one object layer's GPU buffer from the rows already loaded.
    fn rebuild_points(&mut self, index: usize) {
        let Some(layer) = self.layers.get(index) else {
            return;
        };
        let LayerUi::Objects(state) = &layer.ui else {
            return;
        };
        let id = layer.id.clone();
        let Some(data) = self.objects.get(&id) else {
            return;
        };
        let (vertices, shown) = data.to_vertices(state);
        if let Some(cs) = &self.canvas_state {
            if let Some(ref mut canvas) = *cs.borrow_mut() {
                if let Some(old) = canvas.point_buffers.remove(&id) {
                    canvas.renderer.delete_points(&old);
                }
                match canvas.renderer.upload_points(&vertices) {
                    Ok(buffer) => {
                        canvas.point_buffers.insert(id, buffer);
                    }
                    Err(e) => log::warn!("upload points: {}", e),
                }
            }
        }
        if let Some(state) = self.object_mut(index) {
            state.shown = shown;
        }
    }

    /// Fetch the rows of one object layer for the visible rectangle.
    ///
    /// Every column comes back, not just the coloured one: filtering and
    /// colour-by then cost a buffer rebuild rather than a round trip, and a
    /// handful of `f32` columns over at most `MAX_OBJECTS` rows is a few
    /// megabytes.
    pub(super) fn load_objects(
        &mut self,
        ctx: &Context<Self>,
        layer: &LayerState,
        state: &crate::layers::ObjectUiState,
        view: (f32, f32, f32, f32),
        generation: u64,
    ) {
        /// The most rows to draw at once. Above this the server decimates, and
        /// the panel says so rather than showing a subset as if it were all.
        const MAX_OBJECTS: usize = 200_000;

        let z = self.z_slice as f32;
        let (z0, z1) = if state.schema.has_z && state.slab > 0.0 {
            (z - state.slab, z + state.slab)
        } else {
            (f32::NEG_INFINITY, f32::INFINITY)
        };
        let region = ObjectRegion {
            y0: view.1,
            y1: view.3,
            x0: view.0,
            x1: view.2,
            z0,
            z1,
            max: MAX_OBJECTS,
        };
        let columns: Vec<String> = state
            .schema
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect();

        self.tiles_pending += 1;
        let id = layer.id.clone();
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_objects(&id, &region, &columns).await {
                Ok(batch) => link.send_message(ObjectMsg::Loaded {
                    layer: id,
                    batch: Box::new(batch),
                    generation,
                }),
                Err(e) => {
                    log::warn!("objects: {}", e);
                    link.send_message(ObjectMsg::Loaded {
                        layer: id,
                        batch: Box::default(),
                        generation,
                    });
                }
            }
        });
    }
}

/// One inspected row, as a line of text: `id 4 · size 91 · confidence 0.87`.
fn describe_row(row: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let (Some(y), Some(x)) = (
        row.get("y").and_then(|v| v.as_f64()),
        row.get("x").and_then(|v| v.as_f64()),
    ) {
        let z = row.get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
        parts.push(format!("({z:.0}, {y:.0}, {x:.0})"));
    }
    if let Some(columns) = row.get("columns").and_then(|v| v.as_object()) {
        for (name, value) in columns {
            let text = match value {
                serde_json::Value::Number(number) => match number.as_u64() {
                    Some(exact) => exact.to_string(),
                    None => format!("{:.4}", number.as_f64().unwrap_or(f64::NAN)),
                },
                other => other.to_string(),
            };
            parts.push(format!("{name} {text}"));
        }
    }
    parts.join(" \u{00b7} ")
}
