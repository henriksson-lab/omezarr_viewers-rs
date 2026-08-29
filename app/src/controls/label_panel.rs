use yew::prelude::*;

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
    pub on_visibility: Callback<bool>,
    pub on_opacity: Callback<f32>,
    pub on_outline: Callback<bool>,
    pub on_only_selected: Callback<bool>,
    pub on_clear_selection: Callback<()>,
    pub on_remove: Callback<()>,
}

/// Controls for one label layer: opacity, outline mode, and what is selected.
#[function_component(LabelPanel)]
pub fn label_panel(props: &LabelPanelProps) -> Html {
    let on_visibility = {
        let cb = props.on_visibility.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked());
        })
    };
    let on_opacity = {
        let cb = props.on_opacity.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Ok(v) = input.value().parse::<f32>() {
                cb.emit(v);
            }
        })
    };
    let on_outline = {
        let cb = props.on_outline.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked());
        })
    };
    let on_only_selected = {
        let cb = props.on_only_selected.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked());
        })
    };
    let on_clear = {
        let cb = props.on_clear_selection.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
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
                <button class="layer-remove" onclick={on_remove} title="Close layer">{"\u{2715}"}</button>
            </div>
            <div class="slider-row">
                <span>{"Opacity"}</span>
                <input type="range" min="0" max="1" step="0.01"
                    value={props.opacity.to_string()} oninput={on_opacity} />
                <span class="slider-value">{format!("{:.2}", props.opacity)}</span>
            </div>
            <div class="slider-row">
                <label>
                    <input type="checkbox" checked={props.outline} onchange={on_outline} />
                    {" Outlines only"}
                </label>
            </div>
            <div class="slider-row">
                <label>
                    <input type="checkbox" checked={props.only_selected}
                        disabled={props.selected == 0} onchange={on_only_selected} />
                    {" Isolate selection"}
                </label>
            </div>
            <div class="info-text">
                if props.selected == 0 {
                    <p>{"Click a label to select it"}</p>
                } else {
                    <p>
                        { format!("Selected id: {}", props.selected) }
                        <button class="layer-remove" onclick={on_clear} title="Clear selection">{"\u{2715}"}</button>
                    </p>
                }
                if props.has_lut {
                    <p>{"Colours: image-label table"}</p>
                } else {
                    <p>{"Colours: hashed from id"}</p>
                }
            </div>
        </div>
    }
}
