//! Coordinate system transformation hub - THE single source of truth
//!
//! Four distinct coordinate spaces with explicit transformations:
//! 1. Document space: bytes, lines, columns (what editor manipulates)
//! 2. Layout space: logical pixels, pre-scroll (where widgets live)
//! 3. View space: logical pixels, post-scroll (what's visible)
//! 4. Physical space: device pixels (what GPU renders)
//!
//! IMPORTANT: Text rendering is special - it works directly in physical pixels
//! for crisp rendering, bypassing the normal logical->physical transformation.

use std::sync::Arc;

use tiny_core::DocTree;
use tiny_sdk::{
    DocPos, LayoutPos, LayoutRect, LogicalSize, PhysicalPos, PhysicalSize, ViewPos, ViewRect,
};

// === Scrolloff Configuration (Neovim-style) ===
/// Number of lines to keep visible above/below cursor when scrolling vertically
pub const VERTICAL_SCROLLOFF_LINES: f32 = 4.0;
/// Number of characters to keep visible left/right of cursor when scrolling horizontally
pub const HORIZONTAL_SCROLLOFF_CHARS: f32 = 8.0;
// === Line Rendering Mode ===

/// How lines should be rendered - with horizontal scroll or soft wrap
#[derive(Debug, Clone, Copy)]
pub enum LineMode {
    /// No wrapping - lines extend infinitely to the right with horizontal scroll
    NoWrap {
        /// Current horizontal scroll position in logical pixels
        horizontal_scroll: f32,
    },
    /// Soft wrap - lines wrap at viewport width, no horizontal scroll
    SoftWrap {
        /// Width at which to wrap lines in logical pixels
        wrap_width: f32,
    },
}

impl Default for LineMode {
    fn default() -> Self {
        LineMode::NoWrap {
            horizontal_scroll: 0.0,
        }
    }
}

/// Content that's actually visible for a line
#[derive(Debug, Clone)]
pub enum VisibleLineContent {
    /// Extracted columns from a long line (NoWrap mode)
    Columns {
        /// The visible text
        text: String,
        /// Starting column in the original line
        start_col: usize,
        /// X offset for rendering (negative for scrolled content)
        x_offset: f32,
    },
    /// Wrapped visual lines (SoftWrap mode)
    Wrapped {
        /// The visual lines after wrapping
        visual_lines: Vec<String>,
    },
}

// === Text Metrics (single source of truth) ===

/// All text measurement configuration in one place
#[derive(Clone)]
pub struct TextMetrics {
    /// Base font size in logical pixels
    pub font_size: f32,
    /// Line height in logical pixels
    pub line_height: f32,
    /// Average space width in logical pixels (at base font size)
    pub space_width: f32,
    /// Number of spaces per tab
    pub tab_stops: u32,
    /// Baseline offset from line top in logical pixels (from font ascent)
    pub baseline: f32,
}

impl TextMetrics {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_size,
            line_height: font_size * 1.4, // Standard line height multiplier
            space_width: font_size * 0.6, // Will be updated when font system is set
            tab_stops: 4,
            baseline: font_size * 0.8, // Default estimate, updated from font metrics
        }
    }

    pub fn with_line_height(line_height: f32) -> Self {
        let font_size = line_height / 1.4; // Derive font size from line height
        Self {
            font_size,
            line_height,
            space_width: font_size * 0.6,
            tab_stops: 4,
            baseline: font_size * 0.8, // Default estimate
        }
    }

    /// Get tab width in logical pixels
    pub fn tab_width(&self) -> f32 {
        self.space_width * self.tab_stops as f32
    }

    /// Calculate column position for a character position in a line
    pub fn byte_to_column(&self, line_text: &str, byte_in_line: usize) -> u32 {
        let mut column = 0;
        let mut byte_pos = 0;

        for ch in line_text.chars() {
            if byte_pos >= byte_in_line {
                break;
            }
            if ch == '\t' {
                // Tab advances to next tab stop
                column = ((column / self.tab_stops) + 1) * self.tab_stops;
            } else {
                column += 1;
            }
            byte_pos += ch.len_utf8();
        }
        column
    }

    /// Calculate x position for a column
    pub fn column_to_x(&self, column: u32) -> f32 {
        column as f32 * self.space_width
    }
}

