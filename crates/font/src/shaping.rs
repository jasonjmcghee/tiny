//! Text shaping with swash - proper OpenType support for ligatures, kerning, complex scripts

use swash::shape::ShapeContext;
use swash::text::{Script, Language};
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
}

impl Default for ShapingOptions {
    fn default() -> Self {
        Self {
            enable_ligatures: true, // Default to ligatures ON
            enable_kerning: true,   // Always enable kerning
            enable_contextual_alternates: true,
            script: None,           // Auto-detect
            language: None,         // Auto-detect
            font_size: 16.0,
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

/// A text run with a specific font
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Byte range in source text
    pub byte_range: std::ops::Range<usize>,
    /// Text content for this run
    pub text: String,
    /// Whether this run should use emoji font
    pub use_emoji_font: bool,
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

    /// Segment text into runs based on font requirements (emoji vs non-emoji)
    pub fn segment_text(text: &str) -> Vec<TextRun> {
        use crate::emoji::is_emoji;

        let mut runs = Vec::new();
        let mut current_run_start = 0;
        let mut current_run_bytes = 0;
        let mut current_run_text = String::new();
        let mut current_is_emoji = false;

        for (byte_offset, ch) in text.char_indices() {
            let is_emoji_char = is_emoji(ch);

            // Start a new run if font type changes
            if byte_offset > 0 && is_emoji_char != current_is_emoji {
                // Finish current run
                runs.push(TextRun {
                    byte_range: current_run_start..current_run_start + current_run_bytes,
                    text: std::mem::take(&mut current_run_text),
                    use_emoji_font: current_is_emoji,
                });

                // Start new run
                current_run_start = byte_offset;
                current_run_bytes = 0;
                current_is_emoji = is_emoji_char;
            }

            current_run_text.push(ch);
            current_run_bytes += ch.len_utf8();
        }

        // Don't forget the last run
        if !current_run_text.is_empty() {
            runs.push(TextRun {
                byte_range: current_run_start..current_run_start + current_run_bytes,
                text: current_run_text,
                use_emoji_font: current_is_emoji,
            });
        }

        runs
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
