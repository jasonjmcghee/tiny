//! Debug the font system directly to identify glyph positioning bug

use tiny_editor::font::SharedFontSystem;

fn main() {
    println!("🔍 Font System Debug");
    println!("===================");

    let font_system = SharedFontSystem::new();

    // Test 1: Single character
    println!("\n--- Single Character Test ---");
    let layout_single = font_system.layout_text("A", 14.0);
    println!("'A' -> {} glyphs", layout_single.glyphs.len());

    if !layout_single.glyphs.is_empty() {
        let glyph = &layout_single.glyphs[0];
        println!(
            "  Glyph: '{}' at ({:.1}, {:.1})",
            glyph.char, glyph.pos.x.0, glyph.pos.y.0
        );
    }

    // Test 2: Two characters - THIS SHOULD REVEAL THE BUG
    println!("\n--- Two Character Test (Critical) ---");
    let layout_double = font_system.layout_text("AB", 14.0);
    println!("'AB' -> {} glyphs", layout_double.glyphs.len());

    if layout_double.glyphs.len() >= 2 {
        let glyph_a = &layout_double.glyphs[0];
        let glyph_b = &layout_double.glyphs[1];

        println!(
            "  Glyph A: '{}' at ({:.1}, {:.1})",
            glyph_a.char, glyph_a.pos.x.0, glyph_a.pos.y.0
        );
        println!(
            "  Glyph B: '{}' at ({:.1}, {:.1})",
            glyph_b.char, glyph_b.pos.x.0, glyph_b.pos.y.0
        );
        println!(
            "  X advancement: {:.1} pixels",
            glyph_b.pos.x.0 - glyph_a.pos.x.0
        );

        // CRITICAL TEST: Are they at the same position?
        if glyph_a.pos.x == glyph_b.pos.x {
            println!("  🐛 BUG CONFIRMED: Both glyphs at same X position!");
        } else {
            println!("  ✅ Glyphs properly advance horizontally");
        }
    }

    // Test 3: Multiple character sequence
    println!("\n--- Multiple Character Test ---");
    let layout_multi = font_system.layout_text("Hello", 14.0);
    println!("'Hello' -> {} glyphs", layout_multi.glyphs.len());

    for (i, glyph) in layout_multi.glyphs.iter().enumerate() {
        println!(
            "  [{}] '{}' at ({:.3}, {:.3})",
            i, glyph.char, glyph.pos.x.0, glyph.pos.y.0
        );
    }

    // Check if all glyphs are at same position (bug indicator)
    if layout_multi.glyphs.len() > 1 {
        let first_x = layout_multi.glyphs[0].pos.x;
        let all_same = layout_multi.glyphs.iter().all(|g| g.pos.x == first_x);

        if all_same {
            println!(
                "  🚨 CRITICAL BUG: All glyphs at same X position: {:.1}",
                first_x.0
            );
        } else {
            println!("  ✅ Glyphs have different X positions");
        }
    }

    // Test 4: Atlas verification
    println!("\n--- Atlas Verification ---");
    font_system.prerasterize_ascii(14.0);
    let atlas_data = font_system.atlas_data();
    let (width, height) = font_system.atlas_size();
    let non_zero = atlas_data.iter().filter(|&&p| p > 0).count();

    println!("Atlas: {}x{}, {} non-zero pixels", width, height, non_zero);

    if non_zero > 0 {
        println!("  ✅ Atlas contains rasterized glyphs");
    } else {
        println!("  🚨 Atlas is empty - no glyphs rasterized");
    }
}
