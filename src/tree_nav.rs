//! Navigation methods for the document tree - O(log n) operations
//!
//! This module extends the Tree with efficient navigation methods similar to a rope,
//! leveraging our sum tree structure for fast queries without full traversal.

use crate::tree::{Node, Span, Tree};
use crate::coordinates::DocPos;
use std::ops::Range;

impl Tree {
    /// Get total byte count - O(1)
    pub fn byte_count(&self) -> usize {
        match &self.root {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        }
    }

    /// Get total line count - O(1)
    pub fn line_count(&self) -> u32 {
        match &self.root {
            Node::Leaf { sums, .. } => sums.lines,
            Node::Internal { sums, .. } => sums.lines,
        }
    }

    /// Find byte position at start of line - O(log n)
    pub fn line_to_byte(&self, target_line: u32) -> Option<usize> {
        if target_line == 0 {
            return Some(0);
        }
        if target_line > self.line_count() {
            return None;
        }
        self.find_line_start_in_node(&self.root, target_line, 0, 0)
    }

    fn find_line_start_in_node(
        &self,
        node: &Node,
        target_line: u32,
        base_byte: usize,
        base_line: u32,
    ) -> Option<usize> {
        match node {
            Node::Leaf { spans, .. } => {
                let mut byte_offset = base_byte;
                let mut line_offset = base_line;

                for span in spans {
                    if let Span::Text { bytes, lines } = span {
                        if line_offset + lines >= target_line {
                            // Target line is in this span
                            let lines_to_skip = target_line - line_offset;
                            if lines_to_skip == 0 {
                                return Some(byte_offset);
                            }

                            // Find the nth newline in this span
                            let mut newline_count = 0;
                            for (i, &b) in bytes.iter().enumerate() {
                                if b == b'\n' {
                                    newline_count += 1;
                                    if newline_count == lines_to_skip {
                                        return Some(byte_offset + i + 1);
                                    }
                                }
                            }
                        }
                        byte_offset += bytes.len();
                        line_offset += lines;
                    }
                }
                None
            }
            Node::Internal { children, sums: _ } => {
                let mut byte_offset = base_byte;
                let mut line_offset = base_line;

                for child in children {
                    let (child_bytes, child_lines) = match child {
                        Node::Leaf { sums, .. } => (sums.bytes, sums.lines),
                        Node::Internal { sums, .. } => (sums.bytes, sums.lines),
                    };

                    if line_offset + child_lines >= target_line {
                        return self.find_line_start_in_node(
                            child,
                            target_line,
                            byte_offset,
                            line_offset,
                        );
                    }

                    byte_offset += child_bytes;
                    line_offset += child_lines;
                }
                None
            }
        }
    }

    /// Convert byte position to line number - O(log n)
    pub fn byte_to_line(&self, target_byte: usize) -> u32 {
        if target_byte == 0 {
            return 0;
        }
        self.byte_to_line_in_node(&self.root, target_byte, 0, 0)
            .unwrap_or(0)
    }

    fn byte_to_line_in_node(
        &self,
        node: &Node,
        target_byte: usize,
        base_byte: usize,
        base_line: u32,
    ) -> Option<u32> {
        match node {
            Node::Leaf { spans, .. } => {
                let mut byte_offset = base_byte;
                let mut line_count = base_line;

                for span in spans {
                    if let Span::Text { bytes, lines } = span {
                        let span_end = byte_offset + bytes.len();

                        if span_end > target_byte {
                            // Target is in this span - count lines up to target
                            let bytes_in_span = target_byte - byte_offset;
                            for &b in &bytes[..bytes_in_span] {
                                if b == b'\n' {
                                    line_count += 1;
                                }
                            }
                            return Some(line_count);
                        }

                        byte_offset = span_end;
                        line_count += lines;
                    }
                }
                Some(line_count)
            }
            Node::Internal { children, .. } => {
                let mut byte_offset = base_byte;
                let mut line_count = base_line;

                for child in children {
                    let (child_bytes, child_lines) = match child {
                        Node::Leaf { sums, .. } => (sums.bytes, sums.lines),
                        Node::Internal { sums, .. } => (sums.bytes, sums.lines),
                    };

                    if byte_offset + child_bytes > target_byte {
                        return self.byte_to_line_in_node(
                            child,
                            target_byte,
                            byte_offset,
                            line_count,
                        );
                    }

                    byte_offset += child_bytes;
                    line_count += child_lines;
                }
                Some(line_count)
            }
        }
    }

    /// Find next newline position after given byte - O(log n)
    pub fn find_next_newline(&self, pos: usize) -> Option<usize> {
        self.find_next_newline_in_node(&self.root, pos, 0)
    }

