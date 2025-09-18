//! Language-agnostic tree-sitter syntax highlighting
//!
//! Supports any language with a tree-sitter grammar and highlight query

use crate::text_effects::{priority, EffectType, TextEffect, TextStyleProvider};
use arc_swap::ArcSwap;
use std::sync::{mpsc, Arc};
use std::thread;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree as TSTree,
};

/// Language configuration for syntax highlighting
pub struct LanguageConfig {
    pub language: Language,
    pub highlights_query: &'static str,
    pub name: &'static str,
}

/// Supported languages
pub struct Languages;

impl Languages {
    /// Rust language configuration
    pub fn rust() -> LanguageConfig {
        LanguageConfig {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
            name: "rust",
        }
    }

    // Future languages can be added here:
    // pub fn javascript() -> LanguageConfig { ... }
    // pub fn python() -> LanguageConfig { ... }
}

/// Token types (universal across languages)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TokenType {
    Keyword,
    Function,
    Type,
    String,
    Number,
    Comment,
    Constant,
    Operator,
    Punctuation,
    Variable,
    Attribute,
    Namespace,
    Property,
    Parameter,
}

/// Background syntax highlighter with debouncing
#[derive(Clone)]
pub struct SyntaxHighlighter {
    /// Current highlights (lock-free read!)
    highlights: Arc<ArcSwap<Vec<TextEffect>>>,
    /// Send parse requests to background thread
    tx: mpsc::Sender<ParseRequest>,
    /// Provider name
    name: &'static str,
    /// Cached tree for viewport queries
    cached_tree: Arc<ArcSwap<Option<TSTree>>>,
    /// Language for creating queries
    language: Language,
    /// Highlight query
    query: Arc<Query>,
}

/// Text edit information for tree-sitter incremental parsing
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_position: Point,
    pub old_end_position: Point,
    pub new_end_position: Point,
}

/// Parse request for background thread
struct ParseRequest {
    text: String,
    version: u64,
    /// Edit information for incremental parsing
    edit: Option<TextEdit>,
}

/// Viewport query request
pub struct ViewportQuery {
    pub byte_range: std::ops::Range<usize>,
}

impl SyntaxHighlighter {
    /// Apply an incremental edit for efficient reparsing
    pub fn apply_edit(&self, edit: TextEdit) {
        // Send edit to background thread
        let _ = self.tx.send(ParseRequest {
            text: String::new(), // Will be set by request_update_with_edit
            version: 0,          // Will be set by request_update_with_edit
            edit: Some(edit),
        });
    }

    /// Request update with edit information
    pub fn request_update_with_edit(&self, text: &str, version: u64, edit: Option<TextEdit>) {
        if let Some(ref edit_info) = edit {
            println!(
                "SYNTAX: Sending request_update_with_edit WITH InputEdit: start_byte={}",
                edit_info.start_byte
            );
        } else {
            println!("SYNTAX: Sending request_update_with_edit WITHOUT InputEdit");
        }
        let _ = self.tx.send(ParseRequest {
            text: text.to_string(),
            version,
            edit,
        });
    }

