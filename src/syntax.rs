//! Language-agnostic tree-sitter syntax highlighting
//!
//! Supports any language with a tree-sitter grammar and highlight query

use crate::text_effects::{priority, EffectType, TextEffect, TextStyleProvider};
use arc_swap::ArcSwap;
use std::sync::{mpsc, Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree as TSTree,
    WasmStore,
};

// Global WasmStore that lives forever
lazy_static::lazy_static! {
    static ref WASM_STORE: Mutex<Option<Box<WasmStore>>> = Mutex::new(None);
    static ref WGSL_LANGUAGE: Mutex<Option<Language>> = Mutex::new(None);
}

fn get_wgsl_language() -> Language {
    // Check if already loaded
    {
        let lang_guard = WGSL_LANGUAGE.lock().unwrap();
        if let Some(lang) = lang_guard.as_ref() {
            return lang.clone();
        }
    }

    // Need to load it
    let mut store_guard = WASM_STORE.lock().unwrap();

    // Initialize store if needed
    if store_guard.is_none() {
        let engine = tree_sitter::wasmtime::Engine::default();
        let store = Box::new(WasmStore::new(&engine).expect("Failed to create WasmStore"));
        // Leak the box to make it live forever - this ensures the WasmStore outlives any Language references
        let store_ptr: &'static mut WasmStore = Box::leak(store);
        unsafe {
            *store_guard = Some(Box::from_raw(store_ptr));
        }
    }

    // Load language
    const WGSL_WASM: &[u8] = include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
    println!("Loading WGSL from WASM ({} bytes)...", WGSL_WASM.len());

    let store = store_guard.as_mut().unwrap();
    let language = store.load_language("wgsl", WGSL_WASM)
        .expect("Failed to load WGSL language");

    // Cache the language
    let mut lang_guard = WGSL_LANGUAGE.lock().unwrap();
    *lang_guard = Some(language.clone());

    println!("WGSL language loaded and cached");
    language
}

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

    /// WGSL language configuration (loads from WASM)
    pub fn wgsl() -> LanguageConfig {
        const WGSL_HIGHLIGHTS: &str = include_str!("../assets/grammars/wgsl/highlights.scm");

        let language = get_wgsl_language();

        LanguageConfig {
            language,
            highlights_query: WGSL_HIGHLIGHTS,
            name: "wgsl",
        }
    }

    // Future languages can be added here:
    // pub fn javascript() -> LanguageConfig { ... }
    // pub fn python() -> LanguageConfig { ... }
}

