//! Controls for one label layer: how it is drawn, what is selected, and what
//! each id has been classed as.
//!
//! The classing half is annotation for training an **object classifier**: the
//! instances already exist, so the work is a class per id and the raster is
//! never touched. Its whole vocabulary rests on one distinction the panel has
//! to keep visible — an id with no assignment has not been looked at, and an id
//! assigned the empty class has been looked at and was nothing in particular.

use yew::prelude::*;

use crate::controls::channel_panel::color_to_hex;
use crate::controls::layer_header::LayerHeader;
use crate::controls::slider_row::SliderRow;
use crate::layers::assigned_class_color;

/// Props for a label layer's controls.
#[derive(Properties, PartialEq)]
pub struct LabelPanelProps {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub outline: bool,
    pub selected: u32,
    pub only_selected: bool,
    /// True when the store named colours and they are being used.
    pub has_lut: bool,

    // --- Classing.
    /// Does a click on the image class the id it lands on?
    pub classing: bool,
    /// The class the next click assigns.
    pub class: String,
    /// What the selected id is classed as: `None` when nothing has been said
    /// about it, `Some("")` when it was looked at and was nothing in
    /// particular. Collapsing the two would be collapsing the fact.
    pub selected_class: Option<String>,
    /// How many ids carry a class at all.
    pub assigned: usize,
    /// How many of those were classed as nothing in particular.
    pub as_nothing: usize,
    /// The classes in use, for the key.
    pub classes: Vec<String>,
    pub color_by_class: bool,
    pub save_target: String,
    /// The label image the table names, relative to the tables group.
    pub region: String,
    pub saving: bool,
    pub status: Option<String>,

    pub on_visibility: Callback<bool>,
    pub on_opacity: Callback<f32>,
    pub on_outline: Callback<bool>,
    pub on_only_selected: Callback<bool>,
    pub on_clear_selection: Callback<()>,
    pub on_classing: Callback<bool>,
    pub on_class: Callback<String>,
    /// Put the class in the box on the selected id.
    pub on_assign: Callback<()>,
    /// Forget the selected id, which is not classing it as nothing.
    pub on_unassign: Callback<()>,
    pub on_color_by_class: Callback<bool>,
    pub on_save_target: Callback<String>,
    pub on_region: Callback<String>,
    pub on_save: Callback<()>,
    pub on_remove: Callback<()>,
}

/// What an id classed as nothing in particular is called, wherever it is shown.
///
/// One spelling, because the whole point is that a reader tells it apart from
/// "not looked at" at a glance, and two wordings for it would undo that.
const NOTHING: &str = "nothing in particular";

fn checkbox(cb: Callback<bool>) -> Callback<Event> {
    Callback::from(move |e: Event| {
        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
        cb.emit(input.checked());
    })
}

fn text(cb: Callback<String>) -> Callback<InputEvent> {
    Callback::from(move |e: InputEvent| {
        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
        cb.emit(input.value());
    })
}

fn press(cb: Callback<()>) -> Callback<MouseEvent> {
    Callback::from(move |_: MouseEvent| cb.emit(()))
}

/// Controls for one label layer: opacity, outline mode, what is selected, and
/// the class each id carries.
#[function_component(LabelPanel)]
pub fn label_panel(props: &LabelPanelProps) -> Html {
    // `SliderRow` hands back the input's text; the number behind it is ours.
    let on_opacity = {
        let cb = props.on_opacity.clone();
        Callback::from(move |text: String| {
            if let Ok(v) = text.parse::<f32>() {
                cb.emit(v);
            }
        })
    };

    html! {
        <div class="channel-control">
            <LayerHeader
                name={props.name.clone()}
                visible={Some(props.visible)}
                on_visibility={props.on_visibility.clone()}
                on_remove={props.on_remove.clone()}
            />
            <SliderRow label="Opacity" min="0" max="1" step="0.01"
                value={props.opacity.to_string()} display={format!("{:.2}", props.opacity)}
                on_input={on_opacity} />
            <div class="slider-row">
                <label>
                    <input type="checkbox" checked={props.outline}
                        onchange={checkbox(props.on_outline.clone())} />
                    {" Outlines only"}
                </label>
            </div>
            <div class="slider-row">
                <label>
                    <input type="checkbox" checked={props.only_selected}
                        disabled={props.selected == 0}
                        onchange={checkbox(props.on_only_selected.clone())} />
                    {" Isolate selection"}
                </label>
            </div>
            <div class="info-text">
                if props.selected == 0 {
                    <p>{"Click a label to select it"}</p>
                } else {
                    <p>
                        { format!("Selected id: {}", props.selected) }
                        <button class="layer-remove" onclick={press(props.on_clear_selection.clone())}
                            title="Clear selection">{"\u{2715}"}</button>
                    </p>
                }
                if props.color_by_class {
                    <p>{"Colours: the classes below"}</p>
                } else if props.has_lut {
                    <p>{"Colours: image-label table"}</p>
                } else {
                    <p>{"Colours: hashed from id"}</p>
                }
            </div>
            { class_section(props) }
        </div>
    }
}

