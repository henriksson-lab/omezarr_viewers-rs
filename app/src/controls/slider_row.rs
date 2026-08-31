//! One labelled range slider with its readout — the row the panels repeat.
//!
//! Nine of these sit across four panels and were nine copies of the same five
//! lines. The browser suites address them by class (`.slider-row`,
//! `.slider-value`) and by the input's `min`/`max`/`step`/`value`, so the copies
//! drifting apart is a test failure somewhere else entirely.
//!
//! **The number stays with the caller.** The values behind these rows are `f32`
//! in one panel, `f64` in another and a plain count in a third, and each row
//! formats its readout its own way — `12px`, `0.35`, `\u{00b1}4`, `3 z`, `all z`.
//! A component that rendered the number itself would have to pick one type, and
//! widening an `f32` to reach it changes what lands in the DOM: `0.33f32` as an
//! `f64` prints `0.33000001311302185`. So the slider's position and its readout
//! arrive as text the caller already wrote, and only the event comes back as
//! text for the caller to parse into whatever type it keeps.
//!
//! The readout is not optional because every slider row here has one. What
//! wears `slider-row` *without* being one of these is deliberately not a caller:
//!
//! * the dual-range controls — `channel_panel`'s contrast and `object_panel`'s
//!   per-column filter — are two overlapping inputs, and the filter has a clear
//!   button besides. That is a different control that happens to share the row
//!   layout, not this one with a part missing.
//! * the checkbox, `<select>` and text-input rows wear `slider-row` because it
//!   is the panels' row layout, not because there is a slider in them.
//! * `channel_panel`'s opacity names itself with a `<label>` and reads out in
//!   `.value` rather than `.slider-value`. Serving it would mean this component
//!   emitting two spellings of the same row; it is left as it is until the
//!   markup itself is reconciled.
//! * `axis_sliders` is a different row entirely — its own class, and `onchange`
//!   rather than `oninput`, because a z-slice change costs a fetch per step.

use yew::prelude::*;

/// Props for one range-slider row.
#[derive(Properties, PartialEq)]
pub struct SliderRowProps {
    /// What the row is called, in the leading span.
    pub label: AttrValue,
    pub min: AttrValue,
    pub max: AttrValue,
    pub step: AttrValue,
    /// Where the handle sits, as the caller renders its own number.
    pub value: AttrValue,
    /// The readout, already formatted — units and all.
    pub display: AttrValue,
    /// The input's raw text, for the caller to parse.
    pub on_input: Callback<String>,
}

/// Render one slider row.
#[function_component(SliderRow)]
pub fn slider_row(props: &SliderRowProps) -> Html {
    let on_input = {
        let cb = props.on_input.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.value());
        })
    };

    html! {
        <div class="slider-row">
            <span>{ props.label.clone() }</span>
            <input type="range" min={props.min.clone()} max={props.max.clone()}
                step={props.step.clone()} value={props.value.clone()} oninput={on_input} />
            <span class="slider-value">{ props.display.clone() }</span>
        </div>
    }
}
