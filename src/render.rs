//! Render command system - decouples widget painting from GPU execution
//!
//! Widgets emit commands, renderer batches and optimizes them for GPU

use crate::coordinates::{DocPos, LayoutPos, LayoutRect, LogicalPixels, LogicalSize, Viewport};
use crate::text_effects::TextStyleProvider;
use crate::tree::{Node, Point, Rect, Span, Tree};
use crate::widget::Widget;
use std::sync::Arc;
#[allow(unused)]
use wgpu::hal::{DynCommandEncoder, DynDevice, DynQueue};

// === Render Commands ===
/// High-level rendering operations
#[derive(Clone, Debug)]
pub enum RenderOp {
    /// Draw glyphs at position
    Glyphs {
        glyphs: Arc<[GlyphInstance]>,
        style: u32,
    },
    /// Draw filled rectangle
    Rect { rect: Rect, color: u32 },
    /// Draw line (for underlines, etc)
    Line {
        from: Point,
        to: Point,
        color: u32,
        width: f32,
    },
    /// Push clipping rectangle
    PushClip(Rect),
    /// Pop clipping rectangle
    PopClip,
    /// Custom GPU draw
    Custom { pipeline: u32, data: Arc<[u8]> },
}

/// Single glyph instance (in layout space, logical pixels)
#[derive(Clone, Debug)]
pub struct GlyphInstance {
    pub glyph_id: u16,
    pub pos: LayoutPos, // Layout space position (logical pixels)
    pub color: u32,
    pub tex_coords: [f32; 4], // [u0, v0, u1, v1] in atlas
}

/// Batched draw call for GPU
pub enum BatchedDraw {
    /// Multiple glyphs in one draw call
    GlyphBatch {
        instances: Vec<GlyphInstance>,
        texture: u32,
    },
    /// Multiple rects
    RectBatch { instances: Vec<RectInstance> },
    /// State change
    SetClip(Rect),
}

#[derive(Clone, Copy)]
pub struct RectInstance {
    pub rect: Rect,
    pub color: u32,
}

// === Renderer ===

/// Converts tree to render commands
pub struct Renderer {
    /// Current render commands
    commands: Vec<RenderOp>,
    /// Clip stack
    clip_stack: Vec<Rect>,
    /// Current transform
    transform: Transform,
    /// Text style provider for syntax highlighting
    text_styles: Option<Box<dyn crate::text_effects::TextStyleProvider>>,
    /// Syntax highlighter for viewport queries (optional)
    syntax_highlighter: Option<Arc<crate::syntax::SyntaxHighlighter>>,
    /// Font system for text rendering (shared reference)
    font_system: Option<std::sync::Arc<crate::font::SharedFontSystem>>,
    /// Viewport for coordinate transformation
    viewport: Viewport,
    /// Current document position for rendering
    current_doc_pos: DocPos,
    /// GPU renderer reference for widget painting
    gpu_renderer: Option<*const crate::gpu::GpuRenderer>,
    /// Cached document text for syntax queries
    cached_doc_text: Option<String>,
    /// Cached document version
    cached_doc_version: u64,
}

#[derive(Clone, Copy)]
struct Transform {
    #[allow(dead_code)]
    x: f32,
    #[allow(dead_code)]
    y: f32,
}

