//! Layout cache for text positioning and line management

use ahash::HashMap;
use std::ops::Range;
use tiny_sdk::{LayoutPos, LayoutRect, LogicalSize, PhysicalPos, ViewportInfo};

/// Information about a glyph's position and style
#[derive(Clone, Debug)]
pub struct GlyphInfo {
    pub char: char,
    pub layout_pos: LayoutPos,
    pub physical_pos: PhysicalPos,
    pub tex_coords: [f32; 4],
    pub byte_offset: usize,
    pub token_id: u16,
}

/// Information about a line of text
#[derive(Clone, Debug)]
pub struct LineInfo {
    pub line_number: u32,
    pub byte_range: Range<usize>,
    pub glyph_range: Range<usize>,
    pub y_position: f32,
    pub height: f32,
    pub width: f32,
}

/// Cache for text layout information
pub struct LayoutCache {
    // All glyphs with positions
    glyphs: Vec<GlyphInfo>,

    // Line information
    lines: Vec<LineInfo>,

    // Quick lookup from byte offset to glyph index
    byte_to_glyph: HashMap<usize, usize>,

    // Document metadata
    total_height: f32,
    max_width: f32,
    line_height: f32,
    char_width: f32,

    // Version tracking
    version: u64,
}

impl LayoutCache {
    pub fn new() -> Self {
        Self {
            glyphs: Vec::new(),
            lines: Vec::new(),
            byte_to_glyph: HashMap::default(),
            total_height: 0.0,
            max_width: 0.0,
            line_height: 19.6,
            char_width: 8.4,
            version: 0,
        }
    }

    /// Update layout cache with new text
    pub fn update_text(
        &mut self,
        text: &str,
        viewport: &ViewportInfo,
        font_size: f32,
        line_height: f32,
    ) {
        self.glyphs.clear();
        self.lines.clear();
        self.byte_to_glyph.clear();

        self.line_height = line_height;
        // Approximate char width based on font size
        self.char_width = font_size * 0.6;

        let lines_text: Vec<&str> = text.lines().collect();
        let mut byte_offset = 0;
        let mut glyph_index = 0;
        let mut y_pos = viewport.margin.y.0;

        for (line_idx, line_text) in lines_text.iter().enumerate() {
            let line_start_byte = byte_offset;
            let line_start_glyph = glyph_index;
            let mut line_width = 0.0;

            // Layout glyphs for this line
            let mut x_pos = viewport.margin.x.0;
            for ch in line_text.chars() {
                let char_width = if ch == '\t' {
                    self.char_width * 4.0  // Tab width
                } else if ch.is_ascii() {
                    self.char_width
                } else {
                    self.char_width * 2.0  // Wide chars
                };

                let layout_pos = LayoutPos::new(x_pos, y_pos);
                let physical_pos = PhysicalPos::new(
                    x_pos * viewport.scale_factor,
                    y_pos * viewport.scale_factor,
                );

                self.byte_to_glyph.insert(byte_offset, glyph_index);

                self.glyphs.push(GlyphInfo {
                    char: ch,
                    layout_pos,
                    physical_pos,
                    tex_coords: [0.0, 0.0, 1.0, 1.0], // Placeholder
                    byte_offset,
                    token_id: 0, // Will be set by syntax highlighting
                });

                x_pos += char_width;
                line_width += char_width;
                byte_offset += ch.len_utf8();
                glyph_index += 1;
            }

            // Add line info
            self.lines.push(LineInfo {
                line_number: line_idx as u32,
                byte_range: line_start_byte..byte_offset,
                glyph_range: line_start_glyph..glyph_index,
                y_position: y_pos,
                height: self.line_height,
                width: line_width,
            });

            self.max_width = self.max_width.max(line_width);

            // Add newline glyph if not last line
            if line_idx < lines_text.len() - 1 {
                self.byte_to_glyph.insert(byte_offset, glyph_index);

                self.glyphs.push(GlyphInfo {
                    char: '\n',
                    layout_pos: LayoutPos::new(viewport.margin.x.0, y_pos),
                    physical_pos: PhysicalPos::new(
                        viewport.margin.x.0 * viewport.scale_factor,
                        y_pos * viewport.scale_factor,
                    ),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // Invisible
                    byte_offset,
                    token_id: 0,
                });

                byte_offset += 1;
                glyph_index += 1;
            }

            y_pos += self.line_height;
        }

        self.total_height = y_pos;
        self.version += 1;
    }

