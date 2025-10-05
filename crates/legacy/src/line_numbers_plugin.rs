//! Line numbers plugin - renders line numbers as a separate layer

use tiny_core::tree::{Doc, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::LogicalPixels;
use tiny_ui::{coordinates::Viewport, text_view::TextView};

/// Wrapper to make Doc pointer Send + Sync
struct DocPtr(*const Doc);

unsafe impl Send for DocPtr {}
unsafe impl Sync for DocPtr {}

/// Plugin that renders line numbers
pub struct LineNumbersPlugin {
    /// Whether to show line numbers
    pub enabled: bool,
    /// The width (calculated from TextView's content width + padding)
    pub width: f32,
    /// X position for line numbers (offset from margin)
    pub line_number_offset: f32,
    /// Reference to the document (for line count)
    doc: Option<DocPtr>,
    /// TextView for rendering line numbers
    text_view: TextView,
    /// Cache total lines to avoid regenerating text
    last_total_lines: Option<u32>,
    /// Cache font size to detect changes
    last_font_size: Option<f32>,
}

impl LineNumbersPlugin {
    pub fn new() -> Self {
        // Create a default viewport for the TextView (will be updated during rendering)
        let viewport = Viewport::new(60.0, 600.0, 1.0);

        Self {
            enabled: true,
            width: 60.0,
            line_number_offset: -50.0,
            doc: None,
            text_view: TextView::empty(viewport)
                .with_padding_x(8.0)  // Horizontal padding for spacing
                .with_padding_y(0.0)  // No vertical padding - align exactly with text
                .with_width(tiny_ui::text_view::SizeConstraint::HugContents)
                .with_height(tiny_ui::text_view::SizeConstraint::FillContainer),
            last_total_lines: None,
            last_font_size: None,
        }
    }

    /// Set the document reference
    pub fn set_document(&mut self, doc: &Doc) {
        self.doc = Some(DocPtr(doc as *const _));
        // Clear cache when document changes
        self.last_total_lines = None;
    }

    /// Enable or disable line numbers
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Collect glyphs for batched rendering instead of drawing immediately
    pub fn collect_glyphs(&mut self, collector: &mut crate::render::GlyphCollector) {
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
        let scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(0.0);
        let font_size = collector.viewport.font_size;

        // Get total lines in document
        let tree = doc.read();
        let total_lines = tree.line_count();

        let viewport_height = widget_viewport
            .map(|w| w.bounds.height.0)
            .unwrap_or(collector.viewport.logical_size.height.0);

        // Check if we need to regenerate all line numbers or recalculate width
        let content_changed = self.last_total_lines != Some(total_lines);
        let font_size_changed = self.last_font_size != Some(font_size);

        if content_changed {
            // Calculate max line number width for right alignment
            let max_line_num = total_lines;
            let max_width = max_line_num.to_string().len();

            // Generate ALL line numbers with right-alignment padding
            let mut line_numbers_text = String::new();
            for line_num in 0..total_lines as usize {
                if line_num > 0 {
                    line_numbers_text.push('\n');
                }
                let num_str = (line_num + 1).to_string();
                // Pad with spaces for right alignment
                let padding = max_width - num_str.len();
                for _ in 0..padding {
                    line_numbers_text.push(' ');
                }
                line_numbers_text.push_str(&num_str);
            }

            // Update TextView content
            self.text_view.set_text(&line_numbers_text);

            // Update cache
            self.last_total_lines = Some(total_lines);
        }

        // Update TextView viewport metrics and scroll FIRST (before layout)
        self.text_view.viewport.scale_factor = scale_factor;
        self.text_view.viewport.metrics.font_size = font_size;
        self.text_view.viewport.metrics.line_height = line_height;

        // Set bounds and scroll before update_layout so visible range calculation is correct
        let bounds_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
        let bounds_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);

        self.text_view.viewport.bounds = Rect {
            x: LogicalPixels(bounds_x),
            y: LogicalPixels(bounds_y),
            width: LogicalPixels(self.width),
            height: LogicalPixels(viewport_height),
        };

        self.text_view.viewport.scroll.x = LogicalPixels(0.0);
        self.text_view.viewport.scroll.y = LogicalPixels(scroll_y);

        // Update layout (this calls update_visible_range with correct scroll)
        self.text_view.update_layout(&font_service);

        // Recalculate width from actual measured glyphs if content or font size changed
        if content_changed || font_size_changed {
            // intrinsic_width includes horizontal padding (8px left + 8px right)
            self.width = self.text_view.intrinsic_width();
            self.last_font_size = Some(font_size);
        }

        let mut glyphs = self.text_view.collect_glyphs(&font_service);

        // Set token_id for line numbers and add to collector
        for glyph in &mut glyphs {
            glyph.token_id = 255;
        }

        collector.add_glyphs(glyphs);
    }
}

// === Plugin Trait Implementation ===

tiny_sdk::plugin! {
    LineNumbersPlugin {
        name: "line_numbers",
        version: "1.0.0",
        z_index: 100,
        traits: [Init, Paint],
        defaults: [Init, Paint],
    }
}
