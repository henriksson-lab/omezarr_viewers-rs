//! Where the three slices sit in the volume, drawn as a box cut by three planes.
//!
//! Self-contained, following [`crate::ortho_pane`]: it owns its canvas and its
//! GL context, so the main view's tile pipeline does not have to grow a third
//! dimension it was never shaped for. Unlike the ortho panes it fetches
//! **nothing** — a box and three planes are decided entirely by the volume's
//! extent and where the cuts are, both of which the app already knows.
//!
//! The camera is fixed. An orbit would be nicer to look at and would cost a
//! camera to store, input to handle, and a hit test that changes every frame;
//! the planes are draggable instead, which is the part that does work rather
//! than the part that looks like work.
//!
//! # Three decisions worth knowing before touching this
//!
//! **The projection is computed on the CPU and the shader is a pass-through.**
//! This is the only 3D in the codebase and there is no matrix stack anywhere;
//! rather than write one and then write it a second time in GLSL, [`CubeView`]
//! projects to clip space in Rust and the vertex shader hands the result
//! straight to `gl_Position`. The picture and the hit test therefore cannot
//! disagree — the failure `renderer.rs` has a test guarding against — and the
//! whole projection is testable on the host, where there is no GL context.
//! The geometry is 24 wireframe vertices and 72 triangle vertices, rebuilt per
//! draw; a matrix would save nothing measurable.
//!
//! **Everything is in *axis space*.** A point is `[a_x, a_y, a_z]`, each
//! component the signed offset from the box centre along that image axis, so a
//! larger `a_y` is further *down* the image, exactly as the app means it. The
//! y-flip that image coordinates need (screen y is up in clip space, image y is
//! down) is folded into the camera basis once, at construction, instead of
//! being applied at every call site and forgotten at one of them.
//!
//! **Translucency is ordered by construction, not by hoping.** See
//! [`sub_quads`].

use std::cmp::Ordering;

use js_sys::Float32Array;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{
    HtmlCanvasElement, MouseEvent, WebGl2RenderingContext as Gl, WebGlBuffer, WebGlProgram,
    WebGlVertexArrayObject,
};
use yew::prelude::*;

use crate::webgl::context::create_program;

/// Which way a plane faces, and the axis a drag on it scrubs.
pub const AXES: [&str; 3] = ["x", "y", "z"];

/// One colour per axis, so a plane names the panel it belongs to: the x plane
/// is the right-hand `(z, y)` pane, the y plane the bottom `(z, x)` pane, and
/// the z plane the main `(x, y)` view.
const PLANE_COLOR: [[f32; 3]; 3] = [
    [0.91, 0.27, 0.38], // x — the crosshair's red
    [0.30, 0.85, 0.45], // y — green
    [0.25, 0.70, 0.95], // z — blue
];

/// The wireframe, and the pane's own background. The wireframe is the one
/// thing here that writes depth, so it is drawn fully opaque and dimmed by its
/// colour instead of by its alpha — a translucent fragment that writes depth
/// claims to hide what is behind it and then does not.
const BOX_COLOR: [f32; 3] = [0.45, 0.51, 0.66];
const BACKGROUND: [f32; 4] = [0.051, 0.051, 0.102, 1.0];

/// A plane's fill, idle and while it is hovered or dragged.
const FILL_ALPHA: f32 = 0.20;
const FILL_ALPHA_ACTIVE: f32 = 0.42;

/// How far from a plane a click may land and still grab it, in screen pixels.
const GRAB_PX: f32 = 6.0;

/// The camera, as yaw about the vertical and pitch above the horizon.
///
/// Near-isometric but deliberately *not* isometric: at 45°/35.26° the three
/// axes project to the same length and a box reads as a hexagon with no way to
/// tell x from z. Skewing it keeps the two horizontal axes distinguishable.
/// The constraint the numbers satisfy is that no plane is edge-on — the camera
/// direction's component along each axis is 0.55, 0.44 and 0.71, none of them
/// near zero, so every plane shows a face and every axis has a screen direction
/// long enough to drag along. The viewpoint is front, above and to the right:
/// x runs right and slightly down, y straight down, z up and to the right, and
/// no two of those are within 45° of each other, which is what makes it
/// possible to tell by eye which plane a drag is moving.
const YAW: f32 = 38.0_f32.to_radians();
const PITCH: f32 = 26.0_f32.to_radians();

/// Below this the axis is too close to edge-on for a drag to mean anything:
/// the pointer's motion would be divided by a near-zero screen length and the
/// plane would leap from one face to the other.
const MIN_AXIS_SPAN_PX: f32 = 6.0;