    /// Get character position by byte offset
    pub fn get_char_position(&self, byte_offset: usize) -> Option<LayoutPos> {
        self.byte_to_glyph
            .get(&byte_offset)
            .and_then(|&idx| self.glyphs.get(idx))
            .map(|glyph| glyph.layout_pos)
    }

    /// Get character bounds by byte offset
    pub fn get_char_bounds(&self, byte_offset: usize) -> Option<LayoutRect> {
        self.byte_to_glyph
            .get(&byte_offset)
            .and_then(|&idx| self.glyphs.get(idx))
            .map(|glyph| {
                LayoutRect::new(
                    glyph.layout_pos.x.0,
                    glyph.layout_pos.y.0,
                    self.char_width,
                    self.line_height,
                )
            })
    }

    /// Get line bounds by line number
    pub fn get_line_bounds(&self, line_number: u32) -> Option<LayoutRect> {
        self.lines.get(line_number as usize).map(|line| {
            LayoutRect::new(
                0.0,
                line.y_position,
                line.width.max(100.0),  // Minimum width for empty lines
                line.height,
            )
        })
    }

    /// Get visible lines for a given viewport
    pub fn get_visible_lines(&self, scroll_y: f32, viewport_height: f32) -> (u32, u32) {
        let viewport_top = scroll_y;
        let viewport_bottom = scroll_y + viewport_height;

        let mut start_line = None;
        let mut end_line = None;

        for (idx, line) in self.lines.iter().enumerate() {
            let line_bottom = line.y_position + line.height;

            if line_bottom > viewport_top && start_line.is_none() {
                start_line = Some(idx as u32);
            }

            if line.y_position < viewport_bottom {
                end_line = Some(idx as u32 + 1);
            } else if start_line.is_some() {
                break;
            }
        }

        (start_line.unwrap_or(0), end_line.unwrap_or(0))
    }

    /// Get visible glyphs for rendering
    pub fn get_visible_glyphs(
        &self,
        scroll_y: f32,
        viewport_height: f32,
    ) -> Vec<&GlyphInfo> {
        let (start_line, end_line) = self.get_visible_lines(scroll_y, viewport_height);

        let mut visible = Vec::new();
        for line_idx in start_line..end_line {
            if let Some(line) = self.lines.get(line_idx as usize) {
                for glyph_idx in line.glyph_range.clone() {
                    if let Some(glyph) = self.glyphs.get(glyph_idx) {
                        visible.push(glyph);
                    }
                }
            }
        }

        visible
    }

    /// Apply syntax highlighting token IDs
    pub fn apply_syntax_tokens(&mut self, tokens: &[(Range<usize>, u16)]) {
        for (range, token_id) in tokens {
            for glyph in &mut self.glyphs {
                if range.contains(&glyph.byte_offset) {
                    glyph.token_id = *token_id;
                }
            }
        }
    }

    // Getters
    pub fn line_height(&self) -> f32 {
        self.line_height
    }

    pub fn char_width(&self) -> f32 {
        self.char_width
    }

    pub fn total_height(&self) -> f32 {
        self.total_height
    }

    pub fn max_width(&self) -> f32 {
        self.max_width
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn glyphs(&self) -> &[GlyphInfo] {
        &self.glyphs
    }

    pub fn lines(&self) -> &[LineInfo] {
        &self.lines
    }
}