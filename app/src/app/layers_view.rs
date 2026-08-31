//! Everything the window is made of.
//!
//! Markup only: each of these reads the state the handlers in the sibling
//! modules maintain and emits a message when something is clicked. Kept apart
//! from the handlers because 400 lines of `html!` between two match arms is how
//! an arm ends up pasted into the wrong one.

use yew::prelude::*;

use crate::api_client;
use crate::controls::annot_panel::AnnotPanel;
use crate::controls::axis_sliders::AxisSliders;
use crate::controls::channel_panel::ChannelPanel;
use crate::controls::label_panel::LabelPanel;
use crate::controls::object_panel::ObjectPanel;
use crate::controls::table_panel::TablePanel;
use crate::layers::{LayerState, LayerUi};
use crate::ortho_pane::OrthoPane;
use crate::viewer_canvas::{Tool, ViewerCanvas};

use super::{
    AnnotEditMsg, AnnotMsg, AnnotStoreMsg, AnnotStyleMsg, App, ChannelMsg, LabelMsg, ObjectMsg,
    SessionMsg, TableMsg, ViewMsg,
};

impl App {
    pub(super) fn view_body(&self, ctx: &Context<Self>) -> Html {
        if self.layers.is_empty() && self.datasets.is_empty() {
            if let Some(ref error) = self.error {
                return html! {
                    <div class="loading">{format!("Error: {}", error)}</div>
                };
            }
            return html! {
                <div class="loading">{"Loading dataset..."}</div>
            };
        }

        let panel_class = if self.panel_visible {
            "control-panel"
        } else {
            "control-panel hidden"
        };
        let toggle_label = if self.panel_visible {
            "\u{2715}"
        } else {
            "\u{2630}"
        };

        html! {
            <div class="app-container">
                { self.view_viewer(ctx) }
                <button class="panel-toggle" onclick={ctx.link().callback(|_| SessionMsg::TogglePanel)}>
                    {toggle_label}
                </button>
                <div class={panel_class}>
                    { self.view_panel(ctx) }
                </div>
            </div>
        }
    }

    /// The image itself: the canvas, its tool bar, and the orthogonal panes.
    fn view_viewer(&self, ctx: &Context<Self>) -> Html {
        let on_canvas_ready = ctx.link().callback(SessionMsg::CanvasReady);
        let on_camera_changed =
            ctx.link()
                .callback(|(px, py, z, w, h): (f32, f32, f32, f32, f32)| {
                    ViewMsg::Camera(px, py, z, w, h)
                });
        let on_pick = ctx
            .link()
            .callback(|(x, y): (f32, f32)| LabelMsg::Pick(x, y));
        let on_draw = ctx.link().callback(AnnotMsg::Drew);
        let on_edit = ctx.link().callback(AnnotMsg::Edit);

        let world = self.world_size();
        let crosshair = (
            (self.crosshair.0 / world.0.max(1.0)).clamp(0.0, 1.0),
            (self.crosshair.1 / world.1.max(1.0)).clamp(0.0, 1.0),
        );
        let z_fraction = if self.z_max > 1 {
            self.z_slice as f32 / (self.z_max - 1) as f32
        } else {
            0.0
        };

        html! {
                <div class="viewer-area">
                    <div class="viewer-row">
                        <div class="viewer-main">
                            <ViewerCanvas
                                layers={self.render_infos()}
                                world_size={world}
                                on_canvas_ready={on_canvas_ready}
                                on_camera_changed={on_camera_changed}
                                on_pick={on_pick}
                                tool={self.tool}
                                on_draw={on_draw}
                                editable={self.editable()}
                                on_edit={on_edit}
                            />
                            { self.view_toolbar(ctx) }
                            if self.ortho {
                                <div class="crosshair-v" style={format!("left: {}%", crosshair.0 * 100.0)} />
                                <div class="crosshair-h" style={format!("top: {}%", crosshair.1 * 100.0)} />
                            }
                        </div>
                        if self.ortho {
                            <OrthoPane
                                axis="x"
                                transpose={true}
                                t={self.t_index as u64}
                                layers={self.ortho_layers("x")}
                                crosshair={(z_fraction, crosshair.1)}
                                on_pick={ctx.link().callback(|(u, v): (f32, f32)| ViewMsg::OrthoPicked("x", u, v))}
                            />
                        }
                    </div>
                    if self.ortho {
                        <OrthoPane
                            axis="y"
                            transpose={false}
                            t={self.t_index as u64}
                            layers={self.ortho_layers("y")}
                            crosshair={(crosshair.0, z_fraction)}
                            on_pick={ctx.link().callback(|(u, v): (f32, f32)| ViewMsg::OrthoPicked("y", u, v))}
                        />
                    }
                </div>
        }
    }

