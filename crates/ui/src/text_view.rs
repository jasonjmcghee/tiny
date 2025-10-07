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
    capabilities::TextViewCapabilities,
    coordinates::Viewport,
    scroll::Scrollable,
    syntax::SyntaxHighlighter,
    text_renderer::TextRenderer,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tiny_core::tree::{Doc, Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{GlyphInstance, LayoutPos, LogicalPixels};

/// Sizing constraint for width or height
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeConstraint {
    /// Fixed size in logical pixels
    Fixed(f32),
    /// Size to fit content (intrinsic size)
    HugContents,
    /// Fill available space in container
    FillContainer,
}

/// Text alignment
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Arrow direction for keyboard navigation (used by EditableTextView)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArrowDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Universal text display component
///
/// Capabilities control which plugins are active (cursor, selection, etc.)
/// The actual interaction is handled by InputHandler + plugins.
///
/// Used for:
/// - Main editor (full_editor capabilities)
/// - Search input fields (editable capabilities)
/// - Dropdown lists (selectable capabilities)
/// - Line numbers (read_only capabilities)
/// - Status bar text (read_only capabilities)
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

    /// Horizontal padding (logical pixels) - insets text from left/right edges
    pub padding_x: f32,

    /// Vertical padding (logical pixels) - insets text from top/bottom edges
    pub padding_y: f32,

    /// Cached glyphs (at canonical positions before scroll/viewport offset)
    cached_glyphs: Vec<GlyphInstance>,
    /// Last layout version when glyphs were generated (atomic for lock-free check)
    cached_glyph_version: AtomicU64,
    /// Last layout version that was rendered (for external change detection)
    last_rendered_layout_version: AtomicU64,
    /// Last scroll position when glyphs were generated
    cached_scroll_y: f32,
    /// Last scroll position when glyphs were generated
    cached_scroll_x: f32,
    /// Last bounds when glyphs were generated
    cached_bounds: Rect,
    /// Last padding when glyphs were generated
    cached_padding_x: f32,
    cached_padding_y: f32,
    /// Last visible range when glyphs were generated
    cached_visible_lines: std::ops::Range<u32>,

    /// Width sizing constraint
    pub width_constraint: SizeConstraint,

    /// Height sizing constraint
    pub height_constraint: SizeConstraint,

    /// Text alignment (horizontal)
    pub text_align: TextAlign,

    /// Feature flags controlling which plugins/features are active
    pub capabilities: TextViewCapabilities,
}

impl TextView {
    /// Create a new TextView with document and viewport
    pub fn new(doc: Doc, viewport: Viewport) -> Self {
        Self::with_capabilities(doc, viewport, TextViewCapabilities::selectable())
    }

