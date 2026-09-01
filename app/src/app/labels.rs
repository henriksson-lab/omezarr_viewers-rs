//! Label layers, and what a click on the image lands on.

use yew::prelude::*;

use wasm_bindgen_futures::spawn_local;

use crate::api_client;

use super::Picked;
use crate::layers::LayerUi;

use super::{AnnotMsg, App, AppMsg, ObjectMsg};

pub enum LabelMsg {
    Opacity(usize, f32),
    Outline(usize, bool),
    OnlySelected(usize, bool),
    ClearSelection(usize),
    Pick(f32, f32),
    Picked(String, api_client::VoxelValue, (f32, f32)),
    CountRegions,
    RegionsCounted(Vec<api_client::RegionCount>),

    // --- Classing the ids: a class per instance, for training a classifier.
    /// Does a click on the image class the id it lands on?
    Classing(usize, bool),
    /// The class the next click assigns; empty is "nothing in particular".
    SetClass(usize, String),
    /// Put the layer's current class on one id.
    Assign(usize, u64),
    /// Forget an id — not the same as classing it as nothing.
    Unassign(usize, u64),
    /// One edit landed, and how many ids the *server* now holds a class for.
    Assigned(String, usize),
    /// An edit that did not land, in the server's own words.
    ClassFailed(String, String),
    /// Every class a layer holds, as the server has them.
    ClassesLoaded(String, Box<api_client::LabelClasses>),
    /// Paint each classed id by its class.
    ColorByClass(usize, bool),
    SaveTarget(usize, String),
    SaveRegion(usize, String),
    SaveClasses(usize),
    ClassesSaved(String, Result<api_client::SavedClasses, String>),
}

impl From<LabelMsg> for AppMsg {
    fn from(msg: LabelMsg) -> Self {
        AppMsg::Label(msg)
    }
}

