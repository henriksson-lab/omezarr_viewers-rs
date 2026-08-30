//! The session: what is open, what to open next, and where it is saved.

use yew::prelude::*;

use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen_futures::spawn_local;

use omezarr_viewer_common::SessionInfo;

use crate::api_client;
use crate::viewer_canvas::ViewerCanvasState;

use super::{App, AppMsg};

pub enum SessionMsg {
    TogglePanel,
    DatasetsLoaded(Vec<String>),
    DatasetSelected(String),
    SessionLoaded(SessionInfo),
    LoadError(String),
    CanvasReady(Rc<RefCell<Option<ViewerCanvasState>>>),
    SetLayerVisible(usize, bool),
    RemoveLayer(String),
    SetAddSource(String),
    SetAddRole(String),
    SubmitAddLayer,
    SaveProject,
    /// Ask the desktop shell for a path: `pick_folder` or `pick_file`.
    Browse(&'static str),
    Browsed(String, bool),
    TablesLoaded(api_client::StoreTables),
}

impl From<SessionMsg> for AppMsg {
    fn from(msg: SessionMsg) -> Self {
        AppMsg::Session(msg)
    }
}

impl App {
    pub(super) fn update_session(&mut self, ctx: &Context<Self>, msg: SessionMsg) -> bool {
        match msg {
            SessionMsg::TogglePanel => {
                self.panel_visible = !self.panel_visible;
                true
            }
            SessionMsg::DatasetsLoaded(list) => {
                self.datasets = list;
                true
            }
            SessionMsg::DatasetSelected(name) => {
                let link = ctx.link().clone();
                let dataset_name = name.clone();
                self.current_dataset = Some(name);
                self.error = None;
                spawn_local(async move {
                    match api_client::open_dataset(&dataset_name).await {
                        Ok(_) => match api_client::fetch_session().await {
                            Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                            Err(e) => link.send_message(SessionMsg::LoadError(e)),
                        },
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                true
            }
            SessionMsg::SessionLoaded(session) => {
                self.adopt_session(session);
                if self.canvas_state.is_some() {
                    self.install_label_luts();
                    self.load_tiles(ctx);
                }
                // Annotation rows arrive with the session, not as tiles, so
                // this is where a reopened ROI table becomes visible — when the
                // canvas already exists. On a first page load the session
                // answers before the canvas is ready, and `CanvasReady` does it.
                self.upload_annotations();
                // A new mark goes into the last annotation layer opened, which
                // is the one the user was just looking at.
                let last = self
                    .layers
                    .iter()
                    .rev()
                    .find(|l| l.is_annotations())
                    .map(|l| l.id.clone());
                if !self
                    .annot_target
                    .as_ref()
                    .is_some_and(|id| self.layers.iter().any(|l| &l.id == id))
                {
                    self.annot_target = last;
                }
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::fetch_tables(None).await {
                        Ok(tables) => link.send_message(SessionMsg::TablesLoaded(tables)),
                        Err(e) => log::warn!("list ROI tables: {e}"),
                    }
                });
                true
            }
            SessionMsg::LoadError(e) => {
                self.error = Some(e);
                true
            }
            SessionMsg::CanvasReady(state) => {
                self.canvas_state = Some(state);
                if !self.layers.is_empty() {
                    self.install_label_luts();
                    self.upload_annotations();
                    self.load_tiles(ctx);
                }
                false
            }
            SessionMsg::SetLayerVisible(index, visible) => {
                if let Some(layer) = self.layers.get_mut(index) {
                    layer.visible = visible;
                }
                self.load_tiles(ctx);
                true
            }
            SessionMsg::RemoveLayer(id) => {
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::remove_layer(&id).await {
                        Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                false
            }
            SessionMsg::SetAddSource(source) => {
                self.add_source = source;
                false
            }
            SessionMsg::SetAddRole(role) => {
                self.add_role = role;
                false
            }
            SessionMsg::SubmitAddLayer => {
                let source = self.add_source.trim().to_string();
                if source.is_empty() {
                    return false;
                }
                let role = self.add_role.clone();
                let link = ctx.link().clone();
                self.error = None;
                spawn_local(async move {
                    let role = (role != "auto").then_some(role);
                    match api_client::add_layer(&source, role.as_deref()).await {
                        Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                        Err(e) => link.send_message(SessionMsg::LoadError(e)),
                    }
                });
                true
            }
            SessionMsg::Browse(command) => {
                let link = ctx.link().clone();
                let folder = command == "pick_folder";
                spawn_local(async move {
                    if let Some(path) = api_client::pick_path(command).await {
                        link.send_message(SessionMsg::Browsed(path, folder));
                    }
                });
                false
            }
            SessionMsg::Browsed(path, folder) => {
                self.add_source = path;
                // A folder is a run; a file is whatever its header says it is.
                self.add_role = if folder { "project" } else { "auto" }.to_string();
                true
            }
            SessionMsg::SaveProject => {
                // The session as a project file, handed to the browser as a
                // download: what to keep is the user's decision, and the server
                // has no business writing files for a browser.
                spawn_local(async move {
                    if let Err(e) = crate::api_client::download_project().await {
                        log::warn!("save view: {}", e);
                    }
                });
                false
            }
            SessionMsg::TablesLoaded(tables) => {
                self.tables = tables;
                true
            }
        }
    }
}
