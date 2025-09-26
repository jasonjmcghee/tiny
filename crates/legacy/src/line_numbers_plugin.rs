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

        // Use widget viewport if available for proper bounds
        let widget_viewport = collector.widget_viewport.as_ref();

        let line_height = collector.viewport.line_height;
        let scale_factor = collector.viewport.scale_factor;
        // Use widget viewport's scroll if available
        let scroll_y = widget_viewport
            .map(|w| w.scroll.y.0)
            .unwrap_or(0.0);
        let font_size = collector.viewport.font_size;

        // Get total lines in document
        let tree = doc.read();
        let total_lines = tree.line_count();

        // Use widget bounds for height if available
        let viewport_height = widget_viewport
            .map(|w| w.bounds.height.0)
            .unwrap_or(collector.viewport.logical_size.height.0);

        let first_visible_line = (scroll_y / line_height).floor() as usize;
        let last_visible_line = ((scroll_y + viewport_height) / line_height).ceil() as usize + 1;

        let mut line_number_glyphs = Vec::new();

        // Generate line number glyphs for visible lines only
        for line_num in first_visible_line..last_visible_line.min(total_lines as usize) {
            let line_text = (line_num + 1).to_string();

            let text_width = line_text.len() as f32 * 7.0;
            let x_pos = 45.0 - text_width; // Right-align at x=45
            // Y position in LOCAL widget space (0,0 is top-left of line numbers area)
            let y_pos = (line_num as f32 * line_height) - scroll_y;

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
                    // Transform from widget-local space to screen space
                    // Add the widget bounds offset to position on screen
                    let screen_x = g.pos.x.0 + widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
                    let screen_y = g.pos.y.0 + widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
                    // Convert to physical coordinates
                    g.pos = LayoutPos::new(screen_x * scale_factor, screen_y * scale_factor);

                    // Set token_id for line numbers
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

        // Use widget viewport if available (for proper bounds)
        let widget_viewport = ctx.widget_viewport.as_ref();
        let bounds = widget_viewport.map(|w| &w.bounds);

        eprintln!("LineNumbers paint() called");
        if let Some(wv) = widget_viewport {
            eprintln!("  Widget viewport bounds: x={}, y={}, w={}, h={}",
                wv.bounds.x.0, wv.bounds.y.0, wv.bounds.width.0, wv.bounds.height.0);
            eprintln!("  Widget viewport scroll: x={}, y={}", wv.scroll.x.0, wv.scroll.y.0);
        } else {
            eprintln!("  No widget viewport!");
        }

        let line_height = ctx.viewport.line_height;
        let scale_factor = ctx.viewport.scale_factor;
        // Use widget viewport scroll if available, otherwise main viewport
        let scroll_y = widget_viewport
            .map(|w| w.scroll.y.0)
            .unwrap_or(ctx.viewport.scroll.y.0);
        let font_size = ctx.viewport.font_size;

        eprintln!("  line_height={}, scale={}, scroll_y={}, font_size={}",
            line_height, scale_factor, scroll_y, font_size);

        // Get total lines in document
        let tree = doc.read();
        let total_lines = tree.line_count();

        // Use widget bounds for height if available
        let viewport_height = bounds
            .map(|b| b.height.0)
            .unwrap_or(ctx.viewport.logical_size.height.0);

        // Draw line numbers
        if ctx.gpu_renderer != std::ptr::null_mut() {
            unsafe {
                let gpu_renderer = &*(ctx.gpu_renderer as *const tiny_core::GpuRenderer);

                // Generate and draw line number glyphs
                let mut line_number_glyphs = Vec::new();

                // Calculate the first visible line based on scroll
                let first_visible_line = (scroll_y / line_height).floor() as usize;
                let last_visible_line = ((scroll_y + viewport_height) / line_height).ceil() as usize;

                eprintln!("  Generating line numbers {} to {} (total={})",
                    first_visible_line + 1, last_visible_line, total_lines);

                for line_num in first_visible_line..last_visible_line.min(total_lines as usize) {
                    let line_text = (line_num + 1).to_string();

                    let text_width = line_text.len() as f32 * 7.0;
                    let x_pos = 45.0 - text_width; // Right-align at x=45

                    // Y position in document space
                    let doc_y = line_num as f32 * line_height;
                    // Convert to view space (subtract scroll)
                    let view_y = doc_y - scroll_y;

                    // Position in logical coordinates relative to the scissor rect
                    let pos = LayoutPos::new(x_pos, view_y);

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

                    if line_num == first_visible_line {
                        eprintln!("  Line {}: pos=({}, {}), {} glyphs created",
                            line_num + 1, x_pos, view_y, glyphs.len());
                    }

                    for (i, mut g) in glyphs.into_iter().enumerate() {
                        let width = g.tex_coords[2] - g.tex_coords[0];
                        let height = g.tex_coords[3] - g.tex_coords[1];

                        if line_num == first_visible_line && i == 0 {
                            eprintln!("    First glyph: pos=({}, {}), tex=[{}, {}, {}, {}], token={}",
                                g.pos.x.0, g.pos.y.0,
                                g.tex_coords[0], g.tex_coords[1], g.tex_coords[2], g.tex_coords[3],
                                g.token_id);
                        }

                        if width > 0.0001 && height > 0.0001 {
                            // The scissor rect is at (0, global_margin.y * scale) in screen space
                            // Glyphs need to be positioned in screen space, not scissor-relative space
                            let physical_x = g.pos.x.0 * scale_factor;
                            // Add the scissor rect's Y offset to position in screen space
                            let physical_y = g.pos.y.0 * scale_factor + (ctx.viewport.global_margin.y.0 * scale_factor);
                            g.pos = LayoutPos::new(physical_x, physical_y);

                            // Set token_id for line numbers styling
                            g.token_id = 255; // Use max token for line numbers

                            line_number_glyphs.push(g);
                        }
                    }
                }

                // Draw the line number glyphs
                if !line_number_glyphs.is_empty() {
                    eprintln!("Drawing {} line number glyphs, first at ({}, {}), tex=[{:.3}, {:.3}, {:.3}, {:.3}]",
                        line_number_glyphs.len(),
                        line_number_glyphs[0].pos.x.0,
                        line_number_glyphs[0].pos.y.0,
                        line_number_glyphs[0].tex_coords[0],
                        line_number_glyphs[0].tex_coords[1],
                        line_number_glyphs[0].tex_coords[2],
                        line_number_glyphs[0].tex_coords[3]);

                    // Check if font atlas is available
                    eprintln!("  Calling draw_glyphs_styled with use_styled=false");
                    gpu_renderer.draw_glyphs_styled(pass, &line_number_glyphs, false);
                }
            }
        }
    }

    fn z_index(&self) -> i32 {
        100 // Render AFTER main text so our vertices aren't overwritten
    }
}
