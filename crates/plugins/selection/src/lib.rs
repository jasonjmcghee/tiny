//! Selection Plugin - Visual highlight for selected text

use serde::Deserialize;
use std::sync::Arc;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::Buffer;
use tiny_sdk::{
    ffi::{BufferId, PipelineId, ShaderModuleId},
    Capability, Configurable, Initializable, LayoutRect, Library, PaintContext, Paintable, Plugin,
    PluginError, SetupContext,
};

/// Document position
#[derive(Debug, Clone, Copy)]
pub struct DocPos {
    pub line: u32,
    pub column: u32,
}

/// Single selection with start and end positions
#[derive(Debug, Clone)]
pub struct Selection {
    /// Start position (inclusive)
    pub start: DocPos,
    /// End position (exclusive)
    pub end: DocPos,
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

    // Viewport info needed for rectangle calculation
    line_height: f32,
    viewport_width: f32,
    margin_x: f32,
    margin_y: f32,
    scale_factor: f32,
    scroll_x: f32,
    scroll_y: f32,

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
        Self {
            config: SelectionConfig::default(),
            selections: Vec::new(),
            line_height: 19.6, // Default
            viewport_width: 800.0, // Default
            margin_x: 60.0, // Default
            margin_y: 10.0, // Default
            scale_factor: 1.0, // Default
            scroll_x: 0.0, // Default
            scroll_y: 0.0, // Default
            vertex_buffer: None,
            vertex_buffer_id: None,
            custom_pipeline_id: None,
            device: None,
            queue: None,
        }
    }

    /// Update selections from start/end positions
    pub fn set_selections(&mut self, selections: Vec<(DocPos, DocPos)>) {
        self.selections = selections
            .into_iter()
            .map(|(start, end)| {
                Selection { start, end }
            })
            .collect();
    }

    /// Update viewport information (including scroll offset)
    pub fn set_viewport_info(&mut self, line_height: f32, viewport_width: f32, margin_x: f32, margin_y: f32, scale_factor: f32, scroll_x: f32, scroll_y: f32, global_margin_x: f32, global_margin_y: f32) {
        // Check if scale factor seems wrong (e.g., 1.0 on a retina display)
        if scale_factor == 1.0 {
            eprintln!("WARNING: Scale factor is 1.0, might be incorrect for high-DPI display!");
        }

        self.line_height = line_height;
        self.viewport_width = viewport_width;
        self.margin_x = margin_x;
        // Add global margin to the document margin for positioning
        self.margin_y = margin_y + global_margin_y;
        self.scale_factor = scale_factor;
        self.scroll_x = scroll_x;
        self.scroll_y = scroll_y;
    }

    /// Convert DocPos to view position (accounting for scroll)
    fn doc_to_view(&self, pos: DocPos) -> (f32, f32) {
        let x = self.margin_x + (pos.column as f32 * 8.4) - self.scroll_x; // Account for horizontal scroll
        let y = self.margin_y + (pos.line as f32 * self.line_height) - self.scroll_y;
        (x, y)
    }

    /// Generate rectangles for a selection (matching original implementation)
    fn selection_to_rectangles(&self, selection: &Selection) -> Vec<LayoutRect> {
        let mut rects = Vec::new();

        // Skip if it's just a cursor (no selection)
        if selection.start.line == selection.end.line && selection.start.column == selection.end.column {
            return rects;
        }

        // Add dummy rectangle for GPU bug workaround (from original)
        rects.push(LayoutRect::new(0.0, 0.0, 0.0, 0.0));

        if selection.start.line == selection.end.line {
            // Single line selection
            let (start_x, start_y) = self.doc_to_view(selection.start);
            let (end_x, _) = self.doc_to_view(selection.end);

            rects.push(LayoutRect::new(
                start_x - 2.0,
                start_y,
                end_x - start_x,
                self.line_height,
            ));
        } else {
            // Multi-line selection
            let (start_x, start_y) = self.doc_to_view(selection.start);
            let (end_x, end_y) = self.doc_to_view(selection.end);
            let viewport_right = self.viewport_width - self.margin_x;

            // First line - extends to right edge
            rects.push(LayoutRect::new(
                start_x - 2.0,
                start_y,
                (viewport_right - start_x).max(0.0) + 2.0,
                self.line_height,
            ));

            // Middle lines - full width
            if selection.end.line > selection.start.line + 1 {
                rects.push(LayoutRect::new(
                    self.margin_x - self.scroll_x,  // Account for horizontal scroll
                    start_y + self.line_height,
                    self.viewport_width - (self.margin_x * 2.0),
                    (selection.end.line - selection.start.line - 1) as f32 * self.line_height,
                ));
            }

            // Last line - from left margin to end position
            rects.push(LayoutRect::new(
                self.margin_x - self.scroll_x,  // Account for horizontal scroll
                end_y,
                (end_x - self.margin_x + self.scroll_x).max(0.0) - 2.0,
                self.line_height,
            ));
        }

        rects
    }

    /// Create vertex data for all selection rectangles
    fn create_vertices(&self, viewport: &tiny_sdk::ViewportInfo) -> Vec<SelectionVertex> {
        let mut vertices = Vec::new();
        // Use viewport's scale factor, not our stored one (which might be wrong)
        let scale = viewport.scale_factor;
        let color = self.config.style.color;

        for selection in &self.selections {
            let rects = self.selection_to_rectangles(selection);

            for rect in &rects {
                // Rectangle is in logical view space, scale to physical pixels
                let x = rect.x.0 * scale;
                let y = rect.y.0 * scale;
                let w = rect.width.0 * scale;
                let h = rect.height.0 * scale;

                // Create two triangles for a quad
                vertices.extend_from_slice(&[
                    SelectionVertex {
                        position: [x, y],
                        color,
                    },
                    SelectionVertex {
                        position: [x + w, y],
                        color,
                    },
                    SelectionVertex {
                        position: [x, y + h],
                        color,
                    },
                    SelectionVertex {
                        position: [x + w, y],
                        color,
                    },
                    SelectionVertex {
                        position: [x + w, y + h],
                        color,
                    },
                    SelectionVertex {
                        position: [x, y + h],
                        color,
                    },
                ]);
            }
        }

        vertices
    }
}

