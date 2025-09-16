//! Debug the complete document-to-render pipeline to find where positioning breaks

use tiny_editor::{
    font::SharedFontSystem,
    render::{BatchedDraw, Renderer},
    tree::{Content, Doc, Edit, Rect},
};
use std::sync::Arc;

fn main() {
    println!("🔄 Complete Pipeline Debug");
    println!("===========================");

    // Create document with single character
    let doc = Doc::from_str("");
    doc.edit(Edit::Insert {
        pos: 0,
        content: Content::Text("A".to_string()),
    });
    doc.flush();

    let tree = doc.read();
    println!("Document text: '{}'", tree.to_string());

    let viewport = Rect {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 400.0,
    };

    // Set up renderer
    let mut renderer = Renderer::new((viewport.x, viewport.y), 1.0);
    let font_system = Arc::new(SharedFontSystem::new());
    font_system.prerasterize_ascii(14.0);
    renderer.set_font_system(font_system.clone());



    println!("Rendering with viewport: {:?}", viewport);

    // CRITICAL TEST: Render document
    let batches = renderer.render(&tree, viewport);
    println!("Generated {} render batches", batches.len());

    for (i, batch) in batches.iter().enumerate() {
        match batch {
            BatchedDraw::GlyphBatch { instances, .. } => {
                println!("  Batch {}: {} glyphs", i, instances.len());
                for (j, glyph) in instances.iter().enumerate() {
                    println!("    Glyph {}: pos=({:.1}, {:.1}) color=0x{:08X}",
                             j, glyph.x, glyph.y, glyph.color);
                }
            }
            BatchedDraw::RectBatch { instances } => {
                println!("  Batch {}: {} rectangles", i, instances.len());
            }
            _ => {
                println!("  Batch {}: Other type", i);
            }
        }
    }

    // Test 2: Two character document - THE CRITICAL TEST
    println!("\n--- Two Character Pipeline Test ---");

    let doc2 = Doc::from_str("AB");
    let tree2 = doc2.read();
    println!("Document text: '{}'", tree2.to_string());

    let batches2 = renderer.render(&tree2, viewport);
    println!("Generated {} render batches", batches2.len());

    for (i, batch) in batches2.iter().enumerate() {
        match batch {
            BatchedDraw::GlyphBatch { instances, .. } => {
                println!("  Batch {}: {} glyphs", i, instances.len());

                if instances.len() >= 2 {
                    let glyph_a = &instances[0];
                    let glyph_b = &instances[1];

                    println!("    Glyph A: pos=({:.1}, {:.1})", glyph_a.x, glyph_a.y);
                    println!("    Glyph B: pos=({:.1}, {:.1})", glyph_b.x, glyph_b.y);

                    if glyph_a.x == glyph_b.x {
                        println!("    🐛 PIPELINE BUG: Both glyphs at same X!");
                    } else {
                        println!("    ✅ Pipeline glyphs advance properly");
                    }
                }
            }
            _ => {}
        }
    }

    // Test 3: Typing simulation
    println!("\n--- Typing Simulation ---");
    let doc3 = Doc::from_str("");

    for (i, ch) in "asd".chars().enumerate() {
        doc3.edit(Edit::Insert {
            pos: i,
            content: Content::Text(ch.to_string()),
        });
        doc3.flush();

        let tree = doc3.read();
        println!("\nAfter typing '{}': document = '{}'", ch, tree.to_string());

        let batches = renderer.render(&tree, viewport);

        for batch in &batches {
            if let BatchedDraw::GlyphBatch { instances, .. } = batch {
                println!("  Rendered {} glyphs:", instances.len());

                if !instances.is_empty() {
                    let first = &instances[0];
                    println!("    First glyph: pos=({:.1}, {:.1})", first.x, first.y);

                    // Check if all glyphs are at same position (matches your debug output)
                    if instances.len() > 1 {
                        let all_same_x = instances.iter().all(|g| g.x == first.x);
                        if all_same_x {
                            println!("    🚨 FOUND THE BUG: All {} glyphs at same X position: {:.1}",
                                   instances.len(), first.x);
                        } else {
                            println!("    ✅ Glyphs at different X positions");
                        }
                    }
                }
                break;
            }
        }
    }
}