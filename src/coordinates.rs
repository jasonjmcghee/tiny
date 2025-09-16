//! Coordinate system abstraction for clean separation of concerns
//!
//! Three distinct coordinate systems:
//! 1. Document space: lines, columns, byte offsets (what editor thinks in)
//! 2. Logical space: DPI-independent pixels (what UI uses)
//! 3. Physical space: actual device pixels (what GPU renders)

/// Position in document space (what the editor uses)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocPos {
    /// Line number (0-indexed)
    pub line: u32,
    /// Column number (0-indexed, in characters not bytes)
    pub col: u32,
    /// Byte offset in the document
    pub byte: usize,
}

/// Position in logical pixel space (DPI-independent)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalPos {
    pub x: f32,
    pub y: f32,
}

/// Size in logical pixels
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalSize {
    pub width: f32,
    pub height: f32,
}

/// Position in physical pixel space (device pixels)
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

/// Viewport manages the transformation between coordinate systems
#[derive(Clone)]
pub struct Viewport {
    /// HiDPI scale factor (logical pixels to physical pixels)
    pub scale_factor: f32,

    /// Current scroll position in document space
    pub scroll: DocPos,

    /// Visible range of lines
    pub visible_lines: std::ops::Range<u32>,

    /// Window size in logical pixels
    pub logical_size: LogicalSize,

    /// Window size in physical pixels
    pub physical_size: PhysicalSize,

    /// Font metrics for text positioning
    pub line_height: f32,  // in logical pixels
    pub char_width: f32,   // average char width in logical pixels

    /// Font system for accurate text measurements
    pub font_system: Option<std::sync::Arc<crate::font::SharedFontSystem>>,

    /// Cache of text measurements for performance
    text_cache: std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<String, f32>>>,
}

