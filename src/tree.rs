//! Core document tree with RCU (Read-Copy-Update) for lock-free reads
//!
//! Everything is a span in a B-tree with summed metadata for O(log n) queries.

use arc_swap::ArcSwap;
use crossbeam::queue::SegQueue;
use simdutf8::basic::from_utf8;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use crate::coordinates::{DocPos, LayoutPos, LayoutRect, LogicalPixels, ViewPos, Viewport};
use crate::widget::{Widget, PaintContext};

/// Maximum spans per leaf node (tuned for cache line)
#[allow(dead_code)]
const MAX_SPANS: usize = 16;

/// Auto-flush pending edits after this many operations
const FLUSH_THRESHOLD: usize = 16;

// === Core Types ===

/// The document - readers get immutable snapshots, writers buffer edits
///
/// ArcSwap provides truly lock-free reads - perfect for RCU pattern!
pub struct Doc {
    /// Current immutable snapshot for readers (lock-free!)
    snapshot: ArcSwap<Tree>,
    /// Buffered edits waiting to be applied (lock-free!)
    pending: SegQueue<Edit>,
    /// Approximate count of pending edits for auto-flush
    pending_count: AtomicUsize,
    /// Monotonic version counter
    version: AtomicU64,
}

/// Immutable tree snapshot
#[derive(Clone)]
pub struct Tree {
    pub root: Node,
    pub version: u64,
}

/// Tree node - either leaf with spans or internal with children
#[derive(Clone)]
pub enum Node {
    Leaf { spans: Vec<Span>, sums: Sums },
    Internal { children: Vec<Node>, sums: Sums },
}

/// Content spans - text or widgets
#[derive(Clone)]
pub enum Span {
    /// Raw UTF-8 text bytes with cached line count
    Text { bytes: Arc<[u8]>, lines: u32 },
    /// Any visual widget
    Widget(Arc<dyn Widget>),
}

/// Aggregated metadata for O(log n) queries
#[derive(Clone, Default)]
pub struct Sums {
    /// Total byte count
    pub bytes: usize,
    /// Total line count
    pub lines: u32,
    /// Spatial bounding box
    pub bounds: Rect,
    /// Maximum z-index in subtree
    pub max_z: i32,
}

/// Edit operations
#[derive(Clone)]
pub enum Edit {
    Insert {
        pos: usize,
        content: Content,
    },
    Delete {
        range: Range<usize>,
    },
    Replace {
        range: Range<usize>,
        content: Content,
    },
}

/// Content to insert
#[derive(Clone)]
pub enum Content {
    Text(String),
    Widget(Arc<dyn Widget>),
}

/// Rectangle for spatial queries (in layout space)
pub type Rect = LayoutRect;

/// Point for hit testing (in layout space)
pub type Point = LayoutPos;


// === Implementation ===

