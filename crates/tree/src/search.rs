//! Search and replace functionality for the document tree

use super::*;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use ahash::{AHashMap, AHasher};
use bytecount::count as bytecount_count;
use memchr::memchr_iter;
use regex::Regex;
use simdutf8::basic::from_utf8;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
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
    /// Create a new search match
    #[inline]
    fn new(byte_range: Range<usize>, line: u32, column: u32) -> Self {
        Self { byte_range, line, column }
    }

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

// === Helper Functions ===

/// Calculate column from UTF-8 byte slice
#[inline]
fn calculate_column(bytes: &[u8], line_start: usize, match_start: usize) -> u32 {
    if let Ok(text) = from_utf8(&bytes[line_start..match_start]) {
        text.chars().count() as u32
    } else {
        (match_start - line_start) as u32
    }
}

/// Calculate column using UTF-8 continuation byte check
#[inline]
fn calculate_column_fast(bytes: &[u8], line_start: usize, match_start: usize) -> u32 {
    bytes[line_start..match_start]
        .iter()
        .filter(|&&b| (b & 0xC0) != 0x80)
        .count() as u32
}

/// Incremental line and column tracker for search operations
struct IncrementalLineTracker {
    current_line: u32,
    line_start_byte: usize,
    last_checked_byte: usize,
}

impl IncrementalLineTracker {
    fn new(base_line: u32, base_byte: usize) -> Self {
        Self {
            current_line: base_line,
            line_start_byte: base_byte,
            last_checked_byte: base_byte,
        }
    }

    /// Advance to a new match position and return (line, column)
    fn advance_to(&mut self, bytes: &[u8], match_start: usize) -> (u32, u32) {
        let slice = &bytes[self.last_checked_byte..match_start];
        let newline_count = bytecount_count(slice, b'\n') as u32;

        if newline_count > 0 {
            // We crossed newline(s) - update line and reset column tracking
            self.current_line += newline_count;
            self.line_start_byte = memchr_iter(b'\n', slice)
                .last()
                .map(|p| self.last_checked_byte + p + 1)
                .unwrap_or(self.line_start_byte);
            self.last_checked_byte = self.line_start_byte;

            // Count chars from new line_start_byte to match_start
            let remaining_slice = &bytes[self.line_start_byte..match_start];
            let column = calculate_column_fast(remaining_slice, 0, remaining_slice.len());
            (self.current_line, column)
        } else {
            // No newlines - count UTF-8 chars incrementally
            let column = calculate_column_fast(slice, 0, slice.len());
            (self.current_line, column)
        }
    }
}

// === Search Cache ===

// Thread-local cache for compiled searchers (each thread has its own to avoid locking)
thread_local! {
    static SEARCHER_CACHE: RefCell<SearcherCache> = RefCell::new(SearcherCache::new());
}

/// Cache for compiled search patterns to avoid recompilation
struct SearcherCache {
    // Use hash as key for O(1) lookup without allocation
    // Store (pattern, engine) to verify no hash collisions
    cache: AHashMap<u64, (String, bool, bool, bool, Arc<SearchEngine>)>,
    max_size: usize,
}

impl SearcherCache {
    fn new() -> Self {
        Self {
            cache: AHashMap::with_capacity(64),
            max_size: 128, // Larger cache since lookup is now O(1)
        }
    }

    #[inline]
    fn compute_hash(pattern: &str, case_sensitive: bool, whole_word: bool, regex: bool) -> u64 {
        let mut hasher = AHasher::default();
        pattern.hash(&mut hasher);
        case_sensitive.hash(&mut hasher);
        whole_word.hash(&mut hasher);
        regex.hash(&mut hasher);
        hasher.finish()
    }

