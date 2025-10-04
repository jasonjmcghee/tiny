//! TextBufferView - Rendering + Scrolling + Sizing
//!
//! Adds visual presentation to TextBuffer:
//! - GPU buffer management (isolated per instance)
//! - Scrolling with automatic culling
//! - Flexible sizing modes
//! - Paintable trait for rendering

use crate::text_buffer::TextBuffer;
use crate::coordinates::Viewport;
use crate::scroll::Scrollable;
use tiny_core::tree::{Point, Rect};
use tiny_sdk::{LogicalPixels, GlyphInstance};
use tiny_font::{create_glyph_instances, SharedFontSystem};
use std::sync::atomic::{AtomicU64, Ordering};

/// Sizing mode for text buffer view
#[derive(Clone, Debug)]
pub enum SizeMode {
    /// Show exactly N lines (scrollable if content exceeds)
    Fixed { lines: usize },
    /// Fill to specified height (scrollable if content exceeds)
    Fill { height: LogicalPixels },
    /// Size to content (no scrolling)
    Auto,
}

/// Text buffer view with rendering and scrolling
pub struct TextBufferView {
    /// The underlying text buffer
    pub buffer: TextBuffer,

    /// GPU vertex buffer (isolated per instance)
    vertex_buffer: Option<wgpu::Buffer>,

    /// Cache hash to avoid redundant GPU writes
    last_vertex_hash: AtomicU64,

    /// Scroll position
    scroll_position: Point,

    /// Sizing mode
    size_mode: SizeMode,

    /// Computed bounds (updated during layout)
    bounds: Rect,

    /// Visibility flag
    pub visible: bool,

    /// Background color (optional)
    pub background_color: Option<u32>,

    /// Text color for unstyled text
    pub default_text_color: u32,

    /// Highlighted line (for current line highlight or selection in lists)
    pub highlighted_line: Option<usize>,
}

impl TextBufferView {
    /// Create a new text buffer view
    pub fn new(buffer: TextBuffer, size_mode: SizeMode) -> Self {
        Self {
            buffer,
            vertex_buffer: None,
            last_vertex_hash: AtomicU64::new(0),
            scroll_position: Point::default(),
            size_mode,
            bounds: Rect::default(),
            visible: true,
            background_color: None,
            default_text_color: 0xFFFFFFFF, // White
            highlighted_line: None,
        }
    }

    /// Set background color
    pub fn with_background(mut self, color: u32) -> Self {
        self.background_color = Some(color);
        self
    }

    /// Set default text color
    pub fn with_text_color(mut self, color: u32) -> Self {
        self.default_text_color = color;
        self
    }

    /// Set visibility
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Update layout and visible range
    pub fn update_layout(&mut self, font_system: &SharedFontSystem, viewport: &Viewport) {
        // Update text layout
        self.buffer.update_layout(font_system, viewport);

        // Calculate bounds based on size mode
        self.calculate_bounds(viewport);

        // Update visible range for culling
        self.buffer.update_visible_range(viewport);
    }

    /// Update layout with explicit bounds (for popups/widgets with custom positioning)
    ///
    /// This is used when bounds are already set (e.g., by a parent widget like FilterableDropdown)
    /// and we don't want to recalculate them. Creates a local viewport for proper visibility culling.
    pub fn update_layout_with_bounds(
        &mut self,
        font_system: &SharedFontSystem,
        viewport: &Viewport,
        bounds: Rect,
    ) {
        // Set bounds explicitly (don't auto-calculate from SizeMode)
        self.bounds = bounds;

        // Update text layout (shaping, line cache)
        self.buffer.update_layout(font_system, viewport);

        // Create local viewport for THIS text buffer's position/scroll
        let local_viewport = viewport.with_local_view(self.bounds, self.scroll_position);

        // Update visible range using local viewport (not global editor scroll)
        self.buffer.update_visible_range(&local_viewport);
    }

    /// Calculate bounds based on size mode and viewport
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        let line_height = viewport.metrics.line_height;
        let content_height = self.buffer.content_height(line_height);

        let (width, height) = match &self.size_mode {
            SizeMode::Fixed { lines } => {
                let h = (*lines as f32) * line_height;
                (viewport.logical_size.width.0 * 0.9, h)
            }
            SizeMode::Fill { height } => {
                (viewport.logical_size.width.0 * 0.9, height.0.min(content_height))
            }
            SizeMode::Auto => {
                (viewport.logical_size.width.0 * 0.9, content_height)
            }
        };

        // Center horizontally
        let x = (viewport.logical_size.width.0 - width) / 2.0;
        let y = viewport.margin.y.0;

