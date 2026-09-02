use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use crate::webgl::renderer::{
    Blend, FillBuffer, FillRenderInfo, LabelRenderInfo, LineBuffer, LineRenderInfo, PointBuffer,
    PointRenderInfo, Renderer, TileTexture,
};

mod draw;
mod input;

/// Per-channel rendering parameters passed as props to the canvas.
#[derive(Clone, PartialEq)]
pub struct ChannelRenderInfo {
    pub color: [f32; 3],
    pub contrast_min: f32,
    pub contrast_max: f32,
    pub opacity: f32,
}

/// How one layer is drawn, and with which program.
#[derive(Clone, PartialEq)]
pub enum LayerRenderKind {
    Image {
        channels: Vec<ChannelRenderInfo>,
        dtype_max: f32,
        /// How this layer meets the layers under it.
        blend: Blend,
    },
    Labels(LabelRenderInfo),
    Objects(PointRenderInfo),
    /// Two programs, because an annotation layer holds two primitives: points
    /// for the zero-extent marks and box outlines for the rest. Colour comes
    /// from the batch rather than from here, so a layer coloured by class is
    /// one draw call per class with everything else shared.
    Annotations {
        points: PointRenderInfo,
        lines: LineRenderInfo,
        fills: FillRenderInfo,
    },
}

/// What a drag on the canvas does.
///
/// The set is QuPath's, minus the raster brush: a raster annotation belongs in
/// a `labels` image, which is in the OME-Zarr spec and which this viewer
/// already reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tool {
    /// Drag to pan, click to inspect, drag a handle to edit. The normal mode.
    #[default]
    Pan,
    /// Click to drop a point.
    Point,
    /// Drag out an axis-aligned rectangle.
    Box,
    /// Drag out an ellipse, kept as a polygon plus QuPath's `isEllipse` flag.
    Ellipse,
    /// Click each vertex; click the first again, or double-click, to close.
    Polygon,
    /// Click each vertex; double-click to finish, leaving the path open.
    Polyline,
    /// Drag to trace a closed region freehand.
    Freehand,
    /// Drag to trace an open path freehand.
    Line,
}

impl Tool {
    /// Does this tool draw? A drawing tool takes the drag away from the camera,
    /// which is why panning has to be told apart from it before the mouse moves
    /// rather than after.
    pub fn draws(self) -> bool {
        !matches!(self, Tool::Pan)
    }

    /// Is this tool built up click by click rather than in one drag?
    pub fn is_multi_click(self) -> bool {
        matches!(self, Tool::Polygon | Tool::Polyline)
    }

    /// Does the shape close back on itself?
    pub fn closes(self) -> bool {
        matches!(
            self,
            Tool::Box | Tool::Ellipse | Tool::Polygon | Tool::Freehand
        )
    }

    /// Whether a finished shape from this tool is an **open path**, and so
    /// takes the layer's stroke width.
    ///
    /// Not simply `!closes()`: Point and Pan produce no path at all. The
    /// authority on this is `geometry_of`, which decides by looking at the
    /// geometry it built — a shortcut is needed here because the draft has to
    /// be drawn before there is a geometry to look at, and
    /// `a_tool_draws_an_open_path_exactly_when_its_geometry_is_one` pins the
    /// two together so they cannot drift.
    pub fn draws_open_path(self) -> bool {
        matches!(self, Tool::Line | Tool::Polyline)
    }
}

/// One colour's worth of an annotation layer, on the GPU.
pub struct AnnotBuffer {
    pub color: [f32; 3],
    /// The world radius its points draw at, or 0 for a screen-space marker.
    pub radius: f32,
    pub points: PointBuffer,
    /// Where each point is, on the CPU, for the frames a world radius has
    /// outgrown the point-sprite cap and the circles are built as geometry.
    /// Empty unless `radius > 0`.
    pub markers: Vec<[f32; 4]>,
    pub lines: LineBuffer,
    pub fills: FillBuffer,
}

/// What a drag on the selected annotation has hold of.
///
/// Which handles a shape offers is QuPath's rule: a **rectangle or ellipse**
/// gets bounding-box corners, because those are what define it; anything else
/// gets its own **vertices**, because a polygon's shape is its vertices and
/// scaling one from a corner is not how anybody edits a boundary. Both can be
/// moved bodily.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handle {
    /// A bounding-box corner, as `(west, north)`. Rectangles and ellipses.
    Corner(bool, bool),
    /// One vertex, addressed as `(ring or line index, vertex index)`.
    Vertex(usize, usize),
    /// The interior: move the whole thing.
    Body,
}

/// What a drag is doing to an existing annotation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    /// Drag it into a new position or size.
    Drag,
    /// Remove the vertex under the pointer.
    DeleteVertex,
    /// Add a vertex on the edge under the pointer.
    InsertVertex,
}