    /// Everything in the control panel down the right-hand side.
    fn view_panel(&self, ctx: &Context<Self>) -> Html {
        html! {
            <>
                    if !self.datasets.is_empty() {
                        <div class="dataset-selector">
                            <h2>{"Dataset"}</h2>
                            <select onchange={ctx.link().callback(|e: Event| {
                                let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                                SessionMsg::DatasetSelected(input.value())
                            })}>
                                <option value="" disabled=true selected={self.current_dataset.is_none()}>
                                    {"Select dataset..."}
                                </option>
                                { for self.datasets.iter().map(|name| {
                                    let selected = self.current_dataset.as_deref() == Some(name.as_str());
                                    html! {
                                        <option value={name.clone()} selected={selected}>{name}</option>
                                    }
                                })}
                            </select>
                        </div>
                    }
                    { self.view_layers(ctx) }
                    { self.view_annotations(ctx) }
                    { self.view_add_layer(ctx) }
                    { self.view_regions(ctx) }
                    <h3>{"View"}</h3>
                    <div class="slider-row">
                        <label>
                            <input type="checkbox" checked={self.ortho}
                                onchange={ctx.link().callback(|_| ViewMsg::ToggleOrtho)} />
                            {" Orthogonal panes"}
                        </label>
                    </div>
                    <div class="slider-row">
                        <span>{"Z project"}</span>
                        <select onchange={ctx.link().callback(|e: Event| {
                            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                            ViewMsg::Projection(match input.value().as_str() {
                                "max" => Some("max"),
                                "mean" => Some("mean"),
                                _ => None,
                            })
                        })}>
                            <option value="" selected={self.projection.is_none()}>{"(slice)"}</option>
                            <option value="max" selected={matches!(self.projection, Some(("max", _)))}>{"max"}</option>
                            <option value="mean" selected={matches!(self.projection, Some(("mean", _)))}>{"mean"}</option>
                        </select>
                    </div>
                    if let Some((_, depth)) = self.projection {
                        <div class="slider-row">
                            <span>{"Depth"}</span>
                            <input type="range" min="1" max="64" step="1" value={depth.to_string()}
                                oninput={ctx.link().callback(|e: InputEvent| {
                                    let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                                    ViewMsg::ProjectionDepth(input.value().parse().unwrap_or(1))
                                })} />
                            <span class="slider-value">{format!("{depth} planes")}</span>
                        </div>
                    }
                    <h3>{"Axes"}</h3>
                    <AxisSliders
                        z_max={self.z_max}
                        t_max={self.t_max}
                        z_current={self.z_slice}
                        t_current={self.t_index}
                        on_z_change={ctx.link().callback(ViewMsg::ZSlice)}
                        on_t_change={ctx.link().callback(ViewMsg::TIndex)}
                    />
                    <div class="info-text">
                        { self.view_status() }
                    </div>
            </>
        }
    }

    fn view_layers(&self, ctx: &Context<Self>) -> Html {
        html! {
            <>
            { for self.layers.iter().enumerate().map(|(index, layer)| {
                match &layer.ui {
                    LayerUi::Image { .. } => self.view_image_layer(ctx, index, layer),
                    LayerUi::Objects(_) => self.view_objects_layer(ctx, index, layer),
                    LayerUi::Table(_) => self.view_table_layer(ctx, index, layer),
                    LayerUi::Annotations(_) => self.view_annot_layer(ctx, index, layer),
                    LayerUi::Labels(_) => self.view_labels_layer(ctx, index, layer),
                }
            })}
            </>
        }
    }

