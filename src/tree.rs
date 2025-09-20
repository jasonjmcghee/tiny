//! Unified document tree with iterative operations and lock-free reads

use crate::coordinates::{DocPos, LayoutPos, LayoutRect, LogicalPixels};
use crate::widget::Widget;
use arc_swap::ArcSwap;
use crossbeam::queue::SegQueue;
use memchr::{memchr, memrchr};
use simdutf8::basic::from_utf8;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Maximum spans per leaf node (tuned for cache line)
const MAX_SPANS: usize = 16;

/// Auto-flush pending edits after this many operations
const FLUSH_THRESHOLD: usize = 16;

// === Core Types ===

/// The document - readers get immutable snapshots, writers buffer edits
pub struct Doc {
    /// Current immutable snapshot for readers (lock-free!)
    snapshot: ArcSwap<Tree>,
    /// Buffered edits waiting to be applied
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
    /// Cached flattened text representation for performance
    cached_flattened_text: Option<Arc<String>>,
}

/// Tree node - either leaf with spans or internal with children
#[derive(Clone)]
pub enum Node {
    Leaf { spans: Vec<Span>, sums: Sums },
    Internal { children: Vec<Node>, sums: Sums },
}

impl Node {
    fn leaf(spans: Vec<Span>) -> Self {
        Node::Leaf {
            sums: compute_sums(&spans),
            spans,
        }
    }

    fn internal(children: Vec<Node>) -> Self {
        Node::Internal {
            sums: compute_node_sums(&children),
            children,
        }
    }

    fn len(&self) -> usize {
        match self {
            Node::Leaf { spans, .. } => spans.len(),
            Node::Internal { children, .. } => children.len(),
        }
    }

    fn needs_split(&self) -> bool {
        self.len() > MAX_SPANS
    }

    fn split_if_needed(self) -> Self {
        if self.len() <= MAX_SPANS {
            return self;
        }
        let mid = self.len() / 2;
        match self {
            Node::Leaf { spans, .. } => {
                let (l, r) = spans.split_at(mid);
                Node::internal(vec![Node::leaf(l.to_vec()), Node::leaf(r.to_vec())])
            }
            Node::Internal { children, .. } => {
                let (l, r) = children.split_at(mid);
                Node::internal(vec![Node::internal(l.to_vec()), Node::internal(r.to_vec())])
            }
        }
    }
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
    pub bytes: usize,
    pub lines: u32,
    pub bounds: Rect,
    pub max_z: i32,
}

/// Edit operations
#[derive(Clone, Debug)]
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

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Text(s) => write!(f, "Text({:?})", s),
            Content::Widget(_) => write!(f, "Widget(...)"),
        }
    }
}

pub type Rect = LayoutRect;
pub type Point = LayoutPos;

// === Document Implementation ===

