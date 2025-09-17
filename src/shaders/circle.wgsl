// SDF Circle Shader
struct Uniforms {
    viewport_size: vec2<f32>,
    _padding: vec2<f32>,
}

struct CircleData {
    center: vec2<f32>,
    radius: f32,
    color: f32, // Color packed as u32 bits
}

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) circle_center: vec2<f32>,
    @location(2) circle_radius: f32,
    @location(3) circle_color: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec2<f32>,
    @location(1) circle_center: vec2<f32>,
    @location(2) circle_radius: f32,
    @location(3) circle_color: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Convert to clip coordinates
    let clip_x = (input.position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    let clip_y = -((input.position.y / uniforms.viewport_size.y) * 2.0 - 1.0); // Flip Y

    out.clip_position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.world_position = input.position;
    out.circle_center = input.circle_center;
    out.circle_radius = input.circle_radius;

    // Unpack color from f32
    let color_bits = bitcast<u32>(input.circle_color);
    let r = f32((color_bits >> 16u) & 0xFFu) / 255.0;
    let g = f32((color_bits >> 8u) & 0xFFu) / 255.0;
    let b = f32(color_bits & 0xFFu) / 255.0;
    let a = f32((color_bits >> 24u) & 0xFFu) / 255.0;
    out.circle_color = vec4<f32>(r, g, b, a);

    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let distance_to_center = length(input.world_position - input.circle_center);

    // SDF for circle: negative inside, positive outside
    let sdf = distance_to_center - input.circle_radius;

    // Anti-aliasing: smooth step at the edge
    let alpha = 1.0 - smoothstep(-1.0, 1.0, sdf);

    return vec4<f32>(input.circle_color.rgb, input.circle_color.a * alpha);
}