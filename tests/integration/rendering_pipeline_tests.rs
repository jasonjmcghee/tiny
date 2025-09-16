//! Integration tests for the rendering pipeline
//!
//! Tests that multiple components work together correctly

use tiny_editor::{
    font::SharedFontSystem,
    render::{BatchedDraw, Renderer},
    tree::{Content, Doc, Edit, Rect},
};
use std::sync::Arc;

#[test]
fn test_document_to_render_pipeline() {
    // Complete pipeline: Doc → Tree → Renderer → Batches
    let doc = Doc::from_str("ABC");
    let tree = doc.read();

    let mut renderer = Renderer::new((800.0, 600.0), 1.0);
    let font_system = Arc::new(SharedFontSystem::new());
    renderer.set_font_system(font_system);
    renderer.update_viewport(800.0, 600.0, 1.0);

    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };

    let batches = renderer.render(&tree, viewport);

    // Should generate glyph batch
    assert!(!batches.is_empty());

    let total_glyphs: usize = batches
        .iter()
        .filter_map(|batch| match batch {
            BatchedDraw::GlyphBatch { instances, .. } => Some(instances.len()),
            _ => None,
        })
        .sum();

    assert_eq!(total_glyphs, 3, "Should render 3 glyphs for 'ABC'");
}

#[test]
fn test_incremental_typing_render() {
    let doc = Doc::from_str("");
    let mut renderer = Renderer::new((800.0, 600.0), 1.0);
    let font_system = Arc::new(SharedFontSystem::new());
    renderer.set_font_system(font_system);

    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };

    // Type and render each character
    for (i, ch) in "Test".chars().enumerate() {
        doc.edit(Edit::Insert {
            pos: i,
            content: Content::Text(ch.to_string()),
        });
        doc.flush();

        let batches = renderer.render(&doc.read(), viewport);

        let glyph_count: usize = batches
            .iter()
            .filter_map(|b| match b {
                BatchedDraw::GlyphBatch { instances, .. } => Some(instances.len()),
                _ => None,
            })
            .sum();

        assert_eq!(glyph_count, i + 1, "Should render {} glyphs", i + 1);
    }
}

#[test]
fn test_multiline_render() {
    let doc = Doc::from_str("Line1\nLine2\nLine3");
    let tree = doc.read();

    let mut renderer = Renderer::new((800.0, 600.0), 1.0);
    let font_system = Arc::new(SharedFontSystem::new());
    renderer.set_font_system(font_system);

    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };

    let batches = renderer.render(&tree, viewport);

    let total_glyphs: usize = batches
        .iter()
        .filter_map(|b| match b {
            BatchedDraw::GlyphBatch { instances, .. } => Some(instances.len()),
            _ => None,
        })
        .sum();

    // 5 + 5 + 5 = 15 characters (excluding newlines in render)
    assert_eq!(total_glyphs, 15);
}

#[test]
fn test_glyph_batch_properties() {
    let doc = Doc::from_str("Hi");
    let tree = doc.read();

    let mut renderer = Renderer::new((800.0, 600.0), 1.0);
    let font_system = Arc::new(SharedFontSystem::new());
    renderer.set_font_system(font_system.clone());

    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };

    let batches = renderer.render(&tree, viewport);

    // Find glyph batch
    for batch in batches {
        if let BatchedDraw::GlyphBatch { instances, .. } = batch {
            assert_eq!(instances.len(), 2);

            for glyph in &instances {
                // Check texture coordinates are valid
                assert!(glyph.tex_coords[0] < glyph.tex_coords[2]); // u0 < u1
                assert!(glyph.tex_coords[1] < glyph.tex_coords[3]); // v0 < v1
            }

            // H and i should have different x positions
            assert_ne!(instances[0].x, instances[1].x);
            break;
        }
    }
}