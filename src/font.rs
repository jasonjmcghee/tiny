//! Consolidated font system - single source of truth for text rendering
//!
//! Combines font loading, text shaping, glyph rasterization, and atlas management

use crate::render::GlyphInstance;
use fontdue::layout::{CoordinateSystem, Layout, TextStyle};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use crate::coordinates::{PhysicalPos, PhysicalSizeF};

/// Information about a positioned glyph ready for rendering
#[derive(Clone, Debug)]
pub struct PositionedGlyph {
    /// Character for debugging
    pub char: char,
    /// Position in physical pixels (already scaled)
    pub pos: PhysicalPos,
    /// Size in physical pixels (already scaled)
    pub size: PhysicalSizeF,
    /// Texture coordinates in atlas [u0, v0, u1, v1]
    pub tex_coords: [f32; 4],
    /// Color (default white)
    pub color: u32,
}

/// Layout result with positioned glyphs
pub struct TextLayout {
    pub glyphs: Vec<PositionedGlyph>,
    pub width: f32,
    pub height: f32,
}

/// Consolidated font system - handles everything font-related
pub struct FontSystem {
    /// The font (JetBrains Mono)
    font: fontdue::Font,
    /// Text layout engine
    layout: Layout,
    /// Atlas texture data (R8 format)
    atlas_data: Vec<u8>,
    /// Atlas dimensions
    atlas_size: (u32, u32),
    /// Cache of rasterized glyphs: (char, size_in_pixels) -> (tex_coords, metrics)
    glyph_cache: HashMap<(char, u32), GlyphEntry>,
    /// Current atlas cursor
    next_x: u32,
    next_y: u32,
    row_height: u32,
}

#[derive(Clone, Copy, Debug)]
struct GlyphEntry {
    tex_coords: [f32; 4],
    width: f32,
    height: f32,
    advance: f32,
    #[allow(dead_code)]
    bearing_x: f32,
    #[allow(dead_code)]
    bearing_y: f32,
}

impl FontSystem {
    /// Create new font system
    pub fn new() -> Self {
        let font_data = include_bytes!("../assets/JetBrainsMono-VariableFont_wght.ttf");
        let font = fontdue::Font::from_bytes(font_data as &[u8], fontdue::FontSettings::default())
            .expect("Failed to load JetBrains Mono font");

        let layout = Layout::new(CoordinateSystem::PositiveYDown);
        let atlas_size = (2048, 2048);
        let atlas_data = vec![0; (atlas_size.0 * atlas_size.1) as usize];

        Self {
            font,
            layout,
            atlas_data,
            atlas_size,
            glyph_cache: HashMap::new(),
            next_x: 0,
            next_y: 0,
            row_height: 0,
        }
    }

    /// Layout text and get positioned glyphs with texture coordinates
    /// font_size should already include scale_factor (e.g., 14.0 * 2.0 = 28.0 for retina)
    /// Returns positions scaled back to logical coordinates (14pt scale)
    pub fn layout_text(&mut self, text: &str, font_size_px: f32) -> TextLayout {
        // Clear previous layout
        self.layout.clear();

        // Layout the text at the requested size
        self.layout
            .append(&[&self.font], &TextStyle::new(text, font_size_px, 0));

        // Collect glyph info first to avoid borrow issues
        let glyph_info: Vec<_> = self
            .layout
            .glyphs()
            .iter()
            .map(|g| (g.parent, g.x, g.y))
            .collect();

        let mut positioned_glyphs = Vec::new();
        let mut max_x = 0.0f32;
        let mut max_y = 0.0f32;

        // Process each glyph
        for (ch, x, y) in glyph_info {
            // Handle tab character specially
            if ch == '\t' {
                // Tab should advance by 4 space widths but not render a glyph
                // Get space width from font metrics
                let space_entry = self.get_or_rasterize(' ', font_size_px as u32);
                let tab_width = space_entry.advance * 4.0; // 4 spaces

                positioned_glyphs.push(PositionedGlyph {
                    char: ch,
                    pos: PhysicalPos::new(x, y),
                    size: PhysicalSizeF::new(tab_width, space_entry.height),
                    tex_coords: [0.0, 0.0, 0.0, 0.0], // No texture for tab
                    color: 0x00000000, // Transparent
                });

                max_x = max_x.max(x + tab_width);
                max_y = max_y.max(y + space_entry.height);
                continue;
            }

            // Skip other non-printable characters
            if ch.is_control() {
                continue;
            }

            // Rasterize at the same size we're laying out
            let entry = self.get_or_rasterize(ch, font_size_px as u32);

            // Everything is already at the right scale
            positioned_glyphs.push(PositionedGlyph {
                char: ch,
                pos: PhysicalPos::new(x, y), // Already scaled
                size: PhysicalSizeF::new(entry.width, entry.height),
                tex_coords: entry.tex_coords,
                color: 0xFFFFFFFF,
            });

            max_x = max_x.max(x + entry.advance);
            max_y = max_y.max(y + entry.height);
        }

        TextLayout {
            glyphs: positioned_glyphs,
            width: max_x,
            height: max_y,
        }
    }