    /// Create a new TextView with specific capabilities
    pub fn with_capabilities(
        doc: Doc,
        viewport: Viewport,
        capabilities: TextViewCapabilities,
    ) -> Self {
        Self {
            doc,
            renderer: TextRenderer::new(),
            viewport,
            visible: true,
            syntax_highlighter: None,
            padding_x: 0.0,
            padding_y: 0.0,
            cached_glyphs: Vec::new(),
            cached_glyph_version: AtomicU64::new(0),
            last_rendered_layout_version: AtomicU64::new(0),
            cached_scroll_y: 0.0,
            cached_scroll_x: 0.0,
            cached_bounds: Rect { x: LogicalPixels(0.0), y: LogicalPixels(0.0), width: LogicalPixels(0.0), height: LogicalPixels(0.0) },
            cached_padding_x: 0.0,
            cached_padding_y: 0.0,
            cached_visible_lines: 0..0,
            width_constraint: SizeConstraint::FillContainer,
            height_constraint: SizeConstraint::FillContainer,
            text_align: TextAlign::Left,
            capabilities,
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
        self.padding_x = padding;
        self.padding_y = padding;
        self
    }

    /// Set horizontal padding (left + right)
    pub fn with_padding_x(mut self, padding_x: f32) -> Self {
        self.padding_x = padding_x;
        self
    }

    /// Set vertical padding (top + bottom)
    pub fn with_padding_y(mut self, padding_y: f32) -> Self {
        self.padding_y = padding_y;
        self
    }

    /// Set syntax highlighter
    pub fn with_syntax_highlighter(mut self, highlighter: Arc<SyntaxHighlighter>) -> Self {
        self.syntax_highlighter = Some(highlighter);
        self
    }

    /// Set width constraint
    pub fn with_width(mut self, constraint: SizeConstraint) -> Self {
        self.width_constraint = constraint;
        self
    }

    /// Set height constraint
    pub fn with_height(mut self, constraint: SizeConstraint) -> Self {
        self.height_constraint = constraint;
        self
    }

    /// Set text alignment
    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.text_align = align;
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
        self.renderer
            .update_layout(&tree, font_system, &self.viewport, false);

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
    ///
    /// Caching: Returns cached glyphs if doc version unchanged
    pub fn collect_glyphs(&mut self, _font_system: &SharedFontSystem) -> Vec<GlyphInstance> {
        if !self.visible {
            return Vec::new();
        }

        // Check cache - use renderer's layout_version (includes style changes) not doc version
        let current_version = self.renderer.layout_version;
        let cached_version = self.cached_glyph_version.load(Ordering::Relaxed);
        let current_scroll_y = self.viewport.scroll.y.0;
        let current_scroll_x = self.viewport.scroll.x.0;
        let current_visible_lines = &self.renderer.visible_lines;

        // Fast path: if only position changed (not text or visible range), just offset cached glyphs
        // IMPORTANT: Only use fast path if visible range is unchanged (same lines visible)
        if current_version == cached_version
            && !self.cached_glyphs.is_empty()
            && *current_visible_lines == self.cached_visible_lines {

            let scroll_delta_x = current_scroll_x - self.cached_scroll_x;
            let scroll_delta_y = current_scroll_y - self.cached_scroll_y;
            let bounds_delta_x = self.viewport.bounds.x.0 - self.cached_bounds.x.0;
            let bounds_delta_y = self.viewport.bounds.y.0 - self.cached_bounds.y.0;
            let padding_delta_x = self.padding_x - self.cached_padding_x;
            let padding_delta_y = self.padding_y - self.cached_padding_y;

            // Total position delta in logical pixels
            let delta_x = bounds_delta_x + padding_delta_x - scroll_delta_x;
            let delta_y = bounds_delta_y + padding_delta_y - scroll_delta_y;

            // If position changed, adjust cached glyphs (much cheaper than full rebuild)
            if delta_x.abs() > 0.01 || delta_y.abs() > 0.01 {
                let physical_delta_x = delta_x * self.viewport.scale_factor;
                let physical_delta_y = delta_y * self.viewport.scale_factor;

                let mut adjusted = self.cached_glyphs.clone();
                for glyph in &mut adjusted {
                    glyph.pos.x.0 += physical_delta_x;
                    glyph.pos.y.0 += physical_delta_y;
                }

                // Update cache state
                self.cached_glyphs = adjusted.clone();
                self.cached_scroll_x = current_scroll_x;
                self.cached_scroll_y = current_scroll_y;
                self.cached_bounds = self.viewport.bounds;
                self.cached_padding_x = self.padding_x;
                self.cached_padding_y = self.padding_y;

                return adjusted;
            } else {
                // No change at all - return cached as-is
                return self.cached_glyphs.clone();
            }
        }

        let visible_glyphs = self.renderer.get_visible_glyphs_with_style();
        let mut instances = Vec::with_capacity(visible_glyphs.len());

        let line_height = self.viewport.metrics.line_height;
        let viewport_top = self.viewport.bounds.y.0;
        let viewport_bottom = self.viewport.bounds.y.0 + self.viewport.bounds.height.0;

        // Add generous margin to culling bounds to prevent edge-case culling issues
        // This ensures glyphs at boundaries, with descenders, or in small viewports aren't incorrectly culled
        let cull_margin = line_height; // One full line height of margin on each side

        // Calculate horizontal offset based on text alignment
        let align_offset = match self.text_align {
            TextAlign::Left => 0.0,
            TextAlign::Center => {
                let content_width = self.content_width();
                let available_width = self.viewport.bounds.width.0 - (self.padding_x * 2.0);
                ((available_width - content_width) / 2.0).max(0.0)
            }
            TextAlign::Right => {
                let content_width = self.content_width();
                let available_width = self.viewport.bounds.width.0 - (self.padding_x * 2.0);
                (available_width - content_width).max(0.0)
            }
        };

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

            // 3. Apply bounds offset + padding + alignment offset to get screen position
            let screen_x = self.viewport.bounds.x.0 + self.padding_x + align_offset + local_x;
            let screen_y = self.viewport.bounds.y.0 + self.padding_y + local_y;

            // 4. Conservative vertical culling with margin (GPU scissor handles final clipping)
            // Only cull glyphs that are completely outside viewport with generous margin
            // This prevents edge cases where glyphs at boundaries get incorrectly culled
            let line_top = screen_y;
            let line_bottom = screen_y + line_height;
            if line_bottom < viewport_top - cull_margin || line_top > viewport_bottom + cull_margin
            {
                continue;
            }

            // No horizontal culling - GPU scissor rect handles all clipping
            // This prevents glyphs from being culled too aggressively

            // 5. Convert to physical coordinates for crisp text rendering
            let physical_x = screen_x * self.viewport.scale_factor;
            let physical_y = screen_y * self.viewport.scale_factor;

            // Build format flags from glyph attributes
            let mut format = 0u8;
            if glyph.underline {
                format |= 0x02; // Underline bit
            }
            if glyph.strikethrough {
                format |= 0x08; // Strikethrough bit (new)
            }

            instances.push(GlyphInstance {
                pos: LayoutPos::new(physical_x, physical_y),
                tex_coords: glyph.tex_coords,
                relative_pos: glyph.relative_pos,
                shader_id: 0,
                token_id: glyph.token_id as u8,
                format,
                atlas_index: glyph.atlas_index,
                _padding: 0,
            });
        }

        // Update cache
        self.cached_glyphs = instances.clone();
        self.cached_glyph_version.store(current_version, Ordering::Relaxed);
        self.cached_scroll_y = current_scroll_y;
        self.cached_scroll_x = current_scroll_x;
        self.cached_bounds = self.viewport.bounds;
        self.cached_padding_x = self.padding_x;
        self.cached_padding_y = self.padding_y;
        self.cached_visible_lines = self.renderer.visible_lines.clone();

        instances
    }

