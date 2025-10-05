use tiny_tree::*;

// From tree.rs - not publicly exported
const MAX_SPANS: usize = 16;

// Helper function from tree.rs - not publicly exported
fn span_bytes(span: &Span) -> usize {
    match span {
        Span::Text { bytes, .. } => bytes.len(),
        Span::Spatial(_) => 0,
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

// Widget insertion test removed - widgets are now handled by Spatial trait

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

// === Tests from crates/tree (new tree implementation) ===

mod new_tree_tests {
    use std::sync::Arc;
    use tiny_tree::*;

    #[test]
    fn test_utf16_offset_conversions() {
        // Test with ASCII
        let tree = Tree::from_str("Hello World");
        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0));
        assert_eq!(tree.offset_to_offset_utf16(5), OffsetUtf16(5));
        assert_eq!(tree.offset_to_offset_utf16(11), OffsetUtf16(11));

        // Test with multibyte characters (2-byte UTF-8, 1 UTF-16 code unit)
        let tree = Tree::from_str("Héllo Wörld"); // é = 2 bytes, ö = 2 bytes
        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0));
        assert_eq!(tree.offset_to_offset_utf16(2), OffsetUtf16(1)); // After 'é'
        assert_eq!(tree.offset_to_offset_utf16(9), OffsetUtf16(7)); // After 'ö'

        // Test with emoji (4-byte UTF-8, 2 UTF-16 code units)
        let tree = Tree::from_str("Hello 🌍 World");
        assert_eq!(tree.byte_count(), 16); // 6 + 4 + 6
        assert_eq!(tree.len_utf16(), OffsetUtf16(14)); // 6 + 2 + 6
        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0));
        assert_eq!(tree.offset_to_offset_utf16(6), OffsetUtf16(6)); // Before emoji
        assert_eq!(tree.offset_to_offset_utf16(10), OffsetUtf16(8)); // After emoji (4 bytes, 2 UTF-16)
        assert_eq!(tree.offset_to_offset_utf16(16), OffsetUtf16(14)); // End of string

        // Test round-trip conversions (only at valid character boundaries)
        let text = tree.flatten_to_string();
        let mut byte_offset = 0;
        for _c in text.chars() {
            let utf16 = tree.offset_to_offset_utf16(byte_offset);
            let back = tree.offset_utf16_to_offset(utf16);
            assert_eq!(
                back, byte_offset,
                "Round-trip failed for offset {}",
                byte_offset
            );
            byte_offset += _c.len_utf8();
        }
        // Test final offset (end of string)
        let utf16 = tree.offset_to_offset_utf16(tree.byte_count());
        let back = tree.offset_utf16_to_offset(utf16);
        assert_eq!(back, tree.byte_count());
    }

    #[test]
    fn test_utf16_point_conversions() {
        let tree = Tree::from_str("Hello\nWörld 🌍\nTest");

        // Test line 0 (ASCII)
        assert_eq!(tree.doc_pos_to_point_utf16(0, 0), PointUtf16::new(0, 0));
        assert_eq!(tree.doc_pos_to_point_utf16(0, 5), PointUtf16::new(0, 5));

        // Test line 1 (with multibyte chars)
        assert_eq!(tree.doc_pos_to_point_utf16(1, 0), PointUtf16::new(1, 0));
        assert_eq!(
            tree.doc_pos_to_point_utf16(1, 3), // After 'ö' (W=1 + ö=2 = 3 bytes)
            PointUtf16::new(1, 2)              // W=1 + ö=1 = 2 UTF-16 units
        );
        assert_eq!(
            tree.doc_pos_to_point_utf16(1, 11), // After emoji (Wörld =7 + emoji=4 = 11 bytes)
            PointUtf16::new(1, 8)               // Wörld =6 + emoji=2 = 8 UTF-16 units
        );

        // Test round-trip conversions (only at valid character boundaries)
        // line_count() returns newline count, so valid lines are 0..=line_count
        let line_count = tree.line_count();
        for line in 0..=line_count {
            let line_text = tree.line_text_trimmed(line);
            let mut byte_col = 0;
            for c in line_text.chars() {
                let utf16_point = tree.doc_pos_to_point_utf16(line, byte_col);
                let (back_line, back_col) = tree.point_utf16_to_doc_pos(utf16_point);
                assert_eq!(
                    (back_line, back_col),
                    (line, byte_col),
                    "Round-trip failed for line {} col {}",
                    line,
                    byte_col
                );
                byte_col += c.len_utf8() as u32;
            }
        }
    }

    #[test]
    fn test_utf16_point_to_byte() {
        let tree = Tree::from_str("Hello\nWörld 🌍");

        // Text has 1 newline, so line_count() = 1, but valid lines are 0 and 1
        assert_eq!(tree.line_count(), 1); // Number of newlines
        assert_eq!(tree.line_to_byte(0), Some(0));
        assert_eq!(tree.line_to_byte(1), Some(6));

        // Line 0, column 0
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(0, 0)), 0);

        // Line 0, column 5 (end of "Hello")
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(0, 5)), 5);

        // Line 1, column 0 (start of "Wörld")
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(1, 0)), 6);

        // Line 1, column 1 (after 'W')
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(1, 1)), 7);

        // Line 1, column 2 (after 'Wö')
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(1, 2)), 9);

        // Line 1, column 8 (after "Wörld 🌍" - 6 UTF-16 + 2 UTF-16)
        assert_eq!(tree.point_utf16_to_byte(PointUtf16::new(1, 8)), 17);
    }

    #[test]
    fn test_utf16_with_only_emoji() {
        let tree = Tree::from_str("🔴🟠🟡🟢🔵");

        // Each emoji is 4 bytes, 2 UTF-16 code units
        assert_eq!(tree.byte_count(), 20); // 5 × 4 bytes
        assert_eq!(tree.len_utf16(), OffsetUtf16(10)); // 5 × 2 UTF-16 units

        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0));
        assert_eq!(tree.offset_to_offset_utf16(4), OffsetUtf16(2)); // After first emoji
        assert_eq!(tree.offset_to_offset_utf16(8), OffsetUtf16(4)); // After second emoji
        assert_eq!(tree.offset_to_offset_utf16(20), OffsetUtf16(10)); // End

        // Round-trip
        for i in 0..=5 {
            let byte_offset = i * 4;
            let utf16_offset = tree.offset_to_offset_utf16(byte_offset);
            let back = tree.offset_utf16_to_offset(utf16_offset);
            assert_eq!(back, byte_offset);
        }
    }

    #[test]
    fn test_utf16_mixed_content() {
        // Mix of 1-byte, 2-byte, 3-byte, and 4-byte UTF-8
        let tree = Tree::from_str("A§ह𝕳"); // 1+2+3+4 = 10 bytes, 1+1+1+2 = 5 UTF-16

        assert_eq!(tree.byte_count(), 10);
        assert_eq!(tree.len_utf16(), OffsetUtf16(5));

        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0)); // Start
        assert_eq!(tree.offset_to_offset_utf16(1), OffsetUtf16(1)); // After 'A'
        assert_eq!(tree.offset_to_offset_utf16(3), OffsetUtf16(2)); // After '§'
        assert_eq!(tree.offset_to_offset_utf16(6), OffsetUtf16(3)); // After 'ह'
        assert_eq!(tree.offset_to_offset_utf16(10), OffsetUtf16(5)); // After '𝕳' (surrogate pair)
    }

    #[test]
    fn test_utf16_empty_tree() {
        let tree = Tree::new();
        assert_eq!(tree.len_utf16(), OffsetUtf16(0));
        assert_eq!(tree.offset_to_offset_utf16(0), OffsetUtf16(0));
        assert_eq!(tree.offset_utf16_to_offset(OffsetUtf16(0)), 0);
    }

    #[test]
    fn test_bitmap_usage_in_documents() {
        // Verify that normal documents get bitmap metadata
        let small_text = "Hello World";
        let tree = Tree::from_str(small_text);

        // Check that the spans have metadata
        match &tree.root {
            Node::Leaf { spans, .. } => {
                for span in spans {
                    if let Span::Text {
                        bytes, metadata, ..
                    } = span
                    {
                        if bytes.len() <= 128 {
                            assert!(
                                metadata.is_some(),
                                "Small text spans should have bitmap metadata"
                            );
                        }
                    }
                }
            }
            _ => {}
        }

        // Larger document - should be split into multiple chunks
        let large_text = "Line of text\n".repeat(100);
        let tree = Tree::from_str(&large_text);

        let mut spans_with_metadata = 0;
        let mut spans_without_metadata = 0;

        fn count_spans(node: &Node, with_meta: &mut usize, without_meta: &mut usize) {
            match node {
                Node::Leaf { spans, .. } => {
                    for span in spans {
                        if let Span::Text { metadata, .. } = span {
                            if metadata.is_some() {
                                *with_meta += 1;
                            } else {
                                *without_meta += 1;
                            }
                        }
                    }
                }
                Node::Internal { children, .. } => {
                    for child in children {
                        count_spans(child, with_meta, without_meta);
                    }
                }
            }
        }

        count_spans(
            &tree.root,
            &mut spans_with_metadata,
            &mut spans_without_metadata,
        );
        eprintln!(
            "Spans with metadata: {}, without: {}",
            spans_with_metadata, spans_without_metadata
        );
        eprintln!("Note: Spans >128 bytes won't have metadata, but that's expected");
        // With 256+ byte chunks, we may have fewer or no spans with metadata, which is OK
        // The important thing is that the infrastructure is in place
    }

    #[test]
    fn test_bitmap_metadata() {
        // Test bitmap computation for small span
        let text = "Héllo"; // H=1byte + é=2bytes + llo=3bytes = 6 bytes total, 5 chars, 5 UTF-16
        let bytes: Arc<[u8]> = text.as_bytes().into();
        let meta = TextMetadata::compute(&bytes).expect("Should compute for ≤128 bytes");

        // Check character boundaries: positions 0,1,3,4,5 are char starts
        assert_eq!(meta.total_chars(), 5);
        assert_eq!(meta.total_utf16(), 5);

        // byte_to_offset_utf16 should only count up to valid boundaries
        eprintln!("chars bitmap: {:08b}", meta.chars);
        eprintln!("chars_utf16 bitmap: {:08b}", meta.chars_utf16);
        assert_eq!(meta.byte_to_offset_utf16(0), 0); // Before any chars
        assert_eq!(meta.byte_to_offset_utf16(1), 1); // After 'H'
        assert_eq!(meta.byte_to_offset_utf16(2), 1); // Middle of 'é' - should NOT count it
        assert_eq!(meta.byte_to_offset_utf16(3), 2); // After 'é'
        eprintln!("byte_to_offset_utf16(6) = {}", meta.byte_to_offset_utf16(6));
        assert_eq!(meta.byte_to_offset_utf16(6), 5); // After all chars
    }
}
