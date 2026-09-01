//! How an annotation layer is drawn. Nothing here reaches the server: these are all local to the viewer.

use yew::prelude::*;

use omezarr_viewer_common::ObjectType;

use super::{App, AppMsg};
pub enum AnnotStyleMsg {
    Color(usize, [f32; 3]),
    Opacity(usize, f32),
    Size(usize, f32),
    Slab(usize, f32),
    ColorByClass(usize, bool),
    /// Size this layer's points by a world radius rather than by screen pixels.
    WorldRadius(usize, bool),
    /// The radius the class new shapes get draws at, in world pixels.
    Radius(usize, f32),
    Filled(usize, bool),
    /// The object type new shapes in this layer get.
    NewObjectType(usize, ObjectType),
    Filter(usize, Option<String>),
    TExtent(usize, f64),
    ZExtent(usize, f64),
}

impl From<AnnotStyleMsg> for AppMsg {
    fn from(msg: AnnotStyleMsg) -> Self {
        AppMsg::AnnotStyle(msg)
    }
}

impl App {
    pub(super) fn update_annot_style(&mut self, ctx: &Context<Self>, msg: AnnotStyleMsg) -> bool {
        match msg {
            AnnotStyleMsg::ColorByClass(index, on) => {
                if let Some(state) = self.annot_mut(index) {
                    state.color_by_class = on;
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotStyleMsg::NewObjectType(index, kind) => {
                if let Some(state) = self.annot_mut(index) {
                    state.object_type = kind;
                }
                true
            }
            AnnotStyleMsg::WorldRadius(index, on) => {
                if let Some(state) = self.annot_mut(index) {
                    state.world_radius = on;
                }
                // The radius is part of the batch key, so the buffers say which
                // mode they were built in and have to be rebuilt when it moves.
                self.rebuild_annotations(index);
                true
            }
            AnnotStyleMsg::Radius(index, value) => {
                if let Some(state) = self.annot_mut(index) {
                    state.set_radius(value);
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotStyleMsg::Filled(index, on) => {
                if let Some(state) = self.annot_mut(index) {
                    state.filled = on;
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotStyleMsg::Filter(index, class) => {
                if let Some(state) = self.annot_mut(index) {
                    state.filter = class;
                }
                self.rebuild_annotations(index);
                true
            }
            AnnotStyleMsg::ZExtent(index, depth) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let selected = self.annot_mut(index).and_then(|state| state.selected);
                let Some(id) = selected else { return false };
                self.edit_selected(ctx, index, layer, id, |item| item.z_extent = depth as u32);
                true
            }
            AnnotStyleMsg::TExtent(index, frames) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let selected = self.annot_mut(index).and_then(|state| state.selected);
                let Some(id) = selected else { return false };
                self.edit_selected(ctx, index, layer, id, |item| item.t_extent = frames as u32);
                true
            }
            AnnotStyleMsg::Color(index, color) => {
                if let Some(state) = self.annot_mut(index) {
                    state.style.color = color;
                }
                true
            }
            AnnotStyleMsg::Opacity(index, value) => {
                if let Some(state) = self.annot_mut(index) {
                    state.style.opacity = value;
                }
                true
            }
            AnnotStyleMsg::Size(index, value) => {
                if let Some(state) = self.annot_mut(index) {
                    state.style.size = value;
                }
                true
            }
            AnnotStyleMsg::Slab(index, value) => {
                if let Some(state) = self.annot_mut(index) {
                    state.style.slab = value;
                }
                true
            }
        }
    }
}