    /// Collect background rectangles (selection highlights, current line, etc.)
    pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        let rects = Vec::new();

        if !self.visible {
            return rects;
        }

        // Selection/cursor rendering is handled by plugins (cursor, selection)
        // Capabilities control whether those plugins are active

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

    /// Check if rendering is needed (layout changed since last render)
    /// Call this once per frame, it will return true only once per layout change
    pub fn needs_render(&self) -> bool {
        let current = self.renderer.layout_version;
        let last = self.last_rendered_layout_version.load(Ordering::Relaxed);
        if current != last {
            self.last_rendered_layout_version.store(current, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Get content width in logical pixels (max line width)
    pub fn content_width(&self) -> f32 {
        // Calculate max width by looking at ALL glyphs in layout_cache
        let mut max_x = 0.0f32;
        let font_size = self.viewport.metrics.font_size;

        for glyph in &self.renderer.layout_cache {
            // Skip newlines
            if glyph.char == '\n' {
                continue;
            }
            // Glyph position is the LEFT edge, add font_size as approximate char width
            // This is a safe overestimate for monospace fonts
            let glyph_right = glyph.layout_pos.x.0 + font_size;
            max_x = max_x.max(glyph_right);
        }

        max_x
    }

    /// Calculate intrinsic width (content width + horizontal padding)
    pub fn intrinsic_width(&self) -> f32 {
        self.content_width() + (self.padding_x * 2.0)
    }

    /// Calculate intrinsic height (content height + vertical padding)
    pub fn intrinsic_height(&self) -> f32 {
        self.content_height() + (self.padding_y * 2.0)
    }

    /// Set capabilities (controls which plugins/features are active)
    pub fn set_capabilities(&mut self, capabilities: TextViewCapabilities) {
        self.capabilities = capabilities;
    }

    /// Set visibility
    pub fn set_focused(&mut self, _focused: bool) {
        // Focus is tracked by EditableTextView - this is just for compatibility
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
        // Default to selectable (can select and copy, but not edit)
        Self::with_capabilities(Doc::new(), viewport, TextViewCapabilities::selectable())
    }
}