#[derive(Properties, PartialEq)]
pub struct CubePaneProps {
    /// The volume in world pixels, `(x, y)`, and its depth in planes. The box is
    /// drawn to these proportions so an anisotropic volume looks anisotropic.
    pub world: (f32, f32),
    pub depth: u32,
    /// Where each plane cuts its axis, as a fraction in `0..=1`, in `AXES`
    /// order. A fraction rather than an index because the three axes are
    /// different units — two are world pixels and one is planes.
    pub cut: (f32, f32, f32),
    /// A plane was dragged: the axis, and where it now cuts.
    pub on_cut: Callback<(&'static str, f32)>,
}

// ---------------------------------------------------------------------------
// The projection
// ---------------------------------------------------------------------------

/// A fixed orthographic camera onto a box of known proportions.
///
/// Orthographic rather than perspective because the question the panel answers
/// is *where in the volume*, and under perspective the same slice at the front
/// and the back of the box would be two different sizes for no reason the
/// viewer can use.
#[derive(Clone, Debug)]
pub struct CubeView {
    /// Camera basis in **axis space** (see the module docs): screen right,
    /// screen up, and the direction from the box towards the camera.
    right: [f32; 3],
    up: [f32; 3],
    eye: [f32; 3],
    /// The box, normalised so its longest side is 1 — a uniform scale, so the
    /// proportions are the volume's own.
    extent: [f32; 3],
    half: [f32; 3],
    /// Screen pixels per unit of the normalised box.
    scale_px: f32,
    /// The canvas, in the same pixels the mouse events are in.
    size: (f32, f32),
    /// Half-depth of the box along the view direction, for the depth buffer.
    depth_range: f32,
}

impl CubeView {
    /// The pane's camera: [`YAW`] and [`PITCH`], fitted to the canvas.
    pub fn new(world: (f32, f32), depth: u32, canvas: (f32, f32)) -> Self {
        Self::with_camera(world, depth, canvas, YAW, PITCH)
    }

    /// The same, with the camera angles given — which is what lets the tests
    /// aim an axis straight at the camera and check that nothing divides by it.
    pub fn with_camera(
        world: (f32, f32),
        depth: u32,
        canvas: (f32, f32),
        yaw: f32,
        pitch: f32,
    ) -> Self {
        let sizes = [world.0.max(1e-6), world.1.max(1e-6), depth.max(1) as f32];
        let longest = sizes[0].max(sizes[1]).max(sizes[2]);
        let extent = [sizes[0] / longest, sizes[1] / longest, sizes[2] / longest];
        let half = [extent[0] * 0.5, extent[1] * 0.5, extent[2] * 0.5];

        // A right-handed y-up basis looking at the origin from `eye`, the
        // textbook look-at: right = up_world x eye, up = eye x right.
        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let eye = [cp * sy, sp, cp * cy];
        let right = [cy, 0.0, -sy];
        let up = [-sp * sy, cp, -sp * cy];
        // Into axis space, in one place and nowhere else. Two components flip:
        // image y runs *down* where the basis above assumes up, and image z
        // runs *away* from the viewer where the basis assumes the camera looks
        // along -Z. The z flip is what puts the camera in front of the first
        // slice rather than behind the last one, so the box is seen the way the
        // main panel sees it — plane 0 nearest, the stack receding.
        let flip = |v: [f32; 3]| [v[0], -v[1], -v[2]];
        let (right, up, eye) = (flip(right), flip(up), flip(eye));

        // Fit: the furthest corner decides the scale, so the box fills the
        // pane with a margin and no corner is clipped by a resize.
        let mut max_u: f32 = 1e-6;
        let mut max_v: f32 = 1e-6;
        let mut max_d: f32 = 1e-6;
        for corner in corners(half) {
            max_u = max_u.max(dot(corner, right).abs());
            max_v = max_v.max(dot(corner, up).abs());
            max_d = max_d.max(dot(corner, eye).abs());
        }
        let (w, h) = (canvas.0.max(1.0), canvas.1.max(1.0));
        let scale_px = 0.86 * (w / (2.0 * max_u)).min(h / (2.0 * max_v));

        Self {
            right,
            up,
            eye,
            extent,
            half,
            scale_px,
            size: (w, h),
            depth_range: max_d * 1.2,
        }
    }

    /// Where a cut fraction sits on its axis, in axis space.
    pub fn cut_coord(&self, axis: usize, fraction: f32) -> f32 {
        (fraction.clamp(0.0, 1.0) - 0.5) * self.extent[axis]
    }

    /// A point's position on the canvas, in the pixels mouse events use:
    /// origin top-left, y down.
    pub fn project(&self, a: [f32; 3]) -> (f32, f32) {
        let u = dot(a, self.right) * self.scale_px;
        let v = dot(a, self.up) * self.scale_px;
        (self.size.0 * 0.5 + u, self.size.1 * 0.5 - v)
    }

    /// How near the camera a point is; larger is nearer.
    pub fn depth_of(&self, a: [f32; 3]) -> f32 {
        dot(a, self.eye)
    }

    /// A point in clip space, `bias` pulling it towards the camera so an
    /// outline drawn on its own plane wins the depth test against it.
    fn clip(&self, a: [f32; 3], bias: f32) -> [f32; 3] {
        let (px, py) = self.project(a);
        let x = (px - self.size.0 * 0.5) / (self.size.0 * 0.5);
        let y = (self.size.1 * 0.5 - py) / (self.size.1 * 0.5);
        let z = (-self.depth_of(a) / self.depth_range - bias).clamp(-1.0, 1.0);
        [x, y, z]
    }