impl Doc {
    pub fn new() -> Self {
        Self {
            snapshot: ArcSwap::from_pointee(Tree::new()),
            pending: SegQueue::new(),
            pending_count: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }

    pub fn from_str(text: &str) -> Self {
        Self {
            snapshot: ArcSwap::from_pointee(Tree::from_str(text)),
            pending: SegQueue::new(),
            pending_count: AtomicUsize::new(0),
            version: AtomicU64::new(0),
        }
    }

    pub fn read(&self) -> Arc<Tree> {
        self.snapshot.load_full()
    }

    pub fn edit(&self, edit: Edit) {
        self.pending.push(edit);
        let count = self.pending_count.fetch_add(1, Ordering::Relaxed) + 1;

        if count >= FLUSH_THRESHOLD {
            self.flush();
        }
    }

    pub fn flush(&self) {
        let mut edits = Vec::new();
        while let Some(edit) = self.pending.pop() {
            edits.push(edit);
        }

        if edits.is_empty() {
            return;
        }

        self.pending_count.store(0, Ordering::Relaxed);

        let current = self.snapshot.load();
        let new_tree = current.apply_edits(&edits);
        self.version.store(new_tree.version, Ordering::Relaxed);

        self.snapshot.store(Arc::new(new_tree));
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Replace the current tree with a new one (for undo/redo)
    pub fn replace_tree(&self, tree: Arc<Tree>) {
        self.snapshot.store(tree);
        self.version.fetch_add(1, Ordering::Relaxed);
    }
}

// === Tree Implementation ===

impl Tree {
    pub fn new() -> Self {
        Self {
            root: Node::Leaf {
                spans: Vec::new(),
                sums: Sums::default(),
            },
            version: 0,
            cached_flattened_text: Some(Arc::new(String::new())), // Empty tree = empty string
        }
    }

    pub fn from_str(text: &str) -> Self {
        let bytes = text.as_bytes();

        if bytes.is_empty() {
            return Self {
                root: Node::Leaf {
                    spans: Vec::new(),
                    sums: Sums::default(),
                },
                version: 0,
                cached_flattened_text: Some(Arc::new(String::new())),
            };
        }

        // Build leaves, each with up to MAX_SPANS spans
        let mut all_leaves = Vec::new();
        let mut current_leaf_spans = Vec::<Span>::new();

        const CHUNK_SIZE: usize = 1024;
        let mut pos = 0;

        while pos < bytes.len() {
            let end = (pos + CHUNK_SIZE).min(bytes.len());
            // Find safe UTF-8 boundary
            let mut e = end;
            if e < bytes.len() {
                // Only need to find boundary if not at end
                while e > pos && (bytes[e] & 0b11000000) == 0b10000000 {
                    e -= 1;
                }
            }

            // Ensure we make progress
            if e <= pos {
                e = end; // Force progress even if boundary detection fails
            }

            let chunk = &bytes[pos..e];
            let lines = bytecount::count(chunk, b'\n') as u32;
            current_leaf_spans.push(Span::Text {
                bytes: Arc::from(chunk),
                lines,
            });

            // If leaf is full, create it and start a new one
            if current_leaf_spans.len() >= MAX_SPANS {
                all_leaves.push(Node::leaf(current_leaf_spans.clone()));
                current_leaf_spans = Vec::new();
            }

            pos = e;
        }

        // Don't forget the last leaf if it has spans
        if !current_leaf_spans.is_empty() {
            all_leaves.push(Node::leaf(current_leaf_spans));
        }

        // If only one leaf, use it as root
        if all_leaves.len() == 1 {
            return Self {
                root: all_leaves.into_iter().next().unwrap(),
                version: 0,
                cached_flattened_text: Some(Arc::new(text.to_string())),
            };
        }

        // Build internal nodes bottom-up
        let mut nodes = all_leaves;
        while nodes.len() > 1 {
            let mut next_level = Vec::new();
            let mut current_children = Vec::new();

            for node in nodes {
                current_children.push(node);
                if current_children.len() >= MAX_SPANS {
                    next_level.push(Node::internal(current_children.clone()));
                    current_children = Vec::new();
                }
            }

            if !current_children.is_empty() {
                next_level.push(Node::internal(current_children));
            }

            nodes = next_level;
        }

        Self {
            root: nodes.into_iter().next().unwrap(),
            version: 0,
            cached_flattened_text: Some(Arc::new(text.to_string())),
        }
    }

    /// Apply edits using incremental path-based approach
    pub fn apply_edits(&self, edits: &[Edit]) -> Self {
        // For single edit, use incremental approach
        if edits.len() == 1 {
            return self.apply_edit_incremental(&edits[0]);
        }

        // For multiple edits, batch by locality if possible
        let mut new_root = self.root.clone();
        for edit in edits {
            new_root = Self::apply_edit_to_node(new_root, edit);
            debug_assert!(
                validate_tree_structure(&new_root),
                "Tree structure invalid after edit: {:?}",
                edit
            );
        }

        Self {
            root: new_root,
            version: self.version + 1,
            cached_flattened_text: None, // Cache invalidated by edits
        }
    }

    /// Apply single edit incrementally
    fn apply_edit_incremental(&self, edit: &Edit) -> Self {
        let new_root = Self::apply_edit_to_node(self.root.clone(), edit);

        // Validate tree structure in debug builds
        debug_assert!(
            validate_tree_structure(&new_root),
            "Tree structure invalid after edit: {:?}",
            edit
        );

        Self {
            root: new_root,
            version: self.version + 1,
            cached_flattened_text: None, // Cache invalidated by edits
        }
    }

    /// Apply edit to a node, returning new node (copy-on-write)
    fn apply_edit_to_node(node: Node, edit: &Edit) -> Node {
        let result = match edit {
            Edit::Insert { pos, content } => Self::insert_at_node(node, *pos, content),
            Edit::Delete { range } => Self::delete_from_node(node, range),
            Edit::Replace { range, content } => {
                let node = Self::delete_from_node(node, range);
                Self::insert_at_node(node, range.start, content)
            }
        };
        result.split_if_needed()
    }

    fn create_span(content: &Content) -> Span {
        match content {
            Content::Text(s) => {
                let bytes = s.as_bytes();
                Span::Text {
                    bytes: bytes.into(),
                    lines: bytecount::count(bytes, b'\n') as u32,
                }
            }
            Content::Widget(w) => Span::Widget(w.clone()),
        }
    }

    fn text_span(bytes: &[u8]) -> Span {
        Span::Text {
            bytes: bytes.into(),
            lines: bytecount::count(bytes, b'\n') as u32,
        }
    }

    fn merge_text_spans(bytes1: &[u8], lines1: u32, text: &str) -> Span {
        let mut combined = Vec::with_capacity(bytes1.len() + text.len());
        combined.extend_from_slice(bytes1);
        combined.extend_from_slice(text.as_bytes());
        Span::Text {
            bytes: combined.into(),
            lines: lines1 + bytecount::count(text.as_bytes(), b'\n') as u32,
        }
    }

    fn insert_at_node(node: Node, pos: usize, content: &Content) -> Node {
        let total_bytes = node_metrics(&node).0;
        debug_assert!(
            pos <= total_bytes,
            "Insert position {} exceeds node size {}",
            pos,
            total_bytes
        );

        match node {
            Node::Leaf { mut spans, .. } => {
                let new_span = Self::create_span(content);

                // Try to merge at end for text
                if let (Content::Text(text), Some(Span::Text { bytes, lines })) =
                    (content, spans.last())
                {
                    if pos == spans.iter().map(span_bytes).sum::<usize>() {
                        let last = spans.len() - 1;
                        spans[last] = Self::merge_text_spans(bytes, *lines, text);
                        return Node::leaf(spans).split_if_needed();
                    }
                }

                let mut offset = 0;
                for (i, span) in spans.iter().enumerate() {
                    let size = span_bytes(span);
                    if offset <= pos && pos <= offset + size {
                        let split_pos = pos - offset;

                        if split_pos == 0 {
                            spans.insert(i, new_span);
                            return Node::leaf(spans).split_if_needed();
                        }

                        match span {
                            Span::Text { bytes, lines } if split_pos == bytes.len() => {
                                if let Content::Text(text) = content {
                                    spans[i] = Self::merge_text_spans(bytes, *lines, text);
                                } else {
                                    spans.insert(i + 1, new_span);
                                }
                                return Node::leaf(spans).split_if_needed();
                            }
                            Span::Text { bytes, .. } => {
                                let mut new_spans = Vec::with_capacity(spans.len() + 2);
                                new_spans.extend_from_slice(&spans[..i]);

                                let prefix = &bytes[..split_pos];
                                let suffix = &bytes[split_pos..];
                                if !prefix.is_empty() {
                                    new_spans.push(Self::text_span(prefix));
                                }
                                new_spans.push(new_span);
                                if !suffix.is_empty() {
                                    new_spans.push(Self::text_span(suffix));
                                }
                                new_spans.extend_from_slice(&spans[i + 1..]);
                                return Node::leaf(new_spans).split_if_needed();
                            }
                            _ => {
                                spans.insert(if split_pos == 0 { i } else { i + 1 }, new_span);
                                return Node::leaf(spans).split_if_needed();
                            }
                        }
                    }
                    offset += size;
                }
                spans.push(new_span);
                Node::leaf(spans).split_if_needed()
            }
            Node::Internal { mut children, .. } => {
                let mut byte_offset = 0;
                for i in 0..children.len() {
                    let (bytes, _) = node_metrics(&children[i]);
                    if byte_offset + bytes >= pos {
                        // Edit goes in this child
                        let old_child = std::mem::replace(
                            &mut children[i],
                            Node::Leaf {
                                spans: Vec::new(),
                                sums: Sums::default(),
                            },
                        );
                        let new_child = Self::apply_edit_to_node(
                            old_child,
                            &Edit::Insert {
                                pos: pos - byte_offset,
                                content: content.clone(),
                            },
                        );
                        children[i] = new_child;

                        // Check if child needs to be split
                        if children[i].needs_split() {
                            let node_to_split = std::mem::replace(
                                &mut children[i],
                                Node::Leaf {
                                    spans: Vec::new(),
                                    sums: Sums::default(),
                                },
                            );
                            let (left, right) = Self::split_node(node_to_split);
                            children[i] = left;
                            if i + 1 < children.len() {
                                children.insert(i + 1, right);
                            } else {
                                children.push(right);
                            }
                        }

                        return Node::internal(children).split_if_needed();
                    }
                    byte_offset += bytes;
                }

                // Shouldn't reach here
                Node::internal(children)
            }
        }
    }

    fn delete_from_node(node: Node, range: &Range<usize>) -> Node {
        if range.is_empty() {
            return node;
        }

        match node {
            Node::Leaf { spans, .. } => {
                let mut new_spans = Vec::new();
                let mut offset = 0;

                for span in &spans {
                    let size = span_bytes(span);
                    let end = offset + size;

                    if end <= range.start || offset >= range.end {
                        new_spans.push(span.clone());
                    } else if !(offset >= range.start && end <= range.end) {
                        if let Span::Text { bytes, .. } = span {
                            let start = range.start.saturating_sub(offset);
                            let stop = (range.end - offset).min(bytes.len());
                            if start > 0 {
                                new_spans.push(Self::text_span(&bytes[..start]));
                            }
                            if stop < bytes.len() {
                                new_spans.push(Self::text_span(&bytes[stop..]));
                            }
                        } else if offset < range.start || offset >= range.end {
                            new_spans.push(span.clone());
                        }
                    }
                    offset = end;
                }
                Node::leaf(new_spans).split_if_needed()
            }
            Node::Internal { children, .. } => {
                let mut offset = 0;
                let mut new_children = Vec::new();

                for child in &children {
                    let bytes = node_metrics(child).0;
                    let end = offset + bytes;

                    if end <= range.start || offset >= range.end {
                        new_children.push(child.clone());
                    } else if !(offset >= range.start && end <= range.end) {
                        let adj = Range {
                            start: range.start.saturating_sub(offset),
                            end: (range.end - offset).min(bytes),
                        };
                        new_children.push(Self::delete_from_node(child.clone(), &adj));
                    }
                    offset = end;
                }

                if new_children.len() < MAX_SPANS / 2 && new_children.len() > 1 {
                    new_children = Self::merge_children(new_children);
                }
                Node::internal(new_children)
            }
        }
    }

    fn split_node(node: Node) -> (Node, Node) {
        let mid = node.len() / 2;
        match node {
            Node::Leaf { spans, .. } => {
                let (l, r) = spans.split_at(mid);
                (Node::leaf(l.to_vec()), Node::leaf(r.to_vec()))
            }
            Node::Internal { children, .. } => {
                let (l, r) = children.split_at(mid);
                (Node::internal(l.to_vec()), Node::internal(r.to_vec()))
            }
        }
    }

    fn merge_children(children: Vec<Node>) -> Vec<Node> {
        if children.len() >= MAX_SPANS / 2 {
            return children;
        }

        let mut merged = Vec::new();
        let mut i = 0;

        while i < children.len() {
            if i + 1 < children.len() {
                let can_merge = match (&children[i], &children[i + 1]) {
                    (Node::Leaf { spans: s1, .. }, Node::Leaf { spans: s2, .. }) => {
                        s1.len() + s2.len() <= MAX_SPANS
                    }
                    (Node::Internal { children: c1, .. }, Node::Internal { children: c2, .. }) => {
                        c1.len() + c2.len() <= MAX_SPANS
                    }
                    _ => false,
                };

                if can_merge {
                    match (children[i].clone(), children[i + 1].clone()) {
                        (Node::Leaf { spans: mut s1, .. }, Node::Leaf { spans: s2, .. }) => {
                            s1.extend(s2);
                            merged.push(Node::leaf(s1));
                        }
                        (
                            Node::Internal {
                                children: mut c1, ..
                            },
                            Node::Internal { children: c2, .. },
                        ) => {
                            c1.extend(c2);
                            merged.push(Node::internal(c1));
                        }
                        _ => unreachable!(),
                    }
                    i += 2;
                    continue;
                }
            }
            merged.push(children[i].clone());
            i += 1;
        }
        merged
    }

    pub fn flatten_to_string(&self) -> Arc<String> {
        if let Some(ref cached) = self.cached_flattened_text {
            return Arc::clone(cached);
        }

        // This shouldn't happen if we initialize the cache properly, but fallback to computing
        let capacity = match &self.root {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        };
        let mut result = String::with_capacity(capacity);
        collect_text(&self.root, &mut result);
        Arc::new(result)
    }

    // === Navigation Methods (Iterative) ===

    pub fn cursor(&self) -> TreeCursor<'_> {
        TreeCursor::new(self)
    }

    pub fn byte_count(&self) -> usize {
        match &self.root {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        }
    }

    pub fn line_count(&self) -> u32 {
        match &self.root {
            Node::Leaf { sums, .. } => sums.lines,
            Node::Internal { sums, .. } => sums.lines,
        }
    }

    pub fn line_to_byte(&self, line: u32) -> Option<usize> {
        let mut cursor = self.cursor();
        cursor.seek_line(line)
    }

    pub fn byte_to_line(&self, byte: usize) -> u32 {
        let mut cursor = self.cursor();
        cursor.seek_byte(byte);
        cursor.current_line()
    }

    pub fn find_next_newline(&self, pos: usize) -> Option<usize> {
        let mut cursor = self.cursor();
        cursor.seek_byte(pos);
        cursor.find_byte(b'\n', true)
    }

    pub fn find_prev_newline(&self, pos: usize) -> Option<usize> {
        let mut cursor = self.cursor();
        cursor.seek_byte(pos);
        cursor.find_byte(b'\n', false)
    }

    pub fn get_text_slice(&self, range: Range<usize>) -> String {
        let mut cursor = self.cursor();
        cursor.seek_byte(range.start);
        cursor.read_text(range.len())
    }

    pub fn find_line_start_at(&self, pos: usize) -> usize {
        self.find_prev_newline(pos).map(|p| p + 1).unwrap_or(0)
    }

    pub fn find_line_end_at(&self, pos: usize) -> usize {
        self.find_next_newline(pos)
            .unwrap_or_else(|| self.byte_count())
    }

    pub fn get_line_at(&self, pos: usize) -> String {
        let start = self.find_prev_newline(pos).map(|p| p + 1).unwrap_or(0);
        let end = self
            .find_next_newline(pos)
            .unwrap_or_else(|| self.byte_count());
        self.get_text_slice(start..end)
    }

    pub fn char_count(&self) -> usize {
        let mut cursor = self.cursor();
        cursor.count_chars()
    }

    pub fn doc_pos_to_byte(&self, pos: DocPos) -> usize {
        self.doc_pos_to_byte_with_tab_width(pos, 4) // Default tab width
    }

    pub fn doc_pos_to_byte_with_tab_width(&self, pos: DocPos, tab_width: u32) -> usize {
        if let Some(line_start) = self.line_to_byte(pos.line) {
            let line_end = self.line_to_byte(pos.line + 1).unwrap_or(self.byte_count());
            let line_text = self.get_text_slice(line_start..line_end);

            let mut byte_offset = 0;
            let mut visual_column = 0;

            for ch in line_text.chars() {
                if visual_column >= pos.column {
                    break;
                }
                if ch == '\t' {
                    visual_column = ((visual_column / tab_width) + 1) * tab_width;
                } else {
                    visual_column += 1;
                }
                byte_offset += ch.len_utf8();
            }

            line_start + byte_offset
        } else {
            pos.byte_offset
        }
    }

    /// Get the text of a line (including newline if present)
    pub fn line_text(&self, line: u32) -> String {
        if let Some(start) = self.line_to_byte(line) {
            let end = self.line_to_byte(line + 1).unwrap_or(self.byte_count());
            self.get_text_slice(start..end)
        } else {
            String::new()
        }
    }

    /// Get the text of a line without trailing newline
    pub fn line_text_trimmed(&self, line: u32) -> String {
        self.line_text(line).trim_end_matches('\n').to_string()
    }

    /// Get the character count of a line (excluding newline)
    pub fn line_char_count(&self, line: u32) -> usize {
        self.line_text_trimmed(line).chars().count()
    }

    pub fn walk_visible_range<F>(&self, byte_range: Range<usize>, callback: F)
    where
        F: FnMut(&[Span], usize, usize),
    {
        let mut cursor = self.cursor();
        cursor.walk_range(byte_range, callback);
    }
}

// === Tree Cursor (Iterative Navigation) ===

pub struct TreeCursor<'a> {
    tree: &'a Tree,
    stack: Vec<CursorFrame<'a>>,           // Stack frames for traversal
    current_spans: Vec<(&'a Span, usize)>, // spans with byte offsets
    span_idx: usize,
    byte_pos: usize,
    line_pos: u32,
}

/// Stack frame for cursor traversal
struct CursorFrame<'a> {
    node: &'a Node,
    byte_offset: usize,
    line_offset: u32,
    child_index: usize,                  // Current child being processed
    children_offsets: Vec<(usize, u32)>, // Pre-computed child offsets
}

