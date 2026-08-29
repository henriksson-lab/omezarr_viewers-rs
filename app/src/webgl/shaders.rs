//! The three shader programs.
//!
//! All of them draw the same unit quad through the same camera transform, so
//! the vertex attributes are pinned to explicit locations: one VAO is bound
//! once and every program reads it. A program that let the linker choose its
//! own attribute slots would need its own VAO, and the two would silently
//! disagree the first time a shader gained an input.

/// The camera half of the vertex shader, shared verbatim by every program.
const CAMERA: &str = r#"
uniform vec2 u_pan;
uniform float u_zoom;
uniform vec2 u_canvas_size;
uniform vec2 u_tile_offset;  // tile position in image pixels
uniform vec2 u_tile_size;    // tile size in image pixels
uniform vec2 u_image_size;   // full image size in image pixels

// Image-pixel position to clip space, the one place the camera is applied.
vec4 to_clip(vec2 img_pixel) {
    vec2 img_pos = img_pixel / u_image_size;
    vec2 centered = (img_pos - 0.5) * 2.0;
    float fit = u_zoom * min(u_canvas_size.x / u_image_size.x,
                             u_canvas_size.y / u_image_size.y);
    vec2 scale = vec2(fit * u_image_size.x / u_canvas_size.x,
                      fit * u_image_size.y / u_canvas_size.y);
    vec2 screen_pos = (centered * scale) + u_pan * 2.0 / u_canvas_size;
    return vec4(screen_pos.x, -screen_pos.y, 0.0, 1.0);
}
"#;

/// Vertex shader for the tile quad — used by the intensity and label programs.
pub fn tile_vertex_shader() -> String {
    format!(
        r#"#version 300 es
precision highp float;

layout(location = 0) in vec2 a_position;
layout(location = 1) in vec2 a_texcoord;
{CAMERA}
out vec2 v_texcoord;

void main() {{
    gl_Position = to_clip(u_tile_offset + a_position * u_tile_size);
    v_texcoord = a_texcoord;
}}
"#
    )
}

/// Additive multi-channel intensity compositing. Unchanged in substance from
/// the single-program renderer this grew out of.
pub const FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;

in vec2 v_texcoord;
out vec4 fragColor;

// Up to 6 single-channel textures
uniform sampler2D u_channel0;
uniform sampler2D u_channel1;
uniform sampler2D u_channel2;
uniform sampler2D u_channel3;
uniform sampler2D u_channel4;
uniform sampler2D u_channel5;

uniform vec3 u_color[6];
uniform vec2 u_contrast[6];    // (min, max)
uniform float u_opacity[6];
uniform int u_num_channels;
uniform float u_dtype_max;      // e.g., 255.0 for uint8, 65535.0 for uint16

float get_channel(int idx) {
    if (idx == 0) return texture(u_channel0, v_texcoord).r;
    if (idx == 1) return texture(u_channel1, v_texcoord).r;
    if (idx == 2) return texture(u_channel2, v_texcoord).r;
    if (idx == 3) return texture(u_channel3, v_texcoord).r;
    if (idx == 4) return texture(u_channel4, v_texcoord).r;
    if (idx == 5) return texture(u_channel5, v_texcoord).r;
    return 0.0;
}

void main() {
    vec3 rgb = vec3(0.0);
    for (int i = 0; i < 6; i++) {
        if (i >= u_num_channels) break;
        float raw = get_channel(i);
        float normed = clamp(
            (raw - u_contrast[i].x) / max(u_contrast[i].y - u_contrast[i].x, 0.001),
            0.0, 1.0
        );
        rgb += normed * u_color[i] * u_opacity[i];
    }
    // Alpha 1, so this is already premultiplied. See the label shader.
    fragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;

/// Label rendering: integer ids in, a colour per id out.
///
/// Three things this does that the intensity shader cannot. The sampler is a
/// `usampler2D`, so an id arrives as the integer it is rather than as a float
/// that lost its low bits. Sampling is `texelFetch`, so no id is ever
/// interpolated into an id that does not exist. And the outline mode compares
/// a texel with its four neighbours, which is a question about *ids* and is
/// meaningless once they have been averaged.
pub const LABEL_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
precision highp int;

in vec2 v_texcoord;
out vec4 fragColor;

uniform highp usampler2D u_labels;
uniform vec2 u_tex_size;
uniform float u_opacity;
uniform int u_outline;        // 0 = fill, 1 = boundaries only
uniform uint u_selected;      // 0 = nothing selected
uniform int u_only_selected;  // 1 = hide every other id
uniform sampler2D u_lut;      // RGBA8 colour table, one texel per id
uniform int u_lut_size;       // 0 = colour ids by hash

vec3 hsv2rgb(vec3 c) {
    vec4 K = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, 0.0, 1.0), c.y);
}

// A stable colour per id: Knuth's multiplicative hash, spread over hue with
// enough variation in saturation and value that adjacent ids do not collide
// visually the way a pure hue ramp does.
vec3 id_color(uint id) {
    uint h = id * 2654435761u;
    float hue = float((h >> 8) & 1023u) / 1023.0;
    float sat = 0.55 + float((h >> 18) & 63u) / 63.0 * 0.35;
    float val = 0.70 + float((h >> 24) & 63u) / 63.0 * 0.30;
    return hsv2rgb(vec3(hue, sat, val));
}

