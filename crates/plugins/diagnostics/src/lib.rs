//! Diagnostics Plugin - Renders squiggly lines under text and shows popups on hover

use ahash::AHashMap as HashMap;
use serde::Deserialize;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use tiny_font::SharedFontSystem;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::Buffer;
use tiny_sdk::{
    ffi::{
        BindGroupLayoutId, BufferId, PipelineId, ShaderModuleId, VertexAttributeDescriptor,
        VertexFormat,
    },
    types::RoundedRectInstance,
    Capability, Configurable, Initializable, LayoutPos, Library, PaintContext, Paintable, Plugin,
    PluginError, SetupContext, ViewportInfo,
};
use tiny_ui::{TextView, Viewport};

/// Diagnostic severity levels
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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
    /// Precise measured X positions (in logical pixels from layout cache)
    pub start_x: f32,
    pub end_x: f32,
    /// Diagnostic message
    pub message: String,
    /// Severity level
    pub severity: DiagnosticSeverity,
}

/// Represents a symbol with its position
#[derive(Debug, Clone)]
pub struct Symbol {
    /// Symbol name
    pub name: String,
    /// Line number (0-indexed)
    pub line: usize,
    /// Column range (start, end) - 0-indexed character positions
    pub column_range: (usize, usize),
    /// Precise measured X positions (in logical pixels from layout cache)
    pub start_x: f32,
    pub end_x: f32,
    /// Symbol kind (function, struct, etc)
    pub kind: String,
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

/// Vertex data for squiggly lines
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct DiagnosticVertex {
    position: [f32; 2],
    color: u32,
    line_info: [f32; 4], // x, y, width, severity
}

/// Main diagnostics plugin struct
pub struct DiagnosticsPlugin {
    // Configuration
    config: DiagnosticsConfig,

    // Diagnostics data
    diagnostics: Vec<Diagnostic>,

    // Symbol positions from LSP
    symbols: Vec<Symbol>,

    // Hover state machine
    hover_state: HoverState,
    hover_start_time: Option<Instant>,
    last_hover_request: Option<(usize, usize)>, // (line, col) of last hover request

    // Line text cache for accurate width calculation
    line_texts: HashMap<usize, String>,

    // Current mouse position (screen coordinates)
    mouse_position: (f32, f32),
    // Mouse position in layout/document space (for diagnostic hit testing)
    mouse_layout_x: f32,
    mouse_line: Option<usize>,
    mouse_column: Option<usize>,

    // Current popup content to show
    current_popup: Option<PopupContent>,

    // Popup TextView for rendering popup content (RwLock for interior mutability in paint)
    popup_view: RwLock<Option<TextView>>,

    // Viewport info
    viewport: ViewportInfo,

    // Cache tracking for invalidation
    last_font_size: f32,

    // GPU resources (RwLock for interior mutability in paint())
    vertex_buffer: RwLock<Option<Buffer>>,
    vertex_buffer_id: RwLock<Option<BufferId>>,
    custom_pipeline_id: Option<PipelineId>,
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,
}

/// Hover state machine
#[derive(Debug, Clone)]
enum HoverState {
    None,
    WaitingForDelay {
        #[allow(dead_code)]
        over_symbol: bool,
        line: usize,
        column: usize,
        anchor_x: f32, // Precise X position of symbol
    },
    RequestingHover {
        line: usize,
        column: usize,
        anchor_x: f32,
    },
    ShowingHover {
        #[allow(dead_code)]
        content: String,
        line: usize,
        column: usize,
        anchor_x: f32,
    },
}

/// Popup content to display
#[derive(Debug, Clone)]
enum PopupContent {
    Diagnostic {
        message: String,
        line: usize,
        anchor_x: f32, // Precise X position in layout space
    },
    Hover {
        content: String,
        line: usize,
        anchor_x: f32, // Precise X position in layout space
    },
}

impl Default for DiagnosticsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsPlugin {
    /// Create a new diagnostics plugin
    pub fn new() -> Self {
        use tiny_sdk::{LogicalSize, PhysicalSize};

        Self {
            config: DiagnosticsConfig::default(),
            diagnostics: Vec::new(),
            symbols: Vec::new(),
            line_texts: HashMap::new(),
            hover_state: HoverState::None,
            hover_start_time: None,
            last_hover_request: None,
            mouse_position: (0.0, 0.0),
            mouse_layout_x: 0.0,
            mouse_line: None,
            mouse_column: None,
            current_popup: None,
            popup_view: RwLock::new(None),
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
            last_font_size: 14.0,
            vertex_buffer: RwLock::new(None),
            vertex_buffer_id: RwLock::new(None),
            custom_pipeline_id: None,
            device: None,
            queue: None,
        }
    }

