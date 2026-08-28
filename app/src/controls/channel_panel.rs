use yew::prelude::*;

const DEFAULT_COLORS: [[f32; 3]; 6] = [
    [0.0, 1.0, 0.0], // green
    [1.0, 0.0, 1.0], // magenta
    [0.0, 1.0, 1.0], // cyan
    [1.0, 1.0, 0.0], // yellow
    [1.0, 0.0, 0.0], // red
    [0.0, 0.0, 1.0], // blue
];

/// Return a default channel color by index (green, red, blue, ...).
pub fn default_color(index: usize) -> [f32; 3] {
    DEFAULT_COLORS[index % DEFAULT_COLORS.len()]
}

/// Convert an RGB float triplet to a hex color string.
pub fn color_to_hex(c: &[f32; 3]) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        (c[0] * 255.0) as u8,
        (c[1] * 255.0) as u8,
        (c[2] * 255.0) as u8
    )
}

/// Parse a hex color string into an RGB float triplet.
fn hex_to_color(hex: &str) -> Option<[f32; 3]> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()? as f32 / 255.0;
    Some([r, g, b])
}

/// Props for the per-channel control panel (sliders, color picker, visibility).
#[derive(Properties, PartialEq)]
pub struct ChannelPanelProps {
    pub index: usize,
    pub label: String,
    pub visible: bool,
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub contrast_limit: f32,
    pub opacity: f32,
    pub on_visibility: Callback<bool>,
    pub on_color: Callback<[f32; 3]>,
    pub on_contrast_min: Callback<f32>,
    pub on_contrast_max: Callback<f32>,
    pub on_opacity: Callback<f32>,
}

/// Render the control panel for a single channel.
#[function_component(ChannelPanel)]
pub fn channel_panel(props: &ChannelPanelProps) -> Html {
    let on_vis = {
        let cb = props.on_visibility.clone();
        let current = props.visible;
        Callback::from(move |_: Event| {
            cb.emit(!current);
        })
    };

    let on_color = {
        let cb = props.on_color.clone();
        Callback::from(move |e: Event| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Some(c) = hex_to_color(&input.value()) {
                    cb.emit(c);
                }
            }
        })
    };

    let on_cmin = {
        let cb = props.on_contrast_min.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Ok(v) = input.value().parse::<f32>() {
                    cb.emit(v);
                }
            }
        })
    };

    let on_cmax = {
        let cb = props.on_contrast_max.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Ok(v) = input.value().parse::<f32>() {
                    cb.emit(v);
                }
            }
        })
    };

    let on_opacity = {
        let cb = props.on_opacity.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<web_sys::HtmlInputElement>() {
                if let Ok(v) = input.value().parse::<f32>() {
                    cb.emit(v / 100.0);
                }
            }
        })
    };

    let color_hex = color_to_hex(&props.color);
    let opacity_pct = (props.opacity * 100.0) as i32;

    html! {
        <div class="channel-control">
            <div class="channel-header">
                <input
                    type="checkbox"
                    checked={props.visible}
                    onchange={on_vis}
                />
                <label>{&props.label}</label>
                <input
                    type="color"
                    class="color-picker"
                    value={color_hex}
                    onchange={on_color}
                />
            </div>
            if props.visible {
                <div class="slider-row">
                    <label>{"Contrast"}</label>
                    <div class="dual-range">
                        <input
                            type="range"
                            min="0"
                            max={props.contrast_limit.to_string()}
                            step="1"
                            value={props.contrast_min.to_string()}
                            oninput={on_cmin}
                            class="dual-range-min"
                        />
                        <input
                            type="range"
                            min="0"
                            max={props.contrast_limit.to_string()}
                            step="1"
                            value={props.contrast_max.to_string()}
                            oninput={on_cmax}
                            class="dual-range-max"
                        />
                    </div>
                    <span class="value">{format!("{:.0}-{:.0}", props.contrast_min, props.contrast_max)}</span>
                </div>
                <div class="slider-row">
                    <label>{"Opacity"}</label>
                    <input
                        type="range"
                        min="0"
                        max="100"
                        value={opacity_pct.to_string()}
                        oninput={on_opacity}
                    />
                    <span class="value">{format!("{}%", opacity_pct)}</span>
                </div>
            }
        </div>
    }
}
