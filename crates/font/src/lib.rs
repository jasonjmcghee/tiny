//! Consolidated font system - single source of truth for text rendering
//!
//! Combines font loading, text shaping (swash), glyph rasterization, and atlas management
//!
//! ## Emoji Support
//!
//! **Current Status:** Emojis will render as placeholder boxes ("bentos")
//!
//! **Why:** JetBrains Mono is a programming font without emoji glyphs
//!
//! **Solution:** Add emoji fallback font (see `emoji.rs` for implementation notes)

mod cluster_map;
mod emoji;
mod rasterize;
mod shaping;

pub use cluster_map::ClusterMap;
pub use emoji::{contains_emoji, is_emoji};
pub use rasterize::{FontMetrics, RasterResult, Rasterizer};
pub use shaping::{ShapedGlyph, Shaper, ShapingOptions, ShapingResult, TextRun};

use ahash::HashMap;
use parking_lot::Mutex;
use std::sync::Arc;
use swash::FontRef;
use tiny_sdk::services::{
    FontService, PositionedGlyph as SdkPositionedGlyph, TextLayout as SdkTextLayout,
};
use tiny_sdk::types::{LayoutPos, PhysicalPos, PhysicalSizeF};

/// Helper to expand tabs to spaces
fn expand_tabs(text: &str) -> String {
    const TAB_WIDTH: usize = 4;
    const SPACES: &str = "    "; // Pre-allocated 4 spaces

    let mut result = String::with_capacity(text.len() + (text.len() / 8)); // Estimate extra space for tabs
    let mut column = 0;

    for ch in text.chars() {
        if ch == '\t' {
            let spaces_needed = TAB_WIDTH - (column % TAB_WIDTH);
            result.push_str(&SPACES[..spaces_needed]);
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
    /// Atlas index: 0 = monochrome (R8), 1 = color (RGBA8)
    pub atlas_index: u8,
}

/// Layout result with positioned glyphs
pub struct TextLayout {
    pub glyphs: Vec<PositionedGlyph>,
    pub width: f32,
    pub height: f32,
}

/// Layout result with shaped glyphs and cluster map
pub struct ShapedTextLayout {
    pub glyphs: Vec<PositionedGlyph>,
    pub width: f32,
    pub height: f32,
    pub cluster_map: ClusterMap,
}

/// Consolidated font system - handles everything font-related
pub struct FontSystem {
    /// Font data for swash (kept alive for FontRef)
    #[allow(dead_code)]
    font_data: Arc<Vec<u8>>,
    /// Swash font reference (offset 0 = main font)
    font_ref: FontRef<'static>,
    /// Emoji fallback font data (kept alive)
    #[allow(dead_code)]
    emoji_font_data: Option<Arc<Vec<u8>>>,
    /// Emoji font reference for color emoji rendering
    emoji_font_ref: Option<FontRef<'static>>,
    /// Text shaper
    shaper: Shaper,
    /// Glyph rasterizer
    rasterizer: Rasterizer,
    /// Atlas texture data (R8 format for monochrome glyphs)
    atlas_data: Vec<u8>,
    /// Color atlas texture data (RGBA8 format for color emojis)
    color_atlas_data: Vec<u8>,
    /// Atlas dimensions (same for both atlases)
    atlas_size: (u32, u32),
    /// Cache of rasterized glyphs: (glyph_id, size_in_pixels, is_emoji) -> (tex_coords, metrics)
    /// Changed from char to glyph_id to support shaped glyphs
    /// Added is_emoji flag to distinguish between main and emoji font glyphs
    glyph_cache: HashMap<(u16, u32, bool), GlyphEntry>,
    /// Current atlas cursor (monochrome)
    next_x: u32,
    next_y: u32,
    row_height: u32,
    /// Current color atlas cursor
    color_next_x: u32,
    color_next_y: u32,
    color_row_height: u32,
    /// Approx character width coefficient to multiply by the font for fast calculations
    char_width_coef: f32,
    /// Dirty flag - set when monochrome atlas changes and needs GPU upload
    dirty: bool,
    /// Dirty flag for color atlas
    color_dirty: bool,
    /// Default shaping options
    shaping_options: ShapingOptions,
    /// Current frame counter for LRU eviction
    current_frame: u64,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
struct GlyphEntry {
    tex_coords: [f32; 4],
    width: f32,
    height: f32,
    advance: f32,
    bearing_x: f32,
    bearing_y: f32,
    /// Whether this glyph is in the color atlas (true) or monochrome atlas (false)
    pub is_color: bool,
    /// Atlas index: 0 = monochrome (R8), 1 = color (RGBA8)
    pub atlas_index: u8,
    /// Last access frame for LRU eviction
    pub access_order: u64,
}

impl FontSystem {
    /// Evict least recently used glyphs and rebuild atlas
    /// Called when atlas overflows
    fn evict_lru_glyphs(&mut self) {
        // Sort cache entries by access order and collect keys to evict
        let mut entries: Vec<_> = self.glyph_cache.iter()
            .map(|(k, v)| (*k, v.access_order))
            .collect();
        entries.sort_by_key(|(_, access_order)| *access_order);

        // Evict oldest 25%
        let evict_count = entries.len() / 4;
        let evict_count = evict_count.max(1); // Always evict at least one

        eprintln!("⚠️  Font atlas overflow! Evicting {} oldest glyphs ({}% of cache)",
                  evict_count, (evict_count * 100) / entries.len().max(1));

        // Collect keys to evict
        let keys_to_evict: Vec<_> = entries.iter()
            .take(evict_count)
            .map(|(k, _)| *k)
            .collect();

        // Now evict them
        for key in &keys_to_evict {
            self.glyph_cache.remove(key);
        }

        // Clear both atlases
        self.atlas_data.fill(0);
        self.color_atlas_data.fill(0);

        // Reset atlas cursors
        self.next_x = 0;
        self.next_y = 0;
        self.row_height = 0;
        self.color_next_x = 0;
        self.color_next_y = 0;
        self.color_row_height = 0;

        // Rebuild atlas from remaining cache entries
        // We need to re-rasterize all glyphs to rebuild the atlas
        let remaining_keys: Vec<_> = self.glyph_cache.keys().copied().collect();
        self.glyph_cache.clear();

        // Copy font refs before the loop to avoid borrow checker issues
        let main_font_ref = self.font_ref;
        let emoji_font_ref = self.emoji_font_ref;

        // Re-rasterize each remaining glyph
        // This is inefficient but keeps the logic simple
        for (glyph_id, size_px, is_from_emoji_font) in remaining_keys {
            let font_ref = if is_from_emoji_font && emoji_font_ref.is_some() {
                emoji_font_ref.as_ref().unwrap()
            } else {
                &main_font_ref
            };

            // Re-rasterize this glyph (will be added back to cache)
            let _entry = self.get_or_rasterize_glyph_with_font(
                glyph_id,
                size_px,
                font_ref,
                is_from_emoji_font,
            );
        }

        // Mark both atlases as dirty
        self.dirty = true;
        self.color_dirty = true;
    }

    /// Load system emoji font (platform-specific)
    fn load_emoji_font() -> (Option<Arc<Vec<u8>>>, Option<FontRef<'static>>) {
        #[cfg(target_os = "macos")]
        {
            let emoji_path = "/System/Library/Fonts/Apple Color Emoji.ttc";
            if let Ok(emoji_data_vec) = std::fs::read(emoji_path) {
                let emoji_data = Arc::new(emoji_data_vec);

                // SAFETY: Leak the Arc to get a 'static reference
                let emoji_data_ref: &'static [u8] = unsafe {
                    std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len())
                };

                // Apple Color Emoji is a TrueType Collection (.ttc), try index 0
                if let Some(emoji_font_ref) = FontRef::from_index(emoji_data_ref, 0) {
                    eprintln!("✅ Loaded Apple Color Emoji font");
                    return (Some(emoji_data), Some(emoji_font_ref));
                } else {
                    eprintln!("⚠️ Failed to parse Apple Color Emoji font");
                }
            } else {
                eprintln!("⚠️ Could not load Apple Color Emoji from {}", emoji_path);
            }
        }

        #[cfg(target_os = "windows")]
        {
            let emoji_path = "C:\\Windows\\Fonts\\seguiemj.ttf";
            if let Ok(emoji_data_vec) = std::fs::read(emoji_path) {
                let emoji_data = Arc::new(emoji_data_vec);
                let emoji_data_ref: &'static [u8] = unsafe {
                    std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len())
                };

                if let Some(emoji_font_ref) = FontRef::from_index(emoji_data_ref, 0) {
                    eprintln!("✅ Loaded Segoe UI Emoji font");
                    return (Some(emoji_data), Some(emoji_font_ref));
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            let emoji_path = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
            if let Ok(emoji_data_vec) = std::fs::read(emoji_path) {
                let emoji_data = Arc::new(emoji_data_vec);
                let emoji_data_ref: &'static [u8] = unsafe {
                    std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len())
                };

                if let Some(emoji_font_ref) = FontRef::from_index(emoji_data_ref, 0) {
                    eprintln!("✅ Loaded Noto Color Emoji font");
                    return (Some(emoji_data), Some(emoji_font_ref));
                }
            }
        }

        eprintln!("⚠️ No emoji font loaded - emojis will render as placeholders");
        (None, None)
    }

    /// Create new font system
    pub fn new() -> Self {
        let font_data_static = include_bytes!("../assets/JetBrainsMonoNerdFont-Regular.ttf");

        // Create Arc for swash (needs owned data)
        let font_data = Arc::new(font_data_static.to_vec());

        // SAFETY: We leak the Arc to get a 'static reference
        // This is safe because the font data never changes and lives for the program lifetime
        let font_data_ref: &'static [u8] = unsafe {
            std::slice::from_raw_parts(
                font_data.as_ptr(),
                font_data.len(),
            )
        };

        // Create swash FontRef
        let font_ref = FontRef::from_index(font_data_ref, 0)
            .expect("Failed to load font for swash");

        // Load emoji font for fallback
        let (emoji_font_data, emoji_font_ref) = Self::load_emoji_font();

        let mut shaper = Shaper::new();
        let rasterizer = Rasterizer::new();
        let atlas_size = (2048, 2048);
        let atlas_data = vec![0; (atlas_size.0 * atlas_size.1) as usize];
        // Color atlas is RGBA8, so 4 bytes per pixel
        let color_atlas_data = vec![0; (atlas_size.0 * atlas_size.1 * 4) as usize];

        // Get the monospace advance width by shaping a single character
        // In a monospace font, all characters should have the same advance
        let test_size = 16.0;
        let test_opts = ShapingOptions::with_size(test_size);
        let shaped = shaper.shape(&font_ref, "M", &test_opts);
        let char_width_coef = if let Some(glyph) = shaped.glyphs.first() {
            glyph.x_advance / test_size
        } else {
            0.6 // Fallback coefficient
        };

        Self {
            font_data,
            font_ref,
            emoji_font_data,
            emoji_font_ref,
            shaper,
            rasterizer,
            atlas_data,
            color_atlas_data,
            atlas_size,
            glyph_cache: HashMap::default(),
            next_x: 0,
            next_y: 0,
            row_height: 0,
            color_next_x: 0,
            color_next_y: 0,
            color_row_height: 0,
            char_width_coef,
            dirty: true,       // Initially dirty - needs upload
            color_dirty: true, // Color atlas also needs initial upload
            shaping_options: ShapingOptions::default(),
            current_frame: 0,
        }
    }

    /// Layout text and get positioned glyphs with texture coordinates
    /// font_size should already include scale_factor (e.g., 14.0 * 2.0 = 28.0 for retina)
    /// Returns positions scaled back to logical coordinates (14pt scale)
    ///
    /// For multi-line text, pass physical_line_height. If None, uses font metrics.
    pub fn layout_text_with_line_height(
        &mut self,
        text: &str,
        font_size_px: f32,
        physical_line_height: Option<f32>,
    ) -> TextLayout {
        // Handle multi-line text by laying out each line separately
        let lines: Vec<&str> = text.lines().collect();

        // Get line height - either provided or calculated from font metrics
        let line_height = if let Some(lh) = physical_line_height {
            lh
        } else {
            let font_metrics = self.rasterizer.get_font_metrics(&self.font_ref, font_size_px);
            // Swash returns descent as absolute value (positive), so we ADD it
            font_metrics.ascent + font_metrics.descent + font_metrics.leading
        };

        let mut all_glyphs = Vec::new();
        let mut max_width = 0.0f32;
        let mut current_y = 0.0f32;

        for line in lines {
            // Shape this line
            let shaped = self.layout_text_shaped_with_tabs(line, font_size_px, None);

            // Add glyphs with Y offset for this line
            for mut glyph in shaped.glyphs {
                glyph.pos.y.0 += current_y;
                all_glyphs.push(glyph);
            }

            max_width = max_width.max(shaped.width);
            current_y += line_height;
        }

        TextLayout {
            glyphs: all_glyphs,
            width: max_width,
            height: current_y,
        }
    }

    /// Layout text and get positioned glyphs with texture coordinates
    /// Uses font metrics to calculate line height
    pub fn layout_text(&mut self, text: &str, font_size_px: f32) -> TextLayout {
        self.layout_text_with_line_height(text, font_size_px, None)
    }

    /// Shape text with tab expansion and automatic emoji font fallback
    pub fn shape_text_with_tabs(
        &mut self,
        text: &str,
        font_size_px: f32,
        options: Option<&ShapingOptions>,
    ) -> (ShapingResult, ClusterMap, String) {
        let opts = options.unwrap_or(&self.shaping_options);
        let mut opts_with_size = opts.clone();
        opts_with_size.font_size = font_size_px;

        // Expand tabs manually
        let expanded = expand_tabs(text);

        // If we have emoji font, segment text into runs
        if self.emoji_font_ref.is_some() && contains_emoji(&expanded) {
            // Segment into emoji and non-emoji runs
            let runs = Shaper::segment_text(&expanded);

            let mut all_glyphs = Vec::new();
            let mut total_advance = 0.0;

            // Shape each run with appropriate font
            for run in &runs {
                let font_for_run = if run.use_emoji_font {
                    self.emoji_font_ref.as_ref().unwrap()
                } else {
                    &self.font_ref
                };

                let mut run_result = self.shaper.shape(font_for_run, &run.text, &opts_with_size);

                // Adjust cluster positions to be relative to full text
                for glyph in &mut run_result.glyphs {
                    glyph.cluster += run.byte_range.start as u32;
                }

                total_advance += run_result.advance;
                all_glyphs.extend(run_result.glyphs);
            }

            let result = ShapingResult {
                glyphs: all_glyphs,
                advance: total_advance,
            };

            let cluster_map = ClusterMap::from_glyphs(&result.glyphs, expanded.len());
            return (result, cluster_map, expanded);
        }

        // No emoji font or no emojis - use main font only
        let result = self.shaper.shape(&self.font_ref, &expanded, &opts_with_size);
        let cluster_map = ClusterMap::from_glyphs(&result.glyphs, expanded.len());

        (result, cluster_map, expanded)
    }

    /// Get or rasterize a glyph by glyph ID at physical pixel size
    /// font_ref: which font to use for rasterization
    /// is_from_emoji_font: whether this glyph came from emoji font shaping
    fn get_or_rasterize_glyph_with_font(
        &mut self,
        glyph_id: u16,
        size_px: u32,
        font_ref: &FontRef<'static>,
        is_from_emoji_font: bool,
    ) -> GlyphEntry {
        let key = (glyph_id, size_px, is_from_emoji_font);

        // Check cache first
        if let Some(entry) = self.glyph_cache.get_mut(&key) {
            // Update access order for LRU
            entry.access_order = self.current_frame;
            return *entry;
        }

        // Increment frame counter for new rasterizations
        self.current_frame += 1;

        // Check if this is a color glyph
        let is_color = self.rasterizer.is_color_glyph(font_ref, glyph_id);

        // Rasterize using swash with appropriate font
        let raster_result = self
            .rasterizer
            .rasterize(font_ref, glyph_id, size_px as f32)
            .unwrap_or_else(|| {
                panic!(
                    "Failed to rasterize glyph_id={} at size={}px with font (is_emoji={})",
                    glyph_id, size_px, is_from_emoji_font
                );
            });

        let bitmap = &raster_result.bitmap;
        let width = raster_result.width;
        let height = raster_result.height;

        // Choose atlas based on whether this is a color glyph
        let (next_x, next_y, row_height, atlas_index) = if is_color {
            (&mut self.color_next_x, &mut self.color_next_y, &mut self.color_row_height, 1u8)
        } else {
            (&mut self.next_x, &mut self.next_y, &mut self.row_height, 0u8)
        };

        // Check if glyph fits in current row
        if *next_x + width > self.atlas_size.0 {
            *next_x = 0;
            *next_y += *row_height;
            *row_height = 0;
        }

        // If atlas is full, evict LRU glyphs and retry
        if *next_y + height > self.atlas_size.1 {
            self.evict_lru_glyphs();
            // Retry rasterization after eviction
            return self.get_or_rasterize_glyph_with_font(
                glyph_id,
                size_px,
                font_ref,
                is_from_emoji_font,
            );
        }

        // Copy bitmap to appropriate atlas
        if is_color {
            // Color atlas is RGBA8
            // Check if bitmap is already RGBA or needs expansion from grayscale
            let bitmap_bytes_per_pixel = bitmap.len() / ((width * height).max(1)) as usize;

            for y in 0..height {
                for x in 0..width {
                    let atlas_idx =
                        ((*next_y + y) * self.atlas_size.0 + (*next_x + x)) as usize * 4;

                    if atlas_idx + 3 < self.color_atlas_data.len() {
                        if bitmap_bytes_per_pixel >= 4 {
                            // Already RGBA - copy directly
                            let bitmap_idx = (y * width + x) as usize * 4;
                            if bitmap_idx + 3 < bitmap.len() {
                                self.color_atlas_data[atlas_idx] = bitmap[bitmap_idx];
                                self.color_atlas_data[atlas_idx + 1] = bitmap[bitmap_idx + 1];
                                self.color_atlas_data[atlas_idx + 2] = bitmap[bitmap_idx + 2];
                                self.color_atlas_data[atlas_idx + 3] = bitmap[bitmap_idx + 3];
                            }
                        } else {
                            // Grayscale - expand to RGBA (white with alpha)
                            let bitmap_idx = (y * width + x) as usize;
                            if bitmap_idx < bitmap.len() {
                                let alpha = bitmap[bitmap_idx];
                                self.color_atlas_data[atlas_idx] = 255;     // R
                                self.color_atlas_data[atlas_idx + 1] = 255; // G
                                self.color_atlas_data[atlas_idx + 2] = 255; // B
                                self.color_atlas_data[atlas_idx + 3] = alpha; // A
                            }
                        }
                    }
                }
            }
            self.color_dirty = true;
        } else {
            // Monochrome atlas is R8
            for y in 0..height {
                for x in 0..width {
                    let atlas_idx =
                        ((*next_y + y) * self.atlas_size.0 + (*next_x + x)) as usize;
                    let bitmap_idx = (y * width + x) as usize;
                    if atlas_idx < self.atlas_data.len() && bitmap_idx < bitmap.len() {
                        self.atlas_data[atlas_idx] = bitmap[bitmap_idx];
                    }
                }
            }
            self.dirty = true;
        }

        // Calculate texture coordinates
        let u0 = *next_x as f32 / self.atlas_size.0 as f32;
        let v0 = *next_y as f32 / self.atlas_size.1 as f32;
        let u1 = (*next_x + width) as f32 / self.atlas_size.0 as f32;
        let v1 = (*next_y + height) as f32 / self.atlas_size.1 as f32;

        let entry = GlyphEntry {
            tex_coords: [u0, v0, u1, v1],
            width: width as f32,
            height: height as f32,
            advance: raster_result.advance,
            bearing_x: raster_result.bearing_x,
            bearing_y: raster_result.bearing_y,
            is_color,
            atlas_index,
            access_order: self.current_frame,
        };

        // Update atlas position
        *next_x += width + 1; // 1px padding
        *row_height = (*row_height).max(height + 1);

        // Cache and return
        self.glyph_cache.insert(key, entry);
        entry
    }

    /// Get or rasterize a glyph by glyph ID at physical pixel size (main font)
    fn get_or_rasterize_glyph(&mut self, glyph_id: u16, size_px: u32) -> GlyphEntry {
        let font_ref = self.font_ref; // Copy FontRef to avoid borrow issues
        self.get_or_rasterize_glyph_with_font(glyph_id, size_px, &font_ref, false)
    }


    /// Layout text using swash shaping with tab expansion
    pub fn layout_text_shaped_with_tabs(
        &mut self,
        text: &str,
        font_size_px: f32,
        options: Option<&ShapingOptions>,
    ) -> ShapedTextLayout {
        // Get font metrics from main font for baseline
        let main_font_metrics = self.rasterizer.get_font_metrics(&self.font_ref, font_size_px);
        let baseline_y = main_font_metrics.ascent;

        // Expand tabs
        let expanded = expand_tabs(text);

        // Segment into runs if we have emoji font and text contains emojis
        let has_emoji_font = self.emoji_font_ref.is_some();
        let has_emojis = contains_emoji(&expanded);

        if has_emoji_font && has_emojis {
            // Segment text into emoji and non-emoji runs
            let runs = Shaper::segment_text(&expanded);

            let opts = options.unwrap_or(&self.shaping_options);
            let mut opts_with_size = opts.clone();
            opts_with_size.font_size = font_size_px;

            let mut all_shaped_glyphs = Vec::new();
            let mut positioned_glyphs = Vec::new();
            let mut pen_x = 0.0f32;
            let mut max_y = 0.0f32;

            // Shape and position each run
            for run in &runs {
                // For emoji runs, check if the emoji font actually has the glyphs
                // If not, fall back to main font
                let (font_ref, is_emoji_run) = if run.use_emoji_font {
                    let emoji_font = self.emoji_font_ref.as_ref().unwrap();
                    // Check if emoji font has all the glyphs in this run
                    let emoji_has_glyphs = run.text.chars().all(|ch| {
                        emoji_font.charmap().map(ch) != 0
                    });

                    if emoji_has_glyphs {
                        // Use emoji font
                        (*emoji_font, true)
                    } else {
                        // Emoji font doesn't have these glyphs, use main font
                        (self.font_ref, false)
                    }
                } else {
                    // Not an emoji run, use main font
                    (self.font_ref, false)
                };

                let mut run_result = self.shaper.shape(&font_ref, &run.text, &opts_with_size);

                // Adjust cluster positions to be relative to full text
                for glyph in &mut run_result.glyphs {
                    glyph.cluster += run.byte_range.start as u32;
                }

                // Position glyphs in this run
                for shaped_glyph in &run_result.glyphs {
                    let entry = self.get_or_rasterize_glyph_with_font(
                        shaped_glyph.glyph_id,
                        font_size_px as u32,
                        &font_ref,
                        is_emoji_run,
                    );

                    // Get character from expanded text (cluster is byte offset in expanded)
                    let ch = if shaped_glyph.cluster < expanded.len() as u32 {
                        expanded[shaped_glyph.cluster as usize..]
                            .chars()
                            .next()
                            .unwrap_or('?')
                    } else {
                        '?'
                    };

                    let x = pen_x + shaped_glyph.x_offset + entry.bearing_x;
                    let y = baseline_y - entry.bearing_y + shaped_glyph.y_offset;

                    positioned_glyphs.push(PositionedGlyph {
                        char: ch,
                        pos: PhysicalPos::new(x, y),
                        size: PhysicalSizeF::new(entry.width, entry.height),
                        tex_coords: entry.tex_coords,
                        color: 0xE1E1E1FF,
                        atlas_index: entry.atlas_index,
                    });

                    pen_x += shaped_glyph.x_advance;
                    max_y = max_y.max(y + entry.height);
                }

                all_shaped_glyphs.extend(run_result.glyphs);
            }

            let cluster_map = ClusterMap::from_glyphs(&all_shaped_glyphs, expanded.len());

            return ShapedTextLayout {
                glyphs: positioned_glyphs,
                width: pen_x,
                height: max_y,
                cluster_map,
            };
        }

        // No emoji font or no emojis - use main font only (fast path)
        let shaped_font_ref = self.font_ref;

        // Shape directly with main font
        let opts = options.unwrap_or(&self.shaping_options);
        let mut opts_with_size = opts.clone();
        opts_with_size.font_size = font_size_px;

        let shaping_result = self.shaper.shape(&self.font_ref, &expanded, &opts_with_size);
        let cluster_map = ClusterMap::from_glyphs(&shaping_result.glyphs, expanded.len());

        let mut positioned_glyphs = Vec::new();
        let mut pen_x = 0.0f32;
        let mut max_y = 0.0f32;

        // Position each shaped glyph
        for shaped_glyph in &shaping_result.glyphs {
            let entry = self.get_or_rasterize_glyph_with_font(
                shaped_glyph.glyph_id,
                font_size_px as u32,
                &shaped_font_ref,
                false, // Main font
            );

            // Calculate position
            let x = pen_x + shaped_glyph.x_offset + entry.bearing_x;
            let y = baseline_y - entry.bearing_y + shaped_glyph.y_offset;

            // Get character from expanded text (cluster is byte offset in expanded)
            let ch = if shaped_glyph.cluster < expanded.len() as u32 {
                expanded[shaped_glyph.cluster as usize..]
                    .chars()
                    .next()
                    .unwrap_or('?')
            } else {
                '?'
            };

            positioned_glyphs.push(PositionedGlyph {
                char: ch,
                pos: PhysicalPos::new(x, y),
                size: PhysicalSizeF::new(entry.width, entry.height),
                tex_coords: entry.tex_coords,
                color: 0xE1E1E1FF,
                atlas_index: entry.atlas_index,
            });

            pen_x += shaped_glyph.x_advance;
            max_y = max_y.max(y + entry.height);
        }

        ShapedTextLayout {
            glyphs: positioned_glyphs,
            width: pen_x,
            height: max_y,
            cluster_map,
        }
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
        let charmap = self.font_ref.charmap();
        for ch in ' '..='~' {
            let glyph_id = charmap.map(ch);
            if glyph_id != 0 {
                self.get_or_rasterize_glyph(glyph_id, font_size_px as u32);
            }
        }
    }

    /// Get atlas data for GPU upload
    pub fn atlas_data(&self) -> &[u8] {
        &self.atlas_data
    }

    /// Get color atlas data for GPU upload
    pub fn color_atlas_data(&self) -> &[u8] {
        &self.color_atlas_data
    }

    /// Get atlas dimensions
    pub fn atlas_size(&self) -> (u32, u32) {
        self.atlas_size
    }

    pub fn char_width_coef(&self) -> f32 {
        self.char_width_coef
    }

    /// Clear the glyph cache and reset atlas positions
    /// This should be called when font size changes to prevent atlas overflow
    pub fn clear_cache(&mut self) {
        self.glyph_cache.clear();
        self.next_x = 0;
        self.next_y = 0;
        self.row_height = 0;
        self.color_next_x = 0;
        self.color_next_y = 0;
        self.color_row_height = 0;
        // Clear both atlases
        self.atlas_data.fill(0);
        self.color_atlas_data.fill(0);
        // Mark both as dirty - needs GPU upload
        self.dirty = true;
        self.color_dirty = true;
    }

    /// Check if monochrome atlas needs GPU upload
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Check if color atlas needs GPU upload
    pub fn is_color_dirty(&self) -> bool {
        self.color_dirty
    }

    /// Clear dirty flag after GPU upload
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Clear color dirty flag after GPU upload
    pub fn clear_color_dirty(&mut self) {
        self.color_dirty = false;
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
                    atlas_index: g.atlas_index,
                })
                .collect(),
            width: layout.width,
            height: layout.height,
        }
    }

    /// Layout text with explicit line height (in logical pixels)
    pub fn layout_text_with_line_height(
        &self,
        text: &str,
        logical_font_size: f32,
        scale_factor: f32,
        logical_line_height: f32,
    ) -> TextLayout {
        let physical_size = logical_font_size * scale_factor;
        let physical_line_height = logical_line_height * scale_factor;
        self.inner
            .lock()
            .layout_text_with_line_height(text, physical_size, Some(physical_line_height))
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

    /// Get color atlas data
    pub fn color_atlas_data(&self) -> Vec<u8> {
        self.inner.lock().color_atlas_data.clone()
    }

    /// Get atlas size
    pub fn atlas_size(&self) -> (u32, u32) {
        self.inner.lock().atlas_size()
    }

    /// Check if monochrome atlas needs GPU upload
    pub fn is_dirty(&self) -> bool {
        self.inner.lock().is_dirty()
    }

    /// Check if color atlas needs GPU upload
    pub fn is_color_dirty(&self) -> bool {
        self.inner.lock().is_color_dirty()
    }

    /// Clear dirty flag after GPU upload
    pub fn clear_dirty(&self) {
        self.inner.lock().clear_dirty()
    }

    /// Clear color dirty flag after GPU upload
    pub fn clear_color_dirty(&self) {
        self.inner.lock().clear_color_dirty()
    }

    /// Hit test with cluster-aware positioning for ligatures
    /// Returns a byte position that's guaranteed to be at a cluster boundary
    pub fn hit_test_line_shaped(
        &self,
        line_text: &str,
        logical_font_size: f32,
        scale_factor: f32,
        target_x: f32,
    ) -> u32 {
        if line_text.is_empty() {
            return 0;
        }

        // Use shaped layout to get cluster map
        let shaped = self.layout_text_shaped_with_tabs(
            line_text,
            logical_font_size,
            scale_factor,
            None, // Use default shaping options
        );

        if shaped.glyphs.is_empty() {
            return 0;
        }

        let target_x_physical = target_x * scale_factor;

        // Binary search through glyphs
        let mut left = 0;
        let mut right = shaped.glyphs.len();

        while left < right {
            let mid = (left + right) / 2;
            let glyph = &shaped.glyphs[mid];
            let glyph_center = glyph.pos.x.0 + glyph.size.width.0 / 2.0;

            if glyph_center <= target_x_physical {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        // Get byte position from glyph index using cluster map
        let byte_pos = if left < shaped.glyphs.len() {
            // Get byte position from cluster map
            shaped
                .cluster_map
                .glyph_to_byte(left)
                .unwrap_or(line_text.len())
        } else {
            line_text.len()
        };

        // Snap to cluster boundary to ensure cursor is in a valid position
        let snapped_byte = shaped.cluster_map.snap_to_cluster_boundary(byte_pos);

        // Convert byte position to character position
        line_text[..snapped_byte].chars().count() as u32
    }

    /// Hit test: find character position at x coordinate using binary search
    /// Uses the FULL line layout to get accurate positioning with kerning/ligatures
    /// Legacy version - prefer hit_test_line_shaped for ligature support
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

    /// Layout text with swash shaping and tab expansion
    pub fn layout_text_shaped_with_tabs(
        &self,
        text: &str,
        logical_font_size: f32,
        scale_factor: f32,
        options: Option<&ShapingOptions>,
    ) -> ShapedTextLayout {
        let physical_size = logical_font_size * scale_factor;
        self.inner
            .lock()
            .layout_text_shaped_with_tabs(text, physical_size, options)
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