    /// Update viewport information
    pub fn set_viewport_info(&mut self, viewport: ViewportInfo) {
        // Invalidate cache if font size changed
        if (viewport.font_size - self.last_font_size).abs() > 0.01 {
            self.invalidate_position_cache();
            self.last_font_size = viewport.font_size;
        }
        self.viewport = viewport;
    }

    /// Invalidate cached positions when font size changes (host must re-provide positions)
    pub fn invalidate_position_cache(&mut self) {
        // Clear diagnostics when positions become invalid
        // Host must re-add diagnostics with new positions from updated layout
        self.diagnostics.clear();
    }

    /// Update mouse position for hover detection (in editor-local coordinates)
    pub fn set_mouse_position(
        &mut self,
        x: f32,
        y: f32,
        widget_viewport: Option<&tiny_sdk::types::WidgetViewport>,
        services: Option<&tiny_sdk::ServiceRegistry>,
    ) {
        self.mouse_position = (x, y);

        // Calculate document position from mouse coordinates
        if let Some(widget_viewport) = widget_viewport {
            let widget_offset_y = widget_viewport.bounds.y.0;
            let widget_scroll_y = widget_viewport.scroll.y.0;
            let widget_offset_x = widget_viewport.bounds.x.0;
            let widget_scroll_x = widget_viewport.scroll.x.0;

            // Convert mouse position to document coordinates
            let local_mouse_x = x - widget_offset_x;
            let local_mouse_y = y - widget_offset_y;

            // Calculate line number
            let doc_y = local_mouse_y + widget_scroll_y;
            let line = (doc_y / self.viewport.line_height) as usize;

            // Calculate column using font metrics
            if let Some(services) = services {
                if let Some(font_service) = services.get::<SharedFontSystem>() {
                    let char_width = font_service.char_width_coef() * self.viewport.font_size;
                    let doc_x = local_mouse_x + widget_scroll_x;
                    let column = (doc_x / char_width) as usize;

                    // Store layout-space X coordinate for diagnostic hit testing
                    self.mouse_layout_x = doc_x;

                    self.mouse_line = Some(line);
                    self.mouse_column = Some(column);
                }
            }
        }

        // Update hover state
        self.update_hover_state();
    }

