//! Widget system where everything visual is a widget, including text
//!
//! Text rendering uses the consolidated FontSystem from font.rs

use crate::coordinates::{LayoutPos, LayoutRect, LogicalPixels, LogicalSize, Viewport};
use std::sync::Arc;

/// Widget identifier for texture access
pub type WidgetId = u64;

/// Widget event types
#[derive(Debug, Clone)]
pub enum WidgetEvent {
    MouseMove(LayoutPos),
    MouseEnter,
    MouseLeave,
    MouseClick(LayoutPos, winit::event::MouseButton),
    KeyboardInput(winit::event::KeyEvent, winit::event::Modifiers),
}

/// Event response from widgets
#[derive(Debug, Clone)]
pub enum EventResponse {
    Handled,
    Ignored,
    Redraw, // Widget needs redraw
}

/// Layout constraints for widgets
#[derive(Debug, Clone, Copy)]
pub struct LayoutConstraints {
    pub max_width: LogicalPixels,
    pub max_height: LogicalPixels,
}

/// Layout result from widget
#[derive(Debug, Clone, Copy)]
pub struct LayoutResult {
    pub size: LogicalSize,
}

/// Context passed to widgets during painting - with FULL GPU access
pub struct PaintContext<'a> {
    /// Viewport for all coordinate transformations and metrics
    pub viewport: &'a Viewport,
    /// GPU device for creating resources
    pub device: &'a wgpu::Device,
    /// GPU queue for uploading data
    pub queue: &'a wgpu::Queue,
    /// Uniform bind group for viewport uniforms
    pub uniform_bind_group: &'a wgpu::BindGroup,
    /// GPU renderer for access to pipelines and resources
    pub gpu_renderer: &'a crate::gpu::GpuRenderer,
    /// Font system for text layout and rasterization
    pub font_system: &'a std::sync::Arc<crate::font::SharedFontSystem>,
    /// Text style provider for syntax highlighting (optional)
    pub text_styles: Option<&'a dyn crate::text_effects::TextStyleProvider>,

    // Legacy fields for compatibility (will be removed)
    /// Widget's position in layout space (for simple widgets)
    pub layout_pos: LayoutPos,
}

// === Content Type for TextWidget ===

/// Describes what type of content a TextWidget contains
#[derive(Debug, Clone, Default)]
pub enum ContentType {
    /// Full lines (legacy, for non-virtualized content)
    #[default]
    Full,
    /// Extracted columns with horizontal offset (NoWrap mode)
    Columns {
        /// Starting column in the original line
        start_col: usize,
        /// X offset for rendering (negative for scrolled content)
        x_offset: f32,
    },
    /// Wrapped visual lines (SoftWrap mode)
    Wrapped {
        /// The visual lines after wrapping
        visual_lines: Vec<String>,
    },
}

// === Widget Implementations ===
/// Text widget - renders text using the consolidated FontSystem
pub struct TextWidget {
    /// UTF-8 text content
    pub text: Arc<[u8]>,
    /// Style ID for font/size/color
    pub style: StyleId,
    /// Pre-calculated size (measured with actual font system)
    pub size: LogicalSize,
    /// Type of content (full, columns, or wrapped)
    pub content_type: ContentType,
    /// Original byte offset in the document (for syntax highlighting)
    pub original_byte_offset: usize,
}

impl Clone for TextWidget {
    fn clone(&self) -> Self {
        Self {
            text: Arc::clone(&self.text),
            style: self.style,
            size: self.size,
            content_type: self.content_type.clone(),
            original_byte_offset: self.original_byte_offset,
        }
    }
}

/// Cursor widget - blinking text cursor
#[derive(Clone)]
pub struct CursorWidget {
    /// Style for cursor (color, width)
    pub style: CursorStyle,
    /// Animation state
    pub blink_phase: f32,
}

/// Selection widget - highlight for selected text
#[derive(Clone)]
pub struct SelectionWidget {
    /// Byte range of selection
    pub range: std::ops::Range<usize>,
    /// Selection color
    pub color: u32,
}

/// Line number widget
#[derive(Clone)]
pub struct LineNumberWidget {
    pub line: u32,
    pub style: StyleId,
}

