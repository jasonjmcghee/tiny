//! Test for syntax highlighting stability during edits
//!
//! This test demonstrates the bug where typing between syntax highlights
//! causes text to lose color temporarily until tree-sitter catches up.

use tiny_core::tree::{Content, Doc, Edit};
use tiny_editor::{
    coordinates::Viewport,
    text_renderer::{TextRenderer, TokenRange},
};
use tiny_font::SharedFontSystem;

#[test]
fn test_syntax_stability_when_typing_between_highlights() {
    // Setup: Create a simple document with syntax highlighting
    let doc = Doc::from_str("fn main");
    let tree = doc.read();

    // Create renderer and font system
    let mut renderer = TextRenderer::new();
    let font_system = SharedFontSystem::new();

    // Create viewport
    let viewport = Viewport::new(800.0, 600.0, 1.0);

    // Initial layout
    renderer.update_layout(&tree, &font_system, &viewport);

    // Apply initial syntax highlighting:
    // "fn" (bytes 0-2) = keyword (token_id 1)
    // "main" (bytes 3-7) = function (token_id 2)
    let initial_tokens = vec![
        TokenRange {
            byte_range: 0..2,
            token_id: 1, // keyword
        },
        TokenRange {
            byte_range: 3..7,
            token_id: 2, // function
        },
    ];

    renderer.update_syntax(&initial_tokens, true);

    // Verify initial highlighting is correct
    assert_eq!(renderer.layout_cache[0].char, 'f');
    assert_eq!(
        renderer.layout_cache[0].token_id, 1,
        "Initial 'f' should be keyword (1)"
    );
    assert_eq!(renderer.layout_cache[1].char, 'n');
    assert_eq!(
        renderer.layout_cache[1].token_id, 1,
        "Initial 'n' should be keyword (1)"
    );
    assert_eq!(renderer.layout_cache[2].char, ' ');
    assert_eq!(
        renderer.layout_cache[2].token_id, 0,
        "Space should be unstyled (0)"
    );
    assert_eq!(renderer.layout_cache[3].char, 'm');
    assert_eq!(
        renderer.layout_cache[3].token_id, 2,
        "Initial 'm' should be function (2)"
    );

    // Simulate user typing 'x' at position 1: "fn main" -> "fxn main"
    drop(tree);
    doc.edit(Edit::Insert {
        pos: 1,
        content: Content::Text("x".to_string()),
    });
    doc.flush();

    let tree_after_edit = doc.read();
    assert_eq!(tree_after_edit.flatten_to_string().as_str(), "fxn main");

    // Layout rebuilds (clears all tokens to 0)
    renderer.update_layout(&tree_after_edit, &font_system, &viewport);

    // Verify layout cleared tokens
    assert_eq!(renderer.layout_cache[0].char, 'f');
    assert_eq!(
        renderer.layout_cache[0].token_id, 0,
        "After layout rebuild, tokens are cleared"
    );

    // Simulate that an edit happened - track it in the renderer
    // In real code, apply_incremental_edit would be called during flush_pending_edits
    use tiny_core::tree::Edit as TreeEdit;
    renderer.apply_incremental_edit(&TreeEdit::Insert {
        pos: 1,
        content: Content::Text("x".to_string()),
    });

    // Now apply OLD syntax tokens (tree-sitter hasn't caught up yet)
    // The old tokens are still for "fn main" (0..2, 3..7)
    // But the document is now "fxn main" where bytes are:
    // 'f' = 0, 'x' = 1, 'n' = 2, ' ' = 3, 'm' = 4, 'a' = 5, 'i' = 6, 'n' = 7
    renderer.update_syntax(&initial_tokens, false);

    // THIS IS WHERE THE BUG MANIFESTS:
    // We want the text to stay stable - keep old colors even though positions shifted
    //
    // What we WANT (stable highlighting):
    // - 'f' at byte 0: should keep keyword color (token_id 1) from old "fn"
    // - 'x' at byte 1: should infer keyword color (token_id 1) from context
    // - 'n' at byte 2: should keep keyword color (token_id 1) from old "fn"
    // - ' ' at byte 3: unstyled
    // - 'm' at byte 4: should keep function color (token_id 2) from old "main"
    //
    // What we GET (buggy behavior):
    // - 'f' at byte 0: gets token from range 0..2, so token_id 1 ✓
    // - 'x' at byte 1: gets token from range 0..2, so token_id 1 ✓
    // - 'n' at byte 2: outside range 0..2, before range 3..7, so token_id 0 ✗ (WRONG!)
    // - ' ' at byte 3: gets token from range 3..7, so token_id 2 ✗ (WRONG!)
    // - 'm' at byte 4: gets token from range 3..7, so token_id 2 ✓

    println!("After applying old tokens to shifted text:");
    for (i, glyph) in renderer.layout_cache.iter().enumerate() {
        println!(
            "  [{}] char={:?} byte={} token_id={}",
            i, glyph.char, glyph.char_byte_offset, glyph.token_id
        );
    }

    // The test that SHOULD pass but currently FAILS:
    // We want stable highlighting where colors don't flicker
    assert_eq!(renderer.layout_cache[0].char, 'f');
    assert_eq!(
        renderer.layout_cache[0].token_id, 1,
        "'f' should keep keyword color"
    );

    assert_eq!(renderer.layout_cache[1].char, 'x');
    assert_eq!(
        renderer.layout_cache[1].token_id, 1,
        "'x' should infer keyword color from context"
    );

    assert_eq!(renderer.layout_cache[2].char, 'n');
    assert_eq!(
        renderer.layout_cache[2].token_id, 1,
        "'n' should keep keyword color - THIS FAILS!"
    );

    assert_eq!(renderer.layout_cache[3].char, ' ');
    assert_eq!(
        renderer.layout_cache[3].token_id, 0,
        "space should stay unstyled"
    );

    assert_eq!(renderer.layout_cache[4].char, 'm');
    assert_eq!(
        renderer.layout_cache[4].token_id, 2,
        "'m' should keep function color"
    );
}