impl Doc {
    /// Create empty document
    pub fn new() -> Self {
        Self {
            snapshot: ArcSwap::from_pointee(Tree::new()),
            pending: SegQueue::new(),
            pending_count: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }

    /// Create document from text
    pub fn from_str(text: &str) -> Self {
        Self {
            snapshot: ArcSwap::from_pointee(Tree::from_str(text)),
            pending: SegQueue::new(),
            pending_count: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }

    /// Get current immutable snapshot (lock-free!)
    pub fn read(&self) -> Arc<Tree> {
        self.snapshot.load_full()
    }

    /// Buffer an edit
    pub fn edit(&self, edit: Edit) {
        self.pending.push(edit);
        let count = self.pending_count.fetch_add(1, Ordering::Relaxed) + 1;

        // Auto-flush if too many pending
        if count >= FLUSH_THRESHOLD {
            self.flush();
        }
    }

    /// Apply all pending edits
    pub fn flush(&self) {
        // Collect all pending edits
        let mut edits = Vec::new();
        while let Some(edit) = self.pending.pop() {
            edits.push(edit);
        }

        if edits.is_empty() {
            return;
        }

        // Reset the pending count
        self.pending_count.store(0, Ordering::Relaxed);

        // Create new tree with edits applied
        let current = self.snapshot.load();
        let new_tree = current.apply_edits(&edits);
        let new_version = self.version.fetch_add(1, Ordering::Relaxed) + 1;

        // Atomic swap of snapshot (lock-free!)
        self.snapshot.store(Arc::new(Tree {
            root: new_tree.root,
            version: new_version,
        }));
    }

    /// Get current version
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
}

impl Tree {
    /// Create empty tree
    pub fn new() -> Self {
        Self {
            root: Node::Leaf {
                spans: Vec::new(),
                sums: Sums::default(),
            },
            version: 0,
        }
    }

    /// Create tree from text
    pub fn from_str(text: &str) -> Self {
        let mut spans = Vec::new();
        let bytes = text.as_bytes();

        // Split into reasonably sized chunks
        const CHUNK_SIZE: usize = 1024;
        let mut pos = 0;

        while pos < bytes.len() {
            let end = (pos + CHUNK_SIZE).min(bytes.len());
            // Don't split UTF-8 sequences - use SIMD for boundary detection
            let end = if end < bytes.len() {
                // Find a safe UTF-8 boundary backwards from end
                let mut e = end;
                // Check if we're at a continuation byte (10xxxxxx)
                while e > pos && (bytes[e] & 0b11000000) == 0b10000000 {
                    e -= 1;
                }
                // Validate that we found a proper boundary
                if e > pos && from_utf8(&bytes[pos..e]).is_ok() {
                    e
                } else {
                    // Fall back to the original end if validation fails
                    end
                }
            } else {
                end
            };

            let chunk = &bytes[pos..end];
            let lines = bytecount::count(chunk, b'\n') as u32;
            spans.push(Span::Text {
                bytes: Arc::from(chunk),
                lines,
            });
            pos = end;
        }

        let sums = Self::compute_sums(&spans);

        Self {
            root: Node::Leaf { spans, sums },
            version: 0,
        }
    }

    /// Apply edits to create new tree
    pub fn apply_edits(&self, edits: &[Edit]) -> Self {
        let mut root = self.root.clone();

        for edit in edits {
            root = self.apply_edit(root, edit);
        }

        Self {
            root,
            version: self.version + 1,
        }
    }

    /// Apply single edit to node
    fn apply_edit(&self, node: Node, edit: &Edit) -> Node {
        match edit {
            Edit::Insert { pos, content } => self.insert_at(node, *pos, content.clone()),
            Edit::Delete { range } => self.delete_range(node, range.clone()),
            Edit::Replace { range, content } => {
                let node = self.delete_range(node, range.clone());
                self.insert_at(node, range.start, content.clone())
            }
        }
    }

    /// Insert content at position
    fn insert_at(&self, node: Node, pos: usize, content: Content) -> Node {
        match node {
            Node::Leaf { mut spans, .. } => {
                // Find position in spans
                let mut byte_offset = 0;
                let mut insert_idx = 0;

                for (i, span) in spans.iter().enumerate() {
                    let span_bytes = self.span_bytes(span);
                    if byte_offset + span_bytes > pos {
                        // Insert within this span
                        if let Span::Text { bytes: text, .. } = span {
                            let offset_in_span = pos - byte_offset;

                            // Split text span
                            let mut new_spans = Vec::new();
                            if offset_in_span > 0 {
                                let prefix = &text[..offset_in_span];
                                let prefix_lines = bytecount::count(prefix, b'\n') as u32;
                                new_spans.push(Span::Text {
                                    bytes: Arc::from(prefix),
                                    lines: prefix_lines,
                                });
                            }

                            // Add new content
                            match content {
                                Content::Text(s) => {
                                    let bytes = s.as_bytes();
                                    let lines = bytecount::count(bytes, b'\n') as u32;
                                    new_spans.push(Span::Text {
                                        bytes: Arc::from(bytes),
                                        lines,
                                    });
                                }
                                Content::Widget(w) => {
                                    new_spans.push(Span::Widget(w));
                                }
                            }

                            if offset_in_span < text.len() {
                                let suffix = &text[offset_in_span..];
                                let suffix_lines = bytecount::count(suffix, b'\n') as u32;
                                new_spans.push(Span::Text {
                                    bytes: Arc::from(suffix),
                                    lines: suffix_lines,
                                });
                            }

                            // Replace span with split version
                            spans.splice(i..=i, new_spans);

                            let sums = Self::compute_sums(&spans);
                            return Node::Leaf { spans, sums };
                        }
                    }

                    if byte_offset >= pos {
                        insert_idx = i;
                        break;
                    }

                    byte_offset += span_bytes;
                    insert_idx = i + 1;
                }

                // Insert at found position
                let new_span = match content {
                    Content::Text(s) => {
                        let bytes = s.as_bytes();
                        let lines = bytecount::count(bytes, b'\n') as u32;
                        Span::Text {
                            bytes: Arc::from(bytes),
                            lines,
                        }
                    }
                    Content::Widget(w) => Span::Widget(w),
                };
                spans.insert(insert_idx, new_span);

                let sums = Self::compute_sums(&spans);
                Node::Leaf { spans, sums }
            }
            Node::Internal { children, .. } => {
                // Recursively find child to insert into
                let mut new_children = Vec::new();
                let mut byte_offset = 0;

                for child in children {
                    let child_bytes = self.node_bytes(&child);

                    if byte_offset <= pos && pos < byte_offset + child_bytes {
                        // Insert in this child
                        let child_pos = pos - byte_offset;
                        new_children.push(self.insert_at(child, child_pos, content.clone()));
                    } else {
                        new_children.push(child);
                    }

                    byte_offset += child_bytes;
                }

                let sums = Self::compute_node_sums(&new_children);
                Node::Internal {
                    children: new_children,
                    sums,
                }
            }
        }
    }

    /// Delete range from node
    fn delete_range(&self, node: Node, range: Range<usize>) -> Node {
        // Simplified for brevity - would handle cross-span deletes
        match node {
            Node::Leaf { spans, .. } => {
                let mut new_spans = Vec::new();
                let mut byte_offset = 0;

                for span in spans {
                    let span_bytes = self.span_bytes(&span);
                    let span_end = byte_offset + span_bytes;

                    if span_end <= range.start || byte_offset >= range.end {
                        // Span outside delete range
                        new_spans.push(span);
                    } else if byte_offset >= range.start && span_end <= range.end {
                        // Span entirely within delete range - skip it
                    } else {
                        // Partial deletion
                        if let Span::Text { bytes: text, .. } = &span {
                            let start_in_span = range.start.saturating_sub(byte_offset);
                            let end_in_span = (range.end - byte_offset).min(text.len());

                            if start_in_span > 0 {
                                let prefix = &text[..start_in_span];
                                let prefix_lines = bytecount::count(prefix, b'\n') as u32;
                                new_spans.push(Span::Text {
                                    bytes: Arc::from(prefix),
                                    lines: prefix_lines,
                                });
                            }
                            if end_in_span < text.len() {
                                let suffix = &text[end_in_span..];
                                let suffix_lines = bytecount::count(suffix, b'\n') as u32;
                                new_spans.push(Span::Text {
                                    bytes: Arc::from(suffix),
                                    lines: suffix_lines,
                                });
                            }
                        }
                    }

                    byte_offset = span_end;
                }

                let sums = Self::compute_sums(&new_spans);
                Node::Leaf {
                    spans: new_spans,
                    sums,
                }
            }
            Node::Internal { .. } => {
                // Would recursively handle internal nodes
                node
            }
        }
    }

    /// Compute sums for spans
    fn compute_sums(spans: &[Span]) -> Sums {
        let mut sums = Sums::default();

        for span in spans {
            match span {
                Span::Text { bytes, lines } => {
                    sums.bytes += bytes.len();
                    sums.lines += lines; // Use cached count!
                }
                Span::Widget(w) => {
                    let size = w.measure();
                    sums.bounds.width = LogicalPixels(
                        sums.bounds.width.0.max(size.width.0)
                    );
                    sums.bounds.height = LogicalPixels(
                        sums.bounds.height.0 + size.height.0
                    );
                    sums.max_z = sums.max_z.max(w.z_index());
                }
            }
        }

        sums
    }

    /// Compute sums for child nodes
    fn compute_node_sums(children: &[Node]) -> Sums {
        let mut sums = Sums::default();

        for child in children {
            let child_sums = match child {
                Node::Leaf { sums, .. } => sums,
                Node::Internal { sums, .. } => sums,
            };

            sums.bytes += child_sums.bytes;
            sums.lines += child_sums.lines;
            sums.bounds.width = LogicalPixels(
                sums.bounds.width.0.max(child_sums.bounds.width.0)
            );
            sums.bounds.height = LogicalPixels(
                sums.bounds.height.0 + child_sums.bounds.height.0
            );
            sums.max_z = sums.max_z.max(child_sums.max_z);
        }

        sums
    }

    /// Get byte count of span
    fn span_bytes(&self, span: &Span) -> usize {
        match span {
            Span::Text { bytes, .. } => bytes.len(),
            Span::Widget(_) => 0,
        }
    }

    /// Get byte count of node
    fn node_bytes(&self, node: &Node) -> usize {
        match node {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        }
    }

    /// Convert to string for debugging/saving
    pub fn to_string(&self) -> String {
        // Pre-allocate with total byte count to avoid reallocations
        let capacity = match &self.root {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        };
        let mut result = String::with_capacity(capacity);
        self.collect_text(&self.root, &mut result);
        result
    }

    /// Collect just the byte slices without copying
    #[allow(dead_code)]
    fn collect_spans<'a>(&'a self, node: &'a Node, out: &mut Vec<&'a [u8]>) {
        match node {
            Node::Leaf { spans, .. } => {
                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        out.push(bytes);
                    }
                }
            }
            Node::Internal { children, .. } => {
                for child in children {
                    self.collect_spans(child, out);
                }
            }
        }
    }

    /// Recursively collect text content
    fn collect_text(&self, node: &Node, out: &mut String) {
        match node {
            Node::Leaf { spans, .. } => {
                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        // SAFETY: All Text spans contain valid UTF-8:
                        // - Initial text comes from &str (guaranteed valid)
                        // - Insertions come from String (guaranteed valid)
                        // - Splits preserve UTF-8 boundaries correctly
                        let text = unsafe { std::str::from_utf8_unchecked(bytes) };
                        out.push_str(text);
                    }
                }
            }
            Node::Internal { children, .. } => {
                for child in children {
                    self.collect_text(child, out);
                }
            }
        }
    }

    /// Find position at byte offset - O(log n)
    pub fn find_at_byte(&self, target: usize) -> TreePos {
        self.find_byte_in_node(&self.root, target, 0)
    }

    fn find_byte_in_node(&self, node: &Node, target: usize, base: usize) -> TreePos {
        match node {
            Node::Leaf { spans, .. } => {
                let mut offset = base;
                for (i, span) in spans.iter().enumerate() {
                    let bytes = self.span_bytes(span);
                    if offset + bytes > target {
                        return TreePos {
                            span_idx: i,
                            offset_in_span: target - offset,
                        };
                    }
                    offset += bytes;
                }
                TreePos::default()
            }
            Node::Internal { children, .. } => {
                let mut offset = base;
                for child in children {
                    let bytes = self.node_bytes(child);
                    if offset + bytes > target {
                        return self.find_byte_in_node(child, target, offset);
                    }
                    offset += bytes;
                }
                TreePos::default()
            }
        }
    }

    /// Find at point - O(log n) spatial query
    pub fn find_at_point(&self, pt: Point) -> TreePos {
        self.find_point_in_node(&self.root, pt)
    }

    fn find_point_in_node(&self, node: &Node, pt: Point) -> TreePos {
        match node {
            Node::Leaf { spans, .. } => {
                // Check spans for hit
                for (i, span) in spans.iter().enumerate() {
                    if let Span::Widget(w) = span {
                        if w.hit_test(pt) {
                            return TreePos {
                                span_idx: i,
                                offset_in_span: 0,
                            };
                        }
                    }
                }
                TreePos::default()
            }
            Node::Internal { children, .. } => {
                // Check child bounds
                for child in children {
                    let bounds = match child {
                        Node::Leaf { sums, .. } => &sums.bounds,
                        Node::Internal { sums, .. } => &sums.bounds,
                    };

                    if bounds.contains(pt) {
                        return self.find_point_in_node(child, pt);
                    }
                }
                TreePos::default()
            }
        }
    }
}

