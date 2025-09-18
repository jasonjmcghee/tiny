//! Basic test of the editor without graphics

use std::time::Instant;
use tiny_editor::{Content, Doc, Edit};

fn main() {
    println!("=== Tiny Editor Basic Test ===\n");

    // Test 1: Create and manipulate document
    println!("1. Creating document...");
    let doc = Doc::from_str("Hello, world!");
    println!("   Initial: {}", doc.read().flatten_to_string());

    // Test 2: Lock-free reads
    println!("\n2. Testing lock-free reads...");
    let start = Instant::now();
    for _ in 0..1_000_000 {
        let snapshot = doc.read(); // This is lock-free!
        let _ = snapshot.flatten_to_string();
    }
    let elapsed = start.elapsed();
    println!(
        "   1M reads in {:?} ({:.0} reads/sec)",
        elapsed,
        1_000_000.0 / elapsed.as_secs_f64()
    );

    // Test 3: Edits with buffering
    println!("\n3. Testing edits...");
    doc.edit(Edit::Insert {
        pos: 7,
        content: Content::Text("tiny ".to_string()),
    });
    doc.edit(Edit::Insert {
        pos: 18,
        content: Content::Text(" How are you?".to_string()),
    });
    doc.flush();
    println!("   After edits: {}", doc.read().flatten_to_string());

    // Test 4: Concurrent access (simulated)
    println!("\n4. Testing concurrent access...");
    let doc_clone = doc.read();
    std::thread::spawn(move || {
        // Reader thread
        for _ in 0..100 {
            let _ = doc_clone.flatten_to_string();
        }
    });

    // Writer thread
    for i in 0..10 {
        doc.edit(Edit::Insert {
            pos: 0,
            content: Content::Text(format!("{} ", i)),
        });
    }
    doc.flush();
    println!("   Final: {}", doc.read().flatten_to_string());

    // Test 5: Memory usage
    println!("\n5. Building large document...");
    let large_text = "a".repeat(1_000_000);
    let start = Instant::now();
    let large_doc = Doc::from_str(&large_text);
    println!("   Created 1MB document in {:?}", start.elapsed());

    // Test rapid edits
    let start = Instant::now();
    for i in 0..1000 {
        large_doc.edit(Edit::Insert {
            pos: i,
            content: Content::Text("x".to_string()),
        });
    }
    large_doc.flush();
    println!("   1000 edits in {:?}", start.elapsed());

    println!("\n✅ All tests completed!");
}
