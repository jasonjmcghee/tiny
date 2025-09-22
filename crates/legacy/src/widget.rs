//! Widget system where everything visual is a widget, including text
//!
//! Text rendering uses the consolidated FontSystem from font.rs

use crate::coordinates::Viewport;
use crate::input_types::{KeyEvent, Modifiers, MouseButton};
use ahash::HashMap;
use std::sync::Arc;
use tiny_core::{
    gpu::{create_rect_vertices, RectVertex},
    GpuRenderer,
};
use tiny_sdk::types::{
    LayoutPos, LayoutRect, LogicalPixels, LogicalSize, RectInstance,
};

/// Widget identifier for texture access
pub type WidgetId = u64;

/// Widget event types
#[derive(Debug, Clone)]
pub enum WidgetEvent {
    MouseMove(LayoutPos),
    MouseEnter,
    MouseLeave,
    MouseClick(LayoutPos, MouseButton),
    KeyboardInput(KeyEvent, Modifiers),
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

// Re-export SDK PaintContext for use in widgets
pub use tiny_sdk::PaintContext;

// Helper extension trait for easier GPU renderer access
pub trait PaintContextExt {
    fn gpu(&self) -> &GpuRenderer;
    fn gpu_mut(&self) -> &mut GpuRenderer;
    fn uniform_bind_group(&self) -> &wgpu::BindGroup;
}

impl PaintContextExt for PaintContext {
    fn gpu(&self) -> &GpuRenderer {
        unsafe { &*(self.gpu_renderer as *mut GpuRenderer) }
    }

    fn gpu_mut(&self) -> &mut GpuRenderer {
        unsafe { &mut *(self.gpu_renderer as *mut GpuRenderer) }
    }

    fn uniform_bind_group(&self) -> &wgpu::BindGroup {
        // Get from GPU renderer
        self.gpu().uniform_bind_group()
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

// === Supporting Types ===

pub type StyleId = u32;

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
    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass);

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

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        use tiny_font::create_glyph_instances;

        let text = std::str::from_utf8(&self.text).unwrap_or("");
        if text.is_empty() {
            return;
        }

        use PaintContextExt;
        use tiny_sdk::TextStyleService;

        // Get services from registry
        let services = unsafe { ctx.services() };

        // Get font service (concrete type SharedFontSystem)
        let font_service = services.get::<tiny_font::SharedFontSystem>()
            .expect("Font service not found in registry");

        // Get text style service
        let text_style_service = services.get::<crate::text_style_box_adapter::BoxedTextStyleAdapter>();

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
        let layout_pos = LayoutPos::new(
            ctx.viewport.margin.x.0 + x_base_offset,
            ctx.viewport.margin.y.0,
        );

        // Get effects for this text if available
        let effects = if let Some(ref text_styles) = text_style_service {
            let text_range = self.original_byte_offset..(self.original_byte_offset + text.len());
            text_styles.get_effects_in_range(text_range)
        } else {
            Vec::new()
        };

        // Create glyph instances using the helper
        let mut all_glyph_instances = create_glyph_instances(
            &font_service,
            text,
            layout_pos,
            14.0,  // TODO: Get font size from viewport
            ctx.viewport.scale_factor,
            ctx.viewport.line_height,
            if effects.is_empty() { None } else { Some(&effects) },
            self.original_byte_offset,
        );

        // Transform all glyphs from layout to physical coordinates for GPU
        for glyph in &mut all_glyph_instances {
            let physical_pos = ctx.viewport.layout_to_physical(glyph.pos);
            glyph.pos = LayoutPos::new(physical_pos.x.0, physical_pos.y.0);
        }

        // Check for shader effects in text styles and render with appropriate pipeline
        if !all_glyph_instances.is_empty() {
            let mut shader_id = None;

            // Scan text styles for shader effects from service
            if let Some(ref text_styles) = text_style_service {
                // Use the same range as before - document-relative positions
                let text_range = self.original_byte_offset
                    ..(self.original_byte_offset
                        + std::str::from_utf8(&self.text).unwrap_or("").len());
                let effects = text_styles.get_effects_in_range(text_range);

                // Apply shader effects
                for effect in &effects {
                    if let tiny_sdk::services::TextEffectType::Shader { id, params } = &effect.effect {
                        shader_id = Some(*id);
                        println!("TextWidget: Found shader effect with ID: {}", id);
                        // Pass params to shader via uniform buffer
                        if let Some(params) = params {
                            println!("TextWidget: Writing shader params: {:?}", params);
                            if let Some(effect_buffer) = ctx.gpu().effect_uniform_buffer(*id) {
                                ctx.queue.write_buffer(
                                    effect_buffer,
                                    0,
                                    bytemuck::cast_slice(params.as_slice()),
                                );
                            } else {
                                println!(
                                    "TextWidget: No effect buffer found for shader ID {}",
                                    id
                                );
                            }
                        }
                        break; // Use first shader effect found
                    }
                }
            }

            // Render with or without shader effects
            if let Some(id) = shader_id {
                println!("TextWidget: Rendering with shader effect ID: {}", id);
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


impl Widget for SelectionWidget {
    fn widget_id(&self) -> WidgetId {
        2 // Fixed ID for selection
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

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Transform all selection rectangles from layout space to view space
        let rect_instances: Vec<RectInstance> = self
            .rectangles
            .iter()
            .map(|rect| {
                // Transform each rectangle from layout to view space (apply scroll offset)
                let view_rect = ctx.viewport.layout_rect_to_view(*rect);

                // Convert ViewRect back to LayoutRect format for RectInstance
                RectInstance {
                    rect: LayoutRect::new(
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
                let rect_verts = create_rect_vertices(
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
                size: vertices.len() as u64 * std::mem::size_of::<RectVertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            ctx.queue
                .write_buffer(&vertex_buffer, 0, bytemuck::cast_slice(&vertices));
            render_pass.set_pipeline(ctx.gpu().rect_pipeline());
            render_pass.set_bind_group(0, ctx.uniform_bind_group(), &[]);
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

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
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
            widgets: HashMap::default(),
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
    pub fn paint_all(&mut self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        let widgets = self.widgets_in_order();
        if !widgets.is_empty() {
            println!("PAINTING {} widgets", widgets.len());
            for widget in &widgets {
                println!(
                    "  Widget ID: {}, priority: {}, type: {}",
                    widget.widget_id(),
                    widget.priority(),
                    if widget.widget_id() == 1 {
                        "CURSOR"
                    } else if widget.widget_id() == 2 {
                        "SELECTION"
                    } else if widget.widget_id() == 5000 {
                        "LINE_NUMBERS"
                    } else if widget.widget_id() == 1000 {
                        "DOCUMENT"
                    } else {
                        "OTHER"
                    }
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

}
