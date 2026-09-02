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
//!
//! What it *does* keep is the planes it has already read (`PlaneStore`), so
//! scrubbing z back over ground it has covered is a texture lookup rather than
//! a round trip, a decode and an upload. The server already caches the response
//! — planes go through the same tile cache, with the axis in the projection
//! slot — so the repeat was cheap there and expensive here, which is exactly
//! the shape of thing a client cache fixes. It is the *same* store the main
//! view uses (`TileStore`), for the same two reasons: a texture has to be
//! deleted explicitly, and a cap has to be in bytes.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::api_client;
use crate::viewer_canvas::{ChannelRenderInfo, TileStore};
use crate::webgl::context::GlContext;
use crate::webgl::renderer::{Blend, Renderer, TilePlacement, TileTexture};

/// How much GPU memory **one pane's** planes may hold.
///
/// Per pane, because a texture belongs to the context that made it and the
/// panes have one each; the 2x2 grid therefore budgets twice this, next to the
/// main view's own 256 MiB. A plane at a level the pane fits is a few
/// megabytes, so this holds a scrub of some tens of z positions — the range a
/// hand actually moves over — and gives the rest back.
pub const PLANE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

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

/// What identifies one cached plane texture.
///
/// Everything the request varies, plus the one thing it does not: `transpose`,
/// because the texture is uploaded *after* the transpose and a plane read for
/// the other orientation is the wrong picture rather than a slow one. The pane
/// size is deliberately absent — it decides which `level` the app asks for, and
/// the level is here; the plane's own size follows from `(layer, axis, level)`
/// and nothing else. A cached texture is therefore never stale for its key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PlaneKey {
    layer: String,
    axis: &'static str,
    level: usize,
    index: u64,
    t: u64,
    channel: u64,
    transpose: bool,
}

/// The pane's plane cache.
///
/// Eviction is `TileStore`'s: insertion-order, oldest first, capped in bytes,
/// handing every dropped texture back to be deleted. A window around the
/// current index was the alternative and is not better here — for a scrub,
/// insertion order *is* visit order, so the oldest entry is already the index
/// furthest from where the hand has been, and a window has to be re-centred on
/// every move and throws away the far side of a reversal, which is precisely
/// what a scrub back and forth crosses.
type PlaneStore = TileStore<TileTexture, PlaneKey>;

/// One channel of one layer, in draw order.
struct PlaneSlot {
    key: PlaneKey,
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
    /// What this pane is currently showing, bottom to top; a slot is `None`
    /// until its plane has arrived.
    ///
    /// Slots rather than a growing list because arrival order is no longer draw
    /// order: with a cache a hit is filled in the same frame the reload starts
    /// and a miss a round trip later, so the first plane to *finish* says
    /// nothing about which one is the bottom layer.
    slots: Vec<Option<PlaneSlot>>,
    planes: PlaneStore,
}

