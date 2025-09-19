//! Widget system where everything visual is a widget, including text
//!
//! Text rendering uses the consolidated FontSystem from font.rs

use crate::coordinates::{LayoutPos, LayoutRect, LogicalPixels, LogicalSize, Viewport};
use std::collections::HashMap;
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
    pub device: std::sync::Arc<wgpu::Device>,
    /// GPU queue for uploading data
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// Uniform bind group for viewport uniforms
    pub uniform_bind_group: &'a wgpu::BindGroup,
    /// GPU renderer for access to pipelines and resources (raw pointer for flexibility)
    gpu_renderer: *mut crate::gpu::GpuRenderer,
    /// Font system for text layout and rasterization
    pub font_system: &'a std::sync::Arc<crate::font::SharedFontSystem>,
    /// Text style provider for syntax highlighting (optional)
    pub text_styles: Option<&'a dyn crate::text_effects::TextStyleProvider>,
}

impl<'a> PaintContext<'a> {
    /// Create a new PaintContext with raw GPU renderer pointer
    pub fn new(
        viewport: &'a Viewport,
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        uniform_bind_group: &'a wgpu::BindGroup,
        gpu_renderer: *mut crate::gpu::GpuRenderer,
        font_system: &'a std::sync::Arc<crate::font::SharedFontSystem>,
        text_styles: Option<&'a dyn crate::text_effects::TextStyleProvider>,
    ) -> Self {
        Self {
            viewport,
            device,
            queue,
            uniform_bind_group,
            gpu_renderer,
            font_system,
            text_styles,
        }
    }

    /// Get immutable reference to GPU renderer (safe wrapper around raw pointer)
    pub fn gpu(&self) -> &crate::gpu::GpuRenderer {
        unsafe { &*self.gpu_renderer }
    }

    /// Get mutable reference to GPU renderer (safe wrapper around raw pointer)
    pub fn gpu_mut(&self) -> &mut crate::gpu::GpuRenderer {
        unsafe { &mut *self.gpu_renderer }
    }
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
    /// Absolute position in layout space
    pub position: LayoutPos,
    /// Style for cursor (color, width)
    pub style: CursorStyle,
    /// Animation state
    pub blink_phase: f32,
}

/// Selection widget - highlight for selected text
#[derive(Clone)]
pub struct SelectionWidget {
    /// Rectangles that make up this selection (1-3 for multi-line)
    pub rectangles: Vec<LayoutRect>,
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
    fn update(&mut self, _dt: f32) -> bool {
        false
    }

