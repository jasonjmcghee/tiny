//! Selection Plugin - Visual highlight for selected text

use serde::Deserialize;
use std::sync::Arc;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::Buffer;
use tiny_sdk::{
    ffi::{BindGroupLayoutId, BufferId, PipelineId, ShaderModuleId},
    Capability, Configurable, Initializable, LayoutRect, Library, PaintContext, Paintable, Plugin,
    PluginError, SetupContext, ViewPos, ViewportInfo,
};

/// Single selection with start and end positions
#[derive(Debug, Clone)]
pub struct Selection {
    /// Start position in view coordinates (inclusive)
    pub start: ViewPos,
    /// End position in view coordinates (exclusive)
    pub end: ViewPos,
}

/// Selection appearance configuration
#[derive(Debug, Clone)]
pub struct SelectionStyle {
    pub color: u32,
}

/// Configuration loaded from plugin.toml
#[derive(Debug, Clone)]
pub struct SelectionConfig {
    pub style: SelectionStyle,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            style: SelectionStyle {
                color: 0x4080FF40, // Semi-transparent blue
            },
        }
    }
}

/// Main selection plugin struct
pub struct SelectionPlugin {
    // Configuration
    config: SelectionConfig,

    // Current selections
    selections: Vec<Selection>,

    // Viewport info
    viewport: ViewportInfo,

    // GPU resources (created during setup)
    vertex_buffer: Option<Buffer>,
    vertex_buffer_id: Option<BufferId>,
    custom_pipeline_id: Option<PipelineId>,
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,
}

impl SelectionPlugin {
    /// Create a new selection plugin with default configuration
    pub fn new() -> Self {
        use tiny_sdk::{LogicalSize, LayoutPos, PhysicalSize};

        Self {
            config: SelectionConfig::default(),
            selections: Vec::new(),
            viewport: ViewportInfo {
                scroll: LayoutPos::new(0.0, 0.0),
                logical_size: LogicalSize::new(800.0, 600.0),
                physical_size: PhysicalSize { width: 800, height: 600 },
                scale_factor: 1.0,
                line_height: 19.6,
                font_size: 14.0,
                margin: LayoutPos::new(60.0, 10.0),
                global_margin: LayoutPos::new(0.0, 0.0),
            },
            vertex_buffer: None,
            vertex_buffer_id: None,
            custom_pipeline_id: None,
            device: None,
            queue: None,
        }
    }

    /// Update selections from start/end view positions
    pub fn set_selections(&mut self, selections: Vec<(ViewPos, ViewPos)>) {
        self.selections = selections
            .into_iter()
            .map(|(start, end)| {
                // Ensure start comes before end for proper rendering
                if start.y.0 < end.y.0 || (start.y.0 == end.y.0 && start.x.0 <= end.x.0) {
                    Selection { start, end }
                } else {
                    Selection { start: end, end: start }
                }
            })
            .collect();
    }

    /// Update viewport information
    pub fn set_viewport_info(&mut self, viewport: ViewportInfo) {
        // Check if scale factor seems wrong (e.g., 1.0 on a retina display)
        if viewport.scale_factor == 1.0 {
            eprintln!("WARNING: Scale factor is 1.0, might be incorrect for high-DPI display!");
        }
        self.viewport = viewport;
    }


    /// Generate a single bounding rectangle for the entire selection
    fn selection_to_bounding_rect(&self, selection: &Selection) -> Option<LayoutRect> {
        // Skip if it's just a cursor (no selection)
        let epsilon = 0.1;
        if (selection.start.x.0 - selection.end.x.0).abs() < epsilon
            && (selection.start.y.0 - selection.end.y.0).abs() < epsilon
        {
            return None;
        }

        // View positions are already in screen coordinates
        let start_x = selection.start.x.0;
        let start_y = selection.start.y.0;
        let end_x = selection.end.x.0;
        let end_y = selection.end.y.0;

        // Check if single line (same y position)
        if (start_y - end_y).abs() < epsilon {
            // Single line selection - simple rectangle from start to end
            Some(LayoutRect::new(
                start_x,
                start_y,
                end_x - start_x,
                self.viewport.line_height,
            ))
        } else {
            // Multi-line selection: always use full width from margin to margin
            // The shader will handle per-line clipping based on selection data
            let left = self.viewport.margin.x.0;
            let right = self.viewport.logical_size.width.0 - self.viewport.margin.x.0;
            let width = right - left;
            let height = (end_y + self.viewport.line_height) - start_y;

            Some(LayoutRect::new(left, start_y, width, height))
        }
    }

