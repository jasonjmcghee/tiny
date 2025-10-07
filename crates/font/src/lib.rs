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
pub use emoji::{
    contains_emoji, contains_nerd_symbols, contains_special_chars, is_emoji, is_nerd_font_symbol,
    is_variation_selector,
};
pub use rasterize::{FontMetrics, RasterResult, Rasterizer};
pub use shaping::{RunFontType, ShapedGlyph, Shaper, ShapingOptions, ShapingResult, TextRun};

use ahash::HashMap;
use parking_lot::Mutex;
use std::sync::Arc;
use swash::FontRef;
use tiny_sdk::services::{
    FontService, PositionedGlyph as SdkPositionedGlyph, TextLayout as SdkTextLayout,
};
use tiny_sdk::types::{LayoutPos, PhysicalPos, PhysicalSizeF};

/// Helper to expand tabs to spaces and filter variation selectors
fn expand_tabs(text: &str) -> String {
    const TAB_WIDTH: usize = 4;
    const SPACES: &str = "    "; // Pre-allocated 4 spaces

    let mut result = String::with_capacity(text.len() + (text.len() / 8)); // Estimate extra space for tabs
    let mut column = 0;

    for ch in text.chars() {
        // Skip variation selectors - they don't render as separate glyphs
        if is_variation_selector(ch) {
            continue;
        }

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
    /// Regular variable font data (kept alive for FontRef)
    #[allow(dead_code)]
    regular_font_data: Arc<Vec<u8>>,
    /// Regular variable font reference
    regular_font_ref: FontRef<'static>,
    /// Italic variable font data (kept alive for FontRef)
    #[allow(dead_code)]
    italic_font_data: Arc<Vec<u8>>,
    /// Italic variable font reference
    italic_font_ref: FontRef<'static>,
    /// Nerd font data for glyphs (kept alive)
    #[allow(dead_code)]
    nerd_font_data: Arc<Vec<u8>>,
    /// Nerd font reference for glyph fallback
    nerd_font_ref: FontRef<'static>,
    /// System Unicode fallback font data (kept alive)
    #[allow(dead_code)]
    system_fallback_data: Option<Arc<Vec<u8>>>,
    /// System Unicode fallback font for missing symbols
    system_fallback_ref: Option<FontRef<'static>>,
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
    /// Cache of rasterized glyphs: (glyph_id, size_px, italic, weight_quantized, font_source) -> GlyphEntry
    /// - glyph_id: Font-specific glyph identifier
    /// - size_px: Pixel size (already includes scale factor)
    /// - italic: Whether italic variant is used
    /// - weight_quantized: Font weight quantized to nearest 100 (e.g., 400, 700)
    /// - font_source: Which font the glyph came from (Main/Nerd/Emoji)
    glyph_cache: HashMap<(u16, u32, bool, u16, FontSource), GlyphEntry>,
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

impl FontSystem {
    /// Set default font weight (100-900)
    pub fn set_default_weight(&mut self, weight: f32) {
        let clamped = weight.clamp(100.0, 900.0);
        eprintln!("🔧 FontSystem: Setting default weight to {}", clamped);
        self.shaping_options.weight = clamped;
        // DON'T clear cache - let it naturally pick up new glyphs
        // Clearing causes a flash as the entire atlas is wiped
        // Old glyphs will be replaced as text is re-laid out with new weight
    }

    /// Set default italic setting
    pub fn set_default_italic(&mut self, italic: bool) {
        eprintln!("🔧 FontSystem: Setting default italic to {}", italic);
        self.shaping_options.italic = italic;
        // DON'T clear cache - let it naturally pick up new glyphs
        // Clearing causes a flash as the entire atlas is wiped
        // Old glyphs will be replaced as text is re-laid out with new italic
    }

    /// Get current default weight
    pub fn default_weight(&self) -> f32 {
        self.shaping_options.weight
    }

    /// Get current default italic
    pub fn default_italic(&self) -> bool {
        self.shaping_options.italic
    }
}

/// Font source for glyph rendering
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum FontSource {
    /// Regular or italic main font
    Main,
    /// Nerd font for special glyphs
    Nerd,
    /// System fallback font for Unicode symbols
    SystemFallback,
    /// Emoji font for emoji
    Emoji,
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
    /// Select the main font (regular or italic) based on shaping options
    fn select_main_font(&self, italic: bool) -> &FontRef<'static> {
        if italic {
            &self.italic_font_ref
        } else {
            &self.regular_font_ref
        }
    }

    /// Check if a character is in ambiguous symbol ranges that could be either text or emoji
    /// These ranges contain both text symbols (★ ♠) and true emojis (☀️ ⛄)
    /// We prefer monochrome text rendering for these
    fn is_ambiguous_symbol(ch: char) -> bool {
        matches!(ch,
            '\u{2600}'..='\u{26FF}' | // Miscellaneous Symbols (contains ♠♣♥♦, ★☆, ☀☁, etc.)
            '\u{2700}'..='\u{27BF}'   // Dingbats (contains ✂✈, arrows, etc.)
        )
    }

    /// Find which font actually has a glyph for this character
    /// Returns (font_ref, font_source) for the first font that has the glyph
    /// Fallback chain: main -> nerd -> system fallback (for ambiguous symbols) -> emoji -> system fallback (last resort)
    ///
    /// IMPORTANT: Try main font FIRST so we use the variable weight fonts!
    fn select_font_for_char(&self, ch: char, italic: bool) -> (&FontRef<'static>, FontSource) {
        let main_font = self.select_main_font(italic);

        // Try main font FIRST - this is the variable weight font!
        if main_font.charmap().map(ch) != 0 {
            return (main_font, FontSource::Main);
        }

        // Main font doesn't have it - try nerd font (for symbols like ✓ ✗ ♥ ⚠ etc.)
        let nerd_glyph_id = self.nerd_font_ref.charmap().map(ch);
        if nerd_glyph_id != 0 {
            return (&self.nerd_font_ref, FontSource::Nerd);
        }

        // For ambiguous symbols (U+2600-U+26FF, U+2700-U+27BF), try system fallback
        // BEFORE emoji font to prefer monochrome rendering (★ ♠ ♣ ♦ etc.)
        if Self::is_ambiguous_symbol(ch) {
            if let Some(system_font) = self.system_fallback_ref.as_ref() {
                let system_glyph_id = system_font.charmap().map(ch);
                if system_glyph_id != 0 {
                    return (system_font, FontSource::SystemFallback);
                }
            }
        }

        // Try emoji font for true color emojis (🎯 🚀 etc.)
        if let Some(emoji_font) = self.emoji_font_ref.as_ref() {
            let emoji_glyph_id = emoji_font.charmap().map(ch);
            if emoji_glyph_id != 0 {
                return (emoji_font, FontSource::Emoji);
            }
        }

        // Last resort: Try system fallback font for anything else
        if let Some(system_font) = self.system_fallback_ref.as_ref() {
            let system_glyph_id = system_font.charmap().map(ch);
            if system_glyph_id != 0 {
                return (system_font, FontSource::SystemFallback);
            }
        }

        // Nothing has it - use main font anyway (will render as .notdef tofu)
        (main_font, FontSource::Main)
    }

    /// Segment text into runs by checking which font actually has each character
    /// No guessing - we check the charmap for each font to find which one has the glyph
    fn segment_text_by_font(&self, text: &str, italic: bool) -> Vec<TextRun> {
        let mut runs = Vec::new();
        let mut current_run_start = 0;
        let mut current_run_bytes = 0;
        let mut current_run_text = String::new();
        let mut current_font_type: Option<RunFontType> = None;

        for (byte_offset, ch) in text.char_indices() {
            // Skip variation selectors - they modify the previous character but don't render
            if is_variation_selector(ch) {
                continue;
            }

            // Check which font actually has this character
            let (_font_ref, font_source) = self.select_font_for_char(ch, italic);
            let char_font_type = match font_source {
                FontSource::Main => RunFontType::Main,
                FontSource::Nerd => RunFontType::Nerd,
                FontSource::SystemFallback => RunFontType::SystemFallback,
                FontSource::Emoji => RunFontType::Emoji,
            };

            // Initialize font type from first character
            if current_font_type.is_none() {
                current_font_type = Some(char_font_type);
            }

            // Start a new run if font type changes
            if byte_offset > 0 && char_font_type != current_font_type.unwrap() {
                // Finish current run
                runs.push(TextRun {
                    byte_range: current_run_start..current_run_start + current_run_bytes,
                    text: std::mem::take(&mut current_run_text),
                    font_type: current_font_type.unwrap(),
                });

                // Start new run
                current_run_start = byte_offset;
                current_run_bytes = 0;
                current_font_type = Some(char_font_type);
            }

            current_run_text.push(ch);
            current_run_bytes += ch.len_utf8();
        }

        // Don't forget the last run
        if !current_run_text.is_empty() {
            runs.push(TextRun {
                byte_range: current_run_start..current_run_start + current_run_bytes,
                text: current_run_text,
                font_type: current_font_type.unwrap(),
            });
        }

        runs
    }

    /// Evict least recently used glyphs and rebuild atlas
    /// Called when atlas overflows
    fn evict_lru_glyphs(&mut self) {
        // Sort cache entries by access order and collect keys to evict
        let mut entries: Vec<_> = self
            .glyph_cache
            .iter()
            .map(|(k, v)| (*k, v.access_order))
            .collect();
        entries.sort_by_key(|(_, access_order)| *access_order);

        // Evict oldest 25%
        let evict_count = entries.len() / 4;
        let evict_count = evict_count.max(1); // Always evict at least one

        eprintln!(
            "⚠️  Font atlas overflow! Evicting {} oldest glyphs ({}% of cache)",
            evict_count,
            (evict_count * 100) / entries.len().max(1)
        );

        // Collect keys to evict
        let keys_to_evict: Vec<_> = entries.iter().take(evict_count).map(|(k, _)| *k).collect();

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
        let regular_font_ref = self.regular_font_ref;
        let italic_font_ref = self.italic_font_ref;
        let nerd_font_ref = self.nerd_font_ref;
        let system_fallback_ref = self.system_fallback_ref;
        let emoji_font_ref = self.emoji_font_ref;

        // Re-rasterize each remaining glyph
        // This is inefficient but keeps the logic simple
        for (glyph_id, size_px, italic, weight, font_source) in remaining_keys {
            let font_ref = match font_source {
                FontSource::Main => {
                    if italic {
                        &italic_font_ref
                    } else {
                        &regular_font_ref
                    }
                }
                FontSource::Nerd => &nerd_font_ref,
                FontSource::SystemFallback => {
                    if let Some(ref system_ref) = system_fallback_ref {
                        system_ref
                    } else {
                        continue; // Skip if system fallback font not available
                    }
                }
                FontSource::Emoji => {
                    if let Some(ref emoji_ref) = emoji_font_ref {
                        emoji_ref
                    } else {
                        continue; // Skip if emoji font not available
                    }
                }
            };

            // Re-rasterize this glyph (will be added back to cache)
            let _entry = self.get_or_rasterize_glyph_with_font(
                glyph_id,
                size_px,
                font_ref,
                italic,
                weight as f32, // Convert from quantized u16 back to f32
                font_source,
            );
        }

        // Mark both atlases as dirty
        self.dirty = true;
        self.color_dirty = true;
    }

    /// Load system Unicode fallback font (platform-specific)
    /// Tries to find a monospace font with good Unicode symbol coverage
    fn load_unicode_fallback_font() -> (Option<Arc<Vec<u8>>>, Option<FontRef<'static>>) {
        #[cfg(target_os = "macos")]
        {
            // Try Menlo (macOS monospace with decent Unicode coverage)
            let paths = [
                "/System/Library/Fonts/Menlo.ttc",
                "/System/Library/Fonts/Monaco.dfont",
                "/System/Library/Fonts/Helvetica.ttc", // Last resort
            ];

            for path in &paths {
                if let Ok(font_data_vec) = std::fs::read(path) {
                    let font_data = Arc::new(font_data_vec);
                    let font_data_ref: &'static [u8] =
                        unsafe { std::slice::from_raw_parts(font_data.as_ptr(), font_data.len()) };

                    if let Some(font_ref) = FontRef::from_index(font_data_ref, 0) {
                        eprintln!("✅ Loaded system fallback font: {}", path);
                        return (Some(font_data), Some(font_ref));
                    }
                }
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Try fonts with good Unicode coverage on Windows
            let paths = [
                "C:\\Windows\\Fonts\\consola.ttf", // Consolas
                "C:\\Windows\\Fonts\\arial.ttf",   // Arial has good coverage
            ];

            for path in &paths {
                if let Ok(font_data_vec) = std::fs::read(path) {
                    let font_data = Arc::new(font_data_vec);
                    let font_data_ref: &'static [u8] =
                        unsafe { std::slice::from_raw_parts(font_data.as_ptr(), font_data.len()) };

                    if let Some(font_ref) = FontRef::from_index(font_data_ref, 0) {
                        eprintln!("✅ Loaded system fallback font: {}", path);
                        return (Some(font_data), Some(font_ref));
                    }
                }
            }
        }

        #[cfg(target_os = "linux")]
        {
            // Try DejaVu Sans Mono - excellent Unicode coverage
            let paths = [
                "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
                "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
            ];

            for path in &paths {
                if let Ok(font_data_vec) = std::fs::read(path) {
                    let font_data = Arc::new(font_data_vec);
                    let font_data_ref: &'static [u8] =
                        unsafe { std::slice::from_raw_parts(font_data.as_ptr(), font_data.len()) };

                    if let Some(font_ref) = FontRef::from_index(font_data_ref, 0) {
                        eprintln!("✅ Loaded system fallback font: {}", path);
                        return (Some(font_data), Some(font_ref));
                    }
                }
            }
        }

        eprintln!("⚠️ No system Unicode fallback font loaded");
        (None, None)
    }

    /// Load system emoji font (platform-specific)
    fn load_emoji_font() -> (Option<Arc<Vec<u8>>>, Option<FontRef<'static>>) {
        #[cfg(target_os = "macos")]
        {
            let emoji_path = "/System/Library/Fonts/Apple Color Emoji.ttc";
            if let Ok(emoji_data_vec) = std::fs::read(emoji_path) {
                let emoji_data = Arc::new(emoji_data_vec);

                // SAFETY: Leak the Arc to get a 'static reference
                let emoji_data_ref: &'static [u8] =
                    unsafe { std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len()) };

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
                let emoji_data_ref: &'static [u8] =
                    unsafe { std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len()) };

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
                let emoji_data_ref: &'static [u8] =
                    unsafe { std::slice::from_raw_parts(emoji_data.as_ptr(), emoji_data.len()) };

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
        // Load regular variable font
        let regular_font_static = include_bytes!("../assets/JetBrainsMono-VariableFont_wght.ttf");
        let regular_font_data = Arc::new(regular_font_static.to_vec());
        let regular_font_data_ref: &'static [u8] = unsafe {
            std::slice::from_raw_parts(regular_font_data.as_ptr(), regular_font_data.len())
        };
        let regular_font_ref = FontRef::from_index(regular_font_data_ref, 0)
            .expect("Failed to load regular variable font");

        // Load italic variable font
        let italic_font_static =
            include_bytes!("../assets/JetBrainsMono-Italic-VariableFont_wght.ttf");
        let italic_font_data = Arc::new(italic_font_static.to_vec());
        let italic_font_data_ref: &'static [u8] = unsafe {
            std::slice::from_raw_parts(italic_font_data.as_ptr(), italic_font_data.len())
        };
        let italic_font_ref = FontRef::from_index(italic_font_data_ref, 0)
            .expect("Failed to load italic variable font");

        // Load nerd font for glyphs and Unicode symbols
        // Using full JetBrainsMonoNerdFont instead of Symbols-only because we need
        // standard Unicode symbols (✓ ✗ arrows etc) in addition to PUA nerd icons
        let nerd_font_static = include_bytes!("../assets/JetBrainsMonoNerdFont-Regular.ttf");
        let nerd_font_data = Arc::new(nerd_font_static.to_vec());
        let nerd_font_data_ref: &'static [u8] =
            unsafe { std::slice::from_raw_parts(nerd_font_data.as_ptr(), nerd_font_data.len()) };
        let nerd_font_ref =
            FontRef::from_index(nerd_font_data_ref, 0).expect("Failed to load nerd font");

        // Load system Unicode fallback font for missing symbols
        let (system_fallback_data, system_fallback_ref) = Self::load_unicode_fallback_font();

        // Load emoji font for color emoji fallback
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
        let shaped = shaper.shape(&regular_font_ref, "M", &test_opts);
        let char_width_coef = if let Some(glyph) = shaped.glyphs.first() {
            glyph.x_advance / test_size
        } else {
            0.6 // Fallback coefficient
        };

        eprintln!(
            "✅ Loaded JetBrains Mono variable fonts (regular + italic) with nerd font fallback"
        );

        Self {
            regular_font_data,
            regular_font_ref,
            italic_font_data,
            italic_font_ref,
            nerd_font_data,
            nerd_font_ref,
            system_fallback_data,
            system_fallback_ref,
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
            let font_metrics = self
                .rasterizer
                .get_font_metrics(&self.regular_font_ref, font_size_px);
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

        // Segment text by checking which font has each character
        let runs = self.segment_text_by_font(&expanded, opts.italic);

        if runs.len() > 1
            || runs
                .first()
                .map_or(false, |r| r.font_type != RunFontType::Main)
        {
            // We have multiple font runs - shape each separately

            let mut all_glyphs = Vec::new();
            let mut total_advance = 0.0;

            // Copy main font ref to avoid borrow issues
            let main_font = *self.select_main_font(opts.italic);

            // Shape each run with appropriate font
            // Font type already determined by charmap checking in segment_text_by_font
            for run in &runs {
                let font_for_run = match run.font_type {
                    RunFontType::Main => &main_font,
                    RunFontType::Nerd => &self.nerd_font_ref,
                    RunFontType::SystemFallback => {
                        self.system_fallback_ref.as_ref().unwrap_or(&main_font)
                    }
                    RunFontType::Emoji => self.emoji_font_ref.as_ref().unwrap_or(&main_font),
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

        // No special characters (nerd symbols or emoji) - use main font only
        let main_font = *self.select_main_font(opts.italic);
        let result = self.shaper.shape(&main_font, &expanded, &opts_with_size);
        let cluster_map = ClusterMap::from_glyphs(&result.glyphs, expanded.len());

        (result, cluster_map, expanded)
    }

    /// Get or rasterize a glyph by glyph ID at physical pixel size
    /// font_ref: which font to use for rasterization
    /// italic: whether italic variant is being used
    /// weight: font weight (quantized to nearest 100 for caching)
    /// font_source: which font family the glyph came from (Main/Nerd/Emoji)
    fn get_or_rasterize_glyph_with_font(
        &mut self,
        glyph_id: u16,
        size_px: u32,
        font_ref: &FontRef<'static>,
        italic: bool,
        weight: f32,
        font_source: FontSource,
    ) -> GlyphEntry {
        // Quantize weight to nearest 100 for cache efficiency
        let weight_quantized = ((weight / 100.0).round() * 100.0) as u16;
        let key = (glyph_id, size_px, italic, weight_quantized, font_source);

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

        // Rasterize using swash with appropriate font and weight variation
        let raster_result = match self.rasterizer.rasterize_with_weight(
            font_ref,
            glyph_id,
            size_px as f32,
            weight,
        ) {
            Some(result) => result,
            None => {
                // Failed to rasterize (likely corrupted font data) - return empty/missing glyph
                eprintln!(
                    "Warning: Failed to rasterize glyph_id={} at size={}px (italic={}, weight={}, source={:?})",
                    glyph_id, size_px, italic, weight, font_source
                );

                // Return a missing glyph entry (empty space with minimal advance)
                let entry = GlyphEntry {
                    tex_coords: [0.0, 0.0, 0.0, 0.0],
                    width: 0.0,
                    height: 0.0,
                    advance: size_px as f32 * 0.5, // Half the font size as advance
                    bearing_x: 0.0,
                    bearing_y: 0.0,
                    is_color: false,
                    atlas_index: 0,
                    access_order: self.current_frame,
                };

                self.glyph_cache.insert(key, entry);
                return entry;
            }
        };

        let bitmap = &raster_result.bitmap;
        let width = raster_result.width;
        let height = raster_result.height;

        // Choose atlas based on whether this is a color glyph
        let (next_x, next_y, row_height, atlas_index) = if is_color {
            (
                &mut self.color_next_x,
                &mut self.color_next_y,
                &mut self.color_row_height,
                1u8,
            )
        } else {
            (
                &mut self.next_x,
                &mut self.next_y,
                &mut self.row_height,
                0u8,
            )
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
                italic,
                weight,
                font_source,
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
                                self.color_atlas_data[atlas_idx] = 255; // R
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
                    let atlas_idx = ((*next_y + y) * self.atlas_size.0 + (*next_x + x)) as usize;
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

    /// Get or rasterize a glyph by glyph ID at physical pixel size (main font, regular weight, no italic)
    fn get_or_rasterize_glyph(&mut self, glyph_id: u16, size_px: u32) -> GlyphEntry {
        let font_ref = self.regular_font_ref; // Copy FontRef to avoid borrow issues
        self.get_or_rasterize_glyph_with_font(
            glyph_id,
            size_px,
            &font_ref,
            false,
            400.0,
            FontSource::Main,
        )
    }

    /// Layout text using swash shaping with tab expansion
    pub fn layout_text_shaped_with_tabs(
        &mut self,
        text: &str,
        font_size_px: f32,
        options: Option<&ShapingOptions>,
    ) -> ShapedTextLayout {
        // Get font metrics from main font for baseline
        let opts = options.unwrap_or(&self.shaping_options);

        let opts_clone = opts.clone(); // Clone to avoid borrow conflicts
        let main_font_ref = *self.select_main_font(opts_clone.italic); // Copy FontRef
        let main_font_metrics = self
            .rasterizer
            .get_font_metrics(&main_font_ref, font_size_px);
        let baseline_y = main_font_metrics.ascent;

        // Expand tabs
        let expanded = expand_tabs(text);

        // Segment text by checking which font has each character
        let runs = self.segment_text_by_font(&expanded, opts_clone.italic);

        if runs.len() > 1
            || runs
                .first()
                .map_or(false, |r| r.font_type != RunFontType::Main)
        {
            // We have multiple font runs - shape each separately

            let mut opts_with_size = opts.clone();
            opts_with_size.font_size = font_size_px;

            let mut all_shaped_glyphs = Vec::new();
            let mut positioned_glyphs = Vec::new();
            let mut pen_x = 0.0f32;
            let mut max_y = 0.0f32;

            // Shape and position each run
            // Font type already determined by charmap checking in segment_text_by_font
            for run in &runs {
                let (font_ref, font_source) = match run.font_type {
                    RunFontType::Main => (main_font_ref, FontSource::Main),
                    RunFontType::Nerd => (self.nerd_font_ref, FontSource::Nerd),
                    RunFontType::SystemFallback => {
                        if let Some(system_font) = self.system_fallback_ref.as_ref() {
                            (*system_font, FontSource::SystemFallback)
                        } else {
                            (main_font_ref, FontSource::Main)
                        }
                    }
                    RunFontType::Emoji => {
                        if let Some(emoji_font) = self.emoji_font_ref.as_ref() {
                            (*emoji_font, FontSource::Emoji)
                        } else {
                            (main_font_ref, FontSource::Main)
                        }
                    }
                };

                let mut run_result = self.shaper.shape(&font_ref, &run.text, &opts_with_size);

                // Adjust cluster positions to be relative to full text
                for glyph in &mut run_result.glyphs {
                    glyph.cluster += run.byte_range.start as u32;
                }

                // Position glyphs in this run
                for shaped_glyph in &run_result.glyphs {
                    // Use appropriate italic/weight for main font, defaults for others
                    let (use_italic, use_weight) = if font_source == FontSource::Main {
                        (opts_clone.italic, opts_clone.weight)
                    } else {
                        (false, 400.0) // Fallback fonts (nerd/system/emoji) don't have italic/weight
                    };

                    let entry = self.get_or_rasterize_glyph_with_font(
                        shaped_glyph.glyph_id,
                        font_size_px as u32,
                        &font_ref,
                        use_italic,
                        use_weight,
                        font_source,
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

        // No special characters (nerd symbols or emoji) - use main font only (fast path)
        let shaped_font_ref = main_font_ref;

        // Shape directly with main font
        let mut opts_with_size = opts_clone.clone();
        opts_with_size.font_size = font_size_px;

        let shaping_result = self
            .shaper
            .shape(&main_font_ref, &expanded, &opts_with_size);
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
                opts_clone.italic,
                opts_clone.weight,
                FontSource::Main,
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
        let charmap = self.regular_font_ref.charmap();
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

    /// Get baseline offset (ascent) for a given font size in logical pixels
    pub fn get_baseline(&self, logical_font_size: f32, scale_factor: f32) -> f32 {
        let font_system = self.inner.lock();
        let physical_size = logical_font_size * scale_factor;

        // Get metrics directly from font (avoid borrow checker issue)
        let font_ref = &font_system.regular_font_ref;
        let metrics = font_ref.metrics(&[]);
        let scale = physical_size / metrics.units_per_em as f32;
        let ascent_physical = metrics.ascent * scale;

        ascent_physical / scale_factor
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
        self.inner.lock().layout_text_with_line_height(
            text,
            physical_size,
            Some(physical_line_height),
        )
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

        // Expand tabs to match what layout_text_shaped_with_tabs does internally
        let expanded = expand_tabs(line_text);

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
        // Note: byte positions from cluster map are relative to the EXPANDED text
        let byte_pos_expanded = if left < shaped.glyphs.len() {
            // Get byte position from cluster map
            shaped
                .cluster_map
                .glyph_to_byte(left)
                .unwrap_or(expanded.len())
        } else {
            expanded.len()
        };

        // Snap to cluster boundary to ensure cursor is in a valid position
        let snapped_byte_expanded = shaped
            .cluster_map
            .snap_to_cluster_boundary(byte_pos_expanded);

        // Convert expanded byte position to character position in expanded text
        // Then map back to original text by counting characters
        let char_pos_expanded = expanded[..snapped_byte_expanded.min(expanded.len())]
            .chars()
            .count();

        // Map character position in expanded text to character position in original text
        // by walking through both strings in parallel
        let mut orig_char_idx = 0;
        let mut exp_char_idx = 0;

        for ch in line_text.chars() {
            if exp_char_idx >= char_pos_expanded {
                break;
            }

            if ch == '\t' {
                // Tab expands to multiple spaces
                const TAB_WIDTH: usize = 4;
                let spaces_added = TAB_WIDTH - (exp_char_idx % TAB_WIDTH);
                exp_char_idx += spaces_added;
            } else {
                exp_char_idx += 1;
            }
            orig_char_idx += 1;
        }

        orig_char_idx as u32
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

    /// Set default font weight for all text (100-900, where 400=normal, 700=bold)
    pub fn set_default_weight(&self, weight: f32) {
        self.inner.lock().set_default_weight(weight);
    }

    /// Set default italic setting for all text
    pub fn set_default_italic(&self, italic: bool) {
        self.inner.lock().set_default_italic(italic);
    }

    /// Get current default weight
    pub fn default_weight(&self) -> f32 {
        self.inner.lock().default_weight()
    }

    /// Get current default italic
    pub fn default_italic(&self) -> bool {
        self.inner.lock().default_italic()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nerd_font_has_symbols() {
        // Load the full nerd font (not just Symbols)
        let nerd_font_data = include_bytes!("../assets/JetBrainsMonoNerdFont-Regular.ttf");
        let nerd_font_ref =
            FontRef::from_index(nerd_font_data, 0).expect("Failed to load nerd font");

        // Sample of common Nerd Font PUA icons
        let nerd_icons = vec![
            // Powerline
            ('\u{E0A0}', "U+E0A0", "powerline branch"),
            ('\u{E0A1}', "U+E0A1", "powerline ln"),
            ('\u{E0A2}', "U+E0A2", "powerline lock"),
            ('\u{E0B0}', "U+E0B0", "powerline arrow right"),
            ('\u{E0B1}', "U+E0B1", "powerline arrow right alt"),
            ('\u{E0B2}', "U+E0B2", "powerline arrow left"),
            ('\u{E0B3}', "U+E0B3", "powerline arrow left alt"),
            // Devicons (file types)
            ('\u{E7C5}', "U+E7C5", "devicon javascript"),
            ('\u{E718}', "U+E718", "devicon python"),
            ('\u{E7A8}', "U+E7A8", "devicon rust"),
            // Font Awesome
            ('\u{F07C}', "U+F07C", "folder"),
            ('\u{F15B}', "U+F15B", "file"),
            ('\u{F1C0}', "U+F1C0", "database"),
            ('\u{F09B}', "U+F09B", "github"),
            ('\u{F126}', "U+F126", "code"),
            ('\u{F0C9}', "U+F0C9", "list"),
        ];

        let standard_symbols = vec![
            ('✓', "U+2713", "checkmark"),
            ('✗', "U+2717", "ballot x"),
            ('★', "U+2605", "star"),
            ('♥', "U+2665", "heart"),
            ('⚠', "U+26A0", "warning sign"),
        ];

        println!("\n=== Nerd Font Icon Coverage Test ===");
        println!("\nNerd Font PUA Icons:");
        for (ch, unicode, name) in &nerd_icons {
            let glyph_id = nerd_font_ref.charmap().map(*ch);
            println!(
                "  {} {} {:30} glyph_id: {:5} {}",
                if glyph_id != 0 { '✓' } else { '✗' },
                unicode,
                name,
                glyph_id,
                if glyph_id != 0 { "" } else { "NOT FOUND" }
            );
        }

        println!("\nStandard Unicode Symbols:");
        for (ch, unicode, name) in &standard_symbols {
            let glyph_id = nerd_font_ref.charmap().map(*ch);
            println!(
                "  {} {} {} {:30} glyph_id: {:5} {}",
                ch,
                if glyph_id != 0 { '✓' } else { '✗' },
                unicode,
                name,
                glyph_id,
                if glyph_id != 0 { "" } else { "NOT FOUND" }
            );
        }

        // Basic symbols should be present
        let checkmark_glyph = nerd_font_ref.charmap().map('✓');
        let ballot_x_glyph = nerd_font_ref.charmap().map('✗');
        let powerline_glyph = nerd_font_ref.charmap().map('\u{E0A0}');

        assert_ne!(
            checkmark_glyph, 0,
            "Checkmark ✓ should be in full nerd font"
        );
        assert_ne!(ballot_x_glyph, 0, "Ballot X ✗ should be in full nerd font");
        assert_ne!(powerline_glyph, 0, "Powerline  should be in full nerd font");
    }
}
