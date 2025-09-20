use tiny_editor::coordinates::LayoutPos;
use tiny_editor::tree::*;
use tiny_editor::widget;

// From tree.rs - not publicly exported
const MAX_SPANS: usize = 16;

// Helper function from tree.rs - not publicly exported
fn span_bytes(span: &Span) -> usize {
    match span {
        Span::Text { bytes, .. } => bytes.len(),
        Span::Widget { .. } => 0,
    }
}

#[test]
fn test_byte_and_line_counts() {
    let doc = Doc::from_str("Hello\nWorld\n!");
    let tree = doc.read();

    assert_eq!(tree.byte_count(), 13);
    assert_eq!(tree.line_count(), 2);
}

#[test]
fn test_line_to_byte() {
    let doc = Doc::from_str("Line 1\nLine 2\nLine 3\n");
    let tree = doc.read();

    assert_eq!(tree.line_to_byte(0), Some(0));
    assert_eq!(tree.line_to_byte(1), Some(7));
    assert_eq!(tree.line_to_byte(2), Some(14));
    assert_eq!(tree.line_to_byte(3), Some(21));
    assert_eq!(tree.line_to_byte(4), None);
}

#[test]
fn test_byte_to_line() {
    let doc = Doc::from_str("Line 1\nLine 2\nLine 3\n");
    let tree = doc.read();

    // Total: "Line 1\n" (7 bytes) "Line 2\n" (7 bytes) "Line 3\n" (7 bytes)
    assert_eq!(tree.byte_to_line(0), 0); // Start of file
    assert_eq!(tree.byte_to_line(5), 0); // In "Line 1"
    assert_eq!(tree.byte_to_line(7), 1); // Start of "Line 2"
    assert_eq!(tree.byte_to_line(10), 1); // In "Line 2"
    assert_eq!(tree.byte_to_line(14), 2); // Start of "Line 3"
    assert_eq!(tree.byte_to_line(20), 2); // In "Line 3"
}

#[test]
fn test_find_newlines() {
    let doc = Doc::from_str("Hello\nWorld\n!");
    let tree = doc.read();

    assert_eq!(tree.find_next_newline(0), Some(5));
    assert_eq!(tree.find_next_newline(6), Some(11));
    assert_eq!(tree.find_next_newline(12), None);

    assert_eq!(tree.find_prev_newline(0), None);
    assert_eq!(tree.find_prev_newline(6), Some(5));
    assert_eq!(tree.find_prev_newline(12), Some(11));
}

#[test]
fn test_get_text_slice() {
    let doc = Doc::from_str("Hello, World!");
    let tree = doc.read();

    assert_eq!(tree.get_text_slice(0..5), "Hello");
    assert_eq!(tree.get_text_slice(7..12), "World");
    assert_eq!(tree.get_text_slice(0..13), "Hello, World!");
    assert_eq!(tree.get_text_slice(5..5), "");
}

#[test]
fn test_line_navigation() {
    let doc = Doc::from_str("First line\nSecond line\nThird line");
    let tree = doc.read();

    assert_eq!(tree.find_line_start_at(5), 0);
    assert_eq!(tree.find_line_start_at(15), 11);

    assert_eq!(tree.find_line_end_at(5), 10);
    assert_eq!(tree.find_line_end_at(15), 22);

    assert_eq!(tree.get_line_at(5), "First line");
    assert_eq!(tree.get_line_at(15), "Second line");
}

#[test]
fn test_with_edits() {
    let doc = Doc::from_str("Line 1\n");

    doc.edit(Edit::Insert {
        pos: 7,
        content: Content::Text("Line 2\n".to_string()),
    });
    doc.flush();

    let tree = doc.read();
    assert_eq!(tree.line_count(), 2);
    assert_eq!(tree.byte_to_line(8), 1);
}

