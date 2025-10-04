//! Core TextBuffer - Doc + Layout
//!
//! Universal text storage using Doc (rope-based) for everything:
//! - Editor content
//! - Search inputs
//! - File picker
//! - Line numbers
//! - Diagnostics/popups
//! - Menus, status bars

use std::sync::Arc;
use tiny_core::tree::Doc;
use crate::text_renderer::TextRenderer;
use crate::syntax::SyntaxHighlighter;

/// Core text buffer - Doc + layout
///
/// This is the foundation for all text display in the editor.
/// Uses Doc for efficient text storage and TextRenderer for layout caching.
pub struct TextBuffer {
    /// The document (rope-based text storage)
    pub doc: Doc,

    /// Layout cache (positions, lines, culling)
    pub layout: TextRenderer,

    /// Optional syntax highlighting
    pub syntax_highlighter: Option<Arc<SyntaxHighlighter>>,

    /// Last layout version to detect when rebuild is needed
    last_layout_version: u64,
}

impl TextBuffer {
    /// Create a new empty text buffer
    pub fn new() -> Self {
        Self {
            doc: Doc::new(),
            layout: TextRenderer::new(),
            syntax_highlighter: None,
            last_layout_version: 0,
        }
    }

    /// Create from existing text
    pub fn from_str(text: &str) -> Self {
        Self {
            doc: Doc::from_str(text),
            layout: TextRenderer::new(),
            syntax_highlighter: None,
            last_layout_version: 0,
        }
    }

    /// Create from existing Doc
    pub fn from_doc(doc: Doc) -> Self {
        Self {
            doc,
            layout: TextRenderer::new(),
            syntax_highlighter: None,
            last_layout_version: 0,
        }
    }

    /// Set syntax highlighter
    pub fn with_syntax_highlighter(mut self, highlighter: Arc<SyntaxHighlighter>) -> Self {
        self.syntax_highlighter = Some(highlighter);
        self
    }

    /// Get current text as string (flattened)
    pub fn text(&self) -> Arc<String> {
        self.doc.read().flatten_to_string()
    }

    /// Get document version
    pub fn version(&self) -> u64 {
        self.doc.version()
    }

    /// Check if layout needs update
    pub fn layout_needs_update(&self) -> bool {
        self.doc.version() != self.last_layout_version
    }

    /// Update layout if needed
    pub fn update_layout(
        &mut self,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
    ) {
        if self.layout_needs_update() {
            let tree = self.doc.read();
            self.layout.update_layout(&tree, font_system, viewport);
            self.last_layout_version = self.doc.version();
        }
    }

    /// Force layout update (even if version unchanged)
    pub fn force_layout_update(
        &mut self,
        font_system: &tiny_font::SharedFontSystem,
        viewport: &crate::coordinates::Viewport,
    ) {
        let tree = self.doc.read();
        self.layout.update_layout_internal(&tree, font_system, viewport, true);
        self.last_layout_version = self.doc.version();
    }

    /// Update visible range for culling
    pub fn update_visible_range(&mut self, viewport: &crate::coordinates::Viewport) {
        let tree = self.doc.read();
        self.layout.update_visible_range(viewport, &tree);
    }

    /// Get visible glyphs for rendering
    pub fn visible_glyphs(&self) -> Vec<crate::text_renderer::UnifiedGlyph> {
        self.layout.get_visible_glyphs_with_style()
    }

    /// Get line count
    pub fn line_count(&self) -> usize {
        self.layout.line_cache.len()
    }

    /// Get content height in logical pixels
    pub fn content_height(&self, line_height: f32) -> f32 {
        self.layout.line_cache.len() as f32 * line_height
    }

    /// Apply an edit to the document
    pub fn apply_edit(&mut self, edit: tiny_core::tree::Edit) {
        // Apply incremental edit tracking for syntax
        self.layout.apply_incremental_edit(&edit);

        // Apply to document
        self.doc.edit(edit);

        // Force flush to apply pending edits immediately
        // (Doc buffers edits and only auto-flushes after 16 edits)
        self.doc.flush();
    }

    /// Insert text at position
    pub fn insert(&mut self, pos: usize, text: &str) {
        self.apply_edit(tiny_core::tree::Edit::Insert {
            pos,
            content: tiny_core::tree::Content::Text(text.to_string()),
        });
    }

    /// Delete range
    pub fn delete(&mut self, range: std::ops::Range<usize>) {
        self.apply_edit(tiny_core::tree::Edit::Delete { range });
    }

    /// Replace range with text
    pub fn replace(&mut self, range: std::ops::Range<usize>, text: &str) {
        self.apply_edit(tiny_core::tree::Edit::Replace {
            range,
            content: tiny_core::tree::Content::Text(text.to_string()),
        });
    }

    /// Clear all text
    pub fn clear(&mut self) {
        let text_arc = self.text();
        let len = text_arc.len();
        if len > 0 {
            self.delete(0..len);
        }
    }

    /// Set text (replaces all content)
    pub fn set_text(&mut self, text: &str) {
        self.clear();
        if !text.is_empty() {
            self.insert(0, text);
        }
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}
