//! Drawing, editing, classifying and saving annotations.

use yew::prelude::*;

use wasm_bindgen_futures::spawn_local;

use omezarr_viewer_common::{Annotation, Plane};

use crate::api_client;
use crate::layers::{AnnotUiState, LayerState, LayerUi};
use crate::viewer_canvas::{AnnotBuffer, Drawn, Editable, Editing, Tool};

use super::{apply_edit, geometry_of, is_axis_aligned_rect, Undo};
use super::{AnnotStoreMsg, App, AppMsg, SessionMsg};

pub enum AnnotMsg {
    SetTool(Tool),
    /// Abandon a half-drawn shape and drop back to the pan tool.
    CancelDrawing,
    /// A shape the user finished drawing on the canvas.
    Drew(Drawn),
    /// One annotation as the server stored it, id and all.
    Added(String, Box<Annotation>),
    Select(usize, Option<u64>),
    /// A drag that moved, resized or reshaped an existing annotation.
    Edit(Editing),
    /// One annotation as the server stored it after an update.
    Updated(String, Box<Annotation>),
    Undo,
}

impl From<AnnotMsg> for AppMsg {
    fn from(msg: AnnotMsg) -> Self {
        AppMsg::Annot(msg)
    }
}

impl App {
    pub(super) fn update_annotations(&mut self, ctx: &Context<Self>, msg: AnnotMsg) -> bool {
        match msg {
            AnnotMsg::Edit(editing) => self.finish_edit(ctx, editing),
            AnnotMsg::Updated(layer, annotation) => {
                // The server normalises — a backwards drag comes back the right
                // way round — so the authoritative row replaces the local one
                // rather than the local one being assumed correct.
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let changed = self.annot_mut(index).is_some_and(|state| {
                    match state.annotations.iter_mut().find(|a| a.id == annotation.id) {
                        Some(slot) if *slot != *annotation => {
                            *slot = *annotation;
                            true
                        }
                        _ => false,
                    }
                });
                if changed {
                    self.rebuild_annotations(index);
                }
                changed
            }
            AnnotMsg::Undo => self.undo(ctx),
            AnnotMsg::CancelDrawing => {
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        state.pending.clear();
                        state.cursor = None;
                        state.draft = None;
                        state.editing = None;
                    }
                }
                self.tool = Tool::Pan;
                true
            }
            AnnotMsg::SetTool(tool) => {
                // A polygon half-placed under one tool is not a polygon under
                // the next, so switching abandons it rather than leaving stray
                // vertices to be swept into whatever is drawn next.
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        state.pending.clear();
                        state.cursor = None;
                        state.draft = None;
                    }
                }
                self.tool = tool;
                // A drawing tool with nowhere to draw would swallow clicks and
                // do nothing, so picking one makes the layer it needs.
                if tool.draws() && self.annot_target.is_none() {
                    ctx.link().send_message(AnnotStoreMsg::AddLayer);
                }
                true
            }
            AnnotMsg::Drew(drawn) => self.finish_drawing(ctx, drawn),
            AnnotMsg::Added(layer, annotation) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                self.remember(Undo::Added {
                    layer,
                    id: annotation.id,
                });
                if let Some(state) = self.annot_mut(index) {
                    state.selected = Some(annotation.id);
                    state.annotations.push(*annotation);
                    state.dirty = true;
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotMsg::Select(index, id) => {
                if let Some(state) = self.annot_mut(index) {
                    state.selected = id;
                }
                self.rebuild_annotations(index);
                true
            }
        }
    }
}