    /// Get or rasterize a glyph at physical pixel size
    fn get_or_rasterize(&mut self, ch: char, size_px: u32) -> GlyphEntry {
        let key = (ch, size_px);

        // Check cache first
        if let Some(&entry) = self.glyph_cache.get(&key) {
            return entry;
        }

        // Rasterize the glyph at physical size
        let (metrics, bitmap) = self.font.rasterize(ch, size_px as f32);

        // Check if glyph fits in current row
        if self.next_x + metrics.width as u32 > self.atlas_size.0 {
            self.next_x = 0;
            self.next_y += self.row_height;
            self.row_height = 0;
        }

        // If atlas is full, just return empty glyph (should handle better)
        if self.next_y + metrics.height as u32 > self.atlas_size.1 {
            return GlyphEntry {
                tex_coords: [0.0, 0.0, 0.0, 0.0],
                width: 0.0,
                height: 0.0,
                advance: metrics.advance_width,
                bearing_x: 0.0,
                bearing_y: 0.0,
            };
        }

        // Copy bitmap to atlas
        for y in 0..metrics.height {
            for x in 0..metrics.width {
                let atlas_idx = ((self.next_y + y as u32) * self.atlas_size.0
                    + (self.next_x + x as u32)) as usize;
                let bitmap_idx = (y * metrics.width + x) as usize;
                if atlas_idx < self.atlas_data.len() && bitmap_idx < bitmap.len() {
                    self.atlas_data[atlas_idx] = bitmap[bitmap_idx];
                }
            }
        }

        // Calculate texture coordinates
        let u0 = self.next_x as f32 / self.atlas_size.0 as f32;
        let v0 = self.next_y as f32 / self.atlas_size.1 as f32;
        let u1 = (self.next_x + metrics.width as u32) as f32 / self.atlas_size.0 as f32;
        let v1 = (self.next_y + metrics.height as u32) as f32 / self.atlas_size.1 as f32;

        let entry = GlyphEntry {
            tex_coords: [u0, v0, u1, v1],
            width: metrics.width as f32,
            height: metrics.height as f32,
            advance: metrics.advance_width,
            bearing_x: metrics.bounds.xmin,
            bearing_y: metrics.bounds.ymin,
        };

        // Update atlas position
        self.next_x += metrics.width as u32 + 1; // 1px padding
        self.row_height = self.row_height.max(metrics.height as u32 + 1);

        // Cache and return
        self.glyph_cache.insert(key, entry);
        entry
    }

    /// Pre-rasterize common ASCII characters
    /// font_size_px should already include scale (e.g., 14.0 * 2.0 for retina)
    pub fn prerasterize_ascii(&mut self, font_size_px: f32) {
        for ch in ' '..='~' {
            self.get_or_rasterize(ch, font_size_px as u32);
        }
    }

    /// Get atlas data for GPU upload
    pub fn atlas_data(&self) -> &[u8] {
        &self.atlas_data
    }

    /// Get atlas dimensions
    pub fn atlas_size(&self) -> (u32, u32) {
        self.atlas_size
    }
}

/// Thread-safe wrapper for FontSystem
pub struct SharedFontSystem {
    inner: Arc<Mutex<FontSystem>>,
}

