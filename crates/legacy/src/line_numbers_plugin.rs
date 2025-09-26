//! Line numbers plugin - renders line numbers as a separate layer

use tiny_core::tree::Doc;
use tiny_font::{create_glyph_instances, SharedFontSystem};
use tiny_sdk::{
    Capability, Initializable, LayoutPos, PaintContext, Paintable, Plugin, PluginError,
    SetupContext,
};

/// Wrapper to make Doc pointer Send + Sync
struct DocPtr(*const Doc);

unsafe impl Send for DocPtr {}
unsafe impl Sync for DocPtr {}

/// Plugin that renders line numbers
pub struct LineNumbersPlugin {
    /// Whether to show line numbers
    pub enabled: bool,
    /// X position for line numbers (offset from margin)
    pub line_number_offset: f32,
    /// Reference to the document (for line count)
    doc: Option<DocPtr>,
}

impl LineNumbersPlugin {
    pub fn new() -> Self {
        Self {
            enabled: true,
            line_number_offset: -50.0, // Default offset to the left of text
            doc: None,
        }
    }

    /// Set the document reference
    pub fn set_document(&mut self, doc: &Doc) {
        self.doc = Some(DocPtr(doc as *const _));
    }

    /// Enable or disable line numbers
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Collect glyphs for batched rendering instead of drawing immediately
    pub fn collect_glyphs(&self, collector: &mut crate::render::GlyphCollector) {
        if !self.enabled {
            return;
        }

        // Get the document
        let doc = match &self.doc {
            Some(DocPtr(doc_ptr)) => unsafe { &**doc_ptr },
            None => return,
        };

        // Get font service from service registry
        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let line_height = collector.viewport.line_height;
        let scale_factor = collector.viewport.scale_factor;
        let scroll_y = collector.viewport.scroll.y.0;
        let global_margin_y = collector.viewport.global_margin.y.0;
        let font_size = collector.viewport.font_size;

        // Get total lines in document
        let tree = doc.read();
        let total_lines = tree.line_count();

        // Calculate visible lines - show ALL lines, not just viewport
        let viewport_height = collector.viewport.logical_size.height.0;
        let visible_lines = total_lines as usize; // Show all lines

        let mut line_number_glyphs = Vec::new();

        // Generate line number glyphs for all lines
        for line_num in 0..visible_lines {
            let line_text = (line_num + 1).to_string();

            let text_width = line_text.len() as f32 * 7.0;
            let x_pos = 45.0 - text_width; // Right-align at x=45 (like before)
            let y_pos = global_margin_y + (line_num as f32 * line_height); // Add global margin

            let pos = LayoutPos::new(x_pos, y_pos);

            let glyphs = create_glyph_instances(
                &font_service,
                &line_text,
                pos,
                font_size,
                scale_factor,
                line_height,
                None,
                0,
            );

            for mut g in glyphs {
                let width = g.tex_coords[2] - g.tex_coords[0];
                let height = g.tex_coords[3] - g.tex_coords[1];

                if width > 0.0001 && height > 0.0001 {
                    // Convert to physical like main renderer does: layout_to_physical
                    // This subtracts scroll then multiplies by scale
                    let view_x = g.pos.x.0;
                    let view_y = g.pos.y.0 - scroll_y;
                    g.pos = LayoutPos::new(view_x * scale_factor, view_y * scale_factor);

                    // Set token_id to 54 for line numbers
                    g.token_id = 255;

                    line_number_glyphs.push(g);
                }
            }
        }

        collector.add_glyphs(line_number_glyphs);
    }
}

// === Plugin Trait Implementation ===

impl Plugin for LineNumbersPlugin {
    fn name(&self) -> &str {
        "line_numbers"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("line_numbers".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for LineNumbersPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Paintable for LineNumbersPlugin {
    fn paint(&self, ctx: &PaintContext, pass: &mut wgpu::RenderPass) {
        if !self.enabled {
            return;
        }

        // Get the document
        let doc = match &self.doc {
            Some(DocPtr(doc_ptr)) => unsafe { &**doc_ptr },
            None => return,
        };

        // Get font service from registry
        let services = unsafe { ctx.services() };
        let font_service = match services.get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let line_height = ctx.viewport.line_height;
        let scale_factor = ctx.viewport.scale_factor;
        let scroll_y = ctx.viewport.scroll.y.0;
        let global_margin_y = ctx.viewport.global_margin.y.0;
        // Now we have the actual font_size from ViewportInfo
        let font_size = ctx.viewport.font_size;

        // Get total lines in document
        let tree = doc.read();
        let total_lines = tree.line_count();

        // Calculate visible lines based on viewport
        let viewport_height = ctx.viewport.logical_size.height.0;
        let visible_lines =
            ((viewport_height / line_height).ceil() as usize).min(total_lines as usize);

        // First draw a background rectangle to see where line numbers should be
        if ctx.gpu_renderer != std::ptr::null_mut() {
            unsafe {
                let gpu_renderer = &*(ctx.gpu_renderer as *const tiny_core::GpuRenderer);

                // Draw a semi-transparent background for the line number area
                use tiny_sdk::types::{LayoutRect, RectInstance};
                let bg_rect = RectInstance {
                    rect: LayoutRect::new(
                        0.0,
                        0.0,
                        50.0, // Line number area width
                        viewport_height,
                    ),
                    color: 0x00000000, // Dark gray with some transparency
                };
                gpu_renderer.draw_rects(pass, &[bg_rect], scale_factor);
            }
        }
    }

    fn z_index(&self) -> i32 {
        100 // Render AFTER main text so our vertices aren't overwritten
    }
}