    /// Where the pointer's ray meets one plane, if it meets it inside the box.
    ///
    /// Orthographic, so the ray is the same direction everywhere and only its
    /// origin moves: unprojecting a pixel gives a point on the plane through
    /// the box centre, and the ray runs from there away from the camera.
    /// Returns `(t, point)`; `t` is signed, and smaller is nearer the camera.
    pub fn plane_hit(
        &self,
        axis: usize,
        fraction: f32,
        pointer: (f32, f32),
        margin_px: f32,
    ) -> Option<(f32, [f32; 3])> {
        let u = (pointer.0 - self.size.0 * 0.5) / self.scale_px;
        let v = (self.size.1 * 0.5 - pointer.1) / self.scale_px;
        let origin = [
            u * self.right[0] + v * self.up[0],
            u * self.right[1] + v * self.up[1],
            u * self.right[2] + v * self.up[2],
        ];
        let dir = [-self.eye[0], -self.eye[1], -self.eye[2]];

        // In axis space a plane's normal *is* a basis vector, so the dot
        // products are single components. A plane seen edge-on has no defined
        // intersection: refuse it rather than divide by ~0 and answer with a
        // point somewhere off in the next county.
        if dir[axis].abs() < 1e-3 {
            return None;
        }
        let t = (self.cut_coord(axis, fraction) - origin[axis]) / dir[axis];
        let point = [
            origin[0] + t * dir[0],
            origin[1] + t * dir[1],
            origin[2] + t * dir[2],
        ];
        let margin = margin_px / self.scale_px;
        for (other, at) in point.iter().enumerate() {
            if other != axis && at.abs() > self.half[other] + margin {
                return None;
            }
        }
        Some((t, point))
    }

    /// Which plane a click at `pointer` grabs: the nearest one the ray meets,
    /// which is the one whose colour is on top of that pixel.
    pub fn pick(&self, cut: [f32; 3], pointer: (f32, f32)) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (axis, fraction) in cut.iter().enumerate() {
            if let Some((t, _)) = self.plane_hit(axis, *fraction, pointer, GRAB_PX) {
                if best.is_none_or(|(bt, _)| t < bt) {
                    best = Some((t, axis));
                }
            }
        }
        best.map(|(_, axis)| axis)
    }

    /// The screen vector, in pixels, of one axis' full span: drag the pointer
    /// along it and the cut moves from one face of the box to the other.
    pub fn axis_span_px(&self, axis: usize) -> (f32, f32) {
        let e = self.extent[axis];
        (
            e * self.right[axis] * self.scale_px,
            -e * self.up[axis] * self.scale_px,
        )
    }