    /// Create highlighter for any language with background parsing
    pub fn new(config: LanguageConfig) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&config.language)?;

        let query = Arc::new(
            Query::new(&config.language, config.highlights_query)
                .expect("Failed to create highlight query"),
        );
        let query_clone = query.clone();

        let highlights = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let highlights_clone = highlights.clone();
        let cached_tree = Arc::new(ArcSwap::from_pointee(None));
        let cached_tree_clone = cached_tree.clone();

        let (tx, rx) = mpsc::channel::<ParseRequest>();

        // Background parsing thread with debouncing
        thread::spawn(move || {
            println!("SYNTAX: Background thread started");
            let mut tree: Option<TSTree> = None;
            let mut cursor = QueryCursor::new();
            let mut last_text = String::new();
            let mut is_first_parse = true;

            while let Ok(request) = rx.recv() {
                println!(
                    "SYNTAX: Received parse request for {} bytes",
                    request.text.len()
                );
                // Shorter debounce for initial parse, longer for subsequent
                let debounce_ms = if is_first_parse { 10 } else { 100 };
                std::thread::sleep(std::time::Duration::from_millis(debounce_ms));

                // Drain any additional requests that came in during debounce
                let final_request = rx.try_iter().last().unwrap_or(request);

                // Skip if text hasn't changed (avoid redundant parsing)
                if final_request.text == last_text && final_request.edit.is_none() {
                    println!("SYNTAX: Skipping - text unchanged and no edit");
                    continue;
                }
                last_text = final_request.text.clone();

                // Apply edit to existing tree for incremental parsing
                if let Some(edit) = &final_request.edit {
                    if let Some(ref mut existing_tree) = tree {
                        println!("SYNTAX: Applying incremental edit: start_byte={}, old_end={}, new_end={}",
                                 edit.start_byte, edit.old_end_byte, edit.new_end_byte);

                        let ts_edit = InputEdit {
                            start_byte: edit.start_byte,
                            old_end_byte: edit.old_end_byte,
                            new_end_byte: edit.new_end_byte,
                            start_position: edit.start_position,
                            old_end_position: edit.old_end_position,
                            new_end_position: edit.new_end_position,
                        };

                        existing_tree.edit(&ts_edit);
                    }
                }

                println!(
                    "SYNTAX: Parsing {} bytes with tree-sitter (incremental={})",
                    final_request.text.len(),
                    final_request.edit.is_some()
                );
                // Parse with tree-sitter (incremental if we have existing tree)
                tree = parser.parse(&final_request.text, tree.as_ref());

                if let Some(ref ts_tree) = tree {
                    let mut effects = Vec::new();
                    let capture_names = query_clone.capture_names();

                    let mut matches = cursor.matches(
                        &query_clone,
                        ts_tree.root_node(),
                        final_request.text.as_bytes(),
                    );

                    // Extract syntax highlighting from tree-sitter results
                    let mut capture_count = 0;
                    while let Some(match_) = matches.next() {
                        for capture in match_.captures {
                            let capture_name = &capture_names[capture.index as usize];
                            capture_count += 1;

                            // Debug: log first few captures to see what tree-sitter is finding
                            if capture_count <= 10 {
                                let node_text = &final_request.text[capture.node.start_byte()
                                    ..capture.node.end_byte().min(final_request.text.len())];
                                println!(
                                    "SYNTAX: Capture #{}: name='{}', text='{}', range={}..{}",
                                    capture_count,
                                    capture_name,
                                    node_text.chars().take(20).collect::<String>(),
                                    capture.node.start_byte(),
                                    capture.node.end_byte()
                                );
                            }

                            if let Some(token) = Self::capture_name_to_token_type(capture_name) {
                                effects.push(TextEffect {
                                    range: capture.node.start_byte()..capture.node.end_byte(),
                                    effect: EffectType::Color(Self::token_type_to_color(token)),
                                    priority: priority::SYNTAX,
                                });
                            } else if capture_count <= 10 {
                                println!("  -> No token type mapping for '{}'", capture_name);
                            }
                        }
                    }

                    // Sort by range and remove overlaps for clean rendering
                    effects.sort_by_key(|e| (e.range.start, e.range.end));
                    let cleaned = Self::remove_overlaps(effects);

                    println!(
                        "SYNTAX: Generated {} effects for {} bytes of text",
                        cleaned.len(),
                        final_request.text.len()
                    );
                    if !cleaned.is_empty() {
                        println!("  First effect: {:?}", cleaned.first());
                    }

                    // Atomic swap - readers never block! Old highlighting stays until this completes
                    highlights_clone.store(Arc::new(cleaned));

                    // Store the tree for viewport queries
                    cached_tree_clone.store(Arc::new(Some(ts_tree.clone())));

                    // Mark first parse as complete
                    is_first_parse = false;
                }
            }
        });

        Ok(Self {
            highlights,
            tx,
            name: config.name,
            cached_tree,
            language: config.language,
            query,
        })
    }

    /// Create highlighter for Rust (convenience method)
    pub fn new_rust() -> Self {
        Self::new(Languages::rust()).expect("Failed to create Rust highlighter")
    }

    /// Map tree-sitter capture names to token types
    /// These are standard capture names used across tree-sitter grammars
    pub fn capture_name_to_token_type(name: &str) -> Option<TokenType> {
        match name {
            // Keywords
            "keyword"
            | "keyword.control"
            | "keyword.control.conditional"
            | "keyword.control.repeat"
            | "keyword.control.import"
            | "keyword.control.return"
            | "keyword.control.exception"
            | "keyword.function"
            | "keyword.operator"
            | "keyword.storage"
            | "keyword.storage.type"
            | "keyword.storage.modifier" => Some(TokenType::Keyword),

            // Functions
            "function" | "function.builtin" | "function.call" | "function.macro"
            | "function.method" | "method" | "method.call" => Some(TokenType::Function),

            // Types
            "type" | "type.builtin" | "type.primitive" | "type.qualifier" | "class"
            | "storage.type" => Some(TokenType::Type),

            // Strings
            "string" | "string.quoted" | "string.template" | "string.regex" | "string.special"
            | "string.escape" | "char" | "character" => Some(TokenType::String),

            // Numbers
            "number"
            | "constant.numeric"
            | "constant.numeric.integer"
            | "constant.numeric.float"
            | "float" => Some(TokenType::Number),

            // Comments
            "comment" | "comment.line" | "comment.block" | "comment.documentation" => {
                Some(TokenType::Comment)
            }

            // Constants
            "constant"
            | "constant.builtin"
            | "constant.language"
            | "boolean"
            | "constant.builtin.boolean" => Some(TokenType::Constant),

            // Operators
            "operator"
            | "operator.assignment"
            | "operator.arithmetic"
            | "operator.comparison"
            | "operator.logical" => Some(TokenType::Operator),

            // Punctuation
            "punctuation"
            | "punctuation.bracket"
            | "punctuation.delimiter"
            | "punctuation.separator"
            | "punctuation.special" => Some(TokenType::Punctuation),

            // Variables
            "variable"
            | "variable.builtin"
            | "variable.parameter"
            | "variable.other"
            | "variable.other.member" => Some(TokenType::Variable),

            // Attributes
            "attribute" | "decorator" | "annotation" => Some(TokenType::Attribute),

            // Namespaces
            "namespace" | "module" => Some(TokenType::Namespace),

            // Properties
            "property" | "field" => Some(TokenType::Property),

            // Parameters
            "parameter" | "label" => Some(TokenType::Parameter),

            _ => None,
        }
    }

    /// Convert token type to color (RGBA format)
    pub fn token_type_to_color(token: TokenType) -> u32 {
        match token {
            TokenType::Keyword => 0xC678DDFF,     // Purple
            TokenType::Function => 0x61AFEFFF,    // Blue
            TokenType::Type => 0xE5C07BFF,        // Yellow-orange
            TokenType::String => 0x98C379FF,      // Green
            TokenType::Number => 0xD19A66FF,      // Orange
            TokenType::Comment => 0x5C6370FF,     // Gray
            TokenType::Constant => 0xD19A66FF,    // Orange
            TokenType::Operator => 0x56B6C2FF,    // Cyan
            TokenType::Punctuation => 0xABB2BFFF, // Gray
            TokenType::Variable => 0xABB2BFFF,    // Default gray
            TokenType::Attribute => 0xE06C75FF,   // Red
            TokenType::Namespace => 0x61AFEFFF,   // Blue
            TokenType::Property => 0xE5C07BFF,    // Yellow
            TokenType::Parameter => 0xABB2BFFF,   // Gray
        }
    }

    /// Coalesce adjacent effects with the same color
    pub fn coalesce_effects(effects: Vec<TextEffect>) -> Vec<TextEffect> {
        if effects.is_empty() {
            return effects;
        }

        let mut coalesced = Vec::with_capacity(effects.len() / 2); // Estimate
        let mut current_effect: Option<TextEffect> = None;

        for effect in effects {
            if let Some(ref mut curr) = current_effect {
                // Check if we can coalesce with current effect
                if curr.range.end == effect.range.start
                    && curr.priority == effect.priority
                    && matches!(&curr.effect, EffectType::Color(c1) if matches!(&effect.effect, EffectType::Color(c2) if c1 == c2))
                {
                    // Extend current effect
                    curr.range.end = effect.range.end;
                } else {
                    // Can't coalesce, save current and start new
                    coalesced.push(curr.clone());
                    current_effect = Some(effect);
                }
            } else {
                // First effect
                current_effect = Some(effect);
            }
        }

        // Don't forget the last effect
        if let Some(curr) = current_effect {
            coalesced.push(curr);
        }

        coalesced
    }

    /// Get syntax effects for only the visible byte range - O(visible nodes)
    pub fn get_visible_effects(
        &self,
        text: &str,
        byte_range: std::ops::Range<usize>,
    ) -> Vec<TextEffect> {
        // Get the cached tree if available
        let tree_guard = self.cached_tree.load();
        let tree = match tree_guard.as_ref() {
            Some(tree) => tree,
            None => {
                // No tree yet, return empty
                return Vec::new();
            }
        };

        let mut effects = Vec::new();
        let mut cursor = QueryCursor::new();

        // Set the byte range for the query cursor - this is the key optimization!
        // tree-sitter will only visit nodes that intersect this range
        cursor.set_byte_range(byte_range.clone());

        let capture_names = self.query.capture_names();
        let mut matches = cursor.matches(&self.query, tree.root_node(), text.as_bytes());

        // Process only the visible matches
        while let Some(match_) = matches.next() {
            for capture in match_.captures {
                let capture_name = &capture_names[capture.index as usize];

                // Check if this capture is actually in our visible range
                let node_start = capture.node.start_byte();
                let node_end = capture.node.end_byte();

                if node_end < byte_range.start || node_start > byte_range.end {
                    continue; // Skip nodes outside visible range
                }

                if let Some(token) = Self::capture_name_to_token_type(capture_name) {
                    effects.push(TextEffect {
                        range: node_start..node_end,
                        effect: EffectType::Color(Self::token_type_to_color(token)),
                        priority: priority::SYNTAX,
                    });
                }
            }
        }

        // Sort, remove overlaps, and coalesce adjacent effects
        effects.sort_by_key(|e| (e.range.start, e.range.end));
        let cleaned = Self::remove_overlaps(effects);
        Self::coalesce_effects(cleaned)
    }

    /// Walk visible nodes in tree (alternative approach using tree walking)
    pub fn walk_visible_nodes(
        &self,
        text: &str,
        byte_range: std::ops::Range<usize>,
    ) -> Vec<TextEffect> {
        let tree_guard = self.cached_tree.load();
        let tree = match tree_guard.as_ref() {
            Some(tree) => tree,
            None => return Vec::new(),
        };

        let mut effects = Vec::new();
        let mut cursor = tree.root_node().walk();
        self.collect_visible_effects(&mut cursor, text, &byte_range, &mut effects);

        effects.sort_by_key(|e| (e.range.start, e.range.end));
        Self::remove_overlaps(effects)
    }

    fn collect_visible_effects(
        &self,
        cursor: &mut tree_sitter::TreeCursor,
        text: &str,
        visible_range: &std::ops::Range<usize>,
        effects: &mut Vec<TextEffect>,
    ) {
        let node = cursor.node();
        let node_start = node.start_byte();
        let node_end = node.end_byte();

        // Early exit if this entire subtree is outside visible range
        if node_end < visible_range.start || node_start > visible_range.end {
            return;
        }

        // Check if this node has a syntax capture
        let node_kind = node.kind();
        if let Some(token) = self.node_kind_to_token_type(node_kind) {
            effects.push(TextEffect {
                range: node_start..node_end,
                effect: EffectType::Color(Self::token_type_to_color(token)),
                priority: priority::SYNTAX,
            });
        }

        // Only recurse into children if this node intersects visible range
        if cursor.goto_first_child() {
            loop {
                self.collect_visible_effects(cursor, text, visible_range, effects);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            cursor.goto_parent();
        }
    }

    /// Map node kind directly to token type (simpler than capture names)
    fn node_kind_to_token_type(&self, kind: &str) -> Option<TokenType> {
        // This is a simplified version - would need language-specific mapping
        match kind {
            "string" | "string_literal" | "string_content" => Some(TokenType::String),
            "number" | "integer" | "float" => Some(TokenType::Number),
            "comment" | "line_comment" | "block_comment" => Some(TokenType::Comment),
            "function" | "function_declaration" | "method_declaration" => Some(TokenType::Function),
            "type" | "type_identifier" | "primitive_type" => Some(TokenType::Type),
            _ if kind.contains("keyword") => Some(TokenType::Keyword),
            _ if kind.contains("operator") => Some(TokenType::Operator),
            _ => None,
        }
    }

    /// Remove overlapping effects (keeps the more specific one)
    pub fn remove_overlaps(effects: Vec<TextEffect>) -> Vec<TextEffect> {
        if effects.is_empty() {
            return effects;
        }

        let mut result = Vec::with_capacity(effects.len());
        let mut last_end = 0;

        for effect in effects {
            // Skip effects that overlap with previous
            if effect.range.start < last_end {
                continue;
            }

            last_end = effect.range.end;
            result.push(effect);
        }

        result
    }
}

