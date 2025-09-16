//! Tests that lock in the current WORKING state before refactoring
//!
//! These tests verify the exact behavior we have now that works correctly

use tiny_editor::font::SharedFontSystem;
use tiny_editor::render::{BatchedDraw, Renderer};
use tiny_editor::tree::{Content, Doc, Edit, Rect};
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_working_character_spacing() {
        // Test the EXACT behavior we have now with individual character insertion
        let doc = Doc::from_str("");

        // Insert individual characters (like typing)
        for (i, ch) in "ABC".chars().enumerate() {
            doc.edit(Edit::Insert {
                pos: i,
                content: Content::Text(ch.to_string()),
            });
            doc.flush();
        }

        // Set up renderer like our working examples
        let mut renderer = Renderer::new((800.0, 600.0), 2.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system.clone());

        let viewport = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        let batches = renderer.render(&doc.read(), viewport);

        // Should have exactly one glyph batch
        let glyph_batch = batches.iter().find_map(|batch| {
            if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                Some(instances)
            } else {
                None
            }
        });

        assert!(glyph_batch.is_some(), "Should have glyph batch");
        let glyphs = glyph_batch.unwrap();

        assert_eq!(glyphs.len(), 3, "Should have 3 glyphs for ABC");

        // CRITICAL: Test that characters advance properly (not overlap)
        assert!(glyphs[1].x > glyphs[0].x, "B should be right of A");
        assert!(glyphs[2].x > glyphs[1].x, "C should be right of B");

        // Test reasonable spacing (based on current working values)
        let spacing_ab = glyphs[1].x - glyphs[0].x;
        let spacing_bc = glyphs[2].x - glyphs[1].x;

        // Should be reasonable character spacing (not too small, not huge)
        assert!(spacing_ab > 5.0 && spacing_ab < 20.0, "A-B spacing should be reasonable");
        assert!(spacing_bc > 5.0 && spacing_bc < 20.0, "B-C spacing should be reasonable");

        // Should be on same line
        assert_eq!(glyphs[0].y, glyphs[1].y, "Should be on same line");
        assert_eq!(glyphs[1].y, glyphs[2].y, "Should be on same line");
    }

    #[test]
    fn test_current_working_line_spacing() {
        // Test multiline text like our working examples
        let doc = Doc::from_str("A\nB");
        let tree = doc.read();

        let mut renderer = Renderer::new((800.0, 600.0), 2.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        let batches = renderer.render(&tree, viewport);

        if let Some(BatchedDraw::GlyphBatch { instances, .. }) = batches.iter()
            .find(|b| matches!(b, BatchedDraw::GlyphBatch { .. })) {

            // Should have 2 glyphs (A and B)
            assert_eq!(instances.len(), 2, "Should have 2 glyphs for A\\nB");

            let glyph_a = &instances[0];
            let glyph_b = &instances[1];

            // A should be at start of first line
            assert!(glyph_a.x < 5.0, "A should be near start of line");

            // B should be at start of second line
            assert!(glyph_b.x < 5.0, "B should be near start of line");
            assert!(glyph_b.y > glyph_a.y, "B should be below A");

            // Line spacing should be reasonable
            let line_spacing = glyph_b.y - glyph_a.y;
            assert!(line_spacing > 15.0 && line_spacing < 50.0,
                    "Line spacing should be reasonable, got {:.1}", line_spacing);
        } else {
            panic!("Expected glyph batch");
        }
    }

    #[test]
    fn test_current_working_whitespace() {
        // Test space and tab handling
        let doc = Doc::from_str("A B\tC");
        let tree = doc.read();

        let mut renderer = Renderer::new((800.0, 600.0), 2.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        let batches = renderer.render(&tree, viewport);

        if let Some(BatchedDraw::GlyphBatch { instances, .. }) = batches.iter()
            .find(|b| matches!(b, BatchedDraw::GlyphBatch { .. })) {

            // Should have glyphs for A, space, B, tab, C
            assert!(instances.len() >= 3, "Should have at least A, B, C glyphs");

            // Find A, B, C positions
            let mut char_positions = Vec::new();
            for glyph in instances {
                if glyph.color != 0x00000000 { // Skip transparent chars
                    char_positions.push((glyph.x, glyph.y));
                }
            }

            if char_positions.len() >= 3 {
                // A, B, C should advance properly with whitespace
                assert!(char_positions[1].0 > char_positions[0].0, "B should be right of A (space)");
                assert!(char_positions[2].0 > char_positions[1].0, "C should be right of B (tab)");

                // Tab should create larger spacing than space
                let space_width = char_positions[1].0 - char_positions[0].0;
                let tab_width = char_positions[2].0 - char_positions[1].0;
                assert!(tab_width > space_width, "Tab should be wider than space");
            }
        }
    }

    #[test]
    fn test_font_system_actual_metrics() {
        // Document the actual font metrics our system produces
        let font_system = SharedFontSystem::new();

        // Test character widths
        let chars = ['A', 'B', 'i', 'W', ' '];

        for ch in chars {
            let layout = font_system.layout_text(&ch.to_string(), 14.0);
            if !layout.glyphs.is_empty() {
                let glyph = &layout.glyphs[0];
                assert!(glyph.width > 0.0, "Glyph '{}' should have width", ch);
                assert!(glyph.height > 0.0, "Glyph '{}' should have height", ch);
            }
        }

        // Test line height
        let multiline = font_system.layout_text("A\nB", 14.0);
        if multiline.glyphs.len() >= 2 {
            let line_height = multiline.glyphs[1].y - multiline.glyphs[0].y;
            assert!(line_height > 15.0 && line_height < 25.0,
                    "Line height should be reasonable for 14pt font");
        }

        // Test scale factor behavior
        let scaled = font_system.layout_text_scaled("A", 14.0, 2.0);
        if !scaled.glyphs.is_empty() {
            let glyph = &scaled.glyphs[0];
            assert!(glyph.width > 10.0, "Scaled glyph should have substantial width");
            assert!(glyph.height > 10.0, "Scaled glyph should have substantial height");
        }
    }
}