    fn find_next_newline_in_node(
        &self,
        node: &Node,
        target_pos: usize,
        base: usize,
    ) -> Option<usize> {
        match node {
            Node::Leaf { spans, .. } => {
                let mut offset = base;

                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        let span_end = offset + bytes.len();

                        if span_end > target_pos {
                            // Search in this span from target position
                            let start_in_span = target_pos.saturating_sub(offset);
                            for (i, &b) in bytes[start_in_span..].iter().enumerate() {
                                if b == b'\n' {
                                    return Some(offset + start_in_span + i);
                                }
                            }
                        }
                        offset = span_end;
                    }
                }
                None
            }
            Node::Internal { children, .. } => {
                let mut offset = base;

                for child in children {
                    let child_bytes = match child {
                        Node::Leaf { sums, .. } => sums.bytes,
                        Node::Internal { sums, .. } => sums.bytes,
                    };
                    let child_end = offset + child_bytes;

                    if child_end > target_pos {
                        if let Some(pos) = self.find_next_newline_in_node(child, target_pos, offset)
                        {
                            return Some(pos);
                        }
                    }
                    offset = child_end;
                }
                None
            }
        }
    }

    /// Find previous newline position before given byte - O(log n)
    pub fn find_prev_newline(&self, pos: usize) -> Option<usize> {
        if pos == 0 {
            return None;
        }
        self.find_prev_newline_in_node(&self.root, pos, 0)
    }

    fn find_prev_newline_in_node(
        &self,
        node: &Node,
        target_pos: usize,
        base: usize,
    ) -> Option<usize> {
        match node {
            Node::Leaf { spans, .. } => {
                let mut offset = base;
                let mut last_newline = None;

                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        let span_end = offset + bytes.len();

                        // Stop if we've passed the target
                        if offset >= target_pos {
                            break;
                        }

                        // Search this span for newlines before target_pos
                        let end_in_span = if span_end > target_pos {
                            target_pos - offset
                        } else {
                            bytes.len()
                        };

                        for (i, &b) in bytes[..end_in_span].iter().enumerate() {
                            if b == b'\n' {
                                last_newline = Some(offset + i);
                            }
                        }

                        offset = span_end;
                    }
                }
                last_newline
            }
            Node::Internal { children, .. } => {
                let mut offset = base;
                let mut last_newline = None;

                for child in children {
                    let child_bytes = match child {
                        Node::Leaf { sums, .. } => sums.bytes,
                        Node::Internal { sums, .. } => sums.bytes,
                    };
                    let child_end = offset + child_bytes;

                    if offset >= target_pos {
                        // We've passed the target
                        break;
                    }

                    if child_end > target_pos {
                        // Target is in this child
                        return self.find_prev_newline_in_node(child, target_pos, offset);
                    } else {
                        // Check entire child for newlines
                        if let Some(pos) = self.find_last_newline_in_node(child, offset) {
                            last_newline = Some(pos);
                        }
                    }

                    offset = child_end;
                }
                last_newline
            }
        }
    }

    /// Helper: find last newline in a node
    fn find_last_newline_in_node(&self, node: &Node, base: usize) -> Option<usize> {
        match node {
            Node::Leaf { spans, .. } => {
                let mut offset = base;
                let mut last_newline = None;

                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        for (i, &b) in bytes.iter().enumerate() {
                            if b == b'\n' {
                                last_newline = Some(offset + i);
                            }
                        }
                        offset += bytes.len();
                    }
                }
                last_newline
            }
            Node::Internal { children, .. } => {
                let mut offset = base;
                let mut last_newline = None;

                for child in children {
                    let child_bytes = match child {
                        Node::Leaf { sums, .. } => sums.bytes,
                        Node::Internal { sums, .. } => sums.bytes,
                    };

                    if let Some(pos) = self.find_last_newline_in_node(child, offset) {
                        last_newline = Some(pos);
                    }

                    offset += child_bytes;
                }
                last_newline
            }
        }
    }

    /// Get text slice for a byte range - O(log n + k) where k is output size
    pub fn get_text_slice(&self, range: Range<usize>) -> String {
        if range.start >= range.end {
            return String::new();
        }

        let mut result = String::with_capacity(range.len());
        self.collect_text_range(&self.root, range, 0, &mut result);
        result
    }

    fn collect_text_range(&self, node: &Node, range: Range<usize>, base: usize, out: &mut String) {
        match node {
            Node::Leaf { spans, .. } => {
                let mut offset = base;

                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        let span_end = offset + bytes.len();

                        // Check if this span overlaps with our range
                        if span_end > range.start && offset < range.end {
                            let start_in_span = range.start.saturating_sub(offset);
                            let end_in_span = (range.end - offset).min(bytes.len());

                            if start_in_span < end_in_span {
                                // SAFETY: We maintain UTF-8 invariant in all text spans
                                let slice = unsafe {
                                    std::str::from_utf8_unchecked(
                                        &bytes[start_in_span..end_in_span],
                                    )
                                };
                                out.push_str(slice);
                            }
                        }

                        if span_end >= range.end {
                            break;
                        }
                        offset = span_end;
                    }
                }
            }
            Node::Internal { children, .. } => {
                let mut offset = base;

                for child in children {
                    let child_bytes = match child {
                        Node::Leaf { sums, .. } => sums.bytes,
                        Node::Internal { sums, .. } => sums.bytes,
                    };
                    let child_end = offset + child_bytes;

                    if child_end > range.start && offset < range.end {
                        self.collect_text_range(child, range.clone(), offset, out);
                    }

                    if child_end >= range.end {
                        break;
                    }
                    offset = child_end;
                }
            }
        }
    }

    /// Find the start of the current line for a given position - O(log n)
    pub fn find_line_start_at(&self, pos: usize) -> usize {
        self.find_prev_newline(pos).map(|p| p + 1).unwrap_or(0)
    }

    /// Find the end of the current line for a given position - O(log n)
    pub fn find_line_end_at(&self, pos: usize) -> usize {
        self.find_next_newline(pos)
            .unwrap_or_else(|| self.byte_count())
    }

    /// Get the current line text at a given position - O(log n)
    pub fn get_line_at(&self, pos: usize) -> String {
        let start = self.find_line_start_at(pos);
        let end = self.find_line_end_at(pos);
        self.get_text_slice(start..end)
    }

    /// Count characters (not bytes) - O(n) but could be cached
    pub fn char_count(&self) -> usize {
        self.char_count_in_node(&self.root)
    }

    fn char_count_in_node(&self, node: &Node) -> usize {
        match node {
            Node::Leaf { spans, .. } => {
                let mut count = 0;
                for span in spans {
                    if let Span::Text { bytes, .. } = span {
                        // SAFETY: We maintain UTF-8 invariant
                        let s = unsafe { std::str::from_utf8_unchecked(bytes) };
                        count += s.chars().count();
                    }
                }
                count
            }
            Node::Internal { children, .. } => children
                .iter()
                .map(|child| self.char_count_in_node(child))
                .sum(),
        }
    }

    /// Convert DocPos to byte offset in document - O(log n)
    pub fn doc_pos_to_byte(&self, pos: DocPos) -> usize {
        if let Some(line_start) = self.line_to_byte(pos.line) {
            // Get the text of this line to calculate column offset
            let line_end = self.line_to_byte(pos.line + 1).unwrap_or(self.byte_count());
            let line_text = self.get_text_slice(line_start..line_end);

            // Calculate byte offset within line accounting for tabs
            let mut byte_offset = 0;
            let mut visual_column = 0;

            for ch in line_text.chars() {
                if visual_column >= pos.column {
                    break;
                }
                if ch == '\t' {
                    visual_column = ((visual_column / 4) + 1) * 4; // 4-space tabs
                } else {
                    visual_column += 1;
                }
                byte_offset += ch.len_utf8();
            }

            line_start + byte_offset
        } else {
            pos.byte_offset // Fallback to stored byte offset
        }
    }

}

