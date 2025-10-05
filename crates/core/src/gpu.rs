//! GPU rendering implementation using wgpu
//!
//! Provides GPU resources and methods for widget rendering

use crate::gpu_ffi;
use ahash::HashMap;
use bytemuck::{Pod, Zeroable};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_sdk::{types::{RectInstance, RoundedRectInstance}, GlyphInstance, PhysicalSize};
use wgpu::{
    naga, AddressMode, Backends, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, BlendState,
    Buffer, BufferAddress, BufferBindingType, BufferDescriptor, BufferUsages, Color,
    ColorTargetState, ColorWrites, CommandEncoderDescriptor, Device, DeviceDescriptor, Extent3d,
    Features, FilterMode, FragmentState, FrontFace, Instance, InstanceDescriptor, Limits, LoadOp,
    MultisampleState, Operations, Origin3d, PipelineCompilationOptions, PipelineLayoutDescriptor,
    PollType, PolygonMode, PowerPreference, PrimitiveState, PrimitiveTopology, Queue, RenderPass,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor,
    RequestAdapterOptions, Sampler, SamplerBindingType, SamplerDescriptor, ShaderModule,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, Surface, SurfaceConfiguration,
    SurfaceTarget, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureView, TextureViewDescriptor, TextureViewDimension, VertexAttribute, VertexBufferLayout,
    VertexFormat, VertexState, VertexStepMode,
};

// Constants
const ATLAS_SIZE: f32 = 2048.0;
const RECT_BUFFER_SIZE: u64 = 65536; // 64KB
const GLYPH_BUFFER_SIZE: u64 = 4 * 1024 * 1024; // 4MB

/// Vertex data for rectangles (unit quad)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RectVertex {
    pub position: [f32; 2],
}

/// Instance data for rectangles (per-rect data)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RectInstanceData {
    pub rect_pos: [f32; 2],
    pub rect_size: [f32; 2],
    pub color: u32,
    pub _padding: u32, // Align to 16 bytes
}

/// Instance data for rounded rectangles with borders
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RoundedRectInstanceData {
    pub rect_pos: [f32; 2],
    pub rect_size: [f32; 2],
    pub color: u32,
    pub border_color: u32,
    pub corner_radius: f32,
    pub border_width: f32,
}

/// Vertex data for glyphs
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct GlyphVertex {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
    pub token_id: u32,
    pub relative_pos: f32,
    pub format: u32,
}

/// Uniform data for shaders
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Uniforms {
    pub viewport_size: [f32; 2],
    pub scale_factor: f32,
    pub time: f32,
    pub theme_mode: u32,
    pub _padding: [f32; 3],
}

/// Basic uniforms for simple shaders
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct BasicUniforms {
    pub viewport_size: [f32; 2],
}

/// GPU renderer that executes batched draw commands
pub struct GpuRenderer {
    device: Arc<Device>,
    queue: Arc<Queue>,
    surface: Surface<'static>,
    config: SurfaceConfiguration,

    // Shader paths for hot-reloading
    shader_base_path: PathBuf,

    // Cached bind group layouts (these don't change when shaders reload)
    glyph_bind_group_layout: BindGroupLayout,
    rect_uniform_bind_group_layout: BindGroupLayout, // Rect-specific uniforms
    themed_uniform_bind_group_layout: Option<BindGroupLayout>, // Themed shader uniforms
    theme_bind_group_layout: Option<BindGroupLayout>, // Theme texture/sampler
    style_bind_group_layout: Option<BindGroupLayout>, // Style buffer layout (cached)

    // Pipelines
    rect_pipeline: RenderPipeline,
    rounded_rect_pipeline: RenderPipeline,
    glyph_pipeline: RenderPipeline,

    // Text effect shader pipelines
    effect_pipelines: HashMap<u32, RenderPipeline>,
    effect_uniform_buffers: HashMap<u32, Buffer>,
    effect_bind_groups: HashMap<u32, BindGroup>,

    style_buffer: Option<Buffer>,
    palette_texture_view: Option<TextureView>,
    palette_sampler: Option<Sampler>,
    styled_bind_group: Option<BindGroup>,

    // Themed glyph pipeline (uses token IDs + theme texture)
    themed_glyph_pipeline: Option<RenderPipeline>,
    theme_texture: Option<Texture>,
    theme_texture_view: Option<TextureView>,
    theme_sampler: Option<Sampler>,
    theme_bind_group: Option<BindGroup>,
    // TODO: make methods
    pub current_time: f32,
    pub current_theme_mode: u32,

    // Uniform buffers for different shader types
    uniform_buffer: Buffer, // Legacy/compatibility
    uniform_bind_group: BindGroup,
    rect_uniform_buffer: Buffer,
    rect_uniform_bind_group: BindGroup,
    themed_uniform_buffer: Option<Buffer>,
    themed_uniform_bind_group: Option<BindGroup>,

    // Glyph atlas texture
    glyph_texture: Texture,
    glyph_bind_group: BindGroup,

    // Vertex buffers
    rect_vertex_buffer: Buffer,   // Unit quad (6 vertices, static)
    rect_instance_buffer: Buffer, // Per-rect data (dynamic)
    rounded_rect_instance_buffer: Buffer, // Per-rounded-rect data (dynamic)
    glyph_vertex_buffer: Buffer,
    line_number_vertex_buffer: Buffer,
    tab_bar_vertex_buffer: Buffer,
    file_picker_vertex_buffer: Buffer,
    grep_vertex_buffer: Buffer,

    // Store registered IDs for plugin context
    rect_pipeline_id: gpu_ffi::PipelineId,
    uniform_bind_group_id: gpu_ffi::BindGroupId,

    // Dirty flags to avoid redundant updates
    uniforms_dirty: bool,
    themed_uniforms_dirty: bool,

    // Vertex cache: (buffer_ptr, offset) -> (instances_hash, cached_vertices)
    vertex_cache: HashMap<(usize, u64), (u64, Vec<GlyphVertex>)>,

    // Style buffer cache to avoid redundant writes (0 = uninitialized)
    last_style_hash: std::sync::atomic::AtomicU64,

    // Rect instance cache to avoid redundant writes (0 = uninitialized)
    last_rect_instances_hash: std::sync::atomic::AtomicU64,
}

/// Create 6 vertices (2 triangles) for a quad
fn quad_vertices<V, F>(x: f32, y: f32, w: f32, h: f32, make_vertex: F) -> [V; 6]
where
    F: Fn([f32; 2], bool) -> V,
    V: Copy,
{
    let (x1, y1, x2, y2) = (x, y, x + w, y + h);
    let tl = make_vertex([x1, y1], false);
    let tr = make_vertex([x2, y1], true);
    let bl = make_vertex([x1, y2], false);
    let br = make_vertex([x2, y2], true);
    [tl, tr, bl, tr, br, bl]
}