    /// One image layer: its channels, and what to do with the layer itself.
    fn view_image_layer(&self, ctx: &Context<Self>, index: usize, layer: &LayerState) -> Html {
        let link = ctx.link();
        let id = layer.id.clone();
        let LayerUi::Image {
            channels,
            dtype_max,
        } = &layer.ui
        else {
            return html! {};
        };
        html! {
            <div class="layer-block">
                <h2>
                    <label>
                        <input type="checkbox" checked={layer.visible}
                            onchange={link.callback(move |e: Event| {
                                let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                                SessionMsg::SetLayerVisible(index, input.checked())
                            })} />
                        { format!(" {}", layer.name) }
                    </label>
                    if self.layers.len() > 1 {
                        <button class="layer-remove"
                            onclick={link.callback(move |_| SessionMsg::RemoveLayer(id.clone()))}
                            title="Close layer">{"\u{2715}"}</button>
                    }
                </h2>
                { for channels.iter().enumerate().map(|(c, ch)| html! {
                    <ChannelPanel
                        index={c}
                        label={ch.label.clone()}
                        visible={ch.visible}
                        color={ch.color}
                        contrast_min={ch.contrast_min}
                        contrast_max={ch.contrast_max}
                        contrast_limit={*dtype_max}
                        opacity={ch.opacity}
                        on_visibility={link.callback(move |v| ChannelMsg::Visibility(index, c, v))}
                        on_color={link.callback(move |v| ChannelMsg::Color(index, c, v))}
                        on_contrast_min={link.callback(move |v| ChannelMsg::ContrastMin(index, c, v))}
                        on_contrast_max={link.callback(move |v| ChannelMsg::ContrastMax(index, c, v))}
                        on_opacity={link.callback(move |v| ChannelMsg::Opacity(index, c, v))}
                    />
                })}
            </div>
        }
    }

    /// One object layer: how its detections are drawn and filtered.
    fn view_objects_layer(&self, ctx: &Context<Self>, index: usize, layer: &LayerState) -> Html {
        let link = ctx.link();
        let id = layer.id.clone();
        let LayerUi::Objects(state) = &layer.ui else {
            return html! {};
        };
        html! {
            <div class="layer-block">
                <ObjectPanel
                    name={layer.name.clone()}
                    visible={layer.visible}
                    schema={state.schema.clone()}
                    count={state.count}
                    style={state.style}
                    hollow={state.hollow}
                    color_by={state.color_by}
                    filters={state.filters.clone()}
                    loaded={state.loaded}
                    shown={state.shown}
                    total={state.total}
                    selected={self.inspected.get(&layer.id).cloned()}
                    on_visibility={link.callback(move |v| SessionMsg::SetLayerVisible(index, v))}
                    on_color={link.callback(move |v| ObjectMsg::Color(index, v))}
                    on_opacity={link.callback(move |v| ObjectMsg::Opacity(index, v))}
                    on_size={link.callback(move |v| ObjectMsg::Size(index, v))}
                    on_hollow={link.callback(move |v| ObjectMsg::Hollow(index, v))}
                    on_color_by={link.callback(move |v| ObjectMsg::ColorBy(index, v))}
                    on_filter={link.callback(move |(column, filter)| ObjectMsg::Filter(index, column, filter))}
                    on_slab={link.callback(move |v| ObjectMsg::Slab(index, v))}
                    on_remove={link.callback(move |_| SessionMsg::RemoveLayer(id.clone()))}
                />
            </div>
        }
    }

    /// One table layer: its rows, and which labels it can paint.
    fn view_table_layer(&self, ctx: &Context<Self>, index: usize, layer: &LayerState) -> Html {
        let link = ctx.link();
        let id = layer.id.clone();
        let LayerUi::Table(state) = &layer.ui else {
            return html! {};
        };
        html! {
            <div class="layer-block">
                <TablePanel
                    name={layer.name.clone()}
                    table={state.table.clone()}
                    rows={state.rows.clone()}
                    loading={state.loading}
                    coloring={state.coloring.clone()}
                    target={state.target.clone()}
                    label_layers={self.label_layer_names()}
                    on_more={link.callback(move |_| TableMsg::LoadMoreRows(index))}
                    on_color_by={link.callback(move |v| TableMsg::ColorLabelsBy(index, v))}
                    on_remove={link.callback(move |_| SessionMsg::RemoveLayer(id.clone()))}
                />
            </div>
        }
    }