    /// Where a drag has moved a cut to: the pointer's motion since the press,
    /// projected onto that axis' screen direction, added to where the cut was.
    ///
    /// `None` when the axis points at the camera and has no usable screen
    /// direction — the division would be by ~0 and the plane would fly.
    pub fn drag_fraction(&self, axis: usize, start: f32, moved: (f32, f32)) -> Option<f32> {
        let (sx, sy) = self.axis_span_px(axis);
        let len2 = sx * sx + sy * sy;
        if len2 < MIN_AXIS_SPAN_PX * MIN_AXIS_SPAN_PX {
            return None;
        }
        Some((start + (moved.0 * sx + moved.1 * sy) / len2).clamp(0.0, 1.0))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// The box's eight corners, in axis space.
fn corners(half: [f32; 3]) -> [[f32; 3]; 8] {
    let mut out = [[0.0; 3]; 8];
    for (i, corner) in out.iter_mut().enumerate() {
        for (axis, c) in corner.iter_mut().enumerate() {
            *c = if i >> axis & 1 == 1 {
                half[axis]
            } else {
                -half[axis]
            };
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The geometry
// ---------------------------------------------------------------------------

/// One piece of one cutting plane.
#[derive(Clone, Debug)]
pub struct Quad {
    pub axis: usize,
    pub corners: [[f32; 3]; 4],
    /// The centroid's distance towards the camera; larger is nearer.
    pub depth: f32,
}

/// The three planes, cut into pieces that can be sorted, farthest first.
///
/// Three translucent quads that pass through each other cannot be drawn in any
/// order: whichever goes last is wrong along half of every intersection. The
/// usual dodges are to depth-sort and accept the seam, or to depth-peel.
/// Neither is needed here, because the planes are axis-aligned and mutually
/// perpendicular: splitting each one along the other two cuts gives four pieces
/// per plane, twelve in all, and **no two of them intersect**. Each lies wholly
/// within one of the eight octants the cuts carve the box into, so a sort by
/// centroid depth is not an approximation — it is the right order.
///
/// Drawn with the depth test on and depth *writes* off, so the pieces still get
/// occluded by the opaque wireframe but do not occlude each other; the sort
/// alone decides that.
pub fn sub_quads(view: &CubeView, cut: [f32; 3]) -> Vec<Quad> {
    let mut out = Vec::with_capacity(12);
    for axis in 0..3 {
        let j = (axis + 1) % 3;
        let k = (axis + 2) % 3;
        let at = view.cut_coord(axis, cut[axis]);
        let (jc, kc) = (view.cut_coord(j, cut[j]), view.cut_coord(k, cut[k]));
        let js = [(-view.half[j], jc), (jc, view.half[j])];
        let ks = [(-view.half[k], kc), (kc, view.half[k])];
        for (j0, j1) in js {
            for (k0, k1) in ks {
                let point = |jj: f32, kk: f32| {
                    let mut a = [0.0; 3];
                    a[axis] = at;
                    a[j] = jj;
                    a[k] = kk;
                    a
                };
                out.push(Quad {
                    axis,
                    corners: [point(j0, k0), point(j1, k0), point(j1, k1), point(j0, k1)],
                    depth: view.depth_of(point((j0 + j1) * 0.5, (k0 + k1) * 0.5)),
                });
            }
        }
    }
    out.sort_by(|a, b| a.depth.partial_cmp(&b.depth).unwrap_or(Ordering::Equal));
    out
}

/// The wireframe: twelve edges, two vertices each.
pub fn box_edges(view: &CubeView) -> Vec<[f32; 3]> {
    let h = view.half;
    let mut out = Vec::with_capacity(24);
    for axis in 0..3 {
        let j = (axis + 1) % 3;
        let k = (axis + 2) % 3;
        for sj in [-1.0_f32, 1.0] {
            for sk in [-1.0_f32, 1.0] {
                let mut a = [0.0; 3];
                a[j] = sj * h[j];
                a[k] = sk * h[k];
                a[axis] = -h[axis];
                let mut b = a;
                b[axis] = h[axis];
                out.push(a);
                out.push(b);
            }
        }
    }
    out
}

/// One plane's outline: four edges around the whole cross-section.
fn plane_outline(view: &CubeView, axis: usize, fraction: f32) -> Vec<[f32; 3]> {
    let j = (axis + 1) % 3;
    let k = (axis + 2) % 3;
    let at = view.cut_coord(axis, fraction);
    let corner = |sj: f32, sk: f32| {
        let mut a = [0.0; 3];
        a[axis] = at;
        a[j] = sj * view.half[j];
        a[k] = sk * view.half[k];
        a
    };
    let ring = [
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ];
    let mut out = Vec::with_capacity(8);
    for i in 0..4 {
        out.push(ring[i]);
        out.push(ring[(i + 1) % 4]);
    }
    out
}

// ---------------------------------------------------------------------------
// The GL side
// ---------------------------------------------------------------------------

/// Positions arrive already in clip space (see the module docs), so the vertex
/// shader has nothing to do but pass them on. The colour is per-vertex and
/// straight, not premultiplied — the fragment shader premultiplies, as every
/// other program in this codebase does.
const VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;

layout(location = 0) in vec3 a_clip;
layout(location = 1) in vec4 a_rgba;

out vec4 v_rgba;

void main() {
    gl_Position = vec4(a_clip, 1.0);
    v_rgba = a_rgba;
}
"#;

/// A flat translucent colour.
///
/// Premultiplied, like every other program here: the canvas is
/// `premultipliedAlpha` and the blend is `ONE, ONE_MINUS_SRC_ALPHA`, so a
/// colour channel above its own alpha is not a valid pixel and the compositor
/// is free to drop it. Translucent planes are exactly where that shows.
const FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;

in vec4 v_rgba;
out vec4 fragColor;

void main() {
    if (v_rgba.a <= 0.0) discard;
    fragColor = vec4(v_rgba.rgb * v_rgba.a, v_rgba.a);
}
"#;

/// Seven floats a vertex: clip position, then straight RGBA.
const STRIDE: i32 = 7 * 4;

struct Scene {
    gl: Gl,
    program: WebGlProgram,
    vao: WebGlVertexArrayObject,
    buffer: WebGlBuffer,
}

impl Scene {
    fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let gl = canvas
            .get_context("webgl2")
            .map_err(|_| "Failed to get webgl2 context")?
            .ok_or("No webgl2 support")?
            .dyn_into::<Gl>()
            .map_err(|_| "Failed to cast to WebGl2RenderingContext")?;
        let program = create_program(&gl, VERTEX_SHADER, FRAGMENT_SHADER)?;
        let vao = gl
            .create_vertex_array()
            .ok_or("Failed to create cube VAO")?;
        let buffer = gl.create_buffer().ok_or("Failed to create cube buffer")?;
        gl.bind_vertex_array(Some(&vao));
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&buffer));
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_with_i32(0, 3, Gl::FLOAT, false, STRIDE, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_with_i32(1, 4, Gl::FLOAT, false, STRIDE, 12);
        gl.bind_vertex_array(None);

        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::ONE, Gl::ONE_MINUS_SRC_ALPHA);
        gl.enable(Gl::DEPTH_TEST);
        // LEQUAL rather than LESS so an outline drawn on its own plane, with
        // only a small bias towards the camera, still lands.
        gl.depth_func(Gl::LEQUAL);
        Ok(Self {
            gl,
            program,
            vao,
            buffer,
        })
    }

    /// The whole panel, in three passes.
    fn draw(&self, view: &CubeView, cut: [f32; 3], active: Option<usize>) {
        let mut data: Vec<f32> = Vec::new();

        // Pass 1, opaque: the wireframe, which is the only thing that writes
        // depth. Everything translucent is then correctly hidden behind the
        // far edges and drawn over the near ones.
        for a in box_edges(view) {
            push(&mut data, view, a, BOX_COLOR, 1.0, 0.0);
        }
        let wire = data.len() / 7;

        // Pass 2: the plane pieces, farthest first. See `sub_quads`.
        for quad in sub_quads(view, cut) {
            let colour = PLANE_COLOR[quad.axis];
            let alpha = if active == Some(quad.axis) {
                FILL_ALPHA_ACTIVE
            } else {
                FILL_ALPHA
            };
            for i in [0, 1, 2, 0, 2, 3] {
                push(&mut data, view, quad.corners[i], colour, alpha, 0.0);
            }
        }
        let fills = data.len() / 7 - wire;

        // Pass 3: each plane's outline, biased towards the camera so it is not
        // in a depth fight with the fill it belongs to. This is what makes a
        // plane legible when it is nearly edge-on to something.
        for axis in 0..3 {
            let alpha = if active == Some(axis) { 1.0 } else { 0.7 };
            for a in plane_outline(view, axis, cut[axis]) {
                push(&mut data, view, a, PLANE_COLOR[axis], alpha, 2e-3);
            }
        }
        let outlines = data.len() / 7 - wire - fills;

        let gl = &self.gl;
        gl.use_program(Some(&self.program));
        gl.bind_vertex_array(Some(&self.vao));
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffer));
        // SAFETY: `Float32Array::view` borrows the wasm heap rather than
        // copying, and any allocation may move it. Make the view, hand it to
        // `buffer_data_*`, which copies, and drop it — nothing in between
        // allocates. The same rule as `Renderer::upload_vertex_buffer`.
        unsafe {
            let array = Float32Array::view(&data);
            gl.buffer_data_with_array_buffer_view(Gl::ARRAY_BUFFER, &array, Gl::DYNAMIC_DRAW);
        }

        // The depth mask has to be on for the clear itself: a masked depth
        // buffer is not cleared, and the second frame would then be drawn
        // against the first one's depths.
        gl.depth_mask(true);
        gl.clear_color(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2], BACKGROUND[3]);
        gl.clear(Gl::COLOR_BUFFER_BIT | Gl::DEPTH_BUFFER_BIT);