/// Create a unit quad (0,0 to 1,1) for instanced rendering
pub fn create_unit_quad() -> [RectVertex; 6] {
    [
        RectVertex {
            position: [0.0, 0.0],
        }, // TL
        RectVertex {
            position: [1.0, 0.0],
        }, // TR
        RectVertex {
            position: [0.0, 1.0],
        }, // BL
        RectVertex {
            position: [1.0, 0.0],
        }, // TR
        RectVertex {
            position: [1.0, 1.0],
        }, // BR
        RectVertex {
            position: [0.0, 1.0],
        }, // BL
    ]
}

fn create_glyph_vertices(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tex_coords: [f32; 4],
    token_id: u32,
    relative_pos: f32,
    format: u32,
) -> [GlyphVertex; 6] {
    let [u0, v0, u1, v1] = tex_coords;
    quad_vertices(x, y, w, h, |[px, py], is_right| {
        let u = if is_right { u1 } else { u0 };
        let v = if py > y { v1 } else { v0 };
        GlyphVertex {
            position: [px, py],
            tex_coord: [u, v],
            token_id,
            relative_pos,
            format,
        }
    })
}

fn vertex_attr(offset: u64, location: u32, format: VertexFormat) -> VertexAttribute {
    VertexAttribute {
        offset,
        shader_location: location,
        format,
    }
}

fn glyph_vertex_attributes() -> [VertexAttribute; 5] {
    [
        vertex_attr(0, 0, VertexFormat::Float32x2),
        vertex_attr(8, 1, VertexFormat::Float32x2),
        vertex_attr(16, 2, VertexFormat::Uint32),
        vertex_attr(20, 3, VertexFormat::Float32),
        vertex_attr(24, 4, VertexFormat::Uint32),
    ]
}

fn rect_vertex_attributes() -> [VertexAttribute; 1] {
    [
        vertex_attr(0, 0, VertexFormat::Float32x2), // vertex_pos
    ]
}

fn rect_instance_attributes() -> [VertexAttribute; 3] {
    [
        vertex_attr(0, 1, VertexFormat::Float32x2), // rect_pos
        vertex_attr(8, 2, VertexFormat::Float32x2), // rect_size
        vertex_attr(16, 3, VertexFormat::Uint32),   // color
    ]
}

fn rounded_rect_instance_attributes() -> [VertexAttribute; 6] {
    [
        vertex_attr(0, 1, VertexFormat::Float32x2), // rect_pos
        vertex_attr(8, 2, VertexFormat::Float32x2), // rect_size
        vertex_attr(16, 3, VertexFormat::Uint32),   // color
        vertex_attr(20, 4, VertexFormat::Uint32),   // border_color
        vertex_attr(24, 5, VertexFormat::Float32),  // corner_radius
        vertex_attr(28, 6, VertexFormat::Float32),  // border_width
    ]
}

// Helper struct for creating pipelines
struct PipelineBuilder<'a> {
    device: &'a Device,
    format: TextureFormat,
}

impl<'a> PipelineBuilder<'a> {
    fn create_pipeline(
        &self,
        label: &str,
        shader: &ShaderModule,
        bind_group_layouts: &[&BindGroupLayout],
        vertex_attributes: &[VertexAttribute],
        vertex_stride: BufferAddress,
    ) -> RenderPipeline {
        let layout = self
            .device
            .create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some(&format!("{} Layout", label)),
                bind_group_layouts,
                push_constant_ranges: &[],
            });

        self.device
            .create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[VertexBufferLayout {
                        array_stride: vertex_stride,
                        step_mode: VertexStepMode::Vertex,
                        attributes: vertex_attributes,
                    }],
                    compilation_options: PipelineCompilationOptions::default(),
                },
                fragment: Some(FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(ColorTargetState {
                        format: self.format,
                        blend: Some(BlendState::ALPHA_BLENDING),
                        write_mask: ColorWrites::ALL,
                    })],
                    compilation_options: PipelineCompilationOptions::default(),
                }),
                primitive: PrimitiveState {
                    topology: PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: MultisampleState::default(),
                multiview: None,
                cache: None,
            })
    }

    fn create_instanced_pipeline(
        &self,
        label: &str,
        shader: &ShaderModule,
        bind_group_layouts: &[&BindGroupLayout],
        vertex_attributes: &[VertexAttribute],
        vertex_stride: BufferAddress,
        instance_attributes: &[VertexAttribute],
        instance_stride: BufferAddress,
    ) -> RenderPipeline {
        let layout = self
            .device
            .create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some(&format!("{} Layout", label)),
                bind_group_layouts,
                push_constant_ranges: &[],
            });

        self.device
            .create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[
                        VertexBufferLayout {
                            array_stride: vertex_stride,
                            step_mode: VertexStepMode::Vertex,
                            attributes: vertex_attributes,
                        },
                        VertexBufferLayout {
                            array_stride: instance_stride,
                            step_mode: VertexStepMode::Instance,
                            attributes: instance_attributes,
                        },
                    ],
                    compilation_options: PipelineCompilationOptions::default(),
                },
                fragment: Some(FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(ColorTargetState {
                        format: self.format,
                        blend: Some(BlendState::ALPHA_BLENDING),
                        write_mask: ColorWrites::ALL,
                    })],
                    compilation_options: PipelineCompilationOptions::default(),
                }),
                primitive: PrimitiveState {
                    topology: PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: MultisampleState::default(),
                multiview: None,
                cache: None,
            })
    }
}

