//! Render command system - decouples widget painting from GPU execution
//!
//! Widgets emit commands, renderer batches and optimizes them for GPU

use crate::coordinates::{DocPos, LayoutPos, LayoutRect, LogicalPixels, Viewport};
use crate::tree::{Node, Point, Rect, Span, Tree, Widget};
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
    pub pos: LayoutPos,  // Layout space position (logical pixels)
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
    /// Font system for text rendering (shared reference)
    font_system: Option<std::sync::Arc<crate::font::SharedFontSystem>>,
    /// Viewport for coordinate transformation
    viewport: Viewport,
    /// Current document position for rendering
    current_doc_pos: DocPos,
}

#[derive(Clone, Copy)]
struct Transform {
    #[allow(dead_code)]
    x: f32,
    #[allow(dead_code)]
    y: f32,
}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        Self {
            commands: Vec::with_capacity(1000),
            clip_stack: Vec::new(),
            transform: Transform { x: 0.0, y: 0.0 },
            text_styles: None,
            font_system: None,
            viewport: Viewport::new(size.0, size.1, scale_factor),
            current_doc_pos: DocPos::default(),
        }
    }

    /// Set text style provider
    pub fn set_text_styles(&mut self, provider: Box<dyn crate::text_effects::TextStyleProvider>) {
        self.text_styles = Some(provider);
    }

    /// Set font system (takes shared reference)
    pub fn set_font_system(&mut self, font_system: std::sync::Arc<crate::font::SharedFontSystem>) {
        // Set font system on viewport for accurate measurements
        self.viewport.set_font_system(font_system.clone());
        self.font_system = Some(font_system);
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

    /// Render tree to commands
    pub fn render(&mut self, tree: &Tree, viewport: Rect, selections: &[crate::input::Selection]) -> Vec<BatchedDraw> {
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
        println!("  Visible byte range: {}..{}", visible_range.start, visible_range.end);

        self.walk_visible_range(tree, visible_range);
        println!("VISIBLE RANGE WALKING: Finished, found {} widgets total", self.commands.len());

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

    /// Walk tree node with tree reference for cursor positioning
    /// Walk only the visible range using sum-tree navigation
    fn walk_visible_range(&mut self, tree: &Tree, byte_range: std::ops::Range<usize>) {
        // Reset document position to start of visible range
        let start_line = tree.byte_to_line(byte_range.start);
        self.current_doc_pos.byte_offset = byte_range.start;
        self.current_doc_pos.line = start_line;
        self.current_doc_pos.column = 0; // Simplified - would calculate actual column

        // Use tree's efficient range walking
        tree.walk_visible_range(byte_range, |spans, span_start, span_end| {
            self.render_spans_at_position(spans, span_start, span_end);
        });
    }

    /// Render spans at their calculated position (called by walk_visible_range)
    fn render_spans_at_position(&mut self, spans: &[Span], span_start: usize, span_end: usize) {
        println!("RANGE WALKER FOUND: {} spans in byte range {}..{} at doc pos: ({}, {})",
                 spans.len(), span_start, span_end, self.current_doc_pos.line, self.current_doc_pos.column);

        // Collect text spans to render together (keep existing coalescing for efficiency)
        let mut coalesced_text = Vec::new();
        let mut total_lines = 0u32;

        for span in spans {
            if let Span::Text { bytes, lines } = span {
                coalesced_text.extend_from_slice(bytes);
                total_lines += lines;
            }
        }

        // Render the coalesced text if any
        if !coalesced_text.is_empty() {
            let layout_pos = self.viewport.doc_to_layout(self.current_doc_pos);
            let text = std::str::from_utf8(&coalesced_text).unwrap_or("");

            println!("  Rendering text chunk ({} bytes) at layout pos: ({:.1}, {:.1})",
                     coalesced_text.len(), layout_pos.x, layout_pos.y);

            self.render_text(&coalesced_text, layout_pos.x, layout_pos.y);

            // Update document position for next chunk
            self.current_doc_pos.byte_offset += coalesced_text.len();
            self.current_doc_pos.line += total_lines;

            if total_lines > 0 {
                self.current_doc_pos.column = 0;
            } else {
                self.current_doc_pos.column += text.chars().count() as u32;
            }
        }

        // Handle widgets (keep existing widget rendering logic)
        for span in spans {
            if let Span::Widget(widget) = span {
                let layout_pos = self.viewport.doc_to_layout(self.current_doc_pos);
                self.render_widget(widget.as_ref(), layout_pos.x, layout_pos.y);
            }
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

                    println!("  Rendering coalesced text ({} bytes) at layout pos: ({:.1}, {:.1})",
                             coalesced_text.len(), layout_pos.x, layout_pos.y);
                    println!("    First 100 chars: '{}'",
                             text.chars().take(100).collect::<String>());

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

    /// Render text span
    fn render_text(&mut self, bytes: &[u8], x: LogicalPixels, y: LogicalPixels) {
        // Text spans should be rendered via TextWidget, not directly
        // This is a fallback for raw text spans without widgets
        use crate::widget::TextWidget;

        let widget = TextWidget {
            text: Arc::from(bytes),
            style: 0,
        };

        self.render_widget(&widget, x, y);
    }

    /// Render widget
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

        // Check visibility for debug coloring but don't actually cull
        let is_visible = self.viewport.is_visible(widget_rect);
        // Don't cull anything - just use visibility for debug coloring

        println!("RENDERING WIDGET: layout=({:.1},{:.1}), scroll=({:.1},{:.1})",
                 layout_pos.x, layout_pos.y, self.viewport.scroll.x.0, self.viewport.scroll.y.0);

        // Create paint context with proper coordinate info
        let mut paint_ctx = crate::tree::PaintContext {
            layout_pos,
            view_pos: Some(view_pos),
            doc_pos: Some(self.current_doc_pos),
            commands: &mut self.commands,
            text_styles: self.text_styles.as_deref(),
            font_system: self.font_system.as_ref(),
            viewport: &self.viewport,
            debug_offscreen: !is_visible,
        };

        // Let the widget paint itself
        widget.paint(&mut paint_ctx);
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
                    let line_end = tree.line_to_byte(selection.cursor.line + 1).unwrap_or(tree.byte_count());
                    tree.get_text_slice(line_start..line_end)
                } else {
                    String::new()
                };

                // Use accurate text-based positioning
                let layout_pos = self.viewport.doc_to_layout_with_text(selection.cursor, &line_text);
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
                    BatchedDraw::GlyphBatch {  .. } => {
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