    /// Create vertex data for all selection rectangles
    fn create_vertices(&self, viewport: &tiny_sdk::ViewportInfo) -> Vec<SelectionVertex> {
        let mut vertices = Vec::new();
        // Use viewport's scale factor, not our stored one (which might be wrong)
        let scale = viewport.scale_factor;
        let color = self.config.style.color;

        for selection in &self.selections {
            if let Some(rect) = self.selection_to_bounding_rect(selection) {
                // Rectangle is in logical view space, scale to physical pixels
                let x = rect.x.0 * scale;
                let y = rect.y.0 * scale;
                let w = rect.width.0 * scale;
                let h = rect.height.0 * scale;

                // Pass selection info in vertex data for shader to determine visibility
                // View positions are already in logical pixels (screen coordinates)
                let start_x = selection.start.x.0 * scale;
                let start_y = selection.start.y.0 * scale;
                let end_x = selection.end.x.0 * scale;
                let end_y = selection.end.y.0 * scale;
                let line_height = self.viewport.line_height * scale;
                // Margins are also in view coordinates now
                let margin_left = self.viewport.margin.x.0 * scale;
                let margin_right = (self.viewport.logical_size.width.0 - self.viewport.margin.x.0) * scale;

                let selection_data = SelectionData {
                    start_pos: [start_x, start_y],
                    end_pos: [end_x, end_y],
                    line_height,
                    margin_left,
                    margin_right,
                    _padding: 0.0,
                };

                // Create two triangles for a quad
                vertices.extend_from_slice(&[
                    SelectionVertex {
                        position: [x, y],
                        color,
                        selection_data,
                    },
                    SelectionVertex {
                        position: [x + w, y],
                        color,
                        selection_data,
                    },
                    SelectionVertex {
                        position: [x, y + h],
                        color,
                        selection_data,
                    },
                    SelectionVertex {
                        position: [x + w, y],
                        color,
                        selection_data,
                    },
                    SelectionVertex {
                        position: [x + w, y + h],
                        color,
                        selection_data,
                    },
                    SelectionVertex {
                        position: [x, y + h],
                        color,
                        selection_data,
                    },
                ]);
            }
        }

        vertices
    }
}

/// Selection data passed to shader
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct SelectionData {
    start_pos: [f32; 2],
    end_pos: [f32; 2],
    line_height: f32,
    margin_left: f32,
    margin_right: f32,
    _padding: f32, // Ensure 16-byte alignment
}

/// Vertex data for selection rendering
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct SelectionVertex {
    position: [f32; 2],
    color: u32,
    selection_data: SelectionData,
}

// === Plugin Trait Implementation ===