impl<'a> CursorFrame<'a> {
    fn new(node: &'a Node, byte_offset: usize, line_offset: u32) -> Self {
        let children_offsets = if let Node::Internal { children, .. } = node {
            let mut offsets = Vec::new();
            let mut byte_off = byte_offset;
            let mut line_off = line_offset;
            for child in children {
                offsets.push((byte_off, line_off));
                let (bytes, lines) = node_metrics(child);
                byte_off += bytes;
                line_off += lines;
            }
            offsets
        } else {
            Vec::new()
        };

        Self {
            node,
            byte_offset,
            line_offset,
            child_index: 0,
            children_offsets,
        }
    }

    fn advance_to_next_child(&mut self) -> Option<(&'a Node, usize, u32)> {
        if let Node::Internal { children, .. } = self.node {
            if self.child_index < children.len() {
                let result = (
                    &children[self.child_index],
                    self.children_offsets[self.child_index].0,
                    self.children_offsets[self.child_index].1,
                );
                self.child_index += 1;
                return Some(result);
            }
        }
        None
    }
}

impl<'a> TreeCursor<'a> {
    fn new(tree: &'a Tree) -> Self {
        let mut cursor = Self {
            tree,
            stack: Vec::new(),
            current_spans: Vec::new(),
            span_idx: 0,
            byte_pos: 0,
            line_pos: 0,
        };
        cursor.reset();
        cursor
    }

