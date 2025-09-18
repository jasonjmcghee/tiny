//! Debug coordinate transformations to find rendering issues

use tiny_editor::font::SharedFontSystem;

fn main() {
    println!("🧮 Coordinate Debug");
    println!("==================");

    let font_system = SharedFontSystem::new();

    // Test actual glyph size vs texture coordinate calculation
    let layout = font_system.layout_text("A", 14.0);

    if !layout.glyphs.is_empty() {
        let glyph = &layout.glyphs[0];

        println!("Font system glyph 'A':");
        println!("  Position: ({:.3}, {:.3})", glyph.pos.x.0, glyph.pos.y.0);
        println!(
            "  Size from font: ({:.3}, {:.3})",
            glyph.size.width.0, glyph.size.height.0
        );
        println!(
            "  Texture coords: [{:.3}, {:.3}, {:.3}, {:.3}]",
            glyph.tex_coords[0], glyph.tex_coords[1], glyph.tex_coords[2], glyph.tex_coords[3]
        );

        // Calculate size the way GPU currently does it (WRONG)
        let atlas_size = 2048.0;
        let gpu_width = (glyph.tex_coords[2] - glyph.tex_coords[0]) * atlas_size;
        let gpu_height = (glyph.tex_coords[3] - glyph.tex_coords[1]) * atlas_size;

        println!(
            "  GPU calculated size: ({:.3}, {:.3})",
            gpu_width, gpu_height
        );

        // Compare
        if (glyph.size.width.0 - gpu_width).abs() > 1.0 {
            println!(
                "  🐛 SIZE MISMATCH: Font says {:.1}px wide, GPU calculates {:.1}px",
                glyph.size.width.0, gpu_width
            );
        }

        // Test coordinate transformation (simulate shader)
        let viewport_width = 800.0;
        let viewport_height = 400.0;

        let clip_x = (glyph.pos.x.0 / (viewport_width * 0.5)) - 1.0;
        let clip_y = 1.0 - (glyph.pos.y.0 / (viewport_height * 0.5));

        println!("Shader coordinate transformation:");
        println!("  Screen pos: ({:.3}, {:.3})", glyph.pos.x.0, glyph.pos.y.0);
        println!("  Clip space: ({:.3}, {:.3})", clip_x, clip_y);

        // Check if coordinates are reasonable
        if clip_x < -1.0 || clip_x > 1.0 || clip_y < -1.0 || clip_y > 1.0 {
            println!("  ⚠️  Clip coordinates outside [-1,1] range!");
        }

        // Test with position (1.0, 13.0) from your debug output
        println!("\nTesting your debug position (1.0, 13.0):");
        let debug_clip_x = (1.0 / (viewport_width * 0.5)) - 1.0;
        let debug_clip_y = 1.0 - (13.0 / (viewport_height * 0.5));
        println!("  Clip space: ({:.6}, {:.6})", debug_clip_x, debug_clip_y);

        // This should be near the left edge and top of screen
        if debug_clip_x < -0.99 && debug_clip_y > 0.9 {
            println!("  📍 Position is very close to top-left corner");
            println!("  💡 Text might be rendering but too small/close to edge to see");
        }
    }

    // Test multiple characters to see size differences
    println!("\n--- Multiple Character Size Test ---");
    for ch in ['A', 'B', 'i', 'W', 'M'] {
        let layout = font_system.layout_text(&ch.to_string(), 14.0);
        if !layout.glyphs.is_empty() {
            let glyph = &layout.glyphs[0];
            println!(
                "'{}': size=({:.1}, {:.1}) tex_size=({:.1}, {:.1})",
                ch,
                glyph.size.width.0,
                glyph.size.height.0,
                (glyph.tex_coords[2] - glyph.tex_coords[0]) * 2048.0,
                (glyph.tex_coords[3] - glyph.tex_coords[1]) * 2048.0
            );
        }
    }
}
