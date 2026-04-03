pub const VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;

in vec2 a_position;
in vec2 a_texcoord;

uniform vec2 u_pan;
uniform float u_zoom;
uniform vec2 u_canvas_size;
uniform vec2 u_tile_offset;  // tile position in image pixels
uniform vec2 u_tile_size;    // tile size in image pixels
uniform vec2 u_image_size;   // full image size in image pixels

out vec2 v_texcoord;

void main() {
    // Convert tile pixel position to normalized image coords (0..1)
    vec2 img_pos = (u_tile_offset + a_position * u_tile_size) / u_image_size;

    // Apply pan and zoom to get clip space coords
    // Center the image: (0,0) in image space maps to center of screen
    vec2 centered = (img_pos - 0.5) * 2.0;  // -1..1
    vec2 aspect = vec2(u_image_size.x / u_canvas_size.x, u_image_size.y / u_canvas_size.y);
    float scale = u_zoom * min(u_canvas_size.x / u_image_size.x, u_canvas_size.y / u_image_size.y);
    vec2 screen_pos = (centered * scale) + u_pan * 2.0 / u_canvas_size;

    gl_Position = vec4(screen_pos.x, -screen_pos.y, 0.0, 1.0);
    v_texcoord = a_texcoord;
}
"#;

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
    fragColor = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
"#;