    fn get_or_create(&mut self, pattern: &str, options: &SearchOptions) -> Arc<SearchEngine> {
        // Compute hash once (no allocation)
        let hash = Self::compute_hash(pattern, options.case_sensitive, options.whole_word, options.regex);

        // O(1) lookup with hash
        if let Some((cached_pattern, cs, ww, rx, engine)) = self.cache.get(&hash) {
            // Verify no collision (extremely rare)
            if cached_pattern == pattern && *cs == options.case_sensitive
                && *ww == options.whole_word && *rx == options.regex {
                return Arc::clone(engine);
            }
        }

        // Create new searcher (cache miss)
        let engine = Arc::new(if options.regex {
            SearchEngine::Regex(RegexSearcher::new(pattern, options))
        } else {
            SearchEngine::Plain(PlainSearcher::new(pattern, options))
        });

        // Add to cache
        self.cache.insert(
            hash,
            (pattern.to_string(), options.case_sensitive, options.whole_word, options.regex, Arc::clone(&engine))
        );

        // Simple eviction: if cache gets too big, clear it entirely
        if self.cache.len() > self.max_size {
            self.cache.clear();
            // Re-insert current pattern
            self.cache.insert(
                hash,
                (pattern.to_string(), options.case_sensitive, options.whole_word, options.regex, Arc::clone(&engine))
            );
        }

        engine
    }

    /// Clear the cache (useful for testing)
    #[allow(dead_code)]
    fn clear(&mut self) {
        self.cache.clear();
    }
}

// === Helper: Fast search in flat buffer ===

fn search_next_in_bytes(engine: &SearchEngine, bytes: &[u8], start_pos: usize) -> Option<SearchMatch> {
    match engine {
        SearchEngine::Plain(searcher) => {
            let mut pos = start_pos;

            // Count lines up to start_pos using SIMD
            let current_line = bytecount_count(&bytes[..start_pos.min(bytes.len())], b'\n') as u32;
            let line_start_byte = memchr_iter(b'\n', &bytes[..start_pos.min(bytes.len())])
                .last()
                .map(|p| p + 1)
                .unwrap_or(0);

            while pos < bytes.len() {
                if let Some((match_start, match_end)) = searcher.find_in_bytes(bytes, pos) {
                    if match_start > start_pos && searcher.is_word_boundary(bytes, match_start, match_end) {
                        // Count lines from line_start_byte to match_start using SIMD
                        let lines_to_match = bytecount_count(&bytes[line_start_byte..match_start], b'\n') as u32;
                        let final_line = current_line + lines_to_match;

                        let final_line_start = memchr_iter(b'\n', &bytes[..match_start])
                            .last()
                            .map(|p| p + 1)
                            .unwrap_or(0);

                        // Use simdutf8 for faster UTF-8 validation
                        let column = calculate_column(bytes, final_line_start, match_start);

                        return Some(SearchMatch::new(
                            match_start..match_end,
                            final_line,
                            column,
                        ));
                    }
                    pos = match_start + 1;
                } else {
                    break;
                }
            }
            None
        }
        SearchEngine::Regex(searcher) => {
            // Use simdutf8 for faster UTF-8 validation
            if let Ok(text) = from_utf8(bytes) {
                let search_text = &text[start_pos..];

                if let Some(m) = searcher.regex.find(search_text) {
                    let match_start = start_pos + m.start();
                    let match_end = start_pos + m.end();

                    // Count lines up to match using SIMD
                    let current_line = bytecount_count(&bytes[..match_start], b'\n') as u32;

                    // Find line start using SIMD
                    let line_start = memchr_iter(b'\n', &bytes[..match_start])
                        .last()
                        .map(|p| p + 1)
                        .unwrap_or(0);

                    let column = calculate_column(text.as_bytes(), line_start, match_start);

                    return Some(SearchMatch::new(
                        match_start..match_end,
                        current_line,
                        column,
                    ));
                }
            }
            None
        }
    }
}