    /// Update hover state based on current mouse position
    fn update_hover_state(&mut self) {
        let (line, column) = match (self.mouse_line, self.mouse_column) {
            (Some(l), Some(c)) => (l, c),
            _ => {
                // Mouse not over text
                self.hover_state = HoverState::None;
                self.current_popup = None;
                return;
            }
        };

        // Check if we're over a diagnostic (show immediately)
        // Use precise pixel positions instead of character column approximations
        // IMPORTANT: Use layout-space X coordinate, not screen coordinate!
        let mouse_x = self.mouse_layout_x;

        for diagnostic in &self.diagnostics {
            if diagnostic.line == line
                && mouse_x >= diagnostic.start_x
                && mouse_x < diagnostic.end_x
            {
                self.current_popup = Some(PopupContent::Diagnostic {
                    message: diagnostic.message.clone(),
                    line: diagnostic.line,
                    anchor_x: diagnostic.start_x,
                });
                return;
            }
        }

        // Check if we're over a symbol (only if we have symbols loaded)
        // Use precise pixel positions for accurate hover detection (layout space)
        let hovered_symbol = if self.symbols.is_empty() {
            None
        } else {
            self.symbols.iter().find(|symbol| {
                symbol.line == line && mouse_x >= symbol.start_x && mouse_x < symbol.end_x
            })
        };
        let over_symbol = hovered_symbol.is_some();

        // Update hover state machine
        match &self.hover_state {
            HoverState::None => {
                if let Some(symbol) = hovered_symbol {
                    self.hover_state = HoverState::WaitingForDelay {
                        over_symbol: true,
                        line,
                        column,
                        anchor_x: symbol.start_x,
                    };
                    self.hover_start_time = Some(Instant::now());
                } else {
                    self.current_popup = None;
                }
            }
            HoverState::WaitingForDelay {
                over_symbol: _,
                line: prev_line,
                column: prev_column,
                anchor_x: prev_anchor_x,
            } => {
                let current_anchor_x = hovered_symbol.map(|s| s.start_x);
                if line != *prev_line
                    || column != *prev_column
                    || current_anchor_x != Some(*prev_anchor_x)
                {
                    // Position changed, reset
                    if let Some(symbol) = hovered_symbol {
                        self.hover_state = HoverState::WaitingForDelay {
                            over_symbol: true,
                            line,
                            column,
                            anchor_x: symbol.start_x,
                        };
                        self.hover_start_time = Some(Instant::now());
                    } else {
                        self.hover_state = HoverState::None;
                        self.current_popup = None;
                    }
                } else if !over_symbol {
                    // Moved off symbol
                    self.hover_state = HoverState::None;
                    self.current_popup = None;
                }
            }
            HoverState::RequestingHover {
                line: prev_line,
                column: prev_column,
                anchor_x: prev_anchor_x,
            } => {
                let current_anchor_x = hovered_symbol.map(|s| s.start_x);
                if line != *prev_line
                    || column != *prev_column
                    || !over_symbol
                    || current_anchor_x != Some(*prev_anchor_x)
                {
                    // Position changed or moved off symbol
                    self.hover_state = HoverState::None;
                    self.current_popup = None;
                    if let Some(symbol) = hovered_symbol {
                        self.hover_state = HoverState::WaitingForDelay {
                            over_symbol: true,
                            line,
                            column,
                            anchor_x: symbol.start_x,
                        };
                        self.hover_start_time = Some(Instant::now());
                    }
                }
            }
            HoverState::ShowingHover {
                line: prev_line,
                column: prev_column,
                anchor_x: prev_anchor_x,
                ..
            } => {
                let current_anchor_x = hovered_symbol.map(|s| s.start_x);
                if line != *prev_line
                    || column != *prev_column
                    || !over_symbol
                    || current_anchor_x != Some(*prev_anchor_x)
                {
                    // Position changed or moved off symbol
                    self.hover_state = HoverState::None;
                    self.current_popup = None;
                    if let Some(symbol) = hovered_symbol {
                        self.hover_state = HoverState::WaitingForDelay {
                            over_symbol: true,
                            line,
                            column,
                            anchor_x: symbol.start_x,
                        };
                        self.hover_start_time = Some(Instant::now());
                    }
                }
            }
        }
    }

