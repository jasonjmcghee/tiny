//! Render command system - decouples widget painting from GPU execution
//!
//! Widgets emit commands, renderer batches and optimizes them for GPU

use crate::coordinates::{DocPos, Viewport};
use crate::tree::{Node, Point, Rect, Span, Tree, Widget};
use std::sync::Arc;

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

/// Single glyph instance
#[derive(Clone, Copy, Debug)]
pub struct GlyphInstance {
    pub glyph_id: u16,
    pub x: f32,
    pub y: f32,
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
        use crate::coordinates::LogicalSize;

        Self {
            commands: Vec::with_capacity(1000),
            clip_stack: Vec::new(),
            transform: Transform { x: 0.0, y: 0.0 },
            text_styles: None,
            font_system: None,
            viewport: Viewport::new(LogicalSize { width: size.0, height: size.1 }, scale_factor),
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
        use crate::coordinates::LogicalSize;
        self.viewport.resize(LogicalSize { width, height }, scale_factor);
    }

    /// Get reference to viewport (for testing)
    #[cfg(test)]
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// Render tree to commands
    pub fn render(&mut self, tree: &Tree, viewport: Rect) -> Vec<BatchedDraw> {
        println!("Renderer::render called with viewport: {:?}", viewport);

        // Clear previous frame
        self.commands.clear();
        self.clip_stack.clear();
        self.transform = Transform {
            x: viewport.x,
            y: viewport.y,
        };

        // Reset document position for new frame
        self.current_doc_pos = DocPos::default();

        // Walk visible tree portion
        self.walk_node(&tree.root, viewport);

        println!("Generated {} render commands", self.commands.len());

        // Batch and optimize commands
        self.batch_commands()
    }

    /// Walk tree node, emitting commands
    fn walk_node(&mut self, node: &Node, clip: Rect) {
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
                    let line_y = (self.current_doc_pos.line as f32) * self.viewport.line_height;
                    let text = std::str::from_utf8(&coalesced_text).unwrap_or("");

                    println!("  Rendering coalesced text ({} bytes) at pixel pos: (0.0, {:.1})",
                             coalesced_text.len(), line_y);
                    println!("    First 100 chars: '{}'",
                             text.chars().take(100).collect::<String>());

                    self.render_text(&coalesced_text, 0.0, line_y);
                    self.current_doc_pos.byte += coalesced_text.len();
                    self.current_doc_pos.line += total_lines;
                }

                // Handle widgets separately (simplified for now)
                for span in spans {
                    match span {
                        Span::Text { .. } => {
                            // Already handled above


                        }
                        Span::Widget(widget) => {
                            let line_y = (self.current_doc_pos.line as f32) * self.viewport.line_height;
                            // For now, widgets render at position 0
                            self.render_widget(widget.as_ref(), 0.0, line_y);
                        }
                        Span::Selection {
                            range,
                            id: _,
                            is_cursor,
                        } => {
                            // For cursor/selection, we need to calculate the x position based on byte offset
                            // This is simplified - real implementation would measure text up to cursor position
                            let line_y = (self.current_doc_pos.line as f32) * self.viewport.line_height;

                            if *is_cursor {
                                // For now, render cursor at byte offset position
                                // TODO: Calculate actual x position from byte offset in coalesced text
                                let cursor_x = 0.0; // Would calculate from range.start
                                self.render_cursor(cursor_x, line_y);
                            } else {
                                // Render selection
                                self.render_selection(range.clone(), 0.0, line_y);
                            }
                        }
                    }
                }

            }
            Node::Internal { children, .. } => {
                for child in children {
                    // Check if child is visible
                    let child_bounds = self.get_node_bounds(child);
                    if Self::rects_intersect(&child_bounds, &clip) {
                        self.walk_node(child, clip);
                    }
                }
            }
        }
    }

    /// Render text span
    fn render_text(&mut self, bytes: &[u8], x: f32, y: f32) {
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
    fn render_widget(&mut self, widget: &dyn Widget, x: f32, y: f32) {
        // Create paint context
        let mut paint_ctx = crate::tree::PaintContext {
            position: Point { x, y },
            commands: &mut self.commands,
            text_styles: self.text_styles.as_deref(),
            font_system: self.font_system.as_ref(),
            viewport: &self.viewport,
        };

        // Let the widget paint itself
        widget.paint(&mut paint_ctx);
    }

    /// Render cursor
    fn render_cursor(&mut self, x: f32, y: f32) {
        self.commands.push(RenderOp::Rect {
            rect: Rect {
                x,
                y,
                width: 2.0,
                height: 20.0,
            },
            color: 0xFFFFFFFF,
        });
    }

    /// Render selection highlight
    fn render_selection(&mut self, _range: std::ops::Range<usize>, x: f32, y: f32) {
        // Calculate selection bounds
        let width = 100.0; // Would calculate from text
        self.commands.push(RenderOp::Rect {
            rect: Rect {
                x,
                y,
                width,
                height: 20.0,
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

    /// Batch commands for efficient GPU submission
    fn batch_commands(&self) -> Vec<BatchedDraw> {
        let mut batches = Vec::new();
        let mut current_glyphs = Vec::new();
        let mut current_rects = Vec::new();

        for cmd in &self.commands {
            match cmd {
                RenderOp::Glyphs { glyphs, .. } => {
                    // Add to glyph batch
                    current_glyphs.extend_from_slice(glyphs);
                }
                RenderOp::Rect { rect, color } => {
                    // Flush glyphs if any
                    if !current_glyphs.is_empty() {
                        batches.push(BatchedDraw::GlyphBatch {
                            instances: std::mem::take(&mut current_glyphs),
                            texture: 0,
                        });
                    }
                    // Add to rect batch
                    current_rects.push(RectInstance {
                        rect: *rect,
                        color: *color,
                    });
                }
                RenderOp::PushClip(rect) => {
                    // Flush current batches
                    Self::flush_batches(&mut batches, &mut current_glyphs, &mut current_rects);
                    batches.push(BatchedDraw::SetClip(*rect));
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
