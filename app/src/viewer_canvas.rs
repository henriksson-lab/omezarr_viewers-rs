use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::webgl::context::GlContext;
use crate::webgl::renderer::{
    Blend, LabelRenderInfo, PointBuffer, PointRenderInfo, Renderer, TextureKind, TilePlacement,
    TileTexture,
};

/// Per-channel rendering parameters passed as props to the canvas.
#[derive(Clone, PartialEq)]
pub struct ChannelRenderInfo {
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub opacity: f32,
}

/// How one layer is drawn, and with which program.
#[derive(Clone, PartialEq)]
pub enum LayerRenderKind {
    Image {
        channels: Vec<ChannelRenderInfo>,
        dtype_max: f32,
        /// How this layer meets the layers under it.
        blend: Blend,
    },
    Labels(LabelRenderInfo),
    Objects(PointRenderInfo),
}

/// One layer's rendering parameters, in draw order.
#[derive(Clone, PartialEq)]
pub struct LayerRenderInfo {
    pub id: String,
    pub visible: bool,
    pub kind: LayerRenderKind,
}

/// 2D camera state: pan position, zoom level, and drag tracking.
pub struct Camera2d {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
    pub dragging: bool,
    /// Set once a drag actually moves, so a click can be told from a pan.
    pub dragged: bool,
    pub last_mouse: (f32, f32),
    pub pinch_dist: Option<f32>,
    pub pinch_center: (f32, f32),
}

/// Cache key identifying a tile by layer, pyramid level, grid position, and channel.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct TileKey {
    pub layer: String,
    pub level: usize,
    pub tile_y: u32,
    pub tile_x: u32,
    /// The channel, for image layers; 0 for label layers, which have one.
    pub channel: usize,
}

/// Tile grid metadata for one layer at one pyramid level.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelTileInfo {
    /// Size of this level in the layer's own pixels.
    pub level_size: (f32, f32),
    /// Tile size in the layer's own pixels.
    pub tile_size: (f32, f32),
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
    /// Multiply a layer pixel by this to get a world pixel.
    ///
    /// This is what lets a half-resolution label volume land on top of the
    /// image it describes rather than in a quarter of it.
    pub world_scale: (f32, f32),
}

/// Shared mutable state between App (tile uploads) and ViewerCanvas (rendering).
pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_cache: HashMap<TileKey, TileTexture>,
    /// Grid metadata per `(layer id, level)`.
    pub level_info: HashMap<(String, usize), LevelTileInfo>,
    /// The level each layer is currently drawing at.
    pub current_level: HashMap<String, usize>,
    /// One uploaded point batch per object layer, keyed by layer id.
    pub object_buffers: HashMap<String, PointBuffer>,
    /// The world every layer is drawn in — the reference layer's
    /// full-resolution size. Kept here as well as in props so a mouse event,
    /// which has no access to props, can invert the camera transform.
    pub world_size: (f32, f32),
    pub camera: Camera2d,
}

impl ViewerCanvasState {
    /// The levels this layer has grid info for, coarsest first.
    fn levels_of(&self, layer: &str) -> Vec<usize> {
        let mut levels: Vec<usize> = self
            .level_info
            .keys()
            .filter(|(id, _)| id == layer)
            .map(|(_, level)| *level)
            .collect();
        levels.sort_unstable_by(|a, b| b.cmp(a));
        levels
    }
}

/// Props for the ViewerCanvas component.
#[derive(Properties, PartialEq)]
pub struct ViewerCanvasProps {
    /// Layers in draw order, bottom first.
    pub layers: Vec<LayerRenderInfo>,
    /// The coordinate space every layer is drawn in: the reference layer's
    /// full-resolution `(width, height)`.
    pub world_size: (f32, f32),
    #[prop_or_default]
    pub on_canvas_ready: Callback<Rc<RefCell<Option<ViewerCanvasState>>>>,
    #[prop_or_default]
    pub on_camera_changed: Callback<(f32, f32, f32, f32, f32)>, // (pan_x, pan_y, zoom, canvas_w, canvas_h)
    /// A click that was not a drag, in world pixel coordinates.
    #[prop_or_default]
    pub on_pick: Callback<(f32, f32)>,
}