    fn reset(&mut self) {
        self.stack.clear();
        self.current_spans.clear();
        self.span_idx = 0;
        self.byte_pos = 0;
        self.line_pos = 0;

        // Create initial frame
        let frame = CursorFrame::new(&self.tree.root, 0, 0);
        self.stack.push(frame);
        self.descend_to_leaf();
    }

    fn setup_leaf(&mut self, spans: &'a [Span], byte_offset: usize, line_offset: u32) {
        self.current_spans.clear();
        let mut offset = byte_offset;
        for span in spans {
            self.current_spans.push((span, offset));
            offset += span_bytes(span);
        }
        self.byte_pos = byte_offset;
        self.line_pos = line_offset;
        self.span_idx = 0;
    }

    fn descend_to_leaf(&mut self) {
        while let Some(frame) = self.stack.pop() {
            if let Node::Leaf { spans, .. } = frame.node {
                self.setup_leaf(spans, frame.byte_offset, frame.line_offset);
                self.stack.push(frame);
                return;
            }

            self.stack.push(frame);
            if let Some(frame) = self.stack.last_mut() {
                if let Some((child, byte_off, line_off)) = frame.advance_to_next_child() {
                    self.stack.push(CursorFrame::new(child, byte_off, line_off));
                } else {
                    self.stack.pop();
                }
            }
        }
    }