fn search_prev_in_bytes(engine: &SearchEngine, bytes: &[u8], end_pos: usize) -> Option<SearchMatch> {
    let search_bytes = &bytes[..end_pos.min(bytes.len())];

    match engine {
        SearchEngine::Plain(searcher) => {
            let mut pos = 0;
            let mut last_match: Option<SearchMatch> = None;
            let mut current_line = 0u32;
            let mut line_start_byte = 0;

            while pos < search_bytes.len() {
                if let Some((match_start, match_end)) = searcher.find_in_bytes(search_bytes, pos) {
                    if match_end <= end_pos && searcher.is_word_boundary(search_bytes, match_start, match_end) {
                        // Incrementally count lines from last position using SIMD
                        let lines_between = bytecount_count(&search_bytes[line_start_byte..match_start], b'\n') as u32;
                        current_line += lines_between;

                        // Find line start for this match using memchr (SIMD)
                        if lines_between > 0 {
                            line_start_byte = memchr_iter(b'\n', &search_bytes[line_start_byte..match_start])
                                .last()
                                .map(|p| line_start_byte + p + 1)
                                .unwrap_or(line_start_byte);
                        }

                        // Use simdutf8 for faster UTF-8 validation
                        let column = calculate_column(search_bytes, line_start_byte, match_start);

                        last_match = Some(SearchMatch::new(
                            match_start..match_end,
                            current_line,
                            column,
                        ));
                    }
                    pos = match_start + 1;
                } else {
                    break;
                }
            }
            last_match
        }
        SearchEngine::Regex(searcher) => {
            // Use simdutf8 for faster UTF-8 validation
            if let Ok(text) = from_utf8(search_bytes) {
                let mut last_match: Option<SearchMatch> = None;
                let mut current_line = 0u32;
                let mut line_start_byte = 0;

                for m in searcher.regex.find_iter(text) {
                    if m.end() > end_pos {
                        break;
                    }

                    let match_start = m.start();
                    let match_end = m.end();

                    // Incrementally count lines from last position using SIMD
                    let lines_between = bytecount_count(&search_bytes[line_start_byte..match_start], b'\n') as u32;
                    current_line += lines_between;

                    // Find line start for this match using memchr (SIMD)
                    if lines_between > 0 {
                        line_start_byte = memchr_iter(b'\n', &search_bytes[line_start_byte..match_start])
                            .last()
                            .map(|p| line_start_byte + p + 1)
                            .unwrap_or(line_start_byte);
                    }

                    let column = calculate_column(text.as_bytes(), line_start_byte, match_start);

                    last_match = Some(SearchMatch::new(
                        match_start..match_end,
                        current_line,
                        column,
                    ));
                }

                return last_match;
            }
            None
        }
    }
}

/// Fast search that only returns byte ranges (no line/column calculation)
fn search_byte_ranges_only(engine: &SearchEngine, bytes: &[u8], limit: Option<usize>) -> Vec<SearchMatch> {
    let mut matches = Vec::new();

    match engine {
        SearchEngine::Plain(searcher) => {
            let mut pos = 0;

            while pos < bytes.len() {
                if let Some(lim) = limit {
                    if matches.len() >= lim {
                        break;
                    }
                }

                if let Some((match_start, match_end)) = searcher.find_in_bytes(bytes, pos) {
                    if searcher.is_word_boundary(bytes, match_start, match_end) {
                        // No line/column calculation - just byte ranges
                        matches.push(SearchMatch::new(match_start..match_end, 0, 0));
                    }
                    pos = match_start + 1;
                } else {
                    break;
                }
            }
        }
        SearchEngine::Regex(searcher) => {
            if let Ok(text) = from_utf8(bytes) {
                for m in searcher.regex.find_iter(text) {
                    if let Some(lim) = limit {
                        if matches.len() >= lim {
                            break;
                        }
                    }

                    matches.push(SearchMatch::new(m.start()..m.end(), 0, 0));
                }
            }
        }
    }

    matches
}

fn search_in_bytes(engine: &SearchEngine, bytes: &[u8], limit: Option<usize>) -> Vec<SearchMatch> {
    let mut matches = Vec::new();

    match engine {
        SearchEngine::Plain(searcher) => {
            let mut pos = 0;
            let mut tracker = IncrementalLineTracker::new(0, 0);

            while pos < bytes.len() {
                if let Some(lim) = limit {
                    if matches.len() >= lim {
                        break;
                    }
                }

                if let Some((match_start, match_end)) = searcher.find_in_bytes(bytes, pos) {
                    if searcher.is_word_boundary(bytes, match_start, match_end) {
                        let (line, column) = tracker.advance_to(bytes, match_start);
                        matches.push(SearchMatch::new(match_start..match_end, line, column));

                        // Update last_checked to match_start for next iteration
                        tracker.last_checked_byte = match_start;
                    }
                    pos = match_start + 1;
                } else {
                    break;
                }
            }
        }
        SearchEngine::Regex(searcher) => {
            // Use simdutf8 for faster UTF-8 validation
            if let Ok(text) = from_utf8(bytes) {
                let mut tracker = IncrementalLineTracker::new(0, 0);

                for m in searcher.regex.find_iter(text) {
                    if let Some(lim) = limit {
                        if matches.len() >= lim {
                            break;
                        }
                    }

                    let match_start = m.start();
                    let match_end = m.end();

                    let (line, column) = tracker.advance_to(bytes, match_start);
                    matches.push(SearchMatch::new(match_start..match_end, line, column));

                    // Update last_checked to match_start for next iteration
                    tracker.last_checked_byte = match_start;
                }
            }
        }
    }

    matches
}

