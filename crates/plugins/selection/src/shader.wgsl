// Vertex shader
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: u32,
    @location(2) start_pos: vec2<f32>,
    @location(3) end_pos: vec2<f32>,
    @location(4) line_height: f32,
    @location(5) margin_left: f32,
    @location(6) margin_right: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) pixel_pos: vec2<f32>,
    @location(2) start_pos: vec2<f32>,
    @location(3) end_pos: vec2<f32>,
    @location(4) line_height: f32,
    @location(5) margin_left: f32,
    @location(6) margin_right: f32,
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

    // Pass through selection data
    out.start_pos = input.start_pos;
    out.end_pos = input.end_pos;
    out.line_height = input.line_height;
    out.margin_left = input.margin_left;
    out.margin_right = input.margin_right;

    return out;
}

// Fragment shader
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let px = input.pixel_pos.x;
    let py = input.pixel_pos.y;

    // Check if pixel should be visible based on selection shape
    var visible = false;

    // For single line selections
    if abs(input.start_pos.y - input.end_pos.y) < 0.1 {
        // Single line selection - simple range check
        if py >= input.start_pos.y && py < input.start_pos.y + input.line_height &&
           px >= input.start_pos.x && px <= input.end_pos.x {
            visible = true;
        }
    } else {
        // Multi-line selection - check which part of the selection we're in
        if py >= input.start_pos.y && py < input.start_pos.y + input.line_height {
            // First line: from start_x to right margin
            if px >= input.start_pos.x && px <= input.margin_right {
                visible = true;
            }
        } else if py >= input.end_pos.y && py < input.end_pos.y + input.line_height {
            // Last line: from left margin to end_x
            if px >= input.margin_left && px <= input.end_pos.x {
                visible = true;
            }
        } else if py > input.start_pos.y && py < input.end_pos.y {
            // Middle lines: full width from left to right margin
            if px >= input.margin_left && px <= input.margin_right {
                visible = true;
            }
        }
    }

    if visible {
        // Debug: show UV coordinates instead of color
        // Calculate UV within the bounding box
        let min_x = min(input.start_pos.x, input.margin_left);
        let max_x = max(input.end_pos.x, input.margin_right);
        let min_y = input.start_pos.y;
        let max_y = input.end_pos.y + input.line_height;

        let u = (px - min_x) / (max_x - min_x);
        let v = (py - min_y) / (max_y - min_y);

        // return vec4<f32>(u, v, 1.0, 0.2);
        return input.color;
    } else {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0); // Transparent
    }
}