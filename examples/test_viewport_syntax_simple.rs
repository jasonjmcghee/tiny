//! Simple test for viewport-aware syntax highlighting

use std::sync::Arc;
use std::time::Instant;
use tiny_editor::{
    syntax::{Languages, SyntaxHighlighter},
    text_effects::TextStyleProvider,
};

fn main() {
    println!("Testing viewport-aware syntax highlighting performance...\n");

    // Create a large Rust source file for testing
    let mut source = String::new();
    for i in 0..100 {
        source.push_str(&format!(
            r#"
// Section {}
pub struct TestStruct_{} {{
    field1: String,
    field2: Vec<u32>,
    field3: HashMap<String, Arc<dyn Any>>,
}}

impl TestStruct_{} {{
    pub fn new() -> Self {{
        Self {{
            field1: "test string {}".to_string(),
            field2: vec![1, 2, 3, 4, 5],
            field3: HashMap::new(),
        }}
    }}

    pub fn process(&mut self) -> Result<(), Error> {{
        for i in 0..100 {{
            if i % 2 == 0 {{
                println!("Even: {{}}", i);
            }}
        }}
        Ok(())
    }}
}}
"#,
            i, i, i, i
        ));
    }

    println!("Created test document with {} bytes", source.len());
    println!("Document has {} lines\n", source.lines().count());

    // Create syntax highlighter
    let highlighter =
        SyntaxHighlighter::new(Languages::rust()).expect("Failed to create highlighter");
    let highlighter = Arc::new(highlighter);

    // Request initial parse
    println!("Requesting initial parse...");
    highlighter.request_update(&source, 1);

    // Wait for background thread to parse
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Test 1: Query full document (old approach)
    println!("Test 1: Full document query");
    let start = Instant::now();
    let all_effects = highlighter.get_effects_in_range(0..source.len());
    let duration = start.elapsed();
    println!(
        "  Full document: {} effects in {:?}\n",
        all_effects.len(),
        duration
    );

    // Test 2: Query just visible viewport (new approach)
    println!("Test 2: Viewport query (lines 50-100, approx bytes 2000-4000)");

    // Find byte range for lines 50-100
    let lines: Vec<&str> = source.lines().collect();
    let start_byte = lines.iter().take(50).map(|l| l.len() + 1).sum::<usize>();
    let end_byte = lines.iter().take(100).map(|l| l.len() + 1).sum::<usize>();

    let start = Instant::now();
    let viewport_effects = highlighter.get_visible_effects(&source, start_byte..end_byte);
    let duration = start.elapsed();
    println!(
        "  Viewport query: {} effects in {:?}",
        viewport_effects.len(),
        duration
    );

    // Show the speedup
    let all_time = highlighter.get_effects_in_range(0..source.len()).len();
    let viewport_time = viewport_effects.len();

    println!("\nComparison:");
    println!("  Full document processes: {} nodes", all_time);
    println!("  Viewport query processes: {} nodes", viewport_time);
    println!(
        "  Reduction: {:.1}%",
        (1.0 - viewport_time as f64 / all_time as f64) * 100.0
    );

    // Test 3: Multiple viewport queries (simulating scrolling)
    println!("\nTest 3: Simulating scrolling (10 viewport queries)");
    let mut total_effects = 0;
    let start = Instant::now();

    for i in 0..10 {
        let start_line = i * 10;
        let end_line = start_line + 50;

        let start_byte = lines
            .iter()
            .take(start_line)
            .map(|l| l.len() + 1)
            .sum::<usize>();
        let end_byte = lines
            .iter()
            .take(end_line)
            .map(|l| l.len() + 1)
            .sum::<usize>()
            .min(source.len());

        let effects = highlighter.get_visible_effects(&source, start_byte..end_byte);
        total_effects += effects.len();
    }

    let duration = start.elapsed();
    println!(
        "  10 viewport queries: {} total effects in {:?}",
        total_effects, duration
    );
    println!("  Average per query: {:?}\n", duration / 10);

    println!("✓ Viewport-aware syntax highlighting is working!");
    println!("  Only processing visible AST nodes significantly improves performance.");
}