/// Diagnostic widget - error/warning underline
#[derive(Clone)]
pub struct DiagnosticWidget {
    pub severity: Severity,
    pub message: Arc<str>,
    pub range: std::ops::Range<usize>,
}

/// Style widget - changes text appearance
#[derive(Clone)]
pub struct StyleWidget {
    /// Where style ends
    pub end_byte: usize,
    /// New style to apply
    pub style: StyleId,
}

// === Supporting Types ===

pub type StyleId = u32;

#[derive(Clone)]
pub struct CursorStyle {
    pub color: u32,
    pub width: f32,
}

#[derive(Clone, Copy)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

// === Core Widget Trait ===

/// Interactive widget trait - everything visual is a widget
pub trait Widget: Send + Sync {
    /// Unique widget identifier
    fn widget_id(&self) -> WidgetId;

    /// Update widget state (animations, etc.)
    fn update(&mut self, dt: f32) -> bool; // returns needs_redraw

    /// Handle input events
    fn handle_event(&mut self, event: &WidgetEvent) -> EventResponse;

    /// Layout widget with given constraints
    fn layout(&mut self, constraints: LayoutConstraints) -> LayoutResult;

    /// Paint widget - generate render commands or render directly to GPU
    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass);

    /// Widget's bounds for hit testing
    fn bounds(&self) -> LayoutRect;

    /// Render priority (higher values render later/on top)
    fn priority(&self) -> i32 {
        0 // Default priority
    }

    /// Whether this widget should clip its content to its bounds
    fn clips_to_bounds(&self) -> bool {
        true // Default: clip content to widget bounds
    }

    /// Check if point is inside widget
    fn contains_point(&self, point: LayoutPos) -> bool {
        self.bounds().contains(point)
    }

    /// Clone as trait object
    fn clone_box(&self) -> Arc<dyn Widget>;

    /// Get z-index for layering (compatibility)
    fn z_index(&self) -> i32 {
        self.priority()
    }

    /// Measure widget size (compatibility)
    fn measure(&self) -> LogicalSize {
        let bounds = self.bounds();
        LogicalSize::new(bounds.width.0, bounds.height.0)
    }

    /// Test if point hits this widget (compatibility)
    fn hit_test(&self, pt: LayoutPos) -> bool {
        self.contains_point(pt)
    }

    /// Downcast support for type-specific handling in render_widget()
    fn as_any(&self) -> &dyn std::any::Any;
}

// === Widget Implementations ===

