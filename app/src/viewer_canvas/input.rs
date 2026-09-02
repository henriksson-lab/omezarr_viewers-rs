//! What a mouse or a finger on the canvas means.
//!
//! Every `ViewerMsg` a pointer produces is handled here, along with the two
//! questions those handlers keep asking: where in the world is this pointer,
//! and is it near enough to something to have grabbed it.

use std::collections::HashMap;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, MouseEvent, WheelEvent};
use yew::prelude::*;

use crate::webgl::context::GlContext;
use crate::webgl::renderer::Renderer;

use super::{
    camera_world, grab_at, is_worth_keeping as worth_keeping, Camera2d, Drawn, Editing, Tool,
    ViewerCanvas, ViewerCanvasState, ViewerMsg,
};
use super::{TileStore, TILE_BUDGET_BYTES};

impl ViewerCanvas {
    /// Take hold of the canvas element and build the renderer on it.
    pub(super) fn on_init(&mut self, ctx: &Context<Self>) -> bool {
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
                            tile_cache: TileStore::new(TILE_BUDGET_BYTES),
                            level_info: HashMap::new(),
                            current_level: HashMap::new(),
                            point_buffers: HashMap::new(),
                            annot_buffers: HashMap::new(),
                            draft: None,
                            editing: None,
                            pending: Vec::new(),
                            cursor: None,
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
            if let Some(window) = web_sys::window() {
                window.set_onresize(Some(closure.as_ref().unchecked_ref()));
                self._resize_closure = Some(closure);
            }
        }
        false
    }

    /// Start a pan, a draw, or a grab of an existing shape.
    pub(super) fn on_mouse_down(&mut self, ctx: &Context<Self>, e: MouseEvent) -> bool {
        let world = self.world_at(&e);
        let tool = ctx.props().tool;
        // Grabbing the selected annotation's handle outranks both the
        // camera and a new shape: the shape is already there and under
        // the cursor, so a drag on it can only mean "change this one".
        // Shift makes it a vertex delete or insert instead of a drag.
        let grabbed = world.and_then(|(x, y)| self.grab(ctx, x, y, e.shift_key()));
        if let Some(ref mut state) = *self.state.borrow_mut() {
            if let Some(editing) = grabbed {
                state.camera.dragging = false;
                state.draft = None;
                state.editing = Some(editing);
                return false;
            }
            // Shift is the vertex-editing modifier, so a shift-click
            // that misses a vertex or an edge does *nothing*. Panning
            // instead would send the picture sliding away from someone
            // who was aiming at a handle and was a few pixels out.
            if e.shift_key() && ctx.props().editable.is_some() {
                return false;
            }
            // A click-by-click tool collects vertices; the press only
            // starts a drag for the tools that are one.
            if tool.is_multi_click() {
                return false;
            }
            // A drawing tool owns the drag: the camera must not also
            // move, or a shape would be drawn in a frame that slid out
            // from under it. The decision is made here, on the press,
            // because `MouseMove` cannot tell the two apart afterwards.
            match world {
                Some((x, y)) if tool.draws() => {
                    state.camera.dragging = false;
                    state.draft = Some(Drawn {
                        tool,
                        points: vec![(x, y)],
                    });
                }
                _ => {
                    state.draft = None;
                    state.camera.dragging = true;
                    state.camera.dragged = false;
                    state.camera.last_mouse = (e.client_x() as f32, e.client_y() as f32);
                }
            }
        }
        false
    }

    /// Carry whichever of those is in progress.
    pub(super) fn on_mouse_move(&mut self, ctx: &Context<Self>, e: MouseEvent) -> bool {
        let mut needs_redraw = false;
        let world = self.world_at(&e);
        let tool = ctx.props().tool;
        if let Some(ref mut state) = *self.state.borrow_mut() {
            if !state.pending.is_empty() {
                state.cursor = world;
                needs_redraw = true;
            }
            if let (Some(editing), Some((x, y))) = (state.editing.as_mut(), world) {
                editing.to = (x, y);
                needs_redraw = true;
            } else if let (Some(draft), Some(point)) = (state.draft.as_mut(), world) {
                // A freehand tool records the whole path; a rectangle or
                // ellipse only ever needs where the drag started and
                // where it is now.
                match draft.tool {
                    Tool::Freehand | Tool::Line => draft.points.push(point),
                    _ => {
                        draft.points.truncate(1);
                        draft.points.push(point);
                    }
                }
                needs_redraw = true;
            } else if state.camera.dragging {
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
        let _ = tool;
        false
    }

    /// Finish it: emit the shape, the edit, or the plain click.
    pub(super) fn on_mouse_up(&mut self, ctx: &Context<Self>, e: MouseEvent) -> bool {
        let world = self.world_at(&e);
        let tool = ctx.props().tool;

        // An edit in progress finishes here.
        let edited = match *self.state.borrow_mut() {
            Some(ref mut state) => {
                let mut edited = state.editing.take();
                if let (Some(editing), Some((x, y))) = (edited.as_mut(), world) {
                    editing.to = (x, y);
                }
                edited
            }
            None => None,
        };
        if let Some(editing) = edited {
            if editing.changes() {
                ctx.props().on_edit.emit(editing);
            }
            ctx.link().send_message(ViewerMsg::Redraw);
            return false;
        }

        // A click-by-click tool adds a vertex, and closes when the
        // click lands back on the first one.
        if tool.is_multi_click() {
            if let Some((x, y)) = world {
                let finished = {
                    let mut finish = None;
                    if let Some(ref mut state) = *self.state.borrow_mut() {
                        let near = self.grab_tolerance(state);
                        let closes = tool.closes()
                            && state.pending.len() >= 3
                            && state.pending.first().is_some_and(|first| {
                                (first.0 - x).abs() <= near && (first.1 - y).abs() <= near
                            });
                        if closes {
                            finish = Some(std::mem::take(&mut state.pending));
                        } else {
                            state.pending.push((x, y));
                        }
                        state.cursor = Some((x, y));
                    }
                    finish
                };
                if let Some(points) = finished {
                    ctx.props().on_draw.emit(Drawn { tool, points });
                }
                ctx.link().send_message(ViewerMsg::Redraw);
            }
            return false;
        }

        let (was_dragging, was_click, drawn) = match *self.state.borrow_mut() {
            Some(ref mut state) => {
                let mut drawn = state.draft.take();
                if let (Some(draft), Some(point)) = (drawn.as_mut(), world) {
                    match draft.tool {
                        Tool::Freehand | Tool::Line => draft.points.push(point),
                        _ => {
                            draft.points.truncate(1);
                            draft.points.push(point);
                        }
                    }
                }
                let dragging = state.camera.dragging;
                let click = dragging && !state.camera.dragged;
                state.camera.dragging = false;
                state.camera.dragged = false;
                (dragging, click, drawn)
            }
            None => (false, false, None),
        };
        if let Some(drawn) = drawn {
            if self.is_worth_keeping(&drawn) {
                ctx.props().on_draw.emit(drawn);
            }
            ctx.link().send_message(ViewerMsg::Redraw);
            return false;
        }
        if was_click {
            if let Some(world) = world {
                ctx.props().on_pick.emit(world);
            }
        }
        if was_dragging {
            self.emit_camera_changed(ctx);
        }
        false
    }

    /// Close a click-by-click shape.
    pub(super) fn on_double_click(&mut self, ctx: &Context<Self>, e: MouseEvent) -> bool {
        // Finishing a click-by-click shape. A polygon closes; a polyline
        // is left open, which is the whole difference between them.
        let world = self.world_at(&e);
        let tool = ctx.props().tool;
        if !tool.is_multi_click() {
            return false;
        }
        let finished = {
            let mut finish = None;
            if let Some(ref mut state) = *self.state.borrow_mut() {
                if let Some(point) = world {
                    // The preceding `MouseUp` already added this click;
                    // a double-click must not add it twice.
                    if state.pending.last() != Some(&point) {
                        state.pending.push(point);
                    }
                }
                let enough = if tool.closes() { 3 } else { 2 };
                if state.pending.len() >= enough {
                    finish = Some(std::mem::take(&mut state.pending));
                } else {
                    state.pending.clear();
                }
                state.cursor = None;
            }
            finish
        };
        if let Some(points) = finished {
            ctx.props().on_draw.emit(Drawn { tool, points });
        }
        ctx.link().send_message(ViewerMsg::Redraw);
        false
    }

    /// Zoom about the pointer.
    pub(super) fn on_wheel(&mut self, ctx: &Context<Self>, e: WheelEvent) -> bool {
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

    /// A press, by finger.
    pub(super) fn on_touch_start(&mut self, _ctx: &Context<Self>, e: web_sys::TouchEvent) -> bool {
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

    /// Pan with one finger, pinch-zoom with two.
    pub(super) fn on_touch_move(&mut self, ctx: &Context<Self>, e: web_sys::TouchEvent) -> bool {
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

    /// Let go.
    pub(super) fn on_touch_end(&mut self, ctx: &Context<Self>, e: web_sys::TouchEvent) -> bool {
        e.prevent_default();
        if let Some(ref mut state) = *self.state.borrow_mut() {
            state.camera.dragging = false;
            state.camera.pinch_dist = None;
        }
        self.emit_camera_changed(ctx);
        false
    }

    /// Match the drawing buffer to the element.
    pub(super) fn on_resize(&mut self, ctx: &Context<Self>) -> bool {
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

    /// Draw, because something upstream changed.
    pub(super) fn on_redraw(&mut self, ctx: &Context<Self>) -> bool {
        self.redraw(ctx);
        false
    }

    /// Notify the parent App of the current camera state.
    pub(super) fn emit_camera_changed(&self, ctx: &Context<Self>) {
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
    pub(super) fn world_at(&self, e: &MouseEvent) -> Option<(f32, f32)> {
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

    /// How near, in world pixels, counts as "on" a handle.
    ///
    /// Derived from the zoom, because that is what "near" means to a hand: at
    /// 20x a vertex is a fraction of a world pixel away, and at 0.1x it is fifty
    /// of them.
    pub(super) fn grab_tolerance(&self, state: &ViewerCanvasState) -> f32 {
        let world = camera_world(state);
        if world.0 <= 0.0 || world.1 <= 0.0 {
            return 1.0;
        }
        let fit = state.camera.zoom
            * (state.camera.canvas_w / world.0).min(state.camera.canvas_h / world.1);
        (10.0 / fit.max(1e-6)).clamp(0.5, 1.0e6)
    }

    /// Which handle of the selected annotation is under `(x, y)`, if any.
    ///
    /// Two lookups and a decision. The decision is [`grab_at`], which is a pure
    /// function of the shape and the pointer and so can be tested without a
    /// browser — five of this viewer's interaction rules live in it, and each
    /// one was a bug before it was a rule.
    pub(super) fn grab(&self, ctx: &Context<Self>, x: f32, y: f32, shift: bool) -> Option<Editing> {
        let editable = ctx.props().editable.as_ref()?;
        let near = {
            let state = self.state.borrow();
            self.grab_tolerance(state.as_ref()?)
        };
        let (handle, kind) = grab_at(editable, x, y, shift, near)?;
        Some(Editing {
            id: editable.id,
            handle,
            kind,
            from: (x, y),
            to: (x, y),
        })
    }

    /// Is this drawn shape worth storing? See [`is_worth_keeping`].
    pub(super) fn is_worth_keeping(&self, drawn: &Drawn) -> bool {
        worth_keeping(drawn)
    }
}