    /// One annotation layer: its shapes, its classes, and where it saves.
    fn view_annot_layer(&self, ctx: &Context<Self>, index: usize, layer: &LayerState) -> Html {
        let link = ctx.link();
        let id = layer.id.clone();
        let LayerUi::Annotations(state) = &layer.ui else {
            return html! {};
        };
        {
            let classes = state.classes();
            let class_colors = classes
                .iter()
                .map(|class| state.class_color(class))
                .collect::<Vec<_>>();
            let (shown, _) = state.visible_count(self.z_slice as i32, self.t_index as i32);
            html! {
            <div class="layer-block">
                <AnnotPanel
                    name={layer.name.clone()}
                    visible={layer.visible}
                    annotations={state.annotations.clone()}
                    style={state.style}
                    color_by_class={state.color_by_class}
                    filled={state.filled}
                    selected={state.selected}
                    class={state.class.clone()}
                    object_type={state.object_type}
                    filter={state.filter.clone()}
                    classes={classes}
                    class_colors={class_colors}
                    shown={shown}
                    save_target={state.save_target.clone()}
                    saving={state.saving}
                    dirty={state.dirty}
                    status={state.status.clone()}
                    has_t={self.t_max > 1}
                    on_visibility={link.callback(move |v| SessionMsg::SetLayerVisible(index, v))}
                    on_color={link.callback(move |v| AnnotStyleMsg::Color(index, v))}
                    on_color_by_class={link.callback(move |v| AnnotStyleMsg::ColorByClass(index, v))}
                    on_filled={link.callback(move |v| AnnotStyleMsg::Filled(index, v))}
                    on_opacity={link.callback(move |v| AnnotStyleMsg::Opacity(index, v))}
                    on_size={link.callback(move |v| AnnotStyleMsg::Size(index, v))}
                    on_slab={link.callback(move |v| AnnotStyleMsg::Slab(index, v))}
                    on_class={link.callback(move |v| AnnotEditMsg::SetClass(index, v))}
                    on_object_type={link.callback(move |v| AnnotStyleMsg::NewObjectType(index, v))}
                    on_name={link.callback(move |v| AnnotEditMsg::SetName(index, v))}
                    on_selected_type={link.callback(move |v| AnnotEditMsg::SetObjectType(index, v))}
                    on_locked={link.callback(move |v| AnnotEditMsg::SetLocked(index, v))}
                    on_filter={link.callback(move |v| AnnotStyleMsg::Filter(index, v))}
                    on_select={link.callback(move |v| AnnotMsg::Select(index, v))}
                    on_rename={link.callback(move |(id, class)| AnnotEditMsg::Rename(index, id, class))}
                    on_delete={link.callback(move |id| AnnotEditMsg::Delete(index, id))}
                    on_delete_all={link.callback(move |_| AnnotEditMsg::DeleteAll(index))}
                    on_renest={link.callback(move |_| AnnotEditMsg::Renest(index))}
                    on_detach={link.callback(move |_| AnnotEditMsg::Detach(index))}
                    on_z_extent={link.callback(move |v| AnnotStyleMsg::ZExtent(index, v))}
                    on_t_extent={link.callback(move |v| AnnotStyleMsg::TExtent(index, v))}
                    on_save_target={link.callback(move |v| AnnotStoreMsg::SaveTarget(index, v))}
                    on_save={link.callback(move |_| AnnotStoreMsg::Save(index))}
                    on_remove={link.callback(move |_| SessionMsg::RemoveLayer(id.clone()))}
                />
            </div>
            }
        }
    }

    /// One label layer: its opacity, outlines and selection.
    fn view_labels_layer(&self, ctx: &Context<Self>, index: usize, layer: &LayerState) -> Html {
        let link = ctx.link();
        let id = layer.id.clone();
        let LayerUi::Labels(state) = &layer.ui else {
            return html! {};
        };
        html! {
            <div class="layer-block">
                <LabelPanel
                    name={layer.name.clone()}
                    visible={layer.visible}
                    opacity={state.opacity}
                    outline={state.outline}
                    selected={state.selected}
                    only_selected={state.only_selected}
                    has_lut={layer.label_lut().is_some()}
                    on_visibility={link.callback(move |v| SessionMsg::SetLayerVisible(index, v))}
                    on_opacity={link.callback(move |v| LabelMsg::Opacity(index, v))}
                    on_outline={link.callback(move |v| LabelMsg::Outline(index, v))}
                    on_only_selected={link.callback(move |v| LabelMsg::OnlySelected(index, v))}
                    on_clear_selection={link.callback(move |_| LabelMsg::ClearSelection(index))}
                    on_remove={link.callback(move |_| SessionMsg::RemoveLayer(id.clone()))}
                />
            </div>
        }
    }

