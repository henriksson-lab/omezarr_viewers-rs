use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::DatasetInfo;

use crate::api_client;
use crate::controls::axis_sliders::AxisSliders;
use crate::controls::channel_panel::{self, ChannelPanel};
use crate::viewer_canvas::{ChannelRenderInfo, LevelTileInfo, TileKey, ViewerCanvas, ViewerCanvasState};

/// UI state for a single channel: visibility, color, contrast, and opacity.
#[derive(Clone)]
struct ChannelUiState {
    label: String,
    visible: bool,
    color: [f32; 3],
    contrast_min: f32,
    contrast_max: f32,
    opacity: f32,
}

/// Root Yew component managing dataset, channels, tile loading, and layout.
pub struct App {
    dataset: Option<DatasetInfo>,
    datasets: Vec<String>,
    current_dataset: Option<String>,
    channels: Vec<ChannelUiState>,
    z_slice: u32,
    t_index: u32,
    z_max: u32,
    t_max: u32,
    dtype_max: f32,
    canvas_state: Option<Rc<RefCell<Option<ViewerCanvasState>>>>,
    error: Option<String>,
    current_level: usize,
    tile_generation: u64,
    tiles_pending: u32,
    tiles_in_flight: HashSet<TileKey>,
    panel_visible: bool,
}

pub enum AppMsg {
    TogglePanel,
    DatasetsLoaded(Vec<String>),
    DatasetSelected(String),
    DatasetLoaded(DatasetInfo),
    LoadError(String),
    CanvasReady(Rc<RefCell<Option<ViewerCanvasState>>>),
    SetChannelVisibility(usize, bool),
    SetChannelColor(usize, [f32; 3]),
    SetChannelContrastMin(usize, f32),
    SetChannelContrastMax(usize, f32),
    SetChannelOpacity(usize, f32),
    SetZSlice(u32),
    SetTIndex(u32),
    CameraChanged(f32, f32, f32, f32, f32), // (pan_x, pan_y, zoom, canvas_w, canvas_h)
    TileLoaded {
        level: usize,
        channel: usize,
        tile_y: u32,
        tile_x: u32,
        data: Vec<f32>,
        w: u32,
        h: u32,
        generation: u64,
    },
    TileFailed { generation: u64, key: TileKey },
}

