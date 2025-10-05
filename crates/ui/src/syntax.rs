//! Language-agnostic tree-sitter syntax highlighting
//!
//! Supports any language with a tree-sitter grammar and highlight query

use crate::text_effects::{priority, EffectType, TextEffect, TextStyleProvider};
use arc_swap::ArcSwap;
use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree as TSTree,
    WasmStore,
};

// Generic WASM language loader with lazy initialization
// Uses parking_lot::RwLock for better read concurrency when many files open at once
struct WasmLanguageLoader {
    store: Mutex<Option<Box<WasmStore>>>,
    languages: RwLock<ahash::HashMap<&'static str, Language>>,
}

impl WasmLanguageLoader {
    fn new() -> Self {
        Self {
            store: Mutex::new(None),
            languages: RwLock::new(ahash::HashMap::default()),
        }
    }

    fn load(&self, name: &'static str, wasm_bytes: &[u8]) -> Language {
        // Fast path: check cache with read lock (allows concurrent reads)
        {
            let langs = self.languages.read();
            if let Some(lang) = langs.get(name) {
                return lang.clone();
            }
        }

        // Slow path: need to load the language
        // Take write lock to prevent duplicate loading
        let mut langs = self.languages.write();

        // Double-check pattern: another thread might have loaded it
        if let Some(lang) = langs.get(name) {
            return lang.clone();
        }

        // Initialize store if needed (only during first language load)
        let mut store_guard = self.store.lock();
        if store_guard.is_none() {
            let engine = tree_sitter::wasmtime::Engine::default();
            let store = Box::new(WasmStore::new(&engine).expect("Failed to create WasmStore"));
            let store_ptr: &'static mut WasmStore = Box::leak(store);
            unsafe {
                *store_guard = Some(Box::from_raw(store_ptr));
            }
        }

        // Load the language
        let store = store_guard.as_mut().unwrap();
        let language = store
            .load_language(name, wasm_bytes)
            .expect(&format!("Failed to load {} language", name));

        // Cache it (we already have write lock)
        langs.insert(name, language.clone());
        drop(store_guard); // Release store lock early

        language
    }
}

lazy_static::lazy_static! {
    static ref WASM_LOADER: WasmLanguageLoader = WasmLanguageLoader::new();
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
    /// Rust language configuration (native)
    pub fn rust() -> LanguageConfig {
        LanguageConfig {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
            name: "rust",
        }
    }

    /// WGSL language configuration (WASM)
    pub fn wgsl() -> LanguageConfig {
        const WASM: &[u8] = include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
        const HIGHLIGHTS: &str = include_str!("../assets/grammars/wgsl/highlights.scm");

        LanguageConfig {
            language: WASM_LOADER.load("wgsl", WASM),
            highlights_query: HIGHLIGHTS,
            name: "wgsl",
        }
    }

    /// TOML language configuration (WASM)
    pub fn toml() -> LanguageConfig {
        const WASM: &[u8] = include_bytes!("../assets/grammars/toml/tree-sitter-toml.wasm");
        const HIGHLIGHTS: &str = include_str!("../assets/grammars/toml/highlights.scm");

        LanguageConfig {
            language: WASM_LOADER.load("toml", WASM),
            highlights_query: HIGHLIGHTS,
            name: "toml",
        }
    }

    // Adding new WASM languages is now trivial:
    // pub fn javascript() -> LanguageConfig {
    //     const WASM: &[u8] = include_bytes!("../assets/grammars/js/tree-sitter-javascript.wasm");
    //     const HIGHLIGHTS: &str = include_str!("../assets/grammars/js/highlights.scm");
    //     LanguageConfig {
    //         language: WASM_LOADER.load("javascript", WASM),
    //         highlights_query: HIGHLIGHTS,
    //         name: "javascript",
    //     }
    // }
}

/// Syntax highlighting mode for debugging and validation
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SyntaxMode {
    /// Use incremental parsing with InputEdit for efficiency
    Incremental,
    /// Always do full reparse from scratch (slower but more reliable for debugging)
    FullReparse,
    /// Validate: run both modes and compare results (very slow, debug only)
    Validate,
}

