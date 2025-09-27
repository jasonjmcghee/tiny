//! Search and replace functionality for the document tree

use super::*;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use regex::Regex;
use std::ops::Range;
use std::sync::Arc;

/// A match found during search
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub byte_range: Range<usize>,
    pub line: u32,
    pub column: u32, // UTF-8 character column within line
    // Note: match_text removed - extract from tree when needed to avoid allocation
}

impl SearchMatch {
    /// Get the actual text of this match (allocates on demand)
    pub fn text<'a>(&self, tree: &'a Tree) -> String {
        tree.get_text_slice(self.byte_range.clone())
    }
}

/// Options for search operations
#[derive(Clone, Debug)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub limit: Option<usize>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            whole_word: false,
            regex: false,
            limit: None,
        }
    }
}

// === Tree Search Implementation ===

impl Tree {
    /// Find all occurrences of a pattern in the document
    pub fn search(&self, pattern: &str, options: SearchOptions) -> Vec<SearchMatch> {
        if pattern.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        let searcher = if options.regex {
            SearchEngine::Regex(RegexSearcher::new(pattern, &options))
        } else {
            SearchEngine::Plain(PlainSearcher::new(pattern, &options))
        };

        let mut cursor = self.cursor();
        cursor.search_with_engine(searcher, &mut matches, options.limit);
        matches
    }

    /// Find next occurrence after given position
    pub fn search_next(&self, pattern: &str, start_pos: usize, options: SearchOptions) -> Option<SearchMatch> {
        // Search for all matches and return the first one after start_pos
        let matches = self.search(pattern, options);

        matches.into_iter().find(|m| m.byte_range.start > start_pos)
    }

    /// Replace all occurrences - returns new tree
    pub fn replace_all(&self, pattern: &str, replacement: &str, options: SearchOptions) -> Self {
        let matches = self.search(pattern, options);
        if matches.is_empty() {
            return self.clone();
        }

        // Build edits from matches (in reverse order to preserve positions)
        let mut edits = Vec::new();
        for m in matches.iter().rev() {
            edits.push(Edit::Replace {
                range: m.byte_range.clone(),
                content: Content::Text(replacement.to_string()),
            });
        }

        self.apply_edits(&edits)
    }

    /// Interactive replace with callback for each match
    pub fn replace_with<F>(&self, pattern: &str, options: SearchOptions, mut replacer: F) -> Self
    where
        F: FnMut(&SearchMatch) -> Option<String>,
    {
        let matches = self.search(pattern, options);
        if matches.is_empty() {
            return self.clone();
        }

        // Call replacer in forward order but collect replacements with matches
        let mut replacements = Vec::new();
        for m in matches.iter() {
            if let Some(replacement) = replacer(m) {
                replacements.push((m.byte_range.clone(), replacement));
            }
        }

        if replacements.is_empty() {
            return self.clone();
        }

        // Build edits in reverse order to preserve positions
        let mut edits = Vec::new();
        for (range, replacement) in replacements.iter().rev() {
            edits.push(Edit::Replace {
                range: range.clone(),
                content: Content::Text(replacement.clone()),
            });
        }

        self.apply_edits(&edits)
    }
}

// === Doc Search Implementation ===

impl Doc {
    /// Search the document for a pattern
    pub fn search(&self, pattern: &str, options: SearchOptions) -> Vec<SearchMatch> {
        self.flush(); // Ensure pending edits are applied
        self.read().search(pattern, options)
    }

    /// Find next occurrence after given position
    pub fn search_next(&self, pattern: &str, start_pos: usize, options: SearchOptions) -> Option<SearchMatch> {
        self.flush();
        self.read().search_next(pattern, start_pos, options)
    }

    /// Replace all occurrences and return new tree
    pub fn replace_all(&self, pattern: &str, replacement: &str, options: SearchOptions) -> Arc<Tree> {
        self.flush();
        let new_tree = self.read().replace_all(pattern, replacement, options);
        let new_arc = Arc::new(new_tree);
        self.replace_tree(new_arc.clone());
        new_arc
    }

