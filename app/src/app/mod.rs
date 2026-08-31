use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::SessionInfo;

use std::collections::HashMap;

mod annot_edit;
mod annot_store;
mod annot_style;
mod annotations;
mod channels;
mod edit;
mod labels;
mod layers_view;
mod objects;
mod session;
mod tables;
mod tiles;
mod undo;

use annot_edit::AnnotEditMsg;
use annot_store::AnnotStoreMsg;
use annot_style::AnnotStyleMsg;
use annotations::AnnotMsg;
use channels::ChannelMsg;
use labels::LabelMsg;
use objects::ObjectMsg;
use session::SessionMsg;
use tables::TableMsg;
use tiles::ViewMsg;
use undo::Undo;

use edit::{apply_edit, geometry_of, is_axis_aligned_rect};

use crate::api_client::{self};
use crate::layers::{LayerState, LayerUi, ObjectData};
use crate::viewer_canvas::{
    ChannelRenderInfo, LayerRenderInfo, LayerRenderKind, TileKey, Tool, ViewerCanvasState,
};
use crate::webgl::renderer::{
    Blend, FillRenderInfo, LabelRenderInfo, LineRenderInfo, PointRenderInfo,
};

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

/// One step back.
///
/// Inverses rather than snapshots: every annotation edit is exactly one API
/// call, so the undo of each is one too, and a stack of whole layers would grow
/// with the set rather than with the number of edits.
/// How many table rows a page holds.
const TABLE_PAGE: usize = 500;

/// How many steps back the viewer remembers.
///
/// Deep enough to cover a run of mistakes, shallow enough that the stack is not
/// a second copy of the layer.
const UNDO_DEPTH: usize = 100;

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
    /// What a drag on the canvas does.
    tool: Tool,
    /// Which annotation layer a new mark lands in. `None` while there is none,
    /// which is also what makes the point and box tools inert.
    annot_target: Option<String>,
    /// The name box for a new annotation layer.
    new_annot_name: String,
    /// The ROI tables the session's own store already holds, so opening one is
    /// a click rather than a path the user has to remember.
    tables: api_client::StoreTables,
    /// Annotation edits, most recent last.
    undo: Vec<Undo>,
    /// The `beforeunload` handler, alive only while there is unsaved work.
    /// Held because dropping a `Closure` unregisters it.
    unload_guard: Option<Closure<dyn Fn(web_sys::BeforeUnloadEvent) -> String>>,
    /// Whether the guard is currently installed, so `rendered` can tell a
    /// change from the steady state.
    guarding: bool,
    /// The Ctrl-Z listener, held for the same reason as the guard.
    undo_shortcut: Option<Closure<dyn Fn(web_sys::KeyboardEvent)>>,
}

pub enum AppMsg {
    /// The session: what is open, what to open next, and where it is saved.
    Session(SessionMsg),
    /// One image layer's channels: colour, contrast, opacity.
    Channel(ChannelMsg),
    /// Object (detection) layers: how they are drawn, and their rows.
    Object(ObjectMsg),
    /// Label layers, and what a click on the image lands on.
    Label(LabelMsg),
    /// What the camera shows, and the tiles that has to be fetched for it.
    View(ViewMsg),
    /// Feature and ROI tables: their rows, and painting labels by a column.
    Table(TableMsg),
    /// Drawing, editing, classifying and saving annotations.
    Annot(AnnotMsg),
    /// Changing an annotation that already exists.
    AnnotEdit(AnnotEditMsg),
    /// How an annotation layer is drawn.
    AnnotStyle(AnnotStyleMsg),
    /// Where a layer's annotations come from and go back to.
    AnnotStore(AnnotStoreMsg),
}