impl App {
    pub(super) fn annot_mut(&mut self, layer: usize) -> Option<&mut AnnotUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Annotations(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn annotation_layers(&self) -> usize {
        self.layers.iter().filter(|l| l.is_annotations()).count()
    }

    /// Does this layer hold an annotation with this id?
    fn holds(layer: &LayerState, id: u64) -> bool {
        matches!(&layer.ui, LayerUi::Annotations(state)
            if state.annotations.iter().any(|a| a.id == id))
    }

    /// The layer id and selected annotation id, for the per-object controls.
    pub(super) fn selected_in(&mut self, index: usize) -> Option<(String, u64)> {
        let layer = self.layers.get(index)?.id.clone();
        let id = self.annot_mut(index)?.selected?;
        Some((layer, id))
    }

    /// Change one field of the selected annotation and send it.
    ///
    /// Shared by every per-object control — depth, duration, name, type, lock —
    /// which differ only in what they set, and all of which have to record an
    /// undo step, mark the layer dirty and send the row. That is the part worth
    /// not writing five times.
    pub(super) fn edit_selected(
        &mut self,
        ctx: &Context<Self>,
        index: usize,
        layer: String,
        id: u64,
        change: impl FnOnce(&mut Annotation),
    ) {
        let Some(state) = self.annot_mut(index) else {
            return;
        };
        let Some(item) = state.annotations.iter_mut().find(|a| a.id == id) else {
            return;
        };
        let before = item.clone();
        change(item);
        let updated = item.clone();
        if updated == before {
            return;
        }
        state.dirty = true;
        self.remember(Undo::Restore {
            layer: layer.clone(),
            annotation: Box::new(before),
            deleted: false,
        });
        self.rebuild_annotations(index);
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::update_annotation(&layer, &updated).await {
                Ok(stored) => link.send_message(AnnotMsg::Updated(layer, Box::new(stored))),
                Err(e) => log::warn!("set extent: {e}"),
            }
        });
    }

    /// The selected annotation, as the canvas needs it for drag handles.
    ///
    /// Only from a *visible* layer, and only when a drawing tool is not active:
    /// handles that stay grabbable while a shape tool is out would make every
    /// drag near an existing shape an edit rather than a new shape.
    pub(super) fn editable(&self) -> Option<Editable> {
        if self.tool.draws() {
            return None;
        }
        let (z, t) = (self.z_slice as i32, self.t_index as i32);
        for layer in self.layers.iter().rev() {
            if !layer.visible {
                continue;
            }
            let LayerUi::Annotations(state) = &layer.ui else {
                continue;
            };
            let id = state.selected?;
            let item = state.get(id)?;
            if !state.shows(item, z, t) {
                return None;
            }
            let [x0, y0, x1, y1] = item.bounds()?;
            // A rectangle or an ellipse is edited by its bounding corners;
            // everything else by its own vertices. `outlines` gives the rings
            // and lines in the same order `with_path` walks them, which is what
            // keeps a handle index meaning the same thing at both ends.
            let boxlike = item.is_ellipse || is_axis_aligned_rect(item);
            let paths = if boxlike {
                Vec::new()
            } else {
                item.geometry
                    .outlines()
                    .iter()
                    .map(|path| {
                        // The repeated closing vertex is not a separate handle:
                        // it is the first one, and drawing two on top of each
                        // other makes the shape look broken.
                        let mut path = path.clone();
                        if path.len() > 1 && path.first() == path.last() {
                            path.pop();
                        }
                        path.iter().map(|p| (p[0] as f32, p[1] as f32)).collect()
                    })
                    .collect()
            };
            return Some(Editable {
                id,
                bounds: (x0 as f32, y0 as f32, x1 as f32, y1 as f32),
                paths,
                boxlike,
                puncta: item.is_point(),
                locked: item.locked,
            });
        }
        None
    }

    /// Is there annotation work that closing the tab would lose?
    pub(super) fn unsaved_annotations(&self) -> bool {
        self.layers
            .iter()
            .any(|layer| matches!(&layer.ui, LayerUi::Annotations(state) if state.dirty))
    }

    /// Re-upload every annotation layer's buffers.
    ///
    /// Called from both sides of the startup race: the session can arrive
    /// before the canvas exists or after it, and whichever happens second is
    /// the one that has to do the upload. Doing it twice costs one buffer
    /// rebuild; doing it neither time draws nothing at all, which is what a
    /// reopened ROI table looked like before this existed.
    pub(super) fn upload_annotations(&mut self) {
        for index in 0..self.layers.len() {
            if self.layers[index].is_annotations() {
                self.rebuild_annotations(index);
            }
        }
    }

    /// Re-upload one annotation layer's GPU buffers from its rows.
    ///
    /// Rebuilt whole on every edit, and one batch per colour the layer draws
    /// in. The set is what a person drew by hand, so the buffers are kilobytes,
    /// and rebuilding them is what keeps "the picture matches the list" from
    /// being a thing that can drift.
    pub(super) fn rebuild_annotations(&mut self, index: usize) {
        let Some(layer) = self.layers.get(index) else {
            return;
        };
        let LayerUi::Annotations(state) = &layer.ui else {
            return;
        };
        let id = layer.id.clone();
        let batches = state.batches(self.z_slice as i32, self.t_index as i32);
        let Some(cs) = &self.canvas_state else { return };
        let Some(ref mut canvas) = *cs.borrow_mut() else {
            return;
        };
        for old in canvas.annot_buffers.remove(&id).into_iter().flatten() {
            canvas.renderer.delete_points(&old.points);
            canvas.renderer.delete_lines(&old.lines);
            canvas.renderer.delete_fills(&old.fills);
        }
        let mut uploaded = Vec::with_capacity(batches.len());
        for batch in batches {
            match (
                canvas.renderer.upload_points(&batch.points),
                canvas.renderer.upload_lines(&batch.lines),
                canvas.renderer.upload_fills(&batch.fills),
            ) {
                (Ok(points), Ok(lines), Ok(fills)) => uploaded.push(AnnotBuffer {
                    color: batch.color,
                    points,
                    lines,
                    fills,
                }),
                (points, lines, fills) => {
                    // Part of an uploaded batch is a leak, so whichever parts
                    // succeeded are released before the failure is reported.
                    if let Ok(points) = points {
                        canvas.renderer.delete_points(&points);
                    }
                    if let Ok(lines) = lines {
                        canvas.renderer.delete_lines(&lines);
                    }
                    if let Ok(fills) = fills {
                        canvas.renderer.delete_fills(&fills);
                    }
                    log::warn!("upload annotation batch: buffers refused");
                }
            }
        }
        canvas.annot_buffers.insert(id, uploaded);
    }

    /// Turn a finished gesture into an annotation on the target layer.
    fn finish_drawing(&mut self, ctx: &Context<Self>, drawn: Drawn) -> bool {
        let Some(layer) = self.annot_target.clone() else {
            self.error = Some("no annotation layer to draw into".into());
            return true;
        };
        let (class, object_type) = self
            .layers
            .iter()
            .find(|l| l.id == layer)
            .and_then(|l| match &l.ui {
                LayerUi::Annotations(state) => Some((state.class.clone(), state.object_type)),
                _ => None,
            })
            .unwrap_or_default();
        let Some((geometry, is_ellipse)) = geometry_of(&drawn) else {
            return false;
        };
        let annotation = Annotation {
            geometry,
            is_ellipse,
            label: class,
            object_type,
            // The plane the shape was drawn on. One plane, not a span:
            // there is no handle for depth in a 2D view, and the panel
            // is where a span gets widened.
            plane: Plane::at(self.z_slice as i32, self.t_index as i32),
            ..Default::default()
        };
        let link = ctx.link().clone();
        let id = layer.clone();
        spawn_local(async move {
            match api_client::add_annotation(&id, &annotation).await {
                Ok(stored) => link.send_message(AnnotMsg::Added(id, Box::new(stored))),
                Err(e) => link.send_message(SessionMsg::LoadError(e)),
            }
        });
        false
    }

    /// Apply a finished drag to the shape it grabbed, and send it on.
    fn finish_edit(&mut self, ctx: &Context<Self>, editing: Editing) -> bool {
        let Some(index) = self
            .layers
            .iter()
            .position(|l| l.is_annotations() && l.visible && Self::holds(l, editing.id))
        else {
            return false;
        };
        let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
            return false;
        };
        let Some(state) = self.annot_mut(index) else {
            return false;
        };
        let Some(item) = state.annotations.iter_mut().find(|a| a.id == editing.id) else {
            return false;
        };
        let before = item.clone();
        if !apply_edit(item, &editing) {
            return false;
        }
        let updated = item.clone();
        if updated == before {
            return false;
        }
        state.dirty = true;
        self.remember(Undo::Restore {
            layer: layer.clone(),
            annotation: Box::new(before),
            deleted: false,
        });
        self.rebuild_annotations(index);
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::update_annotation(&layer, &updated).await {
                Ok(stored) => link.send_message(AnnotMsg::Updated(layer, Box::new(stored))),
                Err(e) => log::warn!("edit annotation: {e}"),
            }
        });
        true
    }
}
