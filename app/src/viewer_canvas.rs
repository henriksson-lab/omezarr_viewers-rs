use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::webgl::context::GlContext;
use crate::webgl::renderer::{Renderer, TileTexture};

/// Per-channel rendering parameters passed as props to the canvas.
#[derive(Clone)]
pub struct ChannelRenderInfo {
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub opacity: f32,
}

/// 2D camera state: pan position, zoom level, and drag tracking.
pub struct Camera2d {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
    pub dragging: bool,
    pub last_mouse: (f32, f32),
    pub pinch_dist: Option<f32>,
    pub pinch_center: (f32, f32),
}

/// Cache key identifying a tile by pyramid level, grid position, and channel.
#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub struct TileKey {
    pub level: usize,
    pub tile_y: u32,
    pub tile_x: u32,
    pub channel: usize,
}

/// Tile grid metadata for a single pyramid level.
#[derive(Clone, Debug)]
pub struct LevelTileInfo {
    pub image_size: (f32, f32),
    pub tile_size: (f32, f32),
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
}

/// Shared mutable state between App (tile uploads) and ViewerCanvas (rendering).
pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_cache: HashMap<TileKey, TileTexture>,
    pub level_info: HashMap<usize, LevelTileInfo>,
    pub current_level: usize,
    pub camera: Camera2d,
}

/// Props for the ViewerCanvas component.
#[derive(Properties, PartialEq)]
pub struct ViewerCanvasProps {
    pub channel_info: Vec<ChannelRenderInfo>,
    pub dtype_max: f32,
    #[prop_or_default]
    pub on_canvas_ready: Callback<Rc<RefCell<Option<ViewerCanvasState>>>>,
    #[prop_or_default]
    pub on_camera_changed: Callback<(f32, f32, f32, f32, f32)>, // (pan_x, pan_y, zoom, canvas_w, canvas_h)
}

impl PartialEq for ChannelRenderInfo {
    fn eq(&self, other: &Self) -> bool {
        self.color == other.color
            && self.contrast_min == other.contrast_min
            && self.contrast_max == other.contrast_max
            && self.opacity == other.opacity
    }
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
    MouseUp(()),
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
                                    current_level: 0,
                                    camera: Camera2d {
                                        x: 0.0,
                                        y: 0.0,
                                        zoom: 1.0,
                                        canvas_w: rect.width() as f32,
                                        canvas_h: rect.height() as f32,
                                        dragging: false,
                                        last_mouse: (0.0, 0.0),
                                        pinch_dist: None,
                                        pinch_center: (0.0, 0.0),
                                    },
                                };
                                *self.state.borrow_mut() = Some(state);
                                ctx.props()
                                    .on_canvas_ready
                                    .emit(self.state.clone());
                                ctx.props()
                                    .on_camera_changed
                                    .emit((0.0, 0.0, 1.0, rect.width() as f32, rect.height() as f32));
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
            ViewerMsg::MouseUp(_) => {
                let was_dragging;
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    was_dragging = state.camera.dragging;
                    state.camera.dragging = false;
                } else {
                    was_dragging = false;
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
                        let mx = e.client_x() as f32 - rect.left() as f32 - rect.width() as f32 / 2.0;
                        let my = e.client_y() as f32 - rect.top() as f32 - rect.height() as f32 / 2.0;

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
        let on_mouseup = ctx.link().callback(|_: MouseEvent| ViewerMsg::MouseUp(()));
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
        // Re-render when channel info changes
        self.redraw(ctx);
        true
    }
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

    /// Clear and redraw all visible tiles, rendering fallback levels first.
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

        let cw = canvas.width() as f32;
        let ch = canvas.height() as f32;

        state.renderer.clear();

        let props = ctx.props();
        let cur = state.current_level;

        let cur_info = match state.level_info.get(&cur) {
            Some(info) => info,
            None => return,
        };

        // Collect levels present in cache, sorted coarsest first (higher index = coarser)
        let mut levels_in_cache: Vec<usize> = state.level_info.keys().copied()
            .filter(|&l| l != cur)
            .collect();
        levels_in_cache.sort_unstable_by(|a, b| b.cmp(a));

        // Draw fallback levels first (coarsest to finest, excluding current).
        // These provide coverage while current-level tiles are loading.
        for &level in &levels_in_cache {
            let info = &state.level_info[&level];
            self.draw_level_tiles(state, props, level, info, cur_info.image_size, cw, ch);
        }

        // Draw current level on top — replaces fallback where loaded (blend alpha=1)
        self.draw_level_tiles(state, props, cur, cur_info, cur_info.image_size, cw, ch);
    }

    /// Render all cached tiles for a single pyramid level, scaled to the current image coordinate space.
    fn draw_level_tiles(
        &self,
        state: &ViewerCanvasState,
        props: &ViewerCanvasProps,
        level: usize,
        info: &LevelTileInfo,
        render_image_size: (f32, f32),
        cw: f32,
        ch: f32,
    ) {
        let (tw, th) = info.tile_size;
        // Scale factor: map this level's pixel coords to the current level's image coords
        let sx = render_image_size.0 / info.image_size.0;
        let sy = render_image_size.1 / info.image_size.1;

        for ty in 0..info.num_tiles_y {
            for tx in 0..info.num_tiles_x {
                let mut channel_data: Vec<(&TileTexture, [f32; 3], f32, f32, f32)> = Vec::new();
                let mut actual_w = tw;
                let mut actual_h = th;

                for (ch_idx, ch_info) in props.channel_info.iter().enumerate() {
                    if ch_info.opacity <= 0.0 {
                        continue;
                    }
                    let key = TileKey { level, tile_y: ty, tile_x: tx, channel: ch_idx };
                    if let Some(tex) = state.tile_cache.get(&key) {
                        actual_w = tex.width as f32;
                        actual_h = tex.height as f32;
                        channel_data.push((
                            tex,
                            ch_info.color,
                            ch_info.contrast_min,
                            ch_info.contrast_max,
                            ch_info.opacity,
                        ));
                    }
                }

                if !channel_data.is_empty() {
                    let tile_offset = (tx as f32 * tw * sx, ty as f32 * th * sy);
                    let tile_size = (actual_w * sx, actual_h * sy);

                    state.renderer.draw_tile(
                        &channel_data,
                        tile_offset,
                        tile_size,
                        render_image_size,
                        (cw, ch),
                        (state.camera.x, state.camera.y),
                        state.camera.zoom,
                        props.dtype_max,
                    );
                }
            }
        }
    }
}