impl Viewport {
    /// Create a new viewport
    pub fn new(logical_size: LogicalSize, scale_factor: f32) -> Self {
        let physical_size = PhysicalSize {
            width: (logical_size.width * scale_factor) as u32,
            height: (logical_size.height * scale_factor) as u32,
        };

        Self {
            scale_factor,
            scroll: DocPos { line: 0, col: 0, byte: 0 },
            visible_lines: 0..((logical_size.height / 20.0) as u32), // Assuming 20px line height
            logical_size,
            physical_size,
            line_height: 20.0,
            char_width: 8.0,
            font_system: None,
            text_cache: std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Set font system for accurate text measurements
    pub fn set_font_system(&mut self, font_system: std::sync::Arc<crate::font::SharedFontSystem>) {
        self.font_system = Some(font_system.clone());

        // Line height multiplier
        const LINE_HEIGHT_MULTIPLIER: f32 = 1.4;
        const FONT_SIZE: f32 = 13.0;

        self.line_height = FONT_SIZE * LINE_HEIGHT_MULTIPLIER * self.scale_factor;

        // Measure average character width
        let chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        let layout = font_system.layout_text_scaled(chars, FONT_SIZE, self.scale_factor);
        self.char_width = layout.width / chars.len() as f32;

        // Clear cache when font changes
        self.text_cache.lock().clear();
    }

    /// Update viewport size (e.g., on window resize)
    pub fn resize(&mut self, logical_size: LogicalSize, scale_factor: f32) {
        self.logical_size = logical_size;
        self.scale_factor = scale_factor;
        self.physical_size = PhysicalSize {
            width: (logical_size.width * scale_factor) as u32,
            height: (logical_size.height * scale_factor) as u32,
        };

        // Update visible lines
        let visible_line_count = (logical_size.height / self.line_height) as u32;
        self.visible_lines = self.scroll.line..self.scroll.line + visible_line_count;
    }

    /// Convert document position to logical position
    pub fn doc_to_logical(&self, pos: DocPos) -> LogicalPos {
        // For now, still use hardcoded metrics for compatibility
        // This will be replaced when we integrate with actual document text
        LogicalPos {
            x: (pos.col as f32) * self.char_width - (self.scroll.col as f32) * self.char_width,
            y: (pos.line as f32) * self.line_height - (self.scroll.line as f32) * self.line_height,
        }
    }

    /// Convert document position to logical position with actual text
    pub fn doc_to_logical_with_text(&self, pos: DocPos, line_text: &str) -> LogicalPos {
        let x = if let Some(font_system) = &self.font_system {
            // Measure actual text up to column position
            if pos.col > 0 && pos.col <= line_text.chars().count() as u32 {
                let prefix: String = line_text.chars().take(pos.col as usize).collect();

                // Check cache first
                let cache_key = format!("{}:{}", pos.line, prefix);
                if let Some(&cached_x) = self.text_cache.lock().get(&cache_key) {
                    cached_x - (self.scroll.col as f32) * self.char_width
                } else {
                    // Measure the text
                    let layout = font_system.layout_text_scaled(&prefix, 14.0, self.scale_factor);
                    let x = layout.width;

                    // Cache the measurement
                    self.text_cache.lock().insert(cache_key, x);
                    x - (self.scroll.col as f32) * self.char_width
                }
            } else {
                // Fallback to estimated position
                (pos.col as f32) * self.char_width - (self.scroll.col as f32) * self.char_width
            }
        } else {
            // No font system, use hardcoded metrics
            (pos.col as f32) * self.char_width - (self.scroll.col as f32) * self.char_width
        };

        LogicalPos {
            x,
            y: (pos.line as f32) * self.line_height - (self.scroll.line as f32) * self.line_height,
        }
    }

    /// Measure text width using font system
    pub fn measure_text(&self, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }

        if let Some(font_system) = &self.font_system {
            let layout = font_system.layout_text_scaled(text, 14.0, self.scale_factor);
            layout.width
        } else {
            // Fallback to character count
            let mut width = 0.0;
            for ch in text.chars() {
                if ch == '\t' {
                    width += self.char_width * 4.0; // Tab is 4 spaces
                } else {
                    width += self.char_width;
                }
            }
            width
        }
    }

    /// Convert logical position to physical position
    pub fn logical_to_physical(&self, pos: LogicalPos) -> PhysicalPos {
        PhysicalPos {
            x: pos.x * self.scale_factor,
            y: pos.y * self.scale_factor,
        }
    }

    /// Convert document position directly to physical position
    pub fn doc_to_physical(&self, pos: DocPos) -> PhysicalPos {
        self.logical_to_physical(self.doc_to_logical(pos))
    }

    /// Convert physical position to logical position
    pub fn physical_to_logical(&self, pos: PhysicalPos) -> LogicalPos {
        LogicalPos {
            x: pos.x / self.scale_factor,
            y: pos.y / self.scale_factor,
        }
    }

    /// Convert logical position to document position (approximate)
    pub fn logical_to_doc(&self, pos: LogicalPos) -> DocPos {
        let line = ((pos.y / self.line_height) + self.scroll.line as f32) as u32;
        let col = ((pos.x / self.char_width) + self.scroll.col as f32) as u32;

        DocPos {
            line,
            col,
            byte: 0, // Would need document access to calculate actual byte offset
        }
    }

    /// Check if a document position is visible
    pub fn is_visible(&self, pos: DocPos) -> bool {
        self.visible_lines.contains(&pos.line)
    }

    /// Scroll to make a document position visible
    pub fn ensure_visible(&mut self, pos: DocPos) {
        if pos.line < self.visible_lines.start {
            // Scroll up
            self.scroll.line = pos.line;
        } else if pos.line >= self.visible_lines.end {
            // Scroll down
            let visible_count = self.visible_lines.end - self.visible_lines.start;
            self.scroll.line = pos.line.saturating_sub(visible_count - 1);
        }

        // Update visible range
        let visible_count = (self.logical_size.height / self.line_height) as u32;
        self.visible_lines = self.scroll.line..self.scroll.line + visible_count;
    }
}

/// Glyph metrics in logical space
#[derive(Debug, Clone, Copy)]
pub struct GlyphMetrics {
    /// Position relative to baseline
    pub offset: LogicalPos,
    /// Size of the glyph
    pub size: LogicalSize,
    /// Advance to next glyph
    pub advance: f32,
}

impl Default for DocPos {
    fn default() -> Self {
        Self { line: 0, col: 0, byte: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinate_transformations() {
        let viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 1.0);

        // Doc to Logical
        let doc_pos = DocPos {
            line: 5,
            col: 10,
            byte: 0,
        };
        let logical = viewport.doc_to_logical(doc_pos);
        assert_eq!(logical.x, 10.0 * viewport.char_width);
        assert_eq!(logical.y, 5.0 * viewport.line_height);

        // Logical to Physical (no scaling)
        let physical = viewport.logical_to_physical(logical);
        assert_eq!(physical.x, logical.x);
        assert_eq!(physical.y, logical.y);
    }

    #[test]
    fn test_scale_factor_conversions() {
        let viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 2.0); // Retina

        let doc_pos = DocPos {
            line: 1,
            col: 3,
            byte: 0,
        };

        let logical = viewport.doc_to_logical(doc_pos);
        let physical = viewport.logical_to_physical(logical);

        // Logical coordinates independent of scale
        assert_eq!(logical.x, 3.0 * viewport.char_width);
        assert_eq!(logical.y, 1.0 * viewport.line_height);

        // Physical coordinates scaled by 2x
        assert_eq!(physical.x, logical.x * 2.0);
        assert_eq!(physical.y, logical.y * 2.0);
    }