/// Helper function to create uniform bind group layout
fn create_uniform_bind_group_layout(
    device: &Device,
    label: &str,
    visibility: ShaderStages,
) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Helper function to create texture bind group layout
fn create_texture_bind_group_layout(
    device: &Device,
    label: &str,
    view_dimension: TextureViewDimension,
) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: true },
                    view_dimension,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Sampler(SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

impl GpuRenderer {
    /// Helper to create a buffer
    fn create_buffer(&self, label: &str, size: u64, usage: BufferUsages) -> Buffer {
        self.device.create_buffer(&BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        })
    }
    /// Load shader from file system
    fn load_shader_from_file(path: &Path) -> std::io::Result<String> {
        fs::read_to_string(path)
    }

    /// Load shader with fallback to embedded source
    fn load_shader(base_path: &Path, shader_name: &str, fallback: &str) -> String {
        let shader_path = base_path.join(format!("crates/core/src/shaders/{}", shader_name));
        match Self::load_shader_from_file(&shader_path) {
            Ok(source) => {
                eprintln!("Loaded shader from file: {:?}", shader_path);
                source
            }
            Err(e) => {
                eprintln!(
                    "Failed to load shader from {:?}: {}. Using embedded version.",
                    shader_path, e
                );
                fallback.to_string()
            }
        }
    }

    /// Helper to create pipelines using the builder
    fn create_pipeline(
        &self,
        label: &str,
        shader: &ShaderModule,
        bind_group_layouts: &[&BindGroupLayout],
        vertex_attributes: &[VertexAttribute],
        vertex_stride: BufferAddress,
    ) -> RenderPipeline {
        let builder = PipelineBuilder {
            device: &self.device,
            format: self.config.format,
        };
        builder.create_pipeline(
            label,
            shader,
            bind_group_layouts,
            vertex_attributes,
            vertex_stride,
        )
    }

    /// Create rect pipeline with given shader module (instanced rendering)
    fn create_rect_pipeline(&self, shader: &ShaderModule) -> RenderPipeline {
        let builder = PipelineBuilder {
            device: &self.device,
            format: self.config.format,
        };
        builder.create_instanced_pipeline(
            "Rect Pipeline (Instanced)",
            shader,
            &[&self.rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
            &rect_instance_attributes(),
            std::mem::size_of::<RectInstanceData>() as BufferAddress,
        )
    }

    /// Create rounded rect pipeline with given shader module (instanced rendering)
    fn create_rounded_rect_pipeline(&self, shader: &ShaderModule) -> RenderPipeline {
        let builder = PipelineBuilder {
            device: &self.device,
            format: self.config.format,
        };
        builder.create_instanced_pipeline(
            "Rounded Rect Pipeline (Instanced)",
            shader,
            &[&self.rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
            &rounded_rect_instance_attributes(),
            std::mem::size_of::<RoundedRectInstanceData>() as BufferAddress,
        )
    }

    /// Create glyph pipeline with given shader module
    fn create_glyph_pipeline(&self, shader: &ShaderModule) -> RenderPipeline {
        self.create_pipeline(
            "Glyph Pipeline",
            shader,
            &[
                &self.rect_uniform_bind_group_layout,
                &self.glyph_bind_group_layout,
            ],
            &glyph_vertex_attributes(),
            std::mem::size_of::<GlyphVertex>() as BufferAddress,
        )
    }

    /// Get device for custom widget rendering
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Get queue for custom widget rendering
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Get device Arc for custom widget rendering
    pub fn device_arc(&self) -> std::sync::Arc<Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Get queue Arc for custom widget rendering
    pub fn queue_arc(&self) -> std::sync::Arc<Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    /// Get uniform bind group for viewport transforms
    pub fn uniform_bind_group(&self) -> &BindGroup {
        &self.uniform_bind_group
    }

    /// Get surface for custom rendering
    pub fn surface(&mut self) -> &mut Surface<'static> {
        &mut self.surface
    }

    /// Get uniform buffer for custom rendering
    pub fn uniform_buffer(&self) -> &Buffer {
        &self.uniform_buffer
    }

    /// Get rect pipeline for widget backgrounds
    pub fn rect_pipeline(&self) -> &RenderPipeline {
        &self.rect_pipeline
    }

    /// Get rect vertex buffer for widget backgrounds
    pub fn rect_vertex_buffer(&self) -> &Buffer {
        &self.rect_vertex_buffer
    }

    /// Get FFI context for plugins (with IDs for rect pipeline and bind group)
    pub fn get_plugin_context(&self) -> gpu_ffi::PluginGpuContext {
        gpu_ffi::PluginGpuContext {
            rect_pipeline_id: self.rect_pipeline_id,
            uniform_bind_group_id: self.uniform_bind_group_id,
            render_pass: std::ptr::null_mut(),
        }
    }

    /// Test method to write to rect buffer directly
    pub fn test_write_rect_buffer(&self, data: &[u8]) {
        self.queue.write_buffer(&self.rect_vertex_buffer, 0, data);
    }

    /// Draw vertices directly for plugins (avoids passing Buffer objects)
    pub fn draw_plugin_vertices(
        &self,
        render_pass: &mut RenderPass,
        vertex_data: &[u8],
        vertex_count: u32,
    ) {
        self.queue
            .write_buffer(&self.rect_vertex_buffer, 0, vertex_data);

        // Set up pipeline
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        render_pass.draw(0..vertex_count, 0..1);
    }

    /// FFI-safe extern function for drawing vertices
    pub extern "C" fn draw_rect_vertices_extern(
        gpu_renderer: &GpuRenderer,
        pass: *mut RenderPass,
        vertices: *const u8,
        vertices_len: usize,
        count: u32,
    ) {
        unsafe {
            let pass = &mut *pass;
            let vertex_data = std::slice::from_raw_parts(vertices, vertices_len);
            gpu_renderer.draw_plugin_vertices(pass, vertex_data, count);
        }
    }

    /// Get effect uniform buffer for updating shader parameters
    pub fn effect_uniform_buffer(&self, shader_id: u32) -> Option<&Buffer> {
        self.effect_uniform_buffers.get(&shader_id)
    }

    /// Register a text effect shader with the GPU renderer
    pub fn register_text_effect_shader(
        &mut self,
        shader_id: u32,
        shader_source: &str,
        uniform_size: u64,
    ) {
        let shader = self.device.create_shader_module(ShaderModuleDescriptor {
            label: Some(&format!("Text Effect Shader {}", shader_id)),
            source: ShaderSource::Wgsl(shader_source.into()),
        });

        let effect_uniform_buffer = self.create_buffer(
            &format!("Text Effect Uniform Buffer {}", shader_id),
            uniform_size,
            BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        );

        let viewport_layout = create_uniform_bind_group_layout(
            &self.device,
            "Text Effect Viewport Layout",
            ShaderStages::VERTEX,
        );
        let glyph_layout = create_texture_bind_group_layout(
            &self.device,
            "Text Effect Glyph Layout",
            TextureViewDimension::D2,
        );
        let effect_layout = create_uniform_bind_group_layout(
            &self.device,
            "Text Effect Uniform Layout",
            ShaderStages::FRAGMENT,
        );

        let effect_bind_group = self.device.create_bind_group(&BindGroupDescriptor {
            label: Some(&format!("Text Effect Bind Group {}", shader_id)),
            layout: &effect_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: effect_uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline = self.create_pipeline(
            &format!("Text Effect Pipeline {}", shader_id),
            &shader,
            &[&viewport_layout, &glyph_layout, &effect_layout],
            &glyph_vertex_attributes(),
            std::mem::size_of::<GlyphVertex>() as BufferAddress,
        );

        self.effect_pipelines.insert(shader_id, pipeline);
        self.effect_uniform_buffers
            .insert(shader_id, effect_uniform_buffer);
        self.effect_bind_groups.insert(shader_id, effect_bind_group);
    }

    /// Upload style buffer as u32 (for shader compatibility)
    pub fn upload_style_buffer_u32(&mut self, style_data: &[u32]) {
        use std::hash::{Hash, Hasher};
        use std::sync::atomic::Ordering;

        // Hash the style data to check if it changed
        let mut hasher = ahash::AHasher::default();
        style_data.hash(&mut hasher);
        let style_hash = hasher.finish();

        // Check if data actually changed
        let cached_hash = self.last_style_hash.load(Ordering::Relaxed);
        let data_changed = cached_hash != style_hash;

        // Buffer size is already aligned since u32 is 4 bytes
        let buffer_size = (style_data.len() * 4) as u64;

        // Track if buffer was recreated
        let buffer_recreated = self
            .style_buffer
            .as_ref()
            .map(|b| b.size() != buffer_size)
            .unwrap_or(true);

        // Create or recreate buffer if size changed
        if buffer_recreated {
            self.style_buffer = Some(self.create_buffer(
                "Style Buffer",
                buffer_size,
                BufferUsages::STORAGE | BufferUsages::COPY_DST,
            ));
        }

        // Only write data if it changed or buffer was recreated
        if data_changed || buffer_recreated {
            if let Some(buffer) = &self.style_buffer {
                self.queue
                    .write_buffer(buffer, 0, bytemuck::cast_slice(style_data));
                self.last_style_hash.store(style_hash, Ordering::Relaxed);
            }
        }

        // Only recreate bind group when buffer was recreated or it doesn't exist
        if buffer_recreated || self.styled_bind_group.is_none() {
            if let (Some(style_buffer), Some(palette_view), Some(palette_sampler)) = (
                &self.style_buffer,
                &self.palette_texture_view,
                &self.palette_sampler,
            ) {
                // Create bind group layout if not cached
                if self.style_bind_group_layout.is_none() {
                    let style_bind_group_layout =
                        self.device
                            .create_bind_group_layout(&BindGroupLayoutDescriptor {
                                label: Some("Style Buffer Layout"),
                                entries: &[
                                    BindGroupLayoutEntry {
                                        binding: 0,
                                        visibility: ShaderStages::VERTEX,
                                        ty: BindingType::Buffer {
                                            ty: BufferBindingType::Storage { read_only: true },
                                            has_dynamic_offset: false,
                                            min_binding_size: None,
                                        },
                                        count: None,
                                    },
                                    BindGroupLayoutEntry {
                                        binding: 1,
                                        visibility: ShaderStages::FRAGMENT,
                                        ty: BindingType::Texture {
                                            multisampled: false,
                                            view_dimension: TextureViewDimension::D1,
                                            sample_type: TextureSampleType::Float {
                                                filterable: true,
                                            },
                                        },
                                        count: None,
                                    },
                                    BindGroupLayoutEntry {
                                        binding: 2,
                                        visibility: ShaderStages::FRAGMENT,
                                        ty: BindingType::Sampler(SamplerBindingType::Filtering),
                                        count: None,
                                    },
                                ],
                            });
                    self.style_bind_group_layout = Some(style_bind_group_layout);
                }

                // Create bind group using cached layout
                if let Some(layout) = &self.style_bind_group_layout {
                    self.styled_bind_group =
                        Some(self.device.create_bind_group(&BindGroupDescriptor {
                            label: Some("Style Bind Group"),
                            layout,
                            entries: &[
                                BindGroupEntry {
                                    binding: 0,
                                    resource: style_buffer.as_entire_binding(),
                                },
                                BindGroupEntry {
                                    binding: 1,
                                    resource: BindingResource::TextureView(palette_view),
                                },
                                BindGroupEntry {
                                    binding: 2,
                                    resource: BindingResource::Sampler(palette_sampler),
                                },
                            ],
                        }));
                }
            }
        }
    }

    /// Upload font atlas texture to GPU
    pub fn upload_font_atlas(&self, atlas_data: &[u8], width: u32, height: u32) {
        self.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &self.glyph_texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            atlas_data,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width), // For R8 format, 1 byte per pixel
                rows_per_image: Some(height),
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Helper to create and upload theme texture
    fn create_theme_texture(
        &self,
        texture_data: &[u8],
        width: u32,
        height: u32,
        label: &str,
    ) -> Texture {
        let texture = self.device.create_texture(&TextureDescriptor {
            label: Some(label),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            texture_data,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture
    }

    /// Initialize themed pipeline with interpolation between two themes
    pub fn init_themed_interpolation(&mut self, texture_data: &[u8], max_colors: u32) {
        let theme_texture = self.create_theme_texture(
            &texture_data,
            256,
            max_colors * 2,
            "Theme Texture (Interpolated)",
        );
        self.complete_themed_pipeline_setup(theme_texture);
    }

    /// Initialize themed pipeline with a single theme
    pub fn init_themed_pipeline(&mut self, texture_data: &[u8], max_colors: u32) {
        let theme_texture =
            self.create_theme_texture(&texture_data, 256, max_colors, "Theme Texture");
        self.complete_themed_pipeline_setup(theme_texture);
    }

    /// Create themed pipeline with given shader module
    fn create_themed_pipeline(&self, shader: &ShaderModule) -> Option<RenderPipeline> {
        Some(self.create_pipeline(
            "Themed Glyph Pipeline",
            shader,
            &[
                self.themed_uniform_bind_group_layout.as_ref()?,
                &self.glyph_bind_group_layout,
                self.theme_bind_group_layout.as_ref()?,
            ],
            &glyph_vertex_attributes(),
            std::mem::size_of::<GlyphVertex>() as BufferAddress,
        ))
    }

    /// Complete themed pipeline setup with the given texture
    fn complete_themed_pipeline_setup(&mut self, theme_texture: Texture) {
        if self.themed_uniform_bind_group_layout.is_none() {
            let themed_uniform_layout = create_uniform_bind_group_layout(
                &self.device,
                "Themed Uniform Bind Group Layout",
                ShaderStages::VERTEX | ShaderStages::FRAGMENT,
            );
            self.themed_uniform_bind_group_layout = Some(themed_uniform_layout);

            let themed_uniform_buffer = self.device.create_buffer(&BufferDescriptor {
                label: Some("Themed Uniform Buffer"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let themed_uniform_bind_group = self.device.create_bind_group(&BindGroupDescriptor {
                label: Some("Themed Uniform Bind Group"),
                layout: self.themed_uniform_bind_group_layout.as_ref().unwrap(),
                entries: &[BindGroupEntry {
                    binding: 0,
                    resource: themed_uniform_buffer.as_entire_binding(),
                }],
            });

            self.themed_uniform_buffer = Some(themed_uniform_buffer);
            self.themed_uniform_bind_group = Some(themed_uniform_bind_group);
        }

        if self.theme_bind_group_layout.is_none() {
            let theme_layout = create_texture_bind_group_layout(
                &self.device,
                "Theme Bind Group Layout",
                TextureViewDimension::D2,
            );
            self.theme_bind_group_layout = Some(theme_layout);
        }

        let themed_shader_src = Self::load_shader(
            &self.shader_base_path,
            "glyph_themed.wgsl",
            include_str!("shaders/glyph_themed.wgsl"),
        );
        let themed_shader = self.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Themed Glyph Shader"),
            source: ShaderSource::Wgsl(themed_shader_src.into()),
        });

        if let Some(pipeline) = self.create_themed_pipeline(&themed_shader) {
            self.themed_glyph_pipeline = Some(pipeline);
        }

        let theme_view = theme_texture.create_view(&TextureViewDescriptor::default());
        let theme_sampler = self.device.create_sampler(&SamplerDescriptor {
            label: Some("Theme Sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            ..Default::default()
        });

        let theme_bind_group = self.device.create_bind_group(&BindGroupDescriptor {
            label: Some("Theme Bind Group"),
            layout: self.theme_bind_group_layout.as_ref().unwrap(),
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&theme_view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&theme_sampler),
                },
            ],
        });

        self.theme_texture = Some(theme_texture);
        self.theme_texture_view = Some(theme_view);
        self.theme_sampler = Some(theme_sampler);
        self.theme_bind_group = Some(theme_bind_group);
    }

    /// Update time for animations
    pub fn update_time(&mut self, delta_time: f32) {
        self.current_time += delta_time;
        self.themed_uniforms_dirty = true;
    }

    /// Set the current theme mode
    pub fn set_theme_mode(&mut self, mode: u32) {
        self.current_theme_mode = mode;
        self.themed_uniforms_dirty = true;
    }

    /// Check if styled pipeline is available
    pub fn has_styled_pipeline(&self) -> bool {
        self.themed_glyph_pipeline.is_some()
    }

    /// Reload all shaders from disk and recreate pipelines
    pub fn reload_shaders(&mut self) {
        let mut any_success = false;

        // Helper to reload a shader
        let reload = |src: String, name: &str| -> Option<ShaderModule> {
            match naga::front::wgsl::parse_str(&src) {
                Ok(_) => {
                    eprintln!("Successfully compiled {} shader", name);
                    Some(self.device.create_shader_module(ShaderModuleDescriptor {
                        label: Some(&format!("{} Shader (Hot Reload)", name)),
                        source: ShaderSource::Wgsl(src.into()),
                    }))
                }
                Err(e) => {
                    eprintln!("{} shader compilation failed: {}", name, e);
                    None
                }
            }
        };

        // Reload rect shader
        if let Some(shader) = reload(
            Self::load_shader(
                &self.shader_base_path,
                "rect.wgsl",
                include_str!("shaders/rect.wgsl"),
            ),
            "Rect",
        ) {
            self.rect_pipeline = self.create_rect_pipeline(&shader);
            any_success = true;
        }

        // Reload glyph shader
        if let Some(shader) = reload(
            Self::load_shader(
                &self.shader_base_path,
                "glyph.wgsl",
                include_str!("shaders/glyph.wgsl"),
            ),
            "Glyph",
        ) {
            self.glyph_pipeline = self.create_glyph_pipeline(&shader);
            any_success = true;
        }

        // Reload themed shader if initialized
        if self.theme_bind_group_layout.is_some() {
            if let Some(shader) = reload(
                Self::load_shader(
                    &self.shader_base_path,
                    "glyph_themed.wgsl",
                    include_str!("shaders/glyph_themed.wgsl"),
                ),
                "Themed",
            ) {
                self.themed_glyph_pipeline = self.create_themed_pipeline(&shader);
                any_success = true;
            }
        }

        eprintln!(
            "{}",
            if any_success {
                "Shader hot-reload complete!"
            } else {
                "No shaders were reloaded"
            }
        );
    }

    pub async unsafe fn new(window: impl Into<SurfaceTarget<'static>>, size: PhysicalSize) -> Self {
        let instance = Instance::new(&InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance.create_surface(window).unwrap();
        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                // power_preference: PowerPreference::HighPerformance,
                power_preference: PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();

        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("Tiny Editor Device"),
                required_features: Features::empty(),
                required_limits: Limits::default(),
                memory_hints: Default::default(),
                trace: Default::default(),
            })
            .await
            .unwrap();

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        let mut config = surface
            .get_default_config(&adapter, size.width, size.height)
            .unwrap();
        config.format = match config.format {
            TextureFormat::Bgra8UnormSrgb => TextureFormat::Bgra8Unorm,
            TextureFormat::Rgba8UnormSrgb => TextureFormat::Rgba8Unorm,
            other => other,
        };
        // Enable vsync for proper frame rate limiting
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);

        let shader_base_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let create_shader = |name: &str, shader_str: &str| {
            device.create_shader_module(ShaderModuleDescriptor {
                label: Some(name),
                source: ShaderSource::Wgsl(shader_str.into()),
            })
        };

        let rect_shader = create_shader("Rectangle Shader", include_str!("shaders/rect.wgsl"));
        let rounded_rect_shader = create_shader("Rounded Rectangle Shader", include_str!("shaders/rounded_rect.wgsl"));
        let glyph_shader = create_shader("Glyph Shader", include_str!("shaders/glyph.wgsl"));

        let glyph_texture = device.create_texture(&TextureDescriptor {
            label: Some("Glyph Atlas"),
            size: Extent3d {
                width: 2048,
                height: 2048,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::R8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let glyph_texture_view = glyph_texture.create_view(&TextureViewDescriptor::default());
        let glyph_sampler = device.create_sampler(&SamplerDescriptor {
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            mipmap_filter: FilterMode::Nearest,
            ..Default::default()
        });

        let create_uniform_buffer = |label: &str, size: u64| {
            device.create_buffer(&BufferDescriptor {
                label: Some(label),
                size,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        let uniform_buffer =
            create_uniform_buffer("Uniform Buffer", std::mem::size_of::<Uniforms>() as u64);
        let rect_uniform_buffer = create_uniform_buffer(
            "Rect Uniform Buffer",
            std::mem::size_of::<BasicUniforms>() as u64,
        );

        let uniform_bind_group_layout = create_uniform_bind_group_layout(
            &device,
            "Uniform Bind Group Layout (Legacy)",
            ShaderStages::VERTEX | ShaderStages::FRAGMENT,
        );

        let rect_uniform_bind_group_layout = create_uniform_bind_group_layout(
            &device,
            "Rect Uniform Bind Group Layout",
            ShaderStages::VERTEX | ShaderStages::FRAGMENT,
        );

        let uniform_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Uniform Bind Group (Legacy)"),
            layout: &uniform_bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let rect_uniform_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Rect Uniform Bind Group"),
            layout: &rect_uniform_bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: rect_uniform_buffer.as_entire_binding(),
            }],
        });

        let glyph_bind_group_layout = create_texture_bind_group_layout(
            &device,
            "Glyph Bind Group Layout",
            TextureViewDimension::D2,
        );

        let glyph_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("Glyph Bind Group"),
            layout: &glyph_bind_group_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&glyph_texture_view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&glyph_sampler),
                },
            ],
        });

        let create_vertex_buffer = |label: &str, size: u64| {
            device.create_buffer(&BufferDescriptor {
                label: Some(label),
                size,
                usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };

        // Create unit quad vertex buffer (static - never changes)
        let unit_quad = create_unit_quad();
        let rect_vertex_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("Rect Vertex Buffer (Unit Quad)"),
            size: std::mem::size_of_val(&unit_quad) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        // Upload unit quad data immediately
        rect_vertex_buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(bytemuck::cast_slice(&unit_quad));
        rect_vertex_buffer.unmap();

        // Create instance buffer for per-rect data (dynamic)
        let rect_instance_buffer = create_vertex_buffer("Rect Instance Buffer", RECT_BUFFER_SIZE);
        let rounded_rect_instance_buffer = create_vertex_buffer("Rounded Rect Instance Buffer", RECT_BUFFER_SIZE);

        let glyph_vertex_buffer = create_vertex_buffer("Glyph Vertex Buffer", GLYPH_BUFFER_SIZE);
        let line_number_vertex_buffer = create_vertex_buffer("Line Number Vertex Buffer", 1024 * 1024);
        let tab_bar_vertex_buffer = create_vertex_buffer("Tab Bar Vertex Buffer", 256 * 1024);
        let file_picker_vertex_buffer = create_vertex_buffer("File Picker Vertex Buffer", 1024 * 1024);
        let grep_vertex_buffer = create_vertex_buffer("Grep Vertex Buffer", 1024 * 1024);

        let builder = PipelineBuilder {
            device: &device,
            format: config.format,
        };
        let rect_pipeline = builder.create_instanced_pipeline(
            "Rect Pipeline (Instanced)",
            &rect_shader,
            &[&rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
            &rect_instance_attributes(),
            std::mem::size_of::<RectInstanceData>() as BufferAddress,
        );
        let rounded_rect_pipeline = builder.create_instanced_pipeline(
            "Rounded Rect Pipeline (Instanced)",
            &rounded_rect_shader,
            &[&rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
            &rounded_rect_instance_attributes(),
            std::mem::size_of::<RoundedRectInstanceData>() as BufferAddress,
        );
        let glyph_pipeline = builder.create_pipeline(
            "Glyph Pipeline",
            &glyph_shader,
            &[&rect_uniform_bind_group_layout, &glyph_bind_group_layout],
            &glyph_vertex_attributes(),
            std::mem::size_of::<GlyphVertex>() as BufferAddress,
        );

        // Initialize the FFI registry for plugins
        let ffi_registry =
            unsafe { Some(gpu_ffi::init_gpu_registry(device.clone(), queue.clone())) };

        // Register existing resources so plugins can use them and store IDs
        let (rect_pipeline_id, uniform_bind_group_id) = if let Some(ref registry) = ffi_registry {
            let pipeline_id = registry.register_pipeline(rect_pipeline.clone());
            let bind_group_id = registry.register_bind_group(rect_uniform_bind_group.clone());
            eprintln!(
                "Registered rect pipeline with ID: {:?}, bind group with ID: {:?}",
                pipeline_id, bind_group_id
            );
            (pipeline_id, bind_group_id)
        } else {
            (gpu_ffi::PipelineId(0), gpu_ffi::BindGroupId(0))
        };

        Self {
            device,
            queue,
            surface,
            config,
            shader_base_path,
            glyph_bind_group_layout,
            rect_uniform_bind_group_layout,
            rect_pipeline,
            rounded_rect_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_bind_group,
            rect_uniform_buffer,
            rect_uniform_bind_group,
            glyph_texture,
            glyph_bind_group,
            rect_vertex_buffer,
            rect_instance_buffer,
            rounded_rect_instance_buffer,
            glyph_vertex_buffer,
            line_number_vertex_buffer,
            tab_bar_vertex_buffer,
            file_picker_vertex_buffer,
            grep_vertex_buffer,
            themed_uniform_bind_group_layout: None,
            theme_bind_group_layout: None,
            style_bind_group_layout: None,
            themed_uniform_buffer: None,
            themed_uniform_bind_group: None,
            effect_pipelines: HashMap::default(),
            effect_uniform_buffers: HashMap::default(),
            effect_bind_groups: HashMap::default(),
            style_buffer: None,
            palette_texture_view: None,
            palette_sampler: None,
            styled_bind_group: None,
            themed_glyph_pipeline: None,
            theme_texture: None,
            theme_texture_view: None,
            theme_sampler: None,
            theme_bind_group: None,
            current_time: 0.0,
            current_theme_mode: 0,
            rect_pipeline_id,
            uniform_bind_group_id,
            uniforms_dirty: true,
            themed_uniforms_dirty: true,
            vertex_cache: HashMap::default(),
            last_style_hash: std::sync::atomic::AtomicU64::new(0),
            last_rect_instances_hash: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Render with a callback that receives the render pass
    ///
    /// # Safety
    /// This function is unsafe because it interacts with raw GPU resources
    pub unsafe fn render_with_callback<F>(&mut self, uniforms: Uniforms, render_callback: F)
    where
        F: FnOnce(&mut RenderPass),
    {
        // Update uniforms once per frame (only if dirty)
        if self.uniforms_dirty {
            let basic_uniforms = BasicUniforms {
                viewport_size: [self.config.width as f32, self.config.height as f32],
            };
            self.queue.write_buffer(
                &self.rect_uniform_buffer,
                0,
                bytemuck::cast_slice(&[basic_uniforms]),
            );
            self.uniforms_dirty = false;
        }

        if self.themed_uniforms_dirty {
            if let Some(themed_uniform_buffer) = &self.themed_uniform_buffer {
                let themed_uniforms = Uniforms {
                    viewport_size: [self.config.width as f32, self.config.height as f32],
                    scale_factor: 1.0,
                    time: self.current_time,
                    theme_mode: self.current_theme_mode,
                    _padding: [0.0, 0.0, 0.0],
                };
                self.queue.write_buffer(
                    themed_uniform_buffer,
                    0,
                    bytemuck::cast_slice(&[themed_uniforms]),
                );
            }
            self.themed_uniforms_dirty = false;
        }

        // Legacy uniform buffer update
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        let Ok(output) = self.surface.get_current_texture() else {
            eprintln!("Failed to get surface texture");
            return;
        };
        let view = output
            .texture
            .create_view(&TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(Color {
                            r: 0.11,
                            g: 0.12,
                            b: 0.13,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_callback(&mut render_pass);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }

    pub fn draw_rects(
        &self,
        render_pass: &mut RenderPass,
        instances: &[RectInstance],
        scale_factor: f32,
    ) {
        if instances.is_empty() {
            return;
        }

        use std::hash::{Hash, Hasher};
        use std::sync::atomic::Ordering;

        // Hash the instances and scale factor to check if anything changed
        let mut hasher = ahash::AHasher::default();
        scale_factor.to_bits().hash(&mut hasher);
        for instance in instances {
            instance.rect.x.0.to_bits().hash(&mut hasher);
            instance.rect.y.0.to_bits().hash(&mut hasher);
            instance.rect.width.0.to_bits().hash(&mut hasher);
            instance.rect.height.0.to_bits().hash(&mut hasher);
            instance.color.hash(&mut hasher);
        }
        let instances_hash = hasher.finish();

        // Check if instance data changed
        let cached_hash = self.last_rect_instances_hash.load(Ordering::Relaxed);
        let needs_update = cached_hash != instances_hash;

        if needs_update {
            // Convert RectInstance to RectInstanceData
            let instance_data: Vec<RectInstanceData> = instances
                .iter()
                .map(|rect| RectInstanceData {
                    rect_pos: [rect.rect.x.0 * scale_factor, rect.rect.y.0 * scale_factor],
                    rect_size: [
                        rect.rect.width.0 * scale_factor,
                        rect.rect.height.0 * scale_factor,
                    ],
                    color: rect.color,
                    _padding: 0,
                })
                .collect();

            // Write instance data to instance buffer
            self.queue.write_buffer(
                &self.rect_instance_buffer,
                0,
                bytemuck::cast_slice(&instance_data),
            );

            self.last_rect_instances_hash
                .store(instances_hash, Ordering::Relaxed);
        }

        // Always draw (even if buffer didn't change)
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        // Slot 0: vertex buffer (unit quad - 6 vertices)
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        // Slot 1: instance buffer (per-rect data)
        render_pass.set_vertex_buffer(1, self.rect_instance_buffer.slice(..));
        // Draw 6 vertices per instance
        render_pass.draw(0..6, 0..instances.len() as u32);
    }

    pub fn draw_rounded_rects(
        &self,
        render_pass: &mut RenderPass,
        instances: &[RoundedRectInstance],
        scale_factor: f32,
    ) {
        if instances.is_empty() {
            return;
        }

        // Convert RoundedRectInstance to RoundedRectInstanceData
        let instance_data: Vec<RoundedRectInstanceData> = instances
            .iter()
            .map(|rect| RoundedRectInstanceData {
                rect_pos: [rect.rect.x.0 * scale_factor, rect.rect.y.0 * scale_factor],
                rect_size: [
                    rect.rect.width.0 * scale_factor,
                    rect.rect.height.0 * scale_factor,
                ],
                color: rect.color,
                border_color: rect.border_color,
                corner_radius: rect.corner_radius * scale_factor,
                border_width: rect.border_width * scale_factor,
            })
            .collect();

        // Write instance data to instance buffer
        self.queue.write_buffer(
            &self.rounded_rect_instance_buffer,
            0,
            bytemuck::cast_slice(&instance_data),
        );

        // Draw
        render_pass.set_pipeline(&self.rounded_rect_pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        // Slot 0: vertex buffer (unit quad - 6 vertices)
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        // Slot 1: instance buffer (per-rounded-rect data)
        render_pass.set_vertex_buffer(1, self.rounded_rect_instance_buffer.slice(..));
        // Draw 6 vertices per instance
        render_pass.draw(0..6, 0..instances.len() as u32);
    }

    /// Draw glyphs with styled rendering at a specific buffer offset
    pub fn draw_glyphs_styled_with_offset(
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        use_styled_pipeline: bool,
        buffer_offset: u64,
    ) {
        if instances.is_empty() {
            return;
        }

        // Check if themed pipeline is available
        let has_themed_pipeline = self.themed_glyph_pipeline.is_some()
            && self.theme_bind_group.is_some()
            && self.themed_uniform_bind_group.is_some();

        if has_themed_pipeline {
            // Write vertices (uses cache - only writes if instances changed)
            let buffer_ptr = &self.glyph_vertex_buffer as *const Buffer;
            let vertex_count = self.write_cached_vertices(buffer_ptr, buffer_offset, instances);

            // Draw with themed pipeline
            render_pass.set_pipeline(self.themed_glyph_pipeline.as_ref().unwrap());
            render_pass.set_bind_group(0, self.themed_uniform_bind_group.as_ref().unwrap(), &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_bind_group(2, self.theme_bind_group.as_ref().unwrap(), &[]);
            render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(buffer_offset..));
            render_pass.draw(0..vertex_count, 0..1);
        } else if use_styled_pipeline {
            panic!("Styled rendering requested but themed pipeline not available! Make sure to call upload_theme_for_interpolation() or upload_theme() first.");
        } else {
            self.draw_glyphs(render_pass, instances, None);
        }
    }

    /// Draw glyphs with styled rendering (token-based or color-based)
    pub fn draw_glyphs_styled(
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        use_styled_pipeline: bool,
    ) {
        self.draw_glyphs_styled_with_offset(render_pass, instances, use_styled_pipeline, 0)
    }

    /// Draw UI glyphs with batched rendering (for components with multiple views)
    /// Uses dedicated buffer specified by buffer_name
    pub fn draw_ui_glyphs_batched(
        &mut self,
        render_pass: &mut RenderPass,
        batches: &[(Vec<GlyphInstance>, (u32, u32, u32, u32))],
        buffer_name: &str,
    ) {
        if batches.is_empty() {
            return;
        }

        // FIX: Combine all batches into one buffer write to prevent overwriting
        // The issue: each write_buffer overwrites at offset 0, so only the last batch renders
        let mut all_vertices = Vec::new();
        let mut batch_ranges = Vec::new(); // (start_vertex, vertex_count, scissor)

        for (instances, scissor) in batches {
            if instances.is_empty() {
                continue;
            }

            let start_vertex = all_vertices.len() as u32;
            let vertices = self.generate_glyph_vertices(instances);
            let vertex_count = vertices.len() as u32;
            all_vertices.extend(vertices);
            batch_ranges.push((start_vertex, vertex_count, *scissor));
        }

        if all_vertices.is_empty() {
            return;
        }

        // Get the appropriate buffer and write ALL vertices once
        let buffer = self.get_ui_buffer(buffer_name);
        self.queue.write_buffer(buffer, 0, bytemuck::cast_slice(&all_vertices));

        // Now draw each batch with its own scissor and vertex range
        let has_themed_pipeline = self.themed_glyph_pipeline.is_some()
            && self.theme_bind_group.is_some()
            && self.themed_uniform_bind_group.is_some();

        for (start_vertex, vertex_count, (x, y, w, h)) in batch_ranges {
            render_pass.set_scissor_rect(x, y, w, h);

            if has_themed_pipeline {
                render_pass.set_pipeline(self.themed_glyph_pipeline.as_ref().unwrap());
                render_pass.set_bind_group(0, self.themed_uniform_bind_group.as_ref().unwrap(), &[]);
                render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
                render_pass.set_bind_group(2, self.theme_bind_group.as_ref().unwrap(), &[]);
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(start_vertex..start_vertex + vertex_count, 0..1);
            } else {
                render_pass.set_pipeline(&self.glyph_pipeline);
                render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
                render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
                render_pass.set_vertex_buffer(0, buffer.slice(..));
                render_pass.draw(start_vertex..start_vertex + vertex_count, 0..1);
            }
        }
    }

    /// Get buffer for UI component by name
    fn get_ui_buffer(&self, buffer_name: &str) -> &Buffer {
        match buffer_name {
            "line_numbers" => &self.line_number_vertex_buffer,
            "tab_bar" => &self.tab_bar_vertex_buffer,
            "file_picker" => &self.file_picker_vertex_buffer,
            "grep" => &self.grep_vertex_buffer,
            _ => &self.line_number_vertex_buffer, // fallback
        }
    }

    /// Draw UI glyphs using specified buffer
    /// Scissor rect is optional - if None, uses current scissor state
    pub fn draw_ui_glyphs(
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        buffer_name: &str,
        scissor_rect: Option<(u32, u32, u32, u32)>,
    ) {
        if instances.is_empty() {
            return;
        }

        // Set scissor rect if provided
        if let Some((x, y, w, h)) = scissor_rect {
            render_pass.set_scissor_rect(x, y, w, h);
        }

        // Get the appropriate buffer
        let buffer = self.get_ui_buffer(buffer_name);

        // Write vertices directly (no caching for simplicity)
        let vertices = self.generate_glyph_vertices(instances);
        self.queue.write_buffer(buffer, 0, bytemuck::cast_slice(&vertices));

        // Check if themed pipeline is available
        let has_themed_pipeline = self.themed_glyph_pipeline.is_some()
            && self.theme_bind_group.is_some()
            && self.themed_uniform_bind_group.is_some();

        if has_themed_pipeline {
            // Draw with themed pipeline
            render_pass.set_pipeline(self.themed_glyph_pipeline.as_ref().unwrap());
            render_pass.set_bind_group(0, self.themed_uniform_bind_group.as_ref().unwrap(), &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_bind_group(2, self.theme_bind_group.as_ref().unwrap(), &[]);
            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(0..vertices.len() as u32, 0..1);
        } else {
            // Fall back to basic pipeline
            render_pass.set_pipeline(&self.glyph_pipeline);
            render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(0..vertices.len() as u32, 0..1);
        }
    }

    /// Generate vertices from glyph instances
    fn generate_glyph_vertices(&self, instances: &[GlyphInstance]) -> Vec<GlyphVertex> {
        instances
            .iter()
            .flat_map(|glyph| {
                let width = (glyph.tex_coords[2] - glyph.tex_coords[0]) * ATLAS_SIZE;
                let height = (glyph.tex_coords[3] - glyph.tex_coords[1]) * ATLAS_SIZE;
                create_glyph_vertices(
                    glyph.pos.x.0,
                    glyph.pos.y.0,
                    width,
                    height,
                    glyph.tex_coords,
                    glyph.token_id as u32,
                    glyph.relative_pos,
                    glyph.format as u32,
                )
            })
            .collect()
    }

    /// Write vertices to buffer, using cache to avoid regeneration
    /// Returns vertex count for drawing
    /// buffer_ptr is the raw pointer to the buffer to write to
    fn write_cached_vertices(
        &mut self,
        buffer_ptr: *const Buffer,
        offset: u64,
        instances: &[GlyphInstance],
    ) -> u32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let buffer_id = buffer_ptr as usize;
        let cache_key = (buffer_id, offset);

        // Hash the instances
        let mut hasher = DefaultHasher::new();
        for instance in instances {
            instance.pos.x.0.to_bits().hash(&mut hasher);
            instance.pos.y.0.to_bits().hash(&mut hasher);
            // Hash tex_coords as bits (f32 doesn't impl Hash)
            for &coord in &instance.tex_coords {
                coord.to_bits().hash(&mut hasher);
            }
            instance.token_id.hash(&mut hasher);
            instance.relative_pos.to_bits().hash(&mut hasher);
        }
        let instances_hash = hasher.finish();

        // Check cache
        let needs_update = self
            .vertex_cache
            .get(&cache_key)
            .map(|(cached_hash, _)| *cached_hash != instances_hash)
            .unwrap_or(true);

        if needs_update {
            // Cache miss or stale - generate and write vertices
            let vertices = self.generate_glyph_vertices(instances);
            let vertex_count = vertices.len() as u32;

            // Safe to dereference because we know the pointer is valid (it comes from &self.buffer)
            let buffer = unsafe { &*buffer_ptr };
            self.queue
                .write_buffer(buffer, offset, bytemuck::cast_slice(&vertices));
            self.vertex_cache
                .insert(cache_key, (instances_hash, vertices));

            vertex_count
        } else {
            // Cache hit - skip write, just return count
            self.vertex_cache.get(&cache_key).unwrap().1.len() as u32
        }
    }

    /// Get current viewport size
    pub fn viewport_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    /// Update uniforms helper
    pub fn update_uniforms(&self, viewport_width: f32, viewport_height: f32) {
        let uniforms = BasicUniforms {
            viewport_size: [viewport_width, viewport_height],
        };
        self.queue.write_buffer(
            &self.rect_uniform_buffer,
            0,
            bytemuck::cast_slice(&[uniforms]),
        );
    }

    /// Draw glyphs with optional shader effects
    pub fn draw_glyphs(
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        shader_id: Option<u32>,
    ) {
        if instances.is_empty() {
            return;
        }

        // Check if we have an effect pipeline
        let has_effect = shader_id.is_some()
            && self.effect_pipelines.contains_key(&shader_id.unwrap())
            && self.effect_bind_groups.contains_key(&shader_id.unwrap());

        // Write vertices (uses cache - only writes if instances changed)
        let buffer_ptr = &self.glyph_vertex_buffer as *const Buffer;
        let vertex_count = self.write_cached_vertices(buffer_ptr, 0, instances);

        if has_effect {
            let id = shader_id.unwrap();
            render_pass.set_pipeline(self.effect_pipelines.get(&id).unwrap());
            render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_bind_group(2, self.effect_bind_groups.get(&id).unwrap(), &[]);
        } else {
            render_pass.set_pipeline(&self.glyph_pipeline);
            render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
        }

        render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(..));
        render_pass.draw(0..vertex_count, 0..1);
    }

    /// Resize surface when window changes
    pub fn resize(&mut self, new_size: PhysicalSize) {
        if new_size.width > 0 && new_size.height > 0 {
            // Ensure any pending operations complete before reconfiguring
            let _ = self.device.poll(PollType::Wait);

            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);

            // Mark uniforms as dirty since viewport size changed
            self.uniforms_dirty = true;
            self.themed_uniforms_dirty = true;
        }
    }
}
