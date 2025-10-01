//! Line numbers plugin - renders line numbers as a separate layer

use ahash::AHasher;
use arc_swap::ArcSwap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tiny_core::tree::Doc;
use tiny_font::{create_glyph_instances, SharedFontSystem};
use tiny_sdk::{
    types::GlyphInstance, Capability, Initializable, LayoutPos, PaintContext, Paintable, Plugin,
    PluginError, SetupContext,
};

/// Wrapper to make Doc pointer Send + Sync
struct DocPtr(*const Doc);

unsafe impl Send for DocPtr {}
unsafe impl Sync for DocPtr {}

/// Cache key to determine if glyphs need regeneration
#[derive(Clone, Copy, PartialEq, Hash)]
struct CacheKey {
    first_visible_line: usize,
    last_visible_line: usize,
    total_lines: u32,
    font_size_bits: u32, // Store float as bits for hashing
    scale_factor_bits: u32,
    scroll_y: i32,
}

impl CacheKey {
    fn to_hash(&self) -> u64 {
        let mut hasher = AHasher::default();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// Plugin that renders line numbers
pub struct LineNumbersPlugin {
    /// Whether to show line numbers
    pub enabled: bool,
    // The width
    pub width: f32,
    /// X position for line numbers (offset from margin)
    pub line_number_offset: f32,
    /// Reference to the document (for line count)
    doc: Option<DocPtr>,
    /// Cached glyphs (lock-free reads with ArcSwap)
    cached_glyphs: ArcSwap<Vec<GlyphInstance>>,
    /// Cache key hash
    cache_key_hash: AtomicU64,
}

impl LineNumbersPlugin {
    pub fn new() -> Self {
        Self {
            enabled: true,
            width: 60.0,
            line_number_offset: -50.0, // Default offset to the left of text
            doc: None,
            cached_glyphs: ArcSwap::from_pointee(Vec::new()),
            cache_key_hash: AtomicU64::new(0),
        }
    }

    /// Set the document reference
    pub fn set_document(&mut self, doc: &Doc) {
        self.doc = Some(DocPtr(doc as *const _));
        // Invalidate cache when document changes
        self.cache_key_hash.store(0, Ordering::Relaxed);
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

        // Use widget viewport if available for proper bounds
        let widget_viewport = collector.widget_viewport.as_ref();

        let line_height = collector.viewport.line_height;
        let scale_factor = collector.viewport.scale_factor;
        // Use widget viewport's scroll if available
        let scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(0.0);
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

        // Create cache key for current state
        let current_key = CacheKey {
            first_visible_line,
            last_visible_line,
            total_lines,
            font_size_bits: font_size.to_bits(),
            scale_factor_bits: scale_factor.to_bits(),
            scroll_y: scroll_y as i32,
        };

        let current_hash = current_key.to_hash();

        // Check if we need to regenerate glyphs
        let needs_regenerate = self.cache_key_hash.load(Ordering::Relaxed) != current_hash;

        if needs_regenerate {
            // Get font service from service registry
            let font_service = match collector.services().get::<SharedFontSystem>() {
                Some(fs) => fs,
                None => return,
            };

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

            // Update cache
            self.cached_glyphs.store(Arc::new(line_number_glyphs));
            self.cache_key_hash.store(current_hash, Ordering::Relaxed);
        }

        // Use cached glyphs (lock-free read)
        let glyphs = self.cached_glyphs.load();
        collector.add_glyphs((**glyphs).clone());
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
        // No-op: We use collect_glyphs() for batched rendering in render.rs
        // This prevents double-rendering when the plugin is also loaded via plugin_loader
    }

    fn z_index(&self) -> i32 {
        100
    }
}