// === Tree Search Implementation ===

impl Tree {
    /// Find all occurrences of a pattern in the document
    pub fn search(&self, pattern: &str, options: SearchOptions) -> Vec<SearchMatch> {
        if pattern.is_empty() {
            return Vec::new();
        }

        // Use flattened text for search (much faster than tree traversal!)
        let text = self.flatten_to_string();
        let bytes = text.as_bytes();

        // Get cached searcher or create new one
        let searcher = SEARCHER_CACHE.with(|cache| {
            cache.borrow_mut().get_or_create(pattern, &options)
        });

        // Search in flat buffer (like ripgrep does)
        search_in_bytes(&searcher, bytes, options.limit)
    }

    /// Find next occurrence after given position
    pub fn search_next(&self, pattern: &str, start_pos: usize, options: SearchOptions) -> Option<SearchMatch> {
        if pattern.is_empty() {
            return None;
        }

        // Use flattened text for fast search
        let text = self.flatten_to_string();
        let bytes = text.as_bytes();

        if start_pos >= bytes.len() {
            return None;
        }

        // Get cached searcher
        let searcher = SEARCHER_CACHE.with(|cache| {
            cache.borrow_mut().get_or_create(pattern, &options)
        });

        // Search from start_pos to end
        search_next_in_bytes(&searcher, bytes, start_pos)
    }

    /// Find previous occurrence before given position
    pub fn search_prev(&self, pattern: &str, end_pos: usize, options: SearchOptions) -> Option<SearchMatch> {
        if pattern.is_empty() {
            return None;
        }

        // Use flattened text for fast search
        let text = self.flatten_to_string();
        let bytes = text.as_bytes();

        // Get cached searcher
        let searcher = SEARCHER_CACHE.with(|cache| {
            cache.borrow_mut().get_or_create(pattern, &options)
        });

        // Search from beginning to end_pos
        search_prev_in_bytes(&searcher, bytes, end_pos)
    }