impl SharedFontSystem {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FontSystem::new())),
        }
    }

    /// Layout text with automatic crisp rasterization based on stored scale factor
    pub fn layout_text(&self, text: &str, logical_font_size: f32) -> TextLayout {
        // Use scale factor of 1.0 for now - widgets should call layout_text_scaled for crisp rendering
        self.inner.lock().layout_text(text, logical_font_size)
    }

    /// Layout text with explicit scale factor for crisp rendering
    pub fn layout_text_scaled(
        &self,
        text: &str,
        logical_font_size: f32,
        scale_factor: f32,
    ) -> TextLayout {
        let mut font_system = self.inner.lock();
        let physical_size = logical_font_size * scale_factor;
        let layout = font_system.layout_text(text, physical_size);

        // Return layout unchanged - it's already in physical pixels
        // which is what we need for direct GPU rendering
        TextLayout {
            glyphs: layout
                .glyphs
                .iter()
                .map(|g| PositionedGlyph {
                    char: g.char,
                    pos: g.pos.clone(),
                    size: g.size.clone(),
                    tex_coords: g.tex_coords,
                    color: g.color,
                })
                .collect(),
            width: layout.width,
            height: layout.height,
        }
    }

    /// Pre-rasterize ASCII - font_size_px should include scale factor
    pub fn prerasterize_ascii(&self, font_size_px: f32) {
        self.inner.lock().prerasterize_ascii(font_size_px);
    }

    /// Get atlas data
    pub fn atlas_data(&self) -> Vec<u8> {
        self.inner.lock().atlas_data.clone()
    }

    /// Get atlas size
    pub fn atlas_size(&self) -> (u32, u32) {
        self.inner.lock().atlas_size()
    }

    /// Hit test: find character position at x coordinate using binary search
    /// Uses the FULL line layout to get accurate positioning with kerning/ligatures
    pub fn hit_test_line(&self, line_text: &str, logical_font_size: f32, scale_factor: f32, target_x: f32) -> u32 {
        if line_text.is_empty() {
            return 0;
        }

        // Layout the full line to get accurate glyph positions
        let full_layout = self.layout_text_scaled(line_text, logical_font_size, scale_factor);

        // Handle case where layout produces no glyphs (whitespace-only lines)
        if full_layout.glyphs.is_empty() {
            if full_layout.width > 0.0 {
                // Use the font system's actual measurement of the line width
                let line_width_logical = full_layout.width / scale_factor;
                let progress = (target_x / line_width_logical).clamp(0.0, 1.0);
                return (progress * line_text.chars().count() as f32) as u32;
            } else {
                // Truly empty line
                return 0;
            }
        }

        let target_x_physical = target_x * scale_factor; // Convert to physical pixels

        // Binary search through glyph positions
        let mut left = 0;
        let mut right = full_layout.glyphs.len();

        while left < right {
            let mid = (left + right) / 2;
            let glyph = &full_layout.glyphs[mid];
            let glyph_center = glyph.pos.x.0 + glyph.size.width.0 / 2.0;

            if glyph_center <= target_x_physical {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        // Return character position (not glyph position - could differ with ligatures)
        left as u32
    }
}

/// Convert layout to GPU instances
pub fn layout_to_instances(
    layout: &TextLayout,
    offset_x: f32,
    offset_y: f32,
    scale_factor: f32,
) -> Vec<GlyphInstance> {
    layout
        .glyphs
        .iter()
        .map(|g| {
            GlyphInstance {
                glyph_id: 0, // Not used anymore
                pos: PhysicalPos::new(
                    (g.pos.x.0 + offset_x) * scale_factor,
                    (g.pos.y.0 + offset_y) * scale_factor,
                ),
                color: g.color,
                tex_coords: g.tex_coords,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::coordinates::PhysicalPixels;
    use super::*;

    #[test]
    fn test_font_system_creation() {
        let mut font_system = FontSystem::new();
        assert_eq!(font_system.atlas_size(), (2048, 2048));

        // Layout some text at base size
        let layout = font_system.layout_text("Hello", 14.0);
        assert_eq!(layout.glyphs.len(), 5); // 5 characters
        assert!(layout.width > 0.0);
        assert!(layout.height > 0.0);
    }

    #[test]
    fn test_single_char_layout() {
        let mut font_system = FontSystem::new();

        // Layout single 'A' at size 14
        let layout = font_system.layout_text("A", 14.0);
        assert_eq!(layout.glyphs.len(), 1);

        let glyph = &layout.glyphs[0];
        assert_eq!(glyph.char, 'A');
        assert_eq!(glyph.pos.x, PhysicalPixels(0.0)); // First char at x=0
        // The y position from fontdue represents baseline offset
        // For the default font at size 14, this is exactly 4.0
        assert_eq!(glyph.pos.y, PhysicalPixels(4.0));
        assert!(glyph.size.width.0 > 0.0);
        assert!(glyph.size.height.0 > 0.0);

        // Texture coords should be valid
        assert!(glyph.tex_coords[2] > glyph.tex_coords[0]); // u1 > u0
        assert!(glyph.tex_coords[3] > glyph.tex_coords[1]); // v1 > v0
    }

    #[test]
    fn test_layout_with_scale() {
        let mut font_system = FontSystem::new();

        // Layout at 2x scale (simulating retina)
        let layout_1x = font_system.layout_text("AB", 14.0);
        let layout_2x = font_system.layout_text("AB", 28.0); // 14 * 2

        // Should have same number of glyphs
        assert_eq!(layout_1x.glyphs.len(), 2);
        assert_eq!(layout_2x.glyphs.len(), 2);

        // 2x layout should have ~2x the spacing
        // Get the x position of the second character
        let spacing_1x = layout_1x.glyphs[1].pos.x.0;
        let spacing_2x = layout_2x.glyphs[1].pos.x.0;

        // Should be roughly 2x (within rounding tolerance)
        // Font metrics can vary, so we allow a wider range
        let ratio = spacing_2x / spacing_1x;
        assert!(ratio > 1.5 && ratio < 2.5, "Spacing ratio was {}", ratio);
    }

    #[test]
    fn test_shared_font_system() {
        let font_system = SharedFontSystem::new();

        // Test layout with same font size
        let layout1 = font_system.layout_text("Test", 14.0);
        let layout2 = font_system.layout_text("Test", 14.0);

        assert_eq!(layout1.glyphs.len(), 4);
        assert_eq!(layout2.glyphs.len(), 4);

        // Glyphs should have same logical positions
        for (g1, g2) in layout1.glyphs.iter().zip(layout2.glyphs.iter()) {
            assert_eq!(g1.pos.x, g2.pos.x);
            assert_eq!(g1.pos.y, g2.pos.y);
        }
    }

    #[test]
    fn test_prerasterize_ascii() {
        let font_system = SharedFontSystem::new();
        font_system.prerasterize_ascii(14.0);

        // After prerasterization, layout should be fast
        let layout = font_system.layout_text("ABC123", 14.0);
        assert_eq!(layout.glyphs.len(), 6);
    }
}
