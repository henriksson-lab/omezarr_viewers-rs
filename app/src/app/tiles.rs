//! What the camera shows, and the tiles that has to be fetched for it.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api_client::{self, TileAddress};
use crate::layers::{LayerState, LayerUi};
use crate::ortho_pane::OrthoLayer;
use crate::viewer_canvas::{ChannelRenderInfo, LevelTileInfo, TileKey};

use super::{App, AppMsg, TilePayload};

pub enum ViewMsg {
    ToggleOrtho,
    Projection(Option<&'static str>),
    ProjectionDepth(u64),
    /// A click in an orthogonal pane, as fractions of the pane.
    OrthoPicked(&'static str, f32, f32),
    ZSlice(u32),
    TIndex(u32),
    Camera(f32, f32, f32, f32, f32), // (pan_x, pan_y, zoom, canvas_w, canvas_h)
    TileLoaded {
        key: TileKey,
        data: TilePayload,
        w: u32,
        h: u32,
        generation: u64,
    },
    TileFailed {
        generation: u64,
        key: TileKey,
    },
}

impl From<ViewMsg> for AppMsg {
    fn from(msg: ViewMsg) -> Self {
        AppMsg::View(msg)
    }
}

/// Which tiles a layer needs, and for which moment in the volume.
struct RequestArea {
    /// `(tx_min, ty_min, tx_max, ty_max)`, in tiles at this level.
    tiles: (u32, u32, u32, u32),
    z: u64,
    t: u64,
    generation: u64,
}

impl App {
    pub(super) fn update_tiles(&mut self, ctx: &Context<Self>, msg: ViewMsg) -> bool {
        match msg {
            ViewMsg::ToggleOrtho => {
                self.ortho = !self.ortho;
                if self.ortho && self.crosshair == (0.0, 0.0) {
                    let world = self.world_size();
                    self.crosshair = (world.0 / 2.0, world.1 / 2.0);
                }
                true
            }
            ViewMsg::Projection(kind) => {
                self.projection = kind.map(|kind| {
                    let depth = self.projection.map(|(_, depth)| depth).unwrap_or(8);
                    (kind, depth)
                });
                self.load_tiles(ctx);
                true
            }
            ViewMsg::ProjectionDepth(depth) => {
                if let Some((kind, _)) = self.projection {
                    self.projection = Some((kind, depth.max(1)));
                    self.load_tiles(ctx);
                }
                true
            }
            ViewMsg::OrthoPicked(axis, u, v) => {
                let world = self.world_size();
                let z_max = self.z_max.max(1) as f32;
                match axis {
                    // The bottom pane is (z down, x across).
                    "y" => {
                        self.crosshair.0 = u * world.0;
                        self.z_slice = ((v * z_max) as u32).min(self.z_max.saturating_sub(1));
                    }
                    // The right pane is transposed to (y down, z across).
                    _ => {
                        self.crosshair.1 = v * world.1;
                        self.z_slice = ((u * z_max) as u32).min(self.z_max.saturating_sub(1));
                    }
                }
                self.load_tiles(ctx);
                true
            }
            ViewMsg::ZSlice(z) => {
                self.z_slice = z;
                self.load_tiles(ctx);
                true
            }
            ViewMsg::TIndex(t) => {
                self.t_index = t;
                self.load_tiles(ctx);
                true
            }
            ViewMsg::Camera(pan_x, pan_y, zoom, canvas_w, canvas_h) => {
                let world = self.world_size();
                let mut level_changed = false;
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref state) = *cs.borrow() {
                        for layer in &self.layers {
                            let level = layer.pick_level(world, zoom, (canvas_w, canvas_h));
                            if state.current_level.get(&layer.id) != Some(&level) {
                                level_changed = true;
                            }
                        }
                    }
                }
                if level_changed {
                    self.load_tiles_fresh(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
                } else {
                    self.load_visible_tiles(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
                }
                level_changed
            }
            ViewMsg::TileLoaded {
                key,
                data,
                w,
                h,
                generation,
            } => {
                if generation != self.tile_generation {
                    return false;
                }
                self.tiles_pending = self.tiles_pending.saturating_sub(1);
                self.tiles_in_flight.remove(&key);
                // A layer that did not say how to display itself gets its
                // contrast from the first tile that arrives.
                if let TilePayload::Intensity(pixels) = &data {
                    self.auto_contrast(&key, pixels);
                }
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        let uploaded = match &data {
                            TilePayload::Intensity(pixels) => {
                                state.renderer.upload_tile(w, h, pixels)
                            }
                            TilePayload::Labels(ids) => state.renderer.upload_label_tile(w, h, ids),
                        };
                        match uploaded {
                            Ok(tex) => {
                                state.tile_cache.insert(key, tex);
                            }
                            Err(e) => log::warn!("Upload tile: {}", e),
                        }
                    }
                }
                true
            }
            ViewMsg::TileFailed { generation, key } => {
                if generation != self.tile_generation {
                    return false;
                }
                self.tiles_pending = self.tiles_pending.saturating_sub(1);
                self.tiles_in_flight.remove(&key);
                self.tiles_pending > 0
            }
        }
    }
}

