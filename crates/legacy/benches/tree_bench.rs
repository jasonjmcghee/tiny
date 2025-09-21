//! Benchmarks for tree operations matching real editor usage patterns
//!
//! Based on the architecture in plan.md, we test:
//! - RCU performance (lock-free reads during writes)
//! - O(log n) navigation (line/byte conversions)
//! - Edit batching (16ms of keystrokes)
//! - Rendering traversal (visible content only)

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tiny_editor::tree::{Content, Doc, Edit};

/// Generate a realistic document with mixed content
fn generate_document(lines: usize) -> String {
    let mut doc = String::new();
    for i in 0..lines {
        // Mix of code-like lines with varying lengths
        match i % 5 {
            0 => doc.push_str(&format!("fn function_{}() {{\n", i)),
            1 => doc.push_str(&format!(
                "    let variable_{} = \"string literal with some text\";\n",
                i
            )),
            2 => doc.push_str(&format!("    // Comment explaining line {}\n", i)),
            3 => doc.push_str(&format!("    process_data({}, {}, {});\n", i, i * 2, i * 3)),
            _ => doc.push_str("}\n"),
        }
    }
    doc
}

/// Benchmark single character insertion (most common edit)
fn bench_single_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_insert");

    for size in [100, 1000, 10000, 100000].iter() {
        let text = generate_document(*size);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let doc = Doc::from_str(&text);
                let mid = text.len() / 2;

                // Single character insert (typical keystroke)
                doc.edit(Edit::Insert {
                    pos: mid,
                    content: Content::Text("x".to_string()),
                });
                doc.flush();

                std::hint::black_box(doc.read());
            });
        });
    }
    group.finish();
}

/// Benchmark batched edits (simulating 16ms of typing)
fn bench_batched_edits(c: &mut Criterion) {
    let mut group = c.benchmark_group("batched_edits");

    for size in [1000, 10000, 100000].iter() {
        let text = generate_document(*size);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                let doc = Doc::from_str(&text);
                let start = text.len() / 2;

                // Simulate ~10 keystrokes in 16ms window
                for i in 0..10 {
                    doc.edit(Edit::Insert {
                        pos: start + i,
                        content: Content::Text("a".to_string()),
                    });
                }

                // Single flush for all edits (RCU batch)
                doc.flush();

                std::hint::black_box(doc.read());
            });
        });
    }
    group.finish();
}

/// Benchmark navigation operations (O(log n) promise)
fn bench_navigation(c: &mut Criterion) {
    let mut group = c.benchmark_group("navigation");

    for size in [1000, 10000, 100000].iter() {
        let text = generate_document(*size);
        let doc = Doc::from_str(&text);
        let tree = doc.read();

        // Byte to line conversion (cursor movement)
        group.bench_with_input(BenchmarkId::new("byte_to_line", size), size, |b, _| {
            let positions: Vec<usize> = (0..100).map(|i| (text.len() * i) / 100).collect();

            b.iter(|| {
                for &pos in &positions {
                    std::hint::black_box(tree.byte_to_line(pos));
                }
            });
        });

        // Line to byte conversion (goto line)
        group.bench_with_input(BenchmarkId::new("line_to_byte", size), size, |b, _| {
            let line_count = tree.line_count();
            let lines: Vec<u32> = (0..100).map(|i| (line_count * i) / 100).collect();

            b.iter(|| {
                for &line in &lines {
                    std::hint::black_box(tree.line_to_byte(line));
                }
            });
        });

        // Find next/prev newline (line navigation)
        group.bench_with_input(BenchmarkId::new("find_newlines", size), size, |b, _| {
            let positions: Vec<usize> = (0..100).map(|i| (text.len() * i) / 100).collect();

            b.iter(|| {
                for &pos in &positions {
                    std::hint::black_box(tree.find_next_newline(pos));
                    std::hint::black_box(tree.find_prev_newline(pos));
                }
            });
        });
    }
    group.finish();
}

/// Benchmark text extraction for rendering
fn bench_text_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_extraction");

    for size in [1000, 10000, 100000].iter() {
        let text = generate_document(*size);
        let doc = Doc::from_str(&text);
        let tree = doc.read();

        // Extract visible viewport (typically ~50 lines)
        group.bench_with_input(BenchmarkId::new("viewport_slice", size), size, |b, _| {
            // Simulate extracting different viewports
            let viewport_size = 2000; // ~50 lines of code
            let positions: Vec<usize> = (0..10).map(|i| (text.len() * i) / 10).collect();

            b.iter(|| {
                for &pos in &positions {
                    let end = (pos + viewport_size).min(text.len());
                    std::hint::black_box(tree.get_text_slice(pos..end));
                }
            });
        });

        // Extract single lines (for syntax highlighting)
        group.bench_with_input(BenchmarkId::new("line_extraction", size), size, |b, _| {
            let positions: Vec<usize> = (0..100).map(|i| (text.len() * i) / 100).collect();

            b.iter(|| {
                for &pos in &positions {
                    std::hint::black_box(tree.get_line_at(pos));
                }
            });
        });
    }
    group.finish();
}