pub enum PaneMsg {
    Init,
    Reload,
    Loaded {
        generation: u64,
        slot: usize,
        key: PlaneKey,
        pixels: Vec<f32>,
        width: u32,
        height: u32,
        info: ChannelRenderInfo,
        dtype_max: f32,
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
                            slots: Vec::new(),
                            planes: PlaneStore::new(PLANE_BUDGET_BYTES),
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

                // The slot list is built first and in full, so the draw order is
                // the props' order however the fetches finish.
                let wanted: Vec<(PlaneKey, ChannelRenderInfo, f32)> = props
                    .layers
                    .iter()
                    .flat_map(|layer| {
                        layer.channels.iter().map(move |(channel, info)| {
                            (
                                PlaneKey {
                                    layer: layer.id.clone(),
                                    axis: props.axis,
                                    level: layer.level,
                                    index: layer.index,
                                    t: props.t,
                                    channel: *channel as u64,
                                    transpose: props.transpose,
                                },
                                info.clone(),
                                layer.dtype_max,
                            )
                        })
                    })
                    .collect();

                let mut missing = Vec::new();
                {
                    let mut state_ref = self.state.borrow_mut();
                    let Some(state) = state_ref.as_mut() else {
                        return false;
                    };
                    let slots: Vec<Option<PlaneSlot>> = wanted
                        .iter()
                        .enumerate()
                        .map(|(slot, (key, info, dtype_max))| {
                            // Already on the GPU: this is the whole point.
                            if state.planes.contains_key(key) {
                                Some(PlaneSlot {
                                    key: key.clone(),
                                    info: info.clone(),
                                    dtype_max: *dtype_max,
                                })
                            } else {
                                missing.push(slot);
                                None
                            }
                        })
                        .collect();
                    state.slots = slots;
                }

                for slot in missing {
                    let (key, info, dtype_max) = wanted[slot].clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        let fetched = api_client::fetch_slice(
                            &key.layer,
                            key.axis,
                            key.index,
                            key.level,
                            key.t,
                            key.channel,
                        )
                        .await;
                        match fetched {
                            Ok(plane) => link.send_message(PaneMsg::Loaded {
                                generation,
                                slot,
                                key,
                                pixels: plane.pixels,
                                width: plane.width,
                                height: plane.height,
                                info,
                                dtype_max,
                            }),
                            Err(e) => log::warn!("ortho plane: {}", e),
                        }
                    });
                }
                // Draw what the cache already had — and, with nothing wanted,
                // clear, so a stale plane does not linger.
                self.draw();
                false
            }
            PaneMsg::Loaded {
                generation,
                slot,
                key,
                pixels,
                width,
                height,
                info,
                dtype_max,
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
                    match state.renderer.upload_tile(width, height, &pixels) {
                        Ok(texture) => {
                            let freed = state.planes.insert(key.clone(), texture, width, height);
                            state.renderer.delete_tiles(freed);
                            if let Some(slot) = state.slots.get_mut(slot) {
                                *slot = Some(PlaneSlot {
                                    key,
                                    info,
                                    dtype_max,
                                });
                            }
                        }
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
        // read again. "Read" is now usually "found": a contrast drag or a step
        // back in z lands on planes the cache still holds and never leaves the
        // client.
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

    fn destroy(&mut self, _ctx: &Context<Self>) {
        // The context goes with the canvas, but a texture is freed when it is
        // deleted, not when its wrapper is dropped — the same reason eviction
        // hands textures back rather than dropping them.
        if let Some(state) = self.state.borrow_mut().as_mut() {
            let all = state.planes.retain(|_| false);
            state.renderer.delete_tiles(all);
        }
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
    /// Draw every plane the slots have, stretched to fill the pane.
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

        // A slot whose texture the budget took back is skipped rather than
        // waited for: the rest of the stack is still the right picture.
        let bound: Vec<(&TileTexture, [f32; 3], f32, f32, f32)> = state
            .slots
            .iter()
            .flatten()
            .filter_map(|slot| {
                let texture = state.planes.get(&slot.key)?;
                Some((
                    texture,
                    slot.info.color,
                    slot.info.contrast_min,
                    slot.info.contrast_max,
                    slot.info.opacity,
                ))
            })
            .collect();
        if bound.is_empty() {
            return;
        }
        let dtype_max = state
            .slots
            .iter()
            .flatten()
            .map(|slot| slot.dtype_max)
            .next()
            .unwrap_or(1.0);

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
        state
            .renderer
            .draw_tile(&bound, &placement, dtype_max, Blend::Over);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewer_canvas::bytes_for;

    fn key(index: u64) -> PlaneKey {
        PlaneKey {
            layer: "L0".into(),
            axis: "y",
            level: 1,
            index,
            t: 0,
            channel: 0,
            transpose: false,
        }
    }

    /// The texture is a JS object, so the cache is exercised the way
    /// `TileStore`'s own tests do it: with `()` in the payload's place, since
    /// nothing about the accounting or the eviction depends on what is stored.
    type TestStore = TileStore<(), PlaneKey>;

    #[test]
    fn every_axis_of_the_request_separates_two_planes() {
        // The bug this guards: a key that misses one of these serves the plane
        // of another time point, another channel or another pyramid level, and
        // does it *instantly*, which is the hardest kind to notice.
        let base = key(3);
        let variants = [
            PlaneKey {
                layer: "L1".into(),
                ..base.clone()
            },
            PlaneKey {
                axis: "x",
                ..base.clone()
            },
            PlaneKey {
                level: 0,
                ..base.clone()
            },
            key(4),
            PlaneKey {
                t: 1,
                ..base.clone()
            },
            PlaneKey {
                channel: 1,
                ..base.clone()
            },
            PlaneKey {
                transpose: true,
                ..base.clone()
            },
        ];
        let mut store: TestStore = TileStore::new(0);
        let _ = store.insert(base.clone(), (), 8, 8);
        for variant in variants {
            assert!(
                !store.contains_key(&variant),
                "{variant:?} must not hit the cached {base:?}"
            );
        }
    }

    #[test]
    fn a_scrub_is_bounded_by_bytes_and_drops_the_oldest_index() {
        // Four planes' worth of budget, then a walk over ten z: what survives
        // is where the hand is now, not where it started.
        let plane = bytes_for(64, 32);
        let mut store: TestStore = TileStore::new(plane * 4);
        let mut deleted = 0;
        for index in 0..10 {
            deleted += store.insert(key(index), (), 64, 32).len();
        }
        assert_eq!(store.len(), 4, "the budget holds four");
        assert_eq!(store.bytes(), plane * 4, "the accounting follows eviction");
        assert_eq!(deleted, 6, "every evicted texture is handed back");
        for index in 6..10 {
            assert!(store.contains_key(&key(index)), "the recent four stayed");
        }
        assert!(!store.contains_key(&key(5)), "the sixth-oldest went");
    }

    #[test]
    fn scrubbing_back_over_a_held_plane_costs_nothing() {
        let mut store: TestStore = TileStore::new(bytes_for(64, 32) * 4);
        for index in 0..3 {
            let _ = store.insert(key(index), (), 64, 32);
        }
        // Stepping back is a hit, and a hit does not re-enter the store: the
        // whole saving is that no fetch and no upload happen at all.
        assert!(store.contains_key(&key(1)));
        assert_eq!(store.bytes(), bytes_for(64, 32) * 3);
    }

    #[test]
    fn a_plane_larger_than_the_whole_budget_is_still_drawable() {
        // A pane at a fine level over a deep stack can ask for one plane bigger
        // than its own cache. Evicting it on the way in would leave the pane
        // blank for ever, which is worse than being briefly over budget.
        let mut store: TestStore = TileStore::new(1024);
        let freed = store.insert(key(0), (), 512, 512).len();
        assert_eq!(freed, 0);
        assert!(store.contains_key(&key(0)));
        // ...and the next insertion pushes it back out, so the excess is one
        // plane and not a leak.
        let _ = store.insert(key(1), (), 512, 512);
        assert_eq!(store.len(), 1);
        assert!(store.contains_key(&key(1)));
    }
}