impl App {
    /// The camera's current zoom, or 1 before the canvas exists.
    pub(super) fn zoom(&self) -> f32 {
        self.canvas_state
            .as_ref()
            .and_then(|cs| cs.borrow().as_ref().map(|state| state.camera.zoom))
            .unwrap_or(1.0)
    }

    /// The level a layer is currently drawn at.
    pub(super) fn level_of(&self, layer: &LayerState) -> usize {
        self.canvas_state
            .as_ref()
            .and_then(|cs| {
                cs.borrow()
                    .as_ref()
                    .and_then(|state| state.current_level.get(&layer.id).copied())
            })
            .unwrap_or(0)
    }

    /// The z index of this layer's level that the reference layer's `z_slice`
    /// points at. A label volume may have fewer z planes than the image.
    pub(super) fn layer_z(&self, layer: &LayerState, level: usize) -> u64 {
        let layer_z = layer.axis_len_at(level, "z").max(1);
        let reference_z = self
            .reference_layer()
            .map(|r| r.axis_len("z").max(1))
            .unwrap_or(1);
        if reference_z <= 1 {
            return 0;
        }
        let scaled = self.z_slice as u64 * layer_z / reference_z;
        scaled.min(layer_z - 1)
    }

    pub(super) fn layer_t(&self, layer: &LayerState) -> u64 {
        let layer_t = layer.axis_len("t").max(1);
        (self.t_index as u64).min(layer_t - 1)
    }