/// An edit in progress on an existing annotation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Editing {
    pub id: u64,
    pub handle: Handle,
    pub kind: EditKind,
    /// Where the pointer went down, in world pixels.
    pub from: (f32, f32),
    /// Where it is now.
    pub to: (f32, f32),
}

impl Editing {
    /// How far the pointer has travelled, in world pixels.
    pub fn delta(&self) -> (f32, f32) {
        (self.to.0 - self.from.0, self.to.1 - self.from.1)
    }

    /// Did this drag actually change anything?
    ///
    /// A click that grabbed a handle and let go is a selection, not an edit,
    /// and sending it as one would mark the layer dirty for nothing. A vertex
    /// insert or delete is a click by nature and always counts.
    pub fn changes(&self) -> bool {
        if self.kind != EditKind::Drag {
            return true;
        }
        let (dx, dy) = self.delta();
        dx.abs() > 0.01 || dy.abs() > 0.01
    }
}

/// What the canvas needs to know about the selected annotation to edit it.
///
/// Handed down as props rather than looked up, because the canvas holds no
/// annotations — the layer state does, and this is the slice of it a drag needs.
#[derive(Clone, PartialEq, Debug)]
pub struct Editable {
    pub id: u64,
    /// `(x0, y0, x1, y1)` in world pixels.
    pub bounds: (f32, f32, f32, f32),
    /// Every editable vertex, grouped by ring or line, in world pixels.
    pub paths: Vec<Vec<(f32, f32)>>,
    /// True for a rectangle or an ellipse, which are edited by their bounding
    /// corners rather than by their vertices.
    pub boxlike: bool,
    /// True for a point, which can only be moved.
    pub puncta: bool,
    /// The file says this object must not be edited. It still selects, so its
    /// properties can be read and the lock can be taken off.
    pub locked: bool,
    /// The width this shape's outline covers, if it is a stroke.
    ///
    /// A scribble is aimed at by its **band**, because the band is what is on
    /// screen; a tolerance measured from the centreline would refuse a
    /// shift-click the user could see land well inside the shape.
    pub stroke_width: Option<f64>,
}

/// How far from a selected shape still counts as *on* it, in world pixels.
///
/// `near` is the hand's tolerance — a fixed number of screen pixels expressed
/// in world ones, so it shrinks as the view zooms in. A **stroke** is aimed at
/// by the band drawn on screen rather than by the centreline inside it, so on a
/// wide scribble that band is the target and `near` alone is a small fraction
/// of it: an edge click that plainly landed on the annotation would miss.
///
/// `near` stays the floor. A band narrower than a hand is steady must not
/// become *harder* to hit than a bare line would have been.
pub fn grab_reach(near: f32, stroke_width: Option<f64>) -> f32 {
    near.max(stroke_width.unwrap_or(0.0) as f32 / 2.0)
}

/// Which handle of `editable` is under `(x, y)`, and what dragging it would do.
///
/// The order is deliberate: a **vertex** beats an **edge** beats the **body**. A
/// vertex is the smallest target and the one a hand aims at; an edge is only a
/// target when `shift` asks to insert into it; and the body is the fallback that
/// moves the whole shape.
///
/// Pure, and separated from the component for that reason: every rule in here
/// was a bug before it was a rule, and a browser is an expensive and coarse way
/// to ask what a click three pixels from a corner does.
pub fn grab_at(
    editable: &Editable,
    x: f32,
    y: f32,
    shift: bool,
    near: f32,
) -> Option<(Handle, EditKind)> {
    // `isLocked` is not decoration: a file that says "do not edit this" is one
    // somebody locked on purpose, and a viewer that edits it anyway is worse
    // than one that cannot edit at all.
    if editable.locked {
        return None;
    }
    let reach = grab_reach(near, editable.stroke_width);

    // A point has nothing but a body: all four of its "corners" are the same
    // coordinate, so a corner drag would resize a zero-size box into a
    // zero-size box and look like nothing happening at all.
    if editable.puncta {
        let (x0, y0, x1, y1) = editable.bounds;
        let hit = x >= x0 - near && x <= x1 + near && y >= y0 - near && y <= y1 + near;
        return hit.then_some((Handle::Body, EditKind::Drag));
    }

    if editable.boxlike {
        // A rectangle or an ellipse is defined by its bounding box, so that
        // is what it offers — which is also what QuPath offers for them.
        let (x0, y0, x1, y1) = editable.bounds;
        for (west, north, cx, cy) in [
            (true, true, x0, y0),
            (false, true, x1, y0),
            (true, false, x0, y1),
            (false, false, x1, y1),
        ] {
            if (x - cx).abs() <= near && (y - cy).abs() <= near {
                return Some((Handle::Corner(west, north), EditKind::Drag));
            }
        }
    } else {
        // Everything else is edited by its vertices, because a polygon's
        // shape *is* its vertices.
        for (path_index, path) in editable.paths.iter().enumerate() {
            for (vertex, point) in path.iter().enumerate() {
                if (x - point.0).abs() <= near && (y - point.1).abs() <= near {
                    let kind = if shift {
                        EditKind::DeleteVertex
                    } else {
                        EditKind::Drag
                    };
                    return Some((Handle::Vertex(path_index, vertex), kind));
                }
            }
        }
        // Shift on an edge inserts a vertex there; without shift an edge is
        // just part of the body, and dragging it moves the whole shape.
        if shift {
            for (path_index, path) in editable.paths.iter().enumerate() {
                for vertex in 0..path.len() {
                    let a = path[vertex];
                    let b = path[(vertex + 1) % path.len()];
                    if segment_distance(x, y, a, b) <= reach {
                        return Some((Handle::Vertex(path_index, vertex), EditKind::InsertVertex));
                    }
                }
            }
            return None;
        }
    }

    // `reach`, not `near`: the bounds are the *vertices*, and a stroke's
    // band stands half its width outside them. A click on the visible edge
    // of a wide scribble is outside the vertex bounds and inside the shape.
    let (x0, y0, x1, y1) = editable.bounds;
    if x >= x0 - reach && x <= x1 + reach && y >= y0 - reach && y <= y1 + reach {
        return Some((Handle::Body, EditKind::Drag));
    }
    None
}

