//! Coordinate system transformation hub - THE single source of truth
//!
//! Four distinct coordinate spaces with explicit transformations:
//! 1. Document space: bytes, lines, columns (what editor manipulates)
//! 2. Layout space: logical pixels, pre-scroll (where widgets live)
//! 3. View space: logical pixels, post-scroll (what's visible)
//! 4. Physical space: device pixels (what GPU renders)

use std::sync::Arc;

// === Document Space ===

/// Position in document (text/editing operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DocPos {
    /// Byte offset in the document
    pub byte_offset: usize,
    /// Line number (0-indexed)
    pub line: u32,
    /// Visual column (0-indexed, accounts for tabs)
    pub column: u32,
}

// === Layout Space (pre-scroll) ===

/// Position in layout space - where things are before scrolling
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LayoutPos {
    pub x: f32,
    pub y: f32,
}

/// Size in layout space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutSize {
    pub width: f32,
    pub height: f32,
}

/// Rectangle in layout space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

// === View Space (post-scroll) ===

/// Position in view space - layout minus scroll offset
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewPos {
    pub x: f32,
    pub y: f32,
}

/// Size in view space (same as layout size)
pub type ViewSize = LayoutSize;

/// Rectangle in view space
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

// === Physical Space (device pixels) ===

/// Position in physical pixels
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalPos {
    pub x: f32,
    pub y: f32,
}

