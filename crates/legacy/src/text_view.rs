//! TextView - Universal text display component
//!
//! Each TextView has its own Viewport with:
//! - `bounds`: WHERE to render (screen position + size)
//! - `scroll`: WHAT content to show (offset within document)
//! - `metrics`: HOW to measure text (font size, line height, etc.)
//!
//! TextRenderer outputs canonical (0,0)-relative glyphs.
//! TextView transforms them to screen coordinates in one step.

use crate::{
    coordinates::Viewport,
    scroll::Scrollable,
    syntax::SyntaxHighlighter,
    text_renderer::TextRenderer,
};
use std::sync::Arc;
use tiny_core::tree::{Doc, Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{GlyphInstance, LayoutPos, LogicalPixels};

/// Universal text display component
///
/// Used for:
/// - Main editor
/// - Search input fields
/// - Dropdown lists (file picker, grep results)
/// - Line numbers
/// - Status bar text
/// - Pop-up menus
pub struct TextView {
    /// The document (text storage)
    pub doc: Doc,

    /// Text layout engine (outputs (0,0)-relative glyphs)
    pub renderer: TextRenderer,

    /// Complete view configuration: bounds + scroll + metrics
    pub viewport: Viewport,

    /// Visibility flag
    pub visible: bool,

    /// Optional syntax highlighting
    pub syntax_highlighter: Option<Arc<SyntaxHighlighter>>,

    /// Internal padding (logical pixels) - insets text from bounds edges
    pub padding: f32,
}

impl TextView {
    /// Create a new TextView with document and viewport
    pub fn new(doc: Doc, viewport: Viewport) -> Self {
        Self {
            doc,
            renderer: TextRenderer::new(),
            viewport,
            visible: true,
            syntax_highlighter: None,
            padding: 0.0,
        }
    }

    /// Create from text string
    pub fn from_text(text: &str, viewport: Viewport) -> Self {
        Self::new(Doc::from_str(text), viewport)
    }

    /// Create empty TextView
    pub fn empty(viewport: Viewport) -> Self {
        Self::new(Doc::new(), viewport)
    }

    /// Set internal padding (applies to all edges)
    pub fn with_padding(mut self, padding: f32) -> Self {
        self.padding = padding;
        self
    }

    /// Set syntax highlighter
    pub fn with_syntax_highlighter(mut self, highlighter: Arc<SyntaxHighlighter>) -> Self {
        self.syntax_highlighter = Some(highlighter);
        self
    }

    /// Set visibility
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Get current text
    pub fn text(&self) -> Arc<String> {
        self.doc.read().flatten_to_string()
    }

    /// Set text (replaces all content)
    pub fn set_text(&mut self, text: &str) {
        // Clear document
        let current_len = self.doc.read().byte_count();
        if current_len > 0 {
            self.doc.edit(tiny_core::tree::Edit::Delete {
                range: 0..current_len,
            });
        }

        // Insert new text
        if !text.is_empty() {
            self.doc.edit(tiny_core::tree::Edit::Insert {
                pos: 0,
                content: tiny_core::tree::Content::Text(text.to_string()),
            });
        }

        self.doc.flush();
    }

    /// Clear all text
    pub fn clear(&mut self) {
        self.set_text("");
    }

    /// Update layout (shape text, build line cache)
    pub fn update_layout(&mut self, font_system: &SharedFontSystem) {
        let tree = self.doc.read();
        self.renderer.update_layout(&tree, font_system, &self.viewport, false);
        self.renderer.update_visible_range(&self.viewport, &tree);
    }

    /// Get scissor rect for this view (in physical pixels)
    ///
    /// Returns the full bounds as the scissor rect with margin for ascenders/descenders.
    /// This allows glyphs positioned with padding offset to render correctly,
    /// while still clipping anything that extends outside the bounds.
    ///
    /// The height is expanded to ensure at least one full line_height can render,
    /// accommodating glyphs that extend above/below the baseline.
    ///
    /// Note: Uses round() for consistent precision instead of truncation
    pub fn get_scissor_rect(&self) -> (u32, u32, u32, u32) {
        let scale = self.viewport.scale_factor;
        let line_height = self.viewport.metrics.line_height;

        // Use round() for all components to ensure consistent pixel alignment
        // and avoid off-by-one errors from mixing floor/ceil/truncation
        let x = (self.viewport.bounds.x.0 * scale).round().max(0.0) as u32;
        let y = (self.viewport.bounds.y.0 * scale).round().max(0.0) as u32;

        // Ensure scissor height is at least one line_height to accommodate
        // ascenders/descenders, plus a small margin for safety
        let margin = 4.0; // Small margin in logical pixels for edge cases
        let min_height = line_height + margin;
        let logical_height = self.viewport.bounds.height.0.max(min_height);

        let w = (self.viewport.bounds.width.0 * scale).round().max(1.0) as u32;
        let h = (logical_height * scale).round().max(1.0) as u32;
        (x, y, w, h)
    }

    /// Collect glyphs for rendering
    ///
    /// Single-step transform: canonical (0,0) → screen coordinates
    /// 1. Start with renderer's (0,0)-relative glyphs
    /// 2. Apply scroll: local_pos = glyph_pos - viewport.scroll
    /// 3. Apply bounds: screen_pos = viewport.bounds.origin + local_pos
    /// 4. Cull glyphs far outside viewport (conservative culling)
    /// 5. Convert to physical pixels
    ///
    /// NOTE: Caller MUST set GPU scissor rect to viewport.bounds to prevent overflow
    pub fn collect_glyphs(&self, font_system: &SharedFontSystem) -> Vec<GlyphInstance> {
        if !self.visible {
            return Vec::new();
        }

        let visible_glyphs = self.renderer.get_visible_glyphs_with_style();
        let mut instances = Vec::with_capacity(visible_glyphs.len());

        let line_height = self.viewport.metrics.line_height;
        let viewport_top = self.viewport.bounds.y.0;
        let viewport_bottom = self.viewport.bounds.y.0 + self.viewport.bounds.height.0;

        // Add generous margin to culling bounds to prevent edge-case culling issues
        // This ensures glyphs at boundaries, with descenders, or in small viewports aren't incorrectly culled
        let cull_margin = line_height; // One full line height of margin on each side

        for glyph in visible_glyphs {
            // Skip invisible glyphs (newlines, etc)
            if glyph.char == '\n' || glyph.tex_coords == [0.0, 0.0, 0.0, 0.0] {
                continue;
            }

            // Single-step transform: canonical → local → screen
            // 1. Glyph position in canonical (0,0)-relative space
            let canonical_x = glyph.layout_pos.x.0;
            let canonical_y = glyph.layout_pos.y.0;

            // 2. Apply scroll to get local position (within visible area)
            let local_x = canonical_x - self.viewport.scroll.x.0;
            let local_y = canonical_y - self.viewport.scroll.y.0;

            // 3. Apply bounds offset + padding to get screen position
            let screen_x = self.viewport.bounds.x.0 + self.padding + local_x;
            let screen_y = self.viewport.bounds.y.0 + self.padding + local_y;

            // 4. Conservative vertical culling with margin (GPU scissor handles final clipping)
            // Only cull glyphs that are completely outside viewport with generous margin
            // This prevents edge cases where glyphs at boundaries get incorrectly culled
            let line_top = screen_y;
            let line_bottom = screen_y + line_height;
            if line_bottom < viewport_top - cull_margin || line_top > viewport_bottom + cull_margin {
                continue;
            }

            // No horizontal culling - GPU scissor rect handles all clipping
            // This prevents glyphs from being culled too aggressively

            // 5. Convert to physical coordinates for crisp text rendering
            let physical_x = screen_x * self.viewport.scale_factor;
            let physical_y = screen_y * self.viewport.scale_factor;

            instances.push(GlyphInstance {
                pos: LayoutPos::new(physical_x, physical_y),
                tex_coords: glyph.tex_coords,
                relative_pos: glyph.relative_pos,
                shader_id: 0,
                token_id: glyph.token_id as u8,
                format: 0,
                _padding: [0, 0],
            });
        }

        instances
    }

    /// Collect background rectangles (selection highlights, current line, etc.)
    pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        use tiny_sdk::types::RectInstance;
        let mut rects = Vec::new();

        if !self.visible {
            return rects;
        }

        // TODO: Add selection rectangles, highlighted line, etc.
        // This will be handled by EditableTextView for editable content

        rects
    }

    /// Get line count
    pub fn line_count(&self) -> usize {
        self.renderer.line_cache.len()
    }

    /// Get content height in logical pixels
    pub fn content_height(&self) -> f32 {
        self.renderer.line_cache.len() as f32 * self.viewport.metrics.line_height
    }

    /// Get content width in logical pixels (approximate)
    pub fn content_width(&self) -> f32 {
        // TODO: Calculate actual max line width from line_cache
        self.viewport.bounds.width.0
    }
}

