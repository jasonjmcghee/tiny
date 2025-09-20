//! Test decoupled rendering architecture
//!
//! Demonstrates layout/style separation and incremental highlighting

use tiny_editor::{
    coordinates::Viewport,
    font::SharedFontSystem,
    syntax::TokenType,
    text_renderer::{TextRenderer, TokenRange, token_type_to_id},
    tree::{Doc, Edit, Content},
};

fn main() {
    println!("Testing decoupled text rendering architecture\n");

    // Create components
    let font_system = std::sync::Arc::new(SharedFontSystem::new());
    let mut viewport = Viewport::new(800.0, 600.0, 2.0);
    viewport.set_font_system(font_system.clone());
    let mut text_renderer = TextRenderer::new();

    // Create document with some Rust code
    let doc = Doc::from_str(r#"fn main() {
    let x = 42;
    println!("Hello, {}", x);
}"#);

    let tree = doc.read();

    // === STEP 1: Build layout cache (positions only) ===
    println!("1. Building layout cache...");
    text_renderer.update_layout(&tree, &*font_system, &viewport);
    println!("   Layout cache: {} glyphs", text_renderer.layout_cache.len());
    println!("   Line cache: {} lines", text_renderer.line_cache.len());

    // === STEP 2: Apply syntax highlighting (style only) ===
    println!("\n2. Applying syntax highlighting...");

    // Simulate tree-sitter tokens
    let tokens = vec![
        TokenRange {
            byte_range: 0..2,   // "fn"
            token_id: token_type_to_id(TokenType::Keyword),
        },
        TokenRange {
            byte_range: 3..7,   // "main"
            token_id: token_type_to_id(TokenType::Function),
        },
        TokenRange {
            byte_range: 16..19, // "let"
            token_id: token_type_to_id(TokenType::Keyword),
        },
        TokenRange {
            byte_range: 20..21, // "x"
            token_id: token_type_to_id(TokenType::Variable),
        },
        TokenRange {
            byte_range: 24..26, // "42"
            token_id: token_type_to_id(TokenType::Number),
        },
        TokenRange {
            byte_range: 32..39, // "println"
            token_id: token_type_to_id(TokenType::Function),
        },
        TokenRange {
            byte_range: 41..48, // "Hello,"
            token_id: token_type_to_id(TokenType::String),
        },
    ];

    text_renderer.update_syntax(&tokens);
    println!("   Style buffer: {} entries", text_renderer.style_buffer.len());

    // === STEP 3: Simulate incremental edit (typing) ===
    println!("\n3. Simulating incremental edit (typing '// comment')...");

    // User types a comment at the end
    let edit = Edit::Insert {
        pos: tree.byte_count(),
        content: Content::Text("\n// comment".to_string()),
    };

    text_renderer.apply_incremental_edit(&edit);
    println!("   Added {} incremental tokens", text_renderer.syntax_state.incremental_tokens.len());

    // The new text inherits the context (should be treated as comment)
    if let Some(incremental) = text_renderer.syntax_state.incremental_tokens.first() {
        println!("   New text token ID: {} (inherited from context)", incremental.token_id);
    }

    // === STEP 4: Demonstrate culling ===
    println!("\n4. Testing culling...");
    text_renderer.update_visible_range(&viewport, &tree);
    println!("   Visible lines: {:?}", text_renderer.visible_lines);
    println!("   Visible chars: {} out of {}",
             text_renderer.visible_chars.len(),
             text_renderer.layout_cache.len());

    // === STEP 5: Get visible glyphs for rendering ===
    println!("\n5. Getting visible glyphs with styles...");
    let visible_glyphs = text_renderer.get_visible_glyphs();

    // Print first few glyphs with their styles
    for (i, (glyph, token_id)) in visible_glyphs.iter().take(10).enumerate() {
        let token_name = match token_id {
            1 => "Keyword",
            2 => "Function",
            3 => "Type",
            4 => "String",
            5 => "Number",
            6 => "Comment",
            7 => "Operator",
            8 => "Punctuation",
            9 => "Attribute",
            _ => "Default",
        };

        println!("   Glyph {}: '{}' at ({:.1}, {:.1}) - Style: {}",
                 i,
                 glyph.char,
                 glyph.layout_pos.x.0,
                 glyph.layout_pos.y.0,
                 token_name);
    }

    println!("\n=== Key Benefits ===");
    println!("1. Layout cache unchanged when syntax updates");
    println!("2. Style buffer unchanged when scrolling (just culling)");
    println!("3. Incremental tokens provide stable appearance while parsing");
    println!("4. Palette texture enables instant theme switching");
    println!("5. GPU can batch by token ID for efficient rendering");

    println!("\n=== Memory Layout ===");
    println!("Layout cache: {} bytes",
             text_renderer.layout_cache.len() * std::mem::size_of::<tiny_editor::text_renderer::GlyphPosition>());
    println!("Style buffer: {} bytes",
             text_renderer.style_buffer.len() * 2);
    println!("Total: Independent updates, no redundant recalculation!");
}