impl App {
    pub(super) fn update_labels(&mut self, ctx: &Context<Self>, msg: LabelMsg) -> bool {
        match msg {
            LabelMsg::Opacity(layer, value) => {
                if let Some(state) = self.label_mut(layer) {
                    state.opacity = value;
                }
                true
            }
            LabelMsg::Outline(layer, outline) => {
                if let Some(state) = self.label_mut(layer) {
                    state.outline = outline;
                }
                true
            }
            LabelMsg::OnlySelected(layer, only) => {
                if let Some(state) = self.label_mut(layer) {
                    state.only_selected = only;
                }
                true
            }
            LabelMsg::ClearSelection(layer) => {
                if let Some(state) = self.label_mut(layer) {
                    state.selected = 0;
                    state.only_selected = false;
                }
                self.picked = None;
                true
            }
            LabelMsg::Pick(world_x, world_y) => {
                self.pick_at(ctx, world_x, world_y);
                false
            }
            LabelMsg::Picked(layer_id, voxel, world) => {
                let mut layer_name = layer_id.clone();
                let id = voxel.id.unwrap_or(0);
                let mut classing = None;
                if let Some(index) = self.layers.iter().position(|l| l.id == layer_id) {
                    layer_name = self.layers[index].name.clone();
                    if let Some(state) = self.label_mut(index) {
                        state.selected = id as u32;
                        // Id 0 is the background, not an instance: classing it
                        // would put a row on everything nobody segmented.
                        if state.classing && id != 0 {
                            classing = Some(index);
                        }
                    }
                }
                if let Some(index) = classing {
                    ctx.link().send_message(LabelMsg::Assign(index, id));
                }
                let region = match (&voxel.name, &voxel.acronym) {
                    (Some(name), Some(acronym)) => Some(format!("{name} ({acronym})")),
                    (Some(name), None) => Some(name.clone()),
                    _ => None,
                };
                self.picked = Some(Picked {
                    layer_name,
                    region,
                    id: voxel.id.unwrap_or(0),
                    value: voxel.value,
                    dtype: voxel.dtype,
                    world,
                });
                true
            }
            LabelMsg::CountRegions => {
                let (Some(labels), Some(objects)) = (
                    self.layers
                        .iter()
                        .find(|l| l.is_labels())
                        .map(|l| l.id.clone()),
                    self.layers
                        .iter()
                        .find(|l| l.is_objects())
                        .map(|l| l.id.clone()),
                ) else {
                    return false;
                };
                self.counting_regions = true;
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::fetch_regions(&labels, &objects, 25).await {
                        Ok(regions) => link.send_message(LabelMsg::RegionsCounted(regions)),
                        Err(e) => {
                            log::warn!("regions: {}", e);
                            link.send_message(LabelMsg::RegionsCounted(Vec::new()));
                        }
                    }
                });
                true
            }
            LabelMsg::RegionsCounted(regions) => {
                self.regions = regions;
                self.counting_regions = false;
                true
            }
            LabelMsg::Classing(index, on) => {
                if let Some(state) = self.label_mut(index) {
                    state.classing = on;
                }
                true
            }
            LabelMsg::SetClass(index, class) => {
                if let Some(state) = self.label_mut(index) {
                    state.class = class;
                }
                true
            }
            LabelMsg::Assign(index, id) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.label_mut(index) else {
                    return false;
                };
                let class = state.class.clone();
                // Shown at once and confirmed afterwards: a curator clicking
                // through a field of nuclei cannot wait for a round trip per
                // id, and every answer carries the server's own count so a
                // divergence is caught rather than accumulated.
                state.assigned.insert(id, class.clone());
                state.status = None;
                self.repaint_classes(index);
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::set_label_class(&layer, id, &class).await {
                        Ok(count) => link.send_message(LabelMsg::Assigned(layer, count.assigned)),
                        Err(e) => link.send_message(LabelMsg::ClassFailed(layer, e)),
                    }
                });
                true
            }
            LabelMsg::Unassign(index, id) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.label_mut(index) else {
                    return false;
                };
                state.assigned.remove(&id);
                state.status = None;
                self.repaint_classes(index);
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::clear_label_class(&layer, id).await {
                        Ok(count) => link.send_message(LabelMsg::Assigned(layer, count.assigned)),
                        Err(e) => link.send_message(LabelMsg::ClassFailed(layer, e)),
                    }
                });
                true
            }
            LabelMsg::Assigned(layer, assigned) => {
                // The edit is already on screen; this is only the check that
                // the server agrees about how much there is. When it does not —
                // two edits raced, or one never landed — the server is the
                // truth and the panel is rebuilt from it.
                let ours = self
                    .layers
                    .iter()
                    .position(|l| l.id == layer)
                    .and_then(|index| self.label_state(index))
                    .map(|state| state.assigned.len());
                if ours != Some(assigned) {
                    self.reload_classes(ctx, layer);
                }
                false
            }
            LabelMsg::ClassFailed(layer, message) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                if let Some(state) = self.label_mut(index) {
                    state.status = Some(message);
                }
                // The optimistic edit is now a claim nothing backs, so take the
                // classes back from the server rather than leaving a class on
                // screen that no save would write.
                self.reload_classes(ctx, layer);
                true
            }
            LabelMsg::ClassesLoaded(layer, classes) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                if let Some(state) = self.label_mut(index) {
                    state.assigned = classes
                        .assigned
                        .iter()
                        .map(|entry| (entry.id, entry.class.clone()))
                        .collect();
                }
                self.repaint_classes(index);
                true
            }
            LabelMsg::ColorByClass(index, on) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                if let Some(state) = self.label_mut(index) {
                    state.color_by_class = on;
                }
                // Both ways round: switching it off has to put back whatever
                // the store declared, not leave the class colours frozen there.
                self.reinstall_label_lut(&layer);
                true
            }
            LabelMsg::SaveTarget(index, target) => {
                if let Some(state) = self.label_mut(index) {
                    state.save_target = target;
                }
                true
            }
            LabelMsg::SaveRegion(index, region) => {
                if let Some(state) = self.label_mut(index) {
                    state.region = region;
                }
                true
            }
            LabelMsg::SaveClasses(index) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.label_mut(index) else {
                    return false;
                };
                state.saving = true;
                state.status = None;
                let target = state.save_target.trim().to_string();
                let region = state.region.trim().to_string();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = api_client::save_label_classes(&layer, &target, &region).await;
                    link.send_message(LabelMsg::ClassesSaved(layer, result));
                });
                true
            }
            LabelMsg::ClassesSaved(layer, result) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let Some(state) = self.label_mut(index) else {
                    return false;
                };
                state.saving = false;
                state.status = Some(match result {
                    // What was written *and what it was joined to*: a class
                    // table is a column of numbers until the region says which
                    // label image the ids belong to.
                    Ok(saved) => format!(
                        "wrote {} row(s) to {} as a {}, joined to {}",
                        saved.rows, saved.target, saved.format, saved.region
                    ),
                    Err(e) => e,
                });
                true
            }
        }
    }
}

