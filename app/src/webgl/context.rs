use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext, WebGlProgram, WebGlShader};

use super::shaders;

/// WebGL2 context wrapper holding the GL handle and every compiled program.
///
/// One context, several programs: intensity compositing and label colouring
/// answer different questions about different textures, and a uniform-flagged
/// single program would pay for both on every fragment. Points and lines are
/// separate again because their primitives are.
pub struct GlContext {
    pub gl: WebGl2RenderingContext,
    /// Additive multi-channel intensity.
    pub program: WebGlProgram,
    /// Integer label ids.
    pub label_program: WebGlProgram,
    /// Object points, and annotation points.
    pub point_program: WebGlProgram,
    /// Annotation outlines.
    pub line_program: WebGlProgram,
    /// Filled annotation regions.
    pub fill_program: WebGlProgram,
}

impl GlContext {
    /// Create a WebGL2 context from a canvas element, compile shaders, and enable blending.
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let gl = canvas
            .get_context("webgl2")
            .map_err(|_| "Failed to get webgl2 context")?
            .ok_or("No webgl2 support")?
            .dyn_into::<WebGl2RenderingContext>()
            .map_err(|_| "Failed to cast to WebGl2RenderingContext")?;

        // Enable linear filtering for float textures (R32F)
        gl.get_extension("OES_texture_float_linear")
            .map_err(|_| "OES_texture_float_linear not available")?;

        let vertex = shaders::tile_vertex_shader();
        let program = create_program(&gl, &vertex, shaders::FRAGMENT_SHADER)?;
        let label_program = create_program(&gl, &vertex, shaders::LABEL_FRAGMENT_SHADER)?;
        let point_program = create_program(
            &gl,
            &shaders::point_vertex_shader(),
            shaders::POINT_FRAGMENT_SHADER,
        )?;
        let line_program = create_program(
            &gl,
            &shaders::line_vertex_shader(),
            shaders::LINE_FRAGMENT_SHADER,
        )?;
        let fill_program = create_program(
            &gl,
            &shaders::fill_vertex_shader(),
            shaders::FILL_FRAGMENT_SHADER,
        )?;
        gl.use_program(Some(&program));

        // Blending is `over` on **premultiplied** colour, which is what the
        // canvas itself stores (`premultipliedAlpha` defaults to true). Every
        // shader here multiplies its colour by its own alpha, so the source
        // factor is ONE rather than SRC_ALPHA.
        //
        // This is not a preference. With SRC_ALPHA and un-premultiplied output
        // the framebuffer ends up holding pixels whose colour exceeds their
        // alpha, which is not a valid premultiplied pixel; the compositor's
        // behaviour on those is undefined, and what it actually did was drop a
        // channel — a green label over a green image composited to no green at
        // all. Measured, not theorised.
        gl.enable(WebGl2RenderingContext::BLEND);
        gl.blend_func(
            WebGl2RenderingContext::ONE,
            WebGl2RenderingContext::ONE_MINUS_SRC_ALPHA,
        );

        Ok(Self {
            gl,
            program,
            label_program,
            point_program,
            line_program,
            fill_program,
        })
    }
}

/// Compile a GLSL shader source string.
fn compile_shader(
    gl: &WebGl2RenderingContext,
    shader_type: u32,
    source: &str,
) -> Result<WebGlShader, String> {
    let shader = gl
        .create_shader(shader_type)
        .ok_or("Failed to create shader")?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);

    if gl
        .get_shader_parameter(&shader, WebGl2RenderingContext::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        let log = gl.get_shader_info_log(&shader).unwrap_or_default();
        gl.delete_shader(Some(&shader));
        Err(format!("Shader compile error: {}", log))
    }
}

/// Link vertex and fragment shaders into a program.
fn create_program(
    gl: &WebGl2RenderingContext,
    vs_source: &str,
    fs_source: &str,
) -> Result<WebGlProgram, String> {
    let vs = compile_shader(gl, WebGl2RenderingContext::VERTEX_SHADER, vs_source)?;
    let fs = compile_shader(gl, WebGl2RenderingContext::FRAGMENT_SHADER, fs_source)?;

    let program = gl.create_program().ok_or("Failed to create program")?;
    gl.attach_shader(&program, &vs);
    gl.attach_shader(&program, &fs);
    gl.link_program(&program);

    if gl
        .get_program_parameter(&program, WebGl2RenderingContext::LINK_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(program)
    } else {
        let log = gl.get_program_info_log(&program).unwrap_or_default();
        gl.delete_program(Some(&program));
        Err(format!("Program link error: {}", log))
    }
}