impl Component for App {
    type Message = AppMsg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        // Fetch dataset list and current dataset info on mount
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_datasets().await {
                Ok(list) => link.send_message(AppMsg::DatasetsLoaded(list)),
                Err(e) => log::warn!("No dataset list available: {}", e),
            }
        });
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_info().await {
                Ok(info) => link.send_message(AppMsg::DatasetLoaded(info)),
                Err(e) => link.send_message(AppMsg::LoadError(e)),
            }
        });

        Self {
            dataset: None,
            datasets: Vec::new(),
            current_dataset: None,
            channels: Vec::new(),
            z_slice: 0,
            t_index: 0,
            z_max: 1,
            t_max: 1,
            dtype_max: 255.0,
            canvas_state: None,
            error: None,
            current_level: 0,
            tile_generation: 0,
            tiles_pending: 0,
            tiles_in_flight: HashSet::new(),
            panel_visible: true,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            AppMsg::TogglePanel => {
                self.panel_visible = !self.panel_visible;
                true
            }
            AppMsg::DatasetsLoaded(list) => {
                self.datasets = list;
                true
            }
            AppMsg::DatasetSelected(name) => {
                let link = ctx.link().clone();
                let dataset_name = name.clone();
                self.current_dataset = Some(name);
                self.error = None;
                spawn_local(async move {
                    match api_client::open_dataset(&dataset_name).await {
                        Ok(info) => link.send_message(AppMsg::DatasetLoaded(info)),
                        Err(e) => link.send_message(AppMsg::LoadError(e)),
                    }
                });
                true
            }
            AppMsg::DatasetLoaded(info) => {
                // Clear tile cache and reset state for the new dataset
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        state.tile_cache.clear();
                        state.level_info.clear();
                        state.camera.x = 0.0;
                        state.camera.y = 0.0;
                        state.camera.zoom = 1.0;
                    }
                }
                self.tile_generation += 1;
                self.tiles_pending = 0;
                self.tiles_in_flight.clear();

                self.init_from_dataset(&info);
                self.dataset = Some(info);
                if self.canvas_state.is_some() {
                    self.load_tiles(ctx);
                }
                true
            }
            AppMsg::LoadError(e) => {
                self.error = Some(e);
                true
            }
            AppMsg::CanvasReady(state) => {
                self.canvas_state = Some(state);
                // If dataset is already loaded, load tiles
                if self.dataset.is_some() {
                    self.load_tiles(ctx);
                }
                false
            }
            AppMsg::SetChannelVisibility(idx, vis) => {
                if let Some(ch) = self.channels.get_mut(idx) {
                    ch.visible = vis;
                    ch.opacity = if vis { 1.0 } else { 0.0 };
                }
                self.load_tiles(ctx);
                true
            }
            AppMsg::SetChannelColor(idx, color) => {
                if let Some(ch) = self.channels.get_mut(idx) {
                    ch.color = color;
                }
                self.trigger_redraw();
                true
            }
            AppMsg::SetChannelContrastMin(idx, v) => {
                if let Some(ch) = self.channels.get_mut(idx) {
                    ch.contrast_min = v;
                }
                self.trigger_redraw();
                true
            }
            AppMsg::SetChannelContrastMax(idx, v) => {
                if let Some(ch) = self.channels.get_mut(idx) {
                    ch.contrast_max = v;
                }
                self.trigger_redraw();
                true
            }
            AppMsg::SetChannelOpacity(idx, v) => {
                if let Some(ch) = self.channels.get_mut(idx) {
                    ch.opacity = v;
                }
                self.trigger_redraw();
                true
            }
            AppMsg::SetZSlice(z) => {
                self.z_slice = z;
                self.load_tiles(ctx);
                true
            }
            AppMsg::SetTIndex(t) => {
                self.t_index = t;
                self.load_tiles(ctx);
                true
            }
            AppMsg::CameraChanged(pan_x, pan_y, zoom, canvas_w, canvas_h) => {
                let mut level_changed = false;
                if let Some(ref info) = self.dataset {
                    let new_level = self.pick_level(info, zoom, canvas_w, canvas_h);
                    if new_level != self.current_level {
                        self.current_level = new_level;
                        level_changed = true;
                    }
                }
                if level_changed {
                    self.load_tiles_fresh(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
                } else {
                    self.load_visible_tiles(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
                }
                level_changed
            }
            AppMsg::TileLoaded { level, channel, tile_y, tile_x, data, w, h, generation } => {
                if generation != self.tile_generation {
                    return false;
                }
                self.tiles_pending = self.tiles_pending.saturating_sub(1);
                self.tiles_in_flight.remove(&TileKey { level, tile_y, tile_x, channel });
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        match state.renderer.upload_tile(w, h, &data) {
                            Ok(tex) => {
                                let key = TileKey { level, tile_y, tile_x, channel };
                                state.tile_cache.insert(key, tex);
                            }
                            Err(e) => log::warn!("Upload tile: {}", e),
                        }
                    }
                }
                true
            }
            AppMsg::TileFailed { generation, key } => {
                if generation != self.tile_generation {
                    return false;
                }
                self.tiles_pending = self.tiles_pending.saturating_sub(1);
                self.tiles_in_flight.remove(&key);
                self.tiles_pending > 0
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        if self.dataset.is_none() && self.datasets.is_empty() {
            if let Some(ref error) = self.error {
                return html! {
                    <div class="loading">{format!("Error: {}", error)}</div>
                };
            }
            return html! {
                <div class="loading">{"Loading dataset..."}</div>
            };
        }

        let channel_infos: Vec<ChannelRenderInfo> = self
            .channels
            .iter()
            .map(|ch| ChannelRenderInfo {
                color: ch.color,
                contrast_min: ch.contrast_min,
                contrast_max: ch.contrast_max,
                opacity: if ch.visible { ch.opacity } else { 0.0 },
            })
            .collect();

        let on_canvas_ready = ctx.link().callback(AppMsg::CanvasReady);
        let on_camera_changed = ctx.link().callback(|(px, py, z, w, h): (f32, f32, f32, f32, f32)| AppMsg::CameraChanged(px, py, z, w, h));

        let panel_class = if self.panel_visible { "control-panel" } else { "control-panel hidden" };
        let toggle_label = if self.panel_visible { "\u{2715}" } else { "\u{2630}" }; // × or ☰

        html! {
            <div class="app-container">
                <ViewerCanvas
                    channel_info={channel_infos}
                    dtype_max={self.dtype_max}
                    on_canvas_ready={on_canvas_ready}
                    on_camera_changed={on_camera_changed}
                />
                <button class="panel-toggle" onclick={ctx.link().callback(|_| AppMsg::TogglePanel)}>
                    {toggle_label}
                </button>
                <div class={panel_class}>
                    if !self.datasets.is_empty() {
                        <div class="dataset-selector">
                            <h2>{"Dataset"}</h2>
                            <select onchange={ctx.link().callback(|e: Event| {
                                let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                                AppMsg::DatasetSelected(input.value())
                            })}>
                                <option value="" disabled=true selected={self.current_dataset.is_none()}>
                                    {"Select dataset..."}
                                </option>
                                { for self.datasets.iter().map(|name| {
                                    let selected = self.current_dataset.as_deref() == Some(name.as_str());
                                    html! {
                                        <option value={name.clone()} selected={selected}>{name}</option>
                                    }
                                })}
                            </select>
                        </div>
                    }
                    <h2>{"Channels"}</h2>
                    { for self.channels.iter().enumerate().map(|(i, ch)| {
                        let link = ctx.link();
                        html! {
                            <ChannelPanel
                                index={i}
                                label={ch.label.clone()}
                                visible={ch.visible}
                                color={ch.color}
                                contrast_min={ch.contrast_min}
                                contrast_max={ch.contrast_max}
                                contrast_limit={self.dtype_max}
                                opacity={ch.opacity}
                                on_visibility={link.callback(move |v| AppMsg::SetChannelVisibility(i, v))}
                                on_color={link.callback(move |c| AppMsg::SetChannelColor(i, c))}
                                on_contrast_min={link.callback(move |v| AppMsg::SetChannelContrastMin(i, v))}
                                on_contrast_max={link.callback(move |v| AppMsg::SetChannelContrastMax(i, v))}
                                on_opacity={link.callback(move |v| AppMsg::SetChannelOpacity(i, v))}
                            />
                        }
                    })}
                    <h3>{"Axes"}</h3>
                    <AxisSliders
                        z_max={self.z_max}
                        t_max={self.t_max}
                        z_current={self.z_slice}
                        t_current={self.t_index}
                        on_z_change={ctx.link().callback(AppMsg::SetZSlice)}
                        on_t_change={ctx.link().callback(AppMsg::SetTIndex)}
                    />
                    <div class="info-text">
                        if let Some(ref ds) = self.dataset {
                            <p>{format!("Level: {} / {}", self.current_level, ds.arrays.len() - 1)}</p>
                            if let Some(arr) = ds.arrays.get(self.current_level) {
                                <p>{format!("Shape: {:?}", arr.shape)}</p>
                                <p>{format!("Dtype: {}", arr.dtype)}</p>
                            }
                            <p>{self.full_size_label(ds)}</p>
                        }
                        <p>{self.tiles_status()}</p>
                    </div>
                </div>
            </div>
        }
    }
}

