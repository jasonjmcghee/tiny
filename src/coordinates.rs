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
        LineMode::NoWrap { horizontal_scroll: 0.0 }
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

// === Document Space ===

/// Position in document (text/editing operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct DocPos {
    /// Byte offset in the document
    pub byte_offset: usize,
    /// Line number (0-indexed)
    pub line: u32,
    /// Visual column (0-indexed, accounts for tabs)
    pub column: u32,
}

// === Logical Pixels (used by Layout and View spaces) ===

/// Logical pixels - DPI-independent unit
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct LogicalPixels(pub f32);

impl LogicalPixels {
    pub fn to_physical(self, scale_factor: f32) -> PhysicalPixels {
        PhysicalPixels(self.0 * scale_factor)
    }
}

impl std::ops::Add for LogicalPixels {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        LogicalPixels(self.0 + rhs.0)
    }
}

impl std::ops::Add<f32> for LogicalPixels {
    type Output = Self;
    fn add(self, rhs: f32) -> Self {
        LogicalPixels(self.0 + rhs)
    }
}

impl std::ops::Sub for LogicalPixels {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        LogicalPixels(self.0 - rhs.0)
    }
}

impl std::ops::Sub<f32> for LogicalPixels {
    type Output = Self;
    fn sub(self, rhs: f32) -> Self {
        LogicalPixels(self.0 - rhs)
    }
}

impl std::ops::Mul<f32> for LogicalPixels {
    type Output = Self;
    fn mul(self, rhs: f32) -> Self {
        LogicalPixels(self.0 * rhs)
    }
}

impl std::ops::Div<f32> for LogicalPixels {
    type Output = f32;
    fn div(self, rhs: f32) -> f32 {
        self.0 / rhs
    }
}

impl std::fmt::Display for LogicalPixels {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Logical size in DPI-independent pixels
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl LogicalSize {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }
}

// === Layout Space (pre-scroll) ===

/// Position in layout space - where things are before scrolling
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutPos {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
}

impl LayoutPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
        }
    }
}

/// Size in layout space
pub type LayoutSize = LogicalSize;

/// Rectangle in layout space
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutRect {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl LayoutRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }

    pub fn contains(&self, pt: LayoutPos) -> bool {
        pt.x.0 >= self.x.0
            && pt.x.0 <= self.x.0 + self.width.0
            && pt.y.0 >= self.y.0
            && pt.y.0 <= self.y.0 + self.height.0
    }
}

// === View Space (post-scroll) ===

/// Position in view space - layout minus scroll offset
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ViewPos {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
}

impl ViewPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
        }
    }
}

/// Size in view space (same as layout size)
pub type ViewSize = LogicalSize;

/// Rectangle in view space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    pub x: LogicalPixels,
    pub y: LogicalPixels,
    pub width: LogicalPixels,
    pub height: LogicalPixels,
}

impl ViewRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(height),
        }
    }
}

// === Physical Space (device pixels) ===

/// Physical pixels - actual device pixels
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalPixels(pub f32);

impl PhysicalPixels {
    pub fn to_logical(self, scale_factor: f32) -> LogicalPixels {
        LogicalPixels(self.0 / scale_factor)
    }
}

/// Position in physical pixels
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalPos {
    pub x: PhysicalPixels,
    pub y: PhysicalPixels,
}

impl PhysicalPos {
    pub fn new(x: f32, y: f32) -> Self {
        Self {
            x: PhysicalPixels(x),
            y: PhysicalPixels(y),
        }
    }
}

/// Size in physical pixels (keeping u32 for GPU compatibility)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

/// Size in physical pixels (float version for calculations)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalSizeF {
    pub width: PhysicalPixels,
    pub height: PhysicalPixels,
}

impl PhysicalSizeF {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: PhysicalPixels(width),
            height: PhysicalPixels(height),
        }
    }
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
}