/// Is this drawn shape worth storing?
///
/// A drag that barely moved was a misfire, not a zero-size region, and storing
/// it litters the layer with rows nothing can see. A *point* is the exception:
/// a click is exactly what it is.
pub fn is_worth_keeping(drawn: &Drawn) -> bool {
    if drawn.tool == Tool::Point {
        return true;
    }
    let Some((x0, y0, x1, y1)) = drawn.corners() else {
        return false;
    };
    if matches!(drawn.tool, Tool::Freehand | Tool::Line) {
        // A traced path is worth keeping if it went anywhere at all, which its
        // vertex count says better than its bounding box does.
        return drawn.points.len() >= 3;
    }
    (x1 - x0).abs() >= 1.0 || (y1 - y0).abs() >= 1.0
}

/// A shape the user just finished drawing, in world pixels.
///
/// A path rather than two corners, because most of these tools are not
/// rectangles: the box and the ellipse are the *first and last* of a two-point
/// path, and the caller builds the geometry the tool implies.
#[derive(Clone, PartialEq, Debug)]
pub struct Drawn {
    pub tool: Tool,
    pub points: Vec<(f32, f32)>,
}

impl Drawn {
    /// The bounding corners, for the tools defined by a drag.
    pub fn corners(&self) -> Option<(f32, f32, f32, f32)> {
        let first = self.points.first()?;
        let last = self.points.last()?;
        Some((first.0, first.1, last.0, last.1))
    }
}

/// One layer's rendering parameters, in draw order.
#[derive(Clone, PartialEq)]
pub struct LayerRenderInfo {
    pub id: String,
    pub visible: bool,
    pub kind: LayerRenderKind,
}

/// 2D camera state: pan position, zoom level, and drag tracking.
pub struct Camera2d {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
    pub canvas_w: f32,
    pub canvas_h: f32,
    pub dragging: bool,
    /// Set once a drag actually moves, so a click can be told from a pan.
    pub dragged: bool,
    pub last_mouse: (f32, f32),
    pub pinch_dist: Option<f32>,
    pub pinch_center: (f32, f32),
}

/// Cache key identifying a tile by layer, pyramid level, grid position, and channel.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub struct TileKey {
    pub layer: String,
    pub level: usize,
    pub tile_y: u32,
    pub tile_x: u32,
    /// The channel, for image layers; 0 for label layers, which have one.
    pub channel: usize,
}

/// Tile grid metadata for one layer at one pyramid level.
#[derive(Clone, Debug, PartialEq)]
pub struct LevelTileInfo {
    /// Size of this level in the layer's own pixels.
    pub level_size: (f32, f32),
    /// Tile size in the layer's own pixels.
    pub tile_size: (f32, f32),
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
    /// Multiply a layer pixel by this to get a world pixel.
    ///
    /// This is what lets a half-resolution label volume land on top of the
    /// image it describes rather than in a quarter of it.
    pub world_scale: (f32, f32),
}

