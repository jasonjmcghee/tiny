//! Renderer manages widget rendering and viewport transformations
//!
//! Converts document tree to widgets and coordinates their GPU rendering

use crate::coordinates::{LayoutPos, LayoutRect, LogicalSize, Viewport};
use crate::text_renderer::TextRenderer;
use crate::tree::{Rect, Tree};
use crate::widget::WidgetManager;
use std::sync::Arc;
#[allow(unused)]
use wgpu::hal::{DynCommandEncoder, DynDevice, DynQueue};

// === Glyph Instances ===

/// Single glyph instance (in layout space, logical pixels)
#[derive(Clone, Debug)]
pub struct GlyphInstance {
    pub glyph_id: u16,
    pub pos: LayoutPos, // Layout space position (logical pixels)
    pub color: u32,
    pub tex_coords: [f32; 4], // [u0, v0, u1, v1] in atlas
    pub token_id: u8,         // Token type for theme lookup
    pub relative_pos: f32,    // Position within token (0.0-1.0)
}

// RectInstance still used directly by widgets for rendering
#[derive(Clone, Copy)]
pub struct RectInstance {
    pub rect: Rect,
    pub color: u32,
}

// === Renderer ===

/// Converts tree to widgets and manages rendering
pub struct Renderer {
    /// Text style provider for syntax highlighting
    pub text_styles: Option<Box<dyn crate::text_effects::TextStyleProvider>>,
    /// Syntax highlighter for viewport queries (optional)
    pub syntax_highlighter: Option<Arc<crate::syntax::SyntaxHighlighter>>,
    /// Font system for text rendering (shared reference)
    pub font_system: Option<std::sync::Arc<crate::font::SharedFontSystem>>,
    /// Viewport for coordinate transformation
    pub viewport: Viewport,
    /// GPU renderer reference for widget painting
    gpu_renderer: Option<*const crate::gpu::GpuRenderer>,
    /// Cached document text for syntax queries
    pub cached_doc_text: Option<String>,
    /// Cached document version
    pub cached_doc_version: u64,
    /// Widget manager for overlay widgets
    pub widget_manager: WidgetManager,
    /// New decoupled text renderer
    pub text_renderer: TextRenderer,
    /// Last rendered document version for change detection
    last_rendered_version: u64,
    /// Whether layout needs updating due to viewport/font changes
    layout_dirty: bool,
    /// Whether syntax needs updating due to highlighter changes
    syntax_dirty: bool,
}

