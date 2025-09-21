//! GPU rendering implementation using wgpu
//!
//! Provides GPU resources and methods for widget rendering

use ahash::HashMap;
use bytemuck::{Pod, Zeroable};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_sdk::{types::RectInstance, GlyphInstance, PhysicalSize};
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

/// Vertex data for rectangles
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct RectVertex {
    pub position: [f32; 2],
    pub color: u32,
}

/// Vertex data for glyphs
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct GlyphVertex {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
    pub token_id: u32,
    pub relative_pos: f32,
    pub _padding: f32,
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

    // Pipelines
    rect_pipeline: RenderPipeline,
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
    rect_vertex_buffer: Buffer,
    glyph_vertex_buffer: Buffer,
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

pub fn create_rect_vertices(x: f32, y: f32, w: f32, h: f32, color: u32) -> [RectVertex; 6] {
    quad_vertices(x, y, w, h, |pos, _| RectVertex {
        position: pos,
        color,
    })
}

fn create_glyph_vertices(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tex_coords: [f32; 4],
    token_id: u32,
    relative_pos: f32,
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
            _padding: 0.0,
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

fn glyph_vertex_attributes() -> [VertexAttribute; 4] {
    [
        vertex_attr(0, 0, VertexFormat::Float32x2),
        vertex_attr(8, 1, VertexFormat::Float32x2),
        vertex_attr(16, 2, VertexFormat::Uint32),
        vertex_attr(20, 3, VertexFormat::Float32),
    ]
}

fn rect_vertex_attributes() -> [VertexAttribute; 2] {
    [
        vertex_attr(0, 0, VertexFormat::Float32x2),
        vertex_attr(8, 1, VertexFormat::Uint32),
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
        let shader_path = base_path.join(format!("src/shaders/{}", shader_name));
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

    /// Create rect pipeline with given shader module
    fn create_rect_pipeline(&self, shader: &ShaderModule) -> RenderPipeline {
        self.create_pipeline(
            "Rect Pipeline",
            shader,
            &[&self.rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
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
        // Buffer size is already aligned since u32 is 4 bytes
        let buffer_size = (style_data.len() * 4) as u64;

        // Create or recreate buffer if size changed
        if self
            .style_buffer
            .as_ref()
            .map(|b| b.size() != buffer_size)
            .unwrap_or(true)
        {
            self.style_buffer = Some(self.create_buffer(
                "Style Buffer",
                buffer_size,
                BufferUsages::STORAGE | BufferUsages::COPY_DST,
            ));
        }

        if let Some(buffer) = &self.style_buffer {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(style_data));
        }

        // Always recreate bind group when buffer changes
        if let (Some(style_buffer), Some(palette_view), Some(palette_sampler)) = (
            &self.style_buffer,
            &self.palette_texture_view,
            &self.palette_sampler,
        ) {
            // Create bind group layout
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
                                    sample_type: TextureSampleType::Float { filterable: true },
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

            self.styled_bind_group = Some(self.device.create_bind_group(&BindGroupDescriptor {
                label: Some("Style Bind Group"),
                layout: &style_bind_group_layout,
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
    }

    /// Set the current theme mode
    pub fn set_theme_mode(&mut self, mode: u32) {
        self.current_theme_mode = mode;
    }

    /// Check if styled pipeline is available
    pub fn has_styled_pipeline(&self) -> bool {
        self.themed_glyph_pipeline.is_some()
    }

    /// Reload all shaders from disk and recreate pipelines
    pub fn reload_shaders(&mut self) {
        eprintln!("Hot-reloading shaders...");
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
                power_preference: PowerPreference::HighPerformance,
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
        surface.configure(&device, &config);

        let shader_base_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let create_shader = |name: &str, shader_str: &str| {
            device.create_shader_module(ShaderModuleDescriptor {
                label: Some(name),
                source: ShaderSource::Wgsl(shader_str.into()),
            })
        };

        let rect_shader = create_shader("Rectangle Shader", include_str!("shaders/rect.wgsl"));
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

        let rect_vertex_buffer = create_vertex_buffer("Rect Vertex Buffer", RECT_BUFFER_SIZE);
        let glyph_vertex_buffer = create_vertex_buffer("Glyph Vertex Buffer", GLYPH_BUFFER_SIZE);

        let builder = PipelineBuilder {
            device: &device,
            format: config.format,
        };
        let rect_pipeline = builder.create_pipeline(
            "Rect Pipeline",
            &rect_shader,
            &[&rect_uniform_bind_group_layout],
            &rect_vertex_attributes(),
            std::mem::size_of::<RectVertex>() as BufferAddress,
        );
        let glyph_pipeline = builder.create_pipeline(
            "Glyph Pipeline",
            &glyph_shader,
            &[&rect_uniform_bind_group_layout, &glyph_bind_group_layout],
            &glyph_vertex_attributes(),
            std::mem::size_of::<GlyphVertex>() as BufferAddress,
        );

        Self {
            device,
            queue,
            surface,
            config,
            shader_base_path,
            glyph_bind_group_layout,
            rect_uniform_bind_group_layout,
            rect_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_bind_group,
            rect_uniform_buffer,
            rect_uniform_bind_group,
            glyph_texture,
            glyph_bind_group,
            rect_vertex_buffer,
            glyph_vertex_buffer,
            themed_uniform_bind_group_layout: None,
            theme_bind_group_layout: None,
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
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
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

        self.update_uniforms(self.config.width as f32, self.config.height as f32);

        let vertices: Vec<RectVertex> = instances
            .iter()
            .flat_map(|rect| {
                create_rect_vertices(
                    rect.rect.x.0 * scale_factor,
                    rect.rect.y.0 * scale_factor,
                    rect.rect.width.0 * scale_factor,
                    rect.rect.height.0 * scale_factor,
                    rect.color,
                )
            })
            .collect();

        self.queue
            .write_buffer(&self.rect_vertex_buffer, 0, bytemuck::cast_slice(&vertices));

        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Draw glyphs with styled rendering (token-based or color-based)
    pub fn draw_glyphs_styled(
        &self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        use_styled_pipeline: bool,
    ) {
        if instances.is_empty() {
            return;
        }

        if let (Some(themed_pipeline), Some(theme_bind_group), Some(themed_bind_group)) = (
            &self.themed_glyph_pipeline,
            &self.theme_bind_group,
            &self.themed_uniform_bind_group,
        ) {
            let vertices = self.generate_glyph_vertices(instances);
            self.queue.write_buffer(
                &self.glyph_vertex_buffer,
                0,
                bytemuck::cast_slice(&vertices),
            );

            // Update themed uniforms
            if let Some(themed_uniform_buffer) = &self.themed_uniform_buffer {
                let uniforms = Uniforms {
                    viewport_size: [self.config.width as f32, self.config.height as f32],
                    scale_factor: 1.0,
                    time: self.current_time,
                    theme_mode: self.current_theme_mode,
                    _padding: [0.0, 0.0, 0.0],
                };
                self.queue.write_buffer(
                    themed_uniform_buffer,
                    0,
                    bytemuck::cast_slice(&[uniforms]),
                );
            }

            // Draw with themed pipeline
            render_pass.set_pipeline(themed_pipeline);
            render_pass.set_bind_group(0, themed_bind_group, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_bind_group(2, theme_bind_group, &[]);
            render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(..));
            render_pass.draw(0..vertices.len() as u32, 0..1);
        } else if use_styled_pipeline {
            panic!("Styled rendering requested but themed pipeline not available! Make sure to call upload_theme_for_interpolation() or upload_theme() first.");
        } else {
            self.draw_glyphs(render_pass, instances, None);
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
                )
            })
            .collect()
    }

    /// Update uniforms helper
    fn update_uniforms(&self, viewport_width: f32, viewport_height: f32) {
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
        &self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        shader_id: Option<u32>,
    ) {
        if instances.is_empty() {
            return;
        }

        self.update_uniforms(self.config.width as f32, self.config.height as f32);

        let (pipeline, extra_bind_group) = shader_id
            .and_then(|id| {
                Some((
                    self.effect_pipelines.get(&id)?,
                    Some(self.effect_bind_groups.get(&id)?),
                ))
            })
            .unwrap_or((&self.glyph_pipeline, None));

        let vertices = self.generate_glyph_vertices(instances);
        self.queue.write_buffer(
            &self.glyph_vertex_buffer,
            0,
            bytemuck::cast_slice(&vertices),
        );

        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
        if let Some(effect_bind_group) = extra_bind_group {
            render_pass.set_bind_group(2, effect_bind_group, &[]);
        }
        render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Resize surface when window changes
    pub fn resize(&mut self, new_size: PhysicalSize) {
        if new_size.width > 0 && new_size.height > 0 {
            // Ensure any pending operations complete before reconfiguring
            let _ = self.device.poll(PollType::Wait);

            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }
}