/// Token types (universal across languages)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TokenType {
    // Basic tokens (0-14) - keep existing for compatibility
    Keyword,        // 1
    Function,       // 2
    Type,          // 3
    String,        // 4
    Number,        // 5
    Comment,       // 6
    Constant,      // 7
    Operator,      // 8
    Punctuation,   // 9
    Variable,      // 10
    Attribute,     // 11
    Namespace,     // 12
    Property,      // 13
    Parameter,     // 14

    // Extended tokens for richer syntax highlighting (15+)
    Method,           // 15
    Field,            // 16
    Constructor,      // 17
    Enum,             // 18
    EnumMember,       // 19
    Interface,        // 20
    Struct,           // 21
    Class,            // 22
    Module,           // 23
    Macro,            // 24
    Label,            // 25
    KeywordControl,   // 26 - if, else, match, loop, etc.

    // String variants
    StringEscape,     // 27
    StringInterpolation, // 28
    Regex,            // 29

    // Literal variants
    Boolean,          // 30
    Character,        // 31
    Float,            // 32

    // Comment variants
    CommentDoc,       // 33
    CommentTodo,      // 34

    // Operator variants
    ComparisonOp,     // 35
    LogicalOp,        // 36
    ArithmeticOp,     // 37

    // Punctuation variants
    Bracket,          // 38 - [], <>
    Brace,            // 39 - {}
    Parenthesis,      // 40 - ()
    Delimiter,        // 41 - ::, ->
    Semicolon,        // 42
    Comma,            // 43

    // Special highlighting
    Error,            // 44
    Warning,          // 45
    Deprecated,       // 46
    Unused,           // 47

    // Rust-specific semantic tokens
    SelfKeyword,      // 48
    Lifetime,         // 49
    TypeParameter,    // 50
    Generic,          // 51
    Trait,            // 52
    Derive,           // 53
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
    /// Cached text that corresponds to the cached tree
    cached_text: Arc<ArcSwap<Option<String>>>,
    /// Version of the cached text/tree
    cached_version: Arc<AtomicU64>,
    /// Language for creating queries
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
        // Create the parser with proper setup here, then move to thread
        let mut parser = Parser::new();

        // For WGSL, we need special handling
        if config.name == "wgsl" {
            // Create engine and store here
            let engine = Box::leak(Box::new(tree_sitter::wasmtime::Engine::default()));
            let mut store = WasmStore::new(engine).expect("Failed to create WasmStore");

            // Load the language into the store
            const WGSL_WASM: &[u8] = include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
            let language = store.load_language("wgsl", WGSL_WASM)
                .expect("Failed to load WGSL language");

            // Set up the parser
            parser.set_wasm_store(store).expect("Failed to set WasmStore");
            parser.set_language(&language)?;
        } else {
            parser.set_language(&config.language)?;
        }

        let query = Arc::new(
            Query::new(&config.language, config.highlights_query)
                .expect("Failed to create highlight query"),
        );
        let query_clone = query.clone();

        let highlights = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let highlights_clone = highlights.clone();
        let cached_tree = Arc::new(ArcSwap::from_pointee(None));
        let cached_tree_clone = cached_tree.clone();
        let cached_text = Arc::new(ArcSwap::from_pointee(None));
        let cached_text_clone = cached_text.clone();
        let cached_version = Arc::new(AtomicU64::new(0));
        let cached_version_clone = cached_version.clone();

        let (tx, rx) = mpsc::channel::<ParseRequest>();

        let language_name = config.name;

        // Background parsing thread with debouncing - move parser into it
        thread::spawn(move || {
            println!("SYNTAX: Background thread started for {}", language_name);

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
                    "SYNTAX [{}]: Parsing {} bytes with tree-sitter (incremental={})",
                    language_name,
                    final_request.text.len(),
                    final_request.edit.is_some()
                );
                // Parse with tree-sitter (incremental if we have existing tree)
                tree = parser.parse(&final_request.text, tree.as_ref());

                if tree.is_none() {
                    println!("SYNTAX [{}]: WARNING - Failed to parse!", language_name);
                }

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
                                    effect: EffectType::Token(Self::token_type_to_id(token)),
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

                    // Store the tree and corresponding text for viewport queries
                    cached_tree_clone.store(Arc::new(Some(ts_tree.clone())));
                    cached_text_clone.store(Arc::new(Some(final_request.text.clone())));
                    cached_version_clone.store(final_request.version, Ordering::Relaxed);

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
            cached_text,
            cached_version,
            language: config.language,
            query,
        })
    }

    /// Create highlighter for Rust (convenience method)
    pub fn new_rust() -> Self {
        Self::new(Languages::rust()).expect("Failed to create Rust highlighter")
    }

    /// Create highlighter for WGSL (convenience method)
    pub fn new_wgsl() -> Self {
        Self::new(Languages::wgsl()).expect("Failed to create WGSL highlighter")
    }

    /// Create highlighter based on file extension
    pub fn from_file_extension(extension: &str) -> Option<Self> {
        match extension.to_lowercase().as_str() {
            "rs" => Some(Self::new_rust()),
            "wgsl" => Some(Self::new_wgsl()),
            _ => None,
        }
    }

    /// Create highlighter based on file path
    pub fn from_file_path(path: &str) -> Option<Self> {
        std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_file_extension)
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
            "function" | "function.builtin" | "function.call"
            | "function.method" | "method" | "method.call" => Some(TokenType::Function),

            // Macros get special treatment
            "function.macro" => Some(TokenType::Macro),

            // Types
            "type" | "type.builtin" | "type.primitive" | "type.qualifier" | "class"
            | "storage.type" => Some(TokenType::Type),

            // Strings and escape sequences
            "string" | "string.quoted" | "string.template" | "string.regex" | "string.special"
            | "string.escape" | "char" | "character" => Some(TokenType::String),

            // Escape sequences get special treatment
            "escape" => Some(TokenType::StringEscape),

            // Numbers
            "number"
            | "constant.numeric"
            | "constant.numeric.integer"
            | "constant.numeric.float"
            | "float" => Some(TokenType::Number),

            // Comments
            "comment" | "comment.line" | "comment.block" => Some(TokenType::Comment),
            "comment.documentation" => Some(TokenType::CommentDoc),

            // Constants - keep separate from numbers for proper semantic highlighting
            "constant"
            | "constant.language"
            | "boolean"
            | "constant.builtin.boolean" => Some(TokenType::Constant),

            // constant.builtin in Rust is often numbers, but could be other things
            "constant.builtin" => Some(TokenType::Number),

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

            // Attributes - including derive in Rust
            "attribute" | "decorator" | "annotation" | "attribute.builtin" => Some(TokenType::Attribute),

            // Special Rust attributes
            "derive" => Some(TokenType::Derive),

            // Namespaces and modules
            "namespace" | "module" => Some(TokenType::Namespace),

            // Properties and fields
            "property" | "field" => Some(TokenType::Property),

            // Parameters and labels
            "parameter" => Some(TokenType::Parameter),
            "label" => Some(TokenType::Label),

            // Constructor
            "constructor" => Some(TokenType::Constructor),

            // Enums and enum members
            "enum" => Some(TokenType::Enum),
            "enum.member" | "enummember" => Some(TokenType::EnumMember),

            // Structs, classes, interfaces
            "struct" => Some(TokenType::Struct),
            "class" | "class.builtin" => Some(TokenType::Class),
            "interface" => Some(TokenType::Interface),

            // Traits (Rust-specific but useful)
            "trait" => Some(TokenType::Trait),

            // Lifetimes (Rust-specific)
            "lifetime" => Some(TokenType::Lifetime),

            // Type parameters and generics
            "type.parameter" | "typeparameter" | "generic" => Some(TokenType::TypeParameter),

            // Self keyword (Rust/Python)
            "self" | "keyword.self" => Some(TokenType::SelfKeyword),

            // Error and warning markers
            "error" => Some(TokenType::Error),
            "warning" => Some(TokenType::Warning),

            // Additional string types
            "string.documentation" => Some(TokenType::CommentDoc),
            "regex" | "regexp" => Some(TokenType::Regex),
            "char.literal" | "character.literal" => Some(TokenType::Character),

            // Additional operators
            "operator.comparison" => Some(TokenType::ComparisonOp),
            "operator.logical" => Some(TokenType::LogicalOp),
            "operator.arithmetic" => Some(TokenType::ArithmeticOp),

            // Additional punctuation
            "bracket" => Some(TokenType::Bracket),
            "brace" => Some(TokenType::Brace),
            "parenthesis" | "paren" => Some(TokenType::Parenthesis),
            "delimiter" => Some(TokenType::Delimiter),
            "semicolon" => Some(TokenType::Semicolon),
            "comma" => Some(TokenType::Comma),

            // Tag names (for HTML/XML/JSX)
            "tag" | "tag.builtin" => Some(TokenType::Type),
            "tag.attribute" => Some(TokenType::Attribute),

            // JSON/YAML/TOML keys
            "key" => Some(TokenType::Property),

            // SQL keywords
            "keyword.sql" => Some(TokenType::Keyword),

            // Markdown headers
            "heading" | "title" => Some(TokenType::Keyword),

            // WGSL-specific captures
            "storageclass" => Some(TokenType::Keyword),
            "structure" => Some(TokenType::Struct),
            "repeat" => Some(TokenType::Keyword),
            "conditional" => Some(TokenType::Keyword),
            "keyword.function" => Some(TokenType::Keyword),
            "keyword.return" => Some(TokenType::Keyword),
            "function.call" => Some(TokenType::Function),

            // Catch-all for unrecognized names - return None to skip
            _ => None,
        }
    }

    /// Convert token type to token ID for theme lookup
    pub fn token_type_to_id(token: TokenType) -> u8 {
        match token {
            // Basic tokens (1-14) - maintain compatibility
            TokenType::Keyword => 1,
            TokenType::Function => 2,
            TokenType::Type => 3,
            TokenType::String => 4,
            TokenType::Number => 5,
            TokenType::Comment => 6,
            TokenType::Constant => 7,
            TokenType::Operator => 8,
            TokenType::Punctuation => 9,
            TokenType::Variable => 10,
            TokenType::Attribute => 11,
            TokenType::Namespace => 12,
            TokenType::Property => 13,
            TokenType::Parameter => 14,

            // Extended tokens (15+)
            TokenType::Method => 15,
            TokenType::Field => 16,
            TokenType::Constructor => 17,
            TokenType::Enum => 18,
            TokenType::EnumMember => 19,
            TokenType::Interface => 20,
            TokenType::Struct => 21,
            TokenType::Class => 22,
            TokenType::Module => 23,
            TokenType::Macro => 24,
            TokenType::Label => 25,
            TokenType::KeywordControl => 26,

            // String variants
            TokenType::StringEscape => 27,
            TokenType::StringInterpolation => 28,
            TokenType::Regex => 29,

            // Literal variants
            TokenType::Boolean => 30,
            TokenType::Character => 31,
            TokenType::Float => 32,

            // Comment variants
            TokenType::CommentDoc => 33,
            TokenType::CommentTodo => 34,

            // Operator variants
            TokenType::ComparisonOp => 35,
            TokenType::LogicalOp => 36,
            TokenType::ArithmeticOp => 37,

            // Punctuation variants
            TokenType::Bracket => 38,
            TokenType::Brace => 39,
            TokenType::Parenthesis => 40,
            TokenType::Delimiter => 41,
            TokenType::Semicolon => 42,
            TokenType::Comma => 43,

            // Special highlighting
            TokenType::Error => 44,
            TokenType::Warning => 45,
            TokenType::Deprecated => 46,
            TokenType::Unused => 47,

            // Rust-specific semantic tokens
            TokenType::SelfKeyword => 48,
            TokenType::Lifetime => 49,
            TokenType::TypeParameter => 50,
            TokenType::Generic => 51,
            TokenType::Trait => 52,
            TokenType::Derive => 53,
        }
    }

    /// Coalesce adjacent effects with the same shader and params
    pub fn coalesce_effects(effects: Vec<TextEffect>) -> Vec<TextEffect> {
        if effects.is_empty() {
            return effects;
        }

        let mut coalesced = Vec::with_capacity(effects.len() / 2); // Estimate
        let mut current_effect: Option<TextEffect> = None;

        for mut effect in effects {
            if let Some(ref mut curr) = current_effect {
                // Check if we can coalesce with current effect
                let can_coalesce = curr.range.end == effect.range.start
                    && curr.priority == effect.priority
                    && curr.effect == effect.effect;

                if can_coalesce {
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

    /// Get the version of the cached syntax data
    pub fn cached_version(&self) -> u64 {
        self.cached_version.load(Ordering::Relaxed)
    }

    /// Get syntax effects for only the visible byte range - O(visible nodes)
    pub fn get_visible_effects(
        &self,
        _text: &str,  // Ignore the passed-in text
        byte_range: std::ops::Range<usize>,
    ) -> Vec<TextEffect> {
        // Get the cached tree and text - they must match!
        let tree_guard = self.cached_tree.load();
        let text_guard = self.cached_text.load();

        let (tree, cached_text) = match (tree_guard.as_ref(), text_guard.as_ref()) {
            (Some(tree), Some(text)) => (tree, text),
            _ => {
                // No tree or text yet, return empty
                return Vec::new();
            }
        };

        let mut effects = Vec::new();
        let mut cursor = QueryCursor::new();

        // Set the byte range for the query cursor - this is the key optimization!
        // tree-sitter will only visit nodes that intersect this range
        cursor.set_byte_range(byte_range.clone());

        let capture_names = self.query.capture_names();
        // Use the CACHED text that corresponds to the tree!
        let mut matches = cursor.matches(&self.query, tree.root_node(), cached_text.as_bytes());

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
                        effect: EffectType::Token(Self::token_type_to_id(token)),
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
                effect: EffectType::Token(Self::token_type_to_id(token)),
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

/// Helper to create WGSL syntax highlighter
pub fn create_wgsl_highlighter() -> Box<dyn TextStyleProvider> {
    Box::new(SyntaxHighlighter::new_wgsl())
}

/// Helper to create syntax highlighter based on file extension
/// Falls back to Rust if extension is not recognized
pub fn create_highlighter_for_file(path: &str) -> Box<dyn TextStyleProvider> {
    SyntaxHighlighter::from_file_path(path)
        .map(|h| Box::new(h) as Box<dyn TextStyleProvider>)
        .unwrap_or_else(|| create_rust_highlighter())
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

    #[test]
    fn test_wgsl_highlighting() {
        let highlighter = SyntaxHighlighter::new_wgsl();

        // Test basic interface
        assert_eq!(highlighter.name(), "wgsl");

        // Test that WGSL highlighter was created successfully
        let wgsl_code = r#"
@vertex
fn main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 1.0);
}
"#;

        // Request an update (should not crash)
        highlighter.request_update(wgsl_code, 1);
    }

    #[test]
    fn test_wgsl_capture_names() {
        use tree_sitter::{Parser, Query, QueryCursor};

        println!("Loading WGSL using proper WasmStore setup...");

        // Create engine and store
        let engine = tree_sitter::wasmtime::Engine::default();
        let mut store = WasmStore::new(&engine).expect("Failed to create WasmStore");

        // Load WASM bytes
        const WGSL_WASM: &[u8] = include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
        println!("WASM size: {} bytes", WGSL_WASM.len());

        // Load language from WASM
        let language = store.load_language("wgsl", WGSL_WASM)
            .expect("Failed to load WGSL language");

        println!("Language loaded, version: {:?}", language.version());

        // Create parser and set the store FIRST
        let mut parser = Parser::new();
        parser.set_wasm_store(store)
            .expect("Failed to set WasmStore on parser");

        // Now set the language
        match parser.set_language(&language) {
            Ok(()) => println!("Successfully set WGSL language in parser"),
            Err(e) => {
                println!("Failed to set language: {:?}", e);
                return;
            }
        }

        // Check if parser has a language set after setting it
        match parser.language() {
            Some(lang) => println!("Parser has language set, version: {:?}", lang.version()),
            None => {
                println!("ERROR: Parser has no language set!");
                return;
            }
        }

        // Try various WGSL code samples to test parsing
        let test_cases = vec![
            "fn main() {}",
            "let x = 42;",
            "@vertex fn vs() {}",
            "var<private> x: f32;",
            "",  // Empty should parse
        ];

        for code in test_cases {
            println!("\nTrying to parse: '{}'", code);
            let tree = parser.parse(code, None);
            if let Some(tree) = tree {
                let root = tree.root_node();
                println!("  Success! Root node kind: {}", root.kind());
                if root.has_error() {
                    println!("  Warning: Tree has errors");
                }
            } else {
                println!("  Failed to parse!");
            }
        }

        // Now test with the highlights query
        let source_code = "@vertex fn main() { let x: f32 = 1.0; }";
        println!("\nTesting highlights with: '{}'", source_code);

        let tree = parser.parse(source_code, None);
        if tree.is_none() {
            println!("Failed to parse WGSL code for highlights!");
            return;
        }
        let tree = tree.unwrap();

        const WGSL_HIGHLIGHTS: &str = include_str!("../assets/grammars/wgsl/highlights.scm");
        let query = Query::new(&language, WGSL_HIGHLIGHTS).unwrap();

        let mut cursor = QueryCursor::new();
        let capture_names = query.capture_names();

        println!("Available capture names in tree-sitter-wgsl:");
        for (i, name) in capture_names.iter().enumerate() {
            println!("  {}: {}", i, name);
        }

        println!("\nActual captures in WGSL sample code:");
        let mut matches = cursor.matches(&query, tree.root_node(), source_code.as_bytes());
        let mut count = 0;
        while let Some(match_) = matches.next() {
            for capture in match_.captures {
                if count < 30 {
                    let capture_name = &capture_names[capture.index as usize];
                    let node_text = &source_code[capture.node.start_byte()..capture.node.end_byte()];
                    let token_type = SyntaxHighlighter::capture_name_to_token_type(capture_name);
                    println!("  '{}' -> '{}' -> {:?}", node_text, capture_name, token_type);
                    count += 1;
                }
            }
        }
    }

    #[test]
    fn test_file_extension_detection() {
        // Test Rust file detection
        let rust_highlighter = SyntaxHighlighter::from_file_path("main.rs");
        assert!(rust_highlighter.is_some());
        assert_eq!(rust_highlighter.unwrap().name(), "rust");

        // Test WGSL file detection
        let wgsl_highlighter = SyntaxHighlighter::from_file_path("shader.wgsl");
        assert!(wgsl_highlighter.is_some());
        assert_eq!(wgsl_highlighter.unwrap().name(), "wgsl");

        // Test unsupported extension
        let unknown = SyntaxHighlighter::from_file_path("test.txt");
        assert!(unknown.is_none());
    }

    #[test]
    fn test_capture_names() {
        use tree_sitter::{Parser, Query, QueryCursor};

        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();

        let source_code = r#"
fn main() {
    let x = 42;
    println!("Hello, {}", x);
}
"#;

        let tree = parser.parse(source_code, None).unwrap();
        let query = Query::new(
            &tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ).unwrap();

        let mut cursor = QueryCursor::new();
        let capture_names = query.capture_names();

        println!("Available capture names in tree-sitter-rust:");
        for (i, name) in capture_names.iter().enumerate() {
            println!("  {}: {}", i, name);
        }

        println!("\nActual captures in sample code:");
        let mut matches = cursor.matches(&query, tree.root_node(), source_code.as_bytes());
        let mut count = 0;
        while let Some(match_) = matches.next() {
            for capture in match_.captures {
                if count < 30 {
                    let capture_name = &capture_names[capture.index as usize];
                    let node_text = &source_code[capture.node.start_byte()..capture.node.end_byte()];
                    let token_type = SyntaxHighlighter::capture_name_to_token_type(capture_name);
                    println!("  '{}' -> '{}' -> {:?}", node_text, capture_name, token_type);
                    count += 1;
                }
            }
        }
    }
}
