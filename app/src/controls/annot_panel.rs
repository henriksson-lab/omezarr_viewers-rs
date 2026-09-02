//! Controls for one annotation layer: how the marks look, what they are called,
//! which of them show, and where they get written.

use omezarr_viewer_common::{in_tree_order, Annotation, Geometry, ObjectType};
use yew::prelude::*;

use crate::layers::LayerStyle;

use crate::controls::channel_panel::color_to_hex;
use crate::controls::layer_header::LayerHeader;
use crate::controls::slider_row::SliderRow;

/// Props for an annotation layer's controls.
#[derive(Properties, PartialEq)]
pub struct AnnotPanelProps {
    pub name: String,
    pub visible: bool,
    /// Every annotation in the layer, filtered or not — the list says which are
    /// hidden rather than hiding them from the list too.
    pub annotations: Vec<Annotation>,
    /// How the layer is drawn.
    pub style: LayerStyle,
    pub color_by_class: bool,
    /// Size points by a world radius rather than by `style.size` screen pixels.
    pub world_radius: bool,
    /// The radius the *next* shape gets, in world pixels — the class named
    /// below when there is one, the layer's default when there is not.
    pub radius: f32,
    pub filled: bool,
    pub selected: Option<u64>,
    /// The class the next mark drawn into this layer gets.
    pub class: String,
    /// The object type new shapes get.
    pub object_type: ObjectType,
    /// The stroke width the next open path gets, in world pixels, or `None` for
    /// a geometric line covering no pixels at all.
    pub stroke_width: Option<f64>,
    /// Show only this class, when one is chosen.
    pub filter: Option<String>,
    /// Every class present, for the filter list.
    pub classes: Vec<String>,
    /// The colour each class draws in, parallel to `classes`.
    pub class_colors: Vec<[f32; 3]>,
    /// Drawn now, of how many there are.
    pub shown: usize,
    /// How many shapes assert that everything inside them is annotated, and how
    /// many carry a stroke width. Counted by the layer rather than here, so the
    /// panel and the picture cannot come to different answers.
    pub dense: usize,
    pub scribbles: usize,
    pub save_target: String,
    pub saving: bool,
    pub dirty: bool,
    pub status: Option<String>,
    /// Does the session have more than one timepoint? The t controls are noise
    /// when it does not.
    pub has_t: bool,
    pub on_visibility: Callback<bool>,
    pub on_color: Callback<[f32; 3]>,
    pub on_color_by_class: Callback<bool>,
    pub on_world_radius: Callback<bool>,
    pub on_radius: Callback<f32>,
    pub on_filled: Callback<bool>,
    pub on_opacity: Callback<f32>,
    pub on_size: Callback<f32>,
    pub on_slab: Callback<f32>,
    pub on_class: Callback<String>,
    pub on_object_type: Callback<ObjectType>,
    /// The width new open paths get. `None` turns the stroke off entirely.
    pub on_stroke_width: Callback<Option<f64>>,
    /// Per-object, on the selected annotation.
    pub on_name: Callback<String>,
    /// The selected shape's own stroke width, or `None` for a geometric line.
    pub on_selected_stroke: Callback<Option<f64>>,
    /// Mark the selected shape as asserting that everything inside it is
    /// annotated, or unmark it.
    pub on_dense: Callback<bool>,
    pub on_selected_type: Callback<ObjectType>,
    pub on_locked: Callback<bool>,
    pub on_filter: Callback<Option<String>>,
    pub on_select: Callback<Option<u64>>,
    /// `(id, class)` — rename one annotation.
    pub on_rename: Callback<(u64, String)>,
    pub on_delete: Callback<u64>,
    pub on_delete_all: Callback<()>,
    /// Rebuild the hierarchy from where the shapes now are.
    pub on_renest: Callback<()>,
    /// Lift the selected annotation out of its parent.
    pub on_detach: Callback<()>,
    /// Depth and duration of the *selected* annotation.
    pub on_z_extent: Callback<f64>,
    pub on_t_extent: Callback<f64>,
    pub on_save_target: Callback<String>,
    pub on_save: Callback<()>,
    pub on_remove: Callback<()>,
}

/// The most rows listed at once.
///
/// A hand-drawn set is small, but a table read off disk need not be, and a
/// thousand text inputs is a frozen tab. The count line says what is not shown
/// rather than the list quietly stopping.
const MAX_LISTED: usize = 200;

