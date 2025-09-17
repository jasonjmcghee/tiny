//! Debug the widget paint system to find where positioning breaks

use tiny_editor::{
    font::SharedFontSystem,
    widget::text,
    tree::PaintContext,
    render::RenderOp,
    coordinates::{Viewport, LayoutPos},
};
use std::sync::Arc;

fn main() {
    println!("🎨 Widget Paint System Debug");
    println!("=============================");

    let font_system = Arc::new(SharedFontSystem::new());
    let viewport = Viewport::new(800.0, 600.0, 1.0);

    // Test 1: Single character widget
    println!("\n--- Single Character Widget ---");
    let widget = text("A");

    let mut commands = Vec::new();
    let mut ctx = PaintContext {
        layout_pos: LayoutPos { x: 10.0, y: 20.0 },
        view_pos: Some(viewport.layout_to_view(LayoutPos { x: 10.0, y: 20.0 })),
        doc_pos: None,
        commands: &mut commands,
        text_styles: None,
        font_system: Some(&font_system),
        viewport: &viewport,
    };

    widget.paint(&mut ctx);

    println!("Generated {} render commands", commands.len());

    for (i, cmd) in commands.iter().enumerate() {
        match cmd {
            RenderOp::Glyphs { glyphs, .. } => {
                println!("  Command {}: {} glyphs", i, glyphs.len());
                for (j, glyph) in glyphs.iter().enumerate() {
                    println!("    Glyph {}: pos=({:.1}, {:.1})", j, glyph.x, glyph.y);
                }
            }
            _ => println!("  Command {}: Non-glyph", i),
        }
    }

    // Test 2: Multi-character widget - THE CRITICAL TEST
    println!("\n--- Multi-Character Widget (Critical) ---");
    let widget = text("AB");

    let mut commands = Vec::new();
    let mut ctx = PaintContext {
        layout_pos: LayoutPos { x: 0.0, y: 0.0 },
        view_pos: Some(viewport.layout_to_view(LayoutPos { x: 0.0, y: 0.0 })),
        doc_pos: None,
        commands: &mut commands,
        text_styles: None,
        font_system: Some(&font_system),
        viewport: &viewport,
    };

    widget.paint(&mut ctx);

    println!("Generated {} render commands", commands.len());

    for (i, cmd) in commands.iter().enumerate() {
        match cmd {
            RenderOp::Glyphs { glyphs, .. } => {
                println!("  Command {}: {} glyphs", i, glyphs.len());

                if glyphs.len() >= 2 {
                    let glyph_a = &glyphs[0];
                    let glyph_b = &glyphs[1];

                    println!("    Glyph A: pos=({:.1}, {:.1})", glyph_a.x, glyph_a.y);
                    println!("    Glyph B: pos=({:.1}, {:.1})", glyph_b.x, glyph_b.y);

                    if glyph_a.x == glyph_b.x {
                        println!("    🐛 WIDGET BUG: Both glyphs at same X!");
                    } else {
                        println!("    ✅ Widget glyphs advance properly");
                    }
                }
            }
            _ => println!("  Command {}: Non-glyph", i),
        }
    }

    // Test 3: Multiple widget test
    println!("\n--- Multiple Character Widget ---");
    let widget = text("Hello");

    let mut commands = Vec::new();
    let mut ctx = PaintContext {
        layout_pos: LayoutPos { x: 50.0, y: 100.0 },
        view_pos: Some(viewport.layout_to_view(LayoutPos { x: 50.0, y: 100.0 })),
        doc_pos: None,
        commands: &mut commands,
        text_styles: None,
        font_system: Some(&font_system),
        viewport: &viewport,
    };

    widget.paint(&mut ctx);

    for cmd in &commands {
        if let RenderOp::Glyphs { glyphs, .. } = cmd {
            println!("'Hello' widget -> {} glyphs", glyphs.len());

            for (i, glyph) in glyphs.iter().enumerate() {
                println!("  [{}] pos=({:.1}, {:.1})", i, glyph.x, glyph.y);
            }

            // Check if all at same position
            if glyphs.len() > 1 {
                let first_x = glyphs[0].x;
                let all_same = glyphs.iter().all(|g| g.x == first_x);

                if all_same {
                    println!("  🚨 WIDGET BUG: All glyphs at X={:.1}", first_x);
                } else {
                    println!("  ✅ Widget glyphs have different positions");
                }
            }
        }
    }
}