    /// Interactive replace with callback for each match
    pub fn replace_with<F>(&self, pattern: &str, options: SearchOptions, replacer: F) -> Arc<Tree>
    where
        F: FnMut(&SearchMatch) -> Option<String>,
    {
        self.flush();
        let new_tree = self.read().replace_with(pattern, options, replacer);
        let new_arc = Arc::new(new_tree);
        self.replace_tree(new_arc.clone());
        new_arc
    }
}

// === Search Engines ===

pub(super) enum SearchEngine {
    Plain(PlainSearcher),
    Regex(RegexSearcher),
}

pub(super) struct PlainSearcher {
    pattern: Vec<u8>,
    whole_word: bool,
    aho_corasick: AhoCorasick,
}

impl PlainSearcher {
    fn new(pattern: &str, options: &SearchOptions) -> Self {
        let pattern_bytes = pattern.as_bytes().to_vec();

        // Build AhoCorasick with appropriate case sensitivity
        let ac = if options.case_sensitive {
            AhoCorasickBuilder::new()
                .build([pattern])
                .expect("Failed to build AhoCorasick")
        } else {
            AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .build([pattern])
                .expect("Failed to build AhoCorasick")
        };

        Self {
            pattern: pattern_bytes,
            whole_word: options.whole_word,
            aho_corasick: ac,
        }
    }

    fn find_in_bytes(&self, haystack: &[u8], start: usize) -> Option<(usize, usize)> {
        // Use aho-corasick for both case-sensitive and case-insensitive
        self.aho_corasick
            .find(&haystack[start..])
            .map(|m| (start + m.start(), start + m.end()))
    }

    fn is_word_boundary(&self, text: &[u8], pos: usize, end: usize) -> bool {
        if !self.whole_word {
            return true;
        }

        let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

        // Check before
        if pos > 0 && is_word_char(text[pos - 1]) {
            return false;
        }

        // Check after
        if end < text.len() && is_word_char(text[end]) {
            return false;
        }

        true
    }
}


pub(super) struct RegexSearcher {
    regex: Regex,
}

impl RegexSearcher {
    fn new(pattern: &str, options: &SearchOptions) -> Self {
        let mut pattern_str = pattern.to_string();

        // Apply case-insensitive flag
        if !options.case_sensitive {
            pattern_str = format!("(?i){}", pattern_str);
        }

        // Apply whole word boundaries
        if options.whole_word {
            pattern_str = format!(r"\b{}\b", pattern_str);
        }

        Self {
            regex: Regex::new(&pattern_str).unwrap_or_else(|_| {
                // Fallback to literal match if regex is invalid
                Regex::new(&regex::escape(pattern)).unwrap()
            }),
        }
    }
}

// === Cursor Search Extensions ===

