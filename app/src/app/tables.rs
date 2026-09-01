//! Feature and ROI tables: their rows, and painting labels by a column.

use yew::prelude::*;

use wasm_bindgen_futures::spawn_local;

use crate::api_client;
use crate::layers::{LayerState, LayerUi, TableUiState};

use super::TABLE_PAGE;
use super::{App, AppMsg};

pub enum TableMsg {
    /// Fetch the next page of a table layer's rows.
    LoadMoreRows(usize),
    RowsLoaded(String, Box<api_client::TablePage>),
    /// Paint the label layer a table describes, by one of its columns.
    ColorLabelsBy(usize, Option<String>),
    ColumnLoaded(String, Box<api_client::TableColumnValues>),
}

impl From<TableMsg> for AppMsg {
    fn from(msg: TableMsg) -> Self {
        AppMsg::Table(msg)
    }
}

impl App {
    pub(super) fn update_tables(&mut self, ctx: &Context<Self>, msg: TableMsg) -> bool {
        match msg {
            TableMsg::LoadMoreRows(index) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                let Some(state) = self.table_mut(index) else {
                    return false;
                };
                if state.loading {
                    return false;
                }
                state.loading = true;
                let offset = state.offset;
                let link = ctx.link().clone();
                spawn_local(async move {
                    match api_client::fetch_table_rows(&layer, offset, TABLE_PAGE).await {
                        Ok(page) => link.send_message(TableMsg::RowsLoaded(layer, Box::new(page))),
                        Err(e) => log::warn!("table rows: {e}"),
                    }
                });
                true
            }
            TableMsg::RowsLoaded(layer, page) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                if let Some(state) = self.table_mut(index) {
                    state.loading = false;
                    // Only append what continues where we are: a page that
                    // arrives out of order would otherwise duplicate rows.
                    if page.offset == state.offset {
                        state.offset += page.rows.len();
                        state.rows.extend(page.rows.iter().cloned());
                    }
                }
                true
            }
            TableMsg::ColorLabelsBy(index, column) => {
                let Some(layer) = self.layers.get(index).map(|l| l.id.clone()) else {
                    return false;
                };
                // The label layer this table describes: the one whose name the
                // `region` path ends with. A table says `../labels/nuclei`, and
                // the layer is called `nuclei`.
                let target = self
                    .table_mut(index)
                    .and_then(|state| state.table.region.clone())
                    .and_then(|region| {
                        let wanted = region.rsplit('/').next()?.to_string();
                        self.layers
                            .iter()
                            .find(|l| l.is_labels() && l.name.trim_end_matches(".zarr") == wanted)
                            .map(|l| l.id.clone())
                    });
                if let Some(state) = self.table_mut(index) {
                    state.coloring = column.clone();
                    state.target = target.clone();
                }
                let Some(target) = target else {
                    return true;
                };
                match column {
                    None => {
                        // Back to whatever the label image itself declared.
                        if let Some(label) = self
                            .layers
                            .iter()
                            .position(|l| l.id == target)
                            .and_then(|i| self.label_mut(i))
                        {
                            label.colored_by = None;
                        }
                        self.reinstall_label_lut(&target);
                        true
                    }
                    Some(name) => {
                        let link = ctx.link().clone();
                        let id = layer.clone();
                        spawn_local(async move {
                            match api_client::fetch_table_column(&id, &name).await {
                                Ok(values) => {
                                    link.send_message(TableMsg::ColumnLoaded(id, Box::new(values)))
                                }
                                Err(e) => log::warn!("table column: {e}"),
                            }
                        });
                        true
                    }
                }
            }
            TableMsg::ColumnLoaded(layer, values) => {
                let Some(index) = self.layers.iter().position(|l| l.id == layer) else {
                    return false;
                };
                let (target, column) = match self.table_mut(index) {
                    Some(state) => (state.target.clone(), state.coloring.clone()),
                    None => return false,
                };
                let (Some(target), Some(column)) = (target, column) else {
                    return false;
                };
                let Some(lut) = LayerState::measurement_lut(&values.labels, &values.values) else {
                    log::warn!("a column with ids above 65535 cannot colour a label image");
                    return false;
                };
                if let Some(label) = self
                    .layers
                    .iter()
                    .position(|l| l.id == target)
                    .and_then(|i| self.label_mut(i))
                {
                    label.colored_by = Some((layer, column));
                }
                if let Some(cs) = &self.canvas_state {
                    if let Some(ref mut state) = *cs.borrow_mut() {
                        if let Err(e) = state.renderer.set_label_lut(&target, &lut) {
                            log::warn!("measurement LUT: {e}");
                        }
                    }
                }
                true
            }
        }
    }
}

impl App {
    /// Upload the colour table for every label layer that has one.
    pub(super) fn install_label_luts(&self) {
        let Some(cs) = &self.canvas_state else {
            return;
        };
        let Some(ref mut state) = *cs.borrow_mut() else {
            return;
        };
        for layer in &self.layers {
            if state.renderer.has_label_lut(&layer.id) {
                continue;
            }
            // A layer painted by a feature column keeps that colouring; the
            // store's own table is not the current answer for it.
            if matches!(&layer.ui, LayerUi::Labels(l) if l.colored_by.is_some()) {
                continue;
            }
            if let Some(lut) = layer.label_lut() {
                if let Err(e) = state.renderer.set_label_lut(&layer.id, &lut) {
                    log::warn!("label LUT for {}: {}", layer.id, e);
                }
            }
        }
    }

    fn table_mut(&mut self, layer: usize) -> Option<&mut TableUiState> {
        match &mut self.layers.get_mut(layer)?.ui {
            LayerUi::Table(state) => Some(state),
            _ => None,
        }
    }

    /// The names of every open label layer, so a table can say which one it
    /// could paint when none matches its `region`.
    pub(super) fn label_layer_names(&self) -> Vec<String> {
        self.layers
            .iter()
            .filter(|l| l.is_labels())
            .map(|l| l.name.clone())
            .collect()
    }

    /// Put a label layer's own colour table back, after a measurement colouring
    /// is switched off.
    pub(super) fn reinstall_label_lut(&self, id: &str) {
        let Some(layer) = self.layers.iter().find(|l| l.id == id) else {
            return;
        };
        let Some(cs) = &self.canvas_state else { return };
        let Some(ref mut state) = *cs.borrow_mut() else {
            return;
        };
        match layer.label_lut() {
            Some(lut) => {
                if let Err(e) = state.renderer.set_label_lut(id, &lut) {
                    log::warn!("label LUT: {e}");
                }
            }
            // No declared colours: clearing the table drops the layer back to
            // the id hash, which is what it looked like before.
            None => state.renderer.clear_label_lut(id),
        }
    }
}