/// Vertex data for selection rendering
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct SelectionVertex {
    position: [f32; 2],
    color: u32,
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
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
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

    // Unpack color from u32 to vec4
    let r = f32((input.color >> 24u) & 0xFFu) / 255.0;
    let g = f32((input.color >> 16u) & 0xFFu) / 255.0;
    let b = f32((input.color >> 8u) & 0xFFu) / 255.0;
    let a = f32(input.color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    return out;
}

// Fragment shader
@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.color;
}
"#;

        // Create shader modules
        let shader_id = ShaderModuleId::create_from_wgsl(shader_source);

        // Create pipeline with the same shader for vertex and fragment
        let pipeline_id = PipelineId::create_simple(shader_id, shader_id);
        self.custom_pipeline_id = Some(pipeline_id);

        eprintln!("Selection plugin created custom pipeline");

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
                // Format: count (u32), then for each selection:
                //   start_line, start_column, end_line, end_column (u32 each)

                if args.len() < 4 {
                    return Err(PluginError::Other("Invalid args: too short".into()));
                }

                let mut offset = 0;
                let selection_count = u32::from_le_bytes([
                    args[offset], args[offset + 1], args[offset + 2], args[offset + 3]
                ]) as usize;
                offset += 4;

                let mut selections = Vec::with_capacity(selection_count);

                for i in 0..selection_count {
                    if offset + 16 > args.len() {
                        return Err(PluginError::Other("Invalid args: incomplete selection data".into()));
                    }

                    let start_line = u32::from_le_bytes([
                        args[offset], args[offset + 1], args[offset + 2], args[offset + 3]
                    ]);
                    let start_column = u32::from_le_bytes([
                        args[offset + 4], args[offset + 5], args[offset + 6], args[offset + 7]
                    ]);
                    let end_line = u32::from_le_bytes([
                        args[offset + 8], args[offset + 9], args[offset + 10], args[offset + 11]
                    ]);
                    let end_column = u32::from_le_bytes([
                        args[offset + 12], args[offset + 13], args[offset + 14], args[offset + 15]
                    ]);

                    offset += 16;

                    selections.push((
                        DocPos { line: start_line, column: start_column },
                        DocPos { line: end_line, column: end_column }
                    ));
                }

                // Update our selections
                self.set_selections(selections);
                Ok(Vec::new())
            }
            "set_viewport_info" => {
                // Format: line_height, viewport_width, margin_x, margin_y, scale_factor, scroll_x, scroll_y, global_margin_x, global_margin_y (f32 each)
                if args.len() < 36 {
                    // Support both old (28 bytes) and new (36 bytes) formats
                    if args.len() < 28 {
                        return Err(PluginError::Other("Invalid viewport args".into()));
                    }
                    // Old format - no global margin
                    let line_height = f32::from_le_bytes([args[0], args[1], args[2], args[3]]);
                    let viewport_width = f32::from_le_bytes([args[4], args[5], args[6], args[7]]);
                    let margin_x = f32::from_le_bytes([args[8], args[9], args[10], args[11]]);
                    let margin_y = f32::from_le_bytes([args[12], args[13], args[14], args[15]]);
                    let scale_factor = f32::from_le_bytes([args[16], args[17], args[18], args[19]]);
                    let scroll_x = f32::from_le_bytes([args[20], args[21], args[22], args[23]]);
                    let scroll_y = f32::from_le_bytes([args[24], args[25], args[26], args[27]]);

                    self.set_viewport_info(line_height, viewport_width, margin_x, margin_y, scale_factor, scroll_x, scroll_y, 0.0, 0.0);
                } else {
                    // New format - with global margin
                    let line_height = f32::from_le_bytes([args[0], args[1], args[2], args[3]]);
                    let viewport_width = f32::from_le_bytes([args[4], args[5], args[6], args[7]]);
                    let margin_x = f32::from_le_bytes([args[8], args[9], args[10], args[11]]);
                    let margin_y = f32::from_le_bytes([args[12], args[13], args[14], args[15]]);
                    let scale_factor = f32::from_le_bytes([args[16], args[17], args[18], args[19]]);
                    let scroll_x = f32::from_le_bytes([args[20], args[21], args[22], args[23]]);
                    let scroll_y = f32::from_le_bytes([args[24], args[25], args[26], args[27]]);
                    let global_margin_x = f32::from_le_bytes([args[28], args[29], args[30], args[31]]);
                    let global_margin_y = f32::from_le_bytes([args[32], args[33], args[34], args[35]]);

                    self.set_viewport_info(line_height, viewport_width, margin_x, margin_y, scale_factor, scroll_x, scroll_y, global_margin_x, global_margin_y);
                }
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