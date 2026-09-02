//! Changing an annotation that already exists: what it is called, what it is, where it sits in the hierarchy, and whether it still exists.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::{Annotation, ObjectType};

use crate::api_client;

use super::{App, AppMsg, SessionMsg, Undo};
pub enum AnnotEditMsg {
    SetClass(usize, String),
    Rename(usize, u64, String),
    Delete(usize, u64),
    Removed(String, u64),
    /// Per-object, on the selected annotation.
    SetName(usize, String),
    SetObjectType(usize, ObjectType),
    SetLocked(usize, bool),
    /// The selected shape's stroke width, in world pixels; `None` makes it a
    /// geometric line again.
    SetStrokeWidth(usize, Option<f64>),
    /// Whether the selected shape asserts that everything inside it is
    /// annotated.
    SetDense(usize, bool),
    /// Rebuild a layer's hierarchy from where its shapes now are.
    Renest(usize),
    /// Lift the selected annotation out of its parent.
    Detach(usize),
    /// A layer's rows as the server now has them, after a structural change.
    Replaced(String, Vec<Annotation>),
    DeleteAll(usize),
}

impl From<AnnotEditMsg> for AppMsg {
    fn from(msg: AnnotEditMsg) -> Self {
        AppMsg::AnnotEdit(msg)
    }
}

impl App {
    pub(super) fn update_annot_edit(&mut self, ctx: &Context<Self>, msg: AnnotEditMsg) -> bool {
        match msg {
            AnnotEditMsg::SetName(index, name) => {
                let Some((layer, id)) = self.selected_in(index) else {
                    return false;
                };
                self.edit_selected(ctx, index, layer, id, move |item| {
                    item.name = (!name.trim().is_empty()).then_some(name);
                });
                true
            }
            AnnotEditMsg::SetObjectType(index, kind) => {
                let Some((layer, id)) = self.selected_in(index) else {
                    return false;
                };
                self.edit_selected(ctx, index, layer, id, move |item| {
                    item.object_type = kind;
                });
                true
            }
            AnnotEditMsg::SetLocked(index, locked) => {
                let Some((layer, id)) = self.selected_in(index) else {
                    return false;
                };
                self.edit_selected(ctx, index, layer, id, move |item| item.locked = locked);
                true
            }
            AnnotEditMsg::SetStrokeWidth(index, width) => {
                let Some((layer, id)) = self.selected_in(index) else {
                    return false;
                };
                // A width of zero is not stored: `None` is the geometric line,
                // and two spellings of "covers nothing" would be one too many.
                let width = width.filter(|w| *w > 0.0);
                self.edit_selected(ctx, index, layer, id, move |item| {
                    item.stroke_width = width;
                });
                true
            }
            AnnotEditMsg::SetDense(index, dense) => {
                let Some((layer, id)) = self.selected_in(index) else {
                    return false;
                };
                self.edit_selected(ctx, index, layer, id, move |item| {
                    item.dense_region = dense;
                });
                true
            }
            AnnotEditMsg::Renest(index) => self.restructure(ctx, index, true),
            AnnotEditMsg::Detach(index) => self.restructure(ctx, index, false),
            AnnotEditMsg::Replaced(layer, rows) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                if let Some(state) = self.annot_mut(index) {
                    state.annotations = rows;
                    state.selected = state
                        .selected
                        .filter(|id| state.annotations.iter().any(|a| a.id == *id));
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotEditMsg::DeleteAll(index) => self.delete_all(index),
            AnnotEditMsg::SetClass(index, class) => {
                if let Some(state) = self.annot_mut(index) {
                    state.class = class;
                }
                true
            }
            AnnotEditMsg::Rename(index, id, class) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.annot_mut(index) else {
                    return false;
                };
                let Some(item) = state.annotations.iter_mut().find(|a| a.id == id) else {
                    return false;
                };
                if item.label == class {
                    return false;
                }
                let before = item.clone();
                item.label = class;
                let updated = item.clone();
                self.store_edit(ctx, index, layer, before, updated, "rename annotation");
                true
            }
            AnnotEditMsg::Delete(index, id) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::remove_annotation(&layer, id).await {
                        Ok(()) => link.send_message(AnnotEditMsg::Removed(layer, id)),
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                false
            }
            AnnotEditMsg::Removed(layer, id) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let removed = self.annot_mut(index).and_then(|state| {
                    let gone = state.annotations.iter().position(|a| a.id == id)?;
                    let removed = state.annotations.remove(gone);
                    if state.selected == Some(id) {
                        state.selected = None;
                    }
                    state.dirty = true;
                    Some(removed)
                });
                if let Some(removed) = removed {
                    self.remember(Undo::Restore {
                        layer,
                        annotation: Box::new(removed),
                        deleted: true,
                    });
                }
                self.rebuild_annotations(index);
                true
            }
        }
    }

    /// Rebuild a layer's hierarchy, or lift one shape out of its parent.
    ///
    /// Both go to the server and come back as a whole new row set: nesting is
    /// the server's rule to apply, and half of it applied here would be a tree
    /// the file disagrees with.
    fn restructure(&mut self, ctx: &Context<Self>, index: usize, renest: bool) -> bool {
        let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
            return false;
        };
        let selected = self.annot_mut(index).and_then(|state| state.selected);
        // The whole layer's parents can move, so the undo step is the
        // whole layer — a per-row inverse would be a stack of them that
        // only makes sense applied together.
        let before = self
            .annot_mut(index)
            .map(|state| state.annotations.clone())
            .unwrap_or_default();
        self.remember(Undo::RestoreAll {
            layer: layer.clone(),
            annotations: before,
        });
        if let Some(state) = self.annot_mut(index) {
            state.dirty = true;
        }
        let link = ctx.link().clone();
        spawn_local(async move {
            let rows = if renest {
                api_client::renest_annotations(&layer).await
            } else {
                match selected {
                    Some(id) => api_client::detach_annotation(&layer, id).await,
                    None => return,
                }
            };
            match rows {
                Ok(rows) => link.send_message(AnnotEditMsg::Replaced(layer, rows)),
                Err(e) => log::warn!("re-nest: {e}"),
            }
        });
        true
    }

    /// Delete every shape in a layer, in one undoable step.
    fn delete_all(&mut self, index: usize) -> bool {
        let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
            return false;
        };
        // Only what is on screen: with a class filter set, "delete all"
        // means the class being looked at, not the ones hidden from it.
        let (z, t) = (self.z_slice as i32, self.t_index as i32);
        let Some(state) = self.annot_mut(index) else {
            return false;
        };
        let doomed: Vec<Annotation> = state
            .annotations
            .iter()
            .filter(|item| state.shows(item, z, t))
            .cloned()
            .collect();
        if doomed.is_empty() {
            return false;
        }
        let ids: Vec<u64> = doomed.iter().map(|a| a.id).collect();
        state.annotations.retain(|a| !ids.contains(&a.id));
        state.selected = None;
        state.dirty = true;
        self.remember(Undo::RestoreMany {
            layer: layer.clone(),
            annotations: doomed,
        });
        self.rebuild_annotations(index);
        spawn_local(async move {
            for id in ids {
                if let Err(e) = api_client::remove_annotation(&layer, id).await {
                    log::warn!("delete all: {e}");
                }
            }
        });
        true
    }
}