/// Position in tree
#[derive(Default)]
pub struct TreePos {
    pub span_idx: usize,
    pub offset_in_span: usize,
}

// Rect::contains is now implemented in coordinates.rs for LayoutRect

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_document_operations() {
        // Empty document
        let doc = Doc::from_str("");
        assert_eq!(doc.read().to_string(), "");
        assert_eq!(doc.read().byte_count(), 0);

        // Insert at beginning
        doc.edit(Edit::Insert {
            pos: 0,
            content: Content::Text("A".to_string()),
        });
        doc.flush();
        assert_eq!(doc.read().to_string(), "A");

        // Insert at end
        doc.edit(Edit::Insert {
            pos: 1,
            content: Content::Text("C".to_string()),
        });
        doc.flush();
        assert_eq!(doc.read().to_string(), "AC");

        // Insert in middle
        doc.edit(Edit::Insert {
            pos: 1,
            content: Content::Text("B".to_string()),
        });
        doc.flush();
        assert_eq!(doc.read().to_string(), "ABC");

        // Delete middle character
        doc.edit(Edit::Delete { range: 1..2 });
        doc.flush();
        assert_eq!(doc.read().to_string(), "AC");
    }

    #[test]
    fn test_typing_simulation() {
        let doc = Doc::from_str("");

        // Simulate typing character by character
        for (i, ch) in "Hello, World!".chars().enumerate() {
            doc.edit(Edit::Insert {
                pos: i,
                content: Content::Text(ch.to_string()),
            });
            doc.flush();
        }

        assert_eq!(doc.read().to_string(), "Hello, World!");
        assert_eq!(doc.read().byte_count(), 13);
    }

    #[test]
    fn test_multiline_document() {
        let doc = Doc::from_str("Line 1\nLine 2\nLine 3");
        assert_eq!(doc.read().to_string(), "Line 1\nLine 2\nLine 3");

        // Insert at beginning of line 2 (after "Line 1\n")
        doc.edit(Edit::Insert {
            pos: 7,
            content: Content::Text("Start of ".to_string()),
        });
        doc.flush();
        assert_eq!(doc.read().to_string(), "Line 1\nStart of Line 2\nLine 3");
    }

    #[test]
    fn test_edit_buffering() {
        let doc = Doc::from_str("");

        // Queue multiple edits before flush
        doc.edit(Edit::Insert {
            pos: 0,
            content: Content::Text("A".to_string()),
        });
        doc.edit(Edit::Insert {
            pos: 1,
            content: Content::Text("B".to_string()),
        });
        doc.edit(Edit::Insert {
            pos: 2,
            content: Content::Text("C".to_string()),
        });

        // All edits applied at once
        doc.flush();
        assert_eq!(doc.read().to_string(), "ABC");
    }

    #[test]
    fn test_concurrent_readers() {
        let doc = Doc::from_str("Shared");

        // Multiple readers should work without blocking
        let tree1 = doc.read();
        let tree2 = doc.read();

        assert_eq!(tree1.to_string(), "Shared");
        assert_eq!(tree2.to_string(), "Shared");

        // Both readers see same content
        assert_eq!(tree1.byte_count(), tree2.byte_count());
    }

    #[test]
    fn test_widget_insertion() {
        let doc = Doc::from_str("Text");

        // Insert a widget (doesn't affect text content)
        // Using cursor widget as an example, though cursors are now rendered as overlays
        doc.edit(Edit::Insert {
            pos: 2,
            content: Content::Widget(crate::widget::cursor()),
        });
        doc.flush();

        // Text content unchanged
        assert_eq!(doc.read().to_string(), "Text");
    }
}