    /// Create vertices for squiggly lines
    fn create_squiggly_vertices(
        &self,
        widget_viewport: Option<&tiny_sdk::types::WidgetViewport>,
        _services: Option<&tiny_sdk::ServiceRegistry>,
    ) -> Vec<DiagnosticVertex> {
        let mut vertices = Vec::new();
        let scale = self.viewport.scale_factor;

        // Get widget bounds offset and scroll
        let widget_offset_x = widget_viewport.map(|w| w.bounds.x.0).unwrap_or(0.0);
        let widget_offset_y = widget_viewport.map(|w| w.bounds.y.0).unwrap_or(0.0);
        let widget_scroll_x = widget_viewport
            .map(|w| w.scroll.x.0)
            .unwrap_or(self.viewport.scroll.x.0);
        let widget_scroll_y = widget_viewport
            .map(|w| w.scroll.y.0)
            .unwrap_or(self.viewport.scroll.y.0);

        for diagnostic in &self.diagnostics {
            // Positions from layout cache are in layout space (0,0 origin)
            // They already account for actual glyph positions, no approximation
            let layout_start_x = diagnostic.start_x;
            let layout_end_x = diagnostic.end_x;
            let width = layout_end_x - layout_start_x;

            // Y position from line number
            let layout_y = diagnostic.line as f32 * self.viewport.line_height;
            // Position at bottom of line for squiggly effect
            let layout_line_y = layout_y + self.viewport.line_height - 2.0;

            // Convert to view space (subtract scroll)
            let view_x = layout_start_x - widget_scroll_x;
            let view_y = layout_line_y - widget_scroll_y;

            // Transform to screen space: add widget offset and scale to physical pixels
            let screen_x = (view_x + widget_offset_x) * scale;
            let screen_y = (view_y + widget_offset_y) * scale;
            let width_scaled = width * scale;

            // Create a quad that covers the area where the squiggly line will be drawn
            let padding = 4.0 * scale; // Extra height for the wave amplitude
            let line_info = [
                screen_x,
                screen_y,
                width_scaled,
                diagnostic.severity as i32 as f32,
            ];

            // Color isn't used for squiggly lines (determined by severity in shader)
            let color = 0x00000000u32;

            // Create quad vertices
            vertices.extend_from_slice(&[
                DiagnosticVertex {
                    position: [screen_x, screen_y - padding],
                    color,
                    line_info,
                },
                DiagnosticVertex {
                    position: [screen_x + width_scaled, screen_y - padding],
                    color,
                    line_info,
                },
                DiagnosticVertex {
                    position: [screen_x, screen_y + padding],
                    color,
                    line_info,
                },
                DiagnosticVertex {
                    position: [screen_x + width_scaled, screen_y - padding],
                    color,
                    line_info,
                },
                DiagnosticVertex {
                    position: [screen_x + width_scaled, screen_y + padding],
                    color,
                    line_info,
                },
                DiagnosticVertex {
                    position: [screen_x, screen_y + padding],
                    color,
                    line_info,
                },
            ]);
        }

        vertices
    }

    /// Update or create popup TextView with content
    fn update_popup_view(
        &self,
        content: &str,
        anchor_line: usize,
        anchor_x: f32, // Precise X position in layout space (REQUIRED)
        widget_viewport: Option<&tiny_sdk::types::WidgetViewport>,
        services: Option<&tiny_sdk::ServiceRegistry>,
    ) {
        // Get font service
        let font_service = services
            .and_then(|s| s.get::<SharedFontSystem>())
            .expect("Font service required for popup rendering");

        // Get widget bounds and scroll
        let widget_bounds = widget_viewport
            .map(|w| w.bounds)
            .unwrap_or_else(|| tiny_sdk::types::LayoutRect::new(0.0, 0.0, 800.0, 600.0));
        let widget_scroll_x = widget_viewport.map(|w| w.scroll.x.0).unwrap_or(0.0);
        let widget_scroll_y = widget_viewport.map(|w| w.scroll.y.0).unwrap_or(0.0);

        // Use precise anchor position (no approximation)
        let layout_x = anchor_x;
        let layout_y = anchor_line as f32 * self.viewport.line_height;

        // Convert to view space
        let view_x = layout_x - widget_scroll_x;
        let view_y = layout_y - widget_scroll_y;

        // Calculate popup size
        let layout = font_service.layout_text(content, self.viewport.font_size);
        let max_width = 600.0;
        let max_height = 400.0;
        let popup_width = (layout.width + self.config.popup_padding * 2.0).min(max_width);
        let popup_height = (layout.height + self.config.popup_padding * 2.0).min(max_height);

        // Smart positioning: try above first, then below
        let mut popup_x = view_x + widget_bounds.x.0;
        let mut popup_y = view_y + widget_bounds.y.0 - popup_height - 10.0; // 10px above

        // Check if popup fits above
        if popup_y < widget_bounds.y.0 {
            // Not enough space above, position below
            popup_y = view_y + widget_bounds.y.0 + self.viewport.line_height + 10.0;
        }

        // Check if popup fits horizontally
        if popup_x + popup_width > widget_bounds.x.0 + widget_bounds.width.0 {
            popup_x = widget_bounds.x.0 + widget_bounds.width.0 - popup_width - 10.0;
        }
        popup_x = popup_x.max(widget_bounds.x.0 + 10.0);

        // Create viewport for TextView with proper metrics
        let mut popup_viewport =
            Viewport::new(popup_width, popup_height, self.viewport.scale_factor);
        popup_viewport.bounds =
            tiny_sdk::types::LayoutRect::new(popup_x, popup_y, popup_width, popup_height);
        popup_viewport.set_font_size(self.viewport.font_size);

        // Create TextView
        let mut text_view = TextView::from_text(content, popup_viewport);
        text_view.padding_x = self.config.popup_padding;
        text_view.padding_y = self.config.popup_padding;
        text_view.update_layout(&font_service);

        *self.popup_view.write().unwrap() = Some(text_view);
    }