/// Shared mutable state between App (tile uploads) and ViewerCanvas (rendering).
/// How much GPU memory tile textures may hold.
///
/// A number rather than a query, because WebGL2 offers no way to ask how much
/// VRAM there is or how much is left. Chosen to be comfortable on an integrated
/// GPU while leaving room for the label textures, the annotation buffers and
/// whatever else the page is doing; the geometric rule normally keeps the store
/// far below it, and this only decides what happens when it does not.
pub const TILE_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Tile textures held on the GPU, with a byte budget.
///
/// Two things this fixes, and they are different problems.
///
/// **Textures were never freed explicitly.** `TileTexture` holds a
/// `WebGlTexture` and nothing implemented `Drop`, so dropping one released the
/// JS handle and left the GPU memory to be reclaimed whenever the JS garbage
/// collector next ran. That is not a leak — WebGL ties the object's lifetime to
/// its wrapper — but the collector cannot see VRAM pressure, and a wasm app
/// that drops two hundred megabytes of textures has not grown the JS heap at
/// all, so nothing prompts it. Every removal here hands the texture back to the
/// caller, which deletes it; that is the only reason `evict` and `retain`
/// return what they dropped instead of dropping it themselves.
///
/// **Nothing capped the total.** Eviction was geometric only — keep this level
/// and coarser, and within a level only what is on screen. That is the right
/// *primary* rule, because it encodes what is actually visible. The budget is a
/// backstop under it, for the case the geometry alone does not bound: a large
/// window, a fine level and several channels.
///
/// Recency is by **insertion**, not by use. Tracking use would mean a mutable
/// borrow on every lookup in the draw path, and it would buy little: the
/// geometric rule has already decided what is visible, so the budget is only
/// choosing which *off-screen fallback* to give up, and the oldest is a fair
/// answer to that.
///
/// Generic over the key as well as the payload so the orthogonal panes can
/// hold their planes in the same store rather than in a second one with its
/// own eviction rules: what they cache is not tiles and is not keyed like a
/// tile, but the two problems this solves — explicit deletion, and a cap in
/// bytes — are exactly the same ones.
pub struct TileStore<T = TileTexture, K = TileKey> {
    entries: HashMap<K, Held<T>>,
    /// Bytes of texture currently held, by the same arithmetic the uploads use.
    held: usize,
    /// Monotonic insertion counter; the smallest is evicted first.
    clock: u64,
    capacity: usize,
}

struct Held<T> {
    texture: T,
    bytes: usize,
    inserted: u64,
}

impl<T, K: Eq + Hash + Clone> TileStore<T, K> {
    pub fn new(capacity_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            held: 0,
            clock: 0,
            capacity: capacity_bytes,
        }
    }

    pub fn get(&self, key: &K) -> Option<&T> {
        self.entries.get(key).map(|held| &held.texture)
    }

    pub fn contains_key(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Bytes of GPU texture this is holding.
    pub fn bytes(&self) -> usize {
        self.held
    }

    /// Add a tile, returning any texture the caller must now delete — the one
    /// it replaced, plus whatever the budget pushed out.
    #[must_use = "the returned textures are still on the GPU until deleted"]
    pub fn insert(&mut self, key: K, texture: T, width: u32, height: u32) -> Vec<T> {
        let bytes = bytes_for(width, height);
        let mut freed = Vec::new();
        self.clock += 1;
        let held = Held {
            texture,
            bytes,
            inserted: self.clock,
        };
        if let Some(old) = self.entries.insert(key.clone(), held) {
            self.held -= old.bytes;
            freed.push(old.texture);
        }
        self.held += bytes;
        freed.extend(self.trim(&key));
        freed
    }

    /// Keep only what `keep` says, returning the rest to be deleted.
    #[must_use = "the returned textures are still on the GPU until deleted"]
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) -> Vec<T> {
        let doomed: Vec<K> = self
            .entries
            .keys()
            .filter(|key| !keep(key))
            .cloned()
            .collect();
        doomed
            .into_iter()
            .filter_map(|key| self.remove(&key))
            .collect()
    }

    fn remove(&mut self, key: &K) -> Option<T> {
        let held = self.entries.remove(key)?;
        self.held -= held.bytes;
        Some(held.texture)
    }

    /// Drop the oldest until the budget is met, never the entry just inserted.
    ///
    /// A store that instantly forgets what it was just handed is worse than one
    /// that sits marginally over budget: the caller has already deleted its own
    /// copy of the handle, so the entry it needs *now* would be the one it
    /// cannot draw. Tiles never come near this — a tile is a megabyte against a
    /// budget of hundreds — but an orthogonal plane crosses the whole store and
    /// can on its own be a large fraction of a pane's budget.
    fn trim(&mut self, protected: &K) -> Vec<T> {
        let mut freed = Vec::new();
        while self.capacity > 0 && self.held > self.capacity {
            let Some(victim) = self
                .entries
                .iter()
                .filter(|(key, _)| *key != protected)
                .min_by_key(|(_, held)| held.inserted)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(texture) = self.remove(&victim) {
                freed.push(texture);
            }
        }
        freed
    }
}

