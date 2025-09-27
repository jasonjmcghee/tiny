//! Diagnostics Plugin - Renders squiggly lines under text and shows popups on hover

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_font::{create_glyph_instances, measure_text_width, SharedFontSystem};
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::Buffer;
use tiny_sdk::{
    ffi::{
        BindGroupLayoutId, BufferId, PipelineId, ShaderModuleId, VertexAttributeDescriptor,
        VertexFormat,
    },
    Capability, Configurable, GlyphInstance, Initializable, LayoutPos, Library,
    PaintContext, Paintable, Plugin, PluginError, SetupContext, ViewportInfo,
};

/// Diagnostic severity levels
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DiagnosticSeverity {
    Error = 0,
    Warning = 1,
    Info = 2,
}

/// A single diagnostic with location and message
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Line number (0-indexed)
    pub line: usize,
    /// Column range (start, end) - 0-indexed character positions
    pub column_range: (usize, usize),
    /// Byte range in the line (if available for accurate positioning)
    pub byte_range: Option<(usize, usize)>,
    /// Diagnostic message
    pub message: String,
    /// Severity level
    pub severity: DiagnosticSeverity,
}

/// Configuration for diagnostics appearance
#[derive(Debug, Clone)]
pub struct DiagnosticsConfig {
    pub popup_background_color: u32,
    pub popup_text_color: u32,
    pub popup_border_color: u32,
    pub popup_padding: f32,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            popup_background_color: 0x2D2D30FF, // Dark gray
            popup_text_color: 0xCCCCCCFF,       // Light gray
            popup_border_color: 0x464647FF,     // Border gray
            popup_padding: 8.0,
        }
    }
}

/// Vertex data for squiggly lines and popups
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct DiagnosticVertex {
    position: [f32; 2],
    color: u32,
    line_info: [f32; 4], // x, y, width, severity
    is_popup: u32,
}

/// Main diagnostics plugin struct
pub struct DiagnosticsPlugin {
    // Configuration
    config: DiagnosticsConfig,

    // Diagnostics data
    diagnostics: Vec<Diagnostic>,

    // Line text cache for accurate width calculation
    line_texts: HashMap<usize, String>,

    // Hover state
    hovered_diagnostic: Option<usize>,
    mouse_position: (f32, f32),

    // Viewport info
    viewport: ViewportInfo,

    // GPU resources
    vertex_buffer: Option<Buffer>,
    vertex_buffer_id: Option<BufferId>,
    popup_vertex_buffer: Option<Buffer>,
    popup_vertex_buffer_id: Option<BufferId>,
    custom_pipeline_id: Option<PipelineId>,
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,
}

impl DiagnosticsPlugin {
    /// Create a new diagnostics plugin
    pub fn new() -> Self {
        use tiny_sdk::{LogicalSize, PhysicalSize};

        // Hard-coded test diagnostic
        let test_diagnostic = Diagnostic {
            line: 4, // Line 5 (0-indexed)
            column_range: (10, 20),
            byte_range: None,
            message: "Type error: expected string,\ngot number".to_string(),
            severity: DiagnosticSeverity::Error,
        };

        Self {
            config: DiagnosticsConfig::default(),
            diagnostics: vec![test_diagnostic],
            line_texts: HashMap::new(),
            hovered_diagnostic: None,
            mouse_position: (0.0, 0.0),
            viewport: ViewportInfo {
                scroll: LayoutPos::new(0.0, 0.0),
                logical_size: LogicalSize::new(800.0, 600.0),
                physical_size: PhysicalSize {
                    width: 800,
                    height: 600,
                },
                scale_factor: 1.0,
                line_height: 19.6,
                font_size: 14.0,
                margin: LayoutPos::new(60.0, 10.0),
                global_margin: LayoutPos::new(0.0, 0.0),
            },
            vertex_buffer: None,
            vertex_buffer_id: None,
            popup_vertex_buffer: None,
            popup_vertex_buffer_id: None,
            custom_pipeline_id: None,
            device: None,
            queue: None,
        }
    }