    /// Get rounded rect for popup frame
    fn get_popup_frame(&self) -> Option<RoundedRectInstance> {
        let popup_view = self.popup_view.read().unwrap();
        let popup_view = popup_view.as_ref()?;
        let bounds = popup_view.viewport.bounds;

        Some(RoundedRectInstance {
            rect: bounds,
            color: self.config.popup_background_color,
            border_color: self.config.popup_border_color,
            corner_radius: 4.0,
            border_width: 1.0,
        })
    }
}

// === Plugin Trait Implementation ===

tiny_sdk::plugin! {
    DiagnosticsPlugin {
        name: "diagnostics",
        version: "0.1.0",
        z_index: 50,
        traits: [Init, Paint, Library, Config],
        defaults: [],  // All custom implementations
    }
}

// === Initializable Trait Implementation ===

impl Initializable for DiagnosticsPlugin {
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        self.device = Some(ctx.device.clone());
        self.queue = Some(ctx.queue.clone());

        // Create vertex buffers
        let vertex_size = std::mem::size_of::<DiagnosticVertex>();
        // Each diagnostic creates 6 vertices (2 triangles), support up to 50 diagnostics
        let buffer_size = (vertex_size * 6 * 50) as u64; // Space for 50 diagnostics

        // Buffer for squiggly lines
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Diagnostics Vertex Buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *self.vertex_buffer.write().unwrap() = Some(vertex_buffer);

        let buffer_id = BufferId::create(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        *self.vertex_buffer_id.write().unwrap() = Some(buffer_id);

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
        ];

