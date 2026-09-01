//! Putting it on the screen.
//!
//! One pass per layer in session order, then the draft shape and the handles on
//! top of everything: a half-drawn shape hidden under an opaque layer is a
//! shape the hand drawing it cannot see.

use web_sys::HtmlCanvasElement;
use yew::prelude::*;

use crate::webgl::renderer::{
    FillRenderInfo, LineRenderInfo, PointRenderInfo, Renderer, TextureKind, TilePlacement,
    TileTexture,
};

use super::{
    ellipse_path, rect_path, AnnotBuffer, EditKind, Handle, LayerRenderInfo, LayerRenderKind,
    TileKey, Tool, ViewerCanvas, ViewerCanvasState,
};

impl ViewerCanvas {
    /// Clear and redraw every layer, coarse levels first as a fallback.
    /// Clear and redraw every layer, coarse levels first as a fallback.
    pub(super) fn redraw(&self, ctx: &Context<Self>) {
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
            // Points and lines carry their own world position, so the
            // placement only has to say what the world is and where the camera
            // is looking.
            let world_placement = || TilePlacement {
                tile_offset: (0.0, 0.0),
                tile_size: (1.0, 1.0),
                image_size: world,
                canvas_size,
                pan: (state.camera.x, state.camera.y),
                zoom: state.camera.zoom,
            };
            if let LayerRenderKind::Objects(info) = &layer.kind {
                if let Some(points) = state.point_buffers.get(&layer.id) {
                    state.renderer.draw_points(points, &world_placement(), info);
                }
                continue;
            }
            if let LayerRenderKind::Annotations {
                points,
                lines,
                fills,
            } = &layer.kind
            {
                // Fills under outlines under points: a translucent region must
                // not swallow its own boundary, and a point dropped on an edge
                // should still read as a point.
                for batch in state.annot_buffers.get(&layer.id).into_iter().flatten() {
                    state.renderer.draw_fills(
                        &batch.fills,
                        &world_placement(),
                        &FillRenderInfo {
                            color: batch.color,
                            ..*fills
                        },
                    );
                }
                for batch in state.annot_buffers.get(&layer.id).into_iter().flatten() {
                    state.renderer.draw_lines(
                        &batch.lines,
                        &world_placement(),
                        &LineRenderInfo {
                            color: batch.color,
                            ..*lines
                        },
                    );
                    // Which path this batch's points take, and why. A world
                    // radius is drawn as a point sprite for as long as the
                    // device will make one that big — one draw call, and the
                    // vertex shader derives the size from the camera it is
                    // already applying. Past `ALIASED_POINT_SIZE_RANGE` a
                    // sprite is unspecified and in practice clamped, which
                    // would draw a radius nobody chose; so beyond that the
                    // circles become real geometry through the line program,
                    // which has no such ceiling. A screen-space marker never
                    // takes the geometry path: it has no radius to get wrong.
                    if batch.radius > 0.0
                        && !state
                            .renderer
                            .point_sprite_fits(&world_placement(), batch.radius)
                    {
                        self.draw_pick_circles(state, batch, &world_placement(), lines);
                        continue;
                    }
                    state.renderer.draw_points(
                        &batch.points,
                        &world_placement(),
                        &PointRenderInfo {
                            color: batch.color,
                            world_radius: batch.radius,
                            ..points.clone()
                        },
                    );
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

        self.draw_draft(state, world, canvas_size);
        self.draw_handles(ctx, state, world, canvas_size);
    }

    /// One batch's picks as circle *geometry*, for the zooms where a point
    /// sprite cannot be made large enough.
    ///
    /// Built and thrown away each frame, like the draft: at these zooms it is a
    /// handful of rings, and keeping a permanent buffer of them would mean
    /// 64 segments per pick sitting in GPU memory for every layer that has a
    /// radius, most of which never zooms this far.
    ///
    /// Only the picks on screen are built. A picked dataset is thousands of
    /// points, and at the zoom that gets here all but a few are outside the
    /// canvas — a per-frame loop over the whole set would be paid every frame
    /// to produce nothing.
    fn draw_pick_circles(
        &self,
        state: &ViewerCanvasState,
        batch: &AnnotBuffer,
        placement: &TilePlacement,
        info: &LineRenderInfo,
    ) {
        let scale = Renderer::world_scale(placement);
        if scale <= 0.0 {
            return;
        }
        let radius = batch.radius;
        // The world rectangle on screen, inverted from the same transform:
        // `to_clip` puts world `p` at `(p - size / 2) * scale + pan` screen
        // pixels from the centre, so the visible span is half a canvas either
        // side of it. A pick's own radius is the margin: one whose centre is
        // just off screen still has an arc on it.
        let half = (placement.canvas_size.0 / 2.0, placement.canvas_size.1 / 2.0);
        let centre = (placement.image_size.0 / 2.0, placement.image_size.1 / 2.0);
        let x0 = centre.0 + (-half.0 - placement.pan.0) / scale - radius;
        let x1 = centre.0 + (half.0 - placement.pan.0) / scale + radius;
        let y0 = centre.1 + (-half.1 - placement.pan.1) / scale - radius;
        let y1 = centre.1 + (half.1 - placement.pan.1) / scale + radius;

        // Enough segments that the ring reads as a circle rather than as a
        // polygon: roughly one every eight screen pixels of circumference,
        // which is what makes this indistinguishable from the sprite it took
        // over from.
        let segments = ((0.8 * radius * scale) as usize).clamp(32, 256);
        let step = std::f32::consts::TAU / segments as f32;

        let mut vertices: Vec<f32> = Vec::with_capacity(batch.markers.len() * segments * 10);
        for [x, y, z, selected] in batch.markers.iter().copied() {
            if x < x0 || x > x1 || y < y0 || y > y1 {
                continue;
            }
            // `z` twice: a point fades by its distance from the slice, so the
            // ring must use the span the point sprite would have used, not the
            // shape's z extent, or the two paths disagree about when it fades.
            let mut previous = (x + radius, y);
            for step_index in 1..=segments {
                let angle = step * step_index as f32;
                let next = (x + radius * angle.cos(), y + radius * angle.sin());
                vertices.extend_from_slice(&[previous.0, previous.1, z, z, selected]);
                vertices.extend_from_slice(&[next.0, next.1, z, z, selected]);
                previous = next;
            }
        }
        if vertices.is_empty() {
            return;
        }
        let Ok(buffer) = state.renderer.upload_lines(&vertices) else {
            return;
        };
        state.renderer.draw_lines(
            &buffer,
            placement,
            &LineRenderInfo {
                color: batch.color,
                ..*info
            },
        );
        state.renderer.delete_lines(&buffer);
    }

    /// Draw the shape being built right now, over everything else.
    ///
    /// Uploaded and deleted every frame. That is one small buffer per mouse
    /// move, which is nothing beside the tile draws it sits on, and it keeps the
    /// in-progress shape out of the layer state entirely — nothing downstream
    /// has to know the difference between a shape and a shape-so-far.
    pub(super) fn draw_draft(
        &self,
        state: &ViewerCanvasState,
        world: (f32, f32),
        canvas_size: (f32, f32),
    ) {
        let mut vertices: Vec<f32> = Vec::new();
        let mut segment = |a: (f32, f32), b: (f32, f32)| {
            // A z range that swallows every slice, and `selected` set: the draft
            // is the one thing on screen that must never fade.
            vertices.extend_from_slice(&[a.0, a.1, f32::NEG_INFINITY, f32::INFINITY, 1.0]);
            vertices.extend_from_slice(&[b.0, b.1, f32::NEG_INFINITY, f32::INFINITY, 1.0]);
        };

        if let Some(draft) = &state.draft {
            match draft.tool {
                Tool::Box => {
                    if let Some((x0, y0, x1, y1)) = draft.corners() {
                        for pair in rect_path(x0, y0, x1, y1).windows(2) {
                            segment(pair[0], pair[1]);
                        }
                    }
                }
                Tool::Ellipse => {
                    if let Some((x0, y0, x1, y1)) = draft.corners() {
                        for pair in ellipse_path(x0, y0, x1, y1).windows(2) {
                            segment(pair[0], pair[1]);
                        }
                    }
                }
                Tool::Freehand | Tool::Line => {
                    for pair in draft.points.windows(2) {
                        segment(pair[0], pair[1]);
                    }
                    // A freehand region shows its closing edge while it is being
                    // traced, so the shape you are about to get is the shape you
                    // can see.
                    if draft.tool.closes() && draft.points.len() > 2 {
                        if let (Some(first), Some(last)) =
                            (draft.points.first(), draft.points.last())
                        {
                            segment(*last, *first);
                        }
                    }
                }
                Tool::Point | Tool::Pan | Tool::Polygon | Tool::Polyline => {}
            }
        }

        // A click-by-click shape: the vertices placed so far, plus a rubber band
        // to wherever the pointer is.
        if !state.pending.is_empty() {
            for pair in state.pending.windows(2) {
                segment(pair[0], pair[1]);
            }
            if let (Some(last), Some(cursor)) = (state.pending.last(), state.cursor) {
                segment(*last, cursor);
                if let Some(first) = state.pending.first() {
                    if state.pending.len() >= 2 {
                        segment(cursor, *first);
                    }
                }
            }
        }

        if vertices.is_empty() {
            return;
        }
        let Ok(buffer) = state.renderer.upload_lines(&vertices) else {
            return;
        };
        state.renderer.draw_lines(
            &buffer,
            &Self::world_placement(state, world, canvas_size),
            &LineRenderInfo {
                color: [1.0, 1.0, 1.0],
                opacity: 0.9,
                z: 0.0,
                slab: 0.0,
            },
        );
        state.renderer.delete_lines(&buffer);
    }

    /// A placement that says only what the world is and where the camera is.
    pub(super) fn world_placement(
        state: &ViewerCanvasState,
        world: (f32, f32),
        canvas_size: (f32, f32),
    ) -> TilePlacement {
        TilePlacement {
            tile_offset: (0.0, 0.0),
            tile_size: (1.0, 1.0),
            image_size: world,
            canvas_size,
            pan: (state.camera.x, state.camera.y),
            zoom: state.camera.zoom,
        }
    }

    /// The selected annotation's handles, and its shape while being dragged.
    ///
    /// Drawn from the live drag rather than from the layer state: the state only
    /// changes when the server answers, and a shape that lags the pointer by a
    /// round trip is a shape nobody can place.
    pub(super) fn draw_handles(
        &self,
        ctx: &Context<Self>,
        state: &ViewerCanvasState,
        world: (f32, f32),
        canvas_size: (f32, f32),
    ) {
        let Some(editable) = ctx.props().editable.as_ref() else {
            return;
        };
        let editing = state.editing.filter(|e| e.id == editable.id);
        let (dx, dy) = editing.map(|e| e.delta()).unwrap_or((0.0, 0.0));

        // Where each handle is *right now*, given the drag in progress.
        let mut handles: Vec<(f32, f32)> = Vec::new();
        let mut outline: Vec<Vec<(f32, f32)>> = Vec::new();

        if editable.boxlike || editable.puncta {
            let (mut x0, mut y0, mut x1, mut y1) = editable.bounds;
            match editing.map(|e| e.handle) {
                Some(Handle::Body) => {
                    x0 += dx;
                    x1 += dx;
                    y0 += dy;
                    y1 += dy;
                }
                Some(Handle::Corner(west, north)) => {
                    if west {
                        x0 += dx;
                    } else {
                        x1 += dx;
                    }
                    if north {
                        y0 += dy;
                    } else {
                        y1 += dy;
                    }
                }
                _ => {}
            }
            let (x0, x1) = (x0.min(x1), x0.max(x1));
            let (y0, y1) = (y0.min(y1), y0.max(y1));
            outline.push(rect_path(x0, y0, x1, y1));
            if !editable.puncta {
                handles.extend([(x0, y0), (x1, y0), (x0, y1), (x1, y1)]);
            }
        } else {
            for (path_index, path) in editable.paths.iter().enumerate() {
                let moved: Vec<(f32, f32)> = path
                    .iter()
                    .enumerate()
                    .map(|(vertex, point)| match editing.map(|e| e.handle) {
                        Some(Handle::Body) => (point.0 + dx, point.1 + dy),
                        Some(Handle::Vertex(p, v))
                            if p == path_index
                                && v == vertex
                                && editing.map(|e| e.kind) == Some(EditKind::Drag) =>
                        {
                            (point.0 + dx, point.1 + dy)
                        }
                        _ => *point,
                    })
                    .collect();
                handles.extend(moved.iter().copied());
                outline.push(moved);
            }
        }

        // Handles are a fixed *screen* size, so they stay grabbable at any zoom
        // — the same reason `grab` measures its tolerance there.
        let fit = state.camera.zoom * (canvas_size.0 / world.0).min(canvas_size.1 / world.1);
        let r = (4.0 / fit.max(1e-6)).clamp(0.25, 1.0e6);

        let mut vertices: Vec<f32> = Vec::new();
        let mut segment = |a: (f32, f32), b: (f32, f32)| {
            vertices.extend_from_slice(&[a.0, a.1, f32::NEG_INFINITY, f32::INFINITY, 1.0]);
            vertices.extend_from_slice(&[b.0, b.1, f32::NEG_INFINITY, f32::INFINITY, 1.0]);
        };
        for path in &outline {
            for pair in path.windows(2) {
                segment(pair[0], pair[1]);
            }
        }
        for (cx, cy) in handles {
            for pair in rect_path(cx - r, cy - r, cx + r, cy + r).windows(2) {
                segment(pair[0], pair[1]);
            }
        }
        if vertices.is_empty() {
            return;
        }

        let Ok(buffer) = state.renderer.upload_lines(&vertices) else {
            return;
        };
        state.renderer.draw_lines(
            &buffer,
            &Self::world_placement(state, world, canvas_size),
            &LineRenderInfo {
                color: [1.0, 1.0, 1.0],
                opacity: 0.85,
                z: 0.0,
                slab: 0.0,
            },
        );
        state.renderer.delete_lines(&buffer);
    }

    /// Draw every cached tile of one layer at one level.
    pub(super) fn draw_layer_level(
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
                    // Both draw in world coordinates, above, not per tile.
                    LayerRenderKind::Objects(_) | LayerRenderKind::Annotations { .. } => {}
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