/// WebGL2 canvas component handling rendering, pan, and zoom.
pub struct ViewerCanvas {
    canvas_ref: NodeRef,
    state: Rc<RefCell<Option<ViewerCanvasState>>>,
    _resize_closure: Option<Closure<dyn Fn()>>,
}

pub enum ViewerMsg {
    Init,
    MouseDown(MouseEvent),
    MouseMove(MouseEvent),
    MouseUp(MouseEvent),
    Wheel(WheelEvent),
    TouchStart(web_sys::TouchEvent),
    TouchMove(web_sys::TouchEvent),
    TouchEnd(web_sys::TouchEvent),
    Resize,
    Redraw,
}

impl Component for ViewerCanvas {
    type Message = ViewerMsg;
    type Properties = ViewerCanvasProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            canvas_ref: NodeRef::default(),
            state: Rc::new(RefCell::new(None)),
            _resize_closure: None,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            ctx.link().send_message(ViewerMsg::Init);
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            ViewerMsg::Init => {
                if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
                    // Set canvas size to match its display size
                    let rect = canvas.get_bounding_client_rect();
                    canvas.set_width(rect.width() as u32);
                    canvas.set_height(rect.height() as u32);

                    match GlContext::new(&canvas) {
                        Ok(gl_ctx) => match Renderer::new(gl_ctx) {
                            Ok(renderer) => {
                                renderer.resize(rect.width() as u32, rect.height() as u32);
                                renderer.clear();
                                let state = ViewerCanvasState {
                                    renderer,
                                    tile_cache: HashMap::new(),
                                    level_info: HashMap::new(),
                                    current_level: HashMap::new(),
                                    object_buffers: HashMap::new(),
                                    world_size: (0.0, 0.0),
                                    camera: Camera2d {
                                        x: 0.0,
                                        y: 0.0,
                                        zoom: 1.0,
                                        canvas_w: rect.width() as f32,
                                        canvas_h: rect.height() as f32,
                                        dragging: false,
                                        dragged: false,
                                        last_mouse: (0.0, 0.0),
                                        pinch_dist: None,
                                        pinch_center: (0.0, 0.0),
                                    },
                                };
                                *self.state.borrow_mut() = Some(state);
                                ctx.props().on_canvas_ready.emit(self.state.clone());
                                ctx.props().on_camera_changed.emit((
                                    0.0,
                                    0.0,
                                    1.0,
                                    rect.width() as f32,
                                    rect.height() as f32,
                                ));
                            }
                            Err(e) => log::error!("Renderer init: {}", e),
                        },
                        Err(e) => log::error!("WebGL init: {}", e),
                    }
                    // Set up window resize listener
                    let link = ctx.link().clone();
                    let closure = Closure::wrap(Box::new(move || {
                        link.send_message(ViewerMsg::Resize);
                    }) as Box<dyn Fn()>);
                    web_sys::window()
                        .unwrap()
                        .set_onresize(Some(closure.as_ref().unchecked_ref()));
                    self._resize_closure = Some(closure);
                }
                false
            }
            ViewerMsg::MouseDown(e) => {
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    state.camera.dragging = true;
                    state.camera.dragged = false;
                    state.camera.last_mouse = (e.client_x() as f32, e.client_y() as f32);
                }
                false
            }
            ViewerMsg::MouseMove(e) => {
                let mut needs_redraw = false;
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    if state.camera.dragging {
                        let dx = e.client_x() as f32 - state.camera.last_mouse.0;
                        let dy = e.client_y() as f32 - state.camera.last_mouse.1;
                        if dx.abs() > 1.0 || dy.abs() > 1.0 {
                            state.camera.dragged = true;
                        }
                        state.camera.x += dx;
                        state.camera.y += dy;
                        state.camera.last_mouse = (e.client_x() as f32, e.client_y() as f32);
                        needs_redraw = true;
                    }
                }
                if needs_redraw {
                    ctx.link().send_message(ViewerMsg::Redraw);
                }
                false
            }
            ViewerMsg::MouseUp(e) => {
                let (was_dragging, was_click) = match *self.state.borrow_mut() {
                    Some(ref mut state) => {
                        let dragging = state.camera.dragging;
                        let click = dragging && !state.camera.dragged;
                        state.camera.dragging = false;
                        state.camera.dragged = false;
                        (dragging, click)
                    }
                    None => (false, false),
                };
                if was_click {
                    if let Some(world) = self.world_at(&e) {
                        ctx.props().on_pick.emit(world);
                    }
                }
                if was_dragging {
                    self.emit_camera_changed(ctx);
                }
                false
            }
            ViewerMsg::Wheel(e) => {
                e.prevent_default();
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    let delta = -e.delta_y() as f32 * 0.001;
                    let factor = 1.0 + delta;
                    let new_zoom = (state.camera.zoom * factor).max(0.01);
                    let actual_factor = new_zoom / state.camera.zoom;

                    // Mouse position relative to canvas center (in pixels)
                    if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
                        let rect = canvas.get_bounding_client_rect();
                        let mx =
                            e.client_x() as f32 - rect.left() as f32 - rect.width() as f32 / 2.0;
                        let my =
                            e.client_y() as f32 - rect.top() as f32 - rect.height() as f32 / 2.0;

                        // Adjust pan so the point under the cursor stays fixed
                        state.camera.x = mx + (state.camera.x - mx) * actual_factor;
                        state.camera.y = my + (state.camera.y - my) * actual_factor;
                    }

                    state.camera.zoom = new_zoom;
                }
                self.emit_camera_changed(ctx);
                ctx.link().send_message(ViewerMsg::Redraw);
                false
            }
            ViewerMsg::TouchStart(e) => {
                e.prevent_default();
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    let touches = e.touches();
                    if touches.length() == 1 {
                        if let Some(t) = touches.get(0) {
                            state.camera.dragging = true;
                            state.camera.last_mouse = (t.client_x() as f32, t.client_y() as f32);
                            state.camera.pinch_dist = None;
                        }
                    } else if touches.length() == 2 {
                        if let (Some(t0), Some(t1)) = (touches.get(0), touches.get(1)) {
                            let dx = t1.client_x() as f32 - t0.client_x() as f32;
                            let dy = t1.client_y() as f32 - t0.client_y() as f32;
                            state.camera.pinch_dist = Some((dx * dx + dy * dy).sqrt());
                            state.camera.pinch_center = (
                                (t0.client_x() + t1.client_x()) as f32 / 2.0,
                                (t0.client_y() + t1.client_y()) as f32 / 2.0,
                            );
                            state.camera.dragging = false;
                        }
                    }
                }
                false
            }
            ViewerMsg::TouchMove(e) => {
                e.prevent_default();
                let mut needs_redraw = false;
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    let touches = e.touches();
                    if touches.length() == 1 && state.camera.dragging {
                        if let Some(t) = touches.get(0) {
                            let tx = t.client_x() as f32;
                            let ty = t.client_y() as f32;
                            state.camera.x += tx - state.camera.last_mouse.0;
                            state.camera.y += ty - state.camera.last_mouse.1;
                            state.camera.last_mouse = (tx, ty);
                            needs_redraw = true;
                        }
                    } else if touches.length() == 2 {
                        if let (Some(t0), Some(t1)) = (touches.get(0), touches.get(1)) {
                            let dx = t1.client_x() as f32 - t0.client_x() as f32;
                            let dy = t1.client_y() as f32 - t0.client_y() as f32;
                            let new_dist = (dx * dx + dy * dy).sqrt();
                            let cx = (t0.client_x() + t1.client_x()) as f32 / 2.0;
                            let cy = (t0.client_y() + t1.client_y()) as f32 / 2.0;

                            if let Some(old_dist) = state.camera.pinch_dist {
                                let factor = new_dist / old_dist;
                                let new_zoom = (state.camera.zoom * factor).max(0.01);

                                // Zoom around pinch center
                                if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
                                    let rect = canvas.get_bounding_client_rect();
                                    let mx = cx - rect.left() as f32 - rect.width() as f32 / 2.0;
                                    let my = cy - rect.top() as f32 - rect.height() as f32 / 2.0;
                                    state.camera.x = mx + (state.camera.x - mx) * factor;
                                    state.camera.y = my + (state.camera.y - my) * factor;
                                }

                                state.camera.zoom = new_zoom;
                            }
                            state.camera.pinch_dist = Some(new_dist);
                            state.camera.pinch_center = (cx, cy);
                            needs_redraw = true;
                        }
                    }
                }
                if needs_redraw {
                    ctx.link().send_message(ViewerMsg::Redraw);
                }
                false
            }
            ViewerMsg::TouchEnd(e) => {
                e.prevent_default();
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    state.camera.dragging = false;
                    state.camera.pinch_dist = None;
                }
                self.emit_camera_changed(ctx);
                false
            }
            ViewerMsg::Resize => {
                if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
                    let rect = canvas.get_bounding_client_rect();
                    let w = rect.width() as u32;
                    let h = rect.height() as u32;
                    if w > 0 && h > 0 {
                        canvas.set_width(w);
                        canvas.set_height(h);
                        if let Some(ref mut state) = *self.state.borrow_mut() {
                            state.renderer.resize(w, h);
                            state.camera.canvas_w = w as f32;
                            state.camera.canvas_h = h as f32;
                        }
                        self.emit_camera_changed(ctx);
                        ctx.link().send_message(ViewerMsg::Redraw);
                    }
                }
                false
            }
            ViewerMsg::Redraw => {
                self.redraw(ctx);
                false
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_mousedown = ctx.link().callback(ViewerMsg::MouseDown);
        let on_mousemove = ctx.link().callback(ViewerMsg::MouseMove);
        let on_mouseup = ctx.link().callback(ViewerMsg::MouseUp);
        let on_wheel = ctx.link().callback(ViewerMsg::Wheel);
        let on_touchstart = ctx.link().callback(ViewerMsg::TouchStart);
        let on_touchmove = ctx.link().callback(ViewerMsg::TouchMove);
        let on_touchend = ctx.link().callback(ViewerMsg::TouchEnd);

        html! {
            <canvas
                ref={self.canvas_ref.clone()}
                class="viewer-canvas"
                onmousedown={on_mousedown}
                onmousemove={on_mousemove}
                onmouseup={on_mouseup}
                onwheel={on_wheel}
                ontouchstart={on_touchstart}
                ontouchmove={on_touchmove}
                ontouchend={on_touchend}
            />
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, _old_props: &Self::Properties) -> bool {
        // Re-render when layer info changes
        self.redraw(ctx);
        true
    }
}