impl Scrollable for TextView {
    fn get_scroll(&self) -> Point {
        Point {
            x: LogicalPixels(self.viewport.scroll.x.0),
            y: LogicalPixels(self.viewport.scroll.y.0),
        }
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.viewport.scroll.x.0 = scroll.x.0;
        self.viewport.scroll.y.0 = scroll.y.0;
    }

    fn handle_scroll(&mut self, delta: Point, _viewport: &Viewport, _widget_bounds: Rect) -> bool {
        if !self.visible {
            return false;
        }

        // Apply scroll delta (inverted for natural scrolling)
        self.viewport.scroll.y.0 -= delta.y.0;
        self.viewport.scroll.x.0 -= delta.x.0;

        // Get content bounds
        let content_height = self.content_height();
        let content_width = self.content_width();

        // Clamp vertical scroll
        let visible_height = self.viewport.bounds.height.0;
        let max_scroll_y = (content_height - visible_height).max(0.0);
        self.viewport.scroll.y.0 = self.viewport.scroll.y.0.max(0.0).min(max_scroll_y);

        // Clamp horizontal scroll
        let visible_width = self.viewport.bounds.width.0;
        let max_scroll_x = (content_width - visible_width).max(0.0);
        self.viewport.scroll.x.0 = self.viewport.scroll.x.0.max(0.0).min(max_scroll_x);

        // Update visible range after scroll
        let tree = self.doc.read();
        self.renderer.update_visible_range(&self.viewport, &tree);

        true
    }

    fn get_content_bounds(&self, _viewport: &Viewport) -> Rect {
        Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(self.content_width()),
            height: LogicalPixels(self.content_height()),
        }
    }
}

impl Default for TextView {
    fn default() -> Self {
        // Create a default viewport (will be overridden by parent)
        let viewport = Viewport::new(800.0, 600.0, 1.0);
        Self::new(Doc::new(), viewport)
    }
}