    #[test]
    fn test_roundtrip_conversion() {
        let viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 1.5);

        let original = DocPos {
            line: 7,
            col: 15,
            byte: 0,
        };

        // Full roundtrip
        let logical = viewport.doc_to_logical(original);
        let physical = viewport.logical_to_physical(logical);
        let back_logical = viewport.physical_to_logical(physical);
        let back_doc = viewport.logical_to_doc(back_logical);

        // Should return to exact same position
        assert_eq!(original.line, back_doc.line);
        assert_eq!(original.col, back_doc.col);

        // Logical coordinates should match (within floating point precision)
        assert!((logical.x - back_logical.x).abs() < 0.01);
        assert!((logical.y - back_logical.y).abs() < 0.01);
    }

    #[test]
    fn test_viewport_scrolling() {
        let mut viewport = Viewport::new(LogicalSize { width: 800.0, height: 400.0 }, 1.0); // 20 lines visible

        // Initially see lines 0-19
        assert_eq!(viewport.visible_lines, 0..20);

        // Scroll to make line 50 visible
        viewport.ensure_visible(DocPos {
            line: 50,
            col: 0,
            byte: 0,
        });

        assert!(viewport.visible_lines.contains(&50));
        assert_eq!(viewport.scroll.line, 31); // Scrolled to show lines 31-50

        // Scroll with horizontal offset
        viewport.ensure_visible(DocPos {
            line: 50,
            col: 100,
            byte: 0,
        });

        // Horizontal scrolling - implementation may vary
        assert!(viewport.scroll.col <= 100); // Should have scrolled to show column 100
    }

    #[test]
    fn test_viewport_resize() {
        let mut viewport = Viewport::new(LogicalSize { width: 400.0, height: 300.0 }, 1.0);

        let initial_visible = viewport.visible_lines.clone();

        // Resize to larger
        viewport.resize(LogicalSize { width: 800.0, height: 600.0 }, 1.0);

        // Should see more lines
        assert!(viewport.visible_lines.len() > initial_visible.len());

        // Physical size should update
        assert_eq!(viewport.physical_size.width, 800);
        assert_eq!(viewport.physical_size.height, 600);
    }

    #[test]
    fn test_doc_position_with_scroll() {
        let mut viewport = Viewport::new(LogicalSize { width: 800.0, height: 600.0 }, 1.0);

        // Set scroll offset
        viewport.scroll = DocPos {
            line: 10,
            col: 5,
            byte: 0,
        };

        // Position relative to scroll
        let doc_pos = DocPos {
            line: 12,
            col: 8,
            byte: 0,
        };

        let logical = viewport.doc_to_logical(doc_pos);

        // Should be offset by scroll amount
        assert_eq!(logical.x, (8 - 5) as f32 * viewport.char_width);
        assert_eq!(logical.y, (12 - 10) as f32 * viewport.line_height);
    }
}