// === THE Viewport - Central transformation hub ===

/// Manages all coordinate transformations
///
/// Each text view has its own Viewport with:
/// - `bounds`: WHERE to render (screen position + size)
/// - `scroll`: WHAT content to show (offset within document)
/// - `metrics`: HOW to measure text (font size, line height, etc.)
#[derive(Clone)]
pub struct Viewport {
    // === Rendering bounds ===
    /// WHERE to render: screen position and size
    /// All text is rendered relative to bounds.x, bounds.y
    pub bounds: LayoutRect,

    // === Scroll state ===
    /// WHAT to show: content offset within the document
    pub scroll: LayoutPos,

    // === Window dimensions (for root viewport only) ===
    /// Logical size (DPI-independent) - typically only used for root viewport
    pub logical_size: LogicalSize,
    /// Physical size (device pixels) - typically only used for root viewport
    pub physical_size: PhysicalSize,
    /// HiDPI scale factor
    pub scale_factor: f32,

    // === Text metrics ===
    pub metrics: TextMetrics,

    // === Line rendering mode ===
    /// How to render lines (with horizontal scroll or soft wrap)
    pub line_mode: LineMode,

    // === Cached document bounds ===
    /// Cached document bounds (width, height) to avoid recalculation
    cached_doc_bounds: Option<(f32, f32)>,
    /// Document version when bounds were last calculated
    cached_bounds_version: u64,
    /// Character count of the longest line when last measured
    cached_longest_line_chars: usize,

    // === Optional font system for accurate measurement ===
    font_system: Option<Arc<tiny_font::SharedFontSystem>>,
}

impl Viewport {
    /// Create from SDK ViewportInfo (for root viewport)
    pub fn from_viewport_info(info: &tiny_sdk::types::ViewportInfo) -> Self {
        // Root viewport fills entire window by default
        let bounds = LayoutRect::new(
            0.0,
            0.0,
            info.logical_size.width.0,
            info.logical_size.height.0,
        );

        Self {
            bounds,
            scroll: LayoutPos::new(info.scroll.x.0, info.scroll.y.0),
            logical_size: LogicalSize::new(info.logical_size.width.0, info.logical_size.height.0),
            physical_size: PhysicalSize {
                width: (info.logical_size.width.0 * info.scale_factor) as u32,
                height: (info.logical_size.height.0 * info.scale_factor) as u32,
            },
            scale_factor: info.scale_factor,
            metrics: TextMetrics::with_line_height(info.line_height),
            line_mode: LineMode::default(),
            cached_doc_bounds: None,
            cached_bounds_version: 0,
            cached_longest_line_chars: 0,
            font_system: None,
        }
    }

