//! Consolidated font system - single source of truth for text rendering
//!
//! Combines font loading, text shaping, glyph rasterization, and atlas management

use ahash::HashMap;
use fontdue::layout::{CoordinateSystem, Layout, TextStyle};
use parking_lot::Mutex;
use std::sync::Arc;
use tiny_sdk::services::{
    FontService, PositionedGlyph as SdkPositionedGlyph, TextLayout as SdkTextLayout,
};
use tiny_sdk::types::{GlyphInstance, LayoutPos, PhysicalPos, PhysicalSizeF};

/// Helper to expand tabs to spaces
fn expand_tabs(text: &str) -> String {
    const TAB_WIDTH: usize = 4;
    let mut result = String::new();
    let mut column = 0;

    for ch in text.chars() {
        if ch == '\t' {
            let spaces_needed = TAB_WIDTH - (column % TAB_WIDTH);
            for _ in 0..spaces_needed {
                result.push(' ');
            }
            column += spaces_needed;
        } else if ch == '\n' {
            result.push(ch);
            column = 0;
        } else {
            result.push(ch);
            column += 1;
        }
    }

    result
}

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
    /// Approx character width coefficient to multiply by the font for fast calculations
    char_width_coef: f32,
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

        let mut self_ = Self {
            font,
            layout,
            atlas_data,
            atlas_size,
            glyph_cache: HashMap::default(),
            next_x: 0,
            next_y: 0,
            row_height: 0,
            char_width_coef: 0.0,
        };

        // Get the monospace advance width by measuring a single character
        // In a monospace font, all characters should have the same advance
        let test_size = 16.0;
        let (metrics, _) = self_.font.rasterize('M', test_size);
        self_.char_width_coef = metrics.advance_width / test_size;

        self_
    }

    /// Layout text with grid-aligned positioning for monospace fonts
    /// This ensures perfect column alignment for cursor positioning
    pub fn layout_text_grid_aligned(&mut self, text: &str, font_size_px: f32) -> TextLayout {
        let expanded_text = expand_tabs(text);

        // Use the same advance calculation as regular layout
        // This should match what fontdue uses internally
        let advance = self.char_width_coef * font_size_px;

        // First, let fontdue layout the text to get proper baseline positioning
        self.layout.clear();
        self.layout.append(
            &[&self.font],
            &TextStyle::new(&expanded_text, font_size_px, 0),
        );

        // Get the y positions from fontdue (for proper baseline alignment)
        let glyph_info: Vec<_> = self
            .layout
            .glyphs()
            .iter()
            .map(|g| (g.parent, g.y)) // We only need char and y position
            .collect();

        let mut positioned_glyphs = Vec::new();
        let mut max_x = 0.0f32;
        let mut max_y = 0.0f32;
        let mut glyph_index = 0;

        // Position each character on a perfect grid (x), but use fontdue's y
        for (i, ch) in expanded_text.chars().enumerate() {
            if ch.is_control() && ch != ' ' {
                continue;
            }
            if ch == '\n' {
                continue; // Skip newlines
            }

            let entry = self.get_or_rasterize(ch, font_size_px as u32);
            let x = i as f32 * advance; // Perfect grid positioning for x

            // Use fontdue's y position for proper baseline alignment
            let y = if glyph_index < glyph_info.len() {
                let (_, fontdue_y) = glyph_info[glyph_index];
                glyph_index += 1;
                fontdue_y
            } else {
                0.0
            };

            // For grid alignment:
            // - X: use grid position without bearing (cursor aligns with grid)
            // - Y: use fontdue's baseline position without adjustment
            positioned_glyphs.push(PositionedGlyph {
                char: ch,
                pos: PhysicalPos::new(x, y), // No bearing adjustments in grid mode
                size: PhysicalSizeF::new(entry.width, entry.height),
                tex_coords: entry.tex_coords,
                color: 0xE1E1E1FF,
            });

            max_x = (i + 1) as f32 * advance; // Total width is columns * advance
            max_y = max_y.max(y + entry.height);
        }

        TextLayout {
            glyphs: positioned_glyphs,
            width: max_x,
            height: max_y,
        }
    }

    /// Layout text and get positioned glyphs with texture coordinates
    /// font_size should already include scale_factor (e.g., 14.0 * 2.0 = 28.0 for retina)
    /// Returns positions scaled back to logical coordinates (14pt scale)
    pub fn layout_text(&mut self, text: &str, font_size_px: f32) -> TextLayout {
        // Clear previous layout
        self.layout.clear();

        // Expand tabs to spaces for proper layout
        let expanded_text = expand_tabs(text);

        // Layout the text at the requested size
        self.layout.append(
            &[&self.font],
            &TextStyle::new(&expanded_text, font_size_px, 0),
        );

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
            // Skip non-printable characters (tabs are already expanded to spaces)
            if ch.is_control() && ch != ' ' {
                continue;
            }

            // Rasterize at the same size we're laying out
            let entry = self.get_or_rasterize(ch, font_size_px as u32);

            positioned_glyphs.push(PositionedGlyph {
                char: ch,
                pos: PhysicalPos::new(x, y),
                size: PhysicalSizeF::new(entry.width, entry.height),
                tex_coords: entry.tex_coords,
                color: 0xE1E1E1FF,
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

    /// Layout text with explicit scale factor
    /// This is a convenience method that converts logical font size to physical
    pub fn layout_text_scaled(
        &mut self,
        text: &str,
        logical_font_size: f32,
        scale_factor: f32,
    ) -> TextLayout {
        let physical_size = logical_font_size * scale_factor;
        self.layout_text(text, physical_size)
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

    pub fn char_width_coef(&self) -> f32 {
        self.char_width_coef
    }

    /// Clear the glyph cache and reset atlas position
    /// This should be called when font size changes to prevent atlas overflow
    pub fn clear_cache(&mut self) {
        self.glyph_cache.clear();
        self.next_x = 0;
        self.next_y = 0;
        self.row_height = 0;
        // Clear atlas data
        self.atlas_data.fill(0);
    }
}

/// Thread-safe wrapper for FontSystem
#[derive(Clone)]
pub struct SharedFontSystem {
    inner: Arc<Mutex<FontSystem>>,
}

impl SharedFontSystem {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FontSystem::new())),
        }
    }

    pub fn char_width_coef(&self) -> f32 {
        self.inner.lock().char_width_coef()
    }

    /// Layout text with grid-aligned positioning for monospace fonts
    pub fn layout_text_grid_aligned(
        &self,
        text: &str,
        logical_font_size: f32,
        scale_factor: f32,
    ) -> TextLayout {
        let mut font_system = self.inner.lock();
        let physical_size = logical_font_size * scale_factor;
        let layout = font_system.layout_text_grid_aligned(text, physical_size);

        // Return layout with glyphs already in physical pixels
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
        let mut font_system = self.inner.lock();
        // Clear cache before prerasterizing at new size
        font_system.clear_cache();
        font_system.prerasterize_ascii(font_size_px);
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
    pub fn hit_test_line(
        &self,
        line_text: &str,
        logical_font_size: f32,
        scale_factor: f32,
        target_x: f32,
    ) -> u32 {
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

        // Map glyph positions back to original character positions
        // We need this because tabs are expanded to spaces in layout
        let mut char_map = Vec::new(); // Maps expanded position -> original position
        let mut orig_pos = 0;
        let mut exp_pos = 0;

        for orig_ch in line_text.chars() {
            if orig_ch == '\t' {
                let spaces_needed = 4 - (exp_pos % 4);
                for _ in 0..spaces_needed {
                    char_map.push(orig_pos);
                    exp_pos += 1;
                }
            } else {
                char_map.push(orig_pos);
                exp_pos += 1;
            }
            orig_pos += 1;
        }

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

        // Map back to original character position
        if left < char_map.len() {
            char_map[left] as u32
        } else if !char_map.is_empty() {
            *char_map.last().unwrap() as u32 + 1
        } else {
            left as u32
        }
    }

    /// Find which column corresponds to a pixel position
    pub fn pixel_to_column(&self, x_logical: f32, text: &str, font_size: f32, scale: f32) -> usize {
        self.inner
            .lock()
            .pixel_to_column(x_logical, text, font_size, scale)
    }
}