/// The object types offered, with what to call them in a list.
///
/// `root` is left out: it is QuPath's hierarchy anchor, has no geometry, and is
/// not something a person makes.
const TYPES: [(ObjectType, &str); 5] = [
    (ObjectType::Annotation, "annotation"),
    (ObjectType::Detection, "detection"),
    (ObjectType::Cell, "cell"),
    (ObjectType::Tile, "tile"),
    (ObjectType::TmaCore, "TMA core"),
];

/// Sentinels for the two filter options that are not a class name.
///
/// Printable, because a `<select>` value is something a test — or a person
/// reading the DOM — has to be able to type.
const ALL_CLASSES: &str = "__all__";
const UNCLASSIFIED: &str = "__unclassified__";

/// The event adapters every row here needs. Free functions rather than
/// closures inside the component, so the sections below can each have them.
fn checkbox(cb: Callback<bool>) -> Callback<Event> {
    Callback::from(move |e: Event| {
        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
        cb.emit(input.checked());
    })
}

/// `SliderRow` hands back the input's text, because the number behind it is a
/// different type in every panel; these two turn it back into ours.
fn slider(cb: Callback<f32>) -> Callback<String> {
    Callback::from(move |text: String| {
        if let Ok(value) = text.parse::<f32>() {
            cb.emit(value);
        }
    })
}

fn slider64(cb: Callback<f64>) -> Callback<String> {
    Callback::from(move |text: String| {
        if let Ok(value) = text.parse::<f64>() {
            cb.emit(value);
        }
    })
}

fn text(cb: Callback<String>) -> Callback<InputEvent> {
    Callback::from(move |e: InputEvent| {
        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
        cb.emit(input.value());
    })
}

/// The width a stroke starts at when one is turned on, in world pixels.
///
/// A visible band at the zoom regions are usually drawn at, and a round number
/// so what it means is legible: a scribble 8 wide covers the pixels within 4 of
/// the path.
const DEFAULT_STROKE_WIDTH: f64 = 8.0;

/// One "is this a scribble, and how wide" row.
///
/// Two controls rather than one number, because *no width* and *zero width* are
/// different claims and must not share a value. `None` is a geometric line —
/// GeoJSON's `LineString`, a curve covering no pixels at all — and only the
/// checkbox says it; the number input refuses anything that is not positive, so
/// there is no way to type a zero and land somewhere ambiguous. The row says
/// which of the two states it is in in words, not by an empty box.
///
/// The width is in **world** pixels: it is an assertion about the image, so it
/// must not change with the zoom the curator happened to be at.
fn stroke_row(width: Option<f64>, hook: &str, on_change: &Callback<Option<f64>>) -> Html {
    let toggle = {
        let cb = on_change.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            cb.emit(input.checked().then_some(DEFAULT_STROKE_WIDTH));
        })
    };
    let typed = {
        let cb = on_change.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            match input.value().parse::<f64>() {
                Ok(value) if value > 0.0 => cb.emit(Some(value)),
                // Not a width. Turning the stroke off is what the checkbox is
                // for, and doing it from here would mean 0 said it too.
                _ => {}
            }
        })
    };
    html! {
        <div class="slider-row">
            <label title="A scribble covers the pixels within half this width of \
                          the path, in the image's own pixels. Off, the path is a \
                          geometric line covering no pixels \u{2014} which is a \
                          different claim, not a narrower one.">
                <input type="checkbox" class={format!("{hook}-on")}
                    checked={width.is_some()} onchange={toggle} />
                {" Scribble"}
            </label>
            if let Some(value) = width {
                <input type="number" class={format!("{hook}-width")}
                    min="0.5" step="0.5" value={format!("{value}")} onchange={typed} />
                <span class={format!("slider-value {hook}-readout")}>{"px wide"}</span>
            } else {
                <span class={format!("slider-value {hook}-readout")}>{"geometric line"}</span>
            }
        </div>
    }
}

fn object_type(cb: Callback<ObjectType>) -> Callback<Event> {
    Callback::from(move |e: Event| {
        let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
        cb.emit(ObjectType::parse(&input.value()));
    })
}