    /// The per-region tally, when there is a label layer and an object layer
    /// to join.
    fn view_regions(&self, ctx: &Context<Self>) -> Html {
        if !(self.layers.iter().any(|l| l.is_labels())
            && self.layers.iter().any(|l| l.is_objects()))
        {
            return html! {};
        }
        html! {
            <div class="regions">
                <h3>{"Regions"}</h3>
                <div class="slider-row">
                    <button onclick={ctx.link().callback(|_| LabelMsg::CountRegions)}>
                        { if self.counting_regions { "Counting\u{2026}" } else { "Count objects per region" } }
                    </button>
                </div>
                if !self.regions.is_empty() {
                    <table class="region-table">
                        { for self.regions.iter().map(|region| html! {
                            <tr>
                                <td>{ region.acronym.clone()
                                        .or_else(|| region.name.clone())
                                        .unwrap_or_else(|| format!("id {}", region.id)) }</td>
                                <td class="region-count">{ region.count }</td>
                            </tr>
                        })}
                    </table>
                }
            </div>
        }
    }

    /// The drawing tools, floating over the canvas.
    ///
    /// Over the canvas rather than in the side panel because the panel hides,
    /// and a tool you cannot see is a tool that silently eats clicks: the mode
    /// the mouse is in has to be visible wherever the mouse is.
    fn view_toolbar(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let button = |tool: Tool, glyph: &str, title: &str| {
            let class = if self.tool == tool {
                "tool-button active"
            } else {
                "tool-button"
            };
            html! {
                <button class={class} title={title.to_string()}
                    onclick={link.callback(move |_| AnnotMsg::SetTool(tool))}>
                    { glyph.to_string() }
                </button>
            }
        };
        let targets: Vec<&LayerState> = self.layers.iter().filter(|l| l.is_annotations()).collect();
        html! {
            <div class="toolbar">
                { button(Tool::Pan, "\u{270b}", "Pan, select and edit") }
                { button(Tool::Point, "\u{25cf}", "Click to place a point") }
                { button(Tool::Box, "\u{25a1}", "Drag to draw a rectangle") }
                { button(Tool::Ellipse, "\u{25ef}", "Drag to draw an ellipse") }
                { button(Tool::Polygon, "\u{2b21}", "Click each vertex; click the first, or double-click, to close") }
                { button(Tool::Freehand, "\u{270e}", "Drag to trace a region freehand") }
                { button(Tool::Polyline, "\u{2571}", "Click each vertex; double-click to finish an open path") }
                { button(Tool::Line, "\u{223f}", "Drag to trace an open path freehand") }
                <button class="tool-button" title="Undo the last annotation edit"
                    disabled={self.undo.is_empty()}
                    onclick={link.callback(|_| AnnotMsg::Undo)}>
                    { "\u{21b6}" }
                </button>
                if self.unsaved_annotations() {
                    <span class="unsaved" title="unsaved annotations">{"unsaved"}</span>
                }
                if targets.len() > 1 {
                    <select onchange={link.callback(|e: Event| {
                        let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                        AnnotStoreMsg::SetTarget(input.value())
                    })}>
                        { for targets.iter().map(|layer| {
                            let selected = self.annot_target.as_deref() == Some(layer.id.as_str());
                            html! {
                                <option value={layer.id.clone()} selected={selected}>
                                    { layer.name.clone() }
                                </option>
                            }
                        })}
                    </select>
                }
            </div>
        }
    }

    /// One list of things in the store that can be reopened.
    ///
    /// Both kinds are offered: a GeoJSON set is what this viewer writes, and an
    /// ROI table is what the ngio world writes. Listing only one of them is how
    /// a set saved a moment ago became unreachable without retyping its path.
    fn view_openable(
        &self,
        ctx: &Context<Self>,
        heading: &str,
        names: &[String],
        set: bool,
    ) -> Html {
        if names.is_empty() {
            return html! {};
        }
        let link = ctx.link();
        html! {
            <>
                <div class="info-text"><p>{ heading.to_string() }</p></div>
                { for names.iter().map(|name| {
                    let wanted = name.clone();
                    let open = self
                        .layers
                        .iter()
                        .any(|l| l.name == *name && (l.is_annotations() || l.is_table()));
                    html! {
                        <div class="slider-row">
                            <span>{ name.clone() }</span>
                            <button disabled={open}
                                onclick={link.callback(move |_| {
                                    AnnotStoreMsg::OpenStored(wanted.clone(), set)
                                })}>
                                { if open { "Open" } else { "Load" } }
                            </button>
                        </div>
                    }
                })}
            </>
        }
    }