    /// Handle input events
    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored // Text doesn't handle events directly
    }

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

        // Text should include margin positioning to match cursor/selection
        // Add margin to align with cursor/selection coordinate system
        let layout_pos = crate::coordinates::LayoutPos::new(
            ctx.viewport.margin.x.0 + x_base_offset,
            ctx.viewport.margin.y.0
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
                // Use the same range as before - document-relative positions
                let text_range = self.original_byte_offset
                    ..(self.original_byte_offset
                        + std::str::from_utf8(&self.text).unwrap_or("").len());
                let effects = text_styles.get_effects_in_range(text_range);

                // Apply color effects to glyphs by updating their colors
                for effect in &effects {
                    match effect.effect {
                        crate::text_effects::EffectType::Color(color) => {
                            // Find glyphs in this effect's range and update their colors
                            let text_str = std::str::from_utf8(&self.text).unwrap_or("");
                            let mut byte_pos = self.original_byte_offset;
                            let mut glyph_idx = 0;

                            for ch in text_str.chars() {
                                if glyph_idx < all_glyph_instances.len() {
                                    if byte_pos >= effect.range.start && byte_pos < effect.range.end
                                    {
                                        all_glyph_instances[glyph_idx].color = color;
                                    }
                                    glyph_idx += 1;
                                    byte_pos += ch.len_utf8();
                                } else {
                                    break;
                                }
                            }
                        }
                        crate::text_effects::EffectType::Shader { id, ref params } => {
                            shader_id = Some(id);
                            // Update effect uniform buffer with shader parameters
                            if let Some(effect_buffer) = ctx.gpu().effect_uniform_buffer(id)
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
                ctx.gpu()
                    .draw_glyphs(render_pass, &all_glyph_instances, Some(id));
            } else {
                ctx.gpu()
                    .draw_glyphs(render_pass, &all_glyph_instances, None);
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
        // Update blink animation - faster and sharper
        self.blink_phase += dt * 4.0; // 2x faster
        if self.blink_phase > std::f32::consts::TAU {
            self.blink_phase -= std::f32::consts::TAU;
        }
        true // Always redraw for animation
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(self.style.width, 19.6),
        }
    }

    fn bounds(&self) -> LayoutRect {
        LayoutRect::new(self.position.x.0, self.position.y.0, self.style.width, 19.6)
    }

    fn priority(&self) -> i32 {
        100 // Cursor on top
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        use std::sync::atomic::{AtomicU64, Ordering};

        // Track cursor position and last activity time (in milliseconds since program start)
        static LAST_POS: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);
        static CURSOR_LAST_ACTIVE: AtomicU64 = AtomicU64::new(0);
        static PROGRAM_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

        let start = PROGRAM_START.get_or_init(std::time::Instant::now);
        let now_ms = start.elapsed().as_millis() as u64;

        // Check if cursor moved
        let current_pos = (self.position.x.0, self.position.y.0);
        if let Ok(mut last) = LAST_POS.lock() {
            if last.map_or(true, |p| p != current_pos) {
                *last = Some(current_pos);
                // Update last activity time
                CURSOR_LAST_ACTIVE.store(now_ms, Ordering::Relaxed);
            }
        }

        // Simple blink logic
        let last_active = CURSOR_LAST_ACTIVE.load(Ordering::Relaxed);
        let ms_since_activity = now_ms.saturating_sub(last_active);

        let color = if ms_since_activity < 500 {
            // Solid cursor for 500ms after activity
            self.style.color
        } else {
            // Blink: 500ms on, 500ms off
            let blink_phase = (now_ms / 500) % 2;
            if blink_phase == 0 {
                self.style.color
            } else {
                0x00000000
            }
        };

        // Use line height from viewport metrics
        let line_height = ctx.viewport.metrics.line_height;

        // Transform cursor position from layout space to view space (apply scroll offset)
        let view_pos = ctx.viewport.layout_to_view(self.position);

        // Apply 2px left shift for better alignment
        let adjusted_view_pos = crate::coordinates::ViewPos::new(
            view_pos.x.0 - 2.0,
            view_pos.y.0
        );

        // Create view-space rectangle for GPU rendering
        let view_rect = crate::coordinates::ViewRect::new(
            adjusted_view_pos.x.0,
            adjusted_view_pos.y.0,
            self.style.width,
            line_height,
        );

        // Convert to render rect format
        let rect_instance = crate::render::RectInstance {
            rect: crate::coordinates::LayoutRect::new(
                view_rect.x.0,
                view_rect.y.0,
                view_rect.width.0,
                view_rect.height.0,
            ),
            color,
        };

        // Create our own vertex buffer to avoid conflicts with shared buffer
        let scale = ctx.viewport.scale_factor;
        let vertices = crate::gpu::create_rect_vertices(
            rect_instance.rect.x.0 * scale,
            rect_instance.rect.y.0 * scale,
            rect_instance.rect.width.0 * scale,
            rect_instance.rect.height.0 * scale,
            rect_instance.color,
        );

        // Create temporary buffer for this cursor
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Cursor Vertex Buffer"),
            size: vertices.len() as u64 * std::mem::size_of::<crate::gpu::RectVertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        ctx.queue.write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        render_pass.set_pipeline(ctx.gpu().rect_pipeline());
        render_pass.set_bind_group(0, ctx.uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
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
        // Calculate bounding size from rectangles
        if self.rectangles.is_empty() {
            return LayoutResult {
                size: LogicalSize::new(0.0, 0.0),
            };
        }

        let bounds = self.bounds();
        LayoutResult {
            size: LogicalSize::new(bounds.width.0, bounds.height.0),
        }
    }

    fn bounds(&self) -> LayoutRect {
        // Return bounding box that encompasses all selection rectangles
        if self.rectangles.is_empty() {
            return LayoutRect::new(0.0, 0.0, 0.0, 0.0);
        }

        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;

        for rect in &self.rectangles {
            min_x = min_x.min(rect.x.0);
            min_y = min_y.min(rect.y.0);
            max_x = max_x.max(rect.x.0 + rect.width.0);
            max_y = max_y.max(rect.y.0 + rect.height.0);
        }

        LayoutRect::new(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    fn priority(&self) -> i32 {
        -10 // Selection well behind text
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        // Transform all selection rectangles from layout space to view space
        let rect_instances: Vec<crate::render::RectInstance> = self
            .rectangles
            .iter()
            .map(|rect| {
                // Transform each rectangle from layout to view space (apply scroll offset)
                let view_rect = ctx.viewport.layout_rect_to_view(*rect);

                // Convert ViewRect back to LayoutRect format for RectInstance
                crate::render::RectInstance {
                    rect: crate::coordinates::LayoutRect::new(
                        view_rect.x.0,
                        view_rect.y.0,
                        view_rect.width.0,
                        view_rect.height.0,
                    ),
                    color: self.color,
                }
            })
            .collect();

        if !rect_instances.is_empty() {
            // Create our own vertex buffer to avoid conflicts
            let scale = ctx.viewport.scale_factor;
            let mut vertices = Vec::with_capacity(rect_instances.len() * 6);

            for rect in &rect_instances {
                let rect_verts = crate::gpu::create_rect_vertices(
                    rect.rect.x.0 * scale,
                    rect.rect.y.0 * scale,
                    rect.rect.width.0 * scale,
                    rect.rect.height.0 * scale,
                    rect.color,
                );
                vertices.extend_from_slice(&rect_verts);
            }

            // Create temporary buffer
            let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Selection Vertex Buffer"),
                size: vertices.len() as u64 * std::mem::size_of::<crate::gpu::RectVertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            ctx.queue.write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertices));
            render_pass.set_pipeline(ctx.gpu().rect_pipeline());
            render_pass.set_bind_group(0, ctx.uniform_bind_group, &[]);
            render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            render_pass.draw(0..vertices.len() as u32, 0..1);
        }
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
        let widget = TextWidget {
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

    fn paint(&self, _ctx: &PaintContext<'_>, _render_pass: &mut wgpu::RenderPass) {
        let color = match self.severity {
            Severity::Error => 0xFF00FFu32,   // Red
            Severity::Warning => 0xFF88FFu32, // Orange
            Severity::Info => 0x0088FFFFu32,  // Blue
            Severity::Hint => 0x888888FFu32,  // Gray
        };

        // TODO: Draw wavy underline - needs absolute positioning
        // For now, skip wavy underline rendering - would need line drawing capability in GPU renderer
        // TODO: Implement line rendering for diagnostics
        let _ = color; // Silence unused warnings
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

/// Create cursor widget at position
pub fn cursor(position: LayoutPos) -> Arc<dyn Widget> {
    Arc::new(CursorWidget {
        position,
        style: CursorStyle {
            color: 0xFFFFFFFF, // White cursor
            width: 2.0,        // Normal width
        },
        blink_phase: 0.0,
    })
}

/// Create selection widget from rectangles
pub fn selection(rectangles: Vec<LayoutRect>) -> Arc<dyn Widget> {
    Arc::new(SelectionWidget {
        rectangles,
        color: 0x4080FF40, // Semi-transparent blue
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

/// Widget manager - tracks overlay widgets like cursor and selections
pub struct WidgetManager {
    /// All managed widgets
    pub widgets: HashMap<WidgetId, Arc<dyn Widget>>,
    /// Next widget ID to assign
    next_id: WidgetId,
    /// Sorted widget IDs by z-order (priority)
    sorted_ids: Vec<WidgetId>,
    /// Whether sorted_ids needs updating
    needs_sort: bool,
}

impl WidgetManager {
    pub fn new() -> Self {
        Self {
            widgets: HashMap::new(),
            next_id: 1000,
            sorted_ids: Vec::new(),
            needs_sort: false,
        }
    }

    /// Add a widget to the manager
    pub fn add_widget(&mut self, widget: Arc<dyn Widget>) -> WidgetId {
        let id = self.next_id;
        self.next_id += 1;
        self.widgets.insert(id, widget);
        self.sorted_ids.push(id);
        self.needs_sort = true;
        id
    }

    /// Remove a widget by ID
    pub fn remove_widget(&mut self, id: WidgetId) -> Option<Arc<dyn Widget>> {
        if let Some(widget) = self.widgets.remove(&id) {
            self.sorted_ids.retain(|&widget_id| widget_id != id);
            Some(widget)
        } else {
            None
        }
    }

    /// Clear all widgets
    pub fn clear(&mut self) {
        self.widgets.clear();
        self.sorted_ids.clear();
        self.needs_sort = false;
        // Don't reset next_id - let it keep incrementing
    }

    /// Update all widgets (for animations)
    pub fn update_all(&mut self, dt: f32) -> bool {
        let mut needs_redraw = false;
        for widget in self.widgets.values_mut() {
            // Need to get mutable reference through Arc
            if let Some(widget_mut) = Arc::get_mut(widget) {
                if widget_mut.update(dt) {
                    needs_redraw = true;
                }
            }
        }
        needs_redraw
    }

    /// Get widgets in render order (sorted by z-index/priority)
    pub fn widgets_in_order(&mut self) -> Vec<Arc<dyn Widget>> {
        if self.needs_sort {
            // Sort by widget priority (z-index)
            self.sorted_ids
                .sort_by_key(|&id| self.widgets.get(&id).map(|w| w.priority()).unwrap_or(0));
            self.needs_sort = false;
        }

        self.sorted_ids
            .iter()
            .filter_map(|id| self.widgets.get(id).cloned())
            .collect()
    }

    /// Paint all widgets in order
    pub fn paint_all(&mut self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        let widgets = self.widgets_in_order();
        if !widgets.is_empty() {
            println!("PAINTING {} widgets", widgets.len());
            for widget in &widgets {
                println!(
                    "  Widget type: {}, priority: {}",
                    if widget.widget_id() == 1 {
                        "CURSOR"
                    } else if widget.widget_id() == 2 {
                        "SELECTION"
                    } else {
                        "OTHER"
                    },
                    widget.priority()
                );
                widget.paint(ctx, render_pass);
            }
        }
    }

    /// Replace selection widgets with new ones
    pub fn set_selection_widgets(&mut self, selections: Vec<Arc<dyn Widget>>) {
        // Remove all existing selection widgets by widget type
        self.sorted_ids.retain(|&id| {
            if let Some(widget) = self.widgets.get(&id) {
                if widget.widget_id() == 2 {
                    // SelectionWidget returns id 2
                    self.widgets.remove(&id);
                    false
                } else {
                    true
                }
            } else {
                false // Remove invalid IDs
            }
        });

        // Add new selection widgets
        for widget in selections {
            self.add_widget(widget);
        }
    }

    /// Set cursor widget (replaces existing)
    pub fn set_cursor_widget(&mut self, cursor: Arc<dyn Widget>) {
        // Remove all existing cursor widgets by widget type
        self.sorted_ids.retain(|&id| {
            if let Some(widget) = self.widgets.get(&id) {
                if widget.widget_id() == 1 {
                    // CursorWidget returns id 1
                    self.widgets.remove(&id);
                    false
                } else {
                    true
                }
            } else {
                false // Remove invalid IDs
            }
        });

        // Add new cursor widget
        self.add_widget(cursor);
    }
}