impl App {
    /// Format the tile cache status string for the info panel.
    fn tiles_status(&self) -> String {
        let (tile_size, cache_size) = self.canvas_state.as_ref()
            .and_then(|cs| cs.borrow().as_ref().map(|s| {
                let ts = s.level_info.get(&s.current_level).map(|li| li.tile_size);
                (ts, s.tile_cache.len())
            }))
            .unwrap_or((None, 0));
        let size_str = match tile_size {
            Some((tw, th)) => format!(" ({}x{})", tw as u32, th as u32),
            None => String::new(),
        };
        if self.tiles_pending > 0 {
            format!("Tiles{}: {} cached, {} pending", size_str, cache_size, self.tiles_pending)
        } else {
            format!("Tiles{}: {} cached", size_str, cache_size)
        }
    }

    /// Format the full-resolution image dimensions for display.
    fn full_size_label(&self, ds: &DatasetInfo) -> String {
        let axes = &ds.metadata.multiscales[0].axes;
        let full = &ds.arrays[0];
        let dims: Vec<String> = axes.iter().enumerate()
            .filter_map(|(i, axis)| full.shape.get(i).map(|s| format!("{}: {}", axis.name, s)))
            .collect();
        format!("Full size: {}", dims.join(" \u{00d7} "))
    }

