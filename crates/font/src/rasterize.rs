//! Glyph rasterization using swash and zeno
//!
//! Supports both bitmap and outline glyphs, including color emojis

use swash::scale::{ScaleContext, Render, Source, StrikeWith};
use swash::zeno::Format;
use swash::FontRef;

/// Rasterization result
pub struct RasterResult {
    /// Bitmap data (R8 for monochrome, RGBA8 for color)
    pub bitmap: Vec<u8>,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Horizontal bearing (left side)
    pub bearing_x: f32,
    /// Vertical bearing (top side)
    pub bearing_y: f32,
    /// Horizontal advance
    pub advance: f32,
    /// Whether this is a color glyph
    pub is_color: bool,
}

/// Glyph rasterizer using swash
pub struct Rasterizer {
    /// Swash scale context
    context: ScaleContext,
}

/// Font-level metrics for baseline calculation
#[derive(Clone, Copy, Debug)]
pub struct FontMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub leading: f32,
}

impl Rasterizer {
    /// Create a new rasterizer
    pub fn new() -> Self {
        Self {
            context: ScaleContext::new(),
        }
    }

    /// Get font metrics for baseline calculation
    pub fn get_font_metrics(&mut self, font_ref: &FontRef, font_size: f32) -> FontMetrics {
        // Get metrics from the font
        let metrics = font_ref.metrics(&[]);

        // Scale metrics to font size
        let scale = font_size / metrics.units_per_em as f32;

        FontMetrics {
            ascent: metrics.ascent * scale,
            descent: metrics.descent * scale,
            leading: metrics.leading * scale,
        }
    }

    /// Rasterize a glyph by ID with optional weight variation
    pub fn rasterize(
        &mut self,
        font_ref: &FontRef,
        glyph_id: u16,
        font_size: f32,
    ) -> Option<RasterResult> {
        self.rasterize_with_weight(font_ref, glyph_id, font_size, 400.0)
    }

    /// Rasterize a glyph by ID with weight variation
    pub fn rasterize_with_weight(
        &mut self,
        font_ref: &FontRef,
        glyph_id: u16,
        font_size: f32,
        weight: f32,
    ) -> Option<RasterResult> {
        // Create a scaler for this font and size
        let mut builder = self
            .context
            .builder(*font_ref)
            .size(font_size)
            .hint(true);

        // Apply weight variation if not default
        // Standard OpenType 'wght' axis uses tag [119, 103, 104, 116] (b"wght")
        if weight != 400.0 {
            builder = builder.variations(&[([119, 103, 104, 116], weight)]);
        }

        let mut scaler = builder.build();

        // Use catch_unwind to handle panics in swash when parsing malformed font data
        // Some fonts have corrupted sbix tables that cause overflow panics
        // SAFETY: We wrap the render call to catch panics from corrupted font data
        let render_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Render::new(&[
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::ColorOutline(0),
                Source::Outline,
            ])
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id)
        }));

        let image = match render_result {
            Ok(Some(img)) => img,
            Ok(None) => return None,
            Err(_) => {
                eprintln!("Warning: Swash panicked while rendering glyph_id={} (corrupted font data)", glyph_id);
                return None;
            }
        };

        // Check if we got color data (CustomSubpixel would give RGBA, but Format::Alpha gives grayscale)
        // For now, treat all emoji font glyphs as potentially color
        // The actual color rendering will need shader support
        let is_color = image.data.len() == (image.placement.width * image.placement.height * 4) as usize;

        Some(RasterResult {
            bitmap: image.data.to_vec(),
            width: image.placement.width,
            height: image.placement.height,
            bearing_x: image.placement.left as f32,
            bearing_y: image.placement.top as f32,
            advance: image.placement.width as f32,
            is_color,
        })
    }

    /// Check if a glyph is a color emoji
    pub fn is_color_glyph(&mut self, font_ref: &FontRef, glyph_id: u16) -> bool {
        // Check if the font has color tables and this glyph uses them
        let mut scaler = self
            .context
            .builder(*font_ref)
            .size(16.0) // Size doesn't matter for detection
            .build();

        // Try to get color bitmap first - wrap in catch_unwind for corrupted fonts
        let bitmap_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Render::new(&[Source::ColorBitmap(StrikeWith::BestFit)])
                .render(&mut scaler, glyph_id)
        }));

        if matches!(bitmap_result, Ok(Some(_))) {
            return true;
        }

        // Try color outline (COLR/CPAL) - wrap in catch_unwind for corrupted fonts
        let outline_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Render::new(&[Source::ColorOutline(0)])
                .render(&mut scaler, glyph_id)
        }));

        matches!(outline_result, Ok(Some(_)))
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rasterizer_creation() {
        let _rasterizer = Rasterizer::new();
    }
}
