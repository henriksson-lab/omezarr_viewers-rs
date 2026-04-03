use js_sys::Float32Array;
use web_sys::{WebGl2RenderingContext, WebGlTexture};

use super::context::GlContext;

/// A WebGL texture uploaded for a single tile, with its pixel dimensions.
pub struct TileTexture {
    pub texture: WebGlTexture,
    pub width: u32,
    pub height: u32,
}

/// WebGL2 renderer that uploads tile textures and draws multi-channel quads.
pub struct Renderer {
    ctx: GlContext,
    quad_vao: web_sys::WebGlVertexArrayObject,
}

impl Renderer {
    /// Create a renderer with a unit quad VAO for tile drawing.
    pub fn new(ctx: GlContext) -> Result<Self, String> {
        let gl = &ctx.gl;

        // Create a unit quad VAO (0,0)-(1,1) with texcoords
        let vao = gl.create_vertex_array().ok_or("Failed to create VAO")?;
        gl.bind_vertex_array(Some(&vao));

        // Positions and texcoords interleaved: (x, y, u, v)
        let vertices: [f32; 24] = [
            // triangle 1
            0.0, 0.0, 0.0, 0.0, // bottom-left
            1.0, 0.0, 1.0, 0.0, // bottom-right
            0.0, 1.0, 0.0, 1.0, // top-left
            // triangle 2
            1.0, 0.0, 1.0, 0.0, // bottom-right
            1.0, 1.0, 1.0, 1.0, // top-right
            0.0, 1.0, 0.0, 1.0, // top-left
        ];

        let buffer = gl.create_buffer().ok_or("Failed to create buffer")?;
        gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(&buffer));
        unsafe {
            let array = Float32Array::view(&vertices);
            gl.buffer_data_with_array_buffer_view(
                WebGl2RenderingContext::ARRAY_BUFFER,
                &array,
                WebGl2RenderingContext::STATIC_DRAW,
            );
        }

        let pos_loc = gl.get_attrib_location(&ctx.program, "a_position") as u32;
        let tex_loc = gl.get_attrib_location(&ctx.program, "a_texcoord") as u32;

        gl.enable_vertex_attrib_array(pos_loc);
        gl.vertex_attrib_pointer_with_i32(pos_loc, 2, WebGl2RenderingContext::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(tex_loc);
        gl.vertex_attrib_pointer_with_i32(tex_loc, 2, WebGl2RenderingContext::FLOAT, false, 16, 8);

        gl.bind_vertex_array(None);

        Ok(Self {
            ctx,
            quad_vao: vao,
        })
    }

    /// Upload float32 pixel data as an R32F texture.
    pub fn upload_tile(&self, width: u32, height: u32, data: &[f32]) -> Result<TileTexture, String> {
        let gl = &self.ctx.gl;

        let texture = gl.create_texture().ok_or("Failed to create texture")?;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));

        // Set texture parameters
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MIN_FILTER,
            WebGl2RenderingContext::LINEAR as i32,
        );
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MAG_FILTER,
            WebGl2RenderingContext::LINEAR as i32,
        );
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_S,
            WebGl2RenderingContext::CLAMP_TO_EDGE as i32,
        );
        gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_T,
            WebGl2RenderingContext::CLAMP_TO_EDGE as i32,
        );

        // Upload float32 data as R32F texture
        unsafe {
            let array = Float32Array::view(data);
            gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_array_buffer_view(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                WebGl2RenderingContext::R32F as i32,
                width as i32,
                height as i32,
                0,
                WebGl2RenderingContext::RED,
                WebGl2RenderingContext::FLOAT,
                Some(&array),
            )
            .map_err(|e| format!("texImage2D: {:?}", e))?;
        }

        Ok(TileTexture {
            texture,
            width,
            height,
        })
    }

    /// Draw a single tile with multi-channel blending.
    /// `channel_textures` is a vec of (texture, color, contrast_min, contrast_max, opacity).
    pub fn draw_tile(
        &self,
        channel_textures: &[(
            &TileTexture,
            [f32; 3],
            f32,
            f32,
            f32,
        )],
        tile_offset: (f32, f32),
        tile_size: (f32, f32),
        image_size: (f32, f32),
        canvas_size: (f32, f32),
        pan: (f32, f32),
        zoom: f32,
        dtype_max: f32,
    ) {
        let gl = &self.ctx.gl;
        let program = &self.ctx.program;

        gl.use_program(Some(program));
        gl.bind_vertex_array(Some(&self.quad_vao));

        // Set common uniforms
        let set_uniform_2f = |name: &str, x: f32, y: f32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform2f(Some(&loc), x, y);
            }
        };
        let set_uniform_1f = |name: &str, v: f32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform1f(Some(&loc), v);
            }
        };

        set_uniform_2f("u_pan", pan.0, pan.1);
        set_uniform_1f("u_zoom", zoom);
        set_uniform_2f("u_canvas_size", canvas_size.0, canvas_size.1);
        set_uniform_2f("u_tile_offset", tile_offset.0, tile_offset.1);
        set_uniform_2f("u_tile_size", tile_size.0, tile_size.1);
        set_uniform_2f("u_image_size", image_size.0, image_size.1);
        set_uniform_1f("u_dtype_max", dtype_max);

        let num_channels = channel_textures.len().min(6) as i32;
        if let Some(loc) = gl.get_uniform_location(program, "u_num_channels") {
            gl.uniform1i(Some(&loc), num_channels);
        }

        // Bind channel textures and set per-channel uniforms
        for (i, (tex, color, cmin, cmax, opacity)) in channel_textures.iter().enumerate() {
            if i >= 6 {
                break;
            }

            // Bind texture to unit i
            gl.active_texture(WebGl2RenderingContext::TEXTURE0 + i as u32);
            gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&tex.texture));
            let sampler_name = format!("u_channel{}", i);
            if let Some(loc) = gl.get_uniform_location(program, &sampler_name) {
                gl.uniform1i(Some(&loc), i as i32);
            }

            // Color
            let color_name = format!("u_color[{}]", i);
            if let Some(loc) = gl.get_uniform_location(program, &color_name) {
                gl.uniform3f(Some(&loc), color[0], color[1], color[2]);
            }

            // Contrast
            let contrast_name = format!("u_contrast[{}]", i);
            if let Some(loc) = gl.get_uniform_location(program, &contrast_name) {
                gl.uniform2f(Some(&loc), *cmin, *cmax);
            }

            // Opacity
            let opacity_name = format!("u_opacity[{}]", i);
            if let Some(loc) = gl.get_uniform_location(program, &opacity_name) {
                gl.uniform1f(Some(&loc), *opacity);
            }
        }

        gl.draw_arrays(WebGl2RenderingContext::TRIANGLES, 0, 6);
    }

    /// Clear the framebuffer to the background color.
    pub fn clear(&self) {
        let gl = &self.ctx.gl;
        gl.clear_color(0.1, 0.1, 0.12, 1.0);
        gl.clear(WebGl2RenderingContext::COLOR_BUFFER_BIT);
    }

    /// Update the GL viewport to match new canvas dimensions.
    pub fn resize(&self, width: u32, height: u32) {
        self.ctx.gl.viewport(0, 0, width as i32, height as i32);
    }
}