impl TextMetrics {
    pub fn new(font_size: f32) -> Self {
        Self {
            font_size,
            line_height: font_size * 1.4,  // Standard line height multiplier
            space_width: font_size * 0.6,  // Will be updated when font system is set
            tab_stops: 4,
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
#[derive(Clone)]
pub struct Viewport {
    // === Scroll state ===
    /// Current scroll position in layout space
    pub scroll: LayoutPos,

    // === Window dimensions ===
    /// Logical size (DPI-independent)
    pub logical_size: LogicalSize,
    /// Physical size (device pixels)
    pub physical_size: PhysicalSize,
    /// HiDPI scale factor
    pub scale_factor: f32,

    // === Text metrics ===
    pub metrics: TextMetrics,

    // === Document margin ===
    /// Margin for document content (left, top)
    pub margin: LayoutPos,

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
    font_system: Option<Arc<crate::font::SharedFontSystem>>,
}

impl Viewport {
    /// Create new viewport with metrics
    pub fn new(logical_width: f32, logical_height: f32, scale_factor: f32) -> Self {
        let physical_size = PhysicalSize {
            width: (logical_width * scale_factor) as u32,
            height: (logical_height * scale_factor) as u32,
        };

        Self {
            scroll: LayoutPos::new(0.0, 0.0),  // Start at origin
            logical_size: LogicalSize::new(logical_width, logical_height),
            physical_size,
            scale_factor,
            metrics: TextMetrics::new(13.0), // Default 14pt font
            margin: LayoutPos::new(4.0, 4.0), // 4px margin left and top
            line_mode: LineMode::default(),  // Default to no wrap
            cached_doc_bounds: None,
            cached_bounds_version: 0,
            cached_longest_line_chars: 0,
            font_system: None,
        }
    }

    /// Set font system for accurate text measurement
    pub fn set_font_system(&mut self, font_system: Arc<crate::font::SharedFontSystem>) {
        // Cache the actual metrics from the font system once
        let line_layout = font_system.layout_text_scaled("A\nB", self.metrics.font_size, self.scale_factor);
        if line_layout.glyphs.len() >= 2 {
            self.metrics.line_height = (line_layout.glyphs[1].pos.y.0 - line_layout.glyphs[0].pos.y.0) / self.scale_factor;
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

    /// Document position to layout position
    pub fn doc_to_layout(&self, pos: DocPos) -> LayoutPos {
        // Use cached metrics (updated from font system if available)
        // Add margin to position
        LayoutPos::new(
            self.margin.x.0 + self.metrics.column_to_x(pos.column),
            self.margin.y.0 + pos.line as f32 * self.metrics.line_height,
        )
    }

    /// Document position to layout with actual text (more accurate)
    pub fn doc_to_layout_with_text(&self, pos: DocPos, line_text: &str) -> LayoutPos {
        let x = if let Some(font_system) = &self.font_system {
            // Convert column to byte position in line
            let byte_in_line = self.column_to_byte_in_line(line_text, pos.column);
            if byte_in_line > 0 && byte_in_line <= line_text.len() {
                // Measure the actual text up to the byte position
                let prefix = &line_text[..byte_in_line];
                let layout = font_system.layout_text_scaled(prefix, self.metrics.font_size, self.scale_factor);
                // Convert from physical pixels back to logical
                layout.width / self.scale_factor
            } else {
                0.0
            }
        } else {
            // Fallback to column-based positioning
            self.metrics.column_to_x(pos.column)
        };

        // Add margin to the position
        LayoutPos::new(
            self.margin.x.0 + x,
            self.margin.y.0 + pos.line as f32 * self.metrics.line_height,
        )
    }

    /// Layout position to view position (apply scroll)
    pub fn layout_to_view(&self, pos: LayoutPos) -> ViewPos {
        ViewPos::new(
            pos.x.0 - self.scroll.x.0,
            pos.y.0 - self.scroll.y.0,
        )
    }

    /// View position to physical position (apply scale factor)
    pub fn view_to_physical(&self, pos: ViewPos) -> PhysicalPos {
        PhysicalPos::new(
            pos.x.0 * self.scale_factor,
            pos.y.0 * self.scale_factor,
        )
    }

    /// Combined: Document to view position
    pub fn doc_to_view(&self, pos: DocPos) -> ViewPos {
        self.layout_to_view(self.doc_to_layout(pos))
    }

    /// Combined: Document to physical position
    pub fn doc_to_physical(&self, pos: DocPos) -> PhysicalPos {
        self.view_to_physical(self.doc_to_view(pos))
    }

    /// Combined: Layout to physical position
    pub fn layout_to_physical(&self, pos: LayoutPos) -> PhysicalPos {
        self.view_to_physical(self.layout_to_view(pos))
    }

    // === Reverse Transformations (Physical → View → Layout → Doc) ===

    /// Physical position to view position
    pub fn physical_to_view(&self, pos: PhysicalPos) -> ViewPos {
        ViewPos::new(
            pos.x.0 / self.scale_factor,
            pos.y.0 / self.scale_factor,
        )
    }

    /// View position to layout position (unapply scroll)
    pub fn view_to_layout(&self, pos: ViewPos) -> LayoutPos {
        LayoutPos::new(
            pos.x.0 + self.scroll.x.0,
            pos.y.0 + self.scroll.y.0,
        )
    }

    /// Layout position to document position (approximate)
    pub fn layout_to_doc(&self, pos: LayoutPos) -> DocPos {
        // Subtract margin to get document position
        let doc_x = (pos.x.0 - self.margin.x.0).max(0.0);
        let doc_y = (pos.y.0 - self.margin.y.0).max(0.0);

        let line = (doc_y / self.metrics.line_height) as u32;
        let column = (doc_x / self.metrics.space_width) as u32;

        DocPos {
            byte_offset: 0, // Would need document access for accurate byte offset
            line,
            column,
        }
    }

    /// Layout position to document position using font system's binary search hit testing
    pub fn layout_to_doc_with_tree(&self, pos: LayoutPos, tree: &crate::tree::Tree) -> DocPos {
        // Subtract margin to get document position
        let doc_x = (pos.x.0 - self.margin.x.0).max(0.0);
        let doc_y = (pos.y.0 - self.margin.y.0).max(0.0);

        let line = (doc_y / self.metrics.line_height) as u32;

        let column = if let Some(font_system) = &self.font_system {
            // Get the line text and use font system's accurate hit testing
            if let Some(line_start) = tree.line_to_byte(line) {
                let line_end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
                let line_text = tree.get_text_slice(line_start..line_end);

                font_system.hit_test_line(&line_text, self.metrics.font_size, self.scale_factor, doc_x)
            } else {
                0
            }
        } else {
            // Fallback to space-width estimation
            (doc_x / self.metrics.space_width) as u32
        };

        DocPos {
            byte_offset: 0, // Could be calculated by tree.doc_pos_to_byte if needed
            line,
            column,
        }
    }

    /// Combined: Physical to layout position
    pub fn physical_to_layout(&self, pos: PhysicalPos) -> LayoutPos {
        self.view_to_layout(self.physical_to_view(pos))
    }

    /// Combined: Physical to document position
    pub fn physical_to_doc(&self, pos: PhysicalPos) -> DocPos {
        self.layout_to_doc(self.physical_to_layout(pos))
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

        // Add much more generous margins for smooth scrolling
        let margin = self.metrics.line_height * 10.0;  // 10 lines of margin

        let is_visible = view_rect.x.0 < self.logical_size.width.0 + margin
            && view_rect.x.0 + view_rect.width.0 > -margin
            && view_rect.y.0 < self.logical_size.height.0 + margin
            && view_rect.y.0 + view_rect.height.0 > -margin;

        // Debug ALL visibility calculations to find the bug
        println!("VISIBILITY CHECK: layout=({:.1},{:.1} {}x{}), view=({:.1},{:.1} {}x{}), viewport={}x{}, scroll=({:.1},{:.1}), margin={:.1}, result={}",
            rect.x.0, rect.y.0, rect.width.0, rect.height.0,
            view_rect.x.0, view_rect.y.0, view_rect.width.0, view_rect.height.0,
            self.logical_size.width.0, self.logical_size.height.0,
            self.scroll.x.0, self.scroll.y.0, margin, is_visible);

        is_visible
    }

    // === Scrolling ===

    /// Scroll to make a layout position visible (Neovim-style with scrolloff)
    pub fn ensure_visible(&mut self, pos: LayoutPos) {
        let old_scroll_x = self.scroll.x.0;
        let old_scroll_y = self.scroll.y.0;

        // Vertical scrolling with 4-line scrolloff margin (like Neovim)
        let v_scrolloff_lines = 4.0;
        let v_scrolloff = v_scrolloff_lines * self.metrics.line_height;

        // Top margin check - if cursor goes above scrolloff area, scroll up one line
        let top_margin = self.scroll.y.0 + v_scrolloff;
        if pos.y.0 < top_margin {
            // Scroll up by one line at a time
            self.scroll.y.0 = (pos.y.0 - v_scrolloff).max(0.0);
        }

        // Bottom margin check - if cursor goes below scrolloff area, scroll down one line
        let bottom_margin = self.scroll.y.0 + self.logical_size.height.0 - v_scrolloff - self.metrics.line_height;
        if pos.y.0 > bottom_margin {
            // Scroll down by one line at a time
            self.scroll.y.0 = pos.y.0 - self.logical_size.height.0 + v_scrolloff + self.metrics.line_height;
        }

        // Horizontal scrolling with 8-character scrolloff margin (like Neovim)
        let h_scrolloff_chars = 8.0;
        let h_scrolloff = h_scrolloff_chars * self.metrics.space_width;

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

        if old_scroll_x != self.scroll.x.0 || old_scroll_y != self.scroll.y.0 {
            println!("ENSURE_VISIBLE: cursor at ({:.1}, {:.1}), scroll changed from ({:.1}, {:.1}) to ({:.1}, {:.1})",
                     pos.x.0, pos.y.0, old_scroll_x, old_scroll_y, self.scroll.x.0, self.scroll.y.0);
        }
    }

    /// Get visible line range
    pub fn visible_lines(&self) -> std::ops::Range<u32> {
        let first_line = (self.scroll.y / self.metrics.line_height) as u32;
        let last_line = ((self.scroll.y + self.logical_size.height) / self.metrics.line_height) as u32 + 1;

        // Debug output to see what we're calculating
        static mut DEBUG_COUNT: u32 = 0;
        unsafe {
            if DEBUG_COUNT < 3 {
                println!("VISIBLE_LINES: scroll_y={:.1}, window_height={:.1}, line_height={:.1} -> lines {}..{}",
                         self.scroll.y.0, self.logical_size.height.0, self.metrics.line_height,
                         first_line, last_line);
                DEBUG_COUNT += 1;
            }
        }

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
    pub fn visible_byte_range_with_tree(&self, tree: &crate::tree::Tree) -> std::ops::Range<usize> {
        let total_lines = tree.line_count();
        let lines = self.visible_lines_with_margin(2); // 2 lines margin

        // Clamp to valid line ranges
        let start_line = lines.start.min(total_lines.saturating_sub(1));
        let end_line = lines.end.min(total_lines + 5); // Allow 5 lines past end

        let start_byte = tree.line_to_byte(start_line).unwrap_or(0);
        let end_byte = tree.line_to_byte(end_line).unwrap_or(tree.byte_count());

        println!("DEBUG: visible range calculation - scroll_y={:.1}, lines={}..{}, bytes={}..{}, total_lines={}",
                 self.scroll.y.0, start_line, end_line, start_byte, end_byte, total_lines);

        // Ensure we always have SOME content to render
        if start_byte >= end_byte {
            println!("WARNING: Invalid byte range, falling back to entire document");
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
    pub fn get_document_bounds(&mut self, tree: &crate::tree::Tree) -> (f32, f32) {
        // First, find the line with the most characters (just counting)
        let mut longest_line_chars = 0;
        let mut longest_line_text = "".to_string();
        let mut longest_line_num = 0;

        for line_num in 0..tree.line_count() {
            if let Some(line_start) = tree.line_to_byte(line_num) {
                let line_end = tree.line_to_byte(line_num + 1).unwrap_or(tree.byte_count());
                let line_text_trimmed = tree.get_text_slice(line_start..line_end);
                let line_length = line_text_trimmed.chars().count();

                if line_length > longest_line_chars {
                    longest_line_chars = line_length;
                    longest_line_text = line_text_trimmed;
                    longest_line_num = line_num;
                }
            }
        }

        // Check if we can use cached bounds
        if let Some(bounds) = self.cached_doc_bounds {
            if self.cached_bounds_version == tree.version &&
               self.cached_longest_line_chars == longest_line_chars {
                return bounds;
            }
        }

        // Need to recalculate - the longest line changed or tree version changed
        let total_lines = tree.line_count() as f32;
        let doc_height = (total_lines + 5.0) * self.metrics.line_height;

        // Now measure ONLY the longest line
        let max_line_width = if let Some(font_system) = &self.font_system {
            // Measure the actual longest line
            let layout = font_system.layout_text_scaled(&longest_line_text, self.metrics.font_size, self.scale_factor);
            layout.width / self.scale_factor
        } else {
            // Fallback to estimation
            longest_line_chars as f32 * self.metrics.space_width
        };

        // Add 5 characters worth of padding
        let padding = 5.0 * self.metrics.space_width;
        let doc_width = max_line_width + padding;

        // Debug output for very long lines
        if longest_line_chars > 1000 {
            println!("Document bounds: line {} has {} chars, measured width={:.1}, doc_width={:.1} (space_width={:.1})",
                     longest_line_num, longest_line_chars, max_line_width, doc_width, self.metrics.space_width);
        }

        // Cache the result with the character count
        let bounds = (doc_width, doc_height);
        self.cached_doc_bounds = Some(bounds);
        self.cached_bounds_version = tree.version;
        self.cached_longest_line_chars = longest_line_chars;

        bounds
    }

    /// Clamp scroll position to document bounds
    pub fn clamp_scroll_to_bounds(&mut self, tree: &crate::tree::Tree) {
        // Invalidate cache if it might be stale (temporary fix)
        self.cached_doc_bounds = None;

        let (doc_width, doc_height) = self.get_document_bounds(tree);

        // For horizontal scrolling, the maximum scroll should keep content visible
        // Maximum scroll = document width - viewport width + small padding
        // This ensures we can see the end of the line but can't scroll into empty space

        let viewport_width = self.logical_size.width.0;

        // At maximum scroll, we want the last part of the line visible
        // Maximum scroll should be: doc_width - viewport_width
        // This positions the document end at the right edge of the viewport
        let max_scroll_x = (doc_width - viewport_width).max(0.0);

        // For vertical, standard scrolling
        let max_scroll_y = (doc_height - self.logical_size.height.0).max(0.0);

        // Apply the clamping
        let old_scroll_x = self.scroll.x.0;
        self.scroll.x.0 = self.scroll.x.0.clamp(0.0, max_scroll_x);
        self.scroll.y.0 = self.scroll.y.0.clamp(0.0, max_scroll_y);

        if old_scroll_x != self.scroll.x.0 {
            println!("Clamped horizontal scroll from {:.1} to {:.1} (max: {:.1}, doc_width: {:.1}, viewport: {:.1})",
                     old_scroll_x, self.scroll.x.0, max_scroll_x, doc_width, viewport_width);
        }
    }

    // === Horizontal Virtualization ===

    /// Calculate what part of a line is actually visible
    pub fn visible_line_content(&self, line_text: &str, line_num: u32) -> VisibleLineContent {
        // Use the actual scroll.x value instead of the line_mode's stored value
        match self.line_mode {
            LineMode::NoWrap { horizontal_scroll: _ } => {
                // Use the viewport's actual horizontal scroll
                let horizontal_scroll = self.scroll.x.0;

                // Only extract columns if we have a font system and there's enough text to warrant it
                if let Some(font_system) = &self.font_system {
                    // Calculate visible viewport width in logical pixels
                    let viewport_width = self.logical_size.width.0 - self.margin.x.0 * 2.0;

                    // Add 50% buffer on each side to prevent popping
                    let buffer_width = viewport_width * 0.5;

                    // Calculate the actual line width in pixels to respect bounds
                    // Don't count newline characters
                    let line_text_trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');
                    let line_width_pixels = line_text_trimmed.chars().count() as f32 * self.metrics.space_width;

                    // Calculate culling boundaries with buffer, respecting document bounds
                    // Start: buffer normally but don't go negative
                    let cull_start_x = (horizontal_scroll - buffer_width).max(0.0);
                    // End: buffer OR line end, whichever comes first
                    let cull_end_x = (horizontal_scroll + viewport_width + buffer_width)
                        .min(line_width_pixels);

                    // Check if we've scrolled past the end of the line
                    if cull_start_x >= line_width_pixels {
                        // We've scrolled past the end - return empty
                        return VisibleLineContent::Columns {
                            text: String::new(),
                            start_col: line_text.chars().count(),
                            x_offset: 0.0,
                        };
                    }

                    // Debug output to understand the width calculation
                    if line_num == 0 && horizontal_scroll > 0.0 {
                        println!("BUFFER ZONE: scroll={:.1}, viewport={:.1}, buffer={:.1}, cull_range={:.1}..{:.1}, line_width={:.1}",
                                 horizontal_scroll, viewport_width, buffer_width, cull_start_x, cull_end_x, line_width_pixels);
                    }

                    // Find start and end columns based on culling boundaries (with buffer)
                    let start_col = if cull_start_x > 0.0 {
                        font_system.pixel_to_column(
                            cull_start_x,
                            line_text,
                            self.metrics.font_size,
                            self.scale_factor
                        )
                    } else {
                        0
                    };

                    // Find end column (including buffer zone)
                    let end_col = font_system.pixel_to_column(
                        cull_end_x,
                        line_text,
                        self.metrics.font_size,
                        self.scale_factor
                    );

                    // Extract the visible portion of text
                    let chars: Vec<char> = line_text.chars().collect();

                    // Handle edge cases to prevent teleportation
                    let visible_text = if start_col >= chars.len() {
                        // If we're scrolled past the end of the line, return empty
                        String::new()
                    } else {
                        // Clamp end_col to prevent out-of-bounds and ensure smooth scrolling
                        let actual_end = end_col.min(chars.len());
                        // Only extract if we have a valid range
                        if actual_end > start_col {
                            chars[start_col..actual_end].iter().collect()
                        } else {
                            String::new()
                        }
                    };

                    // Calculate x_offset: We need to position the culled text at its original
                    // position in the line so that when scroll is applied, it appears correctly.
                    // But we need to use the actual measured width, not an approximation.
                    let x_offset = if start_col > 0 {
                        cull_start_x
                    } else {
                        0.0
                    };

                    VisibleLineContent::Columns {
                        text: visible_text,
                        start_col,
                        x_offset,  // Position text at its cull boundary
                    }
                } else {
                    // No font system, return full line (fallback)
                    VisibleLineContent::Columns {
                        text: line_text.to_string(),
                        start_col: 0,
                        x_offset: 0.0,  // No offset - scroll is handled by view transformation
                    }
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

    // === Helpers ===

    fn column_to_byte_in_line(&self, line_text: &str, target_column: u32) -> usize {
        let mut column = 0;
        let mut byte_pos = 0;

        for ch in line_text.chars() {
            if column >= target_column {
                break;
            }
            if ch == '\t' {
                column = ((column / self.metrics.tab_stops) + 1) * self.metrics.tab_stops;
            } else {
                column += 1;
            }
            if column <= target_column {
                byte_pos += ch.len_utf8();
            }
        }
        byte_pos
    }
}

// === Convenience Implementations ===

impl ViewRect {
    pub fn contains(&self, pos: ViewPos) -> bool {
        pos.x >= self.x
            && pos.x <= self.x + self.width
            && pos.y >= self.y
            && pos.y <= self.y + self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinate_transformations() {
        let viewport = Viewport::new(800.0, 600.0, 2.0); // 2x scale (retina)

        // Doc → Layout → View → Physical
        let doc_pos = DocPos {
            byte_offset: 0,
            line: 5,
            column: 10,
        };

        let layout_pos = viewport.doc_to_layout(doc_pos);
        assert_eq!(layout_pos.x, LogicalPixels(10.0 * viewport.metrics.space_width));
        assert_eq!(layout_pos.y, LogicalPixels(5.0 * viewport.metrics.line_height));

        let view_pos = viewport.layout_to_view(layout_pos);
        assert_eq!(view_pos.x, layout_pos.x); // No scroll initially
        assert_eq!(view_pos.y, layout_pos.y);

        let physical_pos = viewport.view_to_physical(view_pos);
        assert_eq!(physical_pos.x, PhysicalPixels(view_pos.x.0 * 2.0)); // 2x scale
        assert_eq!(physical_pos.y, PhysicalPixels(view_pos.y.0 * 2.0));
    }

    #[test]
    fn test_scrolling() {
        let mut viewport = Viewport::new(800.0, 600.0, 1.0);
        viewport.scroll = LayoutPos { x: LogicalPixels(100.0), y: LogicalPixels(200.0) };

        let layout_pos = LayoutPos { x: LogicalPixels(150.0), y: LogicalPixels(250.0) };
        let view_pos = viewport.layout_to_view(layout_pos);

        assert_eq!(view_pos.x, LogicalPixels(50.0)); // 150 - 100 scroll
        assert_eq!(view_pos.y, LogicalPixels(50.0)); // 250 - 200 scroll
    }

    #[test]
    fn test_visibility_check() {
        let mut viewport = Viewport::new(800.0, 600.0, 1.0);
        viewport.scroll = LayoutPos { x: LogicalPixels(100.0), y: LogicalPixels(100.0) };

        // Visible rectangle
        let visible_rect = LayoutRect {
            x: LogicalPixels(150.0),
            y: LogicalPixels(150.0),
            width: LogicalPixels(100.0),
            height: LogicalPixels(100.0),
        };
        assert!(viewport.is_visible(visible_rect));

        // Off-screen rectangle
        let offscreen_rect = LayoutRect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(50.0),
            height: LogicalPixels(50.0),
        };
        assert!(!viewport.is_visible(offscreen_rect));
    }

    #[test]
    fn test_tab_handling() {
        let metrics = TextMetrics::new(14.0);

        // Tab should advance to next tab stop
        assert_eq!(metrics.byte_to_column("hello\tworld", 6), 8); // After tab
        assert_eq!(metrics.byte_to_column("\t\t", 0), 0); // Start
        assert_eq!(metrics.byte_to_column("\t\t", 1), 4); // After first tab
        assert_eq!(metrics.byte_to_column("\t\t", 2), 8); // After second tab
    }
}