impl<'a> TreeCursor<'a> {
    pub(super) fn search_with_engine(
        &mut self,
        engine: SearchEngine,
        matches: &mut Vec<SearchMatch>,
        limit: Option<usize>,
    ) {
        self.reset();

        // Buffer to handle patterns spanning boundaries
        let max_pattern_len = match &engine {
            SearchEngine::Plain(p) => p.pattern.len(),
            SearchEngine::Regex(_) => 100, // Reasonable max for regex patterns
        };

        let mut overlap_buffer = Vec::with_capacity(max_pattern_len * 2);
        let mut prev_span_tail = Vec::new();
        let mut prev_offset = 0;
        let mut prev_line = 0;

        loop {
            if let Some(limit) = limit {
                if matches.len() >= limit {
                    return;
                }
            }

            // Process current leaf's spans
            if !self.current_spans.is_empty() {
                let mut current_line_offset = self.line_pos;

                for (i, (span, offset)) in self.current_spans.iter().enumerate() {
                    if let Span::Text { bytes, lines } = span {
                        // For spans after the first, check for patterns spanning the boundary
                        if i > 0 && !prev_span_tail.is_empty() && max_pattern_len > 1 {
                            // Create overlap buffer with end of prev span and start of current
                            overlap_buffer.clear();
                            overlap_buffer.extend_from_slice(&prev_span_tail);
                            let take_from_current = max_pattern_len.min(bytes.len());
                            overlap_buffer.extend_from_slice(&bytes[..take_from_current]);

                            // Search in the overlap buffer but offset matches correctly
                            let overlap_start = prev_offset + prev_span_tail.len().saturating_sub(max_pattern_len);
                            self.search_in_span(
                                &overlap_buffer,
                                overlap_start,
                                prev_line,
                                &engine,
                                matches,
                                limit,
                            );
                        }

                        // Normal search in the span
                        self.search_in_span(
                            bytes,
                            *offset,
                            current_line_offset,
                            &engine,
                            matches,
                            limit,
                        );

                        // Save tail of this span for next boundary check
                        prev_span_tail.clear();
                        if bytes.len() > max_pattern_len {
                            prev_span_tail.extend_from_slice(&bytes[bytes.len() - max_pattern_len..]);
                        } else {
                            prev_span_tail.extend_from_slice(bytes);
                        }
                        prev_offset = *offset;
                        prev_line = current_line_offset;

                        // Update line offset for next span
                        current_line_offset += lines;

                        if let Some(limit) = limit {
                            if matches.len() >= limit {
                                return;
                            }
                        }
                    }
                }
            }

            if !self.advance_leaf() {
                break;
            }
        }
    }