#[function_component(AnnotPanel)]
pub fn annot_panel(props: &AnnotPanelProps) -> Html {
    let confirming = use_state(|| false);

    let points = props.annotations.iter().filter(|a| a.is_point()).count();
    let boxes = props.annotations.len() - points;
    let selected = props
        .selected
        .and_then(|id| props.annotations.iter().find(|a| a.id == id));

    html! {
        <div class="channel-control">
            { layer_section(props) }
            { selected_section(props, selected) }
            { shape_table(props) }
            { store_section(props, &confirming, points, boxes) }
        </div>
    }
}

/// The layer as a whole: what it is called, how it is drawn, and which class is
/// being looked at.
fn layer_section(props: &AnnotPanelProps) -> Html {
    let on_filter = {
        let cb = props.on_filter.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            // "every class" and "the class whose name is empty" are different
            // answers, so both get a sentinel rather than one of them being the
            // empty string and colliding with a real class name.
            cb.emit(match input.value().as_str() {
                ALL_CLASSES => None,
                UNCLASSIFIED => Some(String::new()),
                class => Some(class.to_string()),
            });
        })
    };
    html! {
        <>
        <LayerHeader
            name={props.name.clone()}
            visible={Some(props.visible)}
            on_visibility={props.on_visibility.clone()}
            color={Some(props.style.color)}
            on_color={props.on_color.clone()}
            dirty={props.dirty}
            on_remove={props.on_remove.clone()}
        />

        // What the *next* shape gets. `objectType` is QuPath's processing
        // role, not the semantic kind — the kind is the class beside it —
        // but it decides how QuPath treats the objects on the way back.
        <div class="slider-row">
            <span>{"Class"}</span>
            <input type="text" placeholder="for new shapes"
                value={props.class.clone()} oninput={text(props.on_class.clone())} />
        </div>
        <div class="slider-row">
            <span>{"Type"}</span>
            <select class="annot-new-type"
                onchange={object_type(props.on_object_type.clone())}
                title="What QuPath treats new shapes as. Hand-drawn work is an annotation; \
                       algorithm output is a detection.">
                { for TYPES.iter().map(|(kind, shown)| html! {
                    <option value={kind.as_str()} selected={props.object_type == *kind}>
                        { *shown }
                    </option>
                })}
            </select>
        </div>
        // What a new *open* path covers. A closed region already says where
        // its pixels are; a path has no width until one is given, and the
        // difference is the difference between partial and total supervision.
        { stroke_row(props.stroke_width, "annot-stroke", &props.on_stroke_width) }
        <div class="slider-row">
            <span>{"Show"}</span>
            <select class="annot-filter" onchange={on_filter}>
                <option value={ALL_CLASSES} selected={props.filter.is_none()}>{"all classes"}</option>
                { for props.classes.iter().map(|class| {
                    let selected = props.filter.as_deref() == Some(class.as_str());
                    let (value, shown) = if class.is_empty() {
                        (UNCLASSIFIED.to_string(), "(unclassified)".to_string())
                    } else {
                        (class.clone(), class.clone())
                    };
                    html! { <option value={value} selected={selected}>{shown}</option> }
                })}
            </select>
        </div>
        <div class="slider-row">
            <label>
                <input type="checkbox" checked={props.color_by_class}
                    onchange={checkbox(props.on_color_by_class.clone())} />
                {" Colour by class"}
            </label>
            <label>
                <input type="checkbox" checked={props.filled}
                    onchange={checkbox(props.on_filled.clone())} />
                {" Fill"}
            </label>
        </div>
        if props.color_by_class && !props.classes.is_empty() {
            <div class="class-key">
                { for props.classes.iter().zip(props.class_colors.iter()).map(|(class, color)| html! {
                    <span class="class-chip">
                        <span class="class-swatch"
                            style={format!("background: {}", color_to_hex(color))} />
                        { if class.is_empty() { "(unclassified)".to_string() } else { class.clone() } }
                    </span>
                })}
            </div>
        }
        <SliderRow label="Size" min="2" max="40" step="1"
            value={props.style.size.to_string()} display={format!("{:.0}px", props.style.size)}
            on_input={slider(props.on_size.clone())} />
        // Two ways for a point to have a size, and the choice is per layer.
        // A marker is a fixed number of screen pixels, which is what keeps a
        // detection visible at every zoom. A radius is a claim about the
        // image — a particle pick's circle either encloses the particle or it
        // does not — so it is world pixels and moves with the camera.
        <div class="slider-row">
            <label title="Draw points as a circle of a true size in the image, \
                          growing and shrinking with the zoom, rather than as a \
                          fixed-size screen marker.">
                <input type="checkbox" class="annot-world-radius"
                    checked={props.world_radius}
                    onchange={checkbox(props.on_world_radius.clone())} />
                {" True size"}
            </label>
        </div>
        // Only while it is in use: a radius the layer is not drawing with is a
        // number that does nothing, which reads as a broken control.
        if props.world_radius {
            <SliderRow label={radius_label(props)} min="1" max="1024" step="1"
                value={props.radius.to_string()}
                display={format!("r {:.0}", props.radius)}
                on_input={slider(props.on_radius.clone())} />
        }
        <SliderRow label="Opacity" min="0" max="1" step="0.01"
            value={props.style.opacity.to_string()} display={format!("{:.2}", props.style.opacity)}
            on_input={slider(props.on_opacity.clone())} />
        <SliderRow label="Z slab" min="0" max="64" step="1"
            value={props.style.slab.to_string()}
            display={if props.style.slab > 0.0 { format!("\u{00b1}{:.0}", props.style.slab) } else { "all z".to_string() }}
            on_input={slider(props.on_slab.clone())} />
        </>
    }
}

