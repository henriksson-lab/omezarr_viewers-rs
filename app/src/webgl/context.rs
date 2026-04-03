use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext, WebGlProgram, WebGlShader};

use super::shaders;

/// WebGL2 context wrapper holding the GL handle and compiled shader program.
pub struct GlContext {
    pub gl: WebGl2RenderingContext,
    pub program: WebGlProgram,
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

        let program = create_program(&gl, shaders::VERTEX_SHADER, shaders::FRAGMENT_SHADER)?;
        gl.use_program(Some(&program));

        // Enable blending — channel compositing happens in shader,
        // GL blend just needs latest draw to replace previous (alpha=1.0)
        gl.enable(WebGl2RenderingContext::BLEND);
        gl.blend_func(
            WebGl2RenderingContext::SRC_ALPHA,
            WebGl2RenderingContext::ONE_MINUS_SRC_ALPHA,
        );

        Ok(Self { gl, program })
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
        let log = gl
            .get_shader_info_log(&shader)
            .unwrap_or_default();
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
        let log = gl
            .get_program_info_log(&program)
            .unwrap_or_default();
        gl.delete_program(Some(&program));
        Err(format!("Program link error: {}", log))
    }
}
