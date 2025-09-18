//! Test that tree-sitter incremental parsing works correctly with InputEdit

use std::sync::Arc;
use std::time::Duration;
use tiny_editor::{
    coordinates::DocPos,
    input::InputHandler,
    syntax::{create_text_edit, Languages, SyntaxHighlighter},
    text_effects::TextStyleProvider,
    tree::{Content, Doc, Edit},
};
use winit::event::{ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, ModifiersState};

fn main() {
    println!("Testing tree-sitter incremental parsing with InputEdit...\n");

    // Create initial document
    let initial_text = r#"fn main() {
    let x = 42;
    println!("Hello, {}", x);
}"#;

    let doc = Doc::from_str(initial_text);
    println!("Initial document ({} bytes):\n{}\n", initial_text.len(), initial_text);

    // Create syntax highlighter with proper type
    let highlighter = Arc::new(SyntaxHighlighter::new(Languages::rust()).unwrap());

    // Initial parse
    highlighter.request_update(initial_text, doc.version());
    std::thread::sleep(Duration::from_millis(100));

    // Get initial effects to compare
    let effects_before = highlighter.get_visible_effects(initial_text, 0..initial_text.len());
    println!("Before edit: {} syntax effects", effects_before.len());

    // Show some key token positions
    for effect in effects_before.iter().take(5) {
        let text_slice = &initial_text[effect.range.clone()];
        println!("  '{}' at {}..{}", text_slice, effect.range.start, effect.range.end);
    }

    // Create an edit: insert 's' after 'let' (position 19)
    println!("\n=== Applying Edit: Insert 's' at position 19 ===");

    let tree_before = doc.read();
    let edit = Edit::Insert {
        pos: 19, // After "let"
        content: Content::Text("s".to_string()),
    };

    // Create TextEdit using our new helper
    let text_edit = create_text_edit(&tree_before, &edit);
    println!("Created TextEdit: start_byte={}, old_end={}, new_end={}",
             text_edit.start_byte, text_edit.old_end_byte, text_edit.new_end_byte);
    println!("  start_position: line={}, col={}",
             text_edit.start_position.row, text_edit.start_position.column);

    // Apply the edit to the document
    doc.edit(edit);
    doc.flush();

    let text_after = doc.read().to_string();
    println!("\nAfter edit ({} bytes):\n{}\n", text_after.len(), text_after);

    // Update syntax highlighter with edit information
    highlighter.request_update_with_edit(&text_after, doc.version(), Some(text_edit));
    std::thread::sleep(Duration::from_millis(100));

    // Get effects after edit
    let effects_after = highlighter.get_visible_effects(&text_after, 0..text_after.len());
    println!("After edit: {} syntax effects", effects_after.len());

    // Verify that syntax highlighting is correct
    let mut correct_highlighting = true;
    for effect in effects_after.iter().take(8) {
        let text_slice = &text_after[effect.range.clone()];
        println!("  '{}' at {}..{}", text_slice, effect.range.start, effect.range.end);

        // Check that the highlighted text makes sense
        if text_slice.is_empty() || effect.range.start >= effect.range.end {
            correct_highlighting = false;
            println!("    ❌ Invalid range or empty text");
        }
    }

    // Test 2: Multiple rapid edits to stress test incremental parsing
    println!("\n=== Testing Multiple Rapid Edits ===");

    for i in 0..5 {
        let tree_before = doc.read();
        let edit = Edit::Insert {
            pos: 30 + i, // Insert at different positions
            content: Content::Text(format!("{}", i)),
        };

        let text_edit = create_text_edit(&tree_before, &edit);
        doc.edit(edit);
        doc.flush();

        let text_current = doc.read().to_string();
        highlighter.request_update_with_edit(&text_current, doc.version(), Some(text_edit));
    }

    std::thread::sleep(Duration::from_millis(200)); // Let all edits settle

    let final_text = doc.read().to_string();
    let final_effects = highlighter.get_visible_effects(&final_text, 0..final_text.len());

    println!("Final document:\n{}", final_text);
    println!("Final effects: {}", final_effects.len());

    if correct_highlighting && final_effects.len() > 0 {
        println!("\n✅ Tree-sitter incremental parsing with InputEdit is working!");
    } else {
        println!("\n❌ Issues detected with incremental parsing");
    }
}