    /// Update viewport information
    pub fn set_viewport_info(&mut self, viewport: ViewportInfo) {
        self.viewport = viewport;
    }

    /// Update mouse position for hover detection (in editor-local coordinates)
    pub fn set_mouse_position(&mut self, x: f32, y: f32, widget_viewport: Option<&tiny_sdk::types::WidgetViewport>, services: Option<&tiny_sdk::ServiceRegistry>) {
        self.mouse_position = (x, y);
        self.update_hover_state(widget_viewport, services);
    }

    /// Check which diagnostic (if any) the mouse is hovering over
    fn update_hover_state(&mut self, widget_viewport: Option<&tiny_sdk::types::WidgetViewport>, services: Option<&tiny_sdk::ServiceRegistry>) {
        self.hovered_diagnostic = None;

        let (mouse_x, mouse_y) = self.mouse_position;

        // Get widget bounds and scroll if available
        let widget_offset_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
        let widget_offset_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
        let widget_scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(self.viewport.scroll.y.0);

        // Convert mouse position to editor-local coordinates (remove widget offset)
        let local_mouse_x = mouse_x - widget_offset_x;
        let local_mouse_y = mouse_y - widget_offset_y;

        for (i, diagnostic) in self.diagnostics.iter().enumerate() {
            // Calculate diagnostic position in view coordinates
            let doc_y = diagnostic.line as f32 * self.viewport.line_height;
            let view_y = doc_y - widget_scroll_y;

            // Get font service - required for proper rendering
            let font_service = services
                .and_then(|s| s.get::<SharedFontSystem>())
                .expect("Font service is required for diagnostics plugin");

            // Get actual text width using font metrics
            let (start_x, end_x) = if let Some(line_text) = self.line_texts.get(&diagnostic.line) {
                // Measure actual text width up to the diagnostic positions
                let start_str = &line_text[..line_text.len().min(diagnostic.column_range.0)];
                let end_str = &line_text[..line_text.len().min(diagnostic.column_range.1)];

                let start_width = measure_text_width(&font_service, start_str, self.viewport.font_size);
                let end_width = measure_text_width(&font_service, end_str, self.viewport.font_size);
                (start_width, end_width)
            } else {
                // Use actual monospace character width from font metrics
                let char_width = font_service.char_width_coef() * self.viewport.font_size;
                let start_x = diagnostic.column_range.0 as f32 * char_width;
                let end_x = diagnostic.column_range.1 as f32 * char_width;
                (start_x, end_x)
            };

            // Check if mouse is over the diagnostic area (with some vertical tolerance)
            if local_mouse_x >= start_x
                && local_mouse_x <= end_x
                && local_mouse_y >= view_y
                && local_mouse_y <= view_y + self.viewport.line_height
            {
                self.hovered_diagnostic = Some(i);
                break;
            }
        }
    }

