//! One step backwards.
//!
//! Every reversible message pushes an `Undo` before it acts, and `undo` replays
//! it against the server rather than against local state: the server is where
//! annotations live, so an undo the server did not see is one a reload undoes.

use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use omezarr_viewer_common::Annotation;

use crate::api_client;

use super::session::SessionMsg;
use super::{App, UNDO_DEPTH};

pub enum Undo {
    /// It was added; remove it.
    Added { layer: String, id: u64 },
    /// It was removed or changed; put this version back.
    Restore {
        layer: String,
        annotation: Box<Annotation>,
        /// True when the row is gone from the layer and has to be re-added
        /// rather than updated in place.
        deleted: bool,
    },
    /// Several rows went at once, as a "delete all" does.
    RestoreMany {
        layer: String,
        annotations: Vec<Annotation>,
    },
    /// The whole layer changed shape, as a re-nest does.
    RestoreAll {
        layer: String,
        annotations: Vec<Annotation>,
    },
}

impl App {
    /// Replay the most recent reversible step, if there is one.
    pub(super) fn undo(&mut self, ctx: &Context<Self>) -> bool {
        let Some(step) = self.undo.pop() else {
            return false;
        };
        let link = ctx.link().clone();
        match step {
            Undo::Added { layer, id } => {
                // Undoing an add is a delete, and must not itself become
                // an undo step — `AnnotationRemoved` would push one — so
                // the local removal happens here and the server is told
                // without a round trip back through the message.
                if let Some(index) = self.layers.iter().position(|l| l.id == layer) {
                    if let Some(state) = self.annot_mut(index) {
                        state.annotations.retain(|a| a.id != id);
                        if state.selected == Some(id) {
                            state.selected = None;
                        }
                    }
                    self.rebuild_annotations(index);
                }
                spawn_local(async move {
                    if let Err(e) = api_client::remove_annotation(&layer, id).await {
                        log::warn!("undo add: {e}");
                    }
                });
            }
            Undo::Restore {
                layer,
                annotation,
                deleted,
            } => {
                if let Some(index) = self.layers.iter().position(|l| l.id == layer) {
                    if let Some(state) = self.annot_mut(index) {
                        match state.annotations.iter_mut().find(|a| a.id == annotation.id) {
                            Some(slot) => *slot = (*annotation).clone(),
                            None => state.annotations.push((*annotation).clone()),
                        }
                    }
                    self.rebuild_annotations(index);
                }
                let restored = *annotation;
                spawn_local(async move {
                    // A row the server still has is updated; one it
                    // deleted has to be added back, and comes back with
                    // a *new* id, which is why the reply reloads the
                    // layer rather than being merged locally.
                    let result = if deleted {
                        api_client::add_annotation(&layer, &restored)
                            .await
                            .map(|_| ())
                    } else {
                        api_client::update_annotation(&layer, &restored)
                            .await
                            .map(|_| ())
                    };
                    match result {
                        Ok(()) => match api_client::fetch_session().await {
                            Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                            Err(e) => log::warn!("undo reload: {e}"),
                        },
                        Err(e) => log::warn!("undo: {e}"),
                    }
                });
            }
            Undo::RestoreAll { layer, annotations } => {
                // The parents moved, so the fix is to put every row back
                // as it was rather than to invert one change.
                if let Some(index) = self.layers.iter().position(|l| l.id == layer) {
                    if let Some(state) = self.annot_mut(index) {
                        state.annotations = annotations.clone();
                    }
                    self.rebuild_annotations(index);
                }
                spawn_local(async move {
                    for annotation in &annotations {
                        if let Err(e) = api_client::update_annotation(&layer, annotation).await {
                            log::warn!("undo re-nest: {e}");
                        }
                    }
                });
            }
            Undo::RestoreMany { layer, annotations } => {
                spawn_local(async move {
                    for annotation in &annotations {
                        if let Err(e) = api_client::add_annotation(&layer, annotation).await {
                            log::warn!("undo delete-all: {e}");
                        }
                    }
                    match api_client::fetch_session().await {
                        Ok(session) => link.send_message(SessionMsg::SessionLoaded(session)),
                        Err(e) => log::warn!("undo reload: {e}"),
                    }
                });
            }
        }
        true
    }

    /// Push an undo step, dropping the oldest once the stack is full.
    pub(super) fn remember(&mut self, step: Undo) {
        self.undo.push(step);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }
}