    fn search_in_span(
        &self,
        bytes: &[u8],
        byte_offset: usize,
        base_line: u32,
        engine: &SearchEngine,
        matches: &mut Vec<SearchMatch>,
        limit: Option<usize>,
    ) {
        match engine {
            SearchEngine::Plain(searcher) => {
                let mut pos = 0;
                let mut current_line = base_line;
                let mut line_start_pos = 0;

                while pos < bytes.len() {
                    if let Some(limit) = limit {
                        if matches.len() >= limit {
                            return;
                        }
                    }

                    if let Some((match_start, match_end)) = searcher.find_in_bytes(bytes, pos) {
                        if searcher.is_word_boundary(bytes, match_start, match_end) {
                            // Update line count up to match position incrementally
                            for i in line_start_pos..match_start {
                                if bytes[i] == b'\n' {
                                    current_line += 1;
                                    line_start_pos = i + 1;
                                }
                            }

                            // Calculate column (only for the small slice from line start to match)
                            let column = if let Ok(text) = std::str::from_utf8(&bytes[line_start_pos..match_start]) {
                                text.chars().count() as u32
                            } else {
                                (match_start - line_start_pos) as u32
                            };

                            let new_match = SearchMatch {
                                byte_range: (byte_offset + match_start)..(byte_offset + match_end),
                                line: current_line,
                                column,
                            };

                            // Deduplicate - only add if not already present
                            if matches.last() != Some(&new_match) {
                                matches.push(new_match);
                            }
                        }
                        // Continue searching right after the match start to find overlapping matches
                        // This is important for patterns like "aa" in "aaa" which has 2 matches
                        pos = match_start + 1;
                    } else {
                        break;
                    }
                }
            }
            SearchEngine::Regex(searcher) => {
                if let Ok(text) = std::str::from_utf8(bytes) {
                    let mut current_line = base_line;
                    let mut byte_pos = 0;

                    for m in searcher.regex.find_iter(text) {
                        if let Some(limit) = limit {
                            if matches.len() >= limit {
                                return;
                            }
                        }

                        let match_start_bytes = m.start();
                        let match_end_bytes = m.end();

                        // Count newlines up to match
                        while byte_pos < match_start_bytes {
                            if bytes[byte_pos] == b'\n' {
                                current_line += 1;
                            }
                            byte_pos += 1;
                        }

                        // Find line start for column calculation
                        let line_start = text[..match_start_bytes]
                            .rfind('\n')
                            .map(|p| p + 1)
                            .unwrap_or(0);

                        let column = text[line_start..match_start_bytes].chars().count() as u32;

                        let new_match = SearchMatch {
                            byte_range: (byte_offset + match_start_bytes)..(byte_offset + match_end_bytes),
                            line: current_line,
                            column,
                        };

                        // Deduplicate - only add if not already present
                        if matches.last() != Some(&new_match) {
                            matches.push(new_match);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_search() {
        let tree = Tree::from_str("Hello world\nThis is a test\nHello again");

        let matches = tree.search("Hello", SearchOptions::default());
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].column, 0);
        assert_eq!(matches[1].line, 2);
        assert_eq!(matches[1].column, 0);
    }

    #[test]
    fn test_case_insensitive_search() {
        let tree = Tree::from_str("Hello HELLO hello");

        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };

        let matches = tree.search("hello", options);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_whole_word_search() {
        let tree = Tree::from_str("hello helloworld world_hello hello");

        let options = SearchOptions {
            whole_word: true,
            ..Default::default()
        };

        let matches = tree.search("hello", options);
        assert_eq!(matches.len(), 2); // Only standalone "hello" matches
    }

    #[test]
    fn test_regex_search() {
        let tree = Tree::from_str("foo123 bar456 baz789");

        let options = SearchOptions {
            regex: true,
            ..Default::default()
        };

        let matches = tree.search(r"\w+\d+", options);
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].text(&tree), "foo123");
        assert_eq!(matches[1].text(&tree), "bar456");
        assert_eq!(matches[2].text(&tree), "baz789");
    }

    #[test]
    fn test_search_with_limit() {
        let tree = Tree::from_str("test test test test test");

        let options = SearchOptions {
            limit: Some(3),
            ..Default::default()
        };

        let matches = tree.search("test", options);
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_replace_all() {
        let tree = Tree::from_str("Hello world, Hello universe");

        let new_tree = tree.replace_all("Hello", "Hi", SearchOptions::default());
        let text = new_tree.flatten_to_string();
        assert_eq!(&*text, "Hi world, Hi universe");
    }

    #[test]
    fn test_replace_with_callback() {
        let tree = Tree::from_str("foo bar baz");

        let mut counter = 0;
        let new_tree = tree.replace_with("ba", SearchOptions::default(), |_match| {
            counter += 1;
            if counter == 1 {
                Some("BA".to_string()) // Replace first match
            } else {
                None // Skip second match
            }
        });

        let text = new_tree.flatten_to_string();
        // "ba" appears in both "bar" and "baz", we replace only the first one
        assert_eq!(&*text, "foo BAr baz");
    }

    #[test]
    fn test_search_next() {
        let tree = Tree::from_str("first test second test third test");

        let options = SearchOptions::default();

        // Find first occurrence from the beginning
        let match1 = tree.search_next("test", 0, options.clone());
        assert!(match1.is_some());
        assert_eq!(match1.as_ref().unwrap().byte_range.start, 6);

        // Find next occurrence after first (start from end of first match - 1)
        let match2 = tree.search_next("test", match1.unwrap().byte_range.end - 1, options.clone());
        assert!(match2.is_some());
        assert_eq!(match2.as_ref().unwrap().byte_range.start, 18);

        // Find last occurrence
        let match3 = tree.search_next("test", match2.unwrap().byte_range.end - 1, options);
        assert!(match3.is_some());
        assert_eq!(match3.as_ref().unwrap().byte_range.start, 29);
    }

    #[test]
    fn test_search_multiline() {
        let tree = Tree::from_str("line1\nline2\nline3\nline4");

        let matches = tree.search("line", SearchOptions::default());
        assert_eq!(matches.len(), 4);

        for (i, m) in matches.iter().enumerate() {
            assert_eq!(m.line, i as u32);
            assert_eq!(m.column, 0);
            assert_eq!(m.text(&tree), "line");
        }
    }

    #[test]
    fn test_search_across_spans() {
        // This test ensures search works correctly when text is split across multiple spans
        // which happens when the tree has multiple leaves
        let mut long_text = String::new();
        for i in 0..100 {
            long_text.push_str(&format!("Line {} with some text to search\n", i));
        }

        let tree = Tree::from_str(&long_text);

        // Search for a pattern that appears in every line
        let matches = tree.search("search", SearchOptions::default());
        assert_eq!(matches.len(), 100);

        // Verify each match is on the correct line
        for (i, m) in matches.iter().enumerate() {
            assert_eq!(m.line, i as u32);
            assert_eq!(m.text(&tree), "search");
        }
    }

    #[test]
    fn test_empty_search() {
        let tree = Tree::from_str("test content");

        // Empty pattern should return no matches
        let matches = tree.search("", SearchOptions::default());
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_search_with_special_chars() {
        let tree = Tree::from_str("foo$bar test$case end$");

        // Search for literal $ character
        let matches = tree.search("$", SearchOptions::default());
        assert_eq!(matches.len(), 3);

        // Search for pattern with $
        let matches = tree.search("test$case", SearchOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].text(&tree), "test$case");
    }

    #[test]
    fn test_column_calculation() {
        let tree = Tree::from_str("abc def ghi\n123 456 789");

        // Search for "def" - should be at column 4 (after "abc ")
        let matches = tree.search("def", SearchOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 0);
        assert_eq!(matches[0].column, 4);

        // Search for "456" - should be at column 4 on line 1
        let matches = tree.search("456", SearchOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert_eq!(matches[0].column, 4);
    }

    #[test]
    fn test_utf8_search() {
        let tree = Tree::from_str("Hello 世界 test 你好 world");

        // Search for UTF-8 text
        let matches = tree.search("世界", SearchOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].column, 6); // "Hello " is 6 chars

        let matches = tree.search("你好", SearchOptions::default());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].column, 14); // "Hello 世界 test " is 14 chars
    }

    #[test]
    fn test_case_insensitive_performance() {
        // Test that case-insensitive search works efficiently
        let text = "Test TEST test TeSt".repeat(100);
        let tree = Tree::from_str(&text);

        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };

        let matches = tree.search("test", options);
        assert_eq!(matches.len(), 400); // 4 matches per repetition * 100
    }

