// Rounded rectangle shader with SDF-based borders
// Each instance is a rounded rect with position, size, colors, border, and corner radius

struct Uniforms {
    viewport_size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) border_color: vec4<f32>,
    @location(2) rect_pos: vec2<f32>,
    @location(3) rect_size: vec2<f32>,
    @location(4) corner_radius: f32,
    @location(5) border_width: f32,
    @location(6) frag_pos: vec2<f32>,
}

@vertex
fn vs_main(
    // Vertex data: unit quad corner (0-1 range)
    @location(0) vertex_pos: vec2<f32>,
    // Instance data: per-rect attributes
    @location(1) rect_pos: vec2<f32>,
    @location(2) rect_size: vec2<f32>,
    @location(3) color: u32,
    @location(4) border_color: u32,
    @location(5) corner_radius: f32,
    @location(6) border_width: f32,
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

    // Unpack colors from u32
    let r = f32((color >> 24u) & 0xFFu) / 255.0;
    let g = f32((color >> 16u) & 0xFFu) / 255.0;
    let b = f32((color >> 8u) & 0xFFu) / 255.0;
    let a = f32(color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    let br = f32((border_color >> 24u) & 0xFFu) / 255.0;
    let bg = f32((border_color >> 16u) & 0xFFu) / 255.0;
    let bb = f32((border_color >> 8u) & 0xFFu) / 255.0;
    let ba = f32(border_color & 0xFFu) / 255.0;
    out.border_color = vec4<f32>(br, bg, bb, ba);

    out.rect_pos = rect_pos;
    out.rect_size = rect_size;
    out.corner_radius = corner_radius;
    out.border_width = border_width;
    out.frag_pos = pixel_pos;

    return out;
}

// SDF for rounded rectangle
fn sdf_rounded_rect(pos: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let half_size = size * 0.5;
    let center = half_size;
    let p = abs(pos - center) - half_size + radius;
    return length(max(p, vec2<f32>(0.0))) + min(max(p.x, p.y), 0.0) - radius;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Calculate position relative to rect
    let local_pos = in.frag_pos - in.rect_pos;

    // Calculate SDF distance
    let dist = sdf_rounded_rect(local_pos, in.rect_size, in.corner_radius);

    // Smooth antialiasing
    let aa_width = 1.0;

    // Overall shape alpha (inside the shape)
    let shape_alpha = 1.0 - smoothstep(-aa_width, aa_width, dist);

    var final_color: vec4<f32>;
    if (in.border_width > 0.0 && shape_alpha > 0.0) {
        // Check if we're in the border region
        // dist < 0: inside shape
        // 0 <= dist < border_width: in border ring (outside shape, inside border)
        // dist >= border_width: outside everything

        let inner_edge = -in.border_width;
        let is_in_border = step(inner_edge, dist); // 1.0 if in border region, 0.0 if in fill

        // Mix fill and border colors
        final_color = mix(in.color, in.border_color, is_in_border);
        final_color.a *= shape_alpha;
    } else {
        // No border, just fill
        final_color = in.color;
        final_color.a *= shape_alpha;
    }

    return final_color;
}
