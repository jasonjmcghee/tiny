//! End-to-end pipeline integration tests
//!
//! Tests the complete document -> render -> GPU pipeline

use std::sync::Arc;
use tiny_editor::font::SharedFontSystem;
use tiny_editor::render::{BatchedDraw, Renderer};
use tiny_editor::tree::{Content, Doc, Edit, Rect};

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_editor::coordinates::LogicalPixels;

    #[test]
    fn test_single_character_pipeline() {
        // Complete pipeline: Doc -> Tree -> Render -> Batches
        let doc = Doc::from_str("A");
        let tree = doc.read();

        let mut renderer = Renderer::new((800.0, 600.0), 1.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(600.0),
        };

        let batches = renderer.render(&tree, viewport);

        assert!(!batches.is_empty(), "Should generate at least one batch");

        // Should have a glyph batch
        let glyph_batch = batches.iter().find_map(|batch| {
            if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                Some(instances)
            } else {
                None
            }
        });

        assert!(glyph_batch.is_some(), "Should have a glyph batch");

        let instances = glyph_batch.unwrap();
        assert_eq!(instances.len(), 1, "Should have exactly 1 glyph instance");

        let glyph = &instances[0];
        assert!(
            glyph.tex_coords[2] > glyph.tex_coords[0],
            "Should have valid texture coords"
        );
        assert_eq!(glyph.color, 0xFFFFFFFF, "Should be white by default");
    }

    #[test]
    fn test_typing_simulation() {
        // Simulate typing "Hello" character by character
        let doc = Doc::from_str("");

        for (i, ch) in "Hello".chars().enumerate() {
            doc.edit(Edit::Insert {
                pos: i,
                content: Content::Text(ch.to_string()),
            });
            doc.flush();

            // Test rendering after each character
            let tree = doc.read();
            let mut renderer = Renderer::new((800.0, 600.0), 1.0);
            let font_system = Arc::new(SharedFontSystem::new());
            renderer.set_font_system(font_system);

            let viewport = Rect {
                x: LogicalPixels(0.0),
                y: LogicalPixels(0.0),
                width: LogicalPixels(800.0),
                height: LogicalPixels(600.0),
            };

            let batches = renderer.render(&tree, viewport);

            // Should have glyph batch with correct number of characters
            let total_glyphs: usize = batches
                .iter()
                .map(|batch| {
                    if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                        instances.len()
                    } else {
                        0
                    }
                })
                .sum();

            assert_eq!(
                total_glyphs,
                i + 1,
                "Should render correct number of glyphs"
            );
        }
    }

    #[test]
    fn test_multiline_document_pipeline() {
        let doc = Doc::from_str("Line 1\nLine 2\nLine 3");
        let tree = doc.read();

        let mut renderer = Renderer::new((800.0, 600.0), 1.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(600.0),
        };

        let batches = renderer.render(&tree, viewport);

        // Count total glyphs across all batches
        let total_glyphs: usize = batches
            .iter()
            .map(|batch| {
                if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                    instances.len()
                } else {
                    0
                }
            })
            .sum();

        // Should render all characters (excluding newlines in display)
        assert_eq!(total_glyphs, 18, "Should render all visible characters");
    }

    #[test]
    fn test_viewport_scaling() {
        let doc = Doc::from_str("Test");
        let tree = doc.read();

        // Test at 1x scale
        let mut renderer_1x = Renderer::new((800.0, 600.0), 1.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer_1x.set_font_system(font_system.clone());

        // Test at 2x scale
        let mut renderer_2x = Renderer::new((800.0, 600.0), 2.0);
        renderer_2x.set_font_system(font_system);

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(600.0),
        };

        let batches_1x = renderer_1x.render(&tree, viewport);
        let batches_2x = renderer_2x.render(&tree, viewport);

        // Both should generate batches
        assert!(!batches_1x.is_empty());
        assert!(!batches_2x.is_empty());

        // Both should have same number of glyphs
        let glyphs_1x: usize = batches_1x
            .iter()
            .filter_map(|b| {
                if let BatchedDraw::GlyphBatch { instances, .. } = b {
                    Some(instances.len())
                } else {
                    None
                }
            })
            .sum();

        let glyphs_2x: usize = batches_2x
            .iter()
            .filter_map(|b| {
                if let BatchedDraw::GlyphBatch { instances, .. } = b {
                    Some(instances.len())
                } else {
                    None
                }
            })
            .sum();

        assert_eq!(glyphs_1x, glyphs_2x, "Should render same number of glyphs");
    }

    #[test]
    fn test_empty_document_pipeline() {
        let doc = Doc::from_str("");
        let tree = doc.read();

        let mut renderer = Renderer::new((800.0, 600.0), 1.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(600.0),
        };

        let batches = renderer.render(&tree, viewport);

        // Empty document should still generate some batches (background, etc.)
        // but no glyph instances
        let total_glyphs: usize = batches
            .iter()
            .map(|batch| {
                if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                    instances.len()
                } else {
                    0
                }
            })
            .sum();

        assert_eq!(total_glyphs, 0, "Empty document should have no glyphs");
    }

    #[test]
    fn test_document_with_widgets() {
        let doc = Doc::from_str("Text");

        // Add cursor widget
        doc.edit(Edit::Insert {
            pos: 2,
            content: Content::Widget(tiny_editor::widget::cursor()),
        });
        doc.flush();

        let tree = doc.read();
        let mut renderer = Renderer::new((800.0, 600.0), 1.0);
        let font_system = Arc::new(SharedFontSystem::new());
        renderer.set_font_system(font_system);

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(600.0),
        };

        let batches = renderer.render(&tree, viewport);
        assert!(!batches.is_empty(), "Should render text and widget");

        // Should have both glyph and rect batches
        let has_glyphs = batches
            .iter()
            .any(|b| matches!(b, BatchedDraw::GlyphBatch { .. }));
        let has_rects = batches
            .iter()
            .any(|b| matches!(b, BatchedDraw::RectBatch { .. }));

        assert!(has_glyphs, "Should have text glyphs");
        assert!(has_rects, "Should have cursor rect");
    }
}