    /// Reload from whatever camera the canvas currently has.
    pub(super) fn load_tiles(&mut self, ctx: &Context<Self>) {
        let (pan_x, pan_y, zoom, canvas_w, canvas_h) = match &self.canvas_state {
            Some(cs) => match *cs.borrow() {
                Some(ref state) => {
                    let cam = &state.camera;
                    (cam.x, cam.y, cam.zoom, cam.canvas_w, cam.canvas_h)
                }
                None => return,
            },
            None => return,
        };
        if canvas_w <= 0.0 || canvas_h <= 0.0 {
            return;
        }
        self.load_tiles_fresh(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
    }

    /// Full reload: bump generation, clear counters, invalidate stale in-flight requests.
    fn load_tiles_fresh(
        &mut self,
        ctx: &Context<Self>,
        pan_x: f32,
        pan_y: f32,
        zoom: f32,
        canvas_w: f32,
        canvas_h: f32,
    ) {
        self.tile_generation += 1;
        self.tiles_pending = 0;
        self.tiles_in_flight.clear();
        self.load_visible_tiles(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
    }

    /// Fetch what is visible, for every visible layer.
    fn load_visible_tiles(
        &mut self,
        ctx: &Context<Self>,
        pan_x: f32,
        pan_y: f32,
        zoom: f32,
        canvas_w: f32,
        canvas_h: f32,
    ) {
        let world = self.world_size();
        if world.0 <= 0.0 || world.1 <= 0.0 {
            return;
        }
        let view = visible_world_rect((pan_x, pan_y), zoom, (canvas_w, canvas_h), world);
        let generation = self.tile_generation;
        let layers = self.layers.clone();

        if let Some(cs) = &self.canvas_state {
            if let Some(ref mut state) = *cs.borrow_mut() {
                state.world_size = world;
            }
        }

        for (index, layer) in layers.iter().enumerate() {
            if !layer.visible {
                continue;
            }
            if let LayerUi::Objects(state) = &layer.ui {
                self.load_objects(ctx, layer, state, view, generation);
                let _ = index;
                continue;
            }
            let level = layer.pick_level(world, zoom, (canvas_w, canvas_h));
            let Some(grid) = layer.tile_grid(level) else {
                continue;
            };
            let scale = layer.level_to_world(level, world);
            let tile_world = (grid.tile_w as f32 * scale.0, grid.tile_h as f32 * scale.1);
            if tile_world.0 <= 0.0 || tile_world.1 <= 0.0 {
                continue;
            }

            let tx_min = (view.0 / tile_world.0).floor().max(0.0) as u32;
            let tx_max = ((view.2 / tile_world.0).ceil().max(0.0) as u32).min(grid.num_tiles_x);
            let ty_min = (view.1 / tile_world.1).floor().max(0.0) as u32;
            let ty_max = ((view.3 / tile_world.1).ceil().max(0.0) as u32).min(grid.num_tiles_y);

            self.record_level(layer, level, scale, (tx_min, ty_min, tx_max, ty_max));

            let z = self.layer_z(layer, level);
            let t = self.layer_t(layer);
            let channels: Vec<usize> = match &layer.ui {
                LayerUi::Image { channels, .. } => channels
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| ch.visible)
                    .map(|(i, _)| i)
                    .collect(),
                LayerUi::Labels(_) => vec![0],
                // None of these has tiles: an object layer is fetched as rows,
                // an annotation layer is already in memory, and a table has no
                // pixels at all.
                LayerUi::Objects(_) | LayerUi::Annotations(_) | LayerUi::Table(_) => continue,
            };

            self.request_tiles(
                ctx,
                layer,
                level,
                grid,
                &channels,
                RequestArea {
                    tiles: (tx_min, ty_min, tx_max, ty_max),
                    z,
                    t,
                    generation,
                },
            );
        }
    }

    /// Tell the canvas which level a layer is being drawn at, and drop the
    /// tiles that level makes useless.
    ///
    /// Coarser levels are kept as fallback coverage — they are what fills the
    /// screen while the finer ones are still in flight — but a finer level is
    /// dead weight the moment the camera zooms out past it.
    fn record_level(
        &self,
        layer: &LayerState,
        level: usize,
        scale: (f32, f32),
        tiles: (u32, u32, u32, u32),
    ) {
        let (tx_min, ty_min, tx_max, ty_max) = tiles;
        let Some(grid) = layer.tile_grid(level) else {
            return;
        };
        let Some(cs) = &self.canvas_state else {
            return;
        };
        let Some(ref mut state) = *cs.borrow_mut() else {
            return;
        };
        state.level_info.insert(
            (layer.id.clone(), level),
            LevelTileInfo {
                level_size: layer.level_size(level).unwrap_or((1.0, 1.0)),
                tile_size: (grid.tile_w as f32, grid.tile_h as f32),
                num_tiles_x: grid.num_tiles_x,
                num_tiles_y: grid.num_tiles_y,
                world_scale: scale,
            },
        );
        state.current_level.insert(layer.id.clone(), level);
        state
            .level_info
            .retain(|(id, l), _| id != &layer.id || *l >= level);
        state.tile_cache.retain(|key, _| {
            if key.layer != layer.id {
                return true;
            }
            if key.level != level {
                return key.level > level;
            }
            key.tile_x >= tx_min
                && key.tile_x < tx_max
                && key.tile_y >= ty_min
                && key.tile_y < ty_max
        });
    }

    /// Ask the server for every tile in `area` that is not already here or on
    /// its way.
    fn request_tiles(
        &mut self,
        ctx: &Context<Self>,
        layer: &LayerState,
        level: usize,
        grid: crate::layers::TileGrid,
        channels: &[usize],
        area: RequestArea,
    ) {
        let (tx_min, ty_min, tx_max, ty_max) = area.tiles;
        let (z, t, generation) = (area.z, area.t, area.generation);
        let is_labels_layer = layer.is_labels();
        let projection = self.projection;
        for ty in ty_min..ty_max {
            for tx in tx_min..tx_max {
                let y_start = ty as u64 * grid.tile_h;
                let x_start = tx as u64 * grid.tile_w;
                let h = grid.tile_h.min(grid.img_h.saturating_sub(y_start));
                let w = grid.tile_w.min(grid.img_w.saturating_sub(x_start));
                if h == 0 || w == 0 {
                    continue;
                }

                for &channel in channels {
                    let key = TileKey {
                        layer: layer.id.clone(),
                        level,
                        tile_y: ty,
                        tile_x: tx,
                        channel,
                    };
                    let cached = self
                        .canvas_state
                        .as_ref()
                        .and_then(|cs| {
                            cs.borrow()
                                .as_ref()
                                .map(|state| state.tile_cache.contains_key(&key))
                        })
                        .unwrap_or(false);
                    if cached || self.tiles_in_flight.contains(&key) {
                        continue;
                    }

                    self.tiles_in_flight.insert(key.clone());
                    self.tiles_pending += 1;

                    let address = TileAddress {
                        level,
                        t,
                        c: channel as u64,
                        z,
                        y: y_start,
                        x: x_start,
                        h,
                        w,
                        // A label layer is never projected: the maximum of
                        // a set of ids is not an id.
                        projection: (!is_labels_layer).then_some(projection).flatten(),
                    };
                    let link = ctx.link().clone();
                    let layer_id = layer.id.clone();
                    let is_labels = layer.is_labels();
                    spawn_local(async move {
                        let loaded = if is_labels {
                            api_client::fetch_label_tile(&layer_id, &address)
                                .await
                                .map(TilePayload::Labels)
                        } else {
                            api_client::fetch_tile(&layer_id, &address)
                                .await
                                .map(TilePayload::Intensity)
                        };
                        match loaded {
                            Ok(data) => link.send_message(ViewMsg::TileLoaded {
                                key,
                                data,
                                w: w as u32,
                                h: h as u32,
                                generation,
                            }),
                            Err(e) => {
                                log::warn!("tile: {}", e);
                                link.send_message(ViewMsg::TileFailed { generation, key });
                            }
                        }
                    });
                }
            }
        }
    }
    /// Set a channel's contrast from the first tile it ever loads.
    ///
    /// Only once per channel, and only where nothing else said what the range
    /// should be: an adjustment the user makes must not be undone by the next
    /// tile, and a store with OMERO windows already has an answer.
    fn auto_contrast(&mut self, key: &TileKey, pixels: &[f32]) {
        let Some(index) = self.layers.iter().position(|l| l.id == key.layer) else {
            return;
        };
        let Some(channel) = self.channel_mut(index, key.channel) else {
            return;
        };
        if !channel.auto_contrast {
            return;
        }
        let mut low = f32::INFINITY;
        let mut high = f32::NEG_INFINITY;
        for value in pixels {
            if value.is_finite() {
                low = low.min(*value);
                high = high.max(*value);
            }
        }
        if !(low.is_finite() && high.is_finite() && high > low) {
            // An all-one-value tile says nothing; wait for one that does.
            return;
        }
        channel.contrast_min = low;
        channel.contrast_max = high;
        channel.auto_contrast = false;
    }

    /// What the orthogonal panes draw: every visible image layer's visible
    /// channels, at a level whose plane fits a pane without a second tile grid.
    pub(super) fn ortho_layers(&self, axis: &str) -> Vec<OrthoLayer> {
        let world = self.world_size();
        self.layers
            .iter()
            .filter(|layer| layer.visible)
            .filter_map(|layer| {
                let LayerUi::Image {
                    channels,
                    dtype_max,
                } = &layer.ui
                else {
                    return None;
                };
                let visible: Vec<(usize, ChannelRenderInfo)> = channels
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| ch.visible && ch.opacity > 0.0)
                    .map(|(index, ch)| {
                        (
                            index,
                            ChannelRenderInfo {
                                color: ch.color,
                                contrast_min: ch.contrast_min,
                                contrast_max: ch.contrast_max,
                                opacity: ch.opacity,
                            },
                        )
                    })
                    .collect();
                if visible.is_empty() {
                    return None;
                }
                let level = self.ortho_level(layer);
                let (level_w, level_h) = layer.level_size(level).unwrap_or(world);
                // The crosshair is in world pixels; each layer is cut at the
                // same place in *its* pixels.
                let index = match axis {
                    "y" => (self.crosshair.1 / world.1.max(1.0) * level_h) as u64,
                    _ => (self.crosshair.0 / world.0.max(1.0) * level_w) as u64,
                };
                Some(OrthoLayer {
                    id: layer.id.clone(),
                    level,
                    index,
                    dtype_max: *dtype_max,
                    channels: visible,
                })
            })
            .collect()
    }

