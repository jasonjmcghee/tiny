//! Selection Plugin - Visual highlight for selected text

use ahash::AHasher;
use serde::Deserialize;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::{
    ffi::{BindGroupLayoutId, PipelineId, ShaderModuleId, VertexAttributeDescriptor, VertexFormat},
    CachedBuffer, Capability, Configurable, Initializable, LayoutRect, Library, PaintContext,
    Paintable, Plugin, PluginError, SetupContext, ViewPos, ViewportInfo,
};

/// Single selection with start and end positions
#[derive(Debug, Clone, PartialEq)]
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
    vertex_buffer: Option<CachedBuffer>,
    custom_pipeline_id: Option<PipelineId>,
}

impl SelectionPlugin {
    /// Create a new selection plugin with default configuration
    pub fn new() -> Self {
        use tiny_sdk::{LayoutPos, LogicalSize, PhysicalSize};

        Self {
            config: SelectionConfig::default(),
            selections: Vec::new(),
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
            custom_pipeline_id: None,
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
                    Selection {
                        start: end,
                        end: start,
                    }
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
    fn selection_to_bounding_rect(
        &self,
        selection: &Selection,
        line_height: f32,
        widget_viewport: Option<&tiny_sdk::types::WidgetViewport>,
    ) -> Option<LayoutRect> {
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
                line_height,
            ))
        } else {
            // Multi-line selection: use full width of the editor widget
            // The shader will handle per-line clipping based on selection data
            let widget_vp = widget_viewport.expect("Selection plugin requires widget_viewport");
            let left = widget_vp.bounds.x.0;
            let width = widget_vp.bounds.width.0;
            let height = (end_y + line_height) - start_y;

            Some(LayoutRect::new(left, start_y, width, height))
        }
    }

    /// Create vertex data for given selections
    fn create_vertices_for_selections(
        &self,
        viewport: &tiny_sdk::ViewportInfo,
        widget_viewport: Option<&tiny_sdk::types::WidgetViewport>,
        selections: &[Selection],
    ) -> Vec<SelectionVertex> {
        let mut vertices = Vec::new();
        // Use viewport's scale factor and line_height, not our stored ones (which might be wrong)
        let scale = viewport.scale_factor;
        let line_height = viewport.line_height;
        let color = self.config.style.color;

        for selection in selections {
            if let Some(rect) =
                self.selection_to_bounding_rect(selection, line_height, widget_viewport)
            {
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
                let line_height_scaled = line_height * scale;

                // Margins in screen coordinates (matching transformed selections)
                // widget_viewport is required for correct coordinate system
                let widget_vp = widget_viewport.expect("Selection plugin requires widget_viewport");
                let margin_left = widget_vp.bounds.x.0 * scale;
                let margin_right = (widget_vp.bounds.x.0 + widget_vp.bounds.width.0) * scale;

                let selection_data = SelectionData {
                    start_pos: [start_x, start_y],
                    end_pos: [end_x, end_y],
                    line_height: line_height_scaled,
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

tiny_sdk::plugin! {
    SelectionPlugin {
        name: "selection",
        version: "0.1.0",
        z_index: -10,
        traits: [Init, Paint, Library, Config],
        defaults: [],  // All custom implementations
    }
}

// === Initializable Trait Implementation ===

impl Initializable for SelectionPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        // Create vertex buffer with caching built-in
        // Estimate: avg 2 selections * 3 rects each * 6 vertices per rect
        let vertex_size = std::mem::size_of::<SelectionVertex>();
        let buffer_size = (vertex_size * 36) as u64;
        self.vertex_buffer = Some(CachedBuffer::new(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        ));

        // Create pipeline
        let shader_source = include_str!("shader.wgsl");
        let shader = ShaderModuleId::create_from_wgsl(shader_source);
        let pipeline_layout = BindGroupLayoutId::create_uniform();
        self.custom_pipeline_id = Some(PipelineId::create_with_layout(
            shader,
            shader,
            pipeline_layout,
            44, // stride: position (8) + color (4) + selection_data (32)
            &[
                VertexAttributeDescriptor {
                    offset: 0,
                    location: 0,
                    format: VertexFormat::Float32x2,
                }, // position
                VertexAttributeDescriptor {
                    offset: 8,
                    location: 1,
                    format: VertexFormat::Uint32,
                }, // color
                VertexAttributeDescriptor {
                    offset: 12,
                    location: 2,
                    format: VertexFormat::Float32x2,
                }, // start_pos
                VertexAttributeDescriptor {
                    offset: 20,
                    location: 3,
                    format: VertexFormat::Float32x2,
                }, // end_pos
                VertexAttributeDescriptor {
                    offset: 28,
                    location: 4,
                    format: VertexFormat::Float32,
                }, // line_height
                VertexAttributeDescriptor {
                    offset: 32,
                    location: 5,
                    format: VertexFormat::Float32,
                }, // margin_left
                VertexAttributeDescriptor {
                    offset: 36,
                    location: 6,
                    format: VertexFormat::Float32,
                }, // margin_right
            ],
        ));

        eprintln!("Selection plugin setup complete");

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
                let count_bytes: [u8; 4] = args[0..4]
                    .try_into()
                    .map_err(|_| PluginError::Other("Invalid count".into()))?;
                let selection_count = u32::from_le_bytes(count_bytes) as usize;

                let view_pos_size = std::mem::size_of::<ViewPos>();
                let expected_size = 4 + (selection_count * 2 * view_pos_size);
                if args.len() < expected_size {
                    return Err(PluginError::Other(
                        "Invalid args: incomplete selection data".into(),
                    ));
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
                let viewport_info: &ViewportInfo =
                    bytemuck::from_bytes(&args[0..viewport_info_size]);
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
        // Transform selections to screen coordinates if we have widget viewport
        let transformed_selections = if let Some(ref widget_viewport) = ctx.widget_viewport {
            let offset_x = widget_viewport.bounds.x.0;
            let offset_y = widget_viewport.bounds.y.0;

            self.selections
                .iter()
                .map(|sel| Selection {
                    start: ViewPos::new(sel.start.x.0 + offset_x, sel.start.y.0 + offset_y),
                    end: ViewPos::new(sel.end.x.0 + offset_x, sel.end.y.0 + offset_y),
                })
                .collect()
        } else {
            self.selections.clone()
        };

        // Create cache key from all relevant state
        let mut hasher = AHasher::default();
        tiny_sdk::paint_cache::hash_viewport_base(&mut hasher, &ctx.viewport);
        tiny_sdk::paint_cache::hash_widget_viewport(&mut hasher, &ctx.widget_viewport);
        for sel in &transformed_selections {
            sel.start.x.0.to_bits().hash(&mut hasher);
            sel.start.y.0.to_bits().hash(&mut hasher);
            sel.end.x.0.to_bits().hash(&mut hasher);
            sel.end.y.0.to_bits().hash(&mut hasher);
        }
        self.config.style.color.hash(&mut hasher);
        let cache_key = hasher.finish();

        // Write vertices only if state changed - simplified to one line!
        if let Some(ref vertex_buffer) = self.vertex_buffer {
            let vertices = self.create_vertices_for_selections(
                &ctx.viewport,
                ctx.widget_viewport.as_ref(),
                &transformed_selections,
            );
            if vertices.is_empty() {
                return;
            }
            vertex_buffer.write_if_changed(bytemuck::cast_slice(&vertices), &cache_key);

            // Always draw
            let vertex_count = vertices.len() as u32;

            if let Some(ref gpu_ctx) = ctx.gpu_context {
                if let Some(pipeline_id) = self.custom_pipeline_id {
                    // Use our custom pipeline
                    gpu_ctx.set_pipeline(render_pass, pipeline_id);
                    // Use the host's uniform bind group for viewport transforms
                    gpu_ctx.set_bind_group(render_pass, 0, gpu_ctx.uniform_bind_group_id);
                    // Set our vertex buffer
                    gpu_ctx.set_vertex_buffer(render_pass, 0, vertex_buffer.buffer_id());
                    // Draw!
                    gpu_ctx.draw(render_pass, vertex_count, 1);
                } else {
                    eprintln!("Selection pipeline not available");
                }
            } else {
                eprintln!("No GPU context available");
            }
        }
    }
}

// === Configurable Trait Implementation ===

impl Configurable for SelectionPlugin {
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError> {
        #[derive(Default, Deserialize)]
        struct PluginConfig {
            #[serde(default = "default_color")]
            color: u32,
        }

        fn default_color() -> u32 {
            0x4080FF40 // Semi-transparent blue
        }

        // Parse TOML value first (handles syntax errors gracefully)
        let toml_value: toml::Value = match toml::from_str(config_data) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("❌ TOML syntax error in selection plugin.toml: {}", e);
                eprintln!("   Keeping previous configuration");
                return Ok(()); // Don't fail, just keep current config
            }
        };

        // Extract [config] section and parse fields individually
        if let Some(config_table) = toml_value.get("config").and_then(|v| v.as_table()) {
            let mut temp_config = PluginConfig::default();
            tiny_sdk::parse_fields!(temp_config, config_table, {
                color: default_color(),
            });

            // Apply parsed values
            self.config.style.color = temp_config.color;

            eprintln!(
                "Selection plugin config updated: color={:#010x}",
                self.config.style.color
            );
        }

        Ok(())
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