    /// Making an annotation layer, and reopening one already on disk.
    fn view_annotations(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        html! {
            <div class="add-layer">
                <h3>{"Annotate"}</h3>
                <div class="slider-row">
                    <input type="text" placeholder="new layer name"
                        value={self.new_annot_name.clone()}
                        oninput={link.callback(|e: InputEvent| {
                            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                            AnnotStoreMsg::SetNewName(input.value())
                        })} />
                    <button onclick={link.callback(|_| AnnotStoreMsg::AddLayer)}>
                        {"New layer"}
                    </button>
                </div>
                { self.view_openable(ctx, "Annotation sets in this store",
                                     &self.tables.annotations, true) }
                { self.view_openable(ctx, "ROI tables in this store",
                                     &self.tables.tables, false) }
            </div>
        }
    }

    fn view_add_layer(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        html! {
            <div class="add-layer">
                <h3>{"Add layer"}</h3>
                <input type="text" placeholder="/path/to/labels.zarr or s3://bucket/key"
                    value={self.add_source.clone()}
                    oninput={link.callback(|e: InputEvent| {
                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                        SessionMsg::SetAddSource(input.value())
                    })} />
                <div class="slider-row">
                    <select onchange={link.callback(|e: Event| {
                        let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
                        SessionMsg::SetAddRole(input.value())
                    })}>
                        <option value="auto" selected={self.add_role == "auto"}>{"auto"}</option>
                        <option value="image" selected={self.add_role == "image"}>{"image"}</option>
                        <option value="labels" selected={self.add_role == "labels"}>{"labels"}</option>
                        <option value="objects" selected={self.add_role == "objects"}>{"objects"}</option>
                        <option value="project" selected={self.add_role == "project"}>{"run folder"}</option>
                    </select>
                    <button onclick={link.callback(|_| SessionMsg::SubmitAddLayer)}>{"Open"}</button>
                    <button onclick={link.callback(|_| SessionMsg::SaveProject)}>{"Save view"}</button>
                </div>
                if api_client::is_desktop() {
                    <div class="slider-row">
                        <button onclick={link.callback(|_| SessionMsg::Browse("pick_folder"))}>{"Browse run\u{2026}"}</button>
                        <button onclick={link.callback(|_| SessionMsg::Browse("pick_file"))}>{"Browse file\u{2026}"}</button>
                    </div>
                }
                if let Some(ref error) = self.error {
                    <p class="error-text">{error}</p>
                }
            </div>
        }
    }

    fn view_status(&self) -> Html {
        let cached = self
            .canvas_state
            .as_ref()
            .and_then(|cs| cs.borrow().as_ref().map(|s| s.tile_cache.len()))
            .unwrap_or(0);
        let world = self.world_size();
        html! {
            <>
                <p>{format!("World: {} \u{00d7} {} px", world.0 as u64, world.1 as u64)}</p>
                { for self.layers.iter().map(|layer| {
                    let level = self.level_of(layer);
                    html!{ <p>{format!("{}: level {} / {}", layer.name, level, layer.num_levels().saturating_sub(1))}</p> }
                })}
                if self.tiles_pending > 0 {
                    <p>{format!("Tiles: {} cached, {} pending", cached, self.tiles_pending)}</p>
                } else {
                    <p>{format!("Tiles: {} cached", cached)}</p>
                }
                if let Some(ref picked) = self.picked {
                    <p>{format!("{}: id {} ({}) at ({:.0}, {:.0})",
                        picked.layer_name, picked.id, picked.dtype,
                        picked.world.0, picked.world.1)}</p>
                    if let Some(region) = &picked.region {
                        <p>{region.clone()}</p>
                    }
                    // What the store itself says about the id, from
                    // `image-label.properties` — the in-spec place for it.
                    if let Some(described) = self.label_properties(picked) {
                        <p>{described}</p>
                    }
                    if let Some(value) = picked.value {
                        <p>{format!("value {}", value)}</p>
                    }
                }
            </>
        }
    }
}
