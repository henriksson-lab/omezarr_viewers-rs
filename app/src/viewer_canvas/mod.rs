use std::cell::RefCell;
use std::collections::HashMap;
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
}

/// One colour's worth of an annotation layer, on the GPU.
pub struct AnnotBuffer {
    pub color: [f32; 3],
    pub points: PointBuffer,
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
pub struct ViewerCanvasState {
    pub renderer: Renderer,
    pub tile_cache: HashMap<TileKey, TileTexture>,
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