        self.bounds = Rect {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        };
    }

    /// Set bounds explicitly (for custom positioning)
    pub fn set_bounds(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    /// Get current bounds
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Collect glyphs for rendering
    pub fn collect_glyphs(&self, viewport: &Viewport, font_system: &SharedFontSystem) -> Vec<GlyphInstance> {
        if !self.visible {
            return Vec::new();
        }

        let visible_glyphs = self.buffer.visible_glyphs();
        let mut instances = Vec::with_capacity(visible_glyphs.len());

        for glyph in visible_glyphs {
            // Skip invisible glyphs (newlines, etc)
            if glyph.char == '\n' || glyph.tex_coords == [0.0, 0.0, 0.0, 0.0] {
                continue;
            }

            // Adjust position for scroll and bounds
            let x = self.bounds.x.0 + glyph.layout_pos.x.0 - self.scroll_position.x.0;
            let y = self.bounds.y.0 + glyph.layout_pos.y.0 - self.scroll_position.y.0;

            // Cull glyphs outside bounds
            if x < self.bounds.x.0 || x > self.bounds.x.0 + self.bounds.width.0 {
                continue;
            }
            if y < self.bounds.y.0 || y > self.bounds.y.0 + self.bounds.height.0 {
                continue;
            }

            // Convert to physical coordinates (text rendering uses physical pixels for crispness)
            let physical_x = x * viewport.scale_factor;
            let physical_y = y * viewport.scale_factor;

            instances.push(GlyphInstance {
                pos: tiny_sdk::LayoutPos::new(physical_x, physical_y),
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

    /// Get mutable reference to text buffer
    pub fn buffer_mut(&mut self) -> &mut TextBuffer {
        &mut self.buffer
    }

    /// Get reference to text buffer
    pub fn buffer(&self) -> &TextBuffer {
        &self.buffer
    }

    /// Collect background rectangles for rendering
    /// Returns rectangles for background and highlighted line
    pub fn collect_background_rects(&self, viewport: &Viewport) -> Vec<tiny_sdk::types::RectInstance> {
        use tiny_sdk::types::RectInstance;
        let mut rects = Vec::new();

        if !self.visible {
            return rects;
        }

        // Background for entire view
        if let Some(bg_color) = self.background_color {
            rects.push(RectInstance {
                rect: self.bounds,
                color: bg_color,
            });
        }

        // Highlighted line background
        if let Some(line_idx) = self.highlighted_line {
            if let Some(line_info) = self.buffer.layout.line_cache.get(line_idx) {
                let y = self.bounds.y.0 + line_info.y_position - self.scroll_position.y.0;

                // Only render if within visible bounds
                if y >= self.bounds.y.0 && y < self.bounds.y.0 + self.bounds.height.0 {
                    rects.push(RectInstance {
                        rect: tiny_core::tree::Rect {
                            x: self.bounds.x,
                            y: LogicalPixels(y),
                            width: self.bounds.width,
                            height: LogicalPixels(viewport.metrics.line_height),
                        },
                        color: 0xFF2A2A2A, // Subtle highlight (with full alpha)
                    });
                }
            }
        }

        rects
    }
}

impl Scrollable for TextBufferView {
    fn get_scroll(&self) -> Point {
        self.scroll_position
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.scroll_position = scroll;
    }

    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        if !self.visible {
            return false;
        }

        // Update scroll position
        self.scroll_position.y.0 -= delta.y.0;
        self.scroll_position.x.0 -= delta.x.0;

        // Get content bounds
        let content_bounds = self.get_content_bounds(viewport);
        let visible_height = widget_bounds.height.0;
        let visible_width = widget_bounds.width.0;

        // Clamp vertical scroll
        let max_scroll_y = (content_bounds.height.0 - visible_height).max(0.0);
        self.scroll_position.y.0 = self.scroll_position.y.0.max(0.0).min(max_scroll_y);

        // Clamp horizontal scroll
        let max_scroll_x = (content_bounds.width.0 - visible_width).max(0.0);
        self.scroll_position.x.0 = self.scroll_position.x.0.max(0.0).min(max_scroll_x);

        true
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        let line_height = viewport.metrics.line_height;
        let content_height = self.buffer.content_height(line_height);

        // Estimate content width (could be more precise by checking max line width)
        let content_width = viewport.logical_size.width.0;

        Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(content_width),
            height: LogicalPixels(content_height),
        }
    }
}

impl Default for TextBufferView {
    fn default() -> Self {
        Self::new(TextBuffer::new(), SizeMode::Auto)
    }
}