// SAFETY: Renderer is Send + Sync because the GPU renderer pointer
// is only used during render calls which happen on the same thread
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        Self {
            text_styles: None,
            syntax_highlighter: None,
            font_system: None,
            viewport: Viewport::new(size.0, size.1, scale_factor),
            gpu_renderer: None,
            cached_doc_text: None,
            cached_doc_version: 0,
            widget_manager: WidgetManager::new(),
            text_renderer: TextRenderer::new(),
            last_rendered_version: 0,
            layout_dirty: true, // Start dirty to ensure first render happens
            syntax_dirty: false,
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.viewport.set_font_size(font_size);
        self.layout_dirty = true; // Layout needs updating when font size changes
    }

    /// Set GPU renderer reference for widget painting and initialize theme
    pub fn set_gpu_renderer(&mut self, gpu_renderer: &crate::gpu::GpuRenderer) {
        self.gpu_renderer = Some(gpu_renderer as *const _);
        // Theme initialization is now handled in app.rs
    }

    /// Set text style provider (takes ownership)
    pub fn set_text_styles(&mut self, provider: Box<dyn crate::text_effects::TextStyleProvider>) {
        self.text_styles = Some(provider);
    }

    /// Set syntax highlighter for viewport queries
    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<crate::syntax::SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
        self.syntax_dirty = true; // Syntax needs updating when highlighter changes
    }

    /// Set font system (takes shared reference)
    pub fn set_font_system(&mut self, font_system: std::sync::Arc<crate::font::SharedFontSystem>) {
        // Set font system on viewport for accurate measurements
        self.viewport.set_font_system(font_system.clone());
        self.font_system = Some(font_system);
        self.layout_dirty = true; // Layout needs updating when font system changes
    }

    /// Handle incremental edit for stable typing experience
    pub fn apply_incremental_edit(&mut self, edit: &crate::tree::Edit) {
        self.text_renderer.apply_incremental_edit(edit);
    }

    /// Update viewport size
    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.viewport.resize(width, height, scale_factor);
        self.layout_dirty = true; // Layout needs updating when viewport changes
    }

    /// Set selections and cursor widgets
    pub fn set_selection_widgets(
        &mut self,
        input_handler: &crate::input::InputHandler,
        doc: &crate::tree::Doc,
    ) {
        // Create widgets from current selections
        let (selection_widgets, cursor_widget) = input_handler.create_widgets(doc, &self.viewport);

        // Update widget manager
        self.widget_manager.set_selection_widgets(selection_widgets);
        if let Some(cursor) = cursor_widget {
            self.widget_manager.set_cursor_widget(cursor);
        }
    }

    /// Render tree directly to GPU via widgets
    pub fn render(
        &mut self,
        tree: &Tree,
        viewport: Rect,
        selections: &[crate::input::Selection],
        render_pass: &mut wgpu::RenderPass,
    ) {
        // Simply delegate to render_with_pass which will handle everything
        self.render_with_pass(tree, viewport, selections, Some(render_pass));
    }

    /// Render tree with direct GPU render pass for widgets
    pub fn render_with_pass(
        &mut self,
        tree: &Tree,
        viewport: Rect,
        selections: &[crate::input::Selection],
        render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        self.render_with_pass_and_context(tree, viewport, selections, render_pass, None);
    }

    /// Render tree with direct GPU render pass and optional widget paint context
    pub fn render_with_pass_and_context(
        &mut self,
        tree: &Tree,
        _viewport: Rect,
        _selections: &[crate::input::Selection],
        mut render_pass: Option<&mut wgpu::RenderPass>,
        widget_paint_context: Option<&crate::widget::PaintContext>,
    ) {
        // Early exit if nothing has changed - skip all expensive operations
        if tree.version == self.last_rendered_version && !self.layout_dirty && !self.syntax_dirty {
            return;
        }
        // Initialize TextRenderer - this MUST happen before walk_visible_range_with_pass
        // Update layout cache if text changed
        if let Some(font_system) = &self.font_system {
            self.text_renderer
                .update_layout(tree, font_system, &self.viewport);
        }

        // Update syntax highlighting
        if let Some(ref highlighter) = self.syntax_highlighter {
            // Check if syntax has caught up to document version
            let syntax_version = highlighter.cached_version();
            let doc_version = tree.version;
            let fresh_parse = syntax_version == doc_version;

            // Convert tree-sitter effects to token ranges
            let text = tree.flatten_to_string();
            let effects = highlighter.get_visible_effects(&text, 0..text.len());

            let mut tokens = Vec::new();
            let mut debug_first = true;
            for effect in effects {
                if let crate::text_effects::EffectType::Token(token_id) = effect.effect {
                    tokens.push(crate::text_renderer::TokenRange {
                        byte_range: effect.range.clone(),
                        token_id,
                    });
                }
            }

            // Pass fresh_parse flag so text_renderer knows whether to shift tokens
            self.text_renderer
                .update_syntax_from_tokens(&tokens, fresh_parse);
        }

        // Update visible range for culling
        self.text_renderer
            .update_visible_range(&self.viewport, tree);

        // Update cached doc text for syntax queries if it changed
        if self.cached_doc_text.is_none() || tree.version != self.cached_doc_version {
            self.cached_doc_text = Some(tree.flatten_to_string());
            self.cached_doc_version = tree.version;
        }

        // Paint selections BEFORE text
        if let Some(pass) = render_pass.as_deref_mut() {
            let widgets = self.widget_manager.widgets_in_order();
            if let Some(ctx) = widget_paint_context {
                for widget in widgets {
                    if widget.priority() < 0 {
                        widget.paint(ctx, pass);
                    }
                }
            } else if let (Some(gpu), Some(font)) = (self.gpu_renderer, &self.font_system) {
                let gpu_renderer = unsafe { &*gpu };
                let ctx = crate::widget::PaintContext::new(
                    &self.viewport,
                    gpu_renderer.device_arc(),
                    gpu_renderer.queue_arc(),
                    gpu_renderer.uniform_bind_group(),
                    gpu as *mut _,
                    font,
                    self.text_styles.as_deref(),
                );
                for widget in widgets {
                    if widget.priority() < 0 {
                        widget.paint(&ctx, pass);
                    }
                }
            }
        }

        // Walk visible range
        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        self.walk_visible_range_with_pass(tree, visible_range, render_pass.as_deref_mut());

        // Paint cursor and overlays AFTER text
        if let Some(pass) = render_pass.as_deref_mut() {
            let widgets = self.widget_manager.widgets_in_order();
            if let Some(ctx) = widget_paint_context {
                for widget in widgets {
                    if widget.priority() >= 0 {
                        widget.paint(ctx, pass);
                    }
                }
            } else if let (Some(gpu), Some(font)) = (self.gpu_renderer, &self.font_system) {
                let gpu_renderer = unsafe { &*gpu };
                let ctx = crate::widget::PaintContext::new(
                    &self.viewport,
                    gpu_renderer.device_arc(),
                    gpu_renderer.queue_arc(),
                    gpu_renderer.uniform_bind_group(),
                    gpu as *mut _,
                    font,
                    self.text_styles.as_deref(),
                );
                for widget in widgets {
                    if widget.priority() >= 0 {
                        widget.paint(&ctx, pass);
                    }
                }
            }
        }

        // Update version tracking and clear dirty flags after successful render
        self.last_rendered_version = tree.version;
        self.layout_dirty = false;
        self.syntax_dirty = false;
    }

    /// Walk visible range with direct GPU rendering using new TextRenderer
    fn walk_visible_range_with_pass(
        &mut self,
        tree: &Tree,
        byte_range: std::ops::Range<usize>,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        // Use the new TextRenderer for all text rendering
        if let Some(pass) = render_pass.as_mut() {
            if let (Some(gpu_renderer_ptr), Some(font_system)) =
                (self.gpu_renderer, &self.font_system)
            {
                let gpu_renderer = unsafe { &*gpu_renderer_ptr };

                // Get visible glyphs from TextRenderer with their token IDs and relative positions
                let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

                // Create a style buffer with ONLY the visible glyph token IDs (as u32 for shader)
                let visible_style_buffer: Vec<u32> =
                    visible_glyphs.iter().map(|g| g.token_id as u32).collect();

                // Upload the visible-only style buffer to GPU
                if gpu_renderer.has_styled_pipeline() {
                    let gpu_renderer_mut =
                        unsafe { &mut *(gpu_renderer_ptr as *mut crate::gpu::GpuRenderer) };
                    gpu_renderer_mut.upload_style_buffer_u32(&visible_style_buffer);
                }

                // Convert to GlyphInstances for GPU
                let mut glyph_instances = Vec::new();
                for glyph_data in visible_glyphs {
                    // Transform from layout to physical coordinates
                    let physical_pos = self
                        .viewport
                        .layout_to_physical(glyph_data.glyph_pos.layout_pos);

                    // Map token ID back to color for now (until we update the shader)
                    let color = match glyph_data.token_id {
                        1 => 0xC678DDFF, // Keyword
                        2 => 0x61AFEFFF, // Function
                        3 => 0xE5C07BFF, // Type
                        4 => 0x98C379FF, // String
                        5 => 0xD19A66FF, // Number
                        6 => 0x5C6370FF, // Comment
                        7 => 0x56B6C2FF, // Operator
                        8 => 0xABB2BFFF, // Punctuation
                        9 => 0xE06C75FF, // Attribute
                        _ => 0xFFFFFFFF, // Default white
                    };

                    glyph_instances.push(GlyphInstance {
                        glyph_id: 0,
                        pos: LayoutPos::new(physical_pos.x.0, physical_pos.y.0),
                        color,
                        tex_coords: glyph_data.glyph_pos.tex_coords,
                        token_id: glyph_data.token_id,
                        relative_pos: glyph_data.relative_pos,
                    });
                }

                // Render glyphs with styled pipeline if available
                if !glyph_instances.is_empty() {
                    let use_styled =
                        self.syntax_highlighter.is_some() && gpu_renderer.has_styled_pipeline();
                    gpu_renderer.draw_glyphs_styled(pass, &glyph_instances, use_styled);
                }

                // Still handle widgets separately
                tree.walk_visible_range(byte_range.clone(), |spans, _, _| {
                    for span in spans {
                        if let crate::tree::Span::Widget(widget) = span {
                            let ctx = crate::widget::PaintContext::new(
                                &self.viewport,
                                gpu_renderer.device_arc(),
                                gpu_renderer.queue_arc(),
                                gpu_renderer.uniform_bind_group(),
                                gpu_renderer_ptr as *mut _,
                                font_system,
                                self.text_styles.as_deref(),
                            );
                            widget.paint(&ctx, pass);
                        }
                    }
                });
            }
        }
    }

    #[allow(dead_code)]
    fn walk_visible_range_old(
        &mut self,
        tree: &Tree,
        byte_range: std::ops::Range<usize>,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        use crate::widget::Widget;

        // Collect all visible text WITHOUT culling
        let mut all_visible_bytes = Vec::new();
        tree.walk_visible_range(byte_range.clone(), |spans, _, _| {
            for span in spans {
                match span {
                    crate::tree::Span::Widget(widget) => {
                        // If we have a render pass and supporting resources, paint directly
                        if let Some(pass) = render_pass.as_mut() {
                            if let Some(font_system) = &self.font_system {
                                if let Some(gpu_renderer_ptr) = self.gpu_renderer {
                                    let gpu_renderer = unsafe { &*gpu_renderer_ptr };
                                    let device_arc = gpu_renderer.device_arc();
                                    let queue_arc = gpu_renderer.queue_arc();
                                    let uniform_bind_group = gpu_renderer.uniform_bind_group();

                                    // Create paint context for the widget
                                    let ctx = crate::widget::PaintContext::new(
                                        &self.viewport,
                                        device_arc,
                                        queue_arc,
                                        uniform_bind_group,
                                        gpu_renderer_ptr as *mut _,
                                        font_system,
                                        self.text_styles.as_deref(),
                                    );

                                    // Let widget paint directly to GPU
                                    widget.paint(&ctx, pass);
                                }
                            }
                        }
                    }
                    crate::tree::Span::Text { bytes, .. } => {
                        all_visible_bytes.extend_from_slice(bytes);
                    }
                }
            }
        });

        // Get viewport-specific syntax effects if we have a highlighter
        let visible_effects = if let Some(ref highlighter) = self.syntax_highlighter {
            // Always use the latest cached text which was updated in render()
            let doc_text = self
                .cached_doc_text
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("");

            // Only query effects if we have actual text and the range is valid
            if !doc_text.is_empty() && byte_range.end <= doc_text.len() {
                // Query ONLY the visible AST nodes - O(visible) instead of O(document)!
                let effects = highlighter.get_visible_effects(doc_text, byte_range.clone());
                Some(effects)
            } else {
                None
            }
        } else {
            None
        };

        // Render ALL visible text as a single TextWidget WITHOUT culling
        // This preserves the 1:1 byte mapping for syntax highlighting
        if !all_visible_bytes.is_empty() && render_pass.is_some() {
            use crate::widget::{ContentType, TextWidget};
            use std::sync::Arc;

            // Create a TextWidget for ALL visible text (no culling)
            let text_widget = TextWidget {
                text: Arc::from(all_visible_bytes.as_slice()),
                style: 0,
                size: LogicalSize::new(10000.0, 1000.0), // Large enough for any content
                content_type: ContentType::Full,
                original_byte_offset: byte_range.start,
            };

            // Paint the widget directly
            if let Some(pass) = render_pass.as_mut() {
                if let Some(font_system) = &self.font_system {
                    if let Some(gpu_renderer_ptr) = self.gpu_renderer {
                        let gpu_renderer = unsafe { &*gpu_renderer_ptr };
                        let device_arc = gpu_renderer.device_arc();
                        let queue_arc = gpu_renderer.queue_arc();
                        let uniform_bind_group = gpu_renderer.uniform_bind_group();

                        // Create a custom text style provider that returns our viewport-specific effects
                        let viewport_style_provider = if let Some(effects) = visible_effects {
                            Some(ViewportEffectsProvider {
                                effects,
                                byte_offset: byte_range.start,
                            })
                        } else {
                            None
                        };

                        // Use the InputEdit-aware syntax highlighter for text styles
                        let text_styles_for_widget = if let Some(ref syntax_hl) =
                            self.syntax_highlighter
                        {
                            // Use the syntax highlighter directly (it implements TextStyleProvider)
                            // This ensures widgets get InputEdit-aware effects
                            Some(syntax_hl.as_ref() as &dyn crate::text_effects::TextStyleProvider)
                        } else if let Some(ref viewport_provider) = viewport_style_provider {
                            // Use viewport-specific effects if available
                            Some(viewport_provider as &dyn crate::text_effects::TextStyleProvider)
                        } else {
                            // Fallback to static text styles
                            self.text_styles.as_deref()
                        };

                        let ctx = crate::widget::PaintContext::new(
                            &self.viewport,
                            device_arc,
                            queue_arc,
                            uniform_bind_group,
                            gpu_renderer_ptr as *mut _,
                            font_system,
                            text_styles_for_widget,
                        );

                        text_widget.paint(&ctx, pass);
                    }
                }
            }
        }
    }

    /// Update animation for overlay widgets
    pub fn update_widgets(&mut self, dt: f32) -> bool {
        self.widget_manager.update_all(dt)
    }

    /// Get widget manager for manual widget painting
    pub fn widget_manager(&self) -> &WidgetManager {
        &self.widget_manager
    }

    /// Get mutable widget manager for manual widget painting
    pub fn widget_manager_mut(&mut self) -> &mut WidgetManager {
        &mut self.widget_manager
    }

    /// Update widgets from current selections
    pub fn update_widgets_from_selections(&mut self, selections: &[crate::input::Selection]) {
        // Clear existing widgets
        self.widget_manager.clear();

        // Create widgets from selections
        let mut cursor_widget = None;
        let mut selection_widgets = Vec::new();

        for selection in selections {
            if selection.is_cursor() {
                // Create cursor widget
                let layout_pos = self.viewport.doc_to_layout(selection.cursor);
                cursor_widget = Some(crate::widget::cursor(layout_pos));
            } else {
                // Create selection widget
                let start_layout = self.viewport.doc_to_layout(selection.anchor);
                let end_layout = self.viewport.doc_to_layout(selection.cursor);

                // Simple single rectangle for now
                let (min_x, max_x) = if start_layout.x.0 < end_layout.x.0 {
                    (start_layout.x.0, end_layout.x.0)
                } else {
                    (end_layout.x.0, start_layout.x.0)
                };
                let (min_y, max_y) = if start_layout.y.0 < end_layout.y.0 {
                    (
                        start_layout.y.0,
                        end_layout.y.0 + self.viewport.metrics.line_height,
                    )
                } else {
                    (
                        end_layout.y.0,
                        start_layout.y.0 + self.viewport.metrics.line_height,
                    )
                };

                let rect = LayoutRect::new(min_x, min_y, max_x - min_x, max_y - min_y);
                selection_widgets.push(crate::widget::selection(vec![rect]));
            }
        }

        // Add widgets to manager
        self.widget_manager.set_selection_widgets(selection_widgets);
        if let Some(cursor) = cursor_widget {
            self.widget_manager.set_cursor_widget(cursor);
        }
    }
}

// === Viewport Effects Provider (simplified) ===
struct ViewportEffectsProvider {
    effects: Vec<crate::text_effects::TextEffect>,
    byte_offset: usize,
}

impl crate::text_effects::TextStyleProvider for ViewportEffectsProvider {
    fn get_effects_in_range(
        &self,
        range: std::ops::Range<usize>,
    ) -> Vec<crate::text_effects::TextEffect> {
        let doc_range = (range.start + self.byte_offset)..(range.end + self.byte_offset);
        self.effects
            .iter()
            .filter(|e| e.range.start < doc_range.end && e.range.end > doc_range.start)
            .map(|e| crate::text_effects::TextEffect {
                range: e.range.start.saturating_sub(self.byte_offset)
                    ..e.range.end.saturating_sub(self.byte_offset),
                effect: e.effect.clone(),
                priority: e.priority,
            })
            .collect()
    }
    fn request_update(&self, _: &str, _: u64) {}
    fn name(&self) -> &str {
        "viewport_effects"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
