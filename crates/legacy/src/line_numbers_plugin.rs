//! Line numbers plugin - renders line numbers as a separate layer

use tiny_core::tree::{Doc, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::LogicalPixels;
use tiny_ui::{coordinates::Viewport, text_view::TextView};

/// Wrapper to make Doc pointer Send + Sync
#[derive(Copy, Clone, PartialEq, Eq)]
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
}

impl LineNumbersPlugin {
    pub fn new() -> Self {
        // Create a default viewport for the TextView (will be updated during rendering)
        let viewport = Viewport::new(100.0, 600.0, 1.0);

        Self {
            enabled: true,
            width: 0.0, // Will be calculated on first render
            line_number_offset: -50.0,
            doc: None,
            text_view: TextView::empty(viewport)
                .with_padding_x(8.0)  // Horizontal padding for spacing
                .with_padding_y(0.0)  // No vertical padding - align exactly with text
                .with_width(tiny_ui::text_view::SizeConstraint::HugContents)
                .with_height(tiny_ui::text_view::SizeConstraint::FillContainer),
            last_total_lines: None,
        }
    }

    /// Set the document reference
    pub fn set_document(&mut self, doc: &Doc) {
        let new_ptr = DocPtr(doc as *const _);
        // Only clear cache if document pointer actually changed
        if self.doc != Some(new_ptr) {
            self.doc = Some(new_ptr);
            self.last_total_lines = None;
        }
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

        let doc = match &self.doc {
            Some(DocPtr(doc_ptr)) => unsafe { &**doc_ptr },
            None => return,
        };

        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let widget_viewport = collector.widget_viewport.as_ref();
        let line_height = collector.viewport.line_height;
        let scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(0.0);

        let tree = doc.read();
        let total_lines = tree.line_count();

        let viewport_height = widget_viewport
            .map(|w| w.bounds.height.0)
            .unwrap_or(collector.viewport.logical_size.height.0);

        // Check if we need to regenerate line numbers text
        let content_changed = self.last_total_lines != Some(total_lines);

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

        // Configure viewport with current metrics
        collector.configure_viewport(&mut self.text_view.viewport);

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

        // Update layout (auto-detects metrics changes and rebuilds if needed)
        self.text_view.update_layout(&font_service);

        // Recalculate width only if layout actually changed
        if self.text_view.needs_render() {
            self.width = self.text_view.intrinsic_width();
        }

        let mut glyphs = self.text_view.collect_glyphs(&font_service);
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
