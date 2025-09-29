//! Test undo/redo with actual InputHandler to verify syntax updates

use tiny_core::tree::{Doc, Edit, Content};
use tiny_editor::{
    text_renderer::{TextRenderer, TokenRange},
    coordinates::Viewport,
    input::InputHandler,
    syntax::SyntaxHighlighter,
};
use tiny_font::SharedFontSystem;
use std::sync::Arc;

#[test]
fn test_undo_triggers_syntax_reparse() {
    let doc = Doc::from_str("fn main() {}");
    let mut input_handler = InputHandler::new();
    let font_system = SharedFontSystem::new();

    // Create a syntax highlighter
    let syntax_hl = Arc::new(SyntaxHighlighter::new_rust());
    input_handler.set_syntax_highlighter(syntax_hl.clone());

    // Initial state - trigger first parse
    let initial_text = doc.read().flatten_to_string();
    syntax_hl.request_update_with_edit(&initial_text, doc.version(), None);

    // Wait for parse to complete
    std::thread::sleep(std::time::Duration::from_millis(150));

    let version_before = syntax_hl.cached_version();
    println!("Initial cached_version: {}", version_before);

    // Make an edit through input_handler (saves to history)
    input_handler.set_cursor_for_test(tiny_sdk::DocPos { line: 0, column: 0, byte_offset: 0 });
    use tiny_editor::input::Selection;
    input_handler.selections_mut_for_test().clear();
    input_handler.selections_mut_for_test().push(Selection {
        cursor: tiny_sdk::DocPos { line: 0, column: 0, byte_offset: 0 },
        anchor: tiny_sdk::DocPos { line: 0, column: 0, byte_offset: 0 },
        id: 0,
    });

    input_handler.pending_edits_mut_for_test().push(Edit::Insert {
        pos: 0,
        content: Content::Text("x".to_string()),
    });

    input_handler.flush_pending_edits(&doc);

    let version_after_edit = doc.version();
    println!("Doc version after edit: {}", version_after_edit);

    // CALL UNDO - this should request syntax update
    let undo_result = input_handler.undo(&doc);

    println!("Undo result: {}", undo_result);
    println!("Doc version after undo: {}", doc.version());
    println!("Doc text after undo: {:?}", doc.read().flatten_to_string().as_str());

    assert!(undo_result, "Undo should succeed");

    // Wait for syntax parse triggered by undo
    std::thread::sleep(std::time::Duration::from_millis(150));

    let version_after_undo = syntax_hl.cached_version();
    println!("Cached version after undo: {}", version_after_undo);

    // This is the BUG: cached_version should update after undo triggers reparse
    // If it doesn't change, tree-sitter never reparsed!
    assert_ne!(version_after_undo, version_before,
        "Syntax cached_version should update after undo triggers reparse");
}

#[test]
fn test_token_adjustment_with_edit_deltas() {
    let doc = Doc::from_str("fn main");
    let mut renderer = TextRenderer::new();
    let font_system = SharedFontSystem::new();
    let viewport = Viewport::new(800.0, 600.0, 1.0);

    // Setup initial state
    let tree = doc.read();
    renderer.update_layout(&tree, &font_system, &viewport);
    drop(tree);

    let initial_tokens = vec![
        TokenRange { byte_range: 0..2, token_id: 1 },   // "fn"
        TokenRange { byte_range: 3..7, token_id: 2 },   // "main"
    ];
    renderer.update_syntax(&initial_tokens, true);

    // Apply an edit: insert 'x' at position 1
    renderer.apply_incremental_edit(&Edit::Insert {
        pos: 1,
        content: Content::Text("x".to_string()),
    });

    // Apply the edit to doc
    doc.edit(Edit::Insert { pos: 1, content: Content::Text("x".to_string()) });
    doc.flush();

    // Update layout
    let tree = doc.read();
    renderer.update_layout(&tree, &font_system, &viewport);
    drop(tree);

    // Apply stable tokens with adjustment (fresh_parse=false)
    let stable_copy: Vec<_> = renderer.syntax_state.stable_tokens
        .iter()
        .map(|t| TokenRange { byte_range: t.byte_range.clone(), token_id: t.token_id })
        .collect();

    renderer.update_syntax(&stable_copy, false);

    // Verify adjusted tokens are applied correctly
    // Token 0..2 becomes 0..3 (insert at 1 expands it)
    // Token 3..7 becomes 4..8 (insert before it shifts it)
    assert_eq!(renderer.layout_cache[0].char, 'f');
    assert_eq!(renderer.layout_cache[0].token_id, 1, "'f' should keep keyword");
    assert_eq!(renderer.layout_cache[1].char, 'x');
    assert_eq!(renderer.layout_cache[1].token_id, 1, "'x' should get keyword (expanded range)");
    assert_eq!(renderer.layout_cache[2].char, 'n');
    assert_eq!(renderer.layout_cache[2].token_id, 1, "'n' should keep keyword (expanded range)");
    assert_eq!(renderer.layout_cache[4].char, 'm');
    assert_eq!(renderer.layout_cache[4].token_id, 2, "'m' should keep function (shifted range)");
}