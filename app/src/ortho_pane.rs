//! An orthogonal pane: one `(z, x)` or `(z, y)` plane through the volume.
//!
//! Self-contained on purpose. The pane owns its own WebGL context, fetches its
//! own planes and draws them, so the main view's tile pipeline — levels,
//! fallbacks, eviction — does not have to grow a second set of axes it was
//! never shaped for. What it takes from the app is what to show: which layer,
//! which channels, which index, and where the crosshair is.
//!
//! One request per channel per pane, at a level the pane fits, is the whole
//! loading strategy. A plane crosses every chunk row of the store, so tiling it
//! would multiply requests rather than divide work.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::api_client;
use crate::viewer_canvas::ChannelRenderInfo;
use crate::webgl::context::GlContext;
use crate::webgl::renderer::{Blend, Renderer, TilePlacement, TileTexture};

/// One image layer's contribution to a pane.
#[derive(Clone, PartialEq)]
pub struct OrthoLayer {
    pub id: String,
    pub level: usize,
    /// Where this layer is cut, in **its own** level coordinates. Per layer
    /// rather than per pane because a label volume at half resolution is cut at
    /// half the index the image is.
    pub index: u64,
    pub dtype_max: f32,
    /// `(channel index, how to draw it)` for the visible channels.
    pub channels: Vec<(usize, ChannelRenderInfo)>,
}

#[derive(Properties, PartialEq)]
pub struct OrthoPaneProps {
    /// `"y"` for the `(z, x)` pane, `"x"` for the `(z, y)` pane.
    pub axis: &'static str,
    pub t: u64,
    pub layers: Vec<OrthoLayer>,
    /// Transpose the plane before drawing.
    ///
    /// The `(z, y)` plane is shown with y down the pane, so the right-hand pane
    /// lines up row-for-row with the main view; the bottom pane needs no
    /// transpose because `(z, x)` already has x across.
    pub transpose: bool,
    /// Crosshair position within the pane, as fractions of width and height.
    pub crosshair: (f32, f32),
    /// A click, reported as fractions of width and height.
    #[prop_or_default]
    pub on_pick: Callback<(f32, f32)>,
}

/// What one channel's plane looks like once uploaded.
struct PlaneTexture {
    texture: TileTexture,
    info: ChannelRenderInfo,
    dtype_max: f32,
}

pub struct OrthoPane {
    canvas_ref: NodeRef,
    state: Rc<RefCell<Option<PaneState>>>,
    /// Bumped on every reload; a plane from an older generation is dropped.
    generation: u64,
}

struct PaneState {
    renderer: Renderer,
    planes: Vec<PlaneTexture>,
}

pub enum PaneMsg {
    Init,
    Reload,
    Loaded {
        generation: u64,
        pixels: Vec<f32>,
        width: u32,
        height: u32,
        info: ChannelRenderInfo,
        dtype_max: f32,
        first: bool,
    },
    Click(MouseEvent),
}

