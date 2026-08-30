//! The first row of a layer's control panel: what it is called, whether it is
//! drawn, what colour it draws in, and how to close it.
//!
//! Four of the five panels open with the same markup, and the browser suites
//! address it by class (`.channel-header`, `.layer-remove`, `.dirty-dot`), so
//! it is one component rather than four copies that can drift apart.
//!
//! The parts that not every layer has are optional props rather than a flag
//! choosing between layouts: a table has no visibility to toggle and a label
//! image has no colour of its own, and each simply omits that element from the
//! same row. `channel_panel` is deliberately not a caller — its checkbox sits
//! beside the name rather than inside its `<label>`, which is a different row,
//! not this one with a part missing.

use yew::prelude::*;

use crate::controls::channel_panel::{color_to_hex, hex_to_color};

/// Props for the header row shared by the layer control panels.
#[derive(Properties, PartialEq)]
pub struct LayerHeaderProps {
    pub name: String,
    /// `None` for a layer with nothing to show or hide — a table.
    #[prop_or_default]
    pub visible: Option<bool>,
    #[prop_or_default]
    pub on_visibility: Callback<bool>,
    /// `None` for a layer that does not draw in one colour — a label image
    /// colours by id.
    #[prop_or_default]
    pub color: Option<[f32; 3]>,
    #[prop_or_default]
    pub on_color: Callback<[f32; 3]>,
    /// Unsaved changes, which only a written layer can have.
    #[prop_or_default]
    pub dirty: bool,
    pub on_remove: Callback<()>,
}

/// Render one layer's header row.
#[function_component(LayerHeader)]
pub fn layer_header(props: &LayerHeaderProps) -> Html {
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
    let on_remove = {
        let cb = props.on_remove.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    html! {
        <div class="channel-header">
            <label>
                if let Some(visible) = props.visible {
                    <input type="checkbox" checked={visible} onchange={on_visibility} />
                }
                { format!(" {}", props.name) }
                if props.dirty {
                    <span class="dirty-dot" title="unsaved changes">{"\u{25cf}"}</span>
                }
            </label>
            if let Some(color) = props.color {
                <input type="color" class="color-picker"
                    value={color_to_hex(&color)} onchange={on_color} />
            }
            <button class="layer-remove" onclick={on_remove} title="Close layer">{"\u{2715}"}</button>
        </div>
    }
}
