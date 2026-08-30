//! A table layer, shown as a table.
//!
//! Two ngio table types have no geometry of their own and so cannot be drawn on
//! the image at all:
//!
//! * a **feature table** is per-object measurements keyed to a label image —
//!   one row per label id, `area`, `intensity_mean` and so on;
//! * a **condition table** is experiment-level metadata, which has no position
//!   even in principle.
//!
//! For the first, the useful rendering is not a table at all: it is to paint the
//! label image it describes, colouring each id by one of its columns. That is
//! what the "Colour" control does, and it is why a feature table is worth
//! opening beside a label layer. The table below it is for reading the numbers
//! the picture cannot show.

use omezarr_viewer_common::TableInfo;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct TablePanelProps {
    pub name: String,
    pub table: TableInfo,
    /// The rows fetched so far, as text.
    pub rows: Vec<Vec<String>>,
    pub loading: bool,
    /// The column currently painting a label layer.
    pub coloring: Option<String>,
    /// The label layer being painted, when one was matched.
    pub target: Option<String>,
    /// Every label layer open, to say which one this could paint.
    pub label_layers: Vec<String>,
    pub on_more: Callback<()>,
    /// `None` clears the colouring.
    pub on_color_by: Callback<Option<String>>,
    pub on_remove: Callback<()>,
}

#[function_component(TablePanel)]
pub fn table_panel(props: &TablePanelProps) -> Html {
    let numeric: Vec<&str> = props
        .table
        .columns
        .iter()
        .filter(|c| c.is_number())
        .map(|c| c.name.as_str())
        .collect();

    let on_color_by = {
        let cb = props.on_color_by.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            cb.emit(match input.value().as_str() {
                "" => None,
                name => Some(name.to_string()),
            })
        })
    };
    let on_more = {
        let cb = props.on_more.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    let on_remove = {
        let cb = props.on_remove.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };

    html! {
        <div class="channel-control">
            <div class="channel-header">
                <label>{ format!(" {}", props.name) }</label>
                <button class="layer-remove" onclick={on_remove} title="Close layer">
                    {"\u{2715}"}
                </button>
            </div>

            <div class="info-text">
                <p>{ describe(&props.table) }</p>
                if let Some(region) = &props.table.region {
                    <p>{ format!("describes {region}") }</p>
                }
            </div>

            // Painting the label image is the only way a table with no
            // coordinates can be *seen*, so it comes before the numbers.
            if !numeric.is_empty() && props.table.region.is_some() {
                <div class="slider-row">
                    <span>{"Colour"}</span>
                    <select onchange={on_color_by}>
                        <option value="" selected={props.coloring.is_none()}>{"(off)"}</option>
                        { for numeric.iter().map(|name| {
                            let selected = props.coloring.as_deref() == Some(*name);
                            html! {
                                <option value={name.to_string()} selected={selected}>
                                    { name.to_string() }
                                </option>
                            }
                        })}
                    </select>
                </div>
                <div class="info-text">
                    if let Some(target) = &props.target {
                        <p>{ format!("painting {target}") }</p>
                    } else if props.label_layers.is_empty() {
                        <p class="hint">
                            {"Open the label image it describes to paint it"}
                        </p>
                    } else {
                        <p class="hint">
                            { format!("No open label layer matches; {} open",
                                      props.label_layers.join(", ")) }
                        </p>
                    }
                </div>
            }

            <div class="table-scroll">
                <table class="data-table">
                    <thead>
                        <tr>
                            { for props.table.columns.iter().map(|column| html! {
                                <th title={format!("{} ({})", column.name, column.kind)}>
                                    { column.name.clone() }
                                </th>
                            })}
                        </tr>
                    </thead>
                    <tbody>
                        { for props.rows.iter().map(|row| html! {
                            <tr>
                                { for row.iter().map(|cell| html! {
                                    <td title={cell.clone()}>{ cell.clone() }</td>
                                })}
                            </tr>
                        })}
                    </tbody>
                </table>
            </div>

            if props.rows.len() < props.table.rows {
                <div class="slider-row">
                    <button onclick={on_more} disabled={props.loading}>
                        { if props.loading {
                            "Loading\u{2026}".to_string()
                        } else {
                            format!("Show more \u{2014} {} of {}", props.rows.len(), props.table.rows)
                        } }
                    </button>
                </div>
            }
        </div>
    }
}

/// What kind of table this is, in words, and how big.
fn describe(table: &TableInfo) -> String {
    let kind = match table.table_type.as_str() {
        "feature_table" => "feature table — one row per label",
        "condition_table" => "condition table — metadata for the image",
        "masking_roi_table" => "masking ROI table",
        "roi_table" => "ROI table",
        other => other,
    };
    format!(
        "{kind} \u{00b7} {} rows, {} columns",
        table.rows,
        table.columns.len()
    )
}
