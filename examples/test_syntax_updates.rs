//! Test that syntax highlighting updates correctly after edits

use std::sync::Arc;
use std::time::Duration;
use tiny_editor::{
    syntax::{Languages, SyntaxHighlighter},
    text_effects::TextStyleProvider,
    tree::{Content, Doc, Edit},
};

fn main() {
    println!("Testing syntax highlighting updates after edits...\n");

    // Create initial document
    let initial_text = r#"fn main() {
    let x = 42;
    println!("x = {}", x);
}"#;

    let doc = Doc::from_str(initial_text);
    println!("Initial document:\n{}\n", initial_text);

    // Create syntax highlighter
    let highlighter = Arc::new(SyntaxHighlighter::new(Languages::rust()).unwrap());

    // Initial parse
    highlighter.request_update(initial_text, doc.version());
    std::thread::sleep(Duration::from_millis(100));

    // Get initial effects
    let effects1 = highlighter.get_visible_effects(initial_text, 0..initial_text.len());
    println!("Initial parse: {} effects", effects1.len());
    if let Some(first) = effects1.first() {
        println!("  First effect: {:?}", first);
    }

    // Now edit the document - insert a character
    println!("\nInserting 's' after 'let' to make 'lets'...");
    doc.edit(Edit::Insert {
        pos: 19, // After "let"
        content: Content::Text("s".to_string()),
    });
    doc.flush();

    let edited_text = doc.read().to_string();
    println!("Edited document:\n{}\n", edited_text);

    // Update syntax highlighter with new text
    highlighter.request_update(&edited_text, doc.version());
    std::thread::sleep(Duration::from_millis(100));

    // Get updated effects
    let effects2 = highlighter.get_visible_effects(&edited_text, 0..edited_text.len());
    println!("After edit: {} effects", effects2.len());

    // Check that effects have shifted correctly
    let mut correctly_shifted = true;
    for (i, effect) in effects2.iter().enumerate() {
        if effect.range.start > 19 {
            // Effects after the edit should be shifted by 1
            if let Some(orig) = effects1.iter().find(|e| e.range.start == effect.range.start - 1) {
                if effect.range.end != orig.range.end + 1 {
                    correctly_shifted = false;
                    println!("  ❌ Effect {} not shifted correctly", i);
                }
            }
        }
    }

    if correctly_shifted {
        println!("✓ Effects shifted correctly after edit");
    }

    // Test viewport query with edited document
    println!("\nTesting viewport query on edited document...");
    let viewport_effects = highlighter.get_visible_effects(&edited_text, 10..30);
    println!("Viewport (bytes 10-30): {} effects", viewport_effects.len());
    for effect in viewport_effects.iter().take(3) {
        println!("  Effect: {:?}", effect);
    }

    // Test effect coalescing
    println!("\nTesting effect coalescing...");
    use tiny_editor::text_effects::{TextEffect, EffectType, priority};
    let test_effects = vec![
        TextEffect {
            range: 0..5,
            effect: EffectType::Color(0xFF0000FF),
            priority: priority::SYNTAX,
        },
        TextEffect {
            range: 5..10,
            effect: EffectType::Color(0xFF0000FF), // Same color
            priority: priority::SYNTAX,
        },
        TextEffect {
            range: 10..15,
            effect: EffectType::Color(0x00FF00FF), // Different color
            priority: priority::SYNTAX,
        },
    ];

    let coalesced = SyntaxHighlighter::coalesce_effects(test_effects);
    println!("Coalesced {} effects into {} effects", 3, coalesced.len());
    assert_eq!(coalesced.len(), 2, "Should coalesce adjacent same-color effects");
    assert_eq!(coalesced[0].range, 0..10, "First two should be merged");

    println!("\n✓ All tests passed!");
}