/// Token types (universal across languages)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TokenType {
    // Basic tokens (0-14) - keep existing for compatibility
    Keyword,     // 1
    Function,    // 2
    Type,        // 3
    String,      // 4
    Number,      // 5
    Comment,     // 6
    Constant,    // 7
    Operator,    // 8
    Punctuation, // 9
    Variable,    // 10
    Attribute,   // 11
    Namespace,   // 12
    Property,    // 13
    Parameter,   // 14

    // Extended tokens for richer syntax highlighting (15+)
    Method,         // 15
    Field,          // 16
    Constructor,    // 17
    Enum,           // 18
    EnumMember,     // 19
    Interface,      // 20
    Struct,         // 21
    Class,          // 22
    Module,         // 23
    Macro,          // 24
    Label,          // 25
    KeywordControl, // 26 - if, else, match, loop, etc.

    // String variants
    StringEscape,        // 27
    StringInterpolation, // 28
    Regex,               // 29

    // Literal variants
    Boolean,   // 30
    Character, // 31
    Float,     // 32

    // Comment variants
    CommentDoc,  // 33
    CommentTodo, // 34

    // Operator variants
    ComparisonOp, // 35
    LogicalOp,    // 36
    ArithmeticOp, // 37

    // Punctuation variants
    Bracket,     // 38 - [], <>
    Brace,       // 39 - {}
    Parenthesis, // 40 - ()
    Delimiter,   // 41 - ::, ->
    Semicolon,   // 42
    Comma,       // 43

    // Special highlighting
    Error,      // 44
    Warning,    // 45
    Deprecated, // 46
    Unused,     // 47

    // Rust-specific semantic tokens
    SelfKeyword,   // 48
    Lifetime,      // 49
    TypeParameter, // 50
    Generic,       // 51
    Trait,         // 52
    Derive,        // 53
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
    /// Highlight query string (compiled lazily in background thread)
    highlights_query: &'static str,
    /// Syntax highlighting mode (for debugging/validation)
    mode: Arc<ArcSwap<SyntaxMode>>,
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
    /// Force fresh parse (discard old tree) - needed when multiple edits accumulate
    reset_tree: bool,
}

/// Viewport query request
pub struct ViewportQuery {
    pub byte_range: std::ops::Range<usize>,
}

impl SyntaxHighlighter {
    /// Set syntax highlighting mode (for debugging/validation)
    pub fn set_mode(&self, mode: SyntaxMode) {
        self.mode.store(Arc::new(mode));
    }

    /// Get current syntax highlighting mode
    pub fn get_mode(&self) -> SyntaxMode {
        **self.mode.load()
    }

    /// Apply an incremental edit for efficient reparsing
    pub fn apply_edit(&self, edit: TextEdit) {
        // Send edit to background thread
        let _ = self.tx.send(ParseRequest {
            text: String::new(), // Will be set by request_update_with_edit
            version: 0,          // Will be set by request_update_with_edit
            edit: Some(edit),
            reset_tree: false,
        });
    }

    /// Request update with edit information
    /// Note: For explicit reset (undo/redo), use request_update_with_reset directly
    pub fn request_update_with_edit(&self, text: &str, version: u64, edit: Option<TextEdit>) {
        // Don't override reset_tree - just pass through
        self.request_update_with_reset(text, version, edit, false);
    }

    /// Request update with optional tree reset
    pub fn request_update_with_reset(
        &self,
        text: &str,
        version: u64,
        edit: Option<TextEdit>,
        reset_tree: bool,
    ) {
        let _ = self.tx.send(ParseRequest {
            text: text.to_string(),
            version,
            edit,
            reset_tree,
        });
    }