impl App {
    pub(super) fn label_mut(&mut self, layer: usize) -> Option<&mut crate::layers::LabelUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Labels(state) => Some(state),
            _ => None,
        }
    }

    pub(super) fn label_state(&self, layer: usize) -> Option<&crate::layers::LabelUiState> {
        match &self.layers.get(layer)?.ui {
            LayerUi::Labels(state) => Some(state),
            _ => None,
        }
    }

    /// Take a label layer's classes from the server.
    ///
    /// They live in the session rather than in the layer's metadata, so they do
    /// not arrive with it: without this a reopened tab shows an empty panel
    /// over a set of classes the server still holds, and the first save would
    /// write it back empty.
    pub(super) fn fetch_label_classes(&self, ctx: &Context<Self>) {
        for layer in self.layers.iter().filter(|l| l.is_labels()) {
            self.reload_classes(ctx, layer.id.clone());
        }
    }

    fn reload_classes(&self, ctx: &Context<Self>, layer: String) {
        let link = ctx.link().clone();
        spawn_local(async move {
            match api_client::fetch_label_classes(&layer).await {
                Ok(classes) => link.send_message(LabelMsg::ClassesLoaded(layer, Box::new(classes))),
                Err(e) => log::warn!("label classes: {e}"),
            }
        });
    }

    /// Put the class colours back on the canvas after an assignment moved them.
    ///
    /// Only when the layer is being coloured by class: otherwise the classes
    /// are bookkeeping and repainting would throw away the colour table the
    /// store or a feature column put there.
    fn repaint_classes(&self, index: usize) {
        let by_class = self
            .label_state(index)
            .is_some_and(|state| state.color_by_class);
        if !by_class {
            return;
        }
        if let Some(layer) = self.layers.get(index) {
            self.reinstall_label_lut(&layer.id);
        }
    }

    /// Ask the server what id is under a click, on the topmost visible label
    /// layer. Reading it from the array rather than from the framebuffer keeps
    /// every label tile out of client memory.
    fn pick_at(&mut self, ctx: &Context<Self>, world_x: f32, world_y: f32) {
        let world = self.world_size();
        // A click is also where the orthogonal panes are cut, so the three
        // views always agree about which voxel is being looked at.
        self.crosshair = (world_x, world_y);

        // Annotations sit above everything, and the client holds every row —
        // so this is a hit test, not a request. Selecting is what makes the
        // panel's row and the box on screen the same thing.
        if let Some(index) = self
            .layers
            .iter()
            .rposition(|layer| layer.is_annotations() && layer.visible)
        {
            let LayerUi::Annotations(state) = &self.layers[index].ui else {
                unreachable!("rposition matched an annotation layer")
            };
            // The slack is in world pixels but the mark is drawn in screen
            // pixels, so what looks like a hit depends on the zoom.
            let pad = (state.style.size / self.zoom().max(0.01)).clamp(2.0, 512.0) as f64;
            // Only what is actually drawn right now can be picked, or a click
            // would select a shape on another plane that nothing shows.
            let (z, t) = (self.z_slice as i32, self.t_index as i32);
            let visible: Vec<omezarr_viewer_common::Annotation> = state
                .annotations
                .iter()
                .filter(|item| state.shows(item, z, t))
                .cloned()
                .collect();
            let hit = omezarr_viewer_common::pick_annotation(
                &visible,
                world_x as f64,
                world_y as f64,
                pad,
            )
            .map(|item| item.id);
            if hit.is_some() {
                ctx.link().send_message(AnnotMsg::Select(index, hit));
                return;
            }
        }

        // Objects sit on top of everything else, so a click lands on one first.
        if let Some(layer) = self
            .layers
            .iter()
            .rev()
            .find(|layer| layer.is_objects() && layer.visible)
        {
            let id = layer.id.clone();
            let z = self.z_slice as f32;
            // A generous radius in world pixels: the sprite is drawn in screen
            // pixels, so what looks like a hit depends on the zoom.
            let radius = (14.0 / self.zoom().max(0.01)).clamp(2.0, 512.0);
            let link = ctx.link().clone();
            spawn_local(async move {
                match api_client::fetch_object_at(&id, z, world_y, world_x, radius).await {
                    Ok(row) => link.send_message(ObjectMsg::Inspected(id, row)),
                    Err(e) => log::warn!("pick object: {}", e),
                }
            });
            return;
        }

        let Some(layer) = self
            .layers
            .iter()
            .rev()
            .find(|layer| layer.is_labels() && layer.visible)
        else {
            return;
        };

        let level = self.level_of(layer);
        let (sx, sy) = layer.level_to_world(level, world);
        if sx <= 0.0 || sy <= 0.0 {
            return;
        }
        let x = (world_x / sx).floor();
        let y = (world_y / sy).floor();
        let Some((lw, lh)) = layer.level_size(level) else {
            return;
        };
        if x < 0.0 || y < 0.0 || x >= lw || y >= lh {
            return;
        }

        let id = layer.id.clone();
        let (z, t) = (self.layer_z(layer, level), self.layer_t(layer));
        let link = ctx.link().clone();
        let world_point = (world_x, world_y);
        spawn_local(async move {
            match api_client::fetch_value(&id, level, t, 0, z, y as u64, x as u64).await {
                Ok(voxel) => link.send_message(LabelMsg::Picked(id, voxel, world_point)),
                Err(e) => log::warn!("pick: {}", e),
            }
        });
    }

    /// What the picked layer's `image-label.properties` says about the id.
    pub(super) fn label_properties(&self, picked: &Picked) -> Option<String> {
        self.layers
            .iter()
            .find(|layer| layer.name == picked.layer_name)
            .and_then(|layer| match &layer.ui {
                LayerUi::Labels(state) => state.describe(picked.id),
                _ => None,
            })
    }
}
