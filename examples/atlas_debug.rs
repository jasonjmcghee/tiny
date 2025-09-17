//! Debug the font atlas to see what's actually in it

use tiny_editor::font::SharedFontSystem;

fn main() {
    println!("🔍 Font Atlas Debug");
    println!("==================");

    let font_system = SharedFontSystem::new();

    // Rasterize a single 'A' and examine atlas
    let layout = font_system.layout_text("A", 28.0); // 2x scale like the app
    println!("Layout result: {} glyphs", layout.glyphs.len());

    if !layout.glyphs.is_empty() {
        let glyph = &layout.glyphs[0];
        println!("Glyph 'A':");
        println!("  Position: ({:.1}, {:.1})", glyph.pos.x.0, glyph.pos.y.0);
        println!("  Size: ({:.1}, {:.1})", glyph.size.width.0, glyph.size.height.0);
        println!("  Texture coords: [{:.6}, {:.6}, {:.6}, {:.6}]",
                 glyph.tex_coords[0], glyph.tex_coords[1],
                 glyph.tex_coords[2], glyph.tex_coords[3]);

        // Calculate actual pixel region in atlas
        let (atlas_width, atlas_height) = font_system.atlas_size();
        let pixel_x0 = (glyph.tex_coords[0] * atlas_width as f32) as u32;
        let pixel_y0 = (glyph.tex_coords[1] * atlas_height as f32) as u32;
        let pixel_x1 = (glyph.tex_coords[2] * atlas_width as f32) as u32;
        let pixel_y1 = (glyph.tex_coords[3] * atlas_height as f32) as u32;

        println!("  Atlas region: ({}, {}) to ({}, {})", pixel_x0, pixel_y0, pixel_x1, pixel_y1);
        println!("  Atlas size: {}x{} pixels", pixel_x1 - pixel_x0, pixel_y1 - pixel_y0);

        // Check if there's actual data in that region
        let atlas_data = font_system.atlas_data();
        let mut non_zero_in_region = 0;
        let mut max_value = 0u8;

        for y in pixel_y0..pixel_y1 {
            for x in pixel_x0..pixel_x1 {
                let idx = (y * atlas_width + x) as usize;
                if idx < atlas_data.len() {
                    let pixel = atlas_data[idx];
                    if pixel > 0 {
                        non_zero_in_region += 1;
                        max_value = max_value.max(pixel);
                    }
                }
            }
        }

        println!("  Region analysis: {} non-zero pixels, max value: {}",
                 non_zero_in_region, max_value);

        if non_zero_in_region == 0 {
            println!("  🚨 ATLAS BUG: No glyph data in expected region!");
        } else {
            println!("  ✅ Glyph data exists in atlas");
        }

        // Test full atlas
        let total_non_zero = atlas_data.iter().filter(|&&p| p > 0).count();
        println!("Full atlas: {} total non-zero pixels", total_non_zero);
    }

    // Test if atlas changes after rasterization
    println!("\n--- Multiple Character Atlas Test ---");
    let layout2 = font_system.layout_text("ABC", 28.0);

    let atlas_data_after = font_system.atlas_data();
    let total_after = atlas_data_after.iter().filter(|&&p| p > 0).count();
    println!("After 'ABC': {} total non-zero pixels", total_after);

    for (i, glyph) in layout2.glyphs.iter().enumerate() {
        println!("  Glyph {}: '{}' tex=[{:.3}, {:.3}, {:.3}, {:.3}]",
                 i, glyph.char,
                 glyph.tex_coords[0], glyph.tex_coords[1],
                 glyph.tex_coords[2], glyph.tex_coords[3]);
    }
}