use yew::prelude::*;

/// Props for the Z-slice and time point sliders.
#[derive(Properties, PartialEq)]
pub struct AxisSlidersProps {
    pub z_max: u32,
    pub t_max: u32,
    pub z_current: u32,
    pub t_current: u32,
    pub on_z_change: Callback<u32>,
    pub on_t_change: Callback<u32>,
}

/// Render Z-slice and time point sliders (hidden when max <= 1).
#[function_component(AxisSliders)]
pub fn axis_sliders(props: &AxisSlidersProps) -> Html {
    let on_z = {
        let cb = props.on_z_change.clone();
        Callback::from(move |e: Event| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Ok(v) = input.value().parse::<u32>() {
                    cb.emit(v);
                }
            }
        })
    };

    let on_t = {
        let cb = props.on_t_change.clone();
        Callback::from(move |e: Event| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Ok(v) = input.value().parse::<u32>() {
                    cb.emit(v);
                }
            }
        })
    };

    html! {
        <>
            if props.z_max > 1 {
                <div class="axis-slider">
                    <label>{format!("Z slice: {} / {}", props.z_current, props.z_max - 1)}</label>
                    <input
                        type="range"
                        min="0"
                        max={(props.z_max - 1).to_string()}
                        value={props.z_current.to_string()}
                        onchange={on_z}
                    />
                </div>
            }
            if props.t_max > 1 {
                <div class="axis-slider">
                    <label>{format!("Time: {} / {}", props.t_current, props.t_max - 1)}</label>
                    <input
                        type="range"
                        min="0"
                        max={(props.t_max - 1).to_string()}
                        value={props.t_current.to_string()}
                        onchange={on_t}
                    />
                </div>
            }
        </>
    }
}