        let pipeline_id = PipelineId::create_with_layout(
            shader_id,
            shader_id,
            bind_group_layout,
            28, // vertex stride: position (8) + color (4) + line_info (16) = 28
            &attributes,
        );
        self.custom_pipeline_id = Some(pipeline_id);

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
                // Store the mouse position directly since we need cached viewport/services from paint()
                self.mouse_position = (x, y);
                // Note: actual hover detection will happen in paint() when we have viewport/services
                Ok(Vec::new())
            }
            "add_diagnostic" => {
                // Format: line (u32), col_start (u32), col_end (u32), severity (u8),
                //         start_x (f32), end_x (f32), message_len (u32), message (bytes)
                if args.len() < 25 {
                    return Err(PluginError::Other("Invalid diagnostic args".into()));
                }

                let line: u32 = *bytemuck::from_bytes(&args[0..4]);
                let col_start: u32 = *bytemuck::from_bytes(&args[4..8]);
                let col_end: u32 = *bytemuck::from_bytes(&args[8..12]);
                let severity = match args[12] {
                    0 => DiagnosticSeverity::Error,
                    1 => DiagnosticSeverity::Warning,
                    _ => DiagnosticSeverity::Info,
                };
                let start_x: f32 = *bytemuck::from_bytes(&args[13..17]);
                let end_x: f32 = *bytemuck::from_bytes(&args[17..21]);
                let message_len: u32 = *bytemuck::from_bytes(&args[21..25]);

                if args.len() < 25 + message_len as usize {
                    return Err(PluginError::Other("Invalid message length".into()));
                }

                let message =
                    String::from_utf8_lossy(&args[25..25 + message_len as usize]).to_string();

                self.diagnostics.push(Diagnostic {
                    line: line as usize,
                    column_range: (col_start as usize, col_end as usize),
                    start_x,
                    end_x,
                    message,
                    severity,
                });
                Ok(Vec::new())
            }
            "clear_diagnostics" => {
                self.diagnostics.clear();
                self.line_texts.clear();
                self.current_popup = None;
                self.hover_state = HoverState::None;
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
            "set_symbols" => {
                // Format: count (u32), then for each symbol:
                //   line (u32), col_start (u32), col_end (u32), start_x (f32), end_x (f32),
                //   kind_len (u32), kind, name_len (u32), name
                if args.len() < 4 {
                    return Err(PluginError::Other("Invalid symbols args".into()));
                }

                let count: u32 = *bytemuck::from_bytes(&args[0..4]);
                self.symbols.clear();
                self.symbols.reserve(count as usize);

                let mut offset = 4;
                for _ in 0..count {
                    if args.len() < offset + 24 {
                        return Err(PluginError::Other("Invalid symbol data".into()));
                    }

                    let line: u32 = *bytemuck::from_bytes(&args[offset..offset + 4]);
                    let col_start: u32 = *bytemuck::from_bytes(&args[offset + 4..offset + 8]);
                    let col_end: u32 = *bytemuck::from_bytes(&args[offset + 8..offset + 12]);
                    let start_x: f32 = *bytemuck::from_bytes(&args[offset + 12..offset + 16]);
                    let end_x: f32 = *bytemuck::from_bytes(&args[offset + 16..offset + 20]);
                    let kind_len: u32 = *bytemuck::from_bytes(&args[offset + 20..offset + 24]);

                    offset += 24;
                    if args.len() < offset + kind_len as usize {
                        return Err(PluginError::Other("Invalid symbol kind length".into()));
                    }

                    let kind = String::from_utf8_lossy(&args[offset..offset + kind_len as usize])
                        .to_string();
                    offset += kind_len as usize;

                    if args.len() < offset + 4 {
                        return Err(PluginError::Other(
                            "Invalid symbol name length header".into(),
                        ));
                    }
                    let name_len: u32 = *bytemuck::from_bytes(&args[offset..offset + 4]);
                    offset += 4;

                    if args.len() < offset + name_len as usize {
                        return Err(PluginError::Other("Invalid symbol name length".into()));
                    }

                    let name = String::from_utf8_lossy(&args[offset..offset + name_len as usize])
                        .to_string();
                    offset += name_len as usize;

                    self.symbols.push(Symbol {
                        name,
                        line: line as usize,
                        column_range: (col_start as usize, col_end as usize),
                        start_x,
                        end_x,
                        kind,
                    });
                }
                Ok(Vec::new())
            }
            "set_hover_content" => {
                // Format: line (u32), column (u32), content_len (u32), content (bytes)
                if args.len() < 12 {
                    return Err(PluginError::Other("Invalid hover content args".into()));
                }

                let line = u32::from_le_bytes(args[0..4].try_into().unwrap()) as usize;
                let column = u32::from_le_bytes(args[4..8].try_into().unwrap()) as usize;
                let content_len = u32::from_le_bytes(args[8..12].try_into().unwrap()) as usize;

                if args.len() < 12 + content_len {
                    return Err(PluginError::Other("Invalid hover content length".into()));
                }

                let content = String::from_utf8_lossy(&args[12..12 + content_len]).to_string();
                self.set_hover_content(content, line, column);
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
            ctx.context_data
                .as_ref()
                .map(|data| &*(data as *const _ as *const tiny_sdk::ServiceRegistry))
        };

        // Draw squiggly lines
        let vertices = self.create_squiggly_vertices(ctx.widget_viewport.as_ref(), services);
        if !vertices.is_empty() {
            let vertex_data = bytemuck::cast_slice(&vertices);
            let vertex_count = vertices.len() as u32;
            let required_size = vertex_data.len() as u64;

            // Recreate buffer if it's too small
            if let Some(device) = &self.device {
                let needs_new_buffer = {
                    let buffer = self.vertex_buffer.read().unwrap();
                    buffer.is_none()
                        || buffer
                            .as_ref()
                            .map(|b| b.size() < required_size)
                            .unwrap_or(true)
                };

                if needs_new_buffer {
                    // Create new buffer with exact size needed (plus some padding)
                    let buffer_size = (required_size + 1024).max(required_size * 2); // Add padding for growth
                    let new_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("Diagnostics Vertex Buffer (Dynamic)"),
                        size: buffer_size,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    });
                    *self.vertex_buffer.write().unwrap() = Some(new_buffer);

                    let new_buffer_id = BufferId::create(
                        buffer_size,
                        wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    );
                    *self.vertex_buffer_id.write().unwrap() = Some(new_buffer_id);
                }
            }

