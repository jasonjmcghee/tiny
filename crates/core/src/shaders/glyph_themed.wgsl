// Glyph rendering with theme texture support

struct Uniforms {
    viewport_size: vec2<f32>,
    scale_factor: f32,
    time: f32,  // For animations
    theme_mode: u32  // Which theme effect to use
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) token_id: f32,
    @location(3) relative_pos: f32,
    @location(4) format: u32,
    @location(5) atlas_index: u32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var glyph_texture: texture_2d<f32>;

@group(1) @binding(1)
var glyph_sampler: sampler;

@group(1) @binding(2)
var color_glyph_texture: texture_2d<f32>;

@group(1) @binding(3)
var color_glyph_sampler: sampler;

@group(2) @binding(0)
var theme_texture: texture_2d<f32>;

@group(2) @binding(1)
var theme_sampler: sampler;

fn unpack_color(packed: u32) -> vec4<f32> {
    let r = f32((packed >> 24u) & 0xFFu) / 255.0;
    let g = f32((packed >> 16u) & 0xFFu) / 255.0;
    let b = f32((packed >> 8u) & 0xFFu) / 255.0;
    let a = f32(packed & 0xFFu) / 255.0;
    return vec4<f32>(r, g, b, a);
}

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_idx: u32,
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) token_id: u32,
    @location(3) relative_pos: f32,
    @location(4) format: u32,
    @location(5) atlas_index: u32,
) -> VertexOutput {
    // Convert position from pixel coordinates to normalized device coordinates
    let ndc_x = (position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (position.y / uniforms.viewport_size.y) * 2.0; // Flip Y

    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.tex_coords = tex_coord;
    out.token_id = f32(token_id);
    out.relative_pos = relative_pos;
    out.format = format;
    out.atlas_index = atlas_index;

    return out;
}

// ============================================================================
// COLOR UTILITY FUNCTIONS
// ============================================================================

// HSV to RGB conversion
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = v - c;

    var rgb: vec3<f32>;
    if h < 0.166667 {
        rgb = vec3<f32>(c, x, 0.0);
    } else if h < 0.333333 {
        rgb = vec3<f32>(x, c, 0.0);
    } else if h < 0.5 {
        rgb = vec3<f32>(0.0, c, x);
    } else if h < 0.666667 {
        rgb = vec3<f32>(0.0, x, c);
    } else if h < 0.833333 {
        rgb = vec3<f32>(x, 0.0, c);
    } else {
        rgb = vec3<f32>(c, 0.0, x);
    }

    return rgb + vec3<f32>(m, m, m);
}

// ============================================================================
// THEME EFFECT FUNCTIONS
// ============================================================================

// Effect 1: Pastel Rotating Rainbow
fn pastel_rainbow_effect(token_id: f32, relative_pos: f32, time: f32) -> vec3<f32> {
    // Speed of rotation (slower for more calming effect)
    let rotation_speed = 0.15;

    // Calculate hue based on relative position and time
    let base_hue = fract(relative_pos * 0.7 + time * rotation_speed);

    // Add subtle variation based on token type
    let token_id_uint = u32(token_id);
    let token_variation = f32(token_id_uint % 5u) * 0.15;
    let varied_hue = fract(relative_pos * 0.7 + time * rotation_speed + token_variation);

    // Slightly vary saturation and value per token type for more interest
    let token_sat_variation = 0.35 + f32((token_id_uint + 3u) % 4u) * 0.08; // Range: 0.35-0.59
    let token_val_variation = 0.88 + f32((token_id_uint + 7u) % 3u) * 0.04; // Range: 0.88-0.96

    return hsv_to_rgb(varied_hue, token_sat_variation, token_val_variation);
}

// Effect 2: Vibrant Rotating Rainbow
fn vibrant_rainbow_effect(token_id: f32, relative_pos: f32, time: f32) -> vec3<f32> {
    let rotation_speed = 0.3;
    let token_id_uint = u32(token_id);
    let token_variation = f32(token_id_uint % 7u) / 7.0;
    let hue = fract(relative_pos + time * rotation_speed + token_variation * 0.3);
    return hsv_to_rgb(hue, 1.0, 0.9);
}

// Effect 3: Theme with Shine Wave
fn theme_with_shine_effect(token_id: f32, relative_pos: f32, time: f32) -> vec3<f32> {
    // Sample the base theme color from texture
    // Token IDs are at pixel positions 0-255, texture coordinates are 0-1
    // So we add 0.5 to sample the center of the pixel and divide by 256
    let theme_x = (token_id + 0.5) / 256.0;
    let theme_y = 0.5; // Sample from center of first row (single color themes)
    let base_color = textureSample(theme_texture, theme_sampler, vec2<f32>(theme_x, theme_y)).rgb;

    // Create a ROTATING shine that continuously sweeps left to right
    let wave_speed = 0.2; // Speed of rotation
    let wave_width = 0.15; // Width of the shine band
    let wave_intensity = 0.25; // Brightness of the shine

    // Continuous rotation: the wave goes from -wave_width to 1.0 + wave_width
    // This ensures smooth entry and exit
    let wave_pos = fract(time * wave_speed) * (1.0 + wave_width * 2.0) - wave_width;

    // Calculate distance from current position to wave center
    let dist_to_wave = abs(relative_pos - wave_pos);

    // Create smooth shine gradient
    // Using smoothstep for a nice controlled falloff
    let shine_factor = 1.0 - smoothstep(0.0, wave_width, dist_to_wave);

    // Add extra brightness at the center of the beam
    let center_boost = exp(-dist_to_wave * dist_to_wave * 30.0) * 0.3;

    let total_shine = (shine_factor + center_boost) * wave_intensity;

    // Add shine to base color
    return mix(base_color, vec3<f32>(1.0, 1.0, 1.0), total_shine);
}