/// What one tile costs on the GPU.
///
/// Both texture kinds are one 32-bit channel — `R32F` for intensity, `R32UI`
/// for labels, which is why an id above 2^24 survives — so the arithmetic is
/// the same for each and there is nothing to estimate. An exact size is what
/// makes a byte budget enforceable rather than advisory.
pub fn bytes_for(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_cache: TileStore,
    /// Grid metadata per `(layer id, level)`.
    pub level_info: HashMap<(String, usize), LevelTileInfo>,
    /// The level each layer is currently drawing at.
    pub current_level: HashMap<String, usize>,
    /// One uploaded point batch per object layer, keyed by layer id.
    pub point_buffers: HashMap<String, PointBuffer>,
    /// An annotation layer's batches, one per colour it draws in.
    ///
    /// Separate from `point_buffers` because an annotation layer has *several*
    /// of each primitive — colouring by class splits it — where an object layer
    /// has exactly one point batch and no lines.
    pub annot_buffers: HashMap<String, Vec<AnnotBuffer>>,
    /// The box being dragged out right now, in world pixels, drawn over
    /// everything so the drag has something to follow.
    pub draft: Option<Drawn>,
    /// The edit in progress on an existing annotation, if any.
    pub editing: Option<Editing>,
    /// The vertices placed so far by a click-by-click tool, plus where the
    /// pointer is now, so the shape being built has a rubber band to follow.
    pub pending: Vec<(f32, f32)>,
    pub cursor: Option<(f32, f32)>,
    /// The world every layer is drawn in — the reference layer's
    /// full-resolution size. Kept here as well as in props so a mouse event,
    /// which has no access to props, can invert the camera transform.
    pub world_size: (f32, f32),
    pub camera: Camera2d,
}

impl ViewerCanvasState {
    /// The levels this layer has grid info for, coarsest first.
    fn levels_of(&self, layer: &str) -> Vec<usize> {
        let mut levels: Vec<usize> = self
            .level_info
            .keys()
            .filter(|(id, _)| id == layer)
            .map(|(_, level)| *level)
            .collect();
        levels.sort_unstable_by(|a, b| b.cmp(a));
        levels
    }
}

/// Props for the ViewerCanvas component.
#[derive(Properties, PartialEq)]
pub struct ViewerCanvasProps {
    /// Layers in draw order, bottom first.
    pub layers: Vec<LayerRenderInfo>,
    /// The coordinate space every layer is drawn in: the reference layer's
    /// full-resolution `(width, height)`.
    pub world_size: (f32, f32),
    #[prop_or_default]
    pub on_canvas_ready: Callback<Rc<RefCell<Option<ViewerCanvasState>>>>,
    #[prop_or_default]
    pub on_camera_changed: Callback<(f32, f32, f32, f32, f32)>, // (pan_x, pan_y, zoom, canvas_w, canvas_h)
    /// A click that was not a drag, in world pixel coordinates.
    #[prop_or_default]
    pub on_pick: Callback<(f32, f32)>,
    /// What a drag on the canvas does.
    #[prop_or_default]
    pub tool: Tool,
    /// A shape the user finished drawing.
    #[prop_or_default]
    pub on_draw: Callback<Drawn>,
    /// The selected annotation, so a drag can grab its handles.
    #[prop_or_default]
    pub editable: Option<Editable>,
    /// An edit the user finished.
    #[prop_or_default]
    pub on_edit: Callback<Editing>,
    /// The stroke width the shape being drawn *right now* will be stored with,
    /// or `None` if it will have none.
    ///
    /// Resolved by the app rather than worked out here, because it depends on
    /// the target layer's setting as well as on the tool, and the canvas knows
    /// nothing about layers' annotation state. Drawing the draft without it
    /// meant the band appeared on mouse-up: the shape you were about to get was
    /// not the shape you could see, and the width is the whole claim a scribble
    /// makes.
    #[prop_or_default]
    pub draft_stroke_width: Option<f64>,
}

/// WebGL2 canvas component handling rendering, pan, and zoom.
pub struct ViewerCanvas {
    canvas_ref: NodeRef,
    state: Rc<RefCell<Option<ViewerCanvasState>>>,
    _resize_closure: Option<Closure<dyn Fn()>>,
}

pub enum ViewerMsg {
    Init,
    MouseDown(MouseEvent),
    MouseMove(MouseEvent),
    MouseUp(MouseEvent),
    DoubleClick(MouseEvent),
    Wheel(WheelEvent),
    TouchStart(web_sys::TouchEvent),
    TouchMove(web_sys::TouchEvent),
    TouchEnd(web_sys::TouchEvent),
    Resize,
    Redraw,
}