impl Component for OrthoPane {
    type Message = PaneMsg;
    type Properties = OrthoPaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            canvas_ref: NodeRef::default(),
            state: Rc::new(RefCell::new(None)),
            generation: 0,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            ctx.link().send_message(PaneMsg::Init);
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            PaneMsg::Init => {
                let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() else {
                    log::warn!("ortho {}: no canvas", ctx.props().axis);
                    return false;
                };
                let rect = canvas.get_bounding_client_rect();
                canvas.set_width(rect.width().max(1.0) as u32);
                canvas.set_height(rect.height().max(1.0) as u32);
                match GlContext::new(&canvas).and_then(Renderer::new) {
                    Ok(renderer) => {
                        renderer.resize(canvas.width(), canvas.height());
                        renderer.clear();
                        *self.state.borrow_mut() = Some(PaneState {
                            renderer,
                            planes: Vec::new(),
                        });
                        ctx.link().send_message(PaneMsg::Reload);
                    }
                    Err(e) => log::error!("ortho pane init: {}", e),
                }
                false
            }
            PaneMsg::Reload => {
                self.generation += 1;
                let generation = self.generation;
                let props = ctx.props();
                let axis = props.axis;
                let t = props.t;

                let mut first = true;
                for layer in &props.layers {
                    for (channel, info) in &layer.channels {
                        let id = layer.id.clone();
                        let info = info.clone();
                        let dtype_max = layer.dtype_max;
                        let level = layer.level;
                        let index = layer.index;
                        let channel = *channel as u64;
                        let link = ctx.link().clone();
                        let is_first = first;
                        first = false;
                        spawn_local(async move {
                            match api_client::fetch_slice(&id, axis, index, level, t, channel).await
                            {
                                Ok(plane) => link.send_message(PaneMsg::Loaded {
                                    generation,
                                    pixels: plane.pixels,
                                    width: plane.width,
                                    height: plane.height,
                                    info,
                                    dtype_max,
                                    first: is_first,
                                }),
                                Err(e) => log::warn!("ortho plane: {}", e),
                            }
                        });
                    }
                }
                if first {
                    // Nothing to draw: clear so a stale plane does not linger.
                    if let Some(state) = self.state.borrow_mut().as_mut() {
                        state.planes.clear();
                        state.renderer.clear();
                    }
                }
                false
            }
            PaneMsg::Loaded {
                generation,
                pixels,
                width,
                height,
                info,
                dtype_max,
                first,
            } => {
                if generation != self.generation {
                    return false;
                }
                let (pixels, width, height) = if ctx.props().transpose {
                    (transpose(&pixels, width, height), height, width)
                } else {
                    (pixels, width, height)
                };
                if let Some(state) = self.state.borrow_mut().as_mut() {
                    if first {
                        state.planes.clear();
                    }
                    match state.renderer.upload_tile(width, height, &pixels) {
                        Ok(texture) => state.planes.push(PlaneTexture {
                            texture,
                            info,
                            dtype_max,
                        }),
                        Err(e) => log::warn!("ortho upload: {}", e),
                    }
                }
                self.draw();
                false
            }
            PaneMsg::Click(event) => {
                let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() else {
                    return false;
                };
                let rect = canvas.get_bounding_client_rect();
                let u = (event.client_x() as f64 - rect.left()) / rect.width().max(1.0);
                let v = (event.client_y() as f64 - rect.top()) / rect.height().max(1.0);
                ctx.props()
                    .on_pick
                    .emit((u.clamp(0.0, 1.0) as f32, v.clamp(0.0, 1.0) as f32));
                false
            }
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        // The crosshair moves in the DOM overlay and needs no reload; anything
        // else — a new index, a new level, a channel toggled — needs the plane
        // read again.
        let same_data = old.axis == ctx.props().axis
            && old.transpose == ctx.props().transpose
            && old.t == ctx.props().t
            && old.layers == ctx.props().layers;
        if !same_data {
            ctx.link().send_message(PaneMsg::Reload);
        } else {
            self.draw();
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let (u, v) = ctx.props().crosshair;
        let class = format!("ortho-pane ortho-{}", ctx.props().axis);
        html! {
            <div class={class}>
                <canvas
                    ref={self.canvas_ref.clone()}
                    class="ortho-canvas"
                    onmousedown={ctx.link().callback(PaneMsg::Click)}
                />
                <div class="crosshair-v" style={format!("left: {}%", u * 100.0)} />
                <div class="crosshair-h" style={format!("top: {}%", v * 100.0)} />
            </div>
        }
    }
}

impl OrthoPane {
    /// Draw every uploaded plane, stretched to fill the pane.
    ///
    /// Stretched rather than fitted, and deliberately: a `(z, x)` plane of
    /// eight z against five hundred x drawn at its true aspect is a hairline.
    /// What the pane answers is *where in z*, and stretching keeps that legible
    /// — which is why the crosshair is in fractions of the pane rather than in
    /// pixels of the plane.
    fn draw(&self) {
        let state_ref = self.state.borrow();
        let Some(state) = state_ref.as_ref() else {
            return;
        };
        let Some(canvas) = self.canvas_ref.cast::<HtmlCanvasElement>() else {
            return;
        };
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        state.renderer.resize(width, height);
        state.renderer.clear();
        if state.planes.is_empty() {
            return;
        }

        let first = &state.planes[0];
        // Telling the shader the image is exactly canvas-shaped is what makes
        // the quad cover the pane; the texture coordinates still span the whole
        // plane, so the plane is stretched onto it.
        let canvas_size = (width as f32, height as f32);
        let placement = TilePlacement {
            tile_offset: (0.0, 0.0),
            tile_size: canvas_size,
            image_size: canvas_size,
            canvas_size,
            pan: (0.0, 0.0),
            zoom: 1.0,
        };
        let bound: Vec<(&TileTexture, [f32; 3], f32, f32, f32)> = state
            .planes
            .iter()
            .map(|plane| {
                (
                    &plane.texture,
                    plane.info.color,
                    plane.info.contrast_min,
                    plane.info.contrast_max,
                    plane.info.opacity,
                )
            })
            .collect();
        state
            .renderer
            .draw_tile(&bound, &placement, first.dtype_max, Blend::Over);
    }
}

/// Transpose a row-major plane.
fn transpose(pixels: &[f32], width: u32, height: u32) -> Vec<f32> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0.0; pixels.len()];
    for row in 0..h {
        for column in 0..w {
            if let Some(value) = pixels.get(row * w + column) {
                out[column * h + row] = *value;
            }
        }
    }
    out
}