// Effect 4: Static Theme (no animation)
fn static_theme_effect(token_id: f32, relative_pos: f32) -> vec3<f32> {
    // Sample center of pixel for token_id
    let theme_x = (token_id + 0.5) / 256.0;
    let theme_y = 0.5; // Center of first row for single-color themes
    return textureSample(theme_texture, theme_sampler, vec2<f32>(theme_x, theme_y)).rgb;
}

// Effect 5: Theme Interpolation (between two themes)
fn theme_interpolation_effect(token_id: f32, relative_pos: f32, time: f32) -> vec3<f32> {
    let theme_x = (token_id + 0.5) / 256.0;
    let interpolation_factor = sin(time * 0.5) * 0.5 + 0.5;

    // Sample both themes (stacked vertically in texture)
    // For merged themes, first theme is in top half, second in bottom half
    let theme1_y = 0.25; // Center of top half
    let theme2_y = 0.75; // Center of bottom half

    let color1 = textureSample(theme_texture, theme_sampler, vec2<f32>(theme_x, theme1_y)).rgb;
    let color2 = textureSample(theme_texture, theme_sampler, vec2<f32>(theme_x, theme2_y)).rgb;

    return mix(color1, color2, interpolation_factor);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample from correct atlas based on atlas_index
    var glyph_alpha: f32;
    var glyph_color: vec3<f32>;
    var is_color_glyph: bool = false;

    if in.atlas_index == 1u {
        // Color atlas (RGBA8) - for emojis
        let sampled = textureSample(color_glyph_texture, color_glyph_sampler, in.tex_coords);
        glyph_color = sampled.rgb;
        glyph_alpha = sampled.a;
        is_color_glyph = true;
    } else {
        // Monochrome atlas (R8) - for regular text
        glyph_alpha = textureSample(glyph_texture, glyph_sampler, in.tex_coords).r;
        glyph_color = vec3<f32>(1.0, 1.0, 1.0); // Will be replaced by theme color
    }

    // For color glyphs (emojis), skip theme effects and use direct color
    if is_color_glyph {
        return vec4<f32>(glyph_color, glyph_alpha);
    }

    // Choose theme effect based on uniform
    var final_color: vec3<f32>;

    // Theme modes:
    // 0 = Pastel Rainbow
    // 1 = Vibrant Rainbow
    // 2 = Theme with Shine
    // 3 = Static Theme
    // 4 = Theme Interpolation

    var theme = uniforms.theme_mode;

    theme = 2;

    if u32(in.token_id) == 255u {
        final_color = static_theme_effect(in.token_id, in.relative_pos);
    } else {
        switch theme {
            case 0u: {
                // Pastel rotating rainbow
                final_color = pastel_rainbow_effect(in.token_id, in.relative_pos, uniforms.time);
            }
            case 1u: {
                // Vibrant rotating rainbow
                final_color = vibrant_rainbow_effect(in.token_id, in.relative_pos, uniforms.time);
            }
            case 2u: {
                // Theme with shine wave effect
                final_color = theme_with_shine_effect(in.token_id, in.relative_pos, uniforms.time);
            }
            case 3u: {
                // Static theme (no animation)
                final_color = static_theme_effect(in.token_id, in.relative_pos);
            }
            case 4u: {
                // Theme interpolation
                final_color = theme_interpolation_effect(in.token_id, in.relative_pos, uniforms.time);
            }
            default: {
                // Default to pastel rainbow
                final_color = pastel_rainbow_effect(in.token_id, in.relative_pos, uniforms.time);
            }
        }
    }

    // Apply format modifiers
    // Format bits:
    // Bit 0 (0x01): Half opacity (for autocomplete)
    // Bit 1 (0x02): Underline
    // Bit 2 (0x04): Background highlight
    // Bit 3-7: Reserved for future use

    var final_alpha = glyph_alpha;

    // Half opacity for autocomplete suggestions
    if (in.format & 0x01u) != 0u {
        final_alpha *= 0.5;
    }

    // TODO: Underline rendering (bit 1)
    // TODO: Background highlight (bit 2)

    // Combine glyph shape with chosen color effect and format modifiers
    return vec4<f32>(final_color, final_alpha);
}
