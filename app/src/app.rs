use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::DatasetInfo;

use crate::api_client;
use crate::controls::axis_sliders::AxisSliders;
use crate::controls::channel_panel::{self, ChannelPanel};
use crate::viewer_canvas::{ChannelRenderInfo, ViewerCanvas, ViewerCanvasState};

#[derive(Clone)]
struct ChannelUiState {
    label: String,
    visible: bool,
    color: [f32; 3],
    contrast_min: f32,
    contrast_max: f32,
    opacity: f32,
}

pub struct App {
    dataset: Option<DatasetInfo>,
    channels: Vec<ChannelUiState>,
    z_slice: u32,
    t_index: u32,
    z_max: u32,
    t_max: u32,
    dtype_max: f32,
    canvas_state: Option<Rc<RefCell<Option<ViewerCanvasState>>>>,
    error: Option<String>,
    current_level: usize,
}

pub enum AppMsg {
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
    TilesLoaded(Vec<(usize, Vec<f32>, u32, u32)>), // (channel_idx, data, w, h)
}

impl Component for App {
    type Message = AppMsg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        // Fetch dataset info on mount
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_info().await {
                Ok(info) => link.send_message(AppMsg::DatasetLoaded(info)),
                Err(e) => link.send_message(AppMsg::LoadError(e)),
            }
        });

        Self {
            dataset: None,
            channels: Vec::new(),
            z_slice: 0,
            t_index: 0,
            z_max: 1,
            t_max: 1,
            dtype_max: 255.0,
            canvas_state: None,
            error: None,
            current_level: 0,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            AppMsg::DatasetLoaded(info) => {
                self.init_from_dataset(&info);
                self.dataset = Some(info);
                // If canvas is already ready, load initial tiles
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
            AppMsg::TilesLoaded(tiles) => {
                self.upload_tiles(tiles);
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        if let Some(ref error) = self.error {
            return html! {
                <div class="loading">
                    {format!("Error: {}", error)}
                </div>
            };
        }

        if self.dataset.is_none() {
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

        html! {
            <div class="app-container">
                <ViewerCanvas
                    channel_info={channel_infos}
                    dtype_max={self.dtype_max}
                    on_canvas_ready={on_canvas_ready}
                />
                <div class="control-panel">
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
                    </div>
                </div>
            </div>
        }
    }
}

impl App {
    fn full_size_label(&self, ds: &DatasetInfo) -> String {
        let axes = &ds.metadata.multiscales[0].axes;
        let full = &ds.arrays[0];
        let dims: Vec<String> = axes.iter().enumerate()
            .filter_map(|(i, axis)| full.shape.get(i).map(|s| format!("{}: {}", axis.name, s)))
            .collect();
        format!("Full size: {}", dims.join(" \u{00d7} "))
    }

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
                            c == 0,
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

    fn load_tiles(&self, ctx: &Context<Self>) {
        let info = match &self.dataset {
            Some(i) => i.clone(),
            None => return,
        };

        let level = self.current_level;
        let t = self.t_index as u64;
        let z = self.z_slice as u64;

        // Get image dimensions at this level
        let arr = match info.arrays.get(level) {
            Some(a) => a,
            None => return,
        };
        let axes = &info.metadata.multiscales[0].axes;
        let mut img_w = 1u64;
        let mut img_h = 1u64;
        for (i, axis) in axes.iter().enumerate() {
            match axis.name.as_str() {
                "x" => img_w = arr.shape[i],
                "y" => img_h = arr.shape[i],
                _ => {}
            }
        }

        // Collect visible channels
        let visible_channels: Vec<usize> = self
            .channels
            .iter()
            .enumerate()
            .filter(|(_, ch)| ch.visible)
            .map(|(i, _)| i)
            .collect();

        let link = ctx.link().clone();

        spawn_local(async move {
            let mut tiles = Vec::new();
            for ch_idx in visible_channels {
                match api_client::fetch_tile(level, t, ch_idx as u64, z, 0, 0, img_h, img_w).await
                {
                    Ok(data) => {
                        tiles.push((ch_idx, data, img_w as u32, img_h as u32));
                    }
                    Err(e) => {
                        log::error!("Failed to load tile for channel {}: {}", ch_idx, e);
                    }
                }
            }
            link.send_message(AppMsg::TilesLoaded(tiles));
        });
    }

    fn upload_tiles(&mut self, tiles: Vec<(usize, Vec<f32>, u32, u32)>) {
        let canvas_state = match &self.canvas_state {
            Some(s) => s.clone(),
            None => return,
        };
        let mut state = canvas_state.borrow_mut();
        let state = match state.as_mut() {
            Some(s) => s,
            None => return,
        };

        // Clear old textures
        state.tile_textures.clear();
        state.tile_textures.resize_with(self.channels.len(), Vec::new);

        // Get image size from the dataset at current level
        if let Some(ref info) = self.dataset {
            let arr = &info.arrays[self.current_level];
            let axes = &info.metadata.multiscales[0].axes;
            let mut img_w = 1.0f32;
            let mut img_h = 1.0f32;
            for (i, axis) in axes.iter().enumerate() {
                match axis.name.as_str() {
                    "x" => img_w = arr.shape[i] as f32,
                    "y" => img_h = arr.shape[i] as f32,
                    _ => {}
                }
            }
            state.image_size = (img_w, img_h);
            state.tile_size = (img_w, img_h); // Single tile for now
        }

        for (ch_idx, data, w, h) in tiles {
            match state.renderer.upload_tile(w, h, &data) {
                Ok(tex) => {
                    if ch_idx < state.tile_textures.len() {
                        state.tile_textures[ch_idx].push(tex);
                    }
                }
                Err(e) => {
                    log::error!("Failed to upload texture for ch {}: {}", ch_idx, e);
                }
            }
        }
    }

    fn trigger_redraw(&self) {
        // The redraw happens automatically via ViewerCanvas::changed()
        // since we return true from update which re-renders with new props
    }
}
