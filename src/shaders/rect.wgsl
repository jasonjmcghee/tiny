// Rectangle shader for drawing filled rectangles

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
    @location(0) position: vec2<f32>,
    @location(1) color: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Convert position from pixel coords to clip space
    out.clip_position = vec4<f32>(
        (position.x / (uniforms.viewport_size.x * 0.5)) - 1.0,
        1.0 - (position.y / (uniforms.viewport_size.y * 0.5)),
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