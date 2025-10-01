// Vertex shader
struct VertexInput {
    @location(0) position: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
}

struct Uniforms {
    viewport_size: vec2<f32>,
    color: u32,
    alpha: f32,  // 0.0 = invisible (blinking), 1.0 = visible
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

    return out;
}

// Fragment shader
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Unpack color from u32
    let r = f32((uniforms.color >> 24u) & 0xFFu) / 255.0;
    let g = f32((uniforms.color >> 16u) & 0xFFu) / 255.0;
    let b = f32((uniforms.color >> 8u) & 0xFFu) / 255.0;

    // Apply visibility alpha (for blinking)
    return vec4<f32>(r, g, b, uniforms.alpha);
}