    /// The level an orthogonal pane reads at.
    ///
    /// A pane is a few hundred pixels wide, and a plane is read whole — so the
    /// level is chosen by what fits rather than by the camera. Level 0 of a
    /// whole-brain volume would be a hundred thousand pixels across for a pane
    /// that can show two hundred.
    fn ortho_level(&self, layer: &LayerState) -> usize {
        const MAX_PLANE: f32 = 2048.0;
        let mut level = 0;
        for candidate in 0..layer.num_levels() {
            level = candidate;
            if let Some((w, h)) = layer.level_size(candidate) {
                if w.max(h) <= MAX_PLANE {
                    break;
                }
            }
        }
        level
    }
}

/// The world-pixel rectangle the camera currently shows: the vertex shader's
/// transform inverted at the four clip-space corners.
fn visible_world_rect(
    pan: (f32, f32),
    zoom: f32,
    canvas: (f32, f32),
    world: (f32, f32),
) -> (f32, f32, f32, f32) {
    let fit = zoom * (canvas.0 / world.0).min(canvas.1 / world.1);
    let scale_x = fit * world.0 / canvas.0;
    let scale_y = fit * world.1 / canvas.1;

    let to_world = |clip_x: f32, clip_y: f32| -> (f32, f32) {
        let cx = (clip_x - pan.0 * 2.0 / canvas.0) / scale_x;
        // The shader negates y, so the clip-space corner is flipped back here.
        let cy = (-clip_y - pan.1 * 2.0 / canvas.1) / scale_y;
        ((cx / 2.0 + 0.5) * world.0, (cy / 2.0 + 0.5) * world.1)
    };

    let (x0, y0) = to_world(-1.0, -1.0);
    let (x1, y1) = to_world(1.0, 1.0);
    (
        x0.min(x1).max(0.0),
        y0.min(y1).max(0.0),
        x0.max(x1).min(world.0),
        y0.max(y1).min(world.1),
    )
}