    pub fn seek_byte(&mut self, target: usize) -> bool {
        self.stack.clear();
        self.current_spans.clear();
        self.span_idx = 0;
        self.byte_pos = 0;
        self.line_pos = 0;
        self.stack.push(CursorFrame::new(&self.tree.root, 0, 0));

        if target == 0 {
            self.descend_to_leaf();
            return true;
        }

        while let Some(frame) = self.stack.pop() {
            match frame.node {
                Node::Leaf { spans, .. } => {
                    self.setup_leaf(spans, frame.byte_offset, frame.line_offset);

                    let mut curr_byte = frame.byte_offset;
                    let mut curr_line = frame.line_offset;

                    for (i, span) in spans.iter().enumerate() {
                        let size = span_bytes(span);
                        if target < curr_byte + size {
                            self.byte_pos = target;
                            self.span_idx = i;
                            self.line_pos = curr_line + count_lines_to(span, target - curr_byte);
                            self.stack.push(frame);
                            return true;
                        }
                        curr_byte += size;
                        curr_line += span_lines(span);
                    }

                    if target == curr_byte {
                        self.byte_pos = target;
                        self.line_pos = curr_line;
                        self.span_idx = spans.len().saturating_sub(1);
                        self.stack.push(frame);
                        return true;
                    }
                }
                Node::Internal { children, .. } => {
                    for (i, &(byte_off, line_off)) in frame.children_offsets.iter().enumerate() {
                        if byte_off + node_metrics(&children[i]).0 > target {
                            self.stack
                                .push(CursorFrame::new(&children[i], byte_off, line_off));
                            break;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn seek_line(&mut self, target_line: u32) -> Option<usize> {
        if target_line == 0 {
            return Some(0);
        }
        if target_line > self.tree.line_count() {
            return None;
        }

        self.reset();
        let mut current_line = 0;

        loop {
            if self.current_spans.is_empty() {
                self.descend_to_leaf();
                if self.current_spans.is_empty() {
                    break;
                }
            }

            for (span, offset) in self.current_spans.iter() {
                if let Span::Text { bytes, lines } = span {
                    if current_line + lines >= target_line {
                        let skip = target_line - current_line;
                        if skip == 0 {
                            return Some(*offset);
                        }

                        let mut n = 0;
                        for (i, &b) in bytes.iter().enumerate() {
                            if b == b'\n' {
                                n += 1;
                                if n == skip {
                                    return Some(*offset + i + 1);
                                }
                            }
                        }
                    }
                    current_line += lines;
                }
            }

            if !self.advance_leaf() {
                break;
            }
        }
        None
    }

    pub fn find_byte(&mut self, target: u8, forward: bool) -> Option<usize> {
        let start_idx = self.span_idx;
        let start_pos = self.byte_pos;

        if forward {
            if let Some((span, offset)) = self.current_spans.get(start_idx) {
                if let Some(pos) = find_in_span(span, target, start_pos - offset, true) {
                    return Some(*offset + pos);
                }
            }

            for i in (start_idx + 1)..self.current_spans.len() {
                if let Some((span, offset)) = self.current_spans.get(i) {
                    if let Some(pos) = find_in_span(span, target, 0, true) {
                        return Some(*offset + pos);
                    }
                }
            }

            while self.advance_leaf() {
                for (span, offset) in &self.current_spans {
                    if let Some(pos) = find_in_span(span, target, 0, true) {
                        return Some(*offset + pos);
                    }
                }
            }
        } else {
            if let Some((span, offset)) = self.current_spans.get(start_idx) {
                let pos_in_span = start_pos.saturating_sub(*offset);
                if pos_in_span > 0 {
                    if let Some(pos) = find_in_span(span, target, pos_in_span, false) {
                        return Some(*offset + pos);
                    }
                }
            }

            for i in (0..start_idx).rev() {
                if let Some((span, offset)) = self.current_spans.get(i) {
                    if let Some(pos) = find_in_span(span, target, span_bytes(span), false) {
                        return Some(*offset + pos);
                    }
                }
            }
        }
        None
    }

    fn advance_leaf(&mut self) -> bool {
        if let Some(frame) = self.stack.last() {
            if matches!(frame.node, Node::Leaf { .. }) {
                self.stack.pop();
            }
        }

        loop {
            let Some(frame) = self.stack.last_mut() else {
                return false;
            };

            if let Some((child, byte_off, line_off)) = frame.advance_to_next_child() {
                self.stack.push(CursorFrame::new(child, byte_off, line_off));

                while let Some(frame) = self.stack.last() {
                    if let Node::Leaf { spans, .. } = frame.node {
                        self.setup_leaf(spans, frame.byte_offset, frame.line_offset);
                        return true;
                    }

                    let Some(mut frame) = self.stack.pop() else {
                        return false;
                    };
                    if let Some((child, byte_off, line_off)) = frame.advance_to_next_child() {
                        self.stack.push(frame);
                        self.stack.push(CursorFrame::new(child, byte_off, line_off));
                    } else {
                        return false;
                    }
                }
            } else {
                self.stack.pop();
                if self.stack.is_empty() {
                    return false;
                }
            }
        }
    }

    pub fn current_line(&self) -> u32 {
        self.line_pos
    }

    pub fn read_text(&mut self, len: usize) -> String {
        let mut result = String::with_capacity(len);
        let mut remaining = len;
        let mut idx = self.span_idx;
        let mut pos_in_span =
            self.byte_pos - self.current_spans.get(idx).map(|(_, o)| *o).unwrap_or(0);

        while remaining > 0 && idx < self.current_spans.len() {
            if let Some((span, _)) = self.current_spans.get(idx) {
                if let Span::Text { bytes, .. } = span {
                    let available = bytes.len() - pos_in_span;
                    let to_read = remaining.min(available);
                    let slice = &bytes[pos_in_span..pos_in_span + to_read];
                    // SAFETY: We maintain UTF-8 invariant
                    let text = unsafe { from_utf8(slice).unwrap_unchecked() };
                    result.push_str(text);
                    remaining -= to_read;
                    pos_in_span = 0;
                }
            }
            idx += 1;
        }
        result
    }

    pub fn count_chars(&mut self) -> usize {
        let mut count = 0;
        self.reset();

        loop {
            if self.current_spans.is_empty() {
                self.descend_to_leaf();
                if self.current_spans.is_empty() {
                    break;
                }
            }

            for (span, _) in &self.current_spans {
                if let Span::Text { bytes, .. } = span {
                    count += unsafe { from_utf8(bytes).unwrap_unchecked() }
                        .chars()
                        .count();
                }
            }

            if !self.advance_leaf() {
                break;
            }
        }
        count
    }

    pub fn walk_range<F>(&mut self, byte_range: Range<usize>, mut callback: F)
    where
        F: FnMut(&[Span], usize, usize),
    {
        self.seek_byte(byte_range.start);

        loop {
            if self.current_spans.is_empty() {
                break;
            }

            let start = self.current_spans.first().map(|(_, o)| *o).unwrap_or(0);
            let end = self
                .current_spans
                .last()
                .map(|(s, o)| o + span_bytes(s))
                .unwrap_or(start);

            if start >= byte_range.end {
                break;
            }

            if end > byte_range.start {
                let spans: Vec<_> = self
                    .current_spans
                    .iter()
                    .map(|(s, _)| (*s).clone())
                    .collect();
                callback(&spans, start.max(byte_range.start), end.min(byte_range.end));
            }

            if !self.advance_leaf() {
                break;
            }
        }
    }
}

// === Helper Functions ===

fn span_bytes(span: &Span) -> usize {
    match span {
        Span::Text { bytes, .. } => bytes.len(),
        Span::Widget(_) => 0,
    }
}

fn span_lines(span: &Span) -> u32 {
    match span {
        Span::Text { lines, .. } => *lines,
        Span::Widget(_) => 0,
    }
}

fn node_metrics(node: &Node) -> (usize, u32) {
    let sums = match node {
        Node::Leaf { sums, .. } | Node::Internal { sums, .. } => sums,
    };
    (sums.bytes, sums.lines)
}

fn count_lines_to(span: &Span, byte_offset: usize) -> u32 {
    match span {
        Span::Text { bytes, .. } => {
            bytecount::count(&bytes[..byte_offset.min(bytes.len())], b'\n') as u32
        }
        Span::Widget(_) => 0,
    }
}

fn find_in_span(span: &Span, target: u8, start: usize, forward: bool) -> Option<usize> {
    match span {
        Span::Text { bytes, .. } => {
            if forward {
                // Use SIMD-optimized memchr for forward search
                memchr(target, &bytes[start..]).map(|p| start + p)
            } else {
                // Use SIMD-optimized memrchr for reverse search
                memrchr(target, &bytes[..start])
            }
        }
        Span::Widget(_) => None,
    }
}

fn compute_sums(spans: &[Span]) -> Sums {
    let mut sums = Sums::default();

    for span in spans {
        match span {
            Span::Text { bytes, lines } => {
                sums.bytes += bytes.len();
                sums.lines += lines;
            }
            Span::Widget(w) => {
                let size = w.measure();
                sums.bounds.width = LogicalPixels(sums.bounds.width.0.max(size.width.0));
                sums.bounds.height = LogicalPixels(sums.bounds.height.0 + size.height.0);
                sums.max_z = sums.max_z.max(w.z_index());
            }
        }
    }

    sums
}

fn compute_node_sums(nodes: &[Node]) -> Sums {
    let mut sums = Sums::default();

    for node in nodes {
        let node_sums = match node {
            Node::Leaf { sums, .. } => sums,
            Node::Internal { sums, .. } => sums,
        };

        sums.bytes += node_sums.bytes;
        sums.lines += node_sums.lines;
        sums.bounds.width = LogicalPixels(sums.bounds.width.0.max(node_sums.bounds.width.0));
        sums.bounds.height = LogicalPixels(sums.bounds.height.0 + node_sums.bounds.height.0);
        sums.max_z = sums.max_z.max(node_sums.max_z);
    }

    sums
}

fn collect_text(node: &Node, out: &mut String) {
    match node {
        Node::Leaf { spans, .. } => {
            for span in spans {
                if let Span::Text { bytes, .. } = span {
                    let text = unsafe { from_utf8(bytes).unwrap_unchecked() };
                    out.push_str(text);
                }
            }
        }
        Node::Internal { children, .. } => {
            for child in children {
                collect_text(child, out);
            }
        }
    }
}

/// Validate tree structure invariants (debug builds only)
#[cfg(debug_assertions)]
fn validate_tree_structure(node: &Node) -> bool {
    match node {
        Node::Leaf { spans, sums } => {
            // Check spans don't exceed MAX_SPANS
            if spans.len() > MAX_SPANS {
                eprintln!(
                    "Leaf has {} spans, exceeds MAX_SPANS ({})",
                    spans.len(),
                    MAX_SPANS
                );
                return false;
            }

            // Verify sums match actual content
            let computed_sums = compute_sums(spans);
            if sums.bytes != computed_sums.bytes || sums.lines != computed_sums.lines {
                eprintln!("Leaf sums mismatch: stored ({} bytes, {} lines) vs computed ({} bytes, {} lines)",
                    sums.bytes, sums.lines, computed_sums.bytes, computed_sums.lines);
                return false;
            }

            true
        }
        Node::Internal { children, sums } => {
            // Check children don't exceed MAX_SPANS
            if children.len() > MAX_SPANS {
                eprintln!(
                    "Internal node has {} children, exceeds MAX_SPANS ({})",
                    children.len(),
                    MAX_SPANS
                );
                return false;
            }

            // Recursively validate children
            for child in children {
                if !validate_tree_structure(child) {
                    return false;
                }
            }

            // Verify sums match children
            let computed_sums = compute_node_sums(children);
            if sums.bytes != computed_sums.bytes || sums.lines != computed_sums.lines {
                eprintln!("Internal node sums mismatch: stored ({} bytes, {} lines) vs computed ({} bytes, {} lines)",
                    sums.bytes, sums.lines, computed_sums.bytes, computed_sums.lines);
                return false;
            }

            true
        }
    }
}

#[cfg(not(debug_assertions))]
fn validate_tree_structure(_node: &Node) -> bool {
    true // No-op in release builds
}