    /// Create new viewport with metrics (for root viewport)
    pub fn new(logical_width: f32, logical_height: f32, scale_factor: f32) -> Self {
        let physical_size = PhysicalSize {
            width: (logical_width * scale_factor) as u32,
            height: (logical_height * scale_factor) as u32,
        };

        // Default bounds fill entire window
        let bounds = LayoutRect::new(0.0, 0.0, logical_width, logical_height);

        Self {
            bounds,
            scroll: LayoutPos::new(0.0, 0.0), // Start at origin
            logical_size: LogicalSize::new(logical_width, logical_height),
            physical_size,
            scale_factor,
            metrics: TextMetrics::new(13.0), // Default 14pt font
            line_mode: LineMode::default(),  // Default to no wrap
            cached_doc_bounds: None,
            cached_bounds_version: 0,
            cached_longest_line_chars: 0,
            font_system: None,
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.metrics = TextMetrics::new(font_size);
    }

    /// Update metrics from a source of truth (single-direction data flow)
    pub fn update_metrics(&mut self, metrics: &TextMetrics) {
        self.metrics = metrics.clone();
    }

    /// Create a child viewport with custom bounds (for widgets/overlays)
    /// Inherits metrics from parent, but has independent bounds + scroll
    pub fn child(&self, bounds: LayoutRect) -> Self {
        Self {
            bounds,
            scroll: LayoutPos::new(0.0, 0.0), // Child starts unscrolled
            logical_size: self.logical_size,  // Inherit from parent
            physical_size: self.physical_size, // Inherit from parent
            scale_factor: self.scale_factor,
            metrics: self.metrics.clone(),
            line_mode: self.line_mode.clone(),
            cached_doc_bounds: None,
            cached_bounds_version: 0,
            cached_longest_line_chars: 0,
            font_system: self.font_system.clone(),
        }
    }

    /// Set font system for accurate text measurement
    pub fn set_font_system(&mut self, font_system: Arc<tiny_font::SharedFontSystem>) {
        // Cache the actual metrics from the font system once
        let line_layout =
            font_system.layout_text_scaled("A\nB", self.metrics.font_size, self.scale_factor);
        if line_layout.glyphs.len() >= 2 {
            self.metrics.line_height =
                (line_layout.glyphs[1].pos.y.0 - line_layout.glyphs[0].pos.y.0) / self.scale_factor;
        }

        // Approximated
        self.metrics.space_width = font_system.char_width_coef() * self.metrics.font_size;

        self.font_system = Some(font_system);
    }

    /// Update viewport on window resize
    pub fn resize(&mut self, logical_width: f32, logical_height: f32, scale_factor: f32) {
        self.logical_size = LogicalSize::new(logical_width, logical_height);
        self.scale_factor = scale_factor;
        self.physical_size = PhysicalSize {
            width: (logical_width * scale_factor) as u32,
            height: (logical_height * scale_factor) as u32,
        };
    }

    // === Forward Transformations (Doc → Layout → View → Physical) ===

    /// Document position to layout position (relative to document origin, no bounds offset)
    /// This outputs canonical positions starting at (0, 0).
    /// To get screen position, add viewport.bounds offset and subtract viewport.scroll.
    pub fn doc_to_layout(&self, pos: DocPos) -> LayoutPos {
        // Just convert doc coords to logical pixels - NO positioning
        LayoutPos::new(
            self.metrics.column_to_x(pos.column),
            pos.line as f32 * self.metrics.line_height,
        )
    }

    /// Document position to layout with tree access (gets line text automatically)
    /// Outputs canonical position starting at (0, 0) - no bounds offset
    pub fn doc_to_layout_with_tree(&self, pos: DocPos, tree: &tiny_core::tree::Tree) -> LayoutPos {
        let line_text = if let Some(line_start) = tree.line_to_byte(pos.line) {
            let line_end = tree.line_to_byte(pos.line + 1).unwrap_or(tree.byte_count());
            tree.get_text_slice(line_start..line_end)
        } else {
            String::new()
        };
        self.doc_to_layout_with_text(pos, &line_text)
    }

    /// Document position to layout with actual text (more accurate)
    /// Outputs canonical position starting at (0, 0) - no bounds offset
    pub fn doc_to_layout_with_text(&self, pos: DocPos, line_text: &str) -> LayoutPos {
        eprintln!("🐛 doc_to_layout_with_text: pos.column={}, line_text={:?}", pos.column, line_text);
        let x = if let Some(font_system) = &self.font_system {
            // Build the text up to the cursor position (pos.column is character index)
            let mut expanded = String::new();
            let mut char_index = 0u32;
            let mut visual_column = 0u32;

            for ch in line_text.chars() {
                if char_index >= pos.column {
                    break;
                }

                if ch == '\t' {
                    // Add spaces to reach next tab stop
                    let next_tab_stop =
                        ((visual_column / self.metrics.tab_stops) + 1) * self.metrics.tab_stops;
                    let spaces_to_add = next_tab_stop - visual_column;
                    for _ in 0..spaces_to_add {
                        expanded.push(' ');
                    }
                    visual_column = next_tab_stop;
                } else {
                    expanded.push(ch);
                    visual_column += 1;
                }
                char_index += 1; // Each character (including tab) increments char position by 1
            }

            // Now measure the expanded text
            if !expanded.is_empty() {
                let layout = font_system.layout_text_scaled(
                    &expanded,
                    self.metrics.font_size,
                    self.scale_factor,
                );
                layout.width / self.scale_factor
            } else {
                0.0
            }
        } else {
            // Fallback to column-based positioning
            self.metrics.column_to_x(pos.column)
        };

        // Just convert to logical pixels - NO positioning
        LayoutPos::new(x, pos.line as f32 * self.metrics.line_height)
    }

    /// Layout position to view position (apply scroll)
    pub fn layout_to_view(&self, pos: LayoutPos) -> ViewPos {
        ViewPos::new(pos.x.0 - self.scroll.x.0, pos.y.0 - self.scroll.y.0)
    }

    /// View position to physical position (apply scale factor)
    pub fn view_to_physical(&self, pos: ViewPos) -> PhysicalPos {
        PhysicalPos::new(pos.x.0 * self.scale_factor, pos.y.0 * self.scale_factor)
    }

    /// Document to view position
    pub fn doc_to_view(&self, pos: DocPos) -> ViewPos {
        self.layout_to_view(self.doc_to_layout(pos))
    }

    /// Document to physical position
    pub fn doc_to_physical(&self, pos: DocPos) -> PhysicalPos {
        self.view_to_physical(self.layout_to_view(self.doc_to_layout(pos)))
    }

    /// Layout to physical position
    pub fn layout_to_physical(&self, pos: LayoutPos) -> PhysicalPos {
        self.view_to_physical(self.layout_to_view(pos))
    }

    // === Complete Transformations (Doc → Screen, Screen → Doc) ===
    // These are the ONLY methods you should use for positioning text/cursor/selection
    // They encapsulate ALL coordinate transforms in one step

    /// Complete transform: Document → Screen coordinates
    /// Includes: doc → layout → view (scroll) → screen (bounds + padding)
    /// This is the SINGLE source of truth for positioning
    pub fn doc_to_screen(&self, pos: DocPos, padding_x: f32, padding_y: f32) -> LayoutPos {
        let layout = self.doc_to_layout(pos);
        let view_x = layout.x.0 - self.scroll.x.0;
        let view_y = layout.y.0 - self.scroll.y.0;
        let screen_x = self.bounds.x.0 + padding_x + view_x;
        let screen_y = self.bounds.y.0 + padding_y + view_y;
        LayoutPos::new(screen_x, screen_y)
    }

    /// Complete transform with accurate text measurement
    pub fn doc_to_screen_with_text(&self, pos: DocPos, line_text: &str, padding_x: f32, padding_y: f32) -> LayoutPos {
        let layout = self.doc_to_layout_with_text(pos, line_text);
        let view_x = layout.x.0 - self.scroll.x.0;
        let view_y = layout.y.0 - self.scroll.y.0;
        let screen_x = self.bounds.x.0 + padding_x + view_x;
        let screen_y = self.bounds.y.0 + padding_y + view_y;
        LayoutPos::new(screen_x, screen_y)
    }

    /// Complete reverse transform: Screen → Document coordinates
    pub fn screen_to_doc(&self, screen_pos: LayoutPos, padding_x: f32, padding_y: f32) -> DocPos {
        // Reverse: screen → view → layout → doc
        let view_x = screen_pos.x.0 - self.bounds.x.0 - padding_x;
        let view_y = screen_pos.y.0 - self.bounds.y.0 - padding_y;
        let layout_x = view_x + self.scroll.x.0;
        let layout_y = view_y + self.scroll.y.0;
        self.layout_to_doc(LayoutPos::new(layout_x, layout_y))
    }

    /// Complete reverse transform with tree for accurate positioning
    pub fn screen_to_doc_with_tree(&self, screen_pos: LayoutPos, tree: &DocTree, padding_x: f32, padding_y: f32) -> DocPos {
        let view_x = screen_pos.x.0 - self.bounds.x.0 - padding_x;
        let view_y = screen_pos.y.0 - self.bounds.y.0 - padding_y;
        let layout_x = view_x + self.scroll.x.0;
        let layout_y = view_y + self.scroll.y.0;
        self.layout_to_doc_with_tree(LayoutPos::new(layout_x, layout_y), tree)
    }

    // === Reverse Transformations (Physical → View → Layout → Doc) ===

    /// Physical position to view position
    pub fn physical_to_view(&self, pos: PhysicalPos) -> ViewPos {
        ViewPos::new(pos.x.0 / self.scale_factor, pos.y.0 / self.scale_factor)
    }

    /// View position to layout position (unapply scroll)
    pub fn view_to_layout(&self, pos: ViewPos) -> LayoutPos {
        LayoutPos::new(pos.x.0 + self.scroll.x.0, pos.y.0 + self.scroll.y.0)
    }

    /// Layout position to document position (approximate)
    /// Expects canonical layout position (0,0)-relative
    pub fn layout_to_doc(&self, pos: LayoutPos) -> DocPos {
        // Direct conversion from logical pixels to doc coords - NO margin subtraction
        let doc_x = pos.x.0.max(0.0);
        let doc_y = pos.y.0.max(0.0);

        let line = (doc_y / self.metrics.line_height) as u32;
        let column = (doc_x / self.metrics.space_width) as u32;

        DocPos {
            byte_offset: 0, // Would need document access for accurate byte offset
            line,
            column,
        }
    }

    /// Layout position to document position using font system's binary search hit testing
    /// Expects canonical layout position (0,0)-relative
    pub fn layout_to_doc_with_tree(&self, pos: LayoutPos, tree: &DocTree) -> DocPos {
        // Direct conversion - NO margin subtraction
        let doc_x = pos.x.0.max(0.0);
        let doc_y = pos.y.0.max(0.0);

        // Clamp line to valid document bounds
        let unclamped_line = (doc_y / self.metrics.line_height) as u32;
        let total_lines = tree.line_count();
        let line = if total_lines > 0 {
            unclamped_line.min(total_lines - 1)
        } else {
            0
        };

        eprintln!("🐛 layout_to_doc_with_tree: line={}, doc_x={}, has_font_system={}", line, doc_x, self.font_system.is_some());
        let column = if let Some(font_system) = &self.font_system {
            // Get the line text and use font system's accurate hit testing
            if let Some(line_start) = tree.line_to_byte(line) {
                let line_end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
                let line_text = tree.get_text_slice(line_start..line_end);

                // Strip trailing newline to avoid cursor positioning after it
                let line_text_trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');

                // Use shaped version for proper ligature/cluster handling
                let col = font_system.hit_test_line_shaped(
                    line_text_trimmed,
                    self.metrics.font_size,
                    self.scale_factor,
                    doc_x,
                );

                // Clamp column to line length
                let line_char_count = line_text_trimmed.chars().count() as u32;
                col.min(line_char_count)
            } else {
                0
            }
        } else {
            // Fallback to space-width estimation
            let col = (doc_x / self.metrics.space_width) as u32;
            // Clamp to line length
            if let Some(line_start) = tree.line_to_byte(line) {
                let line_end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
                let line_text = tree.get_text_slice(line_start..line_end);
                let line_text_trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');
                let line_char_count = line_text_trimmed.chars().count() as u32;
                col.min(line_char_count)
            } else {
                0
            }
        };

        DocPos {
            byte_offset: 0, // Could be calculated by tree.doc_pos_to_byte if needed
            line,
            column,
        }
    }

    /// Physical to layout position
    pub fn physical_to_layout(&self, pos: PhysicalPos) -> LayoutPos {
        self.view_to_layout(self.physical_to_view(pos))
    }

    /// Physical to document position
    pub fn physical_to_doc(&self, pos: PhysicalPos) -> DocPos {
        self.layout_to_doc(self.view_to_layout(self.physical_to_view(pos)))
    }

    // === Rectangle Transformations ===

    /// Transform layout rectangle to view rectangle
    pub fn layout_rect_to_view(&self, rect: LayoutRect) -> ViewRect {
        ViewRect::new(
            rect.x.0 - self.scroll.x.0,
            rect.y.0 - self.scroll.y.0,
            rect.width.0,
            rect.height.0,
        )
    }

    /// Check if layout rectangle is visible in view (with margins for smooth scrolling)
    pub fn is_visible(&self, rect: LayoutRect) -> bool {
        let view_rect = self.layout_rect_to_view(rect);

        // Add margins for smooth scrolling
        let margin = self.metrics.line_height * 2.0; // 2 lines of margin

        let is_visible = view_rect.x.0 < self.logical_size.width.0 + margin
            && view_rect.x.0 + view_rect.width.0 > -margin
            && view_rect.y.0 < self.logical_size.height.0 + margin
            && view_rect.y.0 + view_rect.height.0 > -margin;

        is_visible
    }

    // === Scrolling ===

    /// Scroll to make a layout position visible (Neovim-style with scrolloff)
    pub fn ensure_visible(&mut self, pos: LayoutPos) {
        // Vertical scrolling with configurable scrolloff margin
        let v_scrolloff = VERTICAL_SCROLLOFF_LINES * self.metrics.line_height;

        // Top margin check - if cursor goes above scrolloff area, scroll up one line
        let top_margin = self.scroll.y.0 + v_scrolloff;
        if pos.y.0 < top_margin {
            // Scroll up by one line at a time
            self.scroll.y.0 = (pos.y.0 - v_scrolloff).max(0.0);
        }

        // Bottom margin check - if cursor goes below scrolloff area, scroll down one line
        let bottom_margin =
            self.scroll.y.0 + self.logical_size.height.0 - v_scrolloff - self.metrics.line_height;
        if pos.y.0 > bottom_margin {
            // Scroll down by one line at a time
            self.scroll.y.0 =
                pos.y.0 - self.logical_size.height.0 + v_scrolloff + self.metrics.line_height;
        }

        // Horizontal scrolling with configurable scrolloff margin
        let h_scrolloff = HORIZONTAL_SCROLLOFF_CHARS * self.metrics.space_width;

        // Left margin check - if cursor goes before scrolloff area, scroll left one character
        let left_margin = self.scroll.x.0 + h_scrolloff;
        if pos.x.0 < left_margin {
            // Scroll left by one character at a time
            self.scroll.x.0 = (pos.x.0 - h_scrolloff).max(0.0);
        }

        // Right margin check - if cursor goes after scrolloff area, scroll right one character
        let right_margin = self.scroll.x.0 + self.logical_size.width.0 - h_scrolloff;
        if pos.x.0 > right_margin {
            // Scroll right by one character at a time
            self.scroll.x.0 = pos.x.0 - self.logical_size.width.0 + h_scrolloff;
        }
    }

    /// Center the viewport on a layout position
    pub fn center_on(&mut self, pos: LayoutPos) {
        // Center vertically
        self.scroll.y.0 = (pos.y.0 - self.logical_size.height.0 / 2.0).max(0.0);

        // Center horizontally with some left margin for readability
        let target_x = pos.x.0 - self.logical_size.width.0 * 0.3;
        self.scroll.x.0 = target_x.max(0.0);
    }

    /// Get visible line range
    pub fn visible_lines(&self) -> std::ops::Range<u32> {
        let first_line = (self.scroll.y / self.metrics.line_height) as u32;
        // Use bounds.height for the viewport height (works for both root and child viewports)
        let viewport_height = self.bounds.height.0;
        let last_line =
            ((self.scroll.y + viewport_height) / self.metrics.line_height) as u32 + 1;

        first_line..last_line
    }

    /// Get visible line range with margins for smooth scrolling
    pub fn visible_lines_with_margin(&self, margin_lines: u32) -> std::ops::Range<u32> {
        let lines = self.visible_lines();
        let first_line = lines.start.saturating_sub(margin_lines);
        let last_line = lines.end + margin_lines;
        first_line..last_line
    }

    /// Get visible byte range using tree navigation (to be called with tree reference)
    pub fn visible_byte_range_with_tree(&self, tree: &DocTree) -> std::ops::Range<usize> {
        let total_lines = tree.line_count();
        let lines = self.visible_lines_with_margin(2); // 2 lines margin

        // Clamp to valid line ranges
        let start_line = lines.start.min(total_lines.saturating_sub(1));
        let end_line = lines.end.min(total_lines + 5); // Allow 5 lines past end

        let start_byte = tree.line_to_byte(start_line).unwrap_or(0);
        let end_byte = tree.line_to_byte(end_line).unwrap_or(tree.byte_count());

        // Ensure we always have SOME content to render
        if start_byte >= end_byte {
            return 0..tree.byte_count();
        }

        start_byte..end_byte
    }

    /// Get visible layout rectangle (area that should be rendered)
    pub fn visible_layout_rect(&self) -> LayoutRect {
        LayoutRect {
            x: self.scroll.x,
            y: self.scroll.y,
            width: self.logical_size.width,
            height: self.logical_size.height,
        }
    }

    /// Get document bounds with caching
    pub fn get_document_bounds(&mut self, tree: &DocTree) -> (f32, f32) {
        // Check cache first - if version hasn't changed, use cached bounds
        if let Some(bounds) = self.cached_doc_bounds {
            if self.cached_bounds_version == tree.version {
                return bounds;
            }
        }

        // Find longest line by scanning all lines
        // Note: Tree::line_to_byte() now uses SIMD-cached boundaries, so this is fast
        let mut longest_line_chars = 0;

        for line_num in 0..tree.line_count() {
            let line_text = tree.line_text(line_num);
            let char_count = line_text.chars().count();
            if char_count > longest_line_chars {
                longest_line_chars = char_count;
            }
        }

        let total_lines = tree.line_count();
        let doc_height = (total_lines as f32 + 5.0) * self.metrics.line_height;

        // Estimate line width without measuring (measuring is expensive)
        let max_line_width = longest_line_chars as f32 * self.metrics.space_width;

        // Add 5 characters worth of padding
        let padding = 5.0 * self.metrics.space_width;
        let doc_width = max_line_width + padding;

        // Cache the result with the character count
        let bounds = (doc_width, doc_height);
        self.cached_doc_bounds = Some(bounds);
        self.cached_bounds_version = tree.version;
        self.cached_longest_line_chars = longest_line_chars;

        bounds
    }

    /// Clamp scroll position to document bounds
    pub fn clamp_scroll_to_bounds(&mut self, tree: &DocTree, bounds: LayoutRect) {
        // Invalidate cache if it might be stale (temporary fix)
        self.cached_doc_bounds = None;

        let (doc_width, doc_height) = self.get_document_bounds(tree);

        // For horizontal scrolling, the maximum scroll should keep content visible
        // Maximum scroll = document width - viewport width + small padding
        // This ensures we can see the end of the line but can't scroll into empty space

        // Account for the location of the bounds
        let viewport_width = self.logical_size.width.0 - bounds.x.0;
        let viewport_height = self.logical_size.height.0 - bounds.y.0;

        // At maximum scroll, we want the last part of the line visible
        // Maximum scroll should be: doc_width - viewport_width
        // This positions the document end at the right edge of the viewport
        let max_scroll_x = (doc_width - viewport_width).max(0.0);

        // For vertical, standard scrolling
        let max_scroll_y = (doc_height - viewport_height).max(0.0);

        // Apply the clamping
        self.scroll.x.0 = self.scroll.x.0.clamp(0.0, max_scroll_x);
        self.scroll.y.0 = self.scroll.y.0.clamp(0.0, max_scroll_y);
    }

    // === Horizontal Virtualization ===

    /// Convert to SDK ViewportInfo for plugins
    pub fn to_viewport_info(&self) -> tiny_sdk::ViewportInfo {
        tiny_sdk::ViewportInfo {
            scroll: self.scroll.clone(),
            logical_size: self.logical_size.clone(),
            physical_size: self.physical_size.clone(),
            scale_factor: self.scale_factor,
            line_height: self.metrics.line_height,
            font_size: self.metrics.font_size,
            // Margins removed - positioning now handled by viewport.bounds
            margin: LayoutPos::new(0.0, 0.0),
            global_margin: LayoutPos::new(0.0, 0.0),
        }
    }

    /// Calculate what part of a line is actually visible
    pub fn visible_line_content_semantic(
        &self,
        line_text: &str,
        line_num: u32,
        token_boundaries: Option<&[usize]>,
    ) -> VisibleLineContent {
        self.visible_line_content(line_text, line_num, token_boundaries)
    }

    /// Calculate what part of a line is actually visible (with optional token boundaries)
    pub fn visible_line_content(
        &self,
        line_text: &str,
        _line_num: u32,
        token_boundaries: Option<&[usize]>,
    ) -> VisibleLineContent {
        // Use the actual scroll.x value instead of the line_mode's stored value
        match self.line_mode {
            LineMode::NoWrap {
                horizontal_scroll: _,
            } => {
                // Use the viewport's actual horizontal scroll
                let horizontal_scroll = self.scroll.x.0;

                // Extract visible range
                let (start_byte, end_byte, x_offset) = if let Some(_font_system) = &self.font_system
                {
                    let (start, end, offset) = self.calculate_visible_range(
                        line_text,
                        horizontal_scroll,
                        token_boundaries,
                    );
                    (start, end, offset)
                } else {
                    (0, line_text.len(), 0.0)
                };

                // Extract visible text
                let visible_text = if start_byte < line_text.len() {
                    line_text[start_byte.min(line_text.len())..end_byte.min(line_text.len())]
                        .to_string()
                } else {
                    String::new()
                };

                // Calculate start column
                let start_col = line_text[..start_byte.min(line_text.len())].chars().count();

                VisibleLineContent::Columns {
                    text: visible_text,
                    start_col,
                    x_offset,
                }
            }
            LineMode::SoftWrap { wrap_width: _ } => {
                // For now, return as single visual line - will implement wrapping later
                // TODO: Implement proper line wrapping
                VisibleLineContent::Wrapped {
                    visual_lines: vec![line_text.to_string()],
                }
            }
        }
    }

    /// Calculate visible byte range for a line based on horizontal scroll
    fn calculate_visible_range(
        &self,
        line_text: &str,
        horizontal_scroll: f32,
        token_boundaries: Option<&[usize]>,
    ) -> (usize, usize, f32) {
        let font_system = match &self.font_system {
            Some(fs) => fs,
            None => return (0, line_text.len(), 0.0),
        };

        // Use bounds width (margins are now handled by bounds)
        let viewport_width = self.bounds.width.0;
        let buffer_width = viewport_width * 0.5;

        // Calculate visible pixel range
        let visible_start_x = (horizontal_scroll - buffer_width).max(0.0);
        let visible_end_x = horizontal_scroll + viewport_width + buffer_width;

        // Calculate line width
        let line_text_trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');
        let line_width_pixels = line_text_trimmed.chars().count() as f32 * self.metrics.space_width;

        // Check if scrolled past end
        if visible_start_x >= line_width_pixels {
            return (line_text.len(), line_text.len(), 0.0);
        }

        // If we have token boundaries, use them
        if let Some(boundaries) = token_boundaries {
            let mut start_byte = 0;
            let mut end_byte = line_text.len();
            let mut found_start = false;

            for &boundary in boundaries {
                if boundary > line_text.len() {
                    break;
                }

                // Measure position of this boundary
                let prefix = &line_text[..boundary];
                let layout = font_system.layout_text_scaled(
                    prefix,
                    self.metrics.font_size,
                    self.scale_factor,
                );
                let x_pos = layout.width / self.scale_factor;

                if !found_start && x_pos >= visible_start_x {
                    // Found first visible token boundary
                    start_byte = if boundary > 0 {
                        // Back up to previous boundary to include whole token
                        boundaries
                            .iter()
                            .rev()
                            .find(|&&b| b < boundary)
                            .copied()
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    found_start = true;
                }

                if x_pos > visible_end_x {
                    end_byte = boundary;
                    break;
                }
            }

            // Calculate x_offset
            let x_offset = if start_byte > 0 {
                let prefix = &line_text[..start_byte];
                let layout = font_system.layout_text_scaled(
                    prefix,
                    self.metrics.font_size,
                    self.scale_factor,
                );
                layout.width / self.scale_factor
            } else {
                0.0
            };

            (start_byte, end_byte, x_offset)
        } else {
            // Character-based culling
            let start_col = if visible_start_x > 0.0 {
                font_system.pixel_to_column(
                    visible_start_x,
                    line_text,
                    self.metrics.font_size,
                    self.scale_factor,
                )
            } else {
                0
            };

            let end_col = font_system.pixel_to_column(
                visible_end_x.min(line_width_pixels),
                line_text,
                self.metrics.font_size,
                self.scale_factor,
            );

            // Convert columns to byte positions
            let chars: Vec<char> = line_text.chars().collect();
            let start_byte = chars.iter().take(start_col).map(|c| c.len_utf8()).sum();
            let end_byte = chars.iter().take(end_col).map(|c| c.len_utf8()).sum();

            let x_offset = if start_col > 0 { visible_start_x } else { 0.0 };

            (start_byte, end_byte, x_offset)
        }
    }
}
