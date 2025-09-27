// Vertex shader for squiggly lines and popup backgrounds
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: u32,
    @location(2) line_info: vec4<f32>, // x, y, width, severity
    @location(3) is_popup: u32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) pixel_pos: vec2<f32>,
    @location(2) line_info: vec4<f32>,
    @location(3) is_popup: f32,
}

struct Uniforms {
    viewport_size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Convert from pixel coordinates to normalized device coordinates
    let x = (input.position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    let y = 1.0 - (input.position.y / uniforms.viewport_size.y) * 2.0;

    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    out.pixel_pos = input.position;

    // Unpack color from u32 to vec4
    let r = f32((input.color >> 24u) & 0xFFu) / 255.0;
    let g = f32((input.color >> 16u) & 0xFFu) / 255.0;
    let b = f32((input.color >> 8u) & 0xFFu) / 255.0;
    let a = f32(input.color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    out.line_info = input.line_info;
    out.is_popup = f32(input.is_popup);

    return out;
}

// Fragment shader
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let px = input.pixel_pos.x;
    let py = input.pixel_pos.y;

    // Check if this is a popup background
    if input.is_popup > 0.5 {
        // Render solid color with slight transparency for popup
        return vec4<f32>(input.color.rgb, 0.95);
    }

    // Otherwise, render squiggly line
    let line_x = input.line_info.x;
    let line_y = input.line_info.y;
    let line_width = input.line_info.z;
    let severity = input.line_info.w;

    // Check if we're within the line bounds
    if px < line_x || px > line_x + line_width {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Create squiggly pattern using sine wave
    let wave_frequency = 0.5; // Controls how tight the waves are
    let wave_amplitude = 2.0; // Controls wave height in pixels

    // Calculate the expected y position for the squiggle at this x
    let x_offset = px - line_x;
    let wave_y = line_y + sin(x_offset * wave_frequency) * wave_amplitude;

    // Check if pixel is on the squiggly line (with 1.5 pixel tolerance for thickness)
    let distance = abs(py - wave_y);
    if distance < 1.5 {
        // Anti-aliasing: fade out at edges
        let alpha = 1.0 - smoothstep(0.5, 1.5, distance);

        // Color based on severity
        var color: vec3<f32>;
        if severity < 0.5 {
            // Error - red
            color = vec3<f32>(1.0, 0.2, 0.2);
        } else if severity < 1.5 {
            // Warning - yellow
            color = vec3<f32>(1.0, 0.8, 0.2);
        } else {
            // Info - blue
            color = vec3<f32>(0.2, 0.6, 1.0);
        }

        return vec4<f32>(color, alpha);
    }

    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}