uint id_at(ivec2 texel) {
    ivec2 clamped = clamp(texel, ivec2(0), ivec2(u_tex_size) - ivec2(1));
    return texelFetch(u_labels, clamped, 0).r;
}

void main() {
    ivec2 texel = ivec2(v_texcoord * u_tex_size);
    uint id = id_at(texel);

    // 0 is background in every label convention this reads.
    if (id == 0u) discard;
    if (u_only_selected == 1 && id != u_selected) discard;

    float alpha = u_opacity;
    if (u_outline == 1) {
        bool edge = id_at(texel + ivec2(1, 0)) != id
                 || id_at(texel + ivec2(-1, 0)) != id
                 || id_at(texel + ivec2(0, 1)) != id
                 || id_at(texel + ivec2(0, -1)) != id;
        if (!edge) discard;
        alpha = min(1.0, u_opacity * 1.8);
    }

    vec3 rgb;
    if (u_lut_size > 0 && id < uint(u_lut_size)) {
        vec4 entry = texelFetch(u_lut, ivec2(int(id), 0), 0);
        // A zero-alpha entry means the table does not name this id.
        rgb = entry.a > 0.0 ? entry.rgb : id_color(id);
    } else {
        rgb = id_color(id);
    }

    if (u_selected != 0u && id == u_selected) {
        rgb = mix(rgb, vec3(1.0), 0.45);
        alpha = min(1.0, alpha * 1.6 + 0.2);
    }

    // Premultiplied, because the canvas is: a colour channel above the alpha
    // it is paired with is not a valid premultiplied pixel, and the compositor
    // is free to make nonsense of it — which it does, dropping whole channels.
    fragColor = vec4(rgb * alpha, alpha);
}
"#;

/// Object points: one vertex per row, drawn as a round sprite.
///
/// `POINTS` rather than instanced quads because the shape is a disc and the
/// data is one position per row — an instanced quad would upload four vertices
/// to say the same thing. The size is in *screen* pixels, so a cell stays
/// visible when zoomed out and does not swell into a blob when zoomed in.
pub fn point_vertex_shader() -> String {
    format!(
        r#"#version 300 es
precision highp float;

layout(location = 0) in vec3 a_position;   // world (z, y, x)
layout(location = 1) in float a_value;     // the coloured column, or NaN
layout(location = 2) in float a_row;       // row index, for selection
{CAMERA}
uniform float u_point_size;   // screen pixels
uniform float u_z;            // the slice being viewed, in world z
uniform float u_slab;         // z distance at which a point fades out
uniform float u_selected_row;

out float v_value;
out float v_fade;
out float v_selected;

void main() {{
    gl_Position = to_clip(vec2(a_position.z, a_position.y));
    float dz = abs(a_position.x - u_z);
    // A point set with no z (a 2D detector's) has slab 0 and never fades.
    v_fade = u_slab > 0.0 ? clamp(1.0 - dz / u_slab, 0.0, 1.0) : 1.0;
    v_selected = abs(a_row - u_selected_row) < 0.5 ? 1.0 : 0.0;
    v_value = a_value;
    gl_PointSize = u_point_size * (v_selected > 0.5 ? 1.6 : 1.0);
}}
"#
    )
}

/// Object points: a disc, coloured by a column or by a fixed colour.
pub const POINT_FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;

in float v_value;
in float v_fade;
in float v_selected;
out vec4 fragColor;

uniform vec3 u_color;
uniform float u_opacity;
uniform int u_color_by_value;   // 1 = map v_value through the ramp
uniform vec2 u_value_range;     // (min, max) for the ramp
uniform int u_hollow;           // 1 = ring rather than disc

// A perceptual-ish ramp: dark blue -> teal -> green -> yellow. Cheap enough to
// evaluate per fragment and monotone in lightness, which is what makes a value
// readable rather than merely colourful.
vec3 ramp(float t) {
    t = clamp(t, 0.0, 1.0);
    vec3 c0 = vec3(0.27, 0.00, 0.33);
    vec3 c1 = vec3(0.13, 0.42, 0.56);
    vec3 c2 = vec3(0.15, 0.68, 0.49);
    vec3 c3 = vec3(0.99, 0.91, 0.15);
    if (t < 0.33) return mix(c0, c1, t / 0.33);
    if (t < 0.66) return mix(c1, c2, (t - 0.33) / 0.33);
    return mix(c2, c3, (t - 0.66) / 0.34);
}

void main() {
    vec2 offset = gl_PointCoord * 2.0 - 1.0;
    float r = length(offset);
    if (r > 1.0) discard;
    if (u_hollow == 1 && r < 0.55) discard;

    vec3 rgb = u_color;
    if (u_color_by_value == 1) {
        float span = max(u_value_range.y - u_value_range.x, 1e-6);
        rgb = ramp((v_value - u_value_range.x) / span);
    }
    if (v_selected > 0.5) {
        rgb = mix(rgb, vec3(1.0), 0.5);
    }

    // Soften the rim by one pixel's worth of radius rather than leaving a
    // hard-aliased circle; `fwidth` is the derivative of r across the sprite.
    float edge = 1.0 - smoothstep(1.0 - fwidth(r) * 2.0, 1.0, r);
    float alpha = u_opacity * v_fade * edge;
    if (v_selected > 0.5) {
        alpha = min(1.0, alpha * 1.5 + 0.25);
    }
    if (alpha <= 0.0) discard;
    fragColor = vec4(rgb * alpha, alpha);
}
"#;