    #[test]
    fn test_case_insensitive_basic() {
        // Simple test for case-insensitive search
        let tree = Tree::from_str("Test TEST test TeSt");

        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };

        let matches = tree.search("test", options);
        assert_eq!(matches.len(), 4);

        // Check all variations are found
        assert_eq!(matches[0].text(&tree), "Test");
        assert_eq!(matches[1].text(&tree), "TEST");
        assert_eq!(matches[2].text(&tree), "test");
        assert_eq!(matches[3].text(&tree), "TeSt");
    }

    #[test]
    fn test_span_boundary_bug() {
        // Create text that will definitely span multiple spans
        // Each span in our tree is typically 1024 bytes
        // Exactly replicate the failing test
        let text = "Test TEST test TeSt".repeat(100);
        let tree = Tree::from_str(&text);

        let options = SearchOptions {
            case_sensitive: false,
            ..Default::default()
        };

        let matches = tree.search("test", options);

        // Should find 4 matches per repeat
        let expected = 400;

        if matches.len() != expected {
            eprintln!("Expected {} matches but found {}", expected, matches.len());
            eprintln!("Missing match likely at byte positions around 1024 boundary");

            // Find which match is missing
            let mut expected_positions = Vec::new();
            for i in 0..100 {
                let base = i * 19; // "Test TEST test TeSt" is 19 bytes
                expected_positions.push(base);      // Test
                expected_positions.push(base + 5);  // TEST
                expected_positions.push(base + 10); // test
                expected_positions.push(base + 15); // TeSt
            }

            let actual_positions: Vec<_> = matches.iter().map(|m| m.byte_range.start).collect();

            for &exp_pos in &expected_positions {
                if !actual_positions.contains(&exp_pos) {
                    eprintln!("Missing match at byte position {}", exp_pos);
                }
            }
        }

        assert_eq!(matches.len(), expected);
    }
}