    /// Pick the coarsest pyramid level where each level-pixel maps to at least 1 screen pixel.
    fn pick_level(&self, info: &DatasetInfo, zoom: f32, canvas_w: f32, canvas_h: f32) -> usize {
        let axes = &info.metadata.multiscales[0].axes;

        // Get full-res image dimensions
        let full = &info.arrays[0];
        let mut full_w = 1.0f32;
        let mut full_h = 1.0f32;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "x" => full_w = full.shape[i] as f32,
                "y" => full_h = full.shape[i] as f32,
                _ => {}
            }
        }

        // fit = zoom * min(canvas_w/full_w, canvas_h/full_h)
        // Each full-res pixel occupies `fit` screen pixels.
        // Each level-pixel corresponds to (full_dim / level_dim) full-res pixels,
        // so it occupies fit * (full_dim / level_dim) screen pixels.
        // We want min(fit * full_w/lw, fit * full_h/lh) >= 1.
        let fit = zoom * (canvas_w / full_w).min(canvas_h / full_h);

        // spp = screen pixels per level pixel. Fine levels have spp << 1 (wasteful),
        // coarse levels have spp >> 1 (pixelated). We want the coarsest level
        // where quality is still acceptable. Allow up to 2x undersampling (spp <= 2)
        // since linear filtering hides mild undersampling.
        let mut best = 0; // default to finest if none are coarse enough
        for (level, arr) in info.arrays.iter().enumerate() {
            let mut lw = 1.0f32;
            let mut lh = 1.0f32;
            for (i, axis) in axes.iter().enumerate() {
                match axis.name.as_str() {
                    "x" => lw = arr.shape[i] as f32,
                    "y" => lh = arr.shape[i] as f32,
                    _ => {}
                }
            }
            let spp = (fit * full_w / lw).min(fit * full_h / lh);
            if spp > 2.0 {
                break; // too coarse
            }
            best = level;
        }
        best
    }

    /// Initialize channels, axes, and level from dataset metadata.
    fn init_from_dataset(&mut self, info: &DatasetInfo) {
        let axes = &info.metadata.multiscales[0].axes;

        // Determine dtype_max from the first array's dtype
        if let Some(arr) = info.arrays.first() {
            self.dtype_max = match arr.dtype.as_str() {
                "uint8" => 255.0,
                "uint16" => 65535.0,
                "uint32" => 4294967295.0,
                "int8" => 127.0,
                "int16" => 32767.0,
                "float32" | "float64" => 1.0,
                _ => 255.0,
            };
        }

        // Find number of channels, z slices, time points from axes + shape
        let shape = &info.arrays[0].shape;
        let mut num_channels = 1u64;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "c" => num_channels = shape[i],
                "z" => self.z_max = shape[i] as u32,
                "t" => self.t_max = shape[i] as u32,
                _ => {}
            }
        }

        // Pick best initial resolution level (use highest resolution that fits reasonably)
        // For MVP, start with the lowest resolution level
        self.current_level = info.arrays.len().saturating_sub(1);

        // Initialize channel states from OMERO metadata or defaults
        self.channels.clear();
        for c in 0..num_channels {
            let (label, color, visible, window) =
                if let Some(ref omero) = info.metadata.omero {
                    if let Some(ch) = omero.channels.get(c as usize) {
                        let label = ch
                            .label
                            .clone()
                            .unwrap_or_else(|| format!("Ch {}", c));
                        let color = ch
                            .color
                            .as_ref()
                            .and_then(|hex| {
                                let hex = hex.trim_start_matches('#');
                                if hex.len() >= 6 {
                                    let r =
                                        u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
                                    let g =
                                        u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
                                    let b =
                                        u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
                                    Some([r, g, b])
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| channel_panel::default_color(c as usize));
                        let visible = ch.active;
                        let window = ch.window.as_ref().map(|w| (w.start as f32, w.end as f32));
                        (label, color, visible, window)
                    } else {
                        (
                            format!("Ch {}", c),
                            channel_panel::default_color(c as usize),
                            true,
                            None,
                        )
                    }
                } else {
                    (
                        format!("Ch {}", c),
                        channel_panel::default_color(c as usize),
                        c == 0,
                        None,
                    )
                };

            let (cmin, cmax) = window.unwrap_or((0.0, self.dtype_max));

            self.channels.push(ChannelUiState {
                label,
                visible,
                color,
                contrast_min: cmin,
                contrast_max: cmax,
                opacity: if visible { 1.0 } else { 0.0 },
            });
        }
    }

    /// Compute the level's tile grid info for a given level.
    fn level_tile_info(&self, info: &DatasetInfo, level: usize) -> Option<(u64, u64, u64, u64, u32, u32)> {
        let arr = info.arrays.get(level)?;
        let axes = &info.metadata.multiscales[0].axes;
        let mut img_w = 1u64;
        let mut img_h = 1u64;
        let mut chunk_w = 256u64;
        let mut chunk_h = 256u64;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "x" => {
                    img_w = arr.shape[i];
                    if let Some(&cw) = arr.chunks.get(i) { chunk_w = cw; }
                }
                "y" => {
                    img_h = arr.shape[i];
                    if let Some(&ch) = arr.chunks.get(i) { chunk_h = ch; }
                }
                _ => {}
            }
        }
        let tile_w = chunk_w.max(256).min(2048);
        let tile_h = chunk_h.max(256).min(2048);
        let num_tiles_x = ((img_w + tile_w - 1) / tile_w) as u32;
        let num_tiles_y = ((img_h + tile_h - 1) / tile_h) as u32;
        Some((img_w, img_h, tile_w, tile_h, num_tiles_x, num_tiles_y))
    }

    /// Compute which tile indices are visible in the viewport.
    fn visible_tile_range(
        &self,
        pan_x: f32, pan_y: f32, zoom: f32, canvas_w: f32, canvas_h: f32,
        img_w: f32, img_h: f32, tile_w: f32, tile_h: f32,
        num_tiles_x: u32, num_tiles_y: u32,
    ) -> (u32, u32, u32, u32) {
        // Reverse the vertex shader transform to find visible image pixel range.
        // fit = zoom * min(canvas_w/img_w, canvas_h/img_h)
        let fit = zoom * (canvas_w / img_w).min(canvas_h / img_h);
        let scale_x = fit * img_w / canvas_w;
        let scale_y = fit * img_h / canvas_h;

        // Screen edges in clip space are -1 and +1.
        // screen_pos = centered * scale + pan * 2 / canvas
        // centered = (screen_pos - pan * 2 / canvas) / scale
        // img_pos = centered / 2 + 0.5
        // pixel = img_pos * img_size
        let screen_to_pixel = |clip_x: f32, clip_y: f32| -> (f32, f32) {
            let cx = (clip_x - pan_x * 2.0 / canvas_w) / scale_x;
            let cy = (-clip_y - pan_y * 2.0 / canvas_h) / scale_y; // note: y is flipped in shader
            let px = (cx / 2.0 + 0.5) * img_w;
            let py = (cy / 2.0 + 0.5) * img_h;
            (px, py)
        };

        let (px0, py0) = screen_to_pixel(-1.0, -1.0);
        let (px1, py1) = screen_to_pixel(1.0, 1.0);

        let min_px = px0.min(px1).max(0.0);
        let max_px = px0.max(px1).min(img_w);
        let min_py = py0.min(py1).max(0.0);
        let max_py = py0.max(py1).min(img_h);

        let tx_min = (min_px / tile_w).floor().max(0.0) as u32;
        let tx_max = ((max_px / tile_w).ceil() as u32).min(num_tiles_x);
        let ty_min = (min_py / tile_h).floor().max(0.0) as u32;
        let ty_max = ((max_py / tile_h).ceil() as u32).min(num_tiles_y);

        (tx_min, tx_max, ty_min, ty_max)
    }

    /// Called from DatasetLoaded, CanvasReady, SetChannelVisibility, SetZSlice, SetTIndex
    /// when we don't yet have camera info from the canvas — reads it from canvas state.
    /// Called when we don't have camera params (e.g. channel toggle, z/t change).
    /// Reads camera from canvas state; skips if canvas not ready yet.
    fn load_tiles(&mut self, ctx: &Context<Self>) {
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
    /// Called on level/z/t/channel changes.
    fn load_tiles_fresh(&mut self, ctx: &Context<Self>,
        pan_x: f32, pan_y: f32, zoom: f32, canvas_w: f32, canvas_h: f32,
    ) {
        self.tile_generation += 1;
        self.tiles_pending = 0;
        self.tiles_in_flight.clear();
        self.load_visible_tiles(ctx, pan_x, pan_y, zoom, canvas_w, canvas_h);
    }

    /// Fetch tiles for the visible viewport. Does NOT bump generation — in-flight
    /// requests from earlier calls within the same generation remain valid.
    fn load_visible_tiles(
        &mut self, ctx: &Context<Self>,
        pan_x: f32, pan_y: f32, zoom: f32, canvas_w: f32, canvas_h: f32,
    ) {
        let info = match &self.dataset {
            Some(i) => i.clone(),
            None => return,
        };

        let level = self.current_level;
        let t = self.t_index as u64;
        let z = self.z_slice as u64;
        let gen = self.tile_generation;

        let (img_w, img_h, tile_w, tile_h, num_tiles_x, num_tiles_y) =
            match self.level_tile_info(&info, level) {
                Some(v) => v,
                None => return,
            };

        // Compute visible tile range
        let (tx_min, tx_max, ty_min, ty_max) = self.visible_tile_range(
            pan_x, pan_y, zoom, canvas_w, canvas_h,
            img_w as f32, img_h as f32, tile_w as f32, tile_h as f32,
            num_tiles_x, num_tiles_y,
        );

        // Update canvas state
        if let Some(cs) = &self.canvas_state {
            if let Some(ref mut state) = *cs.borrow_mut() {
                state.level_info.insert(level, LevelTileInfo {
                    image_size: (img_w as f32, img_h as f32),
                    tile_size: (tile_w as f32, tile_h as f32),
                    num_tiles_x,
                    num_tiles_y,
                });
                state.current_level = level;

                // Keep current level + coarser fallbacks; evict finer levels
                let keep_levels: HashSet<usize> = state.level_info.keys()
                    .copied()
                    .filter(|&l| l >= level)
                    .collect();
                state.tile_cache.retain(|k, _| keep_levels.contains(&k.level));
                state.level_info.retain(|l, _| keep_levels.contains(l));

                // Evict off-screen tiles from current level
                state.tile_cache.retain(|k, _| {
                    if k.level != level { return true; }
                    k.tile_x >= tx_min && k.tile_x < tx_max &&
                    k.tile_y >= ty_min && k.tile_y < ty_max
                });
            }
        }

        let visible_channels: Vec<usize> = self
            .channels
            .iter()
            .enumerate()
            .filter(|(_, ch)| ch.visible)
            .map(|(i, _)| i)
            .collect();

        // Only fetch visible tiles that aren't already cached
        for ty in ty_min..ty_max {
            for tx in tx_min..tx_max {
                let y_start = ty as u64 * tile_h;
                let x_start = tx as u64 * tile_w;
                let h = tile_h.min(img_h - y_start);
                let w = tile_w.min(img_w - x_start);

                for &ch_idx in &visible_channels {
                    let key = TileKey { level, tile_y: ty, tile_x: tx, channel: ch_idx };

                    // Skip if already cached or already in-flight
                    let already_cached = self.canvas_state.as_ref()
                        .and_then(|cs| cs.borrow().as_ref().map(|s| {
                            s.tile_cache.contains_key(&key)
                        }))
                        .unwrap_or(false);
                    if already_cached || self.tiles_in_flight.contains(&key) {
                        continue;
                    }

                    self.tiles_in_flight.insert(key);
                    self.tiles_pending += 1;
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        match api_client::fetch_tile(level, t, ch_idx as u64, z, y_start, x_start, h, w).await {
                            Ok(data) => {
                                link.send_message(AppMsg::TileLoaded {
                                    level,
                                    channel: ch_idx,
                                    tile_y: ty,
                                    tile_x: tx,
                                    data,
                                    w: w as u32,
                                    h: h as u32,
                                    generation: gen,
                                });
                            }
                            Err(_) => {
                                link.send_message(AppMsg::TileFailed { generation: gen, key });
                            }
                        }
                    });
                }
            }
        }
    }

    /// Signal a redraw by returning true from update (props change triggers ViewerCanvas::changed).
    fn trigger_redraw(&self) {
        // The redraw happens automatically via ViewerCanvas::changed()
        // since we return true from update which re-renders with new props
    }
}
