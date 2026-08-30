//! Where a layer's annotations come from and where they go back to.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use crate::api_client;

use super::{App, AppMsg, SessionMsg};
pub enum AnnotStoreMsg {
    /// Reopen something the store holds: `true` for a GeoJSON annotation set,
    /// `false` for an ngio ROI table.
    OpenStored(String, bool),
    SetTarget(String),
    SetNewName(String),
    AddLayer,
    SaveTarget(usize, String),
    Save(usize),
    Saved(String, Result<api_client::SavedAnnotations, String>),
}

impl From<AnnotStoreMsg> for AppMsg {
    fn from(msg: AnnotStoreMsg) -> Self {
        AppMsg::AnnotStore(msg)
    }
}

impl App {
    pub(super) fn update_annot_store(&mut self, ctx: &Context<Self>, msg: AnnotStoreMsg) -> bool {
        match msg {
            AnnotStoreMsg::OpenStored(name, set) => {
                let Some(store) = self.tables.store.clone() else {
                    return false;
                };
                let group = if set { "annotations" } else { "tables" };
                let source = format!("{store}/{group}/{name}");
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::add_layer(&source, Some("annotations")).await {
                        Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                false
            }
            AnnotStoreMsg::SetTarget(id) => {
                self.annot_target = Some(id);
                true
            }
            AnnotStoreMsg::SetNewName(name) => {
                self.new_annot_name = name;
                true
            }
            AnnotStoreMsg::AddLayer => {
                let name = match self.new_annot_name.trim() {
                    "" => format!("annotations {}", self.annotation_layers() + 1),
                    given => given.to_string(),
                };
                self.new_annot_name.clear();
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::add_annotation_layer(&name).await {
                        Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                true
            }
            AnnotStoreMsg::SaveTarget(index, target) => {
                if let Some(state) = self.annot_mut(index) {
                    state.save_target = target;
                }
                true
            }
            AnnotStoreMsg::Save(index) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.annot_mut(index) else {
                    return false;
                };
                state.saving = true;
                state.status = None;
                let target = state.save_target.trim().to_string();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let target = (!target.is_empty()).then_some(target);
                    let result = api_client::save_annotations(&layer, target.as_deref()).await;
                    link.send_message(AnnotStoreMsg::Saved(layer, result));
                });
                true
            }
            AnnotStoreMsg::Saved(layer, result) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let mut refresh_tables = false;
                if let Some(state) = self.annot_mut(index) {
                    state.saving = false;
                    match result {
                        Ok(saved) => {
                            let mut text =
                                format!("wrote {} row(s) to {}", saved.rows, saved.target);
                            if let (Some(voxel), Some(seconds)) = (saved.voxel, saved.seconds) {
                                text.push_str(&format!(" at {voxel:?} um/px, {seconds} s/frame"));
                            }
                            if saved.format == "geojson" {
                                text.push_str(" as GeoJSON");
                            }
                            // An ROI table holds boxes and nothing else, so a
                            // save that flattened a shape says so rather than
                            // letting it be found on the round trip.
                            if saved.flattened > 0 {
                                text.push_str(&format!(
                                    " \u{2014} {} shape(s) written as bounding boxes; \
                                     save to <store>/annotations/<name> to keep them",
                                    saved.flattened
                                ));
                            }
                            state.status = Some(text);
                            state.save_target = saved.target.clone();
                            state.target = Some(saved.target);
                            state.dirty = false;
                            // The store now holds something it did not a moment
                            // ago, and the list of what can be reopened is only
                            // fetched with the session — so without this a set
                            // saved just now is unreachable until a page reload.
                            refresh_tables = true;
                        }
                        Err(e) => state.status = Some(e),
                    }
                }
                if refresh_tables {
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        match api_client::fetch_tables(None).await {
                            Ok(tables) => link.send_message(SessionMsg::TablesLoaded(tables)),
                            Err(e) => log::warn!("list stored annotations: {e}"),
                        }
                    });
                }
                true
            }
        }
    }
}
