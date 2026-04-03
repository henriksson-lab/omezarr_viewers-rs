use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::webgl::context::GlContext;
use crate::webgl::renderer::{Renderer, TileTexture};

#[derive(Clone)]
pub struct ChannelRenderInfo {
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub opacity: f32,
}

pub struct Camera2d {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub dragging: bool,
    pub last_mouse: (f32, f32),
}

#[derive(Hash, Eq, PartialEq, Clone, Copy, Debug)]
pub struct TileKey {
    pub level: usize,
    pub tile_y: u32,
    pub tile_x: u32,
    pub channel: usize,
}

#[derive(Clone, Debug)]
pub struct LevelTileInfo {
    pub image_size: (f32, f32),
    pub tile_size: (f32, f32),
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
}

pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_cache: HashMap<TileKey, TileTexture>,
    pub level_info: HashMap<usize, LevelTileInfo>,
    pub current_level: usize,
    pub camera: Camera2d,
}

#[derive(Properties, PartialEq)]
pub struct ViewerCanvasProps {
    pub channel_info: Vec<ChannelRenderInfo>,
    pub dtype_max: f32,
    #[prop_or_default]
    pub on_canvas_ready: Callback<Rc<RefCell<Option<ViewerCanvasState>>>>,
    #[prop_or_default]
    pub on_zoom_changed: Callback<(f32, f32, f32)>, // (zoom, canvas_w, canvas_h)
}

impl PartialEq for ChannelRenderInfo {
    fn eq(&self, other: &Self) -> bool {
        self.color == other.color
            && self.contrast_min == other.contrast_min
            && self.contrast_max == other.contrast_max
            && self.opacity == other.opacity
    }
}

pub struct ViewerCanvas {
    canvas_ref: NodeRef,
    state: Rc<RefCell<Option<ViewerCanvasState>>>,
}

pub enum ViewerMsg {
    Init,
    MouseDown(MouseEvent),
    MouseMove(MouseEvent),
    MouseUp(()),
    Wheel(WheelEvent),
    Redraw,
}

impl Component for ViewerCanvas {
    type Message = ViewerMsg;
    type Properties = ViewerCanvasProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            canvas_ref: NodeRef::default(),
            state: Rc::new(RefCell::new(None)),
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
                                        dragging: false,
                                        last_mouse: (0.0, 0.0),
                                    },
                                };
                                *self.state.borrow_mut() = Some(state);
                                ctx.props()
                                    .on_canvas_ready
                                    .emit(self.state.clone());
                                ctx.props()
                                    .on_zoom_changed
                                    .emit((1.0, rect.width() as f32, rect.height() as f32));
                            }
                            Err(e) => log::error!("Renderer init: {}", e),
                        },
                        Err(e) => log::error!("WebGL init: {}", e),
                    }
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
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    state.camera.dragging = false;
                }
                false
            }
            ViewerMsg::Wheel(e) => {
                e.prevent_default();
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    let delta = -e.delta_y() as f32 * 0.001;
                    let factor = 1.0 + delta;
                    let new_zoom = (state.camera.zoom * factor).clamp(0.01, 100.0);
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
                // Notify app of zoom change for level selection
                if let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() {
                    let state_ref = self.state.borrow();
                    if let Some(ref state) = *state_ref {
                        ctx.props().on_zoom_changed.emit((
                            state.camera.zoom,
                            canvas.width() as f32,
                            canvas.height() as f32,
                        ));
                    }
                }
                ctx.link().send_message(ViewerMsg::Redraw);
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

        html! {
            <canvas
                ref={self.canvas_ref.clone()}
                class="viewer-canvas"
                onmousedown={on_mousedown}
                onmousemove={on_mousemove}
                onmouseup={on_mouseup}
                onwheel={on_wheel}
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