/// What the radius row is called.
///
/// It edits one class's radius or the layer's default, decided by the class box
/// above it, so it says which — a slider that silently changed meaning when a
/// class was typed would be a slider nobody could trust.
fn radius_label(props: &AnnotPanelProps) -> String {
    if props.class.is_empty() {
        "Radius".to_string()
    } else {
        format!("Radius ({})", props.class)
    }
}

/// The rows that belong to one annotation, and so only appear when one is
/// selected.
fn selected_section(props: &AnnotPanelProps, selected: Option<&Annotation>) -> Html {
    html! {
        <>
        // These belong to one annotation, so they only appear when there is
        // one to apply them to.
        if let Some(item) = selected {
            <div class="slider-row">
                <span>{"Name"}</span>
                <input type="text" placeholder="this shape's own name"
                    value={item.name.clone().unwrap_or_default()}
                    onchange={Callback::from({
                        let cb = props.on_name.clone();
                        move |e: Event| {
                            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                            cb.emit(input.value());
                        }
                    })} />
            </div>
            <div class="slider-row">
                <span>{"Type"}</span>
                <select class="annot-selected-type"
                    onchange={object_type(props.on_selected_type.clone())}>
                    { for TYPES.iter().map(|(kind, shown)| html! {
                        <option value={kind.as_str()} selected={item.object_type == *kind}>
                            { *shown }
                        </option>
                    })}
                </select>
                <label title="A locked shape cannot be moved or reshaped">
                    <input type="checkbox" checked={item.locked}
                        onchange={checkbox(props.on_locked.clone())} />
                    {" Locked"}
                </label>
            </div>
            { stroke_row(item.stroke_width, "annot-selected-stroke", &props.on_selected_stroke) }
            // What an unlabelled pixel means, and there is no default that is
            // safe to guess. Sparse — the default — says a mark covers what it
            // covers and asserts nothing about anything else. Dense says the
            // curator marked *every* instance inside this shape, so uncovered
            // means background there. A trainer given the second when the first
            // was true learns that every unmarked object is background.
            <div class="slider-row">
                <label title="Everything inside this shape is annotated: uncovered \
                              pixels in it are background, not unexamined. Leave it \
                              off unless every instance inside really is marked.">
                    <input type="checkbox" class="annot-dense" checked={item.dense_region}
                        onchange={checkbox(props.on_dense.clone())} />
                    {" Dense region"}
                </label>
            </div>
            <SliderRow label="Depth" min="0" max="64" step="1"
                value={item.z_extent.to_string()}
                display={format!("{} z", item.z_extent + 1)}
                on_input={slider64(props.on_z_extent.clone())} />
            if props.has_t {
                <SliderRow label="Frames" min="0" max="64" step="1"
                    value={item.t_extent.to_string()}
                    display={format!("t {}\u{2013}{}", item.plane.t, item.plane.t + item.t_extent as i32)}
                    on_input={slider64(props.on_t_extent.clone())} />
            }
        }
        </>
    }
}