impl Plugin for SelectionPlugin {
    fn name(&self) -> &str {
        "selection"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("selection".to_string()),
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

impl Initializable for SelectionPlugin {
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        // Store device and queue for later use
        self.device = Some(ctx.device.clone());
        self.queue = Some(ctx.queue.clone());

        // Create vertex buffer with reasonable initial size
        // Estimate: avg 2 selections * 3 rects each * 6 vertices per rect
        let vertex_size = std::mem::size_of::<SelectionVertex>();
        let buffer_size = (vertex_size * 36) as u64;

        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Selection Plugin Vertex Buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.vertex_buffer = Some(vertex_buffer);

        // Also create an FFI buffer ID for reuse
        let buffer_id = BufferId::create(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        self.vertex_buffer_id = Some(buffer_id);

        // Create custom shader for selection rendering
        let shader_source = r#"
// Vertex shader
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: u32,
    @location(2) start_pos: vec2<f32>,
    @location(3) end_pos: vec2<f32>,
    @location(4) line_height: f32,
    @location(5) margin_left: f32,
    @location(6) margin_right: f32,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) pixel_pos: vec2<f32>,
    @location(2) start_pos: vec2<f32>,
    @location(3) end_pos: vec2<f32>,
    @location(4) line_height: f32,
    @location(5) margin_left: f32,
    @location(6) margin_right: f32,
}

struct Uniforms {
    viewport_size: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Convert from pixel coordinates to normalized device coordinates
    let x = (input.position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    let y = 1.0 - (input.position.y / uniforms.viewport_size.y) * 2.0;

    out.clip_position = vec4<f32>(x, y, 0.0, 1.0);
    out.pixel_pos = input.position;

    // Unpack color from u32 to vec4
    let r = f32((input.color >> 24u) & 0xFFu) / 255.0;
    let g = f32((input.color >> 16u) & 0xFFu) / 255.0;
    let b = f32((input.color >> 8u) & 0xFFu) / 255.0;
    let a = f32(input.color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    // Pass through selection data
    out.start_pos = input.start_pos;
    out.end_pos = input.end_pos;
    out.line_height = input.line_height;
    out.margin_left = input.margin_left;
    out.margin_right = input.margin_right;

    return out;
}

// Fragment shader
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let px = input.pixel_pos.x;
    let py = input.pixel_pos.y;

    // Check if pixel should be visible based on selection shape
    var visible = false;

    // For single line selections
    if abs(input.start_pos.y - input.end_pos.y) < 0.1 {
        // Single line selection - simple range check
        if py >= input.start_pos.y && py < input.start_pos.y + input.line_height &&
           px >= input.start_pos.x && px <= input.end_pos.x {
            visible = true;
        }
    } else {
        // Multi-line selection - check which part of the selection we're in
        if py >= input.start_pos.y && py < input.start_pos.y + input.line_height {
            // First line: from start_x to right margin
            if px >= input.start_pos.x && px <= input.margin_right {
                visible = true;
            }
        } else if py >= input.end_pos.y && py < input.end_pos.y + input.line_height {
            // Last line: from left margin to end_x
            if px >= input.margin_left && px <= input.end_pos.x {
                visible = true;
            }
        } else if py > input.start_pos.y && py < input.end_pos.y {
            // Middle lines: full width from left to right margin
            if px >= input.margin_left && px <= input.margin_right {
                visible = true;
            }
        }
    }

    if visible {
        // Debug: show UV coordinates instead of color
        // Calculate UV within the bounding box
        let min_x = min(input.start_pos.x, input.margin_left);
        let max_x = max(input.end_pos.x, input.margin_right);
        let min_y = input.start_pos.y;
        let max_y = input.end_pos.y + input.line_height;

        let u = (px - min_x) / (max_x - min_x);
        let v = (py - min_y) / (max_y - min_y);

        // return vec4<f32>(u, v, 1.0, 0.2);
        return input.color;
    } else {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0); // Transparent
    }
}
"#;

        // Create shader modules
        let shader_id = ShaderModuleId::create_from_wgsl(shader_source);

        // Create bind group layout for uniforms
        let bind_group_layout = BindGroupLayoutId::create_uniform();

        // Define vertex attributes for our SelectionVertex layout
        let attributes = vec![
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 0,
                location: 0,
                format: tiny_sdk::ffi::VertexFormat::Float32x2, // position
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 8,
                location: 1,
                format: tiny_sdk::ffi::VertexFormat::Uint32, // color
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 12,
                location: 2,
                format: tiny_sdk::ffi::VertexFormat::Float32x2, // start_pos
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 20,
                location: 3,
                format: tiny_sdk::ffi::VertexFormat::Float32x2, // end_pos
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 28,
                location: 4,
                format: tiny_sdk::ffi::VertexFormat::Float32, // line_height
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 32,
                location: 5,
                format: tiny_sdk::ffi::VertexFormat::Float32, // margin_left
            },
            tiny_sdk::ffi::VertexAttributeDescriptor {
                offset: 36,
                location: 6,
                format: tiny_sdk::ffi::VertexFormat::Float32, // margin_right
            },
        ];

        // Create pipeline with custom vertex layout
        let pipeline_id = PipelineId::create_with_layout(
            shader_id,
            shader_id,
            bind_group_layout,
            44, // vertex stride: position (8) + color (4) + selection_data (32) = 44 bytes
            &attributes,
        );
        self.custom_pipeline_id = Some(pipeline_id);

        eprintln!("Selection plugin created custom pipeline with proper vertex layout");

        Ok(())
    }
}

// === Library Trait Implementation ===

impl Library for SelectionPlugin {
    fn name(&self) -> &str {
        "selection_api"
    }