/// Classing the ids: the mode, the class, what the picked id carries, and where
/// the table goes.
fn class_section(props: &LabelPanelProps) -> Html {
    html! {
        <>
        <div class="slider-row">
            <label title="Assign the class below to whatever id the next click lands on. \
                          The label image itself is never written.">
                <input type="checkbox" class="label-classing" checked={props.classing}
                    onchange={checkbox(props.on_classing.clone())} />
                {" Class ids by clicking"}
            </label>
        </div>
        <div class="slider-row">
            <span>{"Class"}</span>
            <input type="text" class="label-class" placeholder={format!("empty = {NOTHING}")}
                value={props.class.clone()} oninput={text(props.on_class.clone())} />
        </div>
        { selected_row(props) }
        <div class="slider-row">
            <label>
                <input type="checkbox" class="label-color-by-class" checked={props.color_by_class}
                    onchange={checkbox(props.on_color_by_class.clone())} />
                {" Colour ids by class"}
            </label>
        </div>
        if props.color_by_class && !props.classes.is_empty() {
            <div class="class-key">
                { for props.classes.iter().map(|class| html! {
                    <span class="class-chip">
                        <span class="class-swatch"
                            style={format!("background: {}", color_to_hex(&assigned_class_color(class)))} />
                        { if class.is_empty() { NOTHING.to_string() } else { class.clone() } }
                    </span>
                })}
            </div>
        }
        <div class="slider-row">
            // Not the annotation panel's `/path/to/image.zarr/...` wording:
            // the browser suites reach that box by its placeholder prefix, and
            // a second box starting the same way would be picked up instead.
            <input type="text" class="label-save-target"
                placeholder="<store>.zarr/tables/cell_types"
                value={props.save_target.clone()}
                oninput={text(props.on_save_target.clone())} />
            <button class="label-save" onclick={press(props.on_save.clone())}
                disabled={props.saving}>
                { if props.saving { "Saving\u{2026}" } else { "Save classes" } }
            </button>
        </div>
        // The region is what makes the table more than a column of numbers: it
        // names the label image the ids belong to, relative to the tables
        // group. Guessed from this layer's own source, and editable because
        // only the person who wrote the store knows when the guess is wrong.
        <div class="slider-row">
            <span>{"Region"}</span>
            <input type="text" class="label-region" placeholder="../labels/nuclei"
                value={props.region.clone()} oninput={text(props.on_region.clone())} />
        </div>
        <div class="info-text">
            <p class="label-class-count">{ counts(props) }</p>
            if let Some(status) = &props.status {
                <p>{ status.clone() }</p>
            }
        </div>
        </>
    }
}

/// The class of the id under the last click, and the two ways to change it.
///
/// Both are offered here and not only by clicking, because the two states a
/// curator has to be able to express are not the same gesture: assigning the
/// empty class *says something* about the id, and clearing it takes the id back
/// to never having been looked at.
fn selected_row(props: &LabelPanelProps) -> Html {
    if props.selected == 0 {
        return html! {};
    }
    html! {
        <div class="slider-row label-selected-class">
            <span class="label-class-of">{ match props.selected_class.as_deref() {
                None => format!("id {} \u{2014} not looked at", props.selected),
                Some("") => format!("id {} \u{2014} {NOTHING}", props.selected),
                Some(class) => format!("id {} \u{2014} {class}", props.selected),
            } }</span>
            <button class="label-assign" onclick={press(props.on_assign.clone())}
                title="Class this id as what the box above says">
                {"Assign"}
            </button>
            if props.selected_class.is_some() {
                <button class="layer-remove label-unassign"
                    onclick={press(props.on_unassign.clone())}
                    title="Forget this id \u{2014} back to not looked at, which is \
                           not the same as classed as nothing">
                    {"\u{2715}"}
                </button>
            }
        </div>
    }
}

/// How much of the set has been looked at, and what was found.
///
/// The two numbers are reported apart because they answer different questions:
/// how far the pass has got, and how many of the instances were rejected.
fn counts(props: &LabelPanelProps) -> String {
    if props.assigned == 0 {
        return "no ids classed yet".to_string();
    }
    let named = props.assigned - props.as_nothing;
    let mut text = format!("{} id(s) classed", props.assigned);
    if props.as_nothing > 0 {
        text.push_str(&format!(", {} as {NOTHING}", props.as_nothing));
    }
    if named > 0 {
        let names: Vec<&str> = props
            .classes
            .iter()
            .filter(|c| !c.is_empty())
            .map(String::as_str)
            .collect();
        text.push_str(&format!(" \u{00b7} {}", names.join(", ")));
    }
    text
}