        gl.draw_arrays(Gl::LINES, 0, wire as i32);
        gl.depth_mask(false);
        gl.draw_arrays(Gl::TRIANGLES, wire as i32, fills as i32);
        gl.draw_arrays(Gl::LINES, (wire + fills) as i32, outlines as i32);
        gl.depth_mask(true);
        gl.bind_vertex_array(None);
    }
}

fn push(
    data: &mut Vec<f32>,
    view: &CubeView,
    a: [f32; 3],
    colour: [f32; 3],
    alpha: f32,
    bias: f32,
) {
    let clip = view.clip(a, bias);
    data.extend_from_slice(&[
        clip[0], clip[1], clip[2], colour[0], colour[1], colour[2], alpha,
    ]);
}

// ---------------------------------------------------------------------------
// The component
// ---------------------------------------------------------------------------

/// A plane being dragged: which one, where it was when the drag started, and
/// where the pointer was then.
///
/// The start fraction is remembered rather than read back from the props on
/// every move, because the app *quantises* what it sends back — a z fraction
/// becomes a slice index — and accumulating deltas against a quantised value
/// makes a slow drag creep.
struct Drag {
    axis: usize,
    start: f32,
    from: (f32, f32),
}

pub struct CubePane {
    canvas: NodeRef,
    scene: Option<Scene>,
    drag: Option<Drag>,
    hover: Option<usize>,
    /// Kept alive for as long as the listener is registered, and removed in
    /// `destroy`: dropping a `Closure` invalidates the JS function, and a
    /// listener still holding it throws on the next resize.
    resize: Option<Closure<dyn Fn()>>,
}

pub enum CubeMsg {
    Init,
    Down(MouseEvent),
    Move(MouseEvent),
    Up,
    Resize,
}

impl Component for CubePane {
    type Message = CubeMsg;
    type Properties = CubePaneProps;

