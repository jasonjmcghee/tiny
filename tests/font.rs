use tiny_editor::coordinates::PhysicalPixels;
use tiny_editor::font::*;

#[test]
fn test_font_system_creation() {
    let mut font_system = FontSystem::new();
    assert_eq!(font_system.atlas_size(), (2048, 2048));

    // Layout some text at base size
    let layout = font_system.layout_text("Hello", 14.0);
    assert_eq!(layout.glyphs.len(), 5); // 5 characters
    assert!(layout.width > 0.0);
    assert!(layout.height > 0.0);
}

#[test]
fn test_single_char_layout() {
    let mut font_system = FontSystem::new();

    // Layout single 'A' at size 14
    let layout = font_system.layout_text("A", 14.0);
    assert_eq!(layout.glyphs.len(), 1);

    let glyph = &layout.glyphs[0];
    assert_eq!(glyph.char, 'A');
    assert_eq!(glyph.pos.x, PhysicalPixels(0.0)); // First char at x=0
                                                  // The y position from fontdue represents baseline offset
                                                  // For the default font at size 14, this is exactly 4.0
    assert_eq!(glyph.pos.y, PhysicalPixels(4.0));
    assert!(glyph.size.width.0 > 0.0);
    assert!(glyph.size.height.0 > 0.0);

    // Texture coords should be valid
    assert!(glyph.tex_coords[2] > glyph.tex_coords[0]); // u1 > u0
    assert!(glyph.tex_coords[3] > glyph.tex_coords[1]); // v1 > v0
}

#[test]
fn test_layout_with_scale() {
    let mut font_system = FontSystem::new();

    // Layout at 2x scale (simulating retina)
    let layout_1x = font_system.layout_text("AB", 14.0);
    let layout_2x = font_system.layout_text("AB", 28.0); // 14 * 2

    // Should have same number of glyphs
    assert_eq!(layout_1x.glyphs.len(), 2);
    assert_eq!(layout_2x.glyphs.len(), 2);

    // 2x layout should have ~2x the spacing
    // Get the x position of the second character
    let spacing_1x = layout_1x.glyphs[1].pos.x.0;
    let spacing_2x = layout_2x.glyphs[1].pos.x.0;

    // Should be roughly 2x (within rounding tolerance)
    // Font metrics can vary, so we allow a wider range
    let ratio = spacing_2x / spacing_1x;
    assert!(ratio > 1.5 && ratio < 2.5, "Spacing ratio was {}", ratio);
}

#[test]
fn test_shared_font_system() {
    let font_system = SharedFontSystem::new();

    // Test layout with same font size
    let layout1 = font_system.layout_text("Test", 14.0);
    let layout2 = font_system.layout_text("Test", 14.0);

    assert_eq!(layout1.glyphs.len(), 4);
    assert_eq!(layout2.glyphs.len(), 4);

    // Glyphs should have same logical positions
    for (g1, g2) in layout1.glyphs.iter().zip(layout2.glyphs.iter()) {
        assert_eq!(g1.pos.x, g2.pos.x);
        assert_eq!(g1.pos.y, g2.pos.y);
    }
}

#[test]
fn test_prerasterize_ascii() {
    let font_system = SharedFontSystem::new();
    font_system.prerasterize_ascii(14.0);

    // After prerasterization, layout should be fast
    let layout = font_system.layout_text("ABC123", 14.0);
    assert_eq!(layout.glyphs.len(), 6);
}