/// The world size the canvas last drew in.
fn camera_world(state: &ViewerCanvasState) -> (f32, f32) {
    state.world_size
}

impl ViewerCanvas {
    /// Notify the parent App of the current camera state.
    fn emit_camera_changed(&self, ctx: &Context<Self>) {
        if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
            let state_ref = self.state.borrow();
            if let Some(ref state) = *state_ref {
                ctx.props().on_camera_changed.emit((
                    state.camera.x,
                    state.camera.y,
                    state.camera.zoom,
                    canvas.width() as f32,
                    canvas.height() as f32,
                ));
            }
        }
    }

    /// The world pixel under a mouse event — the vertex shader's transform,
    /// inverted.
    fn world_at(&self, e: &MouseEvent) -> Option<(f32, f32)> {
        let canvas = self.canvas_ref.cast::<HtmlCanvasElement>()?;
        let rect = canvas.get_bounding_client_rect();
        let state_ref = self.state.borrow();
        let state = state_ref.as_ref()?;
        let camera = &state.camera;

        let (world_w, world_h) = camera_world(state);
        if world_w <= 0.0 || world_h <= 0.0 {
            return None;
        }
        let canvas_w = rect.width() as f32;
        let canvas_h = rect.height() as f32;
        let fit = camera.zoom * (canvas_w / world_w).min(canvas_h / world_h);
        let scale_x = fit * world_w / canvas_w;
        let scale_y = fit * world_h / canvas_h;
        if scale_x == 0.0 || scale_y == 0.0 {
            return None;
        }

        let px = e.client_x() as f32 - rect.left() as f32;
        let py = e.client_y() as f32 - rect.top() as f32;

        // Clip coordinates, with the shader's y flip already undone.
        let clip_x = px / canvas_w * 2.0 - 1.0;
        let flipped_y = py / canvas_h * 2.0 - 1.0;

        let centered_x = (clip_x - camera.x * 2.0 / canvas_w) / scale_x;
        let centered_y = (flipped_y - camera.y * 2.0 / canvas_h) / scale_y;

        let world_x = (centered_x / 2.0 + 0.5) * world_w;
        let world_y = (centered_y / 2.0 + 0.5) * world_h;
        Some((world_x, world_y))
    }

    /// Clear and redraw every layer, coarse levels first as a fallback.
    fn redraw(&self, ctx: &Context<Self>) {
        let state_ref = self.state.borrow();
        let state = match state_ref.as_ref() {
            Some(s) => s,
            None => return,
        };

        let canvas = match self.canvas_ref.cast::<HtmlCanvasElement>() {
            Some(c) => c,
            None => return,
        };

        let canvas_size = (canvas.width() as f32, canvas.height() as f32);
        let props = ctx.props();
        let world = props.world_size;
        if world.0 <= 0.0 || world.1 <= 0.0 {
            return;
        }

        state.renderer.clear();

        for layer in &props.layers {
            if !layer.visible {
                continue;
            }
            if let LayerRenderKind::Objects(info) = &layer.kind {
                if let Some(points) = state.object_buffers.get(&layer.id) {
                    // Points carry their own world position, so the placement
                    // only has to say what the world is and where the camera
                    // is looking.
                    let placement = TilePlacement {
                        tile_offset: (0.0, 0.0),
                        tile_size: (1.0, 1.0),
                        image_size: world,
                        canvas_size,
                        pan: (state.camera.x, state.camera.y),
                        zoom: state.camera.zoom,
                    };
                    state.renderer.draw_points(points, &placement, info);
                }
                continue;
            }

            let current = state.current_level.get(&layer.id).copied().unwrap_or(0);
            // Coarser levels first: they cover the gaps while the current
            // level's tiles are still arriving, and the current level draws
            // over them.
            let mut levels = state.levels_of(&layer.id);
            levels.retain(|&level| level != current);
            levels.push(current);
            for level in levels {
                self.draw_layer_level(state, layer, level, world, canvas_size);
            }
        }
    }

    /// Draw every cached tile of one layer at one level.
    fn draw_layer_level(
        &self,
        state: &ViewerCanvasState,
        layer: &LayerRenderInfo,
        level: usize,
        world: (f32, f32),
        canvas_size: (f32, f32),
    ) {
        let Some(info) = state.level_info.get(&(layer.id.clone(), level)) else {
            return;
        };
        let (tw, th) = info.tile_size;
        let (sx, sy) = info.world_scale;
        let camera = &state.camera;

        for ty in 0..info.num_tiles_y {
            for tx in 0..info.num_tiles_x {
                let placement = |tex: &TileTexture| TilePlacement {
                    tile_offset: (tx as f32 * tw * sx, ty as f32 * th * sy),
                    tile_size: (tex.width as f32 * sx, tex.height as f32 * sy),
                    image_size: world,
                    canvas_size,
                    pan: (camera.x, camera.y),
                    zoom: camera.zoom,
                };

                match &layer.kind {
                    LayerRenderKind::Image {
                        channels,
                        dtype_max,
                        blend,
                    } => {
                        let mut bound: Vec<(&TileTexture, [f32; 3], f32, f32, f32)> = Vec::new();
                        for (channel, info) in channels.iter().enumerate() {
                            if info.opacity <= 0.0 {
                                continue;
                            }
                            let key = TileKey {
                                layer: layer.id.clone(),
                                level,
                                tile_y: ty,
                                tile_x: tx,
                                channel,
                            };
                            if let Some(tex) = state.tile_cache.get(&key) {
                                if tex.kind != TextureKind::Intensity {
                                    continue;
                                }
                                bound.push((
                                    tex,
                                    info.color,
                                    info.contrast_min,
                                    info.contrast_max,
                                    info.opacity,
                                ));
                            }
                        }
                        if let Some((first, _, _, _, _)) = bound.first() {
                            let placement = placement(first);
                            state
                                .renderer
                                .draw_tile(&bound, &placement, *dtype_max, *blend);
                        }
                    }
                    LayerRenderKind::Objects(_) => {}
                    LayerRenderKind::Labels(label_info) => {
                        let key = TileKey {
                            layer: layer.id.clone(),
                            level,
                            tile_y: ty,
                            tile_x: tx,
                            channel: 0,
                        };
                        if let Some(tex) = state.tile_cache.get(&key) {
                            let placement = placement(tex);
                            state
                                .renderer
                                .draw_label_tile(&layer.id, tex, &placement, label_info);
                        }
                    }
                }
            }
        }
    }
}