impl Component for ViewerCanvas {
    type Message = ViewerMsg;
    type Properties = ViewerCanvasProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            canvas_ref: NodeRef::default(),
            state: Rc::new(RefCell::new(None)),
            _resize_closure: None,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            ctx.link().send_message(ViewerMsg::Init);
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            ViewerMsg::Init => self.on_init(ctx),
            ViewerMsg::MouseDown(e) => self.on_mouse_down(ctx, e),
            ViewerMsg::MouseMove(e) => self.on_mouse_move(ctx, e),
            ViewerMsg::MouseUp(e) => self.on_mouse_up(ctx, e),
            ViewerMsg::DoubleClick(e) => self.on_double_click(ctx, e),
            ViewerMsg::Wheel(e) => self.on_wheel(ctx, e),
            ViewerMsg::TouchStart(e) => self.on_touch_start(ctx, e),
            ViewerMsg::TouchMove(e) => self.on_touch_move(ctx, e),
            ViewerMsg::TouchEnd(e) => self.on_touch_end(ctx, e),
            ViewerMsg::Resize => self.on_resize(ctx),
            ViewerMsg::Redraw => self.on_redraw(ctx),
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let on_mousedown = ctx.link().callback(ViewerMsg::MouseDown);
        let on_mousemove = ctx.link().callback(ViewerMsg::MouseMove);
        let on_mouseup = ctx.link().callback(ViewerMsg::MouseUp);
        let on_dblclick = ctx.link().callback(ViewerMsg::DoubleClick);
        let on_wheel = ctx.link().callback(ViewerMsg::Wheel);
        let on_touchstart = ctx.link().callback(ViewerMsg::TouchStart);
        let on_touchmove = ctx.link().callback(ViewerMsg::TouchMove);
        let on_touchend = ctx.link().callback(ViewerMsg::TouchEnd);

        let class = if ctx.props().tool.draws() {
            "viewer-canvas drawing"
        } else {
            "viewer-canvas"
        };

        html! {
            <canvas
                ref={self.canvas_ref.clone()}
                class={class}
                onmousedown={on_mousedown}
                onmousemove={on_mousemove}
                onmouseup={on_mouseup}
                ondblclick={on_dblclick}
                onwheel={on_wheel}
                ontouchstart={on_touchstart}
                ontouchmove={on_touchmove}
                ontouchend={on_touchend}
            />
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, _old_props: &Self::Properties) -> bool {
        // Re-render when layer info changes
        self.redraw(ctx);
        true
    }
}

/// The world size the canvas last drew in.
pub(crate) fn camera_world(state: &ViewerCanvasState) -> (f32, f32) {
    state.world_size
}

/// A closed rectangle as a path of five points.
pub(crate) fn rect_path(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)]
}