    /// Create vertices for squiggly lines
    fn create_squiggly_vertices(&self, widget_viewport: Option<&tiny_sdk::types::WidgetViewport>, services: Option<&tiny_sdk::ServiceRegistry>) -> Vec<DiagnosticVertex> {
        let mut vertices = Vec::new();
        let scale = self.viewport.scale_factor;

        // Get font service for accurate character width
        let char_width = services
            .and_then(|s| s.get::<SharedFontSystem>())
            .map(|fs| fs.char_width_coef() * self.viewport.font_size)
            .unwrap_or_else(|| {
                eprintln!("Warning: Font service not available, using fallback character width");
                self.viewport.font_size * 0.6
            });

        // Get widget bounds offset and scroll
        let widget_offset_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
        let widget_offset_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
        let widget_scroll_x = widget_viewport.map(|w| w.scroll.x.0).unwrap_or(self.viewport.scroll.x.0);
        let widget_scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(self.viewport.scroll.y.0);

        for diagnostic in &self.diagnostics {
            // Calculate line position in document space (absolute position in the document)
            let doc_y = diagnostic.line as f32 * self.viewport.line_height;
            // Position at bottom of line for squiggly effect
            let line_y_doc = doc_y + self.viewport.line_height - 2.0;


            // Calculate X positions in document space
            let start_x_doc = diagnostic.column_range.0 as f32 * char_width;
            let end_x_doc = diagnostic.column_range.1 as f32 * char_width;
            let width = end_x_doc - start_x_doc;

            // Convert to view space (subtract scroll to get visible position)
            let view_x = start_x_doc - widget_scroll_x;
            let view_y = line_y_doc - widget_scroll_y;

            // Transform to physical space: add widget offset first (in logical), then scale
            let screen_x = view_x + widget_offset_x;
            let screen_y = view_y + widget_offset_y;
            let start_x = screen_x * scale;
            let line_y = screen_y * scale;
            let width_scaled = width * scale;


            // Create a quad that covers the area where the squiggly line will be drawn
            let padding = 4.0 * scale; // Extra height for the wave amplitude
            let line_info = [
                start_x,
                line_y,
                width_scaled,
                diagnostic.severity as i32 as f32,
            ];

            // Color isn't used for squiggly lines (determined by severity in shader)
            let color = 0x00000000u32;

            // Create quad vertices
            vertices.extend_from_slice(&[
                DiagnosticVertex {
                    position: [start_x, line_y - padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
                DiagnosticVertex {
                    position: [start_x + width_scaled, line_y - padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
                DiagnosticVertex {
                    position: [start_x, line_y + padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
                DiagnosticVertex {
                    position: [start_x + width_scaled, line_y - padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
                DiagnosticVertex {
                    position: [start_x + width_scaled, line_y + padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
                DiagnosticVertex {
                    position: [start_x, line_y + padding],
                    color,
                    line_info,
                    is_popup: 0,
                },
            ]);
        }

        vertices
    }

    /// Create vertices for popup background
    fn create_popup_vertices(&self, diagnostic: &Diagnostic, widget_viewport: Option<&tiny_sdk::types::WidgetViewport>, services: Option<&tiny_sdk::ServiceRegistry>) -> Vec<DiagnosticVertex> {
        let mut vertices = Vec::new();
        let scale = self.viewport.scale_factor;
        // Get font service for accurate character width
        let char_width = services
            .and_then(|s| s.get::<SharedFontSystem>())
            .map(|fs| fs.char_width_coef() * self.viewport.font_size)
            .unwrap_or_else(|| {
                eprintln!("Warning: Font service not available, using fallback character width");
                self.viewport.font_size * 0.6
            });

        // Get widget bounds offset and scroll
        let widget_offset_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
        let widget_offset_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
        let widget_scroll_x = widget_viewport.map(|w| w.scroll.x.0).unwrap_or(self.viewport.scroll.x.0);
        let widget_scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(self.viewport.scroll.y.0);

        // Calculate popup size using font system's layout for accurate multi-line text dimensions
        let font_service = services
            .and_then(|s| s.get::<SharedFontSystem>())
            .expect("Font service is required for popup rendering");

        let layout = font_service.layout_text(&diagnostic.message, self.viewport.font_size);
        let (text_width, text_height) = (layout.width, layout.height);

        let message_width_logical = text_width + self.config.popup_padding * 2.0;
        let popup_height_logical = text_height + self.config.popup_padding * 2.0;

        // Position popup above the diagnostic in document space
        let doc_y = diagnostic.line as f32 * self.viewport.line_height;
        let doc_x = diagnostic.column_range.0 as f32 * char_width;

        // Convert to view space (subtract scroll)
        let view_x = doc_x - widget_scroll_x;
        let view_y = doc_y - widget_scroll_y;

        // Position popup above the line in view space
        let popup_x_view = view_x;
        let popup_y_view = view_y - popup_height_logical - 10.0; // 10px above the line

        // Transform to screen space: add widget offset, then scale to physical
        let popup_x = (popup_x_view + widget_offset_x) * scale;
        let popup_y = (popup_y_view + widget_offset_y) * scale;
        let message_width = message_width_logical * scale;
        let popup_height = popup_height_logical * scale;

        let color = self.config.popup_background_color;
        let line_info = [0.0, 0.0, 0.0, 0.0]; // Not used for popups

        // Create quad for popup background
        vertices.extend_from_slice(&[
            DiagnosticVertex {
                position: [popup_x, popup_y],
                color,
                line_info,
                is_popup: 1,
            },
            DiagnosticVertex {
                position: [popup_x + message_width, popup_y],
                color,
                line_info,
                is_popup: 1,
            },
            DiagnosticVertex {
                position: [popup_x, popup_y + popup_height],
                color,
                line_info,
                is_popup: 1,
            },
            DiagnosticVertex {
                position: [popup_x + message_width, popup_y],
                color,
                line_info,
                is_popup: 1,
            },
            DiagnosticVertex {
                position: [popup_x + message_width, popup_y + popup_height],
                color,
                line_info,
                is_popup: 1,
            },
            DiagnosticVertex {
                position: [popup_x, popup_y + popup_height],
                color,
                line_info,
                is_popup: 1,
            },
        ]);

        vertices
    }

    /// Collect glyphs for popup text
    pub fn collect_popup_glyphs(&self, services: &tiny_sdk::ServiceRegistry, widget_viewport: Option<&tiny_sdk::types::WidgetViewport>) -> Vec<GlyphInstance> {
        if let Some(diagnostic_idx) = self.hovered_diagnostic {
            if let Some(diagnostic) = self.diagnostics.get(diagnostic_idx) {
                // Get font service - required for text rendering
                let font_service = services.get::<SharedFontSystem>()
                    .expect("Font service is required for popup text rendering");

                let scale = self.viewport.scale_factor;
                // Use actual font metrics for popup text positioning
                let char_width = font_service.char_width_coef() * self.viewport.font_size;

                // Get widget bounds offset and scroll
                let widget_offset_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
                let widget_offset_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
                let widget_scroll_x = widget_viewport.map(|w| w.scroll.x.0).unwrap_or(self.viewport.scroll.x.0);
                let widget_scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(self.viewport.scroll.y.0);

                // Calculate popup position consistently with popup background
                // Position in document space
                let doc_y = diagnostic.line as f32 * self.viewport.line_height;
                let doc_x = diagnostic.column_range.0 as f32 * char_width;

                // Convert to view space (subtract scroll)
                let view_x = doc_x - widget_scroll_x;
                let view_y = doc_y - widget_scroll_y;

                // Get the actual text layout to know the exact height
                let layout = font_service.layout_text(&diagnostic.message, self.viewport.font_size);

                // Calculate popup background position (same as in create_popup_vertices)
                // Use the actual text height from the layout
                let popup_height_logical = layout.height + self.config.popup_padding * 2.0;
                let popup_y_view = view_y - popup_height_logical - 10.0; // 10px above the line

                // Position text inside popup with padding from the top of the popup
                let text_x = view_x + self.config.popup_padding;
                let text_y = popup_y_view + self.config.popup_padding;

                // The position for create_glyph_instances should be in logical pixels
                // relative to where the text will be rendered (already includes widget offset)
                let pos = LayoutPos::new(text_x + widget_offset_x, text_y + widget_offset_y);

                let glyphs = create_glyph_instances(
                    &font_service,
                    &diagnostic.message,
                    pos,
                    self.viewport.font_size,
                    scale,
                    self.viewport.line_height,
                    None,
                    0,
                );

                // The glyphs are already in logical coordinates with the correct position
                // We need to scale them to physical coordinates
                glyphs
                    .into_iter()
                    .map(|mut g| {
                        // Scale position from logical to physical
                        g.pos = LayoutPos::new(g.pos.x.0 * scale, g.pos.y.0 * scale);
                        g.token_id = 254; // Special token for popup text
                        g
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        }
    }
}

// === Plugin Trait Implementation ===

impl Plugin for DiagnosticsPlugin {
    fn name(&self) -> &str {
        "diagnostics"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("diagnostics".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }

    fn as_library(&self) -> Option<&dyn Library> {
        Some(self)
    }

    fn as_library_mut(&mut self) -> Option<&mut dyn Library> {
        Some(self)
    }

    fn as_configurable(&mut self) -> Option<&mut dyn Configurable> {
        Some(self)
    }
}

// === Initializable Trait Implementation ===

impl Initializable for DiagnosticsPlugin {
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        self.device = Some(ctx.device.clone());
        self.queue = Some(ctx.queue.clone());

        // Create vertex buffers
        let vertex_size = std::mem::size_of::<DiagnosticVertex>();
        let buffer_size = (vertex_size * 100) as u64; // Space for multiple diagnostics

        // Buffer for squiggly lines
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Diagnostics Vertex Buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.vertex_buffer = Some(vertex_buffer);

        let buffer_id = BufferId::create(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        self.vertex_buffer_id = Some(buffer_id);

        // Buffer for popup backgrounds
        let popup_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Diagnostics Popup Buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.popup_vertex_buffer = Some(popup_buffer);

        let popup_buffer_id = BufferId::create(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        self.popup_vertex_buffer_id = Some(popup_buffer_id);

        // Create shader and pipeline
        let shader_source = include_str!("shader.wgsl");
        let shader_id = ShaderModuleId::create_from_wgsl(shader_source);
        let bind_group_layout = BindGroupLayoutId::create_uniform();

        // Define vertex attributes
        let attributes = vec![
            VertexAttributeDescriptor {
                offset: 0,
                location: 0,
                format: VertexFormat::Float32x2, // position
            },
            VertexAttributeDescriptor {
                offset: 8,
                location: 1,
                format: VertexFormat::Uint32, // color
            },
            VertexAttributeDescriptor {
                offset: 12,
                location: 2,
                format: VertexFormat::Float32x4, // line_info
            },
            VertexAttributeDescriptor {
                offset: 28,
                location: 3,
                format: VertexFormat::Uint32, // is_popup
            },
        ];

        let pipeline_id = PipelineId::create_with_layout(
            shader_id,
            shader_id,
            bind_group_layout,
            32, // vertex stride: position (8) + color (4) + line_info (16) + is_popup (4) = 32
            &attributes,
        );
        self.custom_pipeline_id = Some(pipeline_id);

        eprintln!("Diagnostics plugin initialized with GPU resources");

        Ok(())
    }
}

// === Library Trait Implementation ===

impl Library for DiagnosticsPlugin {
    fn name(&self) -> &str {
        "diagnostics_api"
    }

    fn call(&mut self, method: &str, args: &[u8]) -> Result<Vec<u8>, PluginError> {
        match method {
            "set_viewport_info" => {
                let viewport_info_size = std::mem::size_of::<ViewportInfo>();
                if args.len() < viewport_info_size {
                    return Err(PluginError::Other("Invalid viewport args".into()));
                }

                let viewport_info: &ViewportInfo =
                    bytemuck::from_bytes(&args[0..viewport_info_size]);
                self.set_viewport_info(*viewport_info);
                Ok(Vec::new())
            }
            "set_mouse_position" => {
                if args.len() < 8 {
                    return Err(PluginError::Other("Invalid mouse position args".into()));
                }

                let x = f32::from_le_bytes(args[0..4].try_into().unwrap());
                let y = f32::from_le_bytes(args[4..8].try_into().unwrap());
                // For Library API calls, we don't have widget viewport or services info
                // The caller should use the paint method with proper viewport
                self.set_mouse_position(x, y, None, None);
                Ok(Vec::new())
            }
            "add_diagnostic" => {
                // Format: line (u32), col_start (u32), col_end (u32), severity (u8), message_len (u32), message (bytes)
                if args.len() < 17 {
                    return Err(PluginError::Other("Invalid diagnostic args".into()));
                }

                let line = u32::from_le_bytes(args[0..4].try_into().unwrap()) as usize;
                let col_start = u32::from_le_bytes(args[4..8].try_into().unwrap()) as usize;
                let col_end = u32::from_le_bytes(args[8..12].try_into().unwrap()) as usize;
                let severity = match args[12] {
                    0 => DiagnosticSeverity::Error,
                    1 => DiagnosticSeverity::Warning,
                    _ => DiagnosticSeverity::Info,
                };
                let message_len = u32::from_le_bytes(args[13..17].try_into().unwrap()) as usize;

                if args.len() < 17 + message_len {
                    return Err(PluginError::Other("Invalid message length".into()));
                }

                let message = String::from_utf8_lossy(&args[17..17 + message_len]).to_string();

                self.diagnostics.push(Diagnostic {
                    line,
                    column_range: (col_start, col_end),
                    byte_range: None,
                    message,
                    severity,
                });
                Ok(Vec::new())
            }
            "clear_diagnostics" => {
                self.diagnostics.clear();
                self.line_texts.clear();
                self.hovered_diagnostic = None;
                Ok(Vec::new())
            }
            "set_line_text" => {
                // Format: line_num (u32), text_len (u32), text (bytes)
                if args.len() < 8 {
                    return Err(PluginError::Other("Invalid line text args".into()));
                }

                let line_num = u32::from_le_bytes(args[0..4].try_into().unwrap()) as usize;
                let text_len = u32::from_le_bytes(args[4..8].try_into().unwrap()) as usize;

                if args.len() < 8 + text_len {
                    return Err(PluginError::Other("Invalid text length".into()));
                }

                let text = String::from_utf8_lossy(&args[8..8 + text_len]).to_string();
                self.line_texts.insert(line_num, text);
                Ok(Vec::new())
            }
            _ => Err(PluginError::Other("Unknown method".into())),
        }
    }
}

// === Paintable Trait Implementation ===

impl Paintable for DiagnosticsPlugin {
    fn z_index(&self) -> i32 {
        50 // Above text but below cursor
    }

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Get services from context
        let services = unsafe {
            ctx.context_data.as_ref().map(|data| {
                &*(data as *const _ as *const tiny_sdk::ServiceRegistry)
            })
        };

        // Draw squiggly lines
        let vertices = self.create_squiggly_vertices(ctx.widget_viewport.as_ref(), services);
        if !vertices.is_empty() {
            let vertex_data = bytemuck::cast_slice(&vertices);
            let vertex_count = vertices.len() as u32;

            if let Some(buffer_id) = self.vertex_buffer_id {
                buffer_id.write(0, vertex_data);

                if let Some(ref gpu_ctx) = ctx.gpu_context {
                    if let Some(pipeline_id) = self.custom_pipeline_id {
                        gpu_ctx.set_pipeline(render_pass, pipeline_id);
                        gpu_ctx.set_bind_group(render_pass, 0, gpu_ctx.uniform_bind_group_id);
                        gpu_ctx.set_vertex_buffer(render_pass, 0, buffer_id);
                        gpu_ctx.draw(render_pass, vertex_count, 1);
                    }
                }
            }
        }

        // Draw popup if hovering
        if let Some(diagnostic_idx) = self.hovered_diagnostic {
            if let Some(diagnostic) = self.diagnostics.get(diagnostic_idx) {
                // Draw popup background
                let popup_vertices = self.create_popup_vertices(diagnostic, ctx.widget_viewport.as_ref(), services);
                if !popup_vertices.is_empty() {
                    let vertex_data = bytemuck::cast_slice(&popup_vertices);
                    let vertex_count = popup_vertices.len() as u32;

                    if let Some(buffer_id) = self.popup_vertex_buffer_id {
                        buffer_id.write(0, vertex_data);

                        if let Some(ref gpu_ctx) = ctx.gpu_context {
                            if let Some(pipeline_id) = self.custom_pipeline_id {
                                gpu_ctx.set_pipeline(render_pass, pipeline_id);
                                gpu_ctx.set_bind_group(render_pass, 0, gpu_ctx.uniform_bind_group_id);
                                gpu_ctx.set_vertex_buffer(render_pass, 0, buffer_id);
                                gpu_ctx.draw(render_pass, vertex_count, 1);
                            }
                        }
                    }
                }

                // Draw popup text using glyph rendering with a separate buffer offset
                // to avoid interfering with main text rendering
                unsafe {
                    if let Some(services) = ctx.context_data.as_ref() {
                        let services = &*(services as *const _ as *const tiny_sdk::ServiceRegistry);
                        let glyphs = self.collect_popup_glyphs(services, ctx.widget_viewport.as_ref());

                        if !glyphs.is_empty() && ctx.gpu_renderer != std::ptr::null_mut() {
                            let gpu_renderer = &*(ctx.gpu_renderer as *const tiny_core::GpuRenderer);
                            // Use a large offset to avoid conflicts with main text buffer
                            // Main text typically uses offsets 0-100KB, so we use 500KB offset
                            let buffer_offset = 500_000;
                            gpu_renderer.draw_glyphs_styled_with_offset(
                                render_pass,
                                &glyphs,
                                false,
                                buffer_offset
                            );
                        }
                    }
                }
            }
        }
    }
}

// === Configurable Trait Implementation ===

impl Configurable for DiagnosticsPlugin {
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError> {
        #[derive(Deserialize)]
        struct PluginToml {
            config: PluginConfig,
        }

        #[derive(Deserialize)]
        struct PluginConfig {
            #[serde(default = "default_popup_bg")]
            popup_background_color: u32,
            #[serde(default = "default_popup_text")]
            popup_text_color: u32,
            #[serde(default = "default_popup_border")]
            popup_border_color: u32,
            #[serde(default = "default_popup_padding")]
            popup_padding: f32,
        }

        fn default_popup_bg() -> u32 { 0x2D2D30FF }
        fn default_popup_text() -> u32 { 0xCCCCCCFF }
        fn default_popup_border() -> u32 { 0x464647FF }
        fn default_popup_padding() -> f32 { 8.0 }

        match toml::from_str::<PluginToml>(config_data) {
            Ok(plugin_toml) => {
                self.config.popup_background_color = plugin_toml.config.popup_background_color;
                self.config.popup_text_color = plugin_toml.config.popup_text_color;
                self.config.popup_border_color = plugin_toml.config.popup_border_color;
                self.config.popup_padding = plugin_toml.config.popup_padding;

                eprintln!("Diagnostics plugin config updated");
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse diagnostics config: {}", e);
                Err(PluginError::Other(format!("Config parse error: {}", e).into()))
            }
        }
    }
}

// === Plugin Entry Point ===

#[no_mangle]
pub extern "C" fn diagnostics_plugin_create() -> Box<dyn Plugin> {
    Box::new(DiagnosticsPlugin::new())
}

// === Public API ===

impl DiagnosticsPlugin {
    /// Add a diagnostic
    pub fn add_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Add a diagnostic with line text for accurate positioning
    pub fn add_diagnostic_with_line_text(&mut self, diagnostic: Diagnostic, line_text: String) {
        let line = diagnostic.line;
        self.diagnostics.push(diagnostic);
        self.line_texts.insert(line, line_text);
    }

    /// Set line text for accurate width calculation
    pub fn set_line_text(&mut self, line: usize, text: String) {
        self.line_texts.insert(line, text);
    }

    /// Clear all diagnostics
    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
        self.line_texts.clear();
        self.hovered_diagnostic = None;
    }

    /// Get diagnostics count
    pub fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }
}