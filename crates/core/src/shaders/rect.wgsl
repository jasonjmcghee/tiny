// Rectangle shader using instanced rendering
// Each instance is a rect with its own position, size, and color

struct Uniforms {
    viewport_size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
    // Vertex data: unit quad corner (0-1 range)
    @location(0) vertex_pos: vec2<f32>,
    // Instance data: per-rect position, size, and color
    @location(1) rect_pos: vec2<f32>,
    @location(2) rect_size: vec2<f32>,
    @location(3) color: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Scale unit quad to rect size and translate to rect position
    let pixel_pos = rect_pos + vertex_pos * rect_size;

    // Convert position from pixel coords to clip space
    out.clip_position = vec4<f32>(
        (pixel_pos.x / uniforms.viewport_size.x) * 2.0 - 1.0,
        1.0 - (pixel_pos.y / uniforms.viewport_size.y) * 2.0,
        0.0,
        1.0
    );

    // Convert color from packed u32 to vec4
    let r = f32((color >> 24u) & 0xFFu) / 255.0;
    let g = f32((color >> 16u) & 0xFFu) / 255.0;
    let b = f32((color >> 8u) & 0xFFu) / 255.0;
    let a = f32(color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}