impl FontSystem {
    /// Find which column corresponds to a pixel position
    /// Returns the column index (character position) that starts at or after the given x pixel position
    /// NOTE: x is in logical pixels, not physical pixels
    pub fn pixel_to_column(
        &mut self,
        x_logical: f32,
        text: &str,
        font_size: f32,
        scale: f32,
    ) -> usize {
        if text.is_empty() || x_logical <= 0.0 {
            return 0;
        }

        // Use fast path for very long lines to avoid expensive full layout
        if text.len() > 1000 {
            let char_width = self.char_width_coef() * font_size / scale;
            let estimated_col = (x_logical / char_width) as usize;
            return estimated_col.min(text.chars().count());
        }

        // For shorter lines, use binary search on character positions
        let chars: Vec<char> = text.chars().collect();
        let mut left = 0;
        let mut right = chars.len();

        while left < right {
            let mid = (left + right) / 2;

            // Measure the width from start to this character position
            let prefix: String = chars[0..mid].iter().collect();
            let layout = self.layout_text_scaled(&prefix, font_size, scale);
            let mid_x = layout.width / scale; // Convert to logical pixels

            if mid_x < x_logical {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        left
    }
}

/// Convert layout to GPU instances with optional text effects
pub fn create_glyph_instances(
    font_system: &SharedFontSystem,
    text: &str,
    pos: LayoutPos,
    font_size: f32,
    scale_factor: f32,
    line_height: f32,
    effects: Option<&[tiny_sdk::services::TextEffect]>,
    original_byte_offset: usize,
) -> Vec<GlyphInstance> {
    let lines: Vec<&str> = text.lines().collect();
    let mut all_glyph_instances = Vec::new();
    let mut y_offset = 0.0;
    let mut global_byte_pos = 0;

    for line_text in lines.iter() {
        // Layout this single line
        let layout = font_system.layout_text_scaled(line_text, font_size, scale_factor);

        let mut byte_pos = 0;
        for glyph in &layout.glyphs {
            let mut found_token_id = None;

            // Apply text effects if available
            if let Some(effects) = effects {
                let char_bytes = glyph.char.len_utf8();
                let doc_pos = original_byte_offset + global_byte_pos + byte_pos;

                for effect in effects {
                    if effect.range.start <= doc_pos && doc_pos < effect.range.end {
                        if let tiny_sdk::services::TextEffectType::Token(token_id) = effect.effect {
                            found_token_id = Some(token_id);
                            break;
                        }
                    }
                }
                byte_pos += char_bytes;
            }

            // Convert from physical to logical pixels and add position
            let glyph_logical_x = glyph.pos.x.0 / scale_factor;
            let glyph_logical_y = glyph.pos.y.0 / scale_factor;

            all_glyph_instances.push(GlyphInstance {
                pos: LayoutPos::new(
                    pos.x.0 + glyph_logical_x,
                    pos.y.0 + y_offset + glyph_logical_y,
                ),
                tex_coords: glyph.tex_coords,
                token_id: found_token_id.unwrap_or(0),
                relative_pos: 0.0,
                shader_id: None,
            });
        }

        global_byte_pos += line_text.len() + 1; // +1 for newline
        y_offset += line_height;
    }

    all_glyph_instances
}

// === FontService Implementation ===

impl FontService for SharedFontSystem {
    fn layout_text(&self, text: &str, font_size: f32) -> SdkTextLayout {
        let layout = self.inner.lock().layout_text(text, font_size);

        // Convert to SDK types
        let glyphs = layout
            .glyphs
            .iter()
            .map(|g| SdkPositionedGlyph {
                char: g.char,
                pos: LayoutPos::new(g.pos.x.0, g.pos.y.0),
                size: g.size.clone(),
                tex_coords: g.tex_coords,
                color: g.color,
            })
            .collect();

        SdkTextLayout {
            glyphs,
            width: layout.width,
            height: layout.height,
        }
    }

    fn layout_text_scaled(&self, text: &str, font_size: f32, scale_factor: f32) -> SdkTextLayout {
        let layout = self
            .inner
            .lock()
            .layout_text_scaled(text, font_size, scale_factor);

        // Convert to SDK types
        let glyphs = layout
            .glyphs
            .iter()
            .map(|g| SdkPositionedGlyph {
                char: g.char,
                pos: LayoutPos::new(g.pos.x.0, g.pos.y.0),
                size: g.size.clone(),
                tex_coords: g.tex_coords,
                color: g.color,
            })
            .collect();

        SdkTextLayout {
            glyphs,
            width: layout.width,
            height: layout.height,
        }
    }

    fn char_width_coef(&self) -> f32 {
        self.inner.lock().char_width_coef()
    }

    fn atlas_data(&self) -> Vec<u8> {
        self.inner.lock().atlas_data.clone()
    }

    fn atlas_size(&self) -> (u32, u32) {
        self.inner.lock().atlas_size()
    }

    fn prerasterize_ascii(&self, font_size_px: f32) {
        self.inner.lock().prerasterize_ascii(font_size_px);
    }

    fn hit_test_line(
        &self,
        line_text: &str,
        font_size: f32,
        scale_factor: f32,
        target_x: f32,
    ) -> u32 {
        self.hit_test_line(line_text, font_size, scale_factor, target_x)
    }
}