#[cfg(test)]
mod tests {
    use crate::tree::{Content, Doc, Edit};

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

        assert_eq!(tree.line_to_byte(0), Some(0)); // Start of line 1
        assert_eq!(tree.line_to_byte(1), Some(7)); // Start of line 2
        assert_eq!(tree.line_to_byte(2), Some(14)); // Start of line 3
        assert_eq!(tree.line_to_byte(3), Some(21)); // After last newline
        assert_eq!(tree.line_to_byte(4), None); // Out of bounds
    }

    #[test]
    fn test_byte_to_line() {
        let doc = Doc::from_str("Line 1\nLine 2\nLine 3\n");
        let tree = doc.read();

        assert_eq!(tree.byte_to_line(0), 0); // Start of file
        assert_eq!(tree.byte_to_line(5), 0); // Middle of line 1
        assert_eq!(tree.byte_to_line(7), 1); // Start of line 2
        assert_eq!(tree.byte_to_line(10), 1); // Middle of line 2
        assert_eq!(tree.byte_to_line(14), 2); // Start of line 3
        assert_eq!(tree.byte_to_line(20), 2); // End of line 3
    }

    #[test]
    fn test_find_newlines() {
        let doc = Doc::from_str("Hello\nWorld\n!");
        let tree = doc.read();

        // Find next newline
        assert_eq!(tree.find_next_newline(0), Some(5)); // First \n
        assert_eq!(tree.find_next_newline(6), Some(11)); // Second \n
        assert_eq!(tree.find_next_newline(12), None); // No more

        // Find previous newline
        assert_eq!(tree.find_prev_newline(0), None); // Start of file
        assert_eq!(tree.find_prev_newline(6), Some(5)); // After first \n
        assert_eq!(tree.find_prev_newline(12), Some(11)); // After second \n
    }

    #[test]
    fn test_get_text_slice() {
        let doc = Doc::from_str("Hello, World!");
        let tree = doc.read();

        assert_eq!(tree.get_text_slice(0..5), "Hello");
        assert_eq!(tree.get_text_slice(7..12), "World");
        assert_eq!(tree.get_text_slice(0..13), "Hello, World!");
        assert_eq!(tree.get_text_slice(5..5), ""); // Empty range
    }

    #[test]
    fn test_line_navigation() {
        let doc = Doc::from_str("First line\nSecond line\nThird line");
        let tree = doc.read();

        // Find line starts
        assert_eq!(tree.find_line_start_at(5), 0); // In first line
        assert_eq!(tree.find_line_start_at(15), 11); // In second line

        // Find line ends
        assert_eq!(tree.find_line_end_at(5), 10); // End of first line
        assert_eq!(tree.find_line_end_at(15), 22); // End of second line

        // Get line text
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
        assert_eq!(tree.byte_to_line(8), 1); // In second line
    }

    #[test]
    fn test_large_document() {
        // Test with a larger document to ensure tree structure works
        let mut text = String::new();
        for i in 0..100 {
            text.push_str(&format!("Line {}\n", i));
        }

        let doc = Doc::from_str(&text);
        let tree = doc.read();

        assert_eq!(tree.line_count(), 100);

        // Test middle access
        let line_50_start = tree.line_to_byte(50).unwrap();
        assert!(tree.get_line_at(line_50_start).starts_with("Line 50"));
    }

    #[test]
    fn test_unicode() {
        let doc = Doc::from_str("你好\n世界\n");
        let tree = doc.read();

        assert_eq!(tree.byte_count(), 14); // 6 + 1 + 6 + 1 bytes
        assert_eq!(tree.line_count(), 2);
        assert_eq!(tree.char_count(), 6); // 2 + 1 + 2 + 1 chars

        assert_eq!(tree.get_line_at(0), "你好");
        assert_eq!(tree.get_line_at(8), "世界");
    }
}