    fn create(_ctx: &Context<Self>) -> Self {
        Self {
            canvas: NodeRef::default(),
            scene: None,
            drag: None,
            hover: None,
            resize: None,
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            ctx.link().send_message(CubeMsg::Init);
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            CubeMsg::Init => {
                let Some(canvas) = self.canvas.cast::<HtmlCanvasElement>() else {
                    log::warn!("cube pane: no canvas");
                    return false;
                };
                match Scene::new(&canvas) {
                    Ok(scene) => self.scene = Some(scene),
                    Err(e) => log::error!("cube pane init: {}", e),
                }
                // `add_event_listener` rather than `set_onresize`: the main
                // canvas owns the `onresize` *property*, and assigning it here
                // would silently replace its listener with this one.
                let link = ctx.link().clone();
                let closure = Closure::wrap(Box::new(move || {
                    link.send_message(CubeMsg::Resize);
                }) as Box<dyn Fn()>);
                if let Some(window) = web_sys::window() {
                    let _ = window.add_event_listener_with_callback(
                        "resize",
                        closure.as_ref().unchecked_ref(),
                    );
                    self.resize = Some(closure);
                }
                self.draw(ctx);
                false
            }
            CubeMsg::Resize | CubeMsg::Up => {
                let was = self.drag.take().is_some();
                self.draw(ctx);
                was
            }
            CubeMsg::Down(event) => {
                let Some(view) = self.camera(ctx) else {
                    return false;
                };
                let pointer = self.pointer(&event);
                let cut = cut_of(ctx);
                // The drag is taken here or not at all: a `mousemove` cannot
                // tell afterwards whether it was meant for a plane, which is
                // the rule the drawing tools in the main canvas learned.
                match view.pick(cut, pointer) {
                    Some(axis) => {
                        self.drag = Some(Drag {
                            axis,
                            start: cut[axis],
                            from: pointer,
                        });
                        self.hover = Some(axis);
                        self.draw(ctx);
                        true
                    }
                    None => false,
                }
            }
            CubeMsg::Move(event) => {
                let Some(view) = self.camera(ctx) else {
                    return false;
                };
                let pointer = self.pointer(&event);
                if let Some(drag) = &self.drag {
                    let moved = (pointer.0 - drag.from.0, pointer.1 - drag.from.1);
                    if let Some(at) = view.drag_fraction(drag.axis, drag.start, moved) {
                        ctx.props().on_cut.emit((AXES[drag.axis], at));
                    }
                    return false;
                }
                let hover = view.pick(cut_of(ctx), pointer);
                if hover != self.hover {
                    self.hover = hover;
                    self.draw(ctx);
                    return true;
                }
                false
            }
        }
    }