    /// Create highlighter for any language with background parsing
    pub fn new(config: LanguageConfig) -> Result<Self, tree_sitter::LanguageError> {
        // Create the parser with proper setup here, then move to thread
        let mut parser = Parser::new();

        // For WASM languages, we need special handling
        if config.name == "wgsl" {
            // Create engine and store here
            let engine = Box::leak(Box::new(tree_sitter::wasmtime::Engine::default()));
            let mut store = WasmStore::new(engine).expect("Failed to create WasmStore");

            // Load the language into the store
            const WGSL_WASM: &[u8] =
                include_bytes!("../assets/grammars/wgsl/tree-sitter-wgsl.wasm");
            let language = store
                .load_language("wgsl", WGSL_WASM)
                .expect("Failed to load WGSL language");

            // Set up the parser
            parser
                .set_wasm_store(store)
                .expect("Failed to set WasmStore");
            parser.set_language(&language)?;
        } else if config.name == "toml" {
            // Create engine and store here
            let engine = Box::leak(Box::new(tree_sitter::wasmtime::Engine::default()));
            let mut store = WasmStore::new(engine).expect("Failed to create WasmStore");

            // Load the language into the store
            const TOML_WASM: &[u8] =
                include_bytes!("../assets/grammars/toml/tree-sitter-toml.wasm");
            let language = store
                .load_language("toml", TOML_WASM)
                .expect("Failed to load TOML language");

            // Set up the parser
            parser
                .set_wasm_store(store)
                .expect("Failed to set WasmStore");
            parser.set_language(&language)?;
        } else {
            parser.set_language(&config.language)?;
        }

        // Don't create query here - defer to background thread
        let language_clone = config.language.clone();
        let highlights_query_clone = config.highlights_query;

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
            let mut tree: Option<TSTree> = None;
            let mut cursor = QueryCursor::new();
            let mut last_text = String::new();
            let mut is_first_parse = true;
            // Lazy query compilation on first use
            let mut query: Option<Query> = None;

            while let Ok(request) = rx.recv() {
                // Shorter debounce for initial parse, longer for subsequent
                let debounce_ms = if is_first_parse { 10 } else { 100 };
                std::thread::sleep(std::time::Duration::from_millis(debounce_ms));

                // Drain any additional requests that came in during debounce
                let final_request = rx.try_iter().last().unwrap_or(request);

                // Skip if text hasn't changed (avoid redundant parsing)
                if final_request.text == last_text
                    && final_request.edit.is_none()
                    && !final_request.reset_tree
                {
                    continue;
                }
                last_text = final_request.text.clone();

                // Reset tree if requested (needed when multiple edits accumulate)
                if final_request.reset_tree {
                    tree = None;
                } else if let Some(edit) = &final_request.edit {
                    // Apply edit to existing tree for incremental parsing
                    if let Some(ref mut existing_tree) = tree {
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

                // Parse with tree-sitter (incremental if we have existing tree, fresh if reset)
                tree = parser.parse(&final_request.text, tree.as_ref());

                if let Some(ref ts_tree) = tree {
                    // Compile query on first use (lazy initialization)
                    if query.is_none() {
                        println!(
                            "SYNTAX [{}]: Compiling tree-sitter query on background thread...",
                            language_name
                        );
                        match Query::new(&language_clone, highlights_query_clone) {
                            Ok(compiled_query) => {
                                println!(
                                    "SYNTAX [{}]: Query compilation successful",
                                    language_name
                                );
                                query = Some(compiled_query);
                            }
                            Err(e) => {
                                println!(
                                    "SYNTAX [{}]: Failed to compile query: {:?}",
                                    language_name, e
                                );
                                continue;
                            }
                        }
                    }

                    let query_ref = query.as_ref().unwrap();
                    let mut effects = Vec::new();
                    let capture_names = query_ref.capture_names();

                    let mut matches = cursor.matches(
                        query_ref,
                        ts_tree.root_node(),
                        final_request.text.as_bytes(),
                    );

                    // Extract syntax highlighting from tree-sitter results
                    let mut capture_count = 0;
                    while let Some(match_) = matches.next() {
                        for capture in match_.captures {
                            let capture_name = &capture_names[capture.index as usize];
                            capture_count += 1;

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

                    // Atomic swap - readers never block! Old highlighting stays until this completes
                    highlights_clone.store(Arc::new(cleaned));

                    // Store the tree and corresponding text for viewport queries
                    cached_tree_clone.store(Arc::new(Some(ts_tree.clone())));
                    cached_text_clone.store(Arc::new(Some(final_request.text.clone())));
                    // IMPORTANT: Always increment version, never go backwards
                    // Version comparison doesn't work with undo/redo
                    let current_v = cached_version_clone.load(Ordering::Relaxed);
                    cached_version_clone.store(current_v + 1, Ordering::Relaxed);

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
            highlights_query: config.highlights_query,
            mode: Arc::new(ArcSwap::from_pointee(SyntaxMode::Incremental)), // Default to incremental
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

    /// Create highlighter for TOML (convenience method)
    pub fn new_toml() -> Self {
        Self::new(Languages::toml()).expect("Failed to create TOML highlighter")
    }

    /// Create highlighter based on file extension
    pub fn from_file_extension(extension: &str) -> Option<Self> {
        match extension.to_lowercase().as_str() {
            "rs" => Some(Self::new_rust()),
            "wgsl" => Some(Self::new_wgsl()),
            "toml" => Some(Self::new_toml()),
            _ => None,
        }
    }

    /// Get the language name for a file extension without creating a highlighter
    pub fn file_extension_to_language(path: &str) -> &'static str {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        match extension.to_lowercase().as_str() {
            "rs" => "rust",
            "wgsl" => "wgsl",
            "toml" => "toml",
            _ => "none", // Default to no highlighting
        }
    }

    /// Create highlighter based on file path
    pub fn from_file_path(path_raw: &str) -> Option<Self> {
        let path = std::path::Path::new(path_raw);
        if let Some(file_name) = path.file_name() {
            if file_name == "Cargo.lock" {
                return Some(Self::new_toml());
            }
        }

        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_file_extension)
    }

    /// Map tree-sitter capture names to token types
    /// These are standard capture names used across tree-sitter grammars
    pub fn capture_name_to_token_type(name: &str) -> Option<TokenType> {
        // Handle special cases first
        match name {
            "function.macro" => return Some(TokenType::Macro),
            "comment.documentation" | "string.documentation" => return Some(TokenType::CommentDoc),
            "derive" => return Some(TokenType::Derive),
            "escape" => return Some(TokenType::StringEscape),
            "constant.builtin" => return Some(TokenType::Number),
            _ => {}
        }

        // Check prefixes for common patterns
        if name.starts_with("keyword")
            || name == "storageclass"
            || name == "repeat"
            || name == "conditional"
        {
            Some(TokenType::Keyword)
        } else if name.starts_with("function") || name.starts_with("method") {
            Some(TokenType::Function)
        } else if name.starts_with("type") || name == "storage.type" {
            Some(TokenType::Type)
        } else if name.starts_with("string") || name == "char" || name == "character" {
            Some(TokenType::String)
        } else if name.starts_with("constant.numeric") || name == "number" || name == "float" {
            Some(TokenType::Number)
        } else if name.starts_with("comment") {
            Some(TokenType::Comment)
        } else if name.starts_with("constant") || name == "boolean" {
            Some(TokenType::Constant)
        } else if name.starts_with("punctuation") {
            Some(TokenType::Punctuation)
        } else if name.starts_with("variable") {
            Some(TokenType::Variable)
        } else if name.starts_with("attribute")
            || name == "decorator"
            || name == "annotation"
            || name == "tag.attribute"
        {
            Some(TokenType::Attribute)
        } else if name.starts_with("operator") {
            match name {
                "operator.comparison" => Some(TokenType::ComparisonOp),
                "operator.logical" => Some(TokenType::LogicalOp),
                "operator.arithmetic" => Some(TokenType::ArithmeticOp),
                _ => Some(TokenType::Operator),
            }
        } else {
            // Handle remaining specific cases
            match name {
                "class" | "class.builtin" => Some(TokenType::Class),
                "namespace" | "module" => Some(TokenType::Namespace),
                "property" | "field" | "key" => Some(TokenType::Property),
                "parameter" => Some(TokenType::Parameter),
                "label" => Some(TokenType::Label),
                "constructor" => Some(TokenType::Constructor),
                "enum" => Some(TokenType::Enum),
                "enum.member" | "enummember" => Some(TokenType::EnumMember),
                "struct" | "structure" => Some(TokenType::Struct),
                "interface" => Some(TokenType::Interface),
                "trait" => Some(TokenType::Trait),
                "lifetime" => Some(TokenType::Lifetime),
                "type.parameter" | "typeparameter" | "generic" => Some(TokenType::TypeParameter),
                "self" | "keyword.self" => Some(TokenType::SelfKeyword),
                "error" => Some(TokenType::Error),
                "warning" => Some(TokenType::Warning),
                "regex" | "regexp" => Some(TokenType::Regex),
                "char.literal" | "character.literal" => Some(TokenType::Character),
                "bracket" => Some(TokenType::Bracket),
                "brace" => Some(TokenType::Brace),
                "parenthesis" | "paren" => Some(TokenType::Parenthesis),
                "delimiter" => Some(TokenType::Delimiter),
                "semicolon" => Some(TokenType::Semicolon),
                "comma" => Some(TokenType::Comma),
                "tag" | "tag.builtin" => Some(TokenType::Type),
                "heading" | "title" => Some(TokenType::Keyword),
                _ => None,
            }
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

        for effect in effects {
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
        _text: &str, // Ignore the passed-in text
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

        // Create query on demand for viewport queries
        // This is a fallback - normally highlighting comes from the background thread
        let query = match Query::new(&self.language, self.highlights_query) {
            Ok(q) => q,
            Err(_) => return Vec::new(), // Query compilation failed
        };

        let mut effects = Vec::new();
        let mut cursor = QueryCursor::new();

        // Set the byte range for the query cursor - this is the key optimization!
        // tree-sitter will only visit nodes that intersect this range
        cursor.set_byte_range(byte_range.clone());

        let capture_names = query.capture_names();
        // Use the CACHED text that corresponds to the tree!
        let mut matches = cursor.matches(&query, tree.root_node(), cached_text.as_bytes());

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
/// IMPORTANT: Tree-sitter expects actual character columns, NOT visual columns
/// A tab character should count as 1 column, not expanded to tab width
fn byte_to_point(tree: &tiny_core::tree::Tree, byte_pos: usize) -> Point {
    let line = tree.byte_to_line(byte_pos);
    let line_start = tree.line_to_byte(line).unwrap_or(0);
    let byte_in_line = byte_pos - line_start;

    // Get the line text to count actual characters (not visual columns)
    let line_end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
    let line_text = tree.get_text_slice(line_start..line_end);

    // Count actual UTF-8 characters up to byte_in_line
    // Each character (including tab) counts as 1 column for tree-sitter
    let mut column = 0;
    let mut byte_offset = 0;
    for ch in line_text.chars() {
        if byte_offset >= byte_in_line {
            break;
        }
        column += 1; // Each character is 1 column (including tabs)
        byte_offset += ch.len_utf8();
    }

    Point {
        row: line as usize,
        column,
    }
}

/// Calculate new point position after inserting text
/// IMPORTANT: Tree-sitter expects actual character positions, NOT visual columns
/// A tab character should count as 1 column, not expanded to tab width
fn calc_new_point(start: Point, text: &str) -> Point {
    let mut line = start.row;
    let mut column = start.column;
    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            column = 0;
        } else {
            // Each character (including tab) counts as 1 column for tree-sitter
            column += 1;
        }
    }
    Point {
        row: line as usize,
        column,
    }
}

/// Create TextEdit from document edit information using tree navigation
pub fn create_text_edit(tree: &tiny_core::tree::Tree, edit: &tiny_core::tree::Edit) -> TextEdit {
    use tiny_core::tree::{Content, Edit};

    let (start_byte, old_end_byte, new_end_byte, content_text) = match edit {
        Edit::Insert { pos, content } => {
            let text = match content {
                Content::Text(s) => s.as_str(),
                Content::Spatial(_) => "",
            };
            (*pos, *pos, *pos + text.len(), text)
        }
        Edit::Delete { range } => (range.start, range.end, range.start, ""),
        Edit::Replace { range, content } => {
            let text = match content {
                Content::Text(s) => s.as_str(),
                Content::Spatial(_) => "",
            };
            (range.start, range.end, range.start + text.len(), text)
        }
    };

    let start_position = byte_to_point(tree, start_byte);
    let old_end_position = if old_end_byte == start_byte {
        start_position
    } else {
        byte_to_point(tree, old_end_byte)
    };
    let new_end_position = if content_text.is_empty() {
        start_position
    } else {
        calc_new_point(start_position, content_text)
    };

    TextEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position,
        old_end_position,
        new_end_position,
    }
}