/// Size in physical pixels
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
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
            space_width: font_size * 0.6,  // Approximate for monospace
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
    pub logical_size: LayoutSize,
    /// Physical size (device pixels)
    pub physical_size: PhysicalSize,
    /// HiDPI scale factor
    pub scale_factor: f32,

    // === Text metrics ===
    pub metrics: TextMetrics,

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
            scroll: LayoutPos { x: 0.0, y: 0.0 },
            logical_size: LayoutSize {
                width: logical_width,
                height: logical_height,
            },
            physical_size,
            scale_factor,
            metrics: TextMetrics::new(14.0), // Default 14pt font
            font_system: None,
        }
    }

    /// Set font system for accurate text measurement
    pub fn set_font_system(&mut self, font_system: Arc<crate::font::SharedFontSystem>) {
        self.font_system = Some(font_system.clone());

        // Update metrics based on actual font measurements
        let test_layout = font_system.layout_text_scaled(" ", self.metrics.font_size, self.scale_factor);
        if !test_layout.glyphs.is_empty() {
            self.metrics.space_width = test_layout.width;
        }
    }

    /// Update viewport on window resize
    pub fn resize(&mut self, logical_width: f32, logical_height: f32, scale_factor: f32) {
        self.logical_size = LayoutSize {
            width: logical_width,
            height: logical_height,
        };
        self.scale_factor = scale_factor;
        self.physical_size = PhysicalSize {
            width: (logical_width * scale_factor) as u32,
            height: (logical_height * scale_factor) as u32,
        };
    }

    // === Forward Transformations (Doc → Layout → View → Physical) ===

    /// Document position to layout position
    pub fn doc_to_layout(&self, pos: DocPos) -> LayoutPos {
        LayoutPos {
            x: self.metrics.column_to_x(pos.column),
            y: pos.line as f32 * self.metrics.line_height,
        }
    }

    /// Document position to layout with actual text (more accurate)
    pub fn doc_to_layout_with_text(&self, pos: DocPos, line_text: &str) -> LayoutPos {
        let x = if let Some(font_system) = &self.font_system {
            // Measure actual text up to column
            let byte_in_line = self.column_to_byte_in_line(line_text, pos.column);
            if byte_in_line > 0 {
                let prefix: String = line_text.chars().take(byte_in_line).collect();
                let layout = font_system.layout_text_scaled(&prefix, self.metrics.font_size, self.scale_factor);
                layout.width
            } else {
                0.0
            }
        } else {
            // Fallback to column-based positioning
            self.metrics.column_to_x(pos.column)
        };

        LayoutPos {
            x,
            y: pos.line as f32 * self.metrics.line_height,
        }
    }

    /// Layout position to view position (apply scroll)
    pub fn layout_to_view(&self, pos: LayoutPos) -> ViewPos {
        ViewPos {
            x: pos.x - self.scroll.x,
            y: pos.y - self.scroll.y,
        }
    }

    /// View position to physical position (apply scale factor)
    pub fn view_to_physical(&self, pos: ViewPos) -> PhysicalPos {
        PhysicalPos {
            x: pos.x * self.scale_factor,
            y: pos.y * self.scale_factor,
        }
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
        ViewPos {
            x: pos.x / self.scale_factor,
            y: pos.y / self.scale_factor,
        }
    }

    /// View position to layout position (unapply scroll)
    pub fn view_to_layout(&self, pos: ViewPos) -> LayoutPos {
        LayoutPos {
            x: pos.x + self.scroll.x,
            y: pos.y + self.scroll.y,
        }
    }

    /// Layout position to document position (approximate)
    pub fn layout_to_doc(&self, pos: LayoutPos) -> DocPos {
        let line = (pos.y / self.metrics.line_height) as u32;
        let column = (pos.x / self.metrics.space_width) as u32;

        DocPos {
            byte_offset: 0, // Would need document access for accurate byte offset
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
        ViewRect {
            x: rect.x - self.scroll.x,
            y: rect.y - self.scroll.y,
            width: rect.width,
            height: rect.height,
        }
    }

    /// Check if layout rectangle is visible in view
    pub fn is_visible(&self, rect: LayoutRect) -> bool {
        let view_rect = self.layout_rect_to_view(rect);
        view_rect.x < self.logical_size.width
            && view_rect.x + view_rect.width > 0.0
            && view_rect.y < self.logical_size.height
            && view_rect.y + view_rect.height > 0.0
    }

    // === Scrolling ===

    /// Scroll to make a layout position visible
    pub fn ensure_visible(&mut self, pos: LayoutPos) {
        // Horizontal scrolling
        if pos.x < self.scroll.x {
            self.scroll.x = pos.x;
        } else if pos.x > self.scroll.x + self.logical_size.width {
            self.scroll.x = pos.x - self.logical_size.width + 50.0; // Leave some margin
        }

        // Vertical scrolling
        if pos.y < self.scroll.y {
            self.scroll.y = pos.y;
        } else if pos.y + self.metrics.line_height > self.scroll.y + self.logical_size.height {
            self.scroll.y = pos.y + self.metrics.line_height - self.logical_size.height;
        }
    }

    /// Get visible line range
    pub fn visible_lines(&self) -> std::ops::Range<u32> {
        let first_line = (self.scroll.y / self.metrics.line_height) as u32;
        let last_line = ((self.scroll.y + self.logical_size.height) / self.metrics.line_height) as u32 + 1;
        first_line..last_line
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

impl LayoutRect {
    pub fn contains(&self, pos: LayoutPos) -> bool {
        pos.x >= self.x
            && pos.x <= self.x + self.width
            && pos.y >= self.y
            && pos.y <= self.y + self.height
    }
}

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
        let mut viewport = Viewport::new(800.0, 600.0, 2.0); // 2x scale (retina)

        // Doc → Layout → View → Physical
        let doc_pos = DocPos {
            byte_offset: 0,
            line: 5,
            column: 10,
        };

        let layout_pos = viewport.doc_to_layout(doc_pos);
        assert_eq!(layout_pos.x, 10.0 * viewport.metrics.space_width);
        assert_eq!(layout_pos.y, 5.0 * viewport.metrics.line_height);

        let view_pos = viewport.layout_to_view(layout_pos);
        assert_eq!(view_pos.x, layout_pos.x); // No scroll initially
        assert_eq!(view_pos.y, layout_pos.y);

        let physical_pos = viewport.view_to_physical(view_pos);
        assert_eq!(physical_pos.x, view_pos.x * 2.0); // 2x scale
        assert_eq!(physical_pos.y, view_pos.y * 2.0);
    }

    #[test]
    fn test_scrolling() {
        let mut viewport = Viewport::new(800.0, 600.0, 1.0);
        viewport.scroll = LayoutPos { x: 100.0, y: 200.0 };

        let layout_pos = LayoutPos { x: 150.0, y: 250.0 };
        let view_pos = viewport.layout_to_view(layout_pos);

        assert_eq!(view_pos.x, 50.0); // 150 - 100 scroll
        assert_eq!(view_pos.y, 50.0); // 250 - 200 scroll
    }

    #[test]
    fn test_visibility_check() {
        let mut viewport = Viewport::new(800.0, 600.0, 1.0);
        viewport.scroll = LayoutPos { x: 100.0, y: 100.0 };

        // Visible rectangle
        let visible_rect = LayoutRect {
            x: 150.0,
            y: 150.0,
            width: 100.0,
            height: 100.0,
        };
        assert!(viewport.is_visible(visible_rect));

        // Off-screen rectangle
        let offscreen_rect = LayoutRect {
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 50.0,
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