    fn destroy(&mut self, _ctx: &Context<Self>) {
        if let (Some(window), Some(closure)) = (web_sys::window(), self.resize.take()) {
            let _ = window
                .remove_event_listener_with_callback("resize", closure.as_ref().unchecked_ref());
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, _old: &Self::Properties) -> bool {
        self.draw(ctx);
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let cursor = match (&self.drag, self.hover) {
            (Some(_), _) => "grabbing",
            (None, Some(_)) => "grab",
            _ => "default",
        };
        let cut = cut_of(ctx);
        html! {
            <div class="cube-pane">
                <canvas
                    class="cube-canvas"
                    style={format!("cursor: {}", cursor)}
                    ref={self.canvas.clone()}
                    onmousedown={ctx.link().callback(CubeMsg::Down)}
                    onmousemove={ctx.link().callback(CubeMsg::Move)}
                    onmouseup={ctx.link().callback(|_| CubeMsg::Up)}
                    onmouseleave={ctx.link().callback(|_| CubeMsg::Up)}
                />
                <div style="position: absolute; top: 4px; left: 6px; font-size: 10px;
                            line-height: 1.4; pointer-events: none; font-family: monospace;">
                    { for (0..3).map(|axis| {
                        let c = PLANE_COLOR[axis];
                        let colour = format!(
                            "color: rgb({}, {}, {})",
                            (c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8,
                        );
                        html! {
                            <div style={colour}>
                                { format!("{} {:.2}", AXES[axis], cut[axis]) }
                            </div>
                        }
                    }) }
                </div>
            </div>
        }
    }
}

impl CubePane {
    /// The camera for the canvas as it is *now*, which is also what makes the
    /// pane resizable: the canvas is a grid cell and the browser decides its
    /// size, so the backing store is matched to the element on every draw
    /// rather than fixed once at startup.
    fn camera(&self, ctx: &Context<Self>) -> Option<CubeView> {
        let canvas = self.canvas.cast::<HtmlCanvasElement>()?;
        let rect = canvas.get_bounding_client_rect();
        let (w, h) = (rect.width().max(1.0) as u32, rect.height().max(1.0) as u32);
        if canvas.width() != w {
            canvas.set_width(w);
        }
        if canvas.height() != h {
            canvas.set_height(h);
        }
        let props = ctx.props();
        Some(CubeView::new(
            props.world,
            props.depth,
            (w as f32, h as f32),
        ))
    }

    /// The pointer in canvas pixels. The backing store is kept the same size as
    /// the element, so these are the same pixels the projection works in.
    fn pointer(&self, event: &MouseEvent) -> (f32, f32) {
        let Some(canvas) = self.canvas.cast::<HtmlCanvasElement>() else {
            return (0.0, 0.0);
        };
        let rect = canvas.get_bounding_client_rect();
        (
            event.client_x() as f32 - rect.left() as f32,
            event.client_y() as f32 - rect.top() as f32,
        )
    }

    fn draw(&self, ctx: &Context<Self>) {
        let Some(view) = self.camera(ctx) else {
            return;
        };
        let Some(scene) = self.scene.as_ref() else {
            return;
        };
        scene
            .gl
            .viewport(0, 0, view.size.0 as i32, view.size.1 as i32);
        let active = self.drag.as_ref().map(|d| d.axis).or(self.hover);
        scene.draw(&view, cut_of(ctx), active);
    }
}

fn cut_of(ctx: &Context<CubePane>) -> [f32; 3] {
    let c = ctx.props().cut;
    [
        c.0.clamp(0.0, 1.0),
        c.1.clamp(0.0, 1.0),
        c.2.clamp(0.0, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> CubeView {
        CubeView::new((512.0, 512.0), 512, (400.0, 300.0))
    }

    #[test]
    fn the_box_keeps_the_volumes_proportions() {
        // A slab is a slab: 512 x 256 x 8 is drawn 64 times wider than deep,
        // not normalised into a cube.
        let v = CubeView::new((512.0, 256.0), 8, (400.0, 300.0));
        let e = v.extent;
        assert!((e[0] - 1.0).abs() < 1e-6, "{e:?}");
        assert!((e[1] - 0.5).abs() < 1e-6, "{e:?}");
        assert!((e[2] - 8.0 / 512.0).abs() < 1e-6, "{e:?}");
    }

    #[test]
    fn a_volume_with_no_depth_is_still_a_box() {
        // `depth` is a count of planes and a 2D image reports 0 or 1 of them;
        // the projection must not divide by it or produce NaN.
        for depth in [0, 1] {
            let v = CubeView::new((512.0, 512.0), depth, (400.0, 300.0));
            let (x, y) = v.project([0.0, 0.0, 0.0]);
            assert!(x.is_finite() && y.is_finite(), "depth {depth}");
        }
    }

    #[test]
    fn the_box_is_centred_and_fits_the_canvas() {
        let v = view();
        let (x, y) = v.project([0.0, 0.0, 0.0]);
        assert!(
            (x - 200.0).abs() < 1e-4 && (y - 150.0).abs() < 1e-4,
            "{x} {y}"
        );
        for corner in corners([0.5, 0.5, 0.5]) {
            let (x, y) = v.project(corner);
            assert!(
                (0.0..=400.0).contains(&x) && (0.0..=300.0).contains(&y),
                "corner {corner:?} projects off the pane at {x}, {y}"
            );
        }
    }

    #[test]
    fn no_plane_is_edge_on_to_the_fixed_camera() {
        // The camera is chosen, not derived, so this is the assertion that the
        // choice is still a usable one: every plane shows a face and every
        // axis has a screen direction long enough to drag along.
        let v = view();
        for axis in 0..3 {
            assert!(
                v.plane_hit(axis, 0.5, (200.0, 150.0), 0.0).is_some(),
                "axis {axis} is edge-on"
            );
            let (sx, sy) = v.axis_span_px(axis);
            assert!(
                (sx * sx + sy * sy).sqrt() > 40.0,
                "axis {axis} projects to {sx}, {sy} — too short to drag"
            );
        }
    }

    #[test]
    fn the_camera_is_in_front_of_the_first_slice_and_the_stack_recedes() {
        // The main panel looks *through* the stack from plane 0; if this camera
        // sat on the other side the box would show the same volume mirrored,
        // and the z plane would slide the wrong way for everybody reading both
        // panels at once.
        let v = view();
        let near = v.depth_of([0.0, 0.0, -v.half[2]]);
        let far = v.depth_of([0.0, 0.0, v.half[2]]);
        assert!(near > far, "plane 0 is behind the last plane: {near} {far}");
    }

    #[test]
    fn the_three_axes_point_in_three_tellable_directions() {
        // The drag maps pointer motion onto an axis' screen direction, so two
        // axes lying along the same screen line would be two planes fighting
        // over the same gesture.
        let v = view();
        let angle = |axis| {
            let (x, y) = v.axis_span_px(axis);
            y.atan2(x).to_degrees()
        };
        for (a, b) in [(0, 1), (1, 2), (0, 2)] {
            let mut apart = (angle(a) - angle(b)).abs();
            if apart > 180.0 {
                apart = 360.0 - apart;
            }
            assert!(apart > 45.0, "axes {a} and {b} are {apart} degrees apart");
        }
    }

    #[test]
    fn a_click_on_a_plane_lands_back_on_the_point_it_was_aimed_at() {
        let v = view();
        // A point on the x plane, well away from the centre so it is not the
        // one point all three planes share.
        let cut = 0.25;
        let point = [v.cut_coord(0, cut), 0.2, -0.15];
        let pointer = v.project(point);
        let (_, hit) = v.plane_hit(0, cut, pointer, 0.0).expect("no hit");
        for axis in 0..3 {
            assert!(
                (hit[axis] - point[axis]).abs() < 1e-4,
                "{hit:?} != {point:?}"
            );
        }
    }

    #[test]
    fn a_click_outside_the_box_grabs_nothing() {
        let v = view();
        assert_eq!(v.pick([0.5, 0.5, 0.5], (2.0, 2.0)), None);
        assert_eq!(v.pick([0.5, 0.5, 0.5], (398.0, 298.0)), None);
    }

    #[test]
    fn a_click_grabs_the_plane_nearest_the_camera() {
        let v = view();
        let cut = [0.3, 0.6, 0.45];
        // Every pixel over the box: whatever `pick` answers must be a plane
        // the ray actually meets, and no plane it meets may be nearer.
        let mut grabbed = 0;
        for px in (10..390).step_by(7) {
            for py in (10..290).step_by(7) {
                let pointer = (px as f32, py as f32);
                let hits: Vec<(usize, f32)> = (0..3)
                    .filter_map(|axis| {
                        v.plane_hit(axis, cut[axis], pointer, GRAB_PX)
                            .map(|(t, _)| (axis, t))
                    })
                    .collect();
                let nearest = hits
                    .iter()
                    .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                    .map(|(axis, _)| *axis);
                assert_eq!(v.pick(cut, pointer), nearest, "at {pointer:?}");
                grabbed += usize::from(nearest.is_some());
            }
        }
        assert!(grabbed > 100, "only {grabbed} pixels grab anything");
    }

    #[test]
    fn dragging_the_length_of_an_axis_moves_its_cut_from_end_to_end() {
        let v = view();
        for axis in 0..3 {
            let span = v.axis_span_px(axis);
            assert_eq!(v.drag_fraction(axis, 0.0, (0.0, 0.0)), Some(0.0));
            let half = v.drag_fraction(axis, 0.0, (span.0 * 0.5, span.1 * 0.5));
            assert!((half.unwrap() - 0.5).abs() < 1e-5, "axis {axis}: {half:?}");
            // Sideways to the axis is not motion along it.
            let across = v.drag_fraction(axis, 0.5, (-span.1, span.0));
            assert!((across.unwrap() - 0.5).abs() < 1e-5, "axis {axis}");
        }
    }

    #[test]
    fn a_drag_past_the_end_of_the_box_stops_at_the_end() {
        let v = view();
        let (sx, sy) = v.axis_span_px(0);
        assert_eq!(v.drag_fraction(0, 0.5, (sx * 4.0, sy * 4.0)), Some(1.0));
        assert_eq!(v.drag_fraction(0, 0.5, (-sx * 4.0, -sy * 4.0)), Some(0.0));
    }

    #[test]
    fn an_axis_pointing_at_the_camera_refuses_the_drag_instead_of_dividing_by_it() {
        // Looking straight down z: the z axis has no screen direction at all,
        // so a pixel of pointer motion would otherwise be worth an unbounded
        // number of slices. And the y plane is exactly edge-on, so it cannot
        // be picked either.
        let v = CubeView::with_camera((512.0, 512.0), 512, (400.0, 300.0), 0.0, 0.0);
        assert_eq!(v.drag_fraction(2, 0.5, (30.0, 30.0)), None);
        assert_eq!(v.plane_hit(1, 0.5, (200.0, 150.0), 0.0), None);
        // The axes across the screen still work.
        assert!(v.drag_fraction(0, 0.5, (10.0, 0.0)).is_some());
    }

    #[test]
    fn the_planes_are_cut_into_pieces_that_do_not_intersect_and_are_sorted() {
        let v = view();
        let cut = [0.3, 0.6, 0.45];
        let quads = sub_quads(&v, cut);
        assert_eq!(quads.len(), 12);
        // Farthest first, so the nearer piece is drawn over the further one.
        for pair in quads.windows(2) {
            assert!(pair[0].depth <= pair[1].depth);
        }
        // Each piece lies wholly on one side of the other two cuts — which is
        // what makes a centroid sort exact rather than a guess.
        for quad in &quads {
            for axis in 0..3 {
                if axis == quad.axis {
                    continue;
                }
                let at = v.cut_coord(axis, cut[axis]);
                let above = quad.corners.iter().filter(|c| c[axis] >= at - 1e-6).count();
                let below = quad.corners.iter().filter(|c| c[axis] <= at + 1e-6).count();
                assert!(above == 4 || below == 4, "piece straddles axis {axis}");
            }
        }
    }

    #[test]
    fn the_wireframe_is_twelve_edges() {
        assert_eq!(box_edges(&view()).len(), 24);
        assert_eq!(plane_outline(&view(), 0, 0.5).len(), 8);
    }
}
