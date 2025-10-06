//! Text shaping with swash - proper OpenType support for ligatures, kerning, complex scripts

use swash::shape::ShapeContext;
use swash::text::{Language, Script};
use swash::FontRef;

/// Shaping options for text layout
#[derive(Clone, Debug)]
pub struct ShapingOptions {
    /// Enable ligatures (e.g., "=>" becomes single glyph)
    pub enable_ligatures: bool,
    /// Enable kerning (almost always true)
    pub enable_kerning: bool,
    /// Enable contextual alternates (script-specific substitutions)
    pub enable_contextual_alternates: bool,
    /// Script hint (None = auto-detect)
    pub script: Option<Script>,
    /// Language hint
    pub language: Option<Language>,
    /// Font size in pixels
    pub font_size: f32,
    /// Use italic variant
    pub italic: bool,
    /// Variable font weight (100-900, where 400=normal, 700=bold)
    pub weight: f32,
}

impl Default for ShapingOptions {
    fn default() -> Self {
        Self {
            enable_ligatures: true, // Default to ligatures ON
            enable_kerning: true,   // Always enable kerning
            enable_contextual_alternates: true,
            script: None,   // Auto-detect
            language: None, // Auto-detect
            font_size: 16.0,
            italic: false, // Default to regular (non-italic)
            weight: 400.0, // Default to normal weight
        }
    }
}

impl ShapingOptions {
    /// Create options with ligatures disabled (for code editors)
    pub fn no_ligatures() -> Self {
        Self {
            enable_ligatures: false,
            ..Default::default()
        }
    }

    /// Create options for a specific font size
    pub fn with_size(font_size: f32) -> Self {
        Self {
            font_size,
            ..Default::default()
        }
    }
}

/// A single shaped glyph with positioning and cluster information
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    /// Font-specific glyph ID
    pub glyph_id: u16,
    /// Source byte offset in original text (cluster start)
    pub cluster: u32,
    /// Horizontal offset from pen position
    pub x_offset: f32,
    /// Vertical offset from baseline
    pub y_offset: f32,
    /// Horizontal advance (how far to move pen)
    pub x_advance: f32,
    /// Vertical advance (usually 0 for horizontal text)
    pub y_advance: f32,
}

/// Result of shaping a text string
#[derive(Debug)]
pub struct ShapingResult {
    /// Shaped glyphs in visual order
    pub glyphs: Vec<ShapedGlyph>,
    /// Total advance width
    pub advance: f32,
}

/// Font type for a text run
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFontType {
    /// Regular text font (main font - regular or italic)
    Main,
    /// Nerd font symbols
    Nerd,
    /// Emoji font
    Emoji,
}

/// A text run with a specific font
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Byte range in source text
    pub byte_range: std::ops::Range<usize>,
    /// Text content for this run
    pub text: String,
    /// Which font type to use for this run (determined by charmap checking)
    pub font_type: RunFontType,
}

/// Text shaper wrapping swash
pub struct Shaper {
    /// Swash shaping context
    context: ShapeContext,
}

impl Shaper {
    /// Create a new text shaper
    pub fn new() -> Self {
        Self {
            context: ShapeContext::new(),
        }
    }

    /// Shape a text string with the given font and options
    pub fn shape(
        &mut self,
        font_ref: &FontRef,
        text: &str,
        options: &ShapingOptions,
    ) -> ShapingResult {
        // Create builder for this font
        let mut builder = self.context.builder(*font_ref);

        // Set font size
        builder = builder.size(options.font_size);

        // Set weight variation if font supports it
        // Standard OpenType 'wght' axis uses tag [119, 103, 104, 116] (b"wght")
        // Clamp weight to valid range (100-900)
        let weight = options.weight.clamp(100.0, 900.0);
        builder = builder.variations(&[([119, 103, 104, 116], weight)]);

        // Configure OpenType features
        // Note: swash enables common features by default (liga, kern, calt)
        // We need to explicitly disable them if requested
        if !options.enable_ligatures {
            // Disable standard ligatures
            builder = builder.features(&[("liga", 0), ("dlig", 0)]);
        }

        // Build the shaper
        let mut shaper = builder.build();

        // Shape the text
        shaper.add_str(text);

        // Collect shaped glyphs
        let mut glyphs = Vec::new();
        let mut total_advance = 0.0;

        shaper.shape_with(|cluster| {
            // cluster.source gives us the byte range in the source text
            let cluster_start = cluster.source.start as u32;

            // Process each glyph in this cluster
            for glyph in cluster.glyphs {
                let shaped_glyph = ShapedGlyph {
                    glyph_id: glyph.id,
                    cluster: cluster_start,
                    x_offset: glyph.x,
                    y_offset: glyph.y,
                    x_advance: glyph.advance,
                    y_advance: 0.0, // Horizontal text
                };

                total_advance += shaped_glyph.x_advance;
                glyphs.push(shaped_glyph);
            }
        });

        ShapingResult {
            glyphs,
            advance: total_advance,
        }
    }
}

impl Default for Shaper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shaping_options() {
        let default = ShapingOptions::default();
        assert!(default.enable_ligatures);
        assert!(default.enable_kerning);

        let no_liga = ShapingOptions::no_ligatures();
        assert!(!no_liga.enable_ligatures);
        assert!(no_liga.enable_kerning);
    }
}