impl Widget for TextWidget {
    fn widget_id(&self) -> WidgetId {
        0 // Text widgets don't need unique IDs for now
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored // Text doesn't handle events directly
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult { size: self.size }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(0.0, 0.0, self.size.width.0, self.size.height.0)
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        use crate::font::create_glyph_instances;

        let text = std::str::from_utf8(&self.text).unwrap_or("");
        if text.is_empty() {
            return;
        }

        // Handle different content types
        let x_base_offset = match &self.content_type {
            ContentType::Columns {
                x_offset,
                start_col: _,
            } => *x_offset,
            _ => 0.0,
        };

        let layout_pos = crate::coordinates::LayoutPos::new(
            ctx.layout_pos.x.0 + x_base_offset,
            ctx.layout_pos.y.0,
        );

        // Get effects for this text if available
        let effects = if let Some(text_styles) = ctx.text_styles {
            let text_range = self.original_byte_offset..(self.original_byte_offset + text.len());
            text_styles.get_effects_in_range(text_range)
        } else {
            Vec::new()
        };

        // Create glyph instances using the helper
        let mut all_glyph_instances = create_glyph_instances(
            ctx.font_system,
            text,
            layout_pos,
            ctx.viewport.metrics.font_size,
            ctx.viewport.scale_factor,
            ctx.viewport.metrics.line_height,
            if effects.is_empty() {
                None
            } else {
                Some(&effects)
            },
            self.original_byte_offset,
        );

        // Transform all glyphs from layout to physical coordinates for GPU
        for glyph in &mut all_glyph_instances {
            let physical_pos = ctx.viewport.layout_to_physical(glyph.pos);
            glyph.pos = crate::coordinates::LayoutPos::new(physical_pos.x.0, physical_pos.y.0);
        }

        // Check for shader effects in text styles and render with appropriate pipeline
        if !all_glyph_instances.is_empty() {
            let mut shader_id = None;

            // Scan text styles for shader effects AND apply color effects to glyphs
            if let Some(text_styles) = ctx.text_styles {
                let text_range = 0..std::str::from_utf8(&self.text).unwrap_or("").len();
                let effects = text_styles.get_effects_in_range(text_range);

                // Apply color effects to glyphs by updating their colors
                for effect in &effects {
                    match effect.effect {
                        crate::text_effects::EffectType::Color(color) => {
                            // Find glyphs in this effect's range and update their colors
                            let mut byte_pos = 0;
                            for glyph in &mut all_glyph_instances {
                                let char_len = std::str::from_utf8(&self.text)
                                    .unwrap_or("")
                                    .chars()
                                    .nth(byte_pos)
                                    .map(|c| c.len_utf8())
                                    .unwrap_or(1);

                                if byte_pos >= effect.range.start && byte_pos < effect.range.end {
                                    glyph.color = color;
                                }
                                byte_pos += char_len;
                            }
                        }
                        crate::text_effects::EffectType::Shader { id, ref params } => {
                            shader_id = Some(id);
                            // Update effect uniform buffer with shader parameters
                            if let Some(effect_buffer) = ctx.gpu_renderer.effect_uniform_buffer(id)
                            {
                                ctx.queue.write_buffer(
                                    effect_buffer,
                                    0,
                                    bytemuck::cast_slice(&**params),
                                );
                            }
                        }
                        // Ignore other effect types for now (weight, italic, etc.)
                        _ => {}
                    }
                }
            }

            // Render with or without shader effects
            if let Some(id) = shader_id {
                ctx.gpu_renderer
                    .draw_glyphs(render_pass, &all_glyph_instances, 1.0, Some(id));
            } else {
                ctx.gpu_renderer
                    .draw_glyphs(render_pass, &all_glyph_instances, 1.0, None);
            }
        }
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Widget for CursorWidget {
    fn widget_id(&self) -> WidgetId {
        1 // Fixed ID for cursor
    }

    fn update(&mut self, dt: f32) -> bool {
        // Update blink animation
        self.blink_phase += dt * 2.0;
        if self.blink_phase > std::f32::consts::TAU {
            self.blink_phase -= std::f32::consts::TAU;
        }
        true // Always redraw for animation
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored // Cursor doesn't handle events
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(self.style.width, 19.6),
        }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(0.0, 0.0, self.style.width, 19.6)
    }

    fn priority(&self) -> i32 {
        100 // Cursor on top
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        use crate::coordinates::LayoutRect as Rect;

        // Apply blinking animation
        let alpha = ((self.blink_phase * 2.0).sin() * 0.5 + 0.5).max(0.3);
        let color = (self.style.color & 0x00FFFFFF) | (((alpha * 255.0) as u32) << 24);

        // Use line height from viewport metrics
        let line_height = ctx.viewport.metrics.line_height;

        // Render cursor rectangle directly to GPU
        let rect_instance = crate::render::RectInstance {
            rect: Rect {
                x: ctx.layout_pos.x,
                y: ctx.layout_pos.y,
                width: LogicalPixels(self.style.width),
                height: LogicalPixels(line_height),
            },
            color,
        };
        ctx.gpu_renderer
            .draw_rects(render_pass, &[rect_instance], ctx.viewport.scale_factor);
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Widget for SelectionWidget {
    fn widget_id(&self) -> WidgetId {
        2 // Fixed ID for selection
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(0.0, 0.0),
        }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(0.0, 0.0, 0.0, 0.0)
    }

    fn priority(&self) -> i32 {
        -1 // Selection behind text
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        use crate::coordinates::LayoutRect as Rect;

        // TODO: Calculate actual bounds based on text range
        // For now, draw a simple rectangle
        let width = LogicalPixels(100.0); // Would be calculated from text metrics
        let height = LogicalPixels(ctx.viewport.metrics.line_height);

        // Render selection rectangle directly to GPU
        let rect_instance = crate::render::RectInstance {
            rect: Rect {
                x: ctx.layout_pos.x,
                y: ctx.layout_pos.y,
                width,
                height,
            },
            color: self.color,
        };
        ctx.gpu_renderer
            .draw_rects(render_pass, &[rect_instance], ctx.viewport.scale_factor);
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Widget for LineNumberWidget {
    fn widget_id(&self) -> WidgetId {
        1000 + self.line as u64 // Unique ID per line number
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        let text = format!("{}", self.line);
        let width = text.len() as f32 * 8.4;
        let height = 19.6;
        LayoutResult {
            size: LogicalSize::new(width, height),
        }
    }

    fn bounds(&self) -> LayoutRect {
        let text = format!("{}", self.line);
        let width = text.len() as f32 * 8.4;
        let height = 19.6;
        LayoutRect::new(0.0, 0.0, width, height)
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        // Create text widget for the line number and paint it
        let text = format!("{}", self.line);
        // For line numbers, we can use approximate size since they're simple
        let width = text.len() as f32 * 8.4;
        let height = 19.6;
        let mut widget = TextWidget {
            text: Arc::from(text.as_bytes()),
            style: self.style,
            size: LogicalSize::new(width, height),
            content_type: ContentType::default(),
            original_byte_offset: 0, // Line numbers don't need syntax highlighting
        };
        widget.paint(ctx, render_pass);
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Widget for DiagnosticWidget {
    fn widget_id(&self) -> WidgetId {
        3 // Fixed ID for diagnostics
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(0.0, 2.0),
        }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(0.0, 0.0, 0.0, 2.0)
    }

    fn priority(&self) -> i32 {
        10 // Above text
    }

    fn paint(&self, ctx: &PaintContext<'_>, _render_pass: &mut wgpu::RenderPass) {
        let color = match self.severity {
            Severity::Error => 0xFFFF0000u32,   // Red
            Severity::Warning => 0xFFFF8800u32, // Orange
            Severity::Info => 0xFF0088FFu32,    // Blue
            Severity::Hint => 0xFF888888u32,    // Gray
        };

        // Draw wavy underline
        let width = 100.0; // TODO: Calculate from text range
        let segments = (width / 4.0) as i32;
        let base_y = ctx.layout_pos.y + ctx.viewport.metrics.line_height - 2.0; // Position at bottom of line

        // For now, skip wavy underline rendering - would need line drawing capability in GPU renderer
        // TODO: Implement line rendering for diagnostics
        let _ = (segments, base_y, color); // Silence unused warnings
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl Widget for StyleWidget {
    fn widget_id(&self) -> WidgetId {
        4 // Fixed ID for style
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(0.0, 0.0),
        }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(0.0, 0.0, 0.0, 0.0)
    }

    fn paint(&self, _ctx: &PaintContext<'_>, _render_pass: &mut wgpu::RenderPass) {
        // StyleWidget doesn't render anything - it's just metadata
        // The TextWidget will look for these when rendering text
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// === Public API ===

/// Create text widget from string
/// Note: This creates a widget with default size - use render_text() for accurate sizing
pub fn text(s: &str) -> Arc<dyn Widget> {
    Arc::new(TextWidget {
        text: Arc::from(s.as_bytes()),
        style: 0,                         // Default style
        size: LogicalSize::new(0.0, 0.0), // Will be calculated properly in render_text
        content_type: ContentType::default(),
        original_byte_offset: 0, // Default offset
    })
}

/// Create cursor widget
pub fn cursor() -> Arc<dyn Widget> {
    Arc::new(CursorWidget {
        style: CursorStyle {
            color: 0xFFFFFFFF,
            width: 2.0,
        },
        blink_phase: 0.0,
    })
}

/// Create selection widget
pub fn selection(range: std::ops::Range<usize>) -> Arc<dyn Widget> {
    Arc::new(SelectionWidget {
        range,
        color: 0x4080FF80, // Semi-transparent blue
    })
}

/// Create line number widget
pub fn line_number(line: u32) -> Arc<dyn Widget> {
    Arc::new(LineNumberWidget { line, style: 0 })
}

/// Create diagnostic widget
pub fn diagnostic(
    severity: Severity,
    message: &str,
    range: std::ops::Range<usize>,
) -> Arc<dyn Widget> {
    Arc::new(DiagnosticWidget {
        severity,
        message: Arc::from(message),
        range,
    })
}