impl TextStyleProvider for SyntaxHighlighter {
    fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<TextEffect> {
        // For now, still use the full cached effects
        // In the future, this could be replaced with on-demand viewport queries
        let all_effects = self.highlights.load();

        // Binary search for efficient range query
        let start_idx = all_effects
            .binary_search_by_key(&range.start, |e| e.range.start)
            .unwrap_or_else(|i| i);

        let result: Vec<TextEffect> = all_effects[start_idx..]
            .iter()
            .take_while(|e| e.range.start < range.end)
            .cloned()
            .collect();

        result
    }

    // Remove the duplicate method - it's not part of the trait

    fn request_update(&self, text: &str, version: u64) {
        println!("SYNTAX: OLD request_update called (no InputEdit) - this should be avoided!");
        // Send to background thread (non-blocking) without edit info
        self.request_update_with_edit(text, version, None);
    }

    fn name(&self) -> &str {
        self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Convert byte position to tree-sitter Point using efficient tree navigation
fn byte_to_point(tree: &crate::tree::Tree, byte_pos: usize) -> Point {
    let line = tree.byte_to_line(byte_pos);

    // Get column by finding start of line and calculating offset
    let line_start = tree.line_to_byte(line).unwrap_or(0);
    let byte_in_line = byte_pos - line_start;

    // Get the line text to calculate visual column (accounting for tabs)
    let line_end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
    let line_text = tree.get_text_slice(line_start..line_end);

    let mut column = 0;
    let mut byte_offset = 0;

    for ch in line_text.chars() {
        if byte_offset >= byte_in_line {
            break;
        }
        if ch == '\t' {
            column = ((column / 4) + 1) * 4; // 4-space tabs
        } else {
            column += 1;
        }
        byte_offset += ch.len_utf8();
    }

    Point {
        row: line as usize,
        column,
    }
}

/// Create TextEdit from document edit information using tree navigation
pub fn create_text_edit(tree: &crate::tree::Tree, edit: &crate::tree::Edit) -> TextEdit {
    use crate::tree::Edit;

    match edit {
        Edit::Insert { pos, content } => {
            let content_text = match content {
                crate::tree::Content::Text(s) => s.clone(),
                crate::tree::Content::Widget(_) => String::new(), // Widgets don't affect text parsing
            };

            let start_point = byte_to_point(tree, *pos);
            let end_point = start_point; // Insert doesn't change end position initially

            // Calculate new end position after insert
            let new_end_point = {
                let mut line = start_point.row;
                let mut column = start_point.column;
                for ch in content_text.chars() {
                    if ch == '\n' {
                        line += 1;
                        column = 0;
                    } else {
                        column += 1;
                    }
                }
                Point {
                    row: line as usize,
                    column,
                }
            };

            TextEdit {
                start_byte: *pos,
                old_end_byte: *pos,
                new_end_byte: *pos + content_text.len(),
                start_position: start_point,
                old_end_position: end_point,
                new_end_position: new_end_point,
            }
        }
        Edit::Delete { range } => {
            let start_point = byte_to_point(tree, range.start);
            let old_end_point = byte_to_point(tree, range.end);

            TextEdit {
                start_byte: range.start,
                old_end_byte: range.end,
                new_end_byte: range.start, // Delete shrinks to start position
                start_position: start_point,
                old_end_position: old_end_point,
                new_end_position: start_point, // New end is at start after deletion
            }
        }
        Edit::Replace { range, content } => {
            let content_text = match content {
                crate::tree::Content::Text(s) => s.clone(),
                crate::tree::Content::Widget(_) => String::new(),
            };

            let start_point = byte_to_point(tree, range.start);
            let old_end_point = byte_to_point(tree, range.end);

            // Calculate new end position after replacement
            let new_end_point = {
                let mut line = start_point.row;
                let mut column = start_point.column;
                for ch in content_text.chars() {
                    if ch == '\n' {
                        line += 1;
                        column = 0;
                    } else {
                        column += 1;
                    }
                }
                Point {
                    row: line as usize,
                    column,
                }
            };

            TextEdit {
                start_byte: range.start,
                old_end_byte: range.end,
                new_end_byte: range.start + content_text.len(),
                start_position: start_point,
                old_end_position: old_end_point,
                new_end_position: new_end_point,
            }
        }
    }
}

/// Helper to create Rust syntax highlighter
pub fn create_rust_highlighter() -> Box<dyn TextStyleProvider> {
    Box::new(SyntaxHighlighter::new_rust())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_highlighting() {
        let highlighter = SyntaxHighlighter::new_rust();

        // Test basic interface
        assert_eq!(highlighter.name(), "rust");

        // Test range query (should return empty initially)
        let effects = highlighter.get_effects_in_range(0..100);
        assert_eq!(effects.len(), 0); // Empty until background parsing completes

        // Test update request (should not crash)
        highlighter.request_update("fn main() {}", 1);
    }

    #[test]
    fn test_language_agnostic() {
        // Test that we can create a highlighter with any language config
        let config = Languages::rust();
        let highlighter = SyntaxHighlighter::new(config);
        assert!(highlighter.is_ok());
    }

    #[test]
    fn test_debounced_updates() {
        let highlighter = SyntaxHighlighter::new_rust();

        // Multiple rapid updates should be debounced
        highlighter.request_update("fn main() {", 1);
        highlighter.request_update("fn main() { let x = 1;", 2);
        highlighter.request_update("fn main() { let x = 42;", 3);

        // Should not crash and should handle rapid updates gracefully
        assert_eq!(highlighter.name(), "rust");
    }
}