    fn call(&mut self, method: &str, args: &[u8]) -> Result<Vec<u8>, PluginError> {
        match method {
            "set_selections" => {
                // Format: count (u32), then ViewPos pairs
                if args.len() < 4 {
                    return Err(PluginError::Other("Invalid args: too short".into()));
                }

                // Read count
                let count_bytes: [u8; 4] = args[0..4].try_into()
                    .map_err(|_| PluginError::Other("Invalid count".into()))?;
                let selection_count = u32::from_le_bytes(count_bytes) as usize;

                let view_pos_size = std::mem::size_of::<ViewPos>();
                let expected_size = 4 + (selection_count * 2 * view_pos_size);
                if args.len() < expected_size {
                    return Err(PluginError::Other("Invalid args: incomplete selection data".into()));
                }

                let mut selections = Vec::with_capacity(selection_count);
                let mut offset = 4;

                for _ in 0..selection_count {
                    // Use bytemuck to deserialize ViewPos directly
                    let start_bytes = &args[offset..offset + view_pos_size];
                    let end_bytes = &args[offset + view_pos_size..offset + 2 * view_pos_size];

                    let start: &ViewPos = bytemuck::from_bytes(start_bytes);
                    let end: &ViewPos = bytemuck::from_bytes(end_bytes);

                    selections.push((*start, *end));
                    offset += 2 * view_pos_size;
                }

                // Update our selections
                self.set_selections(selections);
                Ok(Vec::new())
            }
            "set_viewport_info" => {
                // Expect ViewportInfo struct
                let viewport_info_size = std::mem::size_of::<ViewportInfo>();
                if args.len() < viewport_info_size {
                    return Err(PluginError::Other("Invalid viewport args".into()));
                }

                // Use bytemuck to deserialize ViewportInfo directly
                let viewport_info: &ViewportInfo = bytemuck::from_bytes(&args[0..viewport_info_size]);
                self.set_viewport_info(*viewport_info);
                Ok(Vec::new())
            }
            _ => Err(PluginError::Other("Unknown method".into())),
        }
    }
}

// === Paint Trait Implementation ===

impl Paintable for SelectionPlugin {
    fn z_index(&self) -> i32 {
        -10 // Behind text, same as old SelectionWidget
    }

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Create vertices for current frame
        let vertices = self.create_vertices(&ctx.viewport);
        if vertices.is_empty() {
            return;
        }

        let vertex_data = bytemuck::cast_slice(&vertices);
        let vertex_count = vertices.len() as u32;

        // Check if we need a larger buffer
        let _required_size = vertex_data.len() as u64;

        // Recreate buffer if needed
        if let Some(buffer_id) = self.vertex_buffer_id {
            // For now, just write to existing buffer - could check size first
            buffer_id.write(0, vertex_data);

            // Use atomic render operations with our custom pipeline
            if let Some(ref gpu_ctx) = ctx.gpu_context {
                if let Some(pipeline_id) = self.custom_pipeline_id {
                    // Use our custom pipeline
                    gpu_ctx.set_pipeline(render_pass, pipeline_id);
                    // Use the host's uniform bind group for viewport transforms
                    gpu_ctx.set_bind_group(render_pass, 0, gpu_ctx.uniform_bind_group_id);
                    // Set our vertex buffer
                    gpu_ctx.set_vertex_buffer(render_pass, 0, buffer_id);
                    // Draw!
                    gpu_ctx.draw(render_pass, vertex_count, 1);
                } else {
                    eprintln!("  Fallback to draw_vertices");
                    // Fallback to old method if custom pipeline not available
                    gpu_ctx.draw_vertices(render_pass, buffer_id, vertex_count);
                }
            } else {
                eprintln!("  ERROR: No GPU context available!");
            }
        } else {
            eprintln!("  ERROR: No vertex buffer ID!");
        }
    }
}

// === Configurable Trait Implementation ===

impl Configurable for SelectionPlugin {
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError> {
        // Parse the full plugin.toml structure
        #[derive(Deserialize)]
        struct PluginToml {
            config: PluginConfig,
        }

        #[derive(Deserialize)]
        struct PluginConfig {
            #[serde(default = "default_color")]
            color: u32,
        }

        fn default_color() -> u32 {
            0x4080FF40 // Semi-transparent blue
        }

        match toml::from_str::<PluginToml>(config_data) {
            Ok(plugin_toml) => {
                // Update our config from the parsed values
                self.config.style.color = plugin_toml.config.color;

                eprintln!(
                    "Selection plugin config updated: color={:#010x}",
                    self.config.style.color
                );

                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse selection config: {}", e);
                Err(PluginError::Other(
                    format!("Config parse error: {}", e).into(),
                ))
            }
        }
    }

    fn get_config(&self) -> Option<String> {
        // Convert current config back to TOML
        format!("[config]\ncolor = {:#010x}", self.config.style.color).into()
    }
}

// === Plugin Entry Point (for dynamic loading) ===

/// Create a new selection plugin instance
/// This is the entry point for dynamic library loading
#[no_mangle]
pub extern "C" fn selection_plugin_create() -> Box<dyn Plugin> {
    Box::new(SelectionPlugin::new())
}

// === Public API for direct usage ===

impl SelectionPlugin {
    /// Load configuration from plugin.toml values
    pub fn with_config(mut self, config: SelectionConfig) -> Self {
        self.config = config;
        self
    }

    /// Get current number of selections
    pub fn selection_count(&self) -> usize {
        self.selections.len()
    }
}