// SAFETY: Renderer is Send + Sync because the GPU renderer pointer
// is only used during render calls which happen on the same thread
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        Self {
            commands: Vec::with_capacity(1000),
            clip_stack: Vec::new(),
            transform: Transform { x: 0.0, y: 0.0 },
            text_styles: None,
            syntax_highlighter: None,
            font_system: None,
            viewport: Viewport::new(size.0, size.1, scale_factor),
            current_doc_pos: DocPos::default(),
            gpu_renderer: None,
            cached_doc_text: None,
            cached_doc_version: 0,
        }
    }

    /// Set GPU renderer reference for widget painting
    pub fn set_gpu_renderer(&mut self, gpu_renderer: &crate::gpu::GpuRenderer) {
        self.gpu_renderer = Some(gpu_renderer as *const _);
    }

    /// Get GPU renderer reference
    fn gpu_renderer(&self) -> Option<&crate::gpu::GpuRenderer> {
        self.gpu_renderer.map(|ptr| unsafe { &*ptr })
    }

    /// Set text style provider (takes ownership)
    pub fn set_text_styles(&mut self, provider: Box<dyn crate::text_effects::TextStyleProvider>) {
        self.text_styles = Some(provider);
    }

    /// Set syntax highlighter for viewport queries
    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<crate::syntax::SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
    }

    /// Set text style provider (borrows)
    pub fn set_text_styles_ref(&mut self, provider: &dyn crate::text_effects::TextStyleProvider) {
        // We can't store a borrowed reference, but we can clone the effects for this frame
        // This is a temporary solution - ideally we'd pass the provider through the paint context
        println!(
            "DEBUG: set_text_styles_ref called with provider: {}",
            provider.name()
        );

        // Get all effects from the provider (this is a hack, but works for now)
        let all_effects = provider.get_effects_in_range(0..usize::MAX);
        println!("DEBUG: Got {} effects from provider", all_effects.len());

        // Create a simple provider that returns these effects
        struct StaticEffects {
            effects: Vec<crate::text_effects::TextEffect>,
        }

        impl crate::text_effects::TextStyleProvider for StaticEffects {
            fn get_effects_in_range(
                &self,
                range: std::ops::Range<usize>,
            ) -> Vec<crate::text_effects::TextEffect> {
                let result: Vec<_> = self
                    .effects
                    .iter()
                    .filter(|e| e.range.start < range.end && e.range.end > range.start)
                    .cloned()
                    .collect();

                static mut DEBUG_COUNT: u32 = 0;
                unsafe {
                    if DEBUG_COUNT < 5 && !result.is_empty() {
                        println!(
                            "StaticEffects: Returning {} effects for range {}..{} (total: {})",
                            result.len(),
                            range.start,
                            range.end,
                            self.effects.len()
                        );
                        if let Some(first) = result.first() {
                            println!("  First effect in range: {:?}", first);
                        }
                        DEBUG_COUNT += 1;
                    }
                }

                result
            }

            fn request_update(&self, _text: &str, _version: u64) {}

            fn name(&self) -> &str {
                "static_effects"
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        self.text_styles = Some(Box::new(StaticEffects {
            effects: all_effects,
        }));
    }

    /// Set font system (takes shared reference)
    pub fn set_font_system(&mut self, font_system: std::sync::Arc<crate::font::SharedFontSystem>) {
        // Set font system on viewport for accurate measurements
        self.viewport.set_font_system(font_system.clone());
        self.font_system = Some(font_system);
    }

    /// Cache document text for syntax queries (optional optimization)
    pub fn set_cached_doc_text(&mut self, text: String) {
        self.cached_doc_text = Some(text);
    }

    fn cached_doc_version(&self) -> u64 {
        self.cached_doc_version
    }

    /// Set physical font size for crisp rendering
    pub fn set_physical_font_size(&mut self, _physical_size: f32) {
        // For now, store this for future use
        // The widget will need to know what size to request from font system
    }

    /// Update viewport size
    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.viewport.resize(width, height, scale_factor);
    }

    /// Get reference to viewport
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Get mutable reference to viewport
    pub fn viewport_mut(&mut self) -> &mut Viewport {
        &mut self.viewport
    }

    /// Render tree to commands (or directly to GPU for widgets)
    pub fn render(
        &mut self,
        tree: &Tree,
        viewport: Rect,
        selections: &[crate::input::Selection],
    ) -> Vec<BatchedDraw> {
        // Update cached doc text for syntax queries if it changed
        if self.cached_doc_text.is_none() || tree.version != self.cached_doc_version() {
            self.cached_doc_text = Some(tree.to_string());
            self.cached_doc_version = tree.version;
        }

        self.render_with_pass(tree, viewport, selections, None)
    }

    /// Render tree with optional direct GPU render pass for widgets
    pub fn render_with_pass(
        &mut self,
        tree: &Tree,
        viewport: Rect,
        selections: &[crate::input::Selection],
        render_pass: Option<&mut wgpu::RenderPass>,
    ) -> Vec<BatchedDraw> {
        println!("Renderer::render called with viewport: {:?}", viewport);

        // Clear previous frame
        self.commands.clear();
        self.clip_stack.clear();
        self.transform = Transform {
            x: viewport.x.0,
            y: viewport.y.0,
        };

        // Reset document position for new frame
        self.current_doc_pos = DocPos::default();

        // Use the sum-tree visible range system we built
        println!("VISIBLE RANGE WALKING: Starting visible range rendering");
        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        println!(
            "  Visible byte range: {}..{}",
            visible_range.start, visible_range.end
        );

        if render_pass.is_some() {
            // Direct GPU rendering mode for widgets
            self.walk_visible_range_with_pass(tree, visible_range, render_pass);
        } else {
            // Command generation mode (legacy)
            self.walk_visible_range(tree, visible_range);
        }
        println!(
            "VISIBLE RANGE WALKING: Finished, found {} widgets total",
            self.commands.len()
        );

        // Render selections and cursors as overlays
        self.render_selections(selections, tree);

        println!("Generated {} render commands", self.commands.len());

        // Batch and optimize commands
        self.batch_commands()
    }

    /// Walk tree node, emitting commands
    fn walk_node(&mut self, node: &Node, clip: Rect) {
        self.walk_node_with_tree(node, clip, None);
    }

    /// Walk visible range with direct GPU rendering for widgets
    fn walk_visible_range_with_pass(
        &mut self,
        tree: &Tree,
        byte_range: std::ops::Range<usize>,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        use crate::widget::Widget;

        // Collect all visible text WITHOUT culling
        // We'll render the full visible text and let clipping handle the rest
        let mut all_visible_bytes = Vec::new();
        tree.walk_visible_range(byte_range.clone(), |spans, _, _| {
            for span in spans {
                match span {
                    crate::tree::Span::Widget(widget) => {
                        // If we have a render pass and supporting resources, paint directly
                        if let Some(pass) = render_pass.as_mut() {
                            if let (Some(font_system), Some(gpu_renderer)) =
                                (&self.font_system, self.gpu_renderer())
                            {
                                // Create paint context for the widget
                                let layout_pos = self.viewport.doc_to_layout(self.current_doc_pos);
                                let ctx = crate::widget::PaintContext {
                                    viewport: &self.viewport,
                                    device: gpu_renderer.device(),
                                    queue: gpu_renderer.queue(),
                                    uniform_bind_group: gpu_renderer.uniform_bind_group(),
                                    gpu_renderer,
                                    font_system,
                                    text_styles: self.text_styles.as_deref(),
                                    layout_pos,
                                };

                                // Let widget paint directly to GPU
                                widget.paint(&ctx, pass);
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
            let doc_text = self.cached_doc_text.as_ref().map(|s| s.as_str()).unwrap_or("");

            // Query ONLY the visible AST nodes - O(visible) instead of O(document)!
            static mut DEBUG_COUNT: u32 = 0;
            unsafe {
                if DEBUG_COUNT < 5 {
                    println!("VIEWPORT SYNTAX: Querying AST for byte range {}..{} (doc version {})",
                             byte_range.start, byte_range.end, tree.version);
                    DEBUG_COUNT += 1;
                }
            }

            let effects = highlighter.get_visible_effects(doc_text, byte_range.clone());

            static mut EFFECT_DEBUG_COUNT: u32 = 0;
            unsafe {
                if EFFECT_DEBUG_COUNT < 5 {
                    println!("VIEWPORT SYNTAX: Got {} effects for visible range", effects.len());
                    if !effects.is_empty() {
                        println!("  First effect: {:?}", effects.first());
                    }
                    EFFECT_DEBUG_COUNT += 1;
                }
            }

            Some(effects)
        } else {
            None
        };

        // Render ALL visible text as a single TextWidget WITHOUT culling
        // This preserves the 1:1 byte mapping for syntax highlighting
        if !all_visible_bytes.is_empty() && render_pass.is_some() {
            use crate::widget::{ContentType, TextWidget};
            use std::sync::Arc;

            let layout_pos = crate::coordinates::LayoutPos::new(
                self.viewport.margin.x.0,
                self.viewport.margin.y.0,
            );

            // Create a TextWidget for ALL visible text (no culling)
            let text_widget = TextWidget {
                text: Arc::from(all_visible_bytes.as_slice()),
                style: 0,
                size: crate::coordinates::LogicalSize::new(10000.0, 1000.0), // Large enough for any content
                content_type: ContentType::Full,
                original_byte_offset: byte_range.start,
            };

            // Paint the widget directly
            if let Some(pass) = render_pass.as_mut() {
                if let (Some(font_system), Some(gpu_renderer)) =
                    (&self.font_system, self.gpu_renderer())
                {
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
                    let text_styles_for_widget = if let Some(ref syntax_hl) = self.syntax_highlighter {
                        println!("WIDGET: Using live InputEdit-aware syntax highlighter");
                        // Use the syntax highlighter directly (it implements TextStyleProvider)
                        // This ensures widgets get InputEdit-aware effects
                        Some(syntax_hl.as_ref() as &dyn crate::text_effects::TextStyleProvider)
                    } else if let Some(ref viewport_provider) = viewport_style_provider {
                        println!("WIDGET: Using viewport-specific effects provider");
                        // Use viewport-specific effects if available
                        Some(viewport_provider as &dyn crate::text_effects::TextStyleProvider)
                    } else {
                        println!("WIDGET: Falling back to static text styles");
                        // Fallback to static text styles
                        self.text_styles.as_deref()
                    };

                    let ctx = crate::widget::PaintContext {
                        viewport: &self.viewport,
                        device: gpu_renderer.device(),
                        queue: gpu_renderer.queue(),
                        uniform_bind_group: gpu_renderer.uniform_bind_group(),
                        gpu_renderer,
                        font_system,
                        text_styles: text_styles_for_widget,
                        layout_pos,
                    };

                    text_widget.paint(&ctx, pass);
                }
            }
        }
    }

    /// Walk tree node with tree reference for cursor positioning
    /// Walk only the visible range using sum-tree navigation
    fn walk_visible_range(&mut self, tree: &Tree, byte_range: std::ops::Range<usize>) {
        // Collect all visible text WITHOUT culling
        let mut all_visible_bytes = Vec::new();
        tree.walk_visible_range(byte_range.clone(), |spans, _, _| {
            for span in spans {
                if let Span::Text { bytes, .. } = span {
                    all_visible_bytes.extend_from_slice(bytes);
                }
            }
        });

        // Render ALL visible text as a single widget WITHOUT culling
        // This preserves the 1:1 byte mapping for syntax highlighting
        if !all_visible_bytes.is_empty() {
            let layout_pos = LayoutPos::new(self.viewport.margin.x.0, self.viewport.margin.y.0);

            // Pass the full text with its original byte offset
            self.render_text_with_offset_and_byte(
                &all_visible_bytes,
                layout_pos.x,
                layout_pos.y,
                0.0, // No x_offset needed - horizontal scroll is handled by viewport
                0,   // No column offset
                byte_range.start,
            );
        }
    }

    fn walk_node_with_tree(&mut self, node: &Node, clip: Rect, tree: Option<&Tree>) {
        match node {
            Node::Leaf { spans, .. } => {
                println!("Walking leaf with {} spans", spans.len());

                // First, coalesce adjacent text spans to render as continuous text
                let mut coalesced_text = Vec::new();
                let mut total_lines = 0u32;

                // Collect all adjacent text spans
                for span in spans {
                    if let Span::Text { bytes, lines } = span {
                        coalesced_text.extend_from_slice(bytes);
                        total_lines += lines;
                    }
                }

                // Render the coalesced text as a single unit
                if !coalesced_text.is_empty() {
                    // Use viewport to transform document position to layout position
                    let layout_pos = self.viewport.doc_to_layout(self.current_doc_pos);
                    let text = std::str::from_utf8(&coalesced_text).unwrap_or("");

                    println!(
                        "  Rendering coalesced text ({} bytes) at layout pos: ({:.1}, {:.1})",
                        coalesced_text.len(),
                        layout_pos.x,
                        layout_pos.y
                    );
                    println!(
                        "    First 100 chars: '{}'",
                        text.chars().take(100).collect::<String>()
                    );

                    self.render_text(&coalesced_text, layout_pos.x, layout_pos.y);
                    self.current_doc_pos.byte_offset += coalesced_text.len();
                    self.current_doc_pos.line += total_lines;

                    // Reset column to 0 after newlines (simplified - would track properly)
                    if total_lines > 0 {
                        self.current_doc_pos.column = 0;
                    } else {
                        // Approximate column increment (would need actual char count)
                        self.current_doc_pos.column += coalesced_text.len() as u32;
                    }
                }

                // Handle widgets separately (simplified for now)
                for span in spans {
                    match span {
                        Span::Text { .. } => {
                            // Already handled above
                        }
                        Span::Widget(widget) => {
                            let layout_pos = self.viewport.doc_to_layout(self.current_doc_pos);
                            self.render_widget(widget.as_ref(), layout_pos.x, layout_pos.y);
                        }
                    }
                }
            }
            Node::Internal { children, .. } => {
                for child in children {
                    // Check if child is visible
                    let child_bounds = self.get_node_bounds(child);
                    if Self::rects_intersect(&child_bounds, &clip) {
                        self.walk_node_with_tree(child, clip, tree);
                    }
                }
            }
        }
    }

    /// Render text span (potentially multi-line for virtualized content)
    fn render_text(&mut self, bytes: &[u8], x: LogicalPixels, y: LogicalPixels) {
        self.render_text_with_offset(bytes, x, y, 0.0, 0);
    }

    /// Render text span with horizontal offset for scrolling
    fn render_text_with_offset(
        &mut self,
        bytes: &[u8],
        x: LogicalPixels,
        y: LogicalPixels,
        x_offset: f32,
        start_col: usize,
    ) {
        self.render_text_with_offset_and_byte(bytes, x, y, x_offset, start_col, 0);
    }

    /// Render text with offset and original byte position for syntax highlighting
    fn render_text_with_offset_and_byte(
        &mut self,
        bytes: &[u8],
        x: LogicalPixels,
        y: LogicalPixels,
        x_offset: f32,
        start_col: usize,
        original_byte_offset: usize,
    ) {
        use crate::widget::{ContentType, TextWidget};

        // Pre-calculate the actual size using the font system
        let text = std::str::from_utf8(bytes).unwrap_or("");
        let lines: Vec<&str> = text.lines().collect();

        let mut max_width = 0.0f32;
        let num_lines = lines.len().max(1);
        let total_height = num_lines as f32 * self.viewport.metrics.line_height;

        // Measure each line to find the maximum width
        if let Some(font_system) = &self.font_system {
            for line in &lines {
                if !line.is_empty() {
                    // Layout this line to get its width
                    let layout = font_system.layout_text_scaled(
                        line,
                        self.viewport.metrics.font_size,
                        self.viewport.scale_factor,
                    );
                    // Convert physical width to logical pixels
                    let line_width = layout.width / self.viewport.scale_factor;
                    max_width = max_width.max(line_width);
                }
            }
        }

        // If no font system or empty text, use a minimum width
        if max_width == 0.0 {
            max_width = 1.0;
        }

        // Use appropriate ContentType based on whether we have horizontal scrolling
        // The x_offset positions the culled text where it would have been in the full line
        let content_type = if x_offset != 0.0 || start_col > 0 {
            ContentType::Columns {
                start_col,
                x_offset, // Position text at its original location in the line
            }
        } else {
            ContentType::Full
        };

        let widget = TextWidget {
            text: Arc::from(bytes),
            style: 0,
            size: LogicalSize::new(max_width, total_height),
            content_type,
            original_byte_offset,
        };

        self.render_widget(&widget, x, y);
    }

    /// Render widget by converting it to render commands
    fn render_widget(&mut self, widget: &dyn Widget, x: LogicalPixels, y: LogicalPixels) {
        let layout_pos = LayoutPos { x, y };
        let view_pos = self.viewport.layout_to_view(layout_pos);

        // Only render if visible
        let widget_size = widget.measure();
        let widget_rect = LayoutRect {
            x: layout_pos.x,
            y: layout_pos.y,
            width: widget_size.width,
            height: widget_size.height,
        };

        // Check visibility for debug coloring
        // For horizontally culled text, the view_pos.x will be very negative if we have a large x_offset
        // In that case, we should consider it visible since we're showing the culled portion
        let is_visible = if view_pos.x.0 < -1000.0 {
            // This is likely culled text with large x_offset - it's visible by definition
            // (we extracted the visible columns)
            true
        } else {
            // Normal visibility check
            self.viewport.is_visible(widget_rect)
        };

        if !is_visible {
            return;
        }

        println!(
            "RENDERING WIDGET: layout=({:.1},{:.1}), scroll=({:.1},{:.1})",
            layout_pos.x.0, layout_pos.y.0, self.viewport.scroll.x.0, self.viewport.scroll.y.0
        );

        // Handle TextWidget conversion to RenderOp::Glyphs
        if let Some(text_widget) = widget.as_any().downcast_ref::<crate::widget::TextWidget>() {
            self.render_text_widget_to_commands(text_widget, layout_pos);
        }
        // Future: Handle other widget types here as needed
    }

    /// Convert TextWidget to RenderOp::Glyphs commands
    fn render_text_widget_to_commands(
        &mut self,
        text_widget: &crate::widget::TextWidget,
        layout_pos: LayoutPos,
    ) {
        use crate::widget::ContentType;

        let text = std::str::from_utf8(&text_widget.text).unwrap_or("");
        if text.is_empty() {
            return;
        }

        static mut DEBUG_COUNT: u32 = 0;
        unsafe {
            if DEBUG_COUNT < 3 {
                println!(
                    "render_text_widget_to_commands: text_styles = {}, text len = {}",
                    if self.text_styles.is_some() {
                        "Some"
                    } else {
                        "None"
                    },
                    text.len()
                );
                DEBUG_COUNT += 1;
            }
        }

        // Get the shared font system
        let font_system = if let Some(ref fs) = self.font_system {
            fs.clone()
        } else {
            println!("Warning: No font system available for TextWidget rendering");
            return;
        };

        // Use font size and scale from viewport metrics
        let font_size = self.viewport.metrics.font_size;
        let scale_factor = self.viewport.scale_factor;
        let line_height = self.viewport.metrics.line_height;

        // Handle multi-line text
        let lines: Vec<&str> = text.lines().collect();
        let mut all_glyph_instances = Vec::new();
        let mut y_offset = 0.0;
        let mut global_byte_pos = 0;

        // Handle different content types for horizontal scrolling
        let x_base_offset = match &text_widget.content_type {
            ContentType::Columns {
                x_offset,
                start_col,
            } => {
                if *x_offset != 0.0 {
                    println!(
                        "TextWidget applying x_offset={:.1} for columns starting at {}",
                        x_offset, start_col
                    );
                }
                *x_offset
            }
            _ => 0.0,
        };

        // Get ALL effects for the entire text ONCE (instead of per-character)
        // Use cached effects to avoid race condition with background parsing
        let all_effects = if let Some(ref text_styles) = self.text_styles {
            text_styles.get_effects_in_range(text_widget.original_byte_offset..(text_widget.original_byte_offset + text.len()))
        } else {
            Vec::new()
        };

        // TODO: Race condition issue - background parsing takes time but widget needs effects immediately
        // Options: 1) Synchronous parsing for edits, 2) Eventually consistent with stale effects

        for line_text in lines.iter() {
            // Layout this single line using the font system
            let layout = font_system.layout_text_scaled(line_text, font_size, scale_factor);

            let mut byte_pos = 0;
            for glyph in &layout.glyphs {
                let mut color = glyph.color;

                // Find the appropriate effect for this character (from pre-loaded effects)
                let char_bytes = glyph.char.len_utf8();
                let doc_char_pos = text_widget.original_byte_offset + global_byte_pos + byte_pos;

                // Find effect that contains this character position
                for effect in &all_effects {
                    if effect.range.start <= doc_char_pos && doc_char_pos < effect.range.end {

                        if let crate::text_effects::EffectType::Color(new_color) = effect.effect {
                            color = new_color;
                            break; // Use first matching effect
                        }
                    }
                }
                byte_pos += char_bytes;

                // Font system returns glyphs in physical pixels relative to (0,0)
                // Convert to logical and add layout position plus line offset and x_base_offset
                let glyph_logical_x = glyph.pos.x.0 / scale_factor;
                let glyph_logical_y = glyph.pos.y.0 / scale_factor;

                let glyph_pos = LayoutPos::new(
                    layout_pos.x.0 + x_base_offset + glyph_logical_x,
                    layout_pos.y.0 + y_offset + glyph_logical_y,
                );

                all_glyph_instances.push(GlyphInstance {
                    glyph_id: 0,    // Not used anymore
                    pos: glyph_pos, // In layout space (logical pixels)
                    color,
                    tex_coords: glyph.tex_coords,
                });
            }

            // Update position for next line
            global_byte_pos += line_text.len() + 1; // +1 for newline
            y_offset += line_height;
        }

        // Emit RenderOp::Glyphs command if we have glyphs
        if !all_glyph_instances.is_empty() {
            println!(
                "Emitting RenderOp::Glyphs with {} glyph instances from TextWidget",
                all_glyph_instances.len()
            );
            self.commands.push(RenderOp::Glyphs {
                glyphs: Arc::from(all_glyph_instances),
                style: text_widget.style,
            });
        }
    }

    /// Render cursor
    fn render_cursor(&mut self, x: LogicalPixels, y: LogicalPixels) {
        // x, y are already in layout space, create rect in layout coordinates
        // Shift cursor 2px to the left for better alignment
        self.commands.push(RenderOp::Rect {
            rect: Rect {
                x: x - 2.0,
                y,
                width: LogicalPixels(2.0),
                height: LogicalPixels(self.viewport.metrics.line_height),
            },
            color: 0xFFFFFFFF,
        });
    }

    /// Render all selections and cursors as overlays
    fn render_selections(&mut self, selections: &[crate::input::Selection], tree: &Tree) {
        for selection in selections {
            if selection.is_cursor() {
                // Get the line text for accurate cursor positioning
                let line_text = if let Some(line_start) = tree.line_to_byte(selection.cursor.line) {
                    let line_end = tree
                        .line_to_byte(selection.cursor.line + 1)
                        .unwrap_or(tree.byte_count());
                    tree.get_text_slice(line_start..line_end)
                } else {
                    String::new()
                };

                // Use accurate text-based positioning
                let layout_pos = self
                    .viewport
                    .doc_to_layout_with_text(selection.cursor, &line_text);
                println!("CURSOR DEBUG: DocPos=({}, {}), LayoutPos=({:.1}, {:.1}), scroll=({:.1}, {:.1}), line_height={:.1}",
                         selection.cursor.line, selection.cursor.column,
                         layout_pos.x.0, layout_pos.y.0,
                         self.viewport.scroll.x.0, self.viewport.scroll.y.0,
                         self.viewport.metrics.line_height);
                self.render_cursor(layout_pos.x, layout_pos.y);
            } else {
                // Render selection highlight
                let start_pos = self.viewport.doc_to_layout(selection.anchor);
                let end_pos = self.viewport.doc_to_layout(selection.cursor);
                self.render_selection_range(start_pos, end_pos);
            }
        }
    }

    /// Render selection highlight
    fn render_selection_range(&mut self, start: LayoutPos, end: LayoutPos) {
        // Simple single-line selection for now
        let x = LogicalPixels(start.x.0.min(end.x.0));
        let y = LogicalPixels(start.y.0.min(end.y.0));
        let width = LogicalPixels((end.x.0 - start.x.0).abs());

        self.commands.push(RenderOp::Rect {
            rect: Rect {
                x,
                y,
                width,
                height: LogicalPixels(self.viewport.metrics.line_height),
            },
            color: 0x4080FF80,
        });
    }

    /// Get node bounds
    fn get_node_bounds(&self, node: &Node) -> Rect {
        match node {
            Node::Leaf { sums, .. } => sums.bounds,
            Node::Internal { sums, .. } => sums.bounds,
        }
    }

    /// Check if rectangles intersect
    fn rects_intersect(a: &Rect, b: &Rect) -> bool {
        !(a.x + a.width < b.x
            || b.x + b.width < a.x
            || a.y + a.height < b.y
            || b.y + b.height < a.y)
    }

    /// Batch commands for efficient GPU submission (transforms layout → view space)
    fn batch_commands(&self) -> Vec<BatchedDraw> {
        let mut batches = Vec::new();
        let mut current_glyphs = Vec::new();
        let mut current_rects = Vec::new();

        for cmd in &self.commands {
            match cmd {
                RenderOp::Glyphs { glyphs, .. } => {
                    // Transform glyphs from layout to view space (apply scroll)
                    for glyph in glyphs.iter() {
                        // Glyphs are now in layout space (logical pixels)
                        // Apply scroll to get view position
                        let view_pos = self.viewport.layout_to_view(glyph.pos);
                        // Then convert to physical pixels for GPU
                        let physical_pos = self.viewport.view_to_physical(view_pos);

                        let transformed_glyph = GlyphInstance {
                            glyph_id: glyph.glyph_id,
                            pos: LayoutPos::new(physical_pos.x.0, physical_pos.y.0), // Store as physical for GPU
                            color: glyph.color,
                            tex_coords: glyph.tex_coords,
                        };
                        current_glyphs.push(transformed_glyph);
                    }
                }
                RenderOp::Rect { rect, color } => {
                    // Flush glyphs if any
                    if !current_glyphs.is_empty() {
                        batches.push(BatchedDraw::GlyphBatch {
                            instances: std::mem::take(&mut current_glyphs),
                            texture: 0,
                        });
                    }
                    // Transform rect from layout to view space
                    let layout_rect = LayoutRect {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    };
                    let view_rect = self.viewport.layout_rect_to_view(layout_rect);

                    current_rects.push(RectInstance {
                        rect: Rect {
                            x: view_rect.x,
                            y: view_rect.y,
                            width: view_rect.width,
                            height: view_rect.height,
                        },
                        color: *color,
                    });
                }
                RenderOp::PushClip(rect) => {
                    // Flush current batches
                    Self::flush_batches(&mut batches, &mut current_glyphs, &mut current_rects);

                    // Transform clip rect to view space
                    let layout_rect = LayoutRect {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    };
                    let view_rect = self.viewport.layout_rect_to_view(layout_rect);

                    batches.push(BatchedDraw::SetClip(Rect {
                        x: view_rect.x,
                        y: view_rect.y,
                        width: view_rect.width,
                        height: view_rect.height,
                    }));
                }
                _ => {}
            }
        }

        // Flush remaining
        Self::flush_batches(&mut batches, &mut current_glyphs, &mut current_rects);

        batches
    }

    fn flush_batches(
        batches: &mut Vec<BatchedDraw>,
        glyphs: &mut Vec<GlyphInstance>,
        rects: &mut Vec<RectInstance>,
    ) {
        if !glyphs.is_empty() {
            batches.push(BatchedDraw::GlyphBatch {
                instances: std::mem::take(glyphs),
                texture: 0,
            });
        }
        if !rects.is_empty() {
            batches.push(BatchedDraw::RectBatch {
                instances: std::mem::take(rects),
            });
        }
    }
}

// === GPU Backend ===

/// Temporary provider for viewport-specific effects
struct ViewportEffectsProvider {
    effects: Vec<crate::text_effects::TextEffect>,
    byte_offset: usize,
}

impl crate::text_effects::TextStyleProvider for ViewportEffectsProvider {
    fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<crate::text_effects::TextEffect> {
        // The range is relative to the visible text, but effects are in document coordinates
        // Adjust the range to document coordinates
        let doc_range = (range.start + self.byte_offset)..(range.end + self.byte_offset);

        self.effects.iter()
            .filter(|e| e.range.start < doc_range.end && e.range.end > doc_range.start)
            .filter_map(|e| {
                // Adjust effect range back to be relative to visible text
                let start = e.range.start.saturating_sub(self.byte_offset);
                let end = e.range.end.saturating_sub(self.byte_offset);

                // Ensure the range is valid (start <= end)
                if start <= end {
                    Some(crate::text_effects::TextEffect {
                        range: start..end,
                        effect: e.effect.clone(),
                        priority: e.priority,
                    })
                } else {
                    // Invalid range, skip this effect
                    None
                }
            })
            .collect()
    }

    fn request_update(&self, _text: &str, _version: u64) {
        // No-op for viewport provider
    }

    fn name(&self) -> &str {
        "viewport_effects"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// GPU command executor (simplified)
pub struct GpuBackend {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
}

impl GpuBackend {
    pub unsafe fn execute(&mut self, batches: &[BatchedDraw]) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            for batch in batches {
                match batch {
                    BatchedDraw::GlyphBatch { .. } => {
                        // Draw all glyphs in one call
                        // render_pass.draw_indexed(indices, instances);
                    }
                    BatchedDraw::RectBatch { instances: _ } => {
                        // Draw all rects
                        // render_pass.draw(vertices, instances);
                    }
                    BatchedDraw::SetClip(_rect) => {
                        // render_pass.set_scissor_rect(rect);
                    }
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
    }
}
