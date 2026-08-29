use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::SessionInfo;

use std::collections::HashMap;

use crate::api_client::{self, ObjectBatch, ObjectRegion, TileAddress};
use crate::controls::axis_sliders::AxisSliders;
use crate::controls::channel_panel::ChannelPanel;
use crate::controls::label_panel::LabelPanel;
use crate::controls::object_panel::ObjectPanel;
use crate::layers::{LayerState, LayerUi, ObjectData};
use crate::ortho_pane::{OrthoLayer, OrthoPane};
use crate::viewer_canvas::{
    ChannelRenderInfo, LayerRenderInfo, LayerRenderKind, LevelTileInfo, TileKey, ViewerCanvas,
    ViewerCanvasState,
};
use crate::webgl::renderer::{Blend, LabelRenderInfo, PointRenderInfo};

/// The pixels of one loaded tile, in the form its layer is drawn from.
pub enum TilePayload {
    Intensity(Vec<f32>),
    Labels(Vec<u32>),
}

/// What the last click found.
pub struct Picked {
    pub layer_name: String,
    pub id: u64,
    /// The region the id names, when an atlas ontology is loaded.
    pub region: Option<String>,
    /// The same voxel as a float — the only reading a float array has.
    pub value: Option<f32>,
    pub dtype: String,
    pub world: (f32, f32),
}

/// Root Yew component managing the session, layers, tile loading, and layout.
pub struct App {
    layers: Vec<LayerState>,
    datasets: Vec<String>,
    current_dataset: Option<String>,
    z_slice: u32,
    t_index: u32,
    z_max: u32,
    t_max: u32,
    canvas_state: Option<Rc<RefCell<Option<ViewerCanvasState>>>>,
    error: Option<String>,
    tile_generation: u64,
    tiles_pending: u32,
    tiles_in_flight: HashSet<TileKey>,
    panel_visible: bool,
    picked: Option<Picked>,
    /// The rows each object layer currently holds, keyed by layer id.
    objects: HashMap<String, ObjectData>,
    /// The inspected row per object layer, rendered as text.
    inspected: HashMap<String, String>,
    add_source: String,
    add_role: String,
    /// Where the orthogonal panes are cut, in world pixels.
    crosshair: (f32, f32),
    /// Whether the orthogonal panes are shown at all.
    ortho: bool,
    /// `Some(("max"|"mean", depth))` to project the main view through z.
    projection: Option<(&'static str, u64)>,
    /// The per-region tally, once asked for.
    regions: Vec<api_client::RegionCount>,
    counting_regions: bool,
}

pub enum AppMsg {
    TogglePanel,
    DatasetsLoaded(Vec<String>),
    DatasetSelected(String),
    SessionLoaded(SessionInfo),
    LoadError(String),
    CanvasReady(Rc<RefCell<Option<ViewerCanvasState>>>),
    SetLayerVisible(usize, bool),
    RemoveLayer(String),
    SetAddSource(String),
    SetAddRole(String),
    SubmitAddLayer,
    SaveProject,
    /// Ask the desktop shell for a path: `pick_folder` or `pick_file`.
    Browse(&'static str),
    Browsed(String, bool),
    SetChannelVisibility(usize, usize, bool),
    SetChannelColor(usize, usize, [f32; 3]),
    SetChannelContrastMin(usize, usize, f32),
    SetChannelContrastMax(usize, usize, f32),
    SetChannelOpacity(usize, usize, f32),
    SetObjectColor(usize, [f32; 3]),
    SetObjectOpacity(usize, f32),
    SetObjectSize(usize, f32),
    SetObjectHollow(usize, bool),
    SetObjectColorBy(usize, Option<usize>),
    SetObjectFilter(usize, usize, Option<(f32, f32)>),
    SetObjectSlab(usize, f32),
    ObjectsLoaded {
        layer: String,
        batch: Box<ObjectBatch>,
        generation: u64,
    },
    ObjectInspected(String, Option<serde_json::Value>),
    SetLabelOpacity(usize, f32),
    SetLabelOutline(usize, bool),
    SetLabelOnlySelected(usize, bool),
    ClearLabelSelection(usize),
    Pick(f32, f32),
    Picked(String, api_client::VoxelValue, (f32, f32)),
    CountRegions,
    RegionsCounted(Vec<api_client::RegionCount>),
    ToggleOrtho,
    SetProjection(Option<&'static str>),
    SetProjectionDepth(u64),
    /// A click in an orthogonal pane, as fractions of the pane.
    OrthoPicked(&'static str, f32, f32),
    SetZSlice(u32),
    SetTIndex(u32),
    CameraChanged(f32, f32, f32, f32, f32), // (pan_x, pan_y, zoom, canvas_w, canvas_h)
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

impl Component for App {
    type Message = AppMsg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_datasets().await {
                Ok(list) => link.send_message(AppMsg::DatasetsLoaded(list)),
                Err(e) => log::warn!("No dataset list available: {}", e),
            }
        });
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_session().await {
                Ok(session) => link.send_message(AppMsg::SessionLoaded(session)),
                Err(e) => link.send_message(AppMsg::LoadError(e)),
            }
        });