impl Component for App {
    type Message = AppMsg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_datasets().await {
                Ok(list) => link.send_message(SessionMsg::DatasetsLoaded(list)),
                Err(e) => log::warn!("No dataset list available: {}", e),
            }
        });
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_session().await {
                Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                Err(e) => link.send_message(SessionMsg::LoadError(e)),
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
            tool: Tool::Pan,
            annot_target: None,
            new_annot_name: String::new(),
            tables: api_client::StoreTables::default(),
            undo: Vec::new(),
            unload_guard: None,
            guarding: false,
            undo_shortcut: None,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            AppMsg::Session(msg) => self.update_session(ctx, msg),
            AppMsg::Channel(msg) => self.update_channels(ctx, msg),
            AppMsg::Object(msg) => self.update_objects(ctx, msg),
            AppMsg::Label(msg) => self.update_labels(ctx, msg),
            AppMsg::View(msg) => self.update_tiles(ctx, msg),
            AppMsg::Table(msg) => self.update_tables(ctx, msg),
            AppMsg::Annot(msg) => self.update_annotations(ctx, msg),
            AppMsg::AnnotEdit(msg) => self.update_annot_edit(ctx, msg),
            AppMsg::AnnotStyle(msg) => self.update_annot_style(ctx, msg),
            AppMsg::AnnotStore(msg) => self.update_annot_store(ctx, msg),
        }
    }

    /// Keep the browser's "leave site?" prompt in step with the dirty flag,
    /// and install the undo shortcut once.
    ///
    /// After every render rather than on the edit itself: `dirty` is set from
    /// half a dozen message arms, and the guard has to reflect all of them
    /// including the one that clears it.
    fn view(&self, ctx: &Context<Self>) -> Html {
        self.view_body(ctx)
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        let Some(window) = web_sys::window() else {
            return;
        };
        if first_render {
            // Ctrl/Cmd-Z, because a drawing tool without one is a tool people
            // are afraid to use.
            let link = ctx.link().clone();
            let undo = Closure::<dyn Fn(web_sys::KeyboardEvent)>::wrap(Box::new(
                move |e: web_sys::KeyboardEvent| {
                    if e.key() == "z" && (e.ctrl_key() || e.meta_key()) && !e.shift_key() {
                        e.prevent_default();
                        link.send_message(AnnotMsg::Undo);
                    }
                    // Escape abandons a half-drawn shape and drops back to pan,
                    // which is what every drawing tool does and what a person
                    // reaches for when a polygon has gone wrong.
                    if e.key() == "Escape" {
                        link.send_message(AnnotMsg::CancelDrawing);
                    }
                },
            ));
            let _ =
                window.add_event_listener_with_callback("keydown", undo.as_ref().unchecked_ref());
            self.undo_shortcut = Some(undo);
        }

        let unsaved = self.unsaved_annotations();
        if unsaved == self.guarding {
            return;
        }
        self.guarding = unsaved;
        if unsaved {
            // The handler's *return value* is what arms the prompt; browsers
            // ignore the string and show their own wording.
            let guard = Closure::<dyn Fn(web_sys::BeforeUnloadEvent) -> String>::wrap(Box::new(
                |e: web_sys::BeforeUnloadEvent| {
                    e.prevent_default();
                    e.set_return_value("Annotations have not been saved.");
                    "Annotations have not been saved.".to_string()
                },
            ));
            window.set_onbeforeunload(Some(guard.as_ref().unchecked_ref()));
            self.unload_guard = Some(guard);
        } else {
            window.set_onbeforeunload(None);
            self.unload_guard = None;
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
                match (&mut layer.ui, &old.ui) {
                    // An annotation layer is the one kind whose *content* comes
                    // back with the session, so carrying the old state wholesale
                    // would put the rows the client already had back over the
                    // rows the server just sent — which is exactly what an undo
                    // that reloads is trying to replace.
                    (LayerUi::Annotations(fresh), LayerUi::Annotations(old)) => {
                        fresh.keep_view_of(old)
                    }
                    (fresh, old) => *fresh = old.clone(),
                }
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
                    .point_buffers
                    .keys()
                    .chain(state.annot_buffers.keys())
                    .filter(|id| !live.contains(*id))
                    .cloned()
                    .collect();
                for id in dead {
                    if let Some(buffer) = state.point_buffers.remove(&id) {
                        state.renderer.delete_points(&buffer);
                    }
                    for batch in state.annot_buffers.remove(&id).into_iter().flatten() {
                        state.renderer.delete_points(&batch.points);
                        state.renderer.delete_lines(&batch.lines);
                        state.renderer.delete_fills(&batch.fills);
                    }
                }
                state.level_info.retain(|(id, _), _| live.contains(id));
                state.current_level.retain(|id, _| live.contains(id));
                state.world_size = self.world_size();
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
                    // A table has no geometry: it paints a *label* layer, and
                    // is otherwise invisible on the canvas.
                    LayerUi::Table(_) => LayerRenderKind::Annotations {
                        points: PointRenderInfo::default(),
                        lines: LineRenderInfo {
                            color: [0.0; 3],
                            opacity: 0.0,
                            z: 0.0,
                            slab: 0.0,
                        },
                        fills: FillRenderInfo {
                            color: [0.0; 3],
                            opacity: 0.0,
                            z: 0.0,
                            slab: 0.0,
                        },
                    },
                    LayerUi::Annotations(state) => LayerRenderKind::Annotations {
                        points: PointRenderInfo {
                            color: state.style.color,
                            opacity: state.style.opacity,
                            size: state.style.size,
                            color_by_value: false,
                            value_range: [0.0, 1.0],
                            // Rings, so a point marks a spot without hiding it.
                            hollow: true,
                            z: self.z_slice as f32,
                            slab: state.style.slab,
                            // The point shader matches on the `row` attribute,
                            // which carries the annotation id.
                            selected_row: state.selected.map(|id| id as f32).unwrap_or(-1.0),
                        },
                        lines: LineRenderInfo {
                            color: state.style.color,
                            opacity: state.style.opacity,
                            z: self.z_slice as f32,
                            slab: state.style.slab,
                        },
                        fills: FillRenderInfo {
                            color: state.style.color,
                            // A fill is translucent whatever the outline's
                            // opacity: it covers the pixels the shape was drawn
                            // around, and QuPath's fill is see-through for the
                            // same reason.
                            opacity: state.style.opacity * 0.3,
                            z: self.z_slice as f32,
                            slab: state.style.slab,
                        },
                    },
                    LayerUi::Objects(state) => LayerRenderKind::Objects(PointRenderInfo {
                        color: state.style.color,
                        opacity: state.style.opacity,
                        size: state.style.size,
                        color_by_value: state.color_by.is_some(),
                        value_range: state
                            .color_by
                            .and_then(|column| state.schema.columns.get(column))
                            .and_then(|column| column.range)
                            .map(|r| [r[0] as f32, r[1] as f32])
                            .unwrap_or([0.0, 1.0]),
                        hollow: state.hollow,
                        z: self.z_slice as f32,
                        slab: state.style.slab,
                        selected_row: state.selected_row.map(|row| row as f32).unwrap_or(-1.0),
                    }),
                },
            })
            .collect()
    }
}
