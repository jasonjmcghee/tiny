use tiny_editor::syntax::*;
use tiny_editor::text_effects::TextStyleProvider;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, WasmStore};

#[test]
fn test_rust_highlighting() {
    let highlighter = SyntaxHighlighter::new_rust();

    // Test basic interface
    assert_eq!(highlighter.name(), "rust");

    // Test range query (should return empty initially)
    let effects = highlighter.get_effects_in_range(0..100);
    assert_eq!(effects.len(), 0); // Empty until background parsing completes

    // Test update request (should not crash)
    highlighter.request_update("fn main() {}", 1);
}

#[test]
fn test_language_agnostic() {
    // Test that we can create a highlighter with any language config
    let config = Languages::rust();
    let highlighter = SyntaxHighlighter::new(config);
    assert!(highlighter.is_ok());
}

#[test]
fn test_debounced_updates() {
    let highlighter = SyntaxHighlighter::new_rust();

    // Multiple rapid updates should be debounced
    highlighter.request_update("fn main() {", 1);
    highlighter.request_update("fn main() { let x = 1;", 2);
    highlighter.request_update("fn main() { let x = 42;", 3);

    // Should not crash and should handle rapid updates gracefully
    assert_eq!(highlighter.name(), "rust");
}

#[test]
fn test_wgsl_highlighting() {
    let highlighter = SyntaxHighlighter::new_wgsl();

    // Test basic interface
    assert_eq!(highlighter.name(), "wgsl");

    // Test that WGSL highlighter was created successfully
    let wgsl_code = r#"
@vertex
fn main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 1.0);
}
"#;

    // Request an update (should not crash)
    highlighter.request_update(wgsl_code, 1);
}

#[test]
fn test_wgsl_capture_names() {
    use tree_sitter::{Parser, Query, QueryCursor};

    println!("Loading WGSL using proper WasmStore setup...");

    // Create engine and store
    let engine = tree_sitter::wasmtime::Engine::default();
    let mut store = WasmStore::new(&engine).expect("Failed to create WasmStore");

    // Load WASM bytes
    const WGSL_WASM: &[u8] = include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
    println!("WASM size: {} bytes", WGSL_WASM.len());

    // Load language from WASM
    let language = store
        .load_language("wgsl", WGSL_WASM)
        .expect("Failed to load WGSL language");

    println!("Language loaded, version: {:?}", language.version());

    // Create parser and set the store FIRST
    let mut parser = Parser::new();
    parser
        .set_wasm_store(store)
        .expect("Failed to set WasmStore on parser");

    // Now set the language
    match parser.set_language(&language) {
        Ok(()) => println!("Successfully set WGSL language in parser"),
        Err(e) => {
            println!("Failed to set language: {:?}", e);
            return;
        }
    }

    // Check if parser has a language set after setting it
    match parser.language() {
        Some(lang) => println!("Parser has language set, version: {:?}", lang.version()),
        None => {
            println!("ERROR: Parser has no language set!");
            return;
        }
    }

    // Try various WGSL code samples to test parsing
    let test_cases = vec![
        "fn main() {}",
        "let x = 42;",
        "@vertex fn vs() {}",
        "var<private> x: f32;",
        "", // Empty should parse
    ];

    for code in test_cases {
        println!("\nTrying to parse: '{}'", code);
        let tree = parser.parse(code, None);
        if let Some(tree) = tree {
            let root = tree.root_node();
            println!("  Success! Root node kind: {}", root.kind());
            if root.has_error() {
                println!("  Warning: Tree has errors");
            }
        } else {
            println!("  Failed to parse!");
        }
    }

    // Now test with the highlights query
    let source_code = "@vertex fn main() { let x: f32 = 1.0; }";
    println!("\nTesting highlights with: '{}'", source_code);

    let tree = parser.parse(source_code, None);
    if tree.is_none() {
        println!("Failed to parse WGSL code for highlights!");
        return;
    }
    let tree = tree.unwrap();

    const WGSL_HIGHLIGHTS: &str = include_str!("../assets/grammars/wgsl/highlights.scm");
    let query = Query::new(&language, WGSL_HIGHLIGHTS).unwrap();

    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();

    println!("Available capture names in tree-sitter-wgsl:");
    for (i, name) in capture_names.iter().enumerate() {
        println!("  {}: {}", i, name);
    }

    println!("\nActual captures in WGSL sample code:");
    let mut matches = cursor.matches(&query, tree.root_node(), source_code.as_bytes());
    let mut count = 0;
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            if count < 30 {
                let capture_name = &capture_names[capture.index as usize];
                let node_text = &source_code[capture.node.start_byte()..capture.node.end_byte()];
                let token_type = SyntaxHighlighter::capture_name_to_token_type(capture_name);
                println!(
                    "  '{}' -> '{}' -> {:?}",
                    node_text, capture_name, token_type
                );
                count += 1;
            }
        }
    }
}

#[test]
fn test_file_extension_detection() {
    // Test Rust file detection
    let rust_highlighter = SyntaxHighlighter::from_file_path("main.rs");
    assert!(rust_highlighter.is_some());
    assert_eq!(rust_highlighter.unwrap().name(), "rust");

    // Test WGSL file detection
    let wgsl_highlighter = SyntaxHighlighter::from_file_path("shader.wgsl");
    assert!(wgsl_highlighter.is_some());
    assert_eq!(wgsl_highlighter.unwrap().name(), "wgsl");

    // Test unsupported extension
    let unknown = SyntaxHighlighter::from_file_path("test.txt");
    assert!(unknown.is_none());
}

#[test]
fn test_capture_names() {
    use tree_sitter::{Parser, Query, QueryCursor};

    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .unwrap();

    let source_code = r#"
fn main() {
    let x = 42;
    println!("Hello, {}", x);
}
"#;

    let tree = parser.parse(source_code, None).unwrap();
    let query = Query::new(
        &tree_sitter_rust::LANGUAGE.into(),
        tree_sitter_rust::HIGHLIGHTS_QUERY,
    )
    .unwrap();

    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();

    println!("Available capture names in tree-sitter-rust:");
    for (i, name) in capture_names.iter().enumerate() {
        println!("  {}: {}", i, name);
    }

    println!("\nActual captures in sample code:");
    let mut matches = cursor.matches(&query, tree.root_node(), source_code.as_bytes());
    let mut count = 0;
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            if count < 30 {
                let capture_name = &capture_names[capture.index as usize];
                let node_text = &source_code[capture.node.start_byte()..capture.node.end_byte()];
                let token_type = SyntaxHighlighter::capture_name_to_token_type(capture_name);
                println!(
                    "  '{}' -> '{}' -> {:?}",
                    node_text, capture_name, token_type
                );
                count += 1;
            }
        }
    }
}