impl Tree {
    /// Walk nodes that intersect with a byte range for efficient visible-range rendering
    /// This enables O(log n) navigation to find only the content that should be rendered
    pub fn walk_visible_range<F>(&self, byte_range: std::ops::Range<usize>, mut callback: F)
    where
        F: FnMut(&[crate::tree::Span], usize, usize),  // (spans, byte_start, byte_end)
    {
        self.walk_range_in_node(&self.root, byte_range, 0, &mut callback);
    }

    /// Recursively walk nodes that intersect with the target byte range
    fn walk_range_in_node<F>(&self, node: &Node, range: std::ops::Range<usize>, node_start: usize, callback: &mut F)
    where
        F: FnMut(&[crate::tree::Span], usize, usize),
    {
        let node_bytes = match node {
            Node::Leaf { sums, .. } => sums.bytes,
            Node::Internal { sums, .. } => sums.bytes,
        };

        let node_end = node_start + node_bytes;

        // Skip if this node doesn't intersect with our target range
        if node_end <= range.start || node_start >= range.end {
            return;
        }

        match node {
            Node::Leaf { spans, .. } => {
                // This leaf intersects - call callback with the spans and range info
                let intersect_start = node_start.max(range.start);
                let intersect_end = node_end.min(range.end);
                callback(spans, intersect_start, intersect_end);
            }
            Node::Internal { children, .. } => {
                // Recurse into children that might intersect
                let mut child_start = node_start;
                for child in children {
                    let child_bytes = match child {
                        Node::Leaf { sums, .. } => sums.bytes,
                        Node::Internal { sums, .. } => sums.bytes,
                    };

                    let child_end = child_start + child_bytes;

                    // Only recurse if child intersects with range
                    if child_end > range.start && child_start < range.end {
                        self.walk_range_in_node(child, range.clone(), child_start, callback);
                    }

                    child_start = child_end;
                }
            }
        }
    }
}