/// Benchmark RCU reader/writer concurrency
fn bench_rcu_concurrency(c: &mut Criterion) {
    let mut group = c.benchmark_group("rcu_concurrency");

    let text = generate_document(10000);

    group.bench_function("concurrent_reads_during_writes", |b| {
        b.iter(|| {
            let doc = Arc::new(Doc::from_str(&text));
            let doc_clone = Arc::clone(&doc);

            // Spawn reader thread (simulating renderer at 120fps)
            let reader = thread::spawn(move || {
                let mut sum = 0usize;
                for _ in 0..120 {
                    let tree = doc_clone.read();
                    sum += tree.byte_count();
                    // Simulate 8.3ms frame time
                    thread::sleep(Duration::from_micros(8300));
                }
                sum
            });

            // Writer thread (simulating typing)
            for i in 0..100 {
                doc.edit(Edit::Insert {
                    pos: i,
                    content: Content::Text("x".to_string()),
                });

                // Flush every 16ms (60fps editing)
                if i % 10 == 0 {
                    doc.flush();
                    thread::sleep(Duration::from_micros(16000));
                }
            }

            std::hint::black_box(reader.join().unwrap());
        });
    });

    group.finish();
}

/// Benchmark deletion operations
fn bench_deletion(c: &mut Criterion) {
    let mut group = c.benchmark_group("deletion");

    for size in [1000, 10000, 100000].iter() {
        let text = generate_document(*size);

        // Single character deletion (backspace)
        group.bench_with_input(BenchmarkId::new("single_delete", size), size, |b, _| {
            b.iter(|| {
                let doc = Doc::from_str(&text);
                let mid = text.len() / 2;

                doc.edit(Edit::Delete {
                    range: mid..mid + 1,
                });
                doc.flush();

                std::hint::black_box(doc.read());
            });
        });

        // Line deletion (Ctrl+K)
        group.bench_with_input(BenchmarkId::new("line_delete", size), size, |b, _| {
            b.iter(|| {
                let doc = Doc::from_str(&text);
                let tree = doc.read();
                let mid = text.len() / 2;

                // Find line boundaries
                let line_start = tree.find_line_start_at(mid);
                let line_end = tree.find_line_end_at(mid);

                doc.edit(Edit::Delete {
                    range: line_start..line_end,
                });
                doc.flush();

                std::hint::black_box(doc.read());
            });
        });
    }
    group.finish();
}

/// Benchmark tree traversal for rendering
fn bench_tree_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_traversal");

    for size in [1000, 10000, 100000].iter() {
        let text = generate_document(*size);
        let doc = Doc::from_str(&text);

        // Full document conversion (save operation) - original method
        group.bench_with_input(BenchmarkId::new("to_string", size), size, |b, _| {
            b.iter(|| {
                let tree = doc.read();
                std::hint::black_box(tree.flatten_to_string());
            });
        });
    }
    group.finish();
}

/// Benchmark memory overhead
fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory");

    // Measure Arc clone cost (for undo history)
    group.bench_function("arc_clone", |b| {
        let text = generate_document(10000);
        let doc = Doc::from_str(&text);
        let tree = doc.read();

        b.iter(|| {
            std::hint::black_box(Arc::clone(&tree));
        });
    });

    // Measure tree creation cost
    for size in [1000, 10000, 100000].iter() {
        group.bench_with_input(BenchmarkId::new("tree_creation", size), size, |b, _| {
            let text = generate_document(*size);
            b.iter(|| {
                std::hint::black_box(Doc::from_str(&text));
            });
        });
    }

    group.finish();
}

/// Benchmark realistic editing session
fn bench_realistic_session(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_session");

    group.bench_function("typing_burst", |b| {
        let text = generate_document(5000);

        b.iter(|| {
            let doc = Doc::from_str(&text);
            let mut pos = text.len() / 2;

            // Simulate typing a function
            let code = "fn example() {\n    let x = 42;\n    println!(\"x = {}\", x);\n}\n";

            for ch in code.chars() {
                doc.edit(Edit::Insert {
                    pos,
                    content: Content::Text(ch.to_string()),
                });
                pos += ch.len_utf8();

                // Flush every ~16ms worth of typing (about 3 chars at 60wpm)
                if pos % 3 == 0 {
                    doc.flush();
                }
            }

            std::hint::black_box(doc.read());
        });
    });

    group.bench_function("multi_cursor_edit", |b| {
        let text = generate_document(1000);

        b.iter(|| {
            let doc = Doc::from_str(&text);

            // Simulate multi-cursor editing at 10 positions
            let positions: Vec<usize> = (0..10).map(|i| (text.len() * i) / 10).collect();

            // Insert same text at all positions
            for &pos in &positions {
                doc.edit(Edit::Insert {
                    pos,
                    content: Content::Text("TODO: ".to_string()),
                });
            }

            doc.flush();
            std::hint::black_box(doc.read());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_single_insert,
    bench_batched_edits,
    bench_navigation,
    bench_text_extraction,
    bench_rcu_concurrency,
    bench_deletion,
    bench_tree_traversal,
    bench_memory_usage,
    bench_realistic_session
);

criterion_main!(benches);
