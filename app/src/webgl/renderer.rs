use js_sys::{Float32Array, Uint32Array, Uint8Array};
use web_sys::{WebGl2RenderingContext, WebGlTexture};

use super::context::GlContext;

/// A WebGL texture uploaded for a single tile, with its pixel dimensions.
pub struct TileTexture {
    pub texture: WebGlTexture,
    pub width: u32,
    pub height: u32,
    /// Which program can read it. An `R32F` texture bound to a `usampler2D`
    /// (or the reverse) is undefined behaviour rather than a wrong picture, so
    /// the kind travels with the texture instead of with the call site.
    pub kind: TextureKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextureKind {
    /// `R32F`, linear filtering — intensity.
    Intensity,
    /// `R32UI`, nearest filtering — label ids.
    Labels,
}

/// How a layer's pixels meet what is already on the framebuffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Blend {
    /// Replace — the bottom image layer, which has nothing to preserve.
    Over,
    /// Add — every image layer above it, so a mask or a second stain lights up
    /// the picture instead of hiding it.
    Add,
}

/// A batch of object points on the GPU.
pub struct PointBuffer {
    vao: web_sys::WebGlVertexArrayObject,
    buffer: web_sys::WebGlBuffer,
    pub count: usize,
}

/// How an object layer is drawn.
#[derive(Clone, PartialEq, Debug)]
pub struct PointRenderInfo {
    pub color: [f32; 3],
    pub opacity: f32,
    /// Sprite diameter in screen pixels.
    pub size: f32,
    /// Colour by the value column rather than by `color`.
    pub color_by_value: bool,
    pub value_range: [f32; 2],
    /// Draw rings rather than discs, so the pixels underneath stay visible.
    pub hollow: bool,
    /// The world z the viewer is on, and how far a point may be from it before
    /// it fades out entirely. A slab of 0 disables the fade.
    pub z: f32,
    pub slab: f32,
    /// Row index of the selected object, or -1.
    pub selected_row: f32,
}

impl Default for PointRenderInfo {
    fn default() -> Self {
        Self {
            color: [1.0, 0.85, 0.2],
            opacity: 0.9,
            size: 9.0,
            color_by_value: false,
            value_range: [0.0, 1.0],
            hollow: false,
            z: 0.0,
            slab: 0.0,
            selected_row: -1.0,
        }
    }
}

/// How a label layer is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LabelRenderInfo {
    pub opacity: f32,
    pub outline: bool,
    /// The highlighted id; 0 means none.
    pub selected: u32,
    /// Show only the selected id.
    pub only_selected: bool,
}

impl Default for LabelRenderInfo {
    fn default() -> Self {
        Self {
            opacity: 0.6,
            outline: false,
            selected: 0,
            only_selected: false,
        }
    }
}

/// WebGL2 renderer that uploads tile textures and draws multi-channel quads.
pub struct Renderer {
    ctx: GlContext,
    quad_vao: web_sys::WebGlVertexArrayObject,
    /// One RGBA8 colour table per label layer, keyed by layer id.
    luts: std::collections::HashMap<String, (WebGlTexture, i32)>,
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

        // Attribute locations are pinned in the shader source, so the same VAO
        // feeds every program.
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_with_i32(0, 2, WebGl2RenderingContext::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_with_i32(1, 2, WebGl2RenderingContext::FLOAT, false, 16, 8);

        gl.bind_vertex_array(None);

        Ok(Self {
            ctx,
            quad_vao: vao,
            luts: std::collections::HashMap::new(),
        })
    }

