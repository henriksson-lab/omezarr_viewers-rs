use omezarr_viewer_common::ObjectSchema;
use yew::prelude::*;

use crate::controls::channel_panel::{color_to_hex, hex_to_color};

/// Props for an object layer's controls.
#[derive(Properties, PartialEq)]
pub struct ObjectPanelProps {
    pub name: String,
    pub visible: bool,
    pub schema: ObjectSchema,
    pub count: u64,
    pub color: [f32; 3],
    pub opacity: f32,
    pub size: f32,
    pub hollow: bool,
    pub color_by: Option<usize>,
    pub filters: Vec<Option<(f32, f32)>>,
    pub slab: f32,
    /// Rows loaded, rows drawn after filtering, rows that matched on the server.
    pub loaded: usize,
    pub shown: usize,
    pub total: usize,
    /// The selected row, as the server described it.
    pub selected: Option<String>,
    pub on_visibility: Callback<bool>,
    pub on_color: Callback<[f32; 3]>,
    pub on_opacity: Callback<f32>,
    pub on_size: Callback<f32>,
    pub on_hollow: Callback<bool>,
    pub on_color_by: Callback<Option<usize>>,
    /// `(column, filter)` — `None` clears the column's filter.
    pub on_filter: Callback<(usize, Option<(f32, f32)>)>,
    pub on_slab: Callback<f32>,
    pub on_remove: Callback<()>,
}

/// Controls for one object layer: how the points look, and which ones show.
#[function_component(ObjectPanel)]
pub fn object_panel(props: &ObjectPanelProps) -> Html {
    let on_visibility = {
        let cb = props.on_visibility.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked());
        })
    };
    let on_color = {
        let cb = props.on_color.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Some(color) = hex_to_color(&input.value()) {
                cb.emit(color);
            }
        })
    };
    let slider = |cb: Callback<f32>| {
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Ok(value) = input.value().parse::<f32>() {
                cb.emit(value);
            }
        })
    };
    let on_hollow = {
        let cb = props.on_hollow.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked());
        })
    };
    let on_color_by = {
        let cb = props.on_color_by.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            cb.emit(input.value().parse::<usize>().ok());
        })
    };
    let on_remove = {
        let cb = props.on_remove.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    html! {
        <div class="channel-control">
            <div class="channel-header">
                <label>
                    <input type="checkbox" checked={props.visible} onchange={on_visibility} />
                    { format!(" {}", props.name) }
                </label>
                <input type="color" class="color-picker"
                    value={color_to_hex(&props.color)} onchange={on_color} />
                <button class="layer-remove" onclick={on_remove} title="Close layer">{"\u{2715}"}</button>
            </div>

            <div class="slider-row">
                <span>{"Size"}</span>
                <input type="range" min="2" max="40" step="1"
                    value={props.size.to_string()} oninput={slider(props.on_size.clone())} />
                <span class="slider-value">{format!("{:.0}px", props.size)}</span>
            </div>
            <div class="slider-row">
                <span>{"Opacity"}</span>
                <input type="range" min="0" max="1" step="0.01"
                    value={props.opacity.to_string()} oninput={slider(props.on_opacity.clone())} />
                <span class="slider-value">{format!("{:.2}", props.opacity)}</span>
            </div>
            if props.schema.has_z {
                <div class="slider-row">
                    <span>{"Z slab"}</span>
                    <input type="range" min="0" max="64" step="1"
                        value={props.slab.to_string()} oninput={slider(props.on_slab.clone())} />
                    <span class="slider-value">{format!("\u{00b1}{:.0}", props.slab)}</span>
                </div>
            }
            <div class="slider-row">
                <label>
                    <input type="checkbox" checked={props.hollow} onchange={on_hollow} />
                    {" Rings"}
                </label>
            </div>

            <div class="slider-row">
                <span>{"Colour by"}</span>
                <select onchange={on_color_by}>
                    <option value="" selected={props.color_by.is_none()}>{"(fixed)"}</option>
                    { for props.schema.columns.iter().enumerate().map(|(i, column)| html! {
                        <option value={i.to_string()} selected={props.color_by == Some(i)}>
                            { column.name.clone() }
                        </option>
                    })}
                </select>
            </div>

            { for props.schema.columns.iter().enumerate().map(|(i, column)| {
                let range = column.range.unwrap_or([0.0, 1.0]);
                let current = props.filters.get(i).copied().flatten();
                let (lo, hi) = current.unwrap_or((range[0] as f32, range[1] as f32));
                let step = ((range[1] - range[0]) / 200.0).max(1e-6);
                let on_lo = {
                    let cb = props.on_filter.clone();
                    Callback::from(move |e: InputEvent| {
                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                        if let Ok(v) = input.value().parse::<f32>() {
                            cb.emit((i, Some((v, hi))));
                        }
                    })
                };
                let on_hi = {
                    let cb = props.on_filter.clone();
                    Callback::from(move |e: InputEvent| {
                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                        if let Ok(v) = input.value().parse::<f32>() {
                            cb.emit((i, Some((lo, v))));
                        }
                    })
                };
                let on_clear = {
                    let cb = props.on_filter.clone();
                    Callback::from(move |_: MouseEvent| cb.emit((i, None)))
                };
                html! {
                    <div class="slider-row filter-row">
                        <span>{ column.name.clone() }</span>
                        <div class="dual-range">
                            <input type="range" class="dual-range-min"
                                min={range[0].to_string()} max={range[1].to_string()}
                                step={step.to_string()} value={lo.to_string()} oninput={on_lo} />
                            <input type="range" class="dual-range-max"
                                min={range[0].to_string()} max={range[1].to_string()}
                                step={step.to_string()} value={hi.to_string()} oninput={on_hi} />
                        </div>
                        <span class="slider-value">
                            { if current.is_some() { format!("{lo:.3}\u{2013}{hi:.3}") } else { "all".to_string() } }
                        </span>
                        if current.is_some() {
                            <button class="layer-remove" onclick={on_clear} title="Clear filter">{"\u{2715}"}</button>
                        }
                    </div>
                }
            })}

            <div class="info-text">
                <p>{ counts(props) }</p>
                if let Some(selected) = &props.selected {
                    <p>{ selected.clone() }</p>
                } else {
                    <p>{"Click a point to inspect it"}</p>
                }
            </div>
        </div>
    }
}

/// What is being shown, and — when it is not everything — what is not.
fn counts(props: &ObjectPanelProps) -> String {
    let mut text = format!("{} of {} in view", props.shown, props.total);
    if props.loaded < props.total {
        text.push_str(&format!(" (capped at {})", props.loaded));
    }
    text.push_str(&format!(", {} in the set", props.count));
    text
}
