// Instanced glyph shader with SDF circle text effects
// Based on glyph.wgsl but adds SDF circle effect blending

struct Uniforms {
    viewport_size: vec2<f32>
}

struct CircleEffectUniforms {
    mouse_pos: vec2<f32>,
    radius: f32,
    _padding: f32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var t_glyph: texture_2d<f32>;
@group(1) @binding(1)
var s_glyph: sampler;

@group(2) @binding(0)
var<uniform> circle_effect: CircleEffectUniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_pos: vec2<f32>,
}

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Same positioning as original glyph shader
    out.clip_position = vec4<f32>(
        (position.x / (uniforms.viewport_size.x * 0.5)) - 1.0,
        1.0 - (position.y / (uniforms.viewport_size.y * 0.5)),
        0.0,
        1.0
    );

    out.tex_coord = tex_coord;
    out.world_pos = position; // Pass world position for SDF calculation

    // Convert color from packed u32 to vec4 (same as original)
    let r = f32((color >> 24u) & 0xFFu) / 255.0;
    let g = f32((color >> 16u) & 0xFFu) / 255.0;
    let b = f32((color >> 8u) & 0xFFu) / 255.0;
    let a = f32(color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample glyph texture (alpha channel) - same as original
    let glyph_alpha = textureSample(t_glyph, s_glyph, in.tex_coord).r;

    // Calculate SDF circle effect
    let dist = length(in.world_pos - circle_effect.mouse_pos);
    let sdf = dist - circle_effect.radius;
    let circle_influence = 1.0 - smoothstep(-2.0, 2.0, sdf);

    // Blend original text color with red circle effect
    let base_color = in.color.rgb;
    let circle_color = vec3<f32>(1.0, 0.2, 0.2); // Bright red
    let final_color = mix(base_color, circle_color, circle_influence);

    // Apply glyph alpha (same as original)
    return vec4<f32>(final_color, in.color.a * glyph_alpha);
}