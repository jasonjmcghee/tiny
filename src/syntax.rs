//! Language-agnostic tree-sitter syntax highlighting
//!
//! Supports any language with a tree-sitter grammar and highlight query

use crate::text_effects::{priority, EffectType, TextEffect, TextStyleProvider};
use arc_swap::ArcSwap;
use std::sync::{mpsc, Arc};
use std::thread;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree as TSTree};

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
pub struct SyntaxHighlighter {
    /// Current highlights (lock-free read!)
    highlights: Arc<ArcSwap<Vec<TextEffect>>>,
    /// Send parse requests to background thread
    tx: mpsc::Sender<ParseRequest>,
    /// Provider name
    name: &'static str,
}

/// Parse request for background thread
struct ParseRequest {
    text: String,
    #[allow(dead_code)]
    version: u64,
}

impl SyntaxHighlighter {
    /// Create highlighter for any language with background parsing
    pub fn new(config: LanguageConfig) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&config.language)?;

        let query = Query::new(&config.language, config.highlights_query)
            .expect("Failed to create highlight query");

        let highlights = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let highlights_clone = highlights.clone();

        let (tx, rx) = mpsc::channel::<ParseRequest>();

        // Background parsing thread with debouncing
        thread::spawn(move || {
            let mut tree: Option<TSTree> = None;
            let mut cursor = QueryCursor::new();
            let mut last_text = String::new();

            while let Ok(request) = rx.recv() {
                // Debounce: wait for more requests, use the latest one
                std::thread::sleep(std::time::Duration::from_millis(200));

                // Drain any additional requests that came in during debounce
                let final_request = rx.try_iter().last().unwrap_or(request);

                // Skip if text hasn't changed (avoid redundant parsing)
                if final_request.text == last_text {
                    continue;
                }
                last_text = final_request.text.clone();

                // Parse with tree-sitter (incremental parsing for speed)
                tree = parser.parse(&final_request.text, tree.as_ref());

                if let Some(ref ts_tree) = tree {
                    let mut effects = Vec::new();
                    let capture_names = query.capture_names();

                    let mut matches =
                        cursor.matches(&query, ts_tree.root_node(), final_request.text.as_bytes());

                    // Extract syntax highlighting from tree-sitter results
                    while let Some(match_) = matches.next() {
                        for capture in match_.captures {
                            let capture_name = &capture_names[capture.index as usize];

                            if let Some(token) = Self::capture_name_to_token_type(capture_name) {
                                effects.push(TextEffect {
                                    range: capture.node.start_byte()..capture.node.end_byte(),
                                    effect: EffectType::Color(Self::token_type_to_color(token)),
                                    priority: priority::SYNTAX,
                                });
                            }
                        }
                    }

                    // Sort by range and remove overlaps for clean rendering
                    effects.sort_by_key(|e| (e.range.start, e.range.end));
                    let cleaned = Self::remove_overlaps(effects);

                    // Atomic swap - readers never block! Old highlighting stays until this completes
                    highlights_clone.store(Arc::new(cleaned));
                }
            }
        });

        Ok(Self {
            highlights,
            tx,
            name: config.name,
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

    /// Convert token type to color
    pub fn token_type_to_color(token: TokenType) -> u32 {
        match token {
            TokenType::Keyword => 0xFFC678DD,     // Purple
            TokenType::Function => 0xFF61AFEF,    // Blue
            TokenType::Type => 0xFFE5C07B,        // Yellow-orange
            TokenType::String => 0xFF98C379,      // Green
            TokenType::Number => 0xFFD19A66,      // Orange
            TokenType::Comment => 0xFF5C6370,     // Gray
            TokenType::Constant => 0xFFD19A66,    // Orange
            TokenType::Operator => 0xFF56B6C2,    // Cyan
            TokenType::Punctuation => 0xFFABB2BF, // Gray
            TokenType::Variable => 0xFFABB2BF,    // Default gray
            TokenType::Attribute => 0xFFE06C75,   // Red
            TokenType::Namespace => 0xFF61AFEF,   // Blue
            TokenType::Property => 0xFFE5C07B,    // Yellow
            TokenType::Parameter => 0xFFABB2BF,   // Gray
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
        let all_effects = self.highlights.load();

        // Binary search for efficient range query
        let start_idx = all_effects
            .binary_search_by_key(&range.start, |e| e.range.start)
            .unwrap_or_else(|i| i);

        all_effects[start_idx..]
            .iter()
            .take_while(|e| e.range.start < range.end)
            .cloned()
            .collect()
    }

    fn request_update(&self, text: &str, version: u64) {
        // Send to background thread (non-blocking)
        let _ = self.tx.send(ParseRequest {
            text: text.to_string(),
            version,
        });
    }

    fn name(&self) -> &str {
        self.name
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