/// An ellipse inscribed in a rectangle, as a closed path.
///
/// The same 64 segments the drawing tool will store, so the preview is the
/// shape — not an approximation of it that snaps on release.
pub fn ellipse_path(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (rx, ry) = ((x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0);
    let mut path: Vec<(f32, f32)> = (0..ELLIPSE_SEGMENTS)
        .map(|i| {
            let angle = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
            (cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect();
    if let Some(first) = path.first().copied() {
        path.push(first);
    }
    path
}

/// How finely an ellipse is polygonised.
///
/// GeoJSON has no ellipse, so one is stored as a polygon plus QuPath's
/// `isEllipse` flag — which is what lets QuPath rebuild the real thing from the
/// bounding box. 64 is smooth at the zooms this viewer works at and is what the
/// flag makes recoverable anyway.
pub const ELLIPSE_SEGMENTS: usize = 64;

/// Distance from a point to a line segment, in whatever units went in.
pub(crate) fn segment_distance(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let t = if length > 0.0 {
        (((x - a.0) * dx + (y - a.1) * dy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()
}

impl ViewerCanvas {}

#[cfg(test)]
mod tile_store_tests {
    use super::*;

    /// A texture handle is a JS object we cannot make on the host, so the
    /// accounting is tested through the one thing that decides it — the width
    /// and height — with the handle supplied by the caller in real use.
    fn key(level: usize, x: u32) -> TileKey {
        TileKey {
            layer: "L0".into(),
            level,
            tile_y: 0,
            tile_x: x,
            channel: 0,
        }
    }

    #[test]
    fn a_tile_costs_its_pixels_and_nothing_is_guessed() {
        // Both kinds are one 32-bit channel, so the arithmetic is exact rather
        // than an estimate — which is the whole reason a byte budget is
        // enforceable here at all.
        assert_eq!(bytes_for(256, 256), 256 * 256 * 4);
        assert_eq!(bytes_for(1024, 512), 1024 * 512 * 4);
    }

    #[test]
    fn the_budget_evicts_the_oldest_and_the_accounting_follows() {
        let mut store: TileStore<()> = TileStore::new(bytes_for(256, 256) * 2);
        assert_eq!(store.bytes(), 0);

        let _ = store.insert(key(0, 0), (), 256, 256);
        let _ = store.insert(key(0, 1), (), 256, 256);
        assert_eq!(store.len(), 2, "two fit exactly");
        assert_eq!(store.bytes(), bytes_for(256, 256) * 2);

        let freed = store.insert(key(0, 2), (), 256, 256).len();
        assert_eq!(freed, 1, "the third pushes one out");
        assert_eq!(store.len(), 2);
        assert_eq!(
            store.bytes(),
            bytes_for(256, 256) * 2,
            "held follows evictions"
        );
        assert!(!store.contains_key(&key(0, 0)), "the oldest went");
        assert!(store.contains_key(&key(0, 2)), "the newest stayed");
    }

    #[test]
    fn replacing_a_tile_does_not_double_count_it() {
        // The bug this guards: `held` growing on every re-upload of the same
        // tile, so a camera nudged back and forth reports a cache far larger
        // than it is and evicts things it did not need to.
        let mut store: TileStore<()> = TileStore::new(0);
        let _ = store.insert(key(0, 0), (), 128, 128);
        let freed = store.insert(key(0, 0), (), 128, 128).len();
        assert_eq!(
            freed, 1,
            "the replaced texture is handed back to be deleted"
        );
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.bytes(),
            bytes_for(128, 128),
            "counted once, not twice"
        );
    }

    #[test]
    fn a_zero_budget_means_no_budget_rather_than_no_tiles() {
        // `0` is the server cache's own convention for "uncapped", and a store
        // that evicted everything at zero would draw nothing at all.
        let mut store: TileStore<()> = TileStore::new(0);
        for x in 0..8 {
            let _ = store.insert(key(0, x), (), 512, 512);
        }
        assert_eq!(store.len(), 8);
    }

    #[test]
    fn retain_hands_back_everything_it_dropped() {
        let mut store: TileStore<()> = TileStore::new(0);
        for x in 0..4 {
            let _ = store.insert(key(0, x), (), 64, 64);
        }
        let dropped = store.retain(|key| key.tile_x < 2).len();
        assert_eq!(dropped, 2, "each dropped texture must reach delete_tiles");
        assert_eq!(store.len(), 2);
        assert_eq!(store.bytes(), bytes_for(64, 64) * 2);
    }
}

#[cfg(test)]
mod grab_tests {
    use super::{grab_at, grab_reach, is_worth_keeping, Drawn, EditKind, Editable, Handle, Tool};

    /// A square with vertices at the corners, as a polygon layer would offer it.
    fn square(size: f32) -> Editable {
        Editable {
            id: 1,
            bounds: (0.0, 0.0, size, size),
            paths: vec![vec![(0.0, 0.0), (size, 0.0), (size, size), (0.0, size)]],
            boxlike: false,
            puncta: false,
            locked: false,
            stroke_width: None,
        }
    }

    const NEAR: f32 = 4.0;

    // -- the five rules that live in this decision -------------------------

    /// A file that says "do not edit this" is one somebody locked on purpose.
    #[test]
    fn a_locked_shape_offers_no_handles_at_all() {
        let mut shape = square(100.0);
        shape.locked = true;
        for (x, y) in [(0.0, 0.0), (50.0, 50.0), (100.0, 100.0)] {
            assert_eq!(grab_at(&shape, x, y, false, NEAR), None, "at ({x}, {y})");
            assert_eq!(
                grab_at(&shape, x, y, true, NEAR),
                None,
                "shift at ({x}, {y})"
            );
        }
    }

    /// All four of a point's corners are the same coordinate, so a corner drag
    /// resizes a zero-size box into a zero-size box — which looks exactly like a
    /// broken drag.
    #[test]
    fn a_point_is_grabbed_by_its_body_never_by_a_corner() {
        let point = Editable {
            bounds: (30.0, 30.0, 30.0, 30.0),
            paths: vec![vec![(30.0, 30.0)]],
            puncta: true,
            ..square(0.0)
        };
        assert_eq!(
            grab_at(&point, 30.0, 30.0, false, NEAR),
            Some((Handle::Body, EditKind::Drag))
        );
        // Its own coordinate is also its corner, and it still answers Body.
        assert_eq!(
            grab_at(&point, 32.0, 32.0, false, NEAR),
            Some((Handle::Body, EditKind::Drag))
        );
        assert_eq!(grab_at(&point, 60.0, 60.0, false, NEAR), None, "well clear");
    }

    /// A vertex beats an edge beats the body: the vertex is the smallest target
    /// and the one a hand aims at.
    #[test]
    fn a_vertex_wins_over_the_body_it_sits_on() {
        let shape = square(100.0);
        assert_eq!(
            grab_at(&shape, 1.0, 1.0, false, NEAR),
            Some((Handle::Vertex(0, 0), EditKind::Drag)),
            "next to the first vertex"
        );
        assert_eq!(
            grab_at(&shape, 50.0, 50.0, false, NEAR),
            Some((Handle::Body, EditKind::Drag)),
            "in the middle, far from every vertex"
        );
    }

    /// Shift is the vertex modifier, so a shift-click that misses does nothing.
    /// Panning instead would send the picture sliding away from somebody who was
    /// aiming at a handle and was three pixels out.
    #[test]
    fn shift_deletes_a_vertex_inserts_on_an_edge_and_otherwise_does_nothing() {
        let shape = square(100.0);
        assert_eq!(
            grab_at(&shape, 0.0, 0.0, true, NEAR),
            Some((Handle::Vertex(0, 0), EditKind::DeleteVertex)),
            "on a vertex"
        );
        assert_eq!(
            grab_at(&shape, 50.0, 0.0, true, NEAR),
            Some((Handle::Vertex(0, 0), EditKind::InsertVertex)),
            "halfway along the first edge"
        );
        assert_eq!(
            grab_at(&shape, 50.0, 50.0, true, NEAR),
            None,
            "inside the shape but on nothing: a miss, not a pan"
        );
    }

    /// A rectangle or an ellipse is defined by its bounding box, so that is what
    /// it offers — which is what QuPath offers for them too.
    #[test]
    fn a_boxlike_shape_offers_corners_rather_than_vertices() {
        let mut shape = square(100.0);
        shape.boxlike = true;
        shape.paths.clear();
        assert_eq!(
            grab_at(&shape, 0.0, 0.0, false, NEAR),
            Some((Handle::Corner(true, true), EditKind::Drag))
        );
        assert_eq!(
            grab_at(&shape, 100.0, 100.0, false, NEAR),
            Some((Handle::Corner(false, false), EditKind::Drag))
        );
        assert_eq!(
            grab_at(&shape, 50.0, 50.0, false, NEAR),
            Some((Handle::Body, EditKind::Drag)),
            "the middle is still the body"
        );
    }

    /// A scribble is aimed at by the band on screen, not by the centreline
    /// inside it.
    #[test]
    fn a_scribble_is_grabbed_across_its_band() {
        let mut path = Editable {
            bounds: (0.0, 0.0, 0.0, 100.0),
            paths: vec![vec![(0.0, 0.0), (0.0, 100.0)]],
            ..square(0.0)
        };
        // 9 world px off the centreline: outside the hand's 4, and outside the
        // vertex bounds, which are a zero-width line.
        assert_eq!(
            grab_at(&path, 9.0, 50.0, true, NEAR),
            None,
            "a bare line covers no pixels, so this is a miss"
        );
        path.stroke_width = Some(24.0);
        assert_eq!(
            grab_at(&path, 9.0, 50.0, true, NEAR),
            Some((Handle::Vertex(0, 0), EditKind::InsertVertex)),
            "the same click, on a band 24 wide, lands on the shape"
        );
        assert_eq!(
            grab_at(&path, 24.0, 50.0, true, NEAR),
            None,
            "and the band is a bound, not an unbounded target"
        );
    }

    // -- what is worth storing ---------------------------------------------

    fn drawn(tool: Tool, points: &[(f32, f32)]) -> Drawn {
        Drawn {
            tool,
            points: points.to_vec(),
        }
    }

    /// A near-zero drag was a misfire, not a zero-size region — except from the
    /// point tool, where a click is exactly what it is.
    #[test]
    fn a_click_is_a_point_and_a_misfire_everywhere_else() {
        assert!(is_worth_keeping(&drawn(Tool::Point, &[(5.0, 5.0)])));
        assert!(!is_worth_keeping(&drawn(
            Tool::Box,
            &[(5.0, 5.0), (5.2, 5.1)]
        )));
        assert!(is_worth_keeping(&drawn(
            Tool::Box,
            &[(5.0, 5.0), (25.0, 25.0)]
        )));
    }

    /// A traced path says it went somewhere by its vertex count, not by its box:
    /// a long stroke drawn straight down has no width at all.
    #[test]
    fn a_trace_is_judged_by_its_vertices_not_by_its_bounding_box() {
        let straight_down = drawn(Tool::Line, &[(0.0, 0.0), (0.0, 50.0), (0.0, 99.0)]);
        assert!(
            is_worth_keeping(&straight_down),
            "zero width, and plainly a stroke somebody meant"
        );
        assert!(!is_worth_keeping(&drawn(
            Tool::Line,
            &[(0.0, 0.0), (0.0, 9.0)]
        )));
    }

    #[test]
    fn a_shape_with_no_points_is_not_a_shape() {
        assert!(!is_worth_keeping(&drawn(Tool::Box, &[])));
    }

    #[test]
    fn a_bare_line_is_grabbed_by_the_hands_tolerance() {
        assert_eq!(grab_reach(4.0, None), 4.0);
    }

    #[test]
    fn a_wide_scribble_is_grabbed_by_its_band() {
        // 24 world px wide, zoomed in far enough that the hand's tolerance is
        // 4: the band is the target, and it reaches 12 either side.
        assert_eq!(grab_reach(4.0, Some(24.0)), 12.0);
    }

    #[test]
    fn a_band_narrower_than_the_hand_does_not_make_the_shape_harder_to_hit() {
        // The floor matters: without it a 2px scribble would offer a 1px
        // target, where a bare line with no width at all offers 20.
        assert_eq!(grab_reach(20.0, Some(2.0)), 20.0);
    }

    #[test]
    fn a_width_of_zero_is_not_a_narrower_target_than_none() {
        assert_eq!(grab_reach(4.0, Some(0.0)), grab_reach(4.0, None));
    }
}