            if let Some(buffer_id) = self.vertex_buffer_id.read().unwrap().as_ref() {
                buffer_id.write(0, vertex_data);

                if let Some(ref gpu_ctx) = ctx.gpu_context {
                    if let Some(pipeline_id) = self.custom_pipeline_id {
                        gpu_ctx.set_pipeline(render_pass, pipeline_id);
                        gpu_ctx.set_bind_group(render_pass, 0, gpu_ctx.uniform_bind_group_id);
                        gpu_ctx.set_vertex_buffer(render_pass, 0, *buffer_id);
                        gpu_ctx.draw(render_pass, vertex_count, 1);
                    } else {
                        eprintln!("No custom_pipeline_id set!");
                    }
                } else {
                    eprintln!("No gpu_context!");
                }
            } else {
                eprintln!("No vertex_buffer_id!");
            }
        }

        // Show popup if we have one
        if let Some(ref popup_content) = self.current_popup {
            match popup_content {
                PopupContent::Diagnostic {
                    message,
                    line,
                    anchor_x,
                } => {
                    self.update_popup_view(
                        message,
                        *line,
                        *anchor_x,
                        ctx.widget_viewport.as_ref(),
                        services,
                    );
                }
                PopupContent::Hover {
                    content,
                    line,
                    anchor_x,
                } => {
                    self.update_popup_view(
                        content,
                        *line,
                        *anchor_x,
                        ctx.widget_viewport.as_ref(),
                        services,
                    );
                }
            }

            // Get popup frame BEFORE acquiring any locks
            let frame = self.get_popup_frame();

            // Render popup using TextView and rounded rect
            if let Ok(mut popup_view_guard) = self.popup_view.try_write() {
                if let Some(popup_view) = popup_view_guard.as_mut() {
                    // Draw rounded rect frame using the core rounded rect renderer
                    if let Some(frame) = frame {
                        unsafe {
                            if !ctx.gpu_renderer.is_null() {
                                let gpu_renderer =
                                    &mut *(ctx.gpu_renderer as *mut tiny_core::GpuRenderer);
                                gpu_renderer.draw_rounded_rects(
                                    render_pass,
                                    &[frame],
                                    self.viewport.scale_factor,
                                );
                            }
                        }
                    }

                    // Draw popup text using TextView
                    if let Some(font_service) = services.and_then(|s| s.get::<SharedFontSystem>()) {
                        let glyphs = popup_view.collect_glyphs(&font_service);

                        if !glyphs.is_empty() {
                            unsafe {
                                if !ctx.gpu_renderer.is_null() {
                                    let gpu_renderer =
                                        &mut *(ctx.gpu_renderer as *mut tiny_core::GpuRenderer);
                                    gpu_renderer.draw_glyphs(
                                        render_pass,
                                        &glyphs,
                                        tiny_core::gpu::DrawConfig {
                                            buffer_name: "diagnostics",
                                            use_themed: true,
                                            scissor: Some(popup_view.get_scissor_rect()),
                                        },
                                    );
                                }
                            }
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
        #[derive(Default, Deserialize)]
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

        fn default_popup_bg() -> u32 {
            0x2D2D30FF
        }
        fn default_popup_text() -> u32 {
            0xCCCCCCFF
        }
        fn default_popup_border() -> u32 {
            0x464647FF
        }
        fn default_popup_padding() -> f32 {
            8.0
        }

        // Parse TOML value first (handles syntax errors gracefully)
        let toml_value: toml::Value = match toml::from_str(config_data) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("❌ TOML syntax error in diagnostics plugin.toml: {}", e);
                eprintln!("   Keeping previous configuration");
                return Ok(()); // Don't fail, just keep current config
            }
        };

        // Extract [config] section and parse fields individually
        if let Some(config_table) = toml_value.get("config").and_then(|v| v.as_table()) {
            let mut temp_config = PluginConfig::default();
            tiny_sdk::parse_fields!(temp_config, config_table, {
                popup_background_color: default_popup_bg(),
                popup_text_color: default_popup_text(),
                popup_border_color: default_popup_border(),
                popup_padding: default_popup_padding(),
            });

            // Apply parsed values
            self.config.popup_background_color = temp_config.popup_background_color;
            self.config.popup_text_color = temp_config.popup_text_color;
            self.config.popup_border_color = temp_config.popup_border_color;
            self.config.popup_padding = temp_config.popup_padding;
        }

        Ok(())
    }
}

// === Plugin Entry Point ===

#[no_mangle]
pub extern "C" fn diagnostics_plugin_create() -> Box<dyn Plugin> {
    Box::new(DiagnosticsPlugin::new())
}

// === Public API ===

impl DiagnosticsPlugin {
    /// Add a diagnostic (host must call set_diagnostic_positions after this)
    pub fn add_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Add a diagnostic with precise positions from layout cache
    pub fn add_diagnostic_with_positions(
        &mut self,
        line: usize,
        column_range: (usize, usize),
        message: String,
        severity: DiagnosticSeverity,
        start_x: f32,
        end_x: f32,
    ) {
        self.diagnostics.push(Diagnostic {
            line,
            column_range,
            start_x,
            end_x,
            message,
            severity,
        });
    }

    /// Set line text for accurate width calculation
    pub fn set_line_text(&mut self, line: usize, text: String) {
        self.line_texts.insert(line, text);
    }

    /// Update timing and check if we should request hover (call every frame)
    pub fn update(&mut self) -> Option<(usize, usize)> {
        // Don't request hover if no symbols loaded yet
        if self.symbols.is_empty() {
            self.hover_state = HoverState::None;
            return None;
        }

        // Check if we need to transition hover state based on timing
        if let HoverState::WaitingForDelay {
            line,
            column,
            anchor_x,
            ..
        } = self.hover_state
        {
            if let Some(start_time) = self.hover_start_time {
                if start_time.elapsed().as_millis() >= 500 {
                    // 500ms elapsed, request hover info
                    self.hover_state = HoverState::RequestingHover {
                        line,
                        column,
                        anchor_x,
                    };
                    self.last_hover_request = Some((line, column));
                    return Some((line, column));
                }
            }
        }
        None
    }

    /// Set hover content received from LSP
    pub fn set_hover_content(&mut self, content: String, line: usize, column: usize) {
        if let HoverState::RequestingHover {
            line: req_line,
            column: req_column,
            anchor_x,
        } = self.hover_state
        {
            if line == req_line && column == req_column {
                self.hover_state = HoverState::ShowingHover {
                    content: content.clone(),
                    line,
                    column,
                    anchor_x,
                };
                self.current_popup = Some(PopupContent::Hover {
                    content,
                    line,
                    anchor_x,
                });
            }
        }
    }

    /// Set document symbols from LSP
    pub fn set_symbols(&mut self, symbols: Vec<Symbol>) {
        self.symbols = symbols;
    }

    /// Clear all symbols
    pub fn clear_symbols(&mut self) {
        self.symbols.clear();
    }

    /// Get current mouse position in document coordinates (line, column)
    pub fn get_mouse_document_position(&self) -> Option<(usize, usize)> {
        self.mouse_line
            .and_then(|line| self.mouse_column.map(|col| (line, col)))
    }

    /// Clear all diagnostics
    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
        self.line_texts.clear();
        self.current_popup = None;
        self.hover_state = HoverState::None;
    }

    /// Get diagnostics count
    pub fn diagnostic_count(&self) -> usize {
        self.diagnostics.len()
    }

    /// Check if GPU resources have been initialized
    pub fn is_initialized(&self) -> bool {
        self.device.is_some()
    }
}