/// The list itself, in tree order.
fn shape_table(props: &AnnotPanelProps) -> Html {
    html! {
        <table class="annot-table">
            { for in_tree_order(&props.annotations).into_iter().take(MAX_LISTED)
                 .map(|(item, depth)| {
                let id = item.id;
                let on_select = {
                    let cb = props.on_select.clone();
                    let already = props.selected == Some(id);
                    Callback::from(move |_: MouseEvent| cb.emit((!already).then_some(id)))
                };
                let on_rename = {
                    let cb = props.on_rename.clone();
                    Callback::from(move |e: Event| {
                        let input: web_sys::HtmlInputElement = e.target_unchecked_into();
                        cb.emit((id, input.value()));
                    })
                };
                let on_delete = {
                    let cb = props.on_delete.clone();
                    Callback::from(move |_: MouseEvent| cb.emit(id))
                };
                let row_class = if props.selected == Some(id) {
                    "annot-row selected"
                } else {
                    "annot-row"
                };
                let children = props
                    .annotations
                    .iter()
                    .filter(|a| a.parent == Some(id))
                    .count();
                html! {
                    <tr class={row_class}>
                        <td class={if item.dense_region { "annot-kind dense" } else { "annot-kind" }}
                            onclick={on_select} title={describe(item)}
                            // Indent by nesting depth, so the list reads as
                            // the tree it is. QuPath's hierarchy is spatial:
                            // a cell drawn inside a region is a child of it.
                            style={format!("padding-left: {}px", 2 + depth * 10)}>
                            { glyph_of(item) }
                            // A dense region is a different kind of claim from
                            // everything else in this list, so it is marked
                            // here too rather than only on the canvas.
                            if item.dense_region {
                                <span class="annot-dense-mark"
                                    title="Dense: everything inside is annotated">
                                    {"\u{25a8}"}
                                </span>
                            }
                        </td>
                        <td>
                            <input type="text" value={item.label.clone()}
                                placeholder={placeholder_for(item)} onchange={on_rename} />
                        </td>
                        <td class="annot-children">
                            if children > 0 {
                                <span title={format!("{children} inside")}>
                                    { format!("{children}") }
                                </span>
                            }
                        </td>
                        <td>
                            <button class="layer-remove" onclick={on_delete}
                                title="Delete annotation \u{2014} anything inside it stays">
                                {"\u{2715}"}
                            </button>
                        </td>
                    </tr>
                }
            })}
        </table>
    }
}

/// Saving, re-nesting, deleting, and what the layer currently holds.
fn store_section(
    props: &AnnotPanelProps,
    confirming: &UseStateHandle<bool>,
    points: usize,
    boxes: usize,
) -> Html {
    let selected = props
        .selected
        .and_then(|id| props.annotations.iter().find(|a| a.id == id));
    let on_save = {
        let cb = props.on_save.clone();
        Callback::from(move |_: MouseEvent| cb.emit(()))
    };
    // Two clicks, because deleting a screenful is the one action here that is
    // worth being sure about — even with undo behind it.
    let on_delete_all = {
        let cb = props.on_delete_all.clone();
        let confirming = confirming.clone();
        Callback::from(move |_: MouseEvent| {
            if *confirming {
                cb.emit(());
                confirming.set(false);
            } else {
                confirming.set(true);
            }
        })
    };
    html! {
        <>

        <div class="slider-row">
            <input type="text" placeholder="/path/to/image.zarr/tables/name"
                value={props.save_target.clone()}
                oninput={text(props.on_save_target.clone())} />
            <button onclick={on_save} disabled={props.saving}>
                { if props.saving { "Saving\u{2026}" } else { "Save" } }
            </button>
        </div>
        <div class="slider-row">
            <button onclick={Callback::from({
                let cb = props.on_renest.clone();
                move |_: MouseEvent| cb.emit(())
            })} title="Rebuild the nesting from where the shapes are now">
                {"Re-nest"}
            </button>
            if selected.is_some_and(|item| item.parent.is_some()) {
                <button onclick={Callback::from({
                    let cb = props.on_detach.clone();
                    move |_: MouseEvent| cb.emit(())
                })} title="Lift this shape out of whatever contains it">
                    {"Detach"}
                </button>
            }
        </div>
        <div class="slider-row">
            <button class={if **confirming { "danger" } else { "" }} onclick={on_delete_all}>
                { if **confirming {
                    format!("Delete {} \u{2014} click again", props.shown)
                } else {
                    "Delete shown".to_string()
                } }
            </button>
        </div>

        <div class="info-text">
            <p>{ counts(props, points, boxes) }</p>
            { supervision(props) }
            <p class="hint">
                {"Drag a vertex to move it \u{00b7} shift-click a vertex to \
                  delete \u{00b7} shift-click an edge to insert"}
            </p>
            if let Some(status) = &props.status {
                <p>{ status.clone() }</p>
            }
        </div>
        </>
    }
}