#[test]
fn test_large_document() {
    let mut text = String::new();
    for i in 0..100 {
        text.push_str(&format!("Line {}\n", i));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    assert_eq!(tree.line_count(), 100);

    let line_50_start = tree.line_to_byte(50).unwrap();
    assert!(tree.get_line_at(line_50_start).starts_with("Line 50"));
}

#[test]
fn test_unicode() {
    let doc = Doc::from_str("你好\n世界\n");
    let tree = doc.read();

    assert_eq!(tree.byte_count(), 14);
    assert_eq!(tree.line_count(), 2);
    assert_eq!(tree.char_count(), 6);

    assert_eq!(tree.get_line_at(0), "你好");
    assert_eq!(tree.get_line_at(8), "世界");
}

#[test]
fn test_document_operations() {
    let doc = Doc::from_str("");
    assert_eq!(*doc.read().flatten_to_string(), "");
    assert_eq!(doc.read().byte_count(), 0);

    doc.edit(Edit::Insert {
        pos: 0,
        content: Content::Text("A".to_string()),
    });
    doc.flush();
    assert_eq!(*doc.read().flatten_to_string(), "A");

    doc.edit(Edit::Insert {
        pos: 1,
        content: Content::Text("C".to_string()),
    });
    doc.flush();
    assert_eq!(*doc.read().flatten_to_string(), "AC");

    doc.edit(Edit::Insert {
        pos: 1,
        content: Content::Text("B".to_string()),
    });
    doc.flush();
    assert_eq!(*doc.read().flatten_to_string(), "ABC");

    doc.edit(Edit::Delete { range: 1..2 });
    doc.flush();
    assert_eq!(*doc.read().flatten_to_string(), "AC");
}

#[test]
fn test_typing_simulation() {
    let doc = Doc::from_str("");

    for (i, ch) in "Hello, World!".chars().enumerate() {
        doc.edit(Edit::Insert {
            pos: i,
            content: Content::Text(ch.to_string()),
        });
        doc.flush();
    }

    assert_eq!(*doc.read().flatten_to_string(), "Hello, World!");
    assert_eq!(doc.read().byte_count(), 13);
}

#[test]
fn test_concurrent_readers() {
    let doc = Doc::from_str("Shared");

    let tree1 = doc.read();
    let tree2 = doc.read();

    assert_eq!(*tree1.flatten_to_string(), "Shared");
    assert_eq!(*tree2.flatten_to_string(), "Shared");
    assert_eq!(tree1.byte_count(), tree2.byte_count());
}

#[test]
fn test_widget_insertion() {
    let doc = Doc::from_str("Text");

    doc.edit(Edit::Insert {
        pos: 2,
        content: Content::Widget(widget::cursor(LayoutPos::new(0.0, 0.0))),
    });
    doc.flush();

    assert_eq!(*doc.read().flatten_to_string(), "Text");
}

#[test]
fn test_multi_leaf_creation() {
    // Create text that will span multiple leaves
    let mut text = String::new();
    for i in 0..100 {
        text.push_str(&format!(
            "This is line {} with some content to fill up space.\n",
            i
        ));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    // Verify tree structure has multiple leaves
    match &tree.root {
        Node::Leaf { spans, .. } => {
            // Small text might still fit in one leaf
            assert!(spans.len() <= MAX_SPANS);
        }
        Node::Internal { children, .. } => {
            // Large text should create internal nodes
            assert!(children.len() > 0);
            for child in children {
                if let Node::Leaf { spans, .. } = child {
                    assert!(spans.len() <= MAX_SPANS);
                }
            }
        }
    }

    // Verify content is preserved
    assert_eq!(*tree.flatten_to_string(), text);
}

#[test]
fn test_multi_leaf_traversal() {
    // Create document with multiple leaves
    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&format!("Line {:03}\n", i));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    // Test that document has expected number of lines
    assert_eq!(tree.line_count(), 50);

    // Test that we can access all lines
    for i in 0..50 {
        let line_start = tree.line_to_byte(i).unwrap();
        let line_text = tree.get_line_at(line_start);
        assert!(line_text.starts_with(&format!("Line {:03}", i)));
    }
}

#[test]
fn test_incremental_edit_in_multi_leaf() {
    // Create multi-leaf document
    let mut text = String::new();
    for i in 0..30 {
        text.push_str(&format!("Original line {}\n", i));
    }

    let doc = Doc::from_str(&text);

    // Insert in middle of document
    doc.edit(Edit::Insert {
        pos: text.len() / 2,
        content: Content::Text("INSERTED TEXT\n".to_string()),
    });
    doc.flush();

    let result = doc.read().flatten_to_string();
    assert!(result.contains("INSERTED TEXT"));

    // Delete across leaf boundaries
    doc.edit(Edit::Delete {
        range: (text.len() / 3)..(text.len() * 2 / 3),
    });
    doc.flush();

    let tree = doc.read();
    assert!(tree.byte_count() < text.len());
}

#[test]
fn test_delete_causing_many_span_splits() {
    // This test reproduces the bug where deleting a small range that crosses
    // many span boundaries could create too many spans exceeding MAX_SPANS

    // Create a document with many small text spans
    let doc = Doc::new();

    // Insert many small pieces of text to create multiple spans
    for i in 0..20 {
        doc.edit(Edit::Insert {
            pos: doc.read().byte_count(),
            content: Content::Text(format!("text{} ", i)),
        });
    }
    doc.flush();

    // Now delete a small range that will split multiple spans
    // This could potentially create prefix and suffix for each span it touches
    let delete_start = 10;
    let delete_end = 20;

    doc.edit(Edit::Delete {
        range: delete_start..delete_end,
    });
    doc.flush();

    // The tree should still be valid after this delete
    let tree = doc.read();

    // Verify tree structure is valid (no spans exceed MAX_SPANS)
    fn verify_node(node: &Node) -> bool {
        match node {
            Node::Leaf { spans, .. } => {
                if spans.len() > MAX_SPANS {
                    eprintln!(
                        "Leaf has {} spans, exceeds MAX_SPANS ({})",
                        spans.len(),
                        MAX_SPANS
                    );
                    return false;
                }
                true
            }
            Node::Internal { children, .. } => {
                if children.len() > MAX_SPANS {
                    eprintln!(
                        "Internal has {} children, exceeds MAX_SPANS ({})",
                        children.len(),
                        MAX_SPANS
                    );
                    return false;
                }
                children.iter().all(verify_node)
            }
        }
    }

    assert!(
        verify_node(&tree.root),
        "Tree structure invalid after delete"
    );
}

#[test]
fn test_leaf_splitting() {
    let doc = Doc::from_str("Initial");

    // Insert enough content to force a split
    for i in 0..MAX_SPANS + 5 {
        doc.edit(Edit::Insert {
            pos: 0,
            content: Content::Text(format!("Span {}\n", i)),
        });
    }
    doc.flush();

    let tree = doc.read();

    // Should have created internal node with multiple leaves
    match &tree.root {
        Node::Internal { children, .. } => {
            assert!(children.len() > 1, "Should have split into multiple nodes");
        }
        Node::Leaf { spans, .. } => {
            assert!(spans.len() <= MAX_SPANS, "Leaf should not exceed MAX_SPANS");
        }
    }
}

#[test]
fn test_leaf_merging() {
    // Create document with multiple leaves
    let mut text = String::new();
    for i in 0..40 {
        text.push_str(&format!("Line to delete {}\n", i));
    }

    let doc = Doc::from_str(&text);

    // Delete most content to trigger merging
    doc.edit(Edit::Delete {
        range: 100..text.len() - 100,
    });
    doc.flush();

    let tree = doc.read();

    // Verify structure is still valid
    match &tree.root {
        Node::Leaf { spans, .. } => {
            assert!(spans.len() <= MAX_SPANS);
        }
        Node::Internal { children, .. } => {
            // Should have merged some children
            assert!(children.len() <= MAX_SPANS);
        }
    }
}

#[test]
fn test_advance_leaf_efficiency() {
    // Create multi-leaf document - need enough data to span multiple 1KB chunks
    // and then multiple leaves (16 chunks per leaf)
    let mut text = String::new();
    // Create 20KB of data to ensure multiple leaves
    for i in 0..500 {
        text.push_str(&format!(
            "This is line number {} with some padding text to make it longer\n",
            i
        ));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    // Verify document was created with expected size
    assert_eq!(tree.byte_count(), text.len());

    // Test that we can efficiently access different parts
    for i in (0..500).step_by(50) {
        let line_pos = tree.line_to_byte(i).unwrap();
        let line = tree.get_line_at(line_pos);
        assert!(line.contains(&format!("line number {}", i)));
    }
}

#[test]
fn test_seek_in_multi_leaf() {
    // Create predictable multi-leaf structure
    let mut text = String::new();
    for i in 0..50 {
        text.push_str(&format!("Line {:04}\n", i));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    // Test accessing various positions
    let positions = vec![0, 100, 250, text.len() / 2, text.len() - 1];

    for pos in positions {
        // Verify we can read from position
        let remaining = tree.byte_count() - pos;
        let read_len = remaining.min(10);
        let content = tree.get_text_slice(pos..pos + read_len);

        // Verify the text is what we expect
        let expected = &text[pos..pos + read_len];
        assert_eq!(content, expected);
    }
}

#[test]
fn test_line_navigation_multi_leaf() {
    // Create document where lines span multiple leaves
    let mut text = String::new();
    let lines_per_leaf = 20;
    let total_lines = 60;

    for i in 0..total_lines {
        text.push_str(&format!("Line number {:03}\n", i));
    }

    let doc = Doc::from_str(&text);
    let tree = doc.read();

    // Test line_to_byte for lines in different leaves
    for line in (0..total_lines).step_by(10) {
        let byte_pos = tree.line_to_byte(line);
        assert!(byte_pos.is_some());

        // Verify the position is correct
        let line_text = tree.get_line_at(byte_pos.unwrap());
        assert!(line_text.contains(&format!("Line number {:03}", line)));
    }

    // Test byte_to_line across leaves
    for pos in (0..text.len()).step_by(100) {
        let line = tree.byte_to_line(pos);
        let byte_back = tree.line_to_byte(line);
        assert!(byte_back.is_some());
        assert!(byte_back.unwrap() <= pos);
    }
}
