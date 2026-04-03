use std::cell::RefCell;
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

pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_textures: Vec<Vec<TileTexture>>, // [channel_idx][tile_idx]
    pub pan: (f32, f32),
    pub zoom: f32,
    pub image_size: (f32, f32),
    pub tile_size: (f32, f32),
    pub dragging: bool,
    pub last_mouse: (f32, f32),
}

#[derive(Properties, PartialEq)]
pub struct ViewerCanvasProps {
    pub channel_info: Vec<ChannelRenderInfo>,
    pub dtype_max: f32,
    #[prop_or_default]
    pub on_canvas_ready: Callback<Rc<RefCell<Option<ViewerCanvasState>>>>,
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
                                    tile_textures: Vec::new(),
                                    pan: (0.0, 0.0),
                                    zoom: 1.0,
                                    image_size: (1.0, 1.0),
                                    tile_size: (256.0, 256.0),
                                    dragging: false,
                                    last_mouse: (0.0, 0.0),
                                };
                                *self.state.borrow_mut() = Some(state);
                                ctx.props()
                                    .on_canvas_ready
                                    .emit(self.state.clone());
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
                    state.dragging = true;
                    state.last_mouse = (e.client_x() as f32, e.client_y() as f32);
                }
                false
            }
            ViewerMsg::MouseMove(e) => {
                let mut needs_redraw = false;
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    if state.dragging {
                        let dx = e.client_x() as f32 - state.last_mouse.0;
                        let dy = e.client_y() as f32 - state.last_mouse.1;
                        state.pan.0 += dx;
                        state.pan.1 += dy;
                        state.last_mouse = (e.client_x() as f32, e.client_y() as f32);
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
                    state.dragging = false;
                }
                false
            }
            ViewerMsg::Wheel(e) => {
                e.prevent_default();
                if let Some(ref mut state) = *self.state.borrow_mut() {
                    let delta = -e.delta_y() as f32 * 0.001;
                    state.zoom *= 1.0 + delta;
                    state.zoom = state.zoom.clamp(0.01, 100.0);
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

        // For each tile (currently just one full-image tile per channel)
        if state.tile_textures.is_empty() {
            return;
        }

        // Build channel texture list for rendering
        let num_tiles = state.tile_textures.get(0).map(|t| t.len()).unwrap_or(0);
        for tile_idx in 0..num_tiles {
            let mut channel_data: Vec<(
                &TileTexture,
                [f32; 3],
                f32,
                f32,
                f32,
            )> = Vec::new();

            for (ch_idx, ch_info) in props.channel_info.iter().enumerate() {
                if ch_info.opacity <= 0.0 {
                    continue;
                }
                if let Some(tiles) = state.tile_textures.get(ch_idx) {
                    if let Some(tex) = tiles.get(tile_idx) {
                        channel_data.push((
                            tex,
                            ch_info.color,
                            ch_info.contrast_min,
                            ch_info.contrast_max,
                            ch_info.opacity,
                        ));
                    }
                }
            }

            if !channel_data.is_empty() {
                state.renderer.draw_tile(
                    &channel_data,
                    (0.0, 0.0),
                    state.tile_size,
                    state.image_size,
                    (cw, ch),
                    state.pan,
                    state.zoom,
                    props.dtype_max,
                );
            }
        }
    }
}