        Self {
            layers: Vec::new(),
            datasets: Vec::new(),
            current_dataset: None,
            z_slice: 0,
            t_index: 0,
            z_max: 1,
            t_max: 1,
            canvas_state: None,
            error: None,
            tile_generation: 0,
            tiles_pending: 0,
            tiles_in_flight: HashSet::new(),
            panel_visible: true,
            picked: None,
            objects: HashMap::new(),
            inspected: HashMap::new(),
            add_source: String::new(),
            add_role: "auto".to_string(),
            crosshair: (0.0, 0.0),
            ortho: false,
            projection: None,
            regions: Vec::new(),
            counting_regions: false,
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
                        Ok(_) => match api_client::fetch_session().await {
                            Ok(session) => link.send_message(AppMsg::SessionLoaded(session)),
                            Err(e) => link.send_message(AppMsg::LoadError(e)),
                        },
                        Err(e) => link.send_message(AppMsg::LoadError(e)),
                    }
                });
                true
            }
            AppMsg::SessionLoaded(session) => {
                self.adopt_session(session);
                if self.canvas_state.is_some() {
                    self.install_label_luts();
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
                if !self.layers.is_empty() {
                    self.install_label_luts();
                    self.load_tiles(ctx);
                }
                false
            }
            AppMsg::SetLayerVisible(index, visible) => {
                if let Some(layer) = self.layers.get_mut(index) {
                    layer.visible = visible;
                }
                self.load_tiles(ctx);
                true
            }
            AppMsg::RemoveLayer(id) => {
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::remove_layer(&id).await {
                        Ok(session) => link.send_message(AppMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(AppMsg::LoadError(e)),
                    }
                });
                false
            }
            AppMsg::SetAddSource(source) => {
                self.add_source = source;
                false
            }
            AppMsg::SetAddRole(role) => {
                self.add_role = role;
                false
            }
            AppMsg::SubmitAddLayer => {
                let source = self.add_source.trim().to_string();
                if source.is_empty() {
                    return false;
                }
                let role = self.add_role.clone();
                let link = ctx.link().clone();
                self.error = None;
                spawn_local(async move {
                    let role = (role != "auto").then_some(role);
                    match api_client::add_layer(&source, role.as_deref()).await {
                        Ok(session) => link.send_message(AppMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(AppMsg::LoadError(e)),
                    }
                });
                true
            }
            AppMsg::Browse(command) => {
                let link = ctx.link().clone();
                let folder = command == "pick_folder";
                spawn_local(async move {
                    if let Some(path) = api_client::pick_path(command).await {
                        link.send_message(AppMsg::Browsed(path, folder));
                    }
                });
                false
            }
            AppMsg::Browsed(path, folder) => {
                self.add_source = path;
                // A folder is a run; a file is whatever its header says it is.
                self.add_role = if folder { "project" } else { "auto" }.to_string();
                true
            }
            AppMsg::SaveProject => {
                // The session as a project file, handed to the browser as a
                // download: what to keep is the user's decision, and the server
                // has no business writing files for a browser.
                spawn_local(async move {
                    if let Err(e) = crate::api_client::download_project().await {
                        log::warn!("save view: {}", e);
                    }
                });
                false
            }
            AppMsg::SetChannelVisibility(layer, channel, visible) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.visible = visible;
                    ch.opacity = if visible { 1.0 } else { 0.0 };
                }
                self.load_tiles(ctx);
                true
            }
            AppMsg::SetChannelColor(layer, channel, color) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.color = color;
                }
                true
            }
            AppMsg::SetChannelContrastMin(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.contrast_min = value;
                }
                true
            }
            AppMsg::SetChannelContrastMax(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.contrast_max = value;
                }
                true
            }
            AppMsg::SetChannelOpacity(layer, channel, value) => {
                if let Some(ch) = self.channel_mut(layer, channel) {
                    ch.opacity = value;
                }
                true
            }
            AppMsg::SetObjectColor(layer, color) => {
                if let Some(state) = self.object_mut(layer) {
                    state.color = color;
                }
                true
            }
            AppMsg::SetObjectOpacity(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.opacity = value;
                }
                true
            }
            AppMsg::SetObjectSize(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.size = value;
                }
                true
            }
            AppMsg::SetObjectHollow(layer, hollow) => {
                if let Some(state) = self.object_mut(layer) {
                    state.hollow = hollow;
                }
                true
            }
            AppMsg::SetObjectColorBy(layer, column) => {
                if let Some(state) = self.object_mut(layer) {
                    state.color_by = column;
                }
                self.rebuild_points(layer);
                true
            }
            AppMsg::SetObjectFilter(layer, column, filter) => {
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
            AppMsg::SetObjectSlab(layer, value) => {
                if let Some(state) = self.object_mut(layer) {
                    state.slab = value;
                }
                self.load_tiles(ctx);
                true
            }
            AppMsg::ObjectsLoaded {
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
            AppMsg::ObjectInspected(layer, row) => {
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
            AppMsg::SetLabelOpacity(layer, value) => {
                if let Some(state) = self.label_mut(layer) {
                    state.opacity = value;
                }
                true
            }
            AppMsg::SetLabelOutline(layer, outline) => {
                if let Some(state) = self.label_mut(layer) {
                    state.outline = outline;
                }
                true
            }
            AppMsg::SetLabelOnlySelected(layer, only) => {
                if let Some(state) = self.label_mut(layer) {
                    state.only_selected = only;
                }
                true
            }
            AppMsg::ClearLabelSelection(layer) => {
                if let Some(state) = self.label_mut(layer) {
                    state.selected = 0;
                    state.only_selected = false;
                }
                self.picked = None;
                true
            }
            AppMsg::Pick(world_x, world_y) => {
                self.pick_at(ctx, world_x, world_y);
                false
            }
            AppMsg::Picked(layer_id, voxel, world) => {
                let mut layer_name = layer_id.clone();
                if let Some(index) = self.layers.iter().position(|l| l.id == layer_id) {
                    layer_name = self.layers[index].name.clone();
                    if let Some(state) = self.label_mut(index) {
                        state.selected = voxel.id.unwrap_or(0) as u32;
                    }
                }
                let region = match (&voxel.name, &voxel.acronym) {
                    (Some(name), Some(acronym)) => Some(format!("{name} ({acronym})")),
                    (Some(name), None) => Some(name.clone()),
                    _ => None,
                };
                self.picked = Some(Picked {
                    layer_name,
                    region,
                    id: voxel.id.unwrap_or(0),
                    value: voxel.value,
                    dtype: voxel.dtype,
                    world,
                });
                true
            }
            AppMsg::CountRegions => {
                let (Some(labels), Some(objects)) = (
                    self.layers.iter().find(|l| l.is_labels()).map(|l| l.id.clone()),
                    self.layers.iter().find(|l| l.is_objects()).map(|l| l.id.clone()),
                ) else {
                    return false;
                };
                self.counting_regions = true;
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::fetch_regions(&labels, &objects, 25).await {
                        Ok(regions) => link.send_message(AppMsg::RegionsCounted(regions)),
                        Err(e) => {
                            log::warn!("regions: {}", e);
                            link.send_message(AppMsg::RegionsCounted(Vec::new()));
                        }
                    }
                });
                true
            }
            AppMsg::RegionsCounted(regions) => {
                self.regions = regions;
                self.counting_regions = false;
                true
            }
            AppMsg::ToggleOrtho => {
                self.ortho = !self.ortho;
                if self.ortho && self.crosshair == (0.0, 0.0) {
                    let world = self.world_size();
                    self.crosshair = (world.0 / 2.0, world.1 / 2.0);
                }
                true
            }
            AppMsg::SetProjection(kind) => {
                self.projection = kind.map(|kind| {
                    let depth = self.projection.map(|(_, depth)| depth).unwrap_or(8);
                    (kind, depth)
                });
                self.load_tiles(ctx);
                true
            }
            AppMsg::SetProjectionDepth(depth) => {
                if let Some((kind, _)) = self.projection {
                    self.projection = Some((kind, depth.max(1)));
                    self.load_tiles(ctx);
                }
                true
            }
            AppMsg::OrthoPicked(axis, u, v) => {
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
            AppMsg::TileLoaded {
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
        if self.layers.is_empty() && self.datasets.is_empty() {
            if let Some(ref error) = self.error {
                return html! {
                    <div class="loading">{format!("Error: {}", error)}</div>
                };
            }
            return html! {
                <div class="loading">{"Loading dataset..."}</div>
            };
        }

        let on_canvas_ready = ctx.link().callback(AppMsg::CanvasReady);
        let on_camera_changed = ctx
            .link()
            .callback(|(px, py, z, w, h): (f32, f32, f32, f32, f32)| {
                AppMsg::CameraChanged(px, py, z, w, h)
            });
        let on_pick = ctx
            .link()
            .callback(|(x, y): (f32, f32)| AppMsg::Pick(x, y));

        let panel_class = if self.panel_visible {
            "control-panel"
        } else {
            "control-panel hidden"
        };
        let toggle_label = if self.panel_visible {
            "\u{2715}"
        } else {
            "\u{2630}"
        };

        let world = self.world_size();
        let crosshair = (
            (self.crosshair.0 / world.0.max(1.0)).clamp(0.0, 1.0),
            (self.crosshair.1 / world.1.max(1.0)).clamp(0.0, 1.0),
        );
        let z_fraction = if self.z_max > 1 {
            self.z_slice as f32 / (self.z_max - 1) as f32
        } else {
            0.0
        };

        html! {
            <div class="app-container">
                <div class="viewer-area">
                    <div class="viewer-row">
                        <div class="viewer-main">
                            <ViewerCanvas
                                layers={self.render_infos()}
                                world_size={world}
                                on_canvas_ready={on_canvas_ready}
                                on_camera_changed={on_camera_changed}
                                on_pick={on_pick}
                            />
                            if self.ortho {
                                <div class="crosshair-v" style={format!("left: {}%", crosshair.0 * 100.0)} />
                                <div class="crosshair-h" style={format!("top: {}%", crosshair.1 * 100.0)} />
                            }
                        </div>
                        if self.ortho {
                            <OrthoPane
                                axis="x"
                                transpose={true}
                                t={self.t_index as u64}
                                layers={self.ortho_layers("x")}
                                crosshair={(z_fraction, crosshair.1)}
                                on_pick={ctx.link().callback(|(u, v): (f32, f32)| AppMsg::OrthoPicked("x", u, v))}
                            />
                        }
                    </div>
                    if self.ortho {
                        <OrthoPane
                            axis="y"
                            transpose={false}
                            t={self.t_index as u64}
                            layers={self.ortho_layers("y")}
                            crosshair={(crosshair.0, z_fraction)}
                            on_pick={ctx.link().callback(|(u, v): (f32, f32)| AppMsg::OrthoPicked("y", u, v))}
                        />
                    }
                </div>
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
                    { self.view_layers(ctx) }
                    { self.view_add_layer(ctx) }
                    { self.view_regions(ctx) }
                    <h3>{"View"}</h3>
                    <div class="slider-row">
                        <label>
                            <input type="checkbox" checked={self.ortho}
                                onchange={ctx.link().callback(|_| AppMsg::ToggleOrtho)} />
                            {" Orthogonal panes"}
                        </label>
                    </div>
                    <div class="slider-row">
                        <span>{"Z project"}</span>
                        <select onchange={ctx.link().callback(|e: Event| {
                            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                            AppMsg::SetProjection(match input.value().as_str() {
                                "max" => Some("max"),
                                "mean" => Some("mean"),
                                _ => None,
                            })
                        })}>
                            <option value="" selected={self.projection.is_none()}>{"(slice)"}</option>
                            <option value="max" selected={matches!(self.projection, Some(("max", _)))}>{"max"}</option>
                            <option value="mean" selected={matches!(self.projection, Some(("mean", _)))}>{"mean"}</option>
                        </select>
                    </div>
                    if let Some((_, depth)) = self.projection {
                        <div class="slider-row">
                            <span>{"Depth"}</span>
                            <input type="range" min="1" max="64" step="1" value={depth.to_string()}
                                oninput={ctx.link().callback(|e: InputEvent| {
                                    let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                                    AppMsg::SetProjectionDepth(input.value().parse().unwrap_or(1))
                                })} />
                            <span class="slider-value">{format!("{depth} planes")}</span>
                        </div>
                    }
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
                        { self.view_status() }
                    </div>
                </div>
            </div>
        }
    }
}

impl App {
    /// Replace the layer list with the session the server reported, keeping
    /// per-layer UI state for layers that are still open.
    fn adopt_session(&mut self, session: SessionInfo) {
        let previous = std::mem::take(&mut self.layers);
        let mut layers = Vec::new();
        for info in &session.layers {
            let Some(mut layer) = LayerState::from_info(info) else {
                log::info!("layer {} is a kind this build cannot draw yet", info.id);
                continue;
            };
            if let Some(old) = previous.iter().find(|old| old.id == layer.id) {
                // Same layer, same session: keep what the user set on it.
                layer.visible = old.visible;
                layer.ui = old.ui.clone();
            }
            layers.push(layer);
        }
        self.layers = layers;

        let reference = self.reference_layer().cloned();
        if let Some(reference) = reference {
            self.z_max = reference.axis_len("z").max(1) as u32;
            self.t_max = reference.axis_len("t").max(1) as u32;
            self.z_slice = self.z_slice.min(self.z_max.saturating_sub(1));
            self.t_index = self.t_index.min(self.t_max.saturating_sub(1));
        }

        // Everything on the GPU refers to layers that may be gone.
        self.tile_generation += 1;
        self.tiles_pending = 0;
        self.tiles_in_flight.clear();
        self.picked = None;
        let live: std::collections::HashSet<String> =
            self.layers.iter().map(|l| l.id.clone()).collect();
        self.objects.retain(|id, _| live.contains(id));
        self.inspected.retain(|id, _| live.contains(id));
        if let Some(cs) = &self.canvas_state {
            if let Some(ref mut state) = *cs.borrow_mut() {
                let live: HashSet<String> = live.clone();
                state.tile_cache.retain(|key, _| live.contains(&key.layer));
                let dead: Vec<String> = state
                    .object_buffers
                    .keys()
                    .filter(|id| !live.contains(*id))
                    .cloned()
                    .collect();
                for id in dead {
                    if let Some(buffer) = state.object_buffers.remove(&id) {
                        state.renderer.delete_points(&buffer);
                    }
                }
                state.level_info.retain(|(id, _), _| live.contains(id));
                state.current_level.retain(|id, _| live.contains(id));
                state.world_size = self.world_size();
            }
        }
    }

    /// Upload the colour table for every label layer that has one.
    fn install_label_luts(&self) {
        let Some(cs) = &self.canvas_state else {
            return;
        };
        let Some(ref mut state) = *cs.borrow_mut() else {
            return;
        };
        for layer in &self.layers {
            if state.renderer.has_label_lut(&layer.id) {
                continue;
            }
            if let Some(lut) = layer.label_lut() {
                if let Err(e) = state.renderer.set_label_lut(&layer.id, &lut) {
                    log::warn!("label LUT for {}: {}", layer.id, e);
                }
            }
        }
    }

    /// The layer whose coordinates everything else is drawn in: the first
    /// image layer, or the first layer when there is none.
    fn reference_layer(&self) -> Option<&LayerState> {
        self.layers
            .iter()
            .find(|layer| !layer.is_labels())
            .or_else(|| self.layers.first())
    }

    fn world_size(&self) -> (f32, f32) {
        self.reference_layer()
            .map(|layer| layer.world_size())
            .unwrap_or((1.0, 1.0))
    }

    fn channel_mut(
        &mut self,
        layer: usize,
        channel: usize,
    ) -> Option<&mut crate::layers::ChannelUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Image { channels, .. } => channels.get_mut(channel),
            _ => None,
        }
    }

    fn label_mut(&mut self, layer: usize) -> Option<&mut crate::layers::LabelUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Labels(state) => Some(state),
            _ => None,
        }
    }

    fn object_mut(&mut self, layer: usize) -> Option<&mut crate::layers::ObjectUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Objects(state) => Some(state),
            _ => None,
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
                if let Some(old) = canvas.object_buffers.remove(&id) {
                    canvas.renderer.delete_points(&old);
                }
                match canvas.renderer.upload_points(&vertices) {
                    Ok(buffer) => {
                        canvas.object_buffers.insert(id, buffer);
                    }
                    Err(e) => log::warn!("upload points: {}", e),
                }
            }
        }
        if let Some(state) = self.object_mut(index) {
            state.shown = shown;
        }
    }

    /// The render parameters handed to the canvas, in draw order.
    fn render_infos(&self) -> Vec<LayerRenderInfo> {
        // The bottom-most visible image layer replaces the background; every
        // image layer above it adds, so a mask drawn over a stain lights it up
        // rather than hiding it.
        let base = self
            .layers
            .iter()
            .position(|layer| layer.visible && matches!(layer.ui, LayerUi::Image { .. }));
        self.layers
            .iter()
            .enumerate()
            .map(|(index, layer)| LayerRenderInfo {
                id: layer.id.clone(),
                visible: layer.visible,
                kind: match &layer.ui {
                    LayerUi::Image {
                        channels,
                        dtype_max,
                    } => LayerRenderKind::Image {
                        blend: if base == Some(index) {
                            Blend::Over
                        } else {
                            Blend::Add
                        },
                        channels: channels
                            .iter()
                            .map(|ch| ChannelRenderInfo {
                                color: ch.color,
                                contrast_min: ch.contrast_min,
                                contrast_max: ch.contrast_max,
                                opacity: if ch.visible { ch.opacity } else { 0.0 },
                            })
                            .collect(),
                        dtype_max: *dtype_max,
                    },
                    LayerUi::Labels(state) => LayerRenderKind::Labels(LabelRenderInfo {
                        opacity: state.opacity,
                        outline: state.outline,
                        selected: state.selected,
                        only_selected: state.only_selected,
                    }),
                    LayerUi::Objects(state) => LayerRenderKind::Objects(PointRenderInfo {
                        color: state.color,
                        opacity: state.opacity,
                        size: state.size,
                        color_by_value: state.color_by.is_some(),
                        value_range: state
                            .color_by
                            .and_then(|column| state.schema.columns.get(column))
                            .and_then(|column| column.range)
                            .map(|r| [r[0] as f32, r[1] as f32])
                            .unwrap_or([0.0, 1.0]),
                        hollow: state.hollow,
                        z: self.z_slice as f32,
                        slab: state.slab,
                        selected_row: state
                            .selected_row
                            .map(|row| row as f32)
                            .unwrap_or(-1.0),
                    }),
                },
            })
            .collect()
    }

    /// Ask the server what id is under a click, on the topmost visible label
    /// layer. Reading it from the array rather than from the framebuffer keeps
    /// every label tile out of client memory.
    fn pick_at(&mut self, ctx: &Context<Self>, world_x: f32, world_y: f32) {
        let world = self.world_size();
        // A click is also where the orthogonal panes are cut, so the three
        // views always agree about which voxel is being looked at.
        self.crosshair = (world_x, world_y);

        // Objects sit on top of everything, so a click lands on one first.
        if let Some(layer) = self
            .layers
            .iter()
            .rev()
            .find(|layer| layer.is_objects() && layer.visible)
        {
            let id = layer.id.clone();
            let z = self.z_slice as f32;
            // A generous radius in world pixels: the sprite is drawn in screen
            // pixels, so what looks like a hit depends on the zoom.
            let radius = (14.0 / self.zoom().max(0.01)).clamp(2.0, 512.0);
            let link = ctx.link().clone();
            spawn_local(async move {
                match api_client::fetch_object_at(&id, z, world_y, world_x, radius).await {
                    Ok(row) => link.send_message(AppMsg::ObjectInspected(id, row)),
                    Err(e) => log::warn!("pick object: {}", e),
                }
            });
            return;
        }

        let Some(layer) = self
            .layers
            .iter()
            .rev()
            .find(|layer| layer.is_labels() && layer.visible)
        else {
            return;
        };

        let level = self.level_of(layer);
        let (sx, sy) = layer.level_to_world(level, world);
        if sx <= 0.0 || sy <= 0.0 {
            return;
        }
        let x = (world_x / sx).floor();
        let y = (world_y / sy).floor();
        let Some((lw, lh)) = layer.level_size(level) else {
            return;
        };
        if x < 0.0 || y < 0.0 || x >= lw || y >= lh {
            return;
        }

        let id = layer.id.clone();
        let (z, t) = (self.layer_z(layer, level), self.layer_t(layer));
        let link = ctx.link().clone();
        let world_point = (world_x, world_y);
        spawn_local(async move {
            match api_client::fetch_value(&id, level, t, 0, z, y as u64, x as u64).await {
                Ok(voxel) => link.send_message(AppMsg::Picked(id, voxel, world_point)),
                Err(e) => log::warn!("pick: {}", e),
            }
        });
    }

    /// What the orthogonal panes draw: every visible image layer's visible
    /// channels, at a level whose plane fits a pane without a second tile grid.
    fn ortho_layers(&self, axis: &str) -> Vec<OrthoLayer> {
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

    /// The camera's current zoom, or 1 before the canvas exists.
    fn zoom(&self) -> f32 {
        self.canvas_state
            .as_ref()
            .and_then(|cs| cs.borrow().as_ref().map(|state| state.camera.zoom))
            .unwrap_or(1.0)
    }

    /// The level a layer is currently drawn at.
    fn level_of(&self, layer: &LayerState) -> usize {
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
    fn layer_z(&self, layer: &LayerState, level: usize) -> u64 {
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

    fn layer_t(&self, layer: &LayerState) -> u64 {
        let layer_t = layer.axis_len("t").max(1);
        (self.t_index as u64).min(layer_t - 1)
    }

    /// Reload from whatever camera the canvas currently has.
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

            if let Some(cs) = &self.canvas_state {
                if let Some(ref mut state) = *cs.borrow_mut() {
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

                    // Keep this level and coarser ones as fallback coverage;
                    // finer levels are dead weight once the camera moved out.
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
            }

            let z = self.layer_z(layer, level);
            let t = self.layer_t(layer);
            let is_labels_layer = layer.is_labels();
            let projection = self.projection;
            let channels: Vec<usize> = match &layer.ui {
                LayerUi::Image { channels, .. } => channels
                    .iter()
                    .enumerate()
                    .filter(|(_, ch)| ch.visible)
                    .map(|(i, _)| i)
                    .collect(),
                LayerUi::Labels(_) => vec![0],
                // Object layers never reach here; they are answered above.
                LayerUi::Objects(_) => continue,
            };

            for ty in ty_min..ty_max {
                for tx in tx_min..tx_max {
                    let y_start = ty as u64 * grid.tile_h;
                    let x_start = tx as u64 * grid.tile_w;
                    let h = grid.tile_h.min(grid.img_h.saturating_sub(y_start));
                    let w = grid.tile_w.min(grid.img_w.saturating_sub(x_start));
                    if h == 0 || w == 0 {
                        continue;
                    }

                    for &channel in &channels {
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
                                Ok(data) => link.send_message(AppMsg::TileLoaded {
                                    key,
                                    data,
                                    w: w as u32,
                                    h: h as u32,
                                    generation,
                                }),
                                Err(e) => {
                                    log::warn!("tile: {}", e);
                                    link.send_message(AppMsg::TileFailed { generation, key });
                                }
                            }
                        });
                    }
                }
            }
        }
    }

    /// Fetch the rows of one object layer for the visible rectangle.
    ///
    /// Every column comes back, not just the coloured one: filtering and
    /// colour-by then cost a buffer rebuild rather than a round trip, and a
    /// handful of `f32` columns over at most `MAX_OBJECTS` rows is a few
    /// megabytes.
    fn load_objects(
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
                Ok(batch) => link.send_message(AppMsg::ObjectsLoaded {
                    layer: id,
                    batch: Box::new(batch),
                    generation,
                }),
                Err(e) => {
                    log::warn!("objects: {}", e);
                    link.send_message(AppMsg::ObjectsLoaded {
                        layer: id,
                        batch: Box::default(),
                        generation,
                    });
                }
            }
        });
    }

    fn view_layers(&self, ctx: &Context<Self>) -> Html {
        html! {
            <>
            { for self.layers.iter().enumerate().map(|(index, layer)| {
                let link = ctx.link();
                let id = layer.id.clone();
                match &layer.ui {
                    LayerUi::Image { channels, dtype_max } => html! {
                        <div class="layer-block">
                            <h2>
                                <label>
                                    <input type="checkbox" checked={layer.visible}
                                        onchange={link.callback(move |e: Event| {
                                            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                                            AppMsg::SetLayerVisible(index, input.checked())
                                        })} />
                                    { format!(" {}", layer.name) }
                                </label>
                                if self.layers.len() > 1 {
                                    <button class="layer-remove"
                                        onclick={link.callback(move |_| AppMsg::RemoveLayer(id.clone()))}
                                        title="Close layer">{"\u{2715}"}</button>
                                }
                            </h2>
                            { for channels.iter().enumerate().map(|(c, ch)| html! {
                                <ChannelPanel
                                    index={c}
                                    label={ch.label.clone()}
                                    visible={ch.visible}
                                    color={ch.color}
                                    contrast_min={ch.contrast_min}
                                    contrast_max={ch.contrast_max}
                                    contrast_limit={*dtype_max}
                                    opacity={ch.opacity}
                                    on_visibility={link.callback(move |v| AppMsg::SetChannelVisibility(index, c, v))}
                                    on_color={link.callback(move |v| AppMsg::SetChannelColor(index, c, v))}
                                    on_contrast_min={link.callback(move |v| AppMsg::SetChannelContrastMin(index, c, v))}
                                    on_contrast_max={link.callback(move |v| AppMsg::SetChannelContrastMax(index, c, v))}
                                    on_opacity={link.callback(move |v| AppMsg::SetChannelOpacity(index, c, v))}
                                />
                            })}
                        </div>
                    },
                    LayerUi::Objects(state) => html! {
                        <div class="layer-block">
                            <ObjectPanel
                                name={layer.name.clone()}
                                visible={layer.visible}
                                schema={state.schema.clone()}
                                count={state.count}
                                color={state.color}
                                opacity={state.opacity}
                                size={state.size}
                                hollow={state.hollow}
                                color_by={state.color_by}
                                filters={state.filters.clone()}
                                slab={state.slab}
                                loaded={state.loaded}
                                shown={state.shown}
                                total={state.total}
                                selected={self.inspected.get(&layer.id).cloned()}
                                on_visibility={link.callback(move |v| AppMsg::SetLayerVisible(index, v))}
                                on_color={link.callback(move |v| AppMsg::SetObjectColor(index, v))}
                                on_opacity={link.callback(move |v| AppMsg::SetObjectOpacity(index, v))}
                                on_size={link.callback(move |v| AppMsg::SetObjectSize(index, v))}
                                on_hollow={link.callback(move |v| AppMsg::SetObjectHollow(index, v))}
                                on_color_by={link.callback(move |v| AppMsg::SetObjectColorBy(index, v))}
                                on_filter={link.callback(move |(column, filter)| AppMsg::SetObjectFilter(index, column, filter))}
                                on_slab={link.callback(move |v| AppMsg::SetObjectSlab(index, v))}
                                on_remove={link.callback(move |_| AppMsg::RemoveLayer(id.clone()))}
                            />
                        </div>
                    },
                    LayerUi::Labels(state) => html! {
                        <div class="layer-block">
                            <LabelPanel
                                name={layer.name.clone()}
                                visible={layer.visible}
                                opacity={state.opacity}
                                outline={state.outline}
                                selected={state.selected}
                                only_selected={state.only_selected}
                                has_lut={layer.label_lut().is_some()}
                                on_visibility={link.callback(move |v| AppMsg::SetLayerVisible(index, v))}
                                on_opacity={link.callback(move |v| AppMsg::SetLabelOpacity(index, v))}
                                on_outline={link.callback(move |v| AppMsg::SetLabelOutline(index, v))}
                                on_only_selected={link.callback(move |v| AppMsg::SetLabelOnlySelected(index, v))}
                                on_clear_selection={link.callback(move |_| AppMsg::ClearLabelSelection(index))}
                                on_remove={link.callback(move |_| AppMsg::RemoveLayer(id.clone()))}
                            />
                        </div>
                    },
                }
            })}
            </>
        }
    }

    /// The per-region tally, when there is a label layer and an object layer
    /// to join.
    fn view_regions(&self, ctx: &Context<Self>) -> Html {
        if !(self.layers.iter().any(|l| l.is_labels()) && self.layers.iter().any(|l| l.is_objects()))
        {
            return html! {};
        }
        html! {
            <div class="regions">
                <h3>{"Regions"}</h3>
                <div class="slider-row">
                    <button onclick={ctx.link().callback(|_| AppMsg::CountRegions)}>
                        { if self.counting_regions { "Counting\u{2026}" } else { "Count objects per region" } }
                    </button>
                </div>
                if !self.regions.is_empty() {
                    <table class="region-table">
                        { for self.regions.iter().map(|region| html! {
                            <tr>
                                <td>{ region.acronym.clone()
                                        .or_else(|| region.name.clone())
                                        .unwrap_or_else(|| format!("id {}", region.id)) }</td>
                                <td class="region-count">{ region.count }</td>
                            </tr>
                        })}
                    </table>
                }
            </div>
        }
    }

    fn view_add_layer(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        html! {
            <div class="add-layer">
                <h3>{"Add layer"}</h3>
                <input type="text" placeholder="/path/to/labels.zarr or s3://bucket/key"
                    value={self.add_source.clone()}
                    oninput={link.callback(|e: InputEvent| {
                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                        AppMsg::SetAddSource(input.value())
                    })} />
                <div class="slider-row">
                    <select onchange={link.callback(|e: Event| {
                        let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                        AppMsg::SetAddRole(input.value())
                    })}>
                        <option value="auto" selected={self.add_role == "auto"}>{"auto"}</option>
                        <option value="image" selected={self.add_role == "image"}>{"image"}</option>
                        <option value="labels" selected={self.add_role == "labels"}>{"labels"}</option>
                        <option value="objects" selected={self.add_role == "objects"}>{"objects"}</option>
                        <option value="project" selected={self.add_role == "project"}>{"run folder"}</option>
                    </select>
                    <button onclick={link.callback(|_| AppMsg::SubmitAddLayer)}>{"Open"}</button>
                    <button onclick={link.callback(|_| AppMsg::SaveProject)}>{"Save view"}</button>
                </div>
                if api_client::is_desktop() {
                    <div class="slider-row">
                        <button onclick={link.callback(|_| AppMsg::Browse("pick_folder"))}>{"Browse run\u{2026}"}</button>
                        <button onclick={link.callback(|_| AppMsg::Browse("pick_file"))}>{"Browse file\u{2026}"}</button>
                    </div>
                }
                if let Some(ref error) = self.error {
                    <p class="error-text">{error}</p>
                }
            </div>
        }
    }

    fn view_status(&self) -> Html {
        let cached = self
            .canvas_state
            .as_ref()
            .and_then(|cs| cs.borrow().as_ref().map(|s| s.tile_cache.len()))
            .unwrap_or(0);
        let world = self.world_size();
        html! {
            <>
                <p>{format!("World: {} \u{00d7} {} px", world.0 as u64, world.1 as u64)}</p>
                { for self.layers.iter().map(|layer| {
                    let level = self.level_of(layer);
                    html!{ <p>{format!("{}: level {} / {}", layer.name, level, layer.num_levels().saturating_sub(1))}</p> }
                })}
                if self.tiles_pending > 0 {
                    <p>{format!("Tiles: {} cached, {} pending", cached, self.tiles_pending)}</p>
                } else {
                    <p>{format!("Tiles: {} cached", cached)}</p>
                }
                if let Some(ref picked) = self.picked {
                    <p>{format!("{}: id {} ({}) at ({:.0}, {:.0})",
                        picked.layer_name, picked.id, picked.dtype,
                        picked.world.0, picked.world.1)}</p>
                    if let Some(region) = &picked.region {
                        <p>{region.clone()}</p>
                    }
                    if let Some(value) = picked.value {
                        <p>{format!("value {}", value)}</p>
                    }
                }
            </>
        }
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

/// One inspected row, as a line of text: `id 4 · size 91 · confidence 0.87`.
fn describe_row(row: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    if let (Some(y), Some(x)) = (row.get("y").and_then(|v| v.as_f64()), row.get("x").and_then(|v| v.as_f64())) {
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