/// What to show in an unclassified row's class box.
///
/// A named object says its name there rather than "unclassified", because that
/// is what identifies it — the box still edits the class.
fn placeholder_for(item: &Annotation) -> String {
    match item.name.as_deref() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "unclassified".to_string(),
    }
}

/// One annotation as a line of text, for the row's tooltip.
fn describe(item: &Annotation) -> String {
    let [x0, y0, x1, y1] = item.bounds().unwrap_or([0.0; 4]);
    let mut text = format!("{} at ({x0:.0}, {y0:.0})", kind_of(item));
    if !item.is_point() {
        text.push_str(&format!(", {:.0}\u{00d7}{:.0}", x1 - x0, y1 - y0));
    }
    let (z0, z1) = item.z_range();
    text.push_str(&format!(", z {z0}"));
    if z1 != z0 {
        text.push_str(&format!("\u{2013}{z1}"));
    }
    if item.plane.t != 0 || item.t_extent != 0 {
        text.push_str(&format!(
            ", t {}\u{2013}{}",
            item.plane.t,
            item.plane.t + item.t_extent as i32
        ));
    }
    if let Some(width) = item.stroke_width.filter(|w| *w > 0.0) {
        text.push_str(&format!(", stroke {width} wide"));
    }
    if item.dense_region {
        text.push_str(", dense");
    }
    if let Some(name) = &item.name {
        text.push_str(&format!(" \u{00b7} {name}"));
    }
    text
}

/// What kind of shape this is, in one word.
fn kind_of(item: &Annotation) -> &'static str {
    if item.is_ellipse {
        return "ellipse";
    }
    if item.nucleus.is_some() {
        return "cell";
    }
    match &item.geometry {
        Geometry::Point(_) => "point",
        Geometry::MultiPoint(_) => "points",
        Geometry::LineString(_) | Geometry::MultiLineString(_) => "line",
        Geometry::Polygon(rings) if rings.len() > 1 => "region with holes",
        Geometry::Polygon(_) => "region",
        Geometry::MultiPolygon(_) => "regions",
    }
}

/// The glyph shown in the list for each kind of shape.
fn glyph_of(item: &Annotation) -> &'static str {
    if item.is_ellipse {
        return "\u{25ef}";
    }
    match &item.geometry {
        Geometry::Point(_) | Geometry::MultiPoint(_) => "\u{25cf}",
        Geometry::LineString(_) | Geometry::MultiLineString(_) => "\u{2571}",
        Geometry::Polygon(rings) if rings.len() > 1 => "\u{25a3}",
        _ => "\u{25a1}",
    }
}

/// What this layer says about the pixels nothing covers.
///
/// The two counts are the layer's supervision state, and there is nowhere else
/// to read it: a set with no dense region asserts nothing about any pixel it
/// does not cover, and one with a dense region asserts *background* everywhere
/// inside it. Both are legitimate; reading one as the other is what ruins a
/// training set, so the panel says which this is rather than leaving it to be
/// inferred from the drawing.
fn supervision(props: &AnnotPanelProps) -> Html {
    let (dense, scribbles) = (props.dense, props.scribbles);
    html! {
        <p class="annot-supervision">
            <span class="annot-dense-count">{ format!("{dense} dense region(s)") }</span>
            { " \u{00b7} " }
            <span class="annot-scribble-count">{ format!("{scribbles} scribble(s)") }</span>
            <br />
            <span class="hint">
                { if dense == 0 {
                    "No dense region: every pixel nothing covers is unexamined."
                } else {
                    "Unlabelled means background inside a dense region, \
                     and unexamined everywhere else."
                } }
            </span>
        </p>
    }
}

fn counts(props: &AnnotPanelProps, points: usize, boxes: usize) -> String {
    let total = props.annotations.len();
    let mut text = format!("{points} point(s), {boxes} box(es)");
    if props.shown != total {
        text.push_str(&format!(" \u{2014} {} drawn", props.shown));
    }
    if total > MAX_LISTED {
        text.push_str(&format!(", {MAX_LISTED} listed"));
    }
    text
}