    /// Upload float32 pixel data as an R32F texture.
    pub fn upload_tile(
        &self,
        width: u32,
        height: u32,
        data: &[f32],
    ) -> Result<TileTexture, String> {
        let gl = &self.ctx.gl;

        let texture = gl.create_texture().ok_or("Failed to create texture")?;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));
        self.set_filtering(WebGl2RenderingContext::LINEAR);

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
            kind: TextureKind::Intensity,
        })
    }

    /// Upload label ids as an R32UI texture.
    ///
    /// Nearest filtering is not a preference: an integer texture cannot be
    /// linearly filtered at all, and averaging two ids would invent a third.
    pub fn upload_label_tile(
        &self,
        width: u32,
        height: u32,
        data: &[u32],
    ) -> Result<TileTexture, String> {
        let gl = &self.ctx.gl;

        let texture = gl.create_texture().ok_or("Failed to create texture")?;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));
        self.set_filtering(WebGl2RenderingContext::NEAREST);

        unsafe {
            let array = Uint32Array::view(data);
            gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_array_buffer_view(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                WebGl2RenderingContext::R32UI as i32,
                width as i32,
                height as i32,
                0,
                WebGl2RenderingContext::RED_INTEGER,
                WebGl2RenderingContext::UNSIGNED_INT,
                Some(&array),
            )
            .map_err(|e| format!("texImage2D (labels): {:?}", e))?;
        }

        Ok(TileTexture {
            texture,
            width,
            height,
            kind: TextureKind::Labels,
        })
    }

    /// Install a colour table for a label layer: `rgba[id]`, four bytes per id.
    ///
    /// An id whose entry is fully transparent is treated as unnamed and falls
    /// back to the hash colouring, so a sparse table costs nothing to express.
    pub fn set_label_lut(&mut self, layer: &str, rgba: &[u8]) -> Result<(), String> {
        let gl = &self.ctx.gl;
        let width = (rgba.len() / 4) as i32;
        if width == 0 {
            self.luts.remove(layer);
            return Ok(());
        }
        let texture = gl.create_texture().ok_or("Failed to create LUT texture")?;
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));
        self.set_filtering(WebGl2RenderingContext::NEAREST);
        unsafe {
            let array = Uint8Array::view(rgba);
            gl.tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_array_buffer_view(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                WebGl2RenderingContext::RGBA8 as i32,
                width,
                1,
                0,
                WebGl2RenderingContext::RGBA,
                WebGl2RenderingContext::UNSIGNED_BYTE,
                Some(&array),
            )
            .map_err(|e| format!("texImage2D (lut): {:?}", e))?;
        }
        self.luts.insert(layer.to_string(), (texture, width));
        Ok(())
    }

    pub fn has_label_lut(&self, layer: &str) -> bool {
        self.luts.contains_key(layer)
    }

    fn set_filtering(&self, filter: u32) {
        let gl = &self.ctx.gl;
        for (name, value) in [
            (WebGl2RenderingContext::TEXTURE_MIN_FILTER, filter),
            (WebGl2RenderingContext::TEXTURE_MAG_FILTER, filter),
            (
                WebGl2RenderingContext::TEXTURE_WRAP_S,
                WebGl2RenderingContext::CLAMP_TO_EDGE,
            ),
            (
                WebGl2RenderingContext::TEXTURE_WRAP_T,
                WebGl2RenderingContext::CLAMP_TO_EDGE,
            ),
        ] {
            gl.tex_parameteri(WebGl2RenderingContext::TEXTURE_2D, name, value as i32);
        }
    }

    /// Where a tile sits and how the camera sees it — the uniforms every
    /// program shares.
    fn set_camera(&self, program: &web_sys::WebGlProgram, placement: &TilePlacement) {
        let gl = &self.ctx.gl;
        let set2 = |name: &str, x: f32, y: f32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform2f(Some(&loc), x, y);
            }
        };
        let set1 = |name: &str, v: f32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform1f(Some(&loc), v);
            }
        };
        set2("u_pan", placement.pan.0, placement.pan.1);
        set1("u_zoom", placement.zoom);
        set2(
            "u_canvas_size",
            placement.canvas_size.0,
            placement.canvas_size.1,
        );
        set2(
            "u_tile_offset",
            placement.tile_offset.0,
            placement.tile_offset.1,
        );
        set2("u_tile_size", placement.tile_size.0, placement.tile_size.1);
        set2(
            "u_image_size",
            placement.image_size.0,
            placement.image_size.1,
        );
    }

    /// Draw a single tile with multi-channel blending.
    /// `channel_textures` is a slice of (texture, color, contrast_min, contrast_max, opacity).
    pub fn draw_tile(
        &self,
        channel_textures: &[(&TileTexture, [f32; 3], f32, f32, f32)],
        placement: &TilePlacement,
        dtype_max: f32,
        blend: Blend,
    ) {
        let gl = &self.ctx.gl;
        let program = &self.ctx.program;

        self.set_blend(blend);
        gl.use_program(Some(program));
        gl.bind_vertex_array(Some(&self.quad_vao));
        self.set_camera(program, placement);

        if let Some(loc) = gl.get_uniform_location(program, "u_dtype_max") {
            gl.uniform1f(Some(&loc), dtype_max);
        }

        let num_channels = channel_textures.len().min(6) as i32;
        if let Some(loc) = gl.get_uniform_location(program, "u_num_channels") {
            gl.uniform1i(Some(&loc), num_channels);
        }

        for (i, (tex, color, cmin, cmax, opacity)) in channel_textures.iter().enumerate() {
            if i >= 6 {
                break;
            }
            gl.active_texture(WebGl2RenderingContext::TEXTURE0 + i as u32);
            gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&tex.texture));
            if let Some(loc) = gl.get_uniform_location(program, &format!("u_channel{}", i)) {
                gl.uniform1i(Some(&loc), i as i32);
            }
            if let Some(loc) = gl.get_uniform_location(program, &format!("u_color[{}]", i)) {
                gl.uniform3f(Some(&loc), color[0], color[1], color[2]);
            }
            if let Some(loc) = gl.get_uniform_location(program, &format!("u_contrast[{}]", i)) {
                gl.uniform2f(Some(&loc), *cmin, *cmax);
            }
            if let Some(loc) = gl.get_uniform_location(program, &format!("u_opacity[{}]", i)) {
                gl.uniform1f(Some(&loc), *opacity);
            }
        }

        gl.draw_arrays(WebGl2RenderingContext::TRIANGLES, 0, 6);
        self.set_blend(Blend::Over);
    }

    /// Set the blend equation for the next draw.
    ///
    /// Both modes are premultiplied — see `context.rs` for why nothing here may
    /// write un-premultiplied colour.
    fn set_blend(&self, blend: Blend) {
        let gl = &self.ctx.gl;
        match blend {
            Blend::Over => gl.blend_func(
                WebGl2RenderingContext::ONE,
                WebGl2RenderingContext::ONE_MINUS_SRC_ALPHA,
            ),
            Blend::Add => gl.blend_func(WebGl2RenderingContext::ONE, WebGl2RenderingContext::ONE),
        }
    }

    /// Draw one label tile over whatever is already on the framebuffer.
    pub fn draw_label_tile(
        &self,
        layer: &str,
        texture: &TileTexture,
        placement: &TilePlacement,
        info: &LabelRenderInfo,
    ) {
        if texture.kind != TextureKind::Labels {
            return;
        }
        let gl = &self.ctx.gl;
        let program = &self.ctx.label_program;

        gl.use_program(Some(program));
        gl.bind_vertex_array(Some(&self.quad_vao));
        self.set_camera(program, placement);

        gl.active_texture(WebGl2RenderingContext::TEXTURE0);
        gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture.texture));
        if let Some(loc) = gl.get_uniform_location(program, "u_labels") {
            gl.uniform1i(Some(&loc), 0);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_tex_size") {
            gl.uniform2f(Some(&loc), texture.width as f32, texture.height as f32);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_opacity") {
            gl.uniform1f(Some(&loc), info.opacity);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_outline") {
            gl.uniform1i(Some(&loc), i32::from(info.outline));
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_selected") {
            gl.uniform1ui(Some(&loc), info.selected);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_only_selected") {
            gl.uniform1i(
                Some(&loc),
                i32::from(info.only_selected && info.selected != 0),
            );
        }

        let lut_size = match self.luts.get(layer) {
            Some((lut, size)) => {
                gl.active_texture(WebGl2RenderingContext::TEXTURE1);
                gl.bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(lut));
                if let Some(loc) = gl.get_uniform_location(program, "u_lut") {
                    gl.uniform1i(Some(&loc), 1);
                }
                *size
            }
            None => 0,
        };
        if let Some(loc) = gl.get_uniform_location(program, "u_lut_size") {
            gl.uniform1i(Some(&loc), lut_size);
        }

        gl.draw_arrays(WebGl2RenderingContext::TRIANGLES, 0, 6);
    }

    /// Upload one batch of points.
    ///
    /// `data` is interleaved `(z, y, x, value, row)` per point — five floats,
    /// one buffer, one draw call for the whole layer.
    pub fn upload_points(&self, data: &[f32]) -> Result<PointBuffer, String> {
        let gl = &self.ctx.gl;
        let vao = gl
            .create_vertex_array()
            .ok_or("Failed to create point VAO")?;
        let buffer = gl.create_buffer().ok_or("Failed to create point buffer")?;
        gl.bind_vertex_array(Some(&vao));
        gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(&buffer));
        unsafe {
            let array = Float32Array::view(data);
            gl.buffer_data_with_array_buffer_view(
                WebGl2RenderingContext::ARRAY_BUFFER,
                &array,
                WebGl2RenderingContext::STATIC_DRAW,
            );
        }
        let stride = 5 * 4;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_with_i32(0, 3, WebGl2RenderingContext::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_with_i32(1, 1, WebGl2RenderingContext::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer_with_i32(2, 1, WebGl2RenderingContext::FLOAT, false, stride, 16);
        gl.bind_vertex_array(None);

        Ok(PointBuffer {
            vao,
            buffer,
            count: data.len() / 5,
        })
    }

    /// Release a point batch's GPU memory.
    pub fn delete_points(&self, points: &PointBuffer) {
        let gl = &self.ctx.gl;
        gl.delete_buffer(Some(&points.buffer));
        gl.delete_vertex_array(Some(&points.vao));
    }

    /// Draw one batch of object points.
    pub fn draw_points(
        &self,
        points: &PointBuffer,
        placement: &TilePlacement,
        info: &PointRenderInfo,
    ) {
        if points.count == 0 {
            return;
        }
        let gl = &self.ctx.gl;
        let program = &self.ctx.point_program;
        gl.use_program(Some(program));
        gl.bind_vertex_array(Some(&points.vao));
        self.set_camera(program, placement);

        let set1 = |name: &str, v: f32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform1f(Some(&loc), v);
            }
        };
        let set1i = |name: &str, v: i32| {
            if let Some(loc) = gl.get_uniform_location(program, name) {
                gl.uniform1i(Some(&loc), v);
            }
        };
        set1("u_point_size", info.size);
        set1("u_z", info.z);
        set1("u_slab", info.slab);
        set1("u_selected_row", info.selected_row);
        set1("u_opacity", info.opacity);
        set1i("u_color_by_value", i32::from(info.color_by_value));
        set1i("u_hollow", i32::from(info.hollow));
        if let Some(loc) = gl.get_uniform_location(program, "u_color") {
            gl.uniform3f(Some(&loc), info.color[0], info.color[1], info.color[2]);
        }
        if let Some(loc) = gl.get_uniform_location(program, "u_value_range") {
            gl.uniform2f(Some(&loc), info.value_range[0], info.value_range[1]);
        }

        gl.draw_arrays(WebGl2RenderingContext::POINTS, 0, points.count as i32);
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

/// Where one tile goes on screen: its place in image pixels, and the camera.
///
/// Bundled because every draw call needs all six of them and half of them are
/// `(f32, f32)` pairs that read identically at a call site.
#[derive(Clone, Copy, Debug)]
pub struct TilePlacement {
    pub tile_offset: (f32, f32),
    pub tile_size: (f32, f32),
    /// The world size this layer's pixels are expressed in — the reference
    /// layer's image size, so layers of different resolutions overlay.
    pub image_size: (f32, f32),
    pub canvas_size: (f32, f32),
    pub pan: (f32, f32),
    pub zoom: f32,
}