    /// Replace all occurrences - returns new tree
    pub fn replace_all(&self, pattern: &str, replacement: &str, options: SearchOptions) -> Self {
        // For large numbers of replacements, flatten → replace → rebuild is faster
        // than applying individual tree edits
        const BATCH_THRESHOLD: usize = 100;

        // Get flattened text once (cached if available)
        let text = self.flatten_to_string();
        let bytes = text.as_bytes();

        // Get cached searcher
        let searcher = SEARCHER_CACHE.with(|cache| {
            cache.borrow_mut().get_or_create(pattern, &options)
        });

        // Use fast search that skips line/column calculation (replace_all doesn't need it)
        let matches = search_byte_ranges_only(&searcher, bytes, options.limit);

        if matches.is_empty() {
            return self.clone();
        }

        if matches.len() >= BATCH_THRESHOLD {
            // Fast path: use the already-flattened text (no second flatten!)

            // Better capacity estimation to avoid reallocation
            let pattern_len = match &*searcher {
                SearchEngine::Plain(p) => p.pattern.len(),
                SearchEngine::Regex(_) => {
                    // For regex, estimate from first match if available
                    matches.first().map(|m| m.byte_range.len()).unwrap_or(pattern.len())
                }
            };
            let size_diff = replacement.len().saturating_sub(pattern_len);
            let estimated_capacity = text.len() + (matches.len() * size_diff);
            let mut result = String::with_capacity(estimated_capacity);
            let mut last_end = 0;

            // Filter out overlapping matches (e.g., "aa" in "aaa" finds matches at 0-2 and 1-3)
            // We only replace non-overlapping matches
            for m in &matches {
                if m.byte_range.start >= last_end {
                    result.push_str(&text[last_end..m.byte_range.start]);
                    result.push_str(replacement);
                    last_end = m.byte_range.end;
                }
                // Skip overlapping matches
            }
            result.push_str(&text[last_end..]);

            return Self::from_str(&result);
        }

        // Slow path: individual tree edits for small numbers of replacements
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
        let mut replacements = Vec::with_capacity(matches.len()); // Pre-allocate worst case
        for m in matches.iter() {
            if let Some(replacement) = replacer(m) {
                replacements.push((m.byte_range.clone(), replacement));
            }
        }

        if replacements.is_empty() {
            return self.clone();
        }

        // Fast path for many replacements: use string building instead of tree edits
        const BATCH_THRESHOLD: usize = 100;
        if replacements.len() >= BATCH_THRESHOLD {
            let text = self.flatten_to_string();

            // Estimate capacity
            let avg_match_len = replacements.iter().map(|(r, _)| r.len()).sum::<usize>() / replacements.len();
            let avg_replacement_len = replacements.iter().map(|(_, s)| s.len()).sum::<usize>() / replacements.len();
            let size_diff = avg_replacement_len.saturating_sub(avg_match_len);
            let estimated_capacity = text.len() + (replacements.len() * size_diff);

            let mut result = String::with_capacity(estimated_capacity);
            let mut last_end = 0;

            for (range, replacement) in &replacements {
                result.push_str(&text[last_end..range.start]);
                result.push_str(replacement);
                last_end = range.end;
            }
            result.push_str(&text[last_end..]);

            return Self::from_str(&result);
        }

        // Slow path: Build edits in reverse order to preserve positions
        let mut edits = Vec::with_capacity(replacements.len());
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

    /// Find previous occurrence before given position
    pub fn search_prev(&self, pattern: &str, end_pos: usize, options: SearchOptions) -> Option<SearchMatch> {
        self.flush();
        self.read().search_prev(pattern, end_pos, options)
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
    fn test_search_prev() {
        let tree = Tree::from_str("first test second test third test");

        let options = SearchOptions::default();

        // Find last occurrence from the end
        let match1 = tree.search_prev("test", tree.byte_count(), options.clone());
        assert!(match1.is_some());
        assert_eq!(match1.as_ref().unwrap().byte_range.start, 29);

        // Find previous occurrence before last
        let match2 = tree.search_prev("test", match1.unwrap().byte_range.start, options.clone());
        assert!(match2.is_some());
        assert_eq!(match2.as_ref().unwrap().byte_range.start, 18);

        // Find first occurrence
        let match3 = tree.search_prev("test", match2.unwrap().byte_range.start, options.clone());
        assert!(match3.is_some());
        assert_eq!(match3.as_ref().unwrap().byte_range.start, 6);

        // No match before first occurrence
        let match4 = tree.search_prev("test", match3.unwrap().byte_range.start, options);
        assert!(match4.is_none());
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
    fn test_word_boundary_search() {
        // Test for double-click word selection scenario
        let tree = Tree::from_str("hello world\tfoo\nbar baz");

        // Click position in middle of "world" (at 'r')
        let click_pos = 8;

        // Search backwards for whitespace (space, tab, newline)
        let prev_space = tree.search_prev(" ", click_pos, SearchOptions::default());
        let prev_tab = tree.search_prev("\t", click_pos, SearchOptions::default());
        let prev_newline = tree.search_prev("\n", click_pos, SearchOptions::default());

        // Find the closest boundary before click
        let mut word_start = 0;
        if let Some(m) = prev_space {
            word_start = word_start.max(m.byte_range.end);
        }
        if let Some(m) = prev_tab {
            word_start = word_start.max(m.byte_range.end);
        }
        if let Some(m) = prev_newline {
            word_start = word_start.max(m.byte_range.end);
        }

        // Search forwards for whitespace
        let next_space = tree.search_next(" ", click_pos, SearchOptions::default());
        let next_tab = tree.search_next("\t", click_pos, SearchOptions::default());
        let next_newline = tree.search_next("\n", click_pos, SearchOptions::default());

        // Find the closest boundary after click
        let mut word_end = tree.byte_count();
        if let Some(m) = next_space {
            word_end = word_end.min(m.byte_range.start);
        }
        if let Some(m) = next_tab {
            word_end = word_end.min(m.byte_range.start);
        }
        if let Some(m) = next_newline {
            word_end = word_end.min(m.byte_range.start);
        }

        // Extract the word
        let word = tree.get_text_slice(word_start..word_end);
        assert_eq!(word, "world");
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