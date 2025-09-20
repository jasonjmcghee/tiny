//! GPU rendering implementation using wgpu
//!
//! Provides GPU resources and methods for widget rendering

use crate::render::{GlyphInstance, RectInstance};
use ahash::HashMap;
use bytemuck::{Pod, Zeroable};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    pub _padding: f32, // Align to 32 bytes
}

/// Uniform data for basic shaders (rect and glyph)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct BasicUniforms {
    pub viewport_size: [f32; 2],
}

/// Uniform data for themed shaders with animations
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ThemedUniforms {
    pub viewport_size: [f32; 2],
    pub scale_factor: f32,
    pub time: f32,          // For animations
    pub theme_mode: u32,    // Which theme effect to use
    pub _padding: [f32; 3], // Align to 16 bytes
}

/// Basic shader uniforms for viewport information
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ShaderUniforms {
    pub viewport_size: [f32; 2],
    pub _padding: [f32; 2],
}

/// GPU renderer that executes batched draw commands
pub struct GpuRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,

    // Shader paths for hot-reloading
    shader_base_path: PathBuf,

    // Cached bind group layouts (these don't change when shaders reload)
    glyph_bind_group_layout: wgpu::BindGroupLayout,
    rect_uniform_bind_group_layout: wgpu::BindGroupLayout, // Rect-specific uniforms
    themed_uniform_bind_group_layout: Option<wgpu::BindGroupLayout>, // Themed shader uniforms
    theme_bind_group_layout: Option<wgpu::BindGroupLayout>, // Theme texture/sampler

    // Pipelines
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,

    // Text effect shader pipelines
    effect_pipelines: HashMap<u32, wgpu::RenderPipeline>,
    effect_uniform_buffers: HashMap<u32, wgpu::Buffer>,
    effect_bind_groups: HashMap<u32, wgpu::BindGroup>,

    style_buffer: Option<wgpu::Buffer>,
    palette_texture_view: Option<wgpu::TextureView>,
    palette_sampler: Option<wgpu::Sampler>,
    styled_bind_group: Option<wgpu::BindGroup>,

    // Themed glyph pipeline (uses token IDs + theme texture)
    themed_glyph_pipeline: Option<wgpu::RenderPipeline>,
    theme_texture: Option<wgpu::Texture>,
    theme_texture_view: Option<wgpu::TextureView>,
    theme_sampler: Option<wgpu::Sampler>,
    theme_bind_group: Option<wgpu::BindGroup>,
    current_time: f32,
    current_theme_mode: u32,

    // Uniform buffers for different shader types
    uniform_buffer: wgpu::Buffer, // Legacy/compatibility
    uniform_bind_group: wgpu::BindGroup,
    rect_uniform_buffer: wgpu::Buffer,
    rect_uniform_bind_group: wgpu::BindGroup,
    themed_uniform_buffer: Option<wgpu::Buffer>,
    themed_uniform_bind_group: Option<wgpu::BindGroup>,

    // Glyph atlas texture
    glyph_texture: wgpu::Texture,
    glyph_bind_group: wgpu::BindGroup,

    // Vertex buffers
    rect_vertex_buffer: wgpu::Buffer,
    glyph_vertex_buffer: wgpu::Buffer,
}

/// Helper to create 6 vertices (2 triangles) for a rectangle
pub fn create_rect_vertices(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: u32,
) -> [RectVertex; 6] {
    let x1 = x;
    let y1 = y;
    let x2 = x + width;
    let y2 = y + height;

    // Two triangles in counter-clockwise order
    // Triangle 1: top-left, top-right, bottom-left
    // Triangle 2: top-right, bottom-right, bottom-left
    [
        // Triangle 1
        RectVertex {
            position: [x1, y1],
            color,
        },
        RectVertex {
            position: [x2, y1],
            color,
        },
        RectVertex {
            position: [x1, y2],
            color,
        },
        // Triangle 2
        RectVertex {
            position: [x2, y1],
            color,
        },
        RectVertex {
            position: [x2, y2],
            color,
        },
        RectVertex {
            position: [x1, y2],
            color,
        },
    ]
}

/// Helper to create 6 vertices (2 triangles) for a glyph quad
fn create_glyph_vertices(
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    tex_coords: [f32; 4],
    token_id: u32,
    relative_pos: f32,
) -> [GlyphVertex; 6] {
    let x1 = x;
    let y1 = y;
    let x2 = x + width;
    let y2 = y + height;

    let [u0, v0, u1, v1] = tex_coords;

    [
        // Triangle 1
        GlyphVertex {
            position: [x1, y1],
            tex_coord: [u0, v0],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
        GlyphVertex {
            position: [x2, y1],
            tex_coord: [u1, v0],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
        GlyphVertex {
            position: [x1, y2],
            tex_coord: [u0, v1],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
        // Triangle 2
        GlyphVertex {
            position: [x2, y1],
            tex_coord: [u1, v0],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
        GlyphVertex {
            position: [x2, y2],
            tex_coord: [u1, v1],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
        GlyphVertex {
            position: [x1, y2],
            tex_coord: [u0, v1],
            token_id,
            relative_pos,
            _padding: 0.0,
        },
    ]
}

impl GpuRenderer {
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


    /// Create rect pipeline with given shader module
    fn create_rect_pipeline(&self, shader: &wgpu::ShaderModule) -> wgpu::RenderPipeline {
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Rect Pipeline Layout"),
                bind_group_layouts: &[&self.rect_uniform_bind_group_layout],
                push_constant_ranges: &[],
            });

        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Rect Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<RectVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Uint32,
                            },
                        ],
                    }],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
    }

    /// Create glyph pipeline with given shader module
    fn create_glyph_pipeline(&self, shader: &wgpu::ShaderModule) -> wgpu::RenderPipeline {
        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Glyph Pipeline Layout"),
                bind_group_layouts: &[
                    &self.rect_uniform_bind_group_layout,
                    &self.glyph_bind_group_layout,
                ],
                push_constant_ranges: &[],
            });

        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Glyph Pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GlyphVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2, // position
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2, // tex_coord
                            },
                            wgpu::VertexAttribute {
                                offset: 16,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Uint32, // token_id
                            },
                            wgpu::VertexAttribute {
                                offset: 20,
                                shader_location: 3,
                                format: wgpu::VertexFormat::Float32, // relative_pos
                            },
                        ],
                    }],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            })
    }

    /// Get device for custom widget rendering
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Get queue for custom widget rendering
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Get device Arc for custom widget rendering
    pub fn device_arc(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Get queue Arc for custom widget rendering
    pub fn queue_arc(&self) -> std::sync::Arc<wgpu::Queue> {
        std::sync::Arc::clone(&self.queue)
    }

    /// Get uniform bind group for viewport transforms
    pub fn uniform_bind_group(&self) -> &wgpu::BindGroup {
        &self.uniform_bind_group
    }

    /// Get surface for custom rendering
    pub fn surface(&mut self) -> &mut wgpu::Surface<'static> {
        &mut self.surface
    }

    /// Get uniform buffer for custom rendering
    pub fn uniform_buffer(&self) -> &wgpu::Buffer {
        &self.uniform_buffer
    }

    /// Get rect pipeline for widget backgrounds
    pub fn rect_pipeline(&self) -> &wgpu::RenderPipeline {
        &self.rect_pipeline
    }

    /// Get rect vertex buffer for widget backgrounds
    pub fn rect_vertex_buffer(&self) -> &wgpu::Buffer {
        &self.rect_vertex_buffer
    }

    /// Get effect uniform buffer for updating shader parameters
    pub fn effect_uniform_buffer(&self, shader_id: u32) -> Option<&wgpu::Buffer> {
        self.effect_uniform_buffers.get(&shader_id)
    }

    /// Register a text effect shader with the GPU renderer
    pub fn register_text_effect_shader(
        &mut self,
        shader_id: u32,
        shader_source: &str,
        uniform_size: u64,
    ) {
        // Create shader module
        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&format!("Text Effect Shader {}", shader_id)),
                source: wgpu::ShaderSource::Wgsl(shader_source.into()),
            });

        // Create uniform buffer for this effect
        let effect_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("Text Effect Uniform Buffer {}", shader_id)),
            size: uniform_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layouts (generic for any text effect)
        let viewport_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Text Effect Viewport Layout"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        let glyph_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Text Effect Glyph Layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });

        let effect_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("Text Effect Uniform Layout"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

        // Create effect bind group
        let effect_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("Text Effect Bind Group {}", shader_id)),
            layout: &effect_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: effect_uniform_buffer.as_entire_binding(),
            }],
        });

        // Create pipeline layout
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("Text Effect Pipeline Layout {}", shader_id)),
                bind_group_layouts: &[
                    &viewport_bind_group_layout,
                    &glyph_bind_group_layout,
                    &effect_bind_group_layout,
                ],
                push_constant_ranges: &[],
            });

        // Create render pipeline
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&format!("Text Effect Pipeline {}", shader_id)),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<GlyphVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2, // position
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2, // tex_coord
                            },
                            wgpu::VertexAttribute {
                                offset: 16,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Uint32, // token_id
                            },
                            wgpu::VertexAttribute {
                                offset: 20,
                                shader_location: 3,
                                format: wgpu::VertexFormat::Float32, // relative_pos
                            },
                        ],
                    }],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.config.format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        // Store in registries
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
            self.style_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Style Buffer"),
                size: buffer_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
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
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("Style Buffer Layout"),
                        entries: &[
                            wgpu::BindGroupLayoutEntry {
                                binding: 0,
                                visibility: wgpu::ShaderStages::VERTEX,
                                ty: wgpu::BindingType::Buffer {
                                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                                    has_dynamic_offset: false,
                                    min_binding_size: None,
                                },
                                count: None,
                            },
                            wgpu::BindGroupLayoutEntry {
                                binding: 1,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Texture {
                                    multisampled: false,
                                    view_dimension: wgpu::TextureViewDimension::D1,
                                    sample_type: wgpu::TextureSampleType::Float {
                                        filterable: true,
                                    },
                                },
                                count: None,
                            },
                            wgpu::BindGroupLayoutEntry {
                                binding: 2,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                                count: None,
                            },
                        ],
                    });

            self.styled_bind_group =
                Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Style Bind Group"),
                    layout: &style_bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: style_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(palette_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(palette_sampler),
                        },
                    ],
                }));
        }
    }

    /// Upload font atlas texture to GPU
    pub fn upload_font_atlas(&self, atlas_data: &[u8], width: u32, height: u32) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.glyph_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            atlas_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width), // For R8 format, 1 byte per pixel
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Initialize themed pipeline with interpolation between two themes
    pub fn init_themed_interpolation(
        &mut self,
        theme1: &crate::theme::Theme,
        theme2: &crate::theme::Theme,
    ) {
        let texture_data = crate::theme::Theme::merge_for_interpolation(theme1, theme2);
        let texture_width = 256;
        let max_colors = theme1
            .max_colors_per_token
            .max(theme2.max_colors_per_token)
            .max(1);
        let texture_height = max_colors * 2; // Stack two themes

        // Create theme texture
        let theme_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Theme Texture (Interpolated)"),
            size: wgpu::Extent3d {
                width: texture_width,
                height: texture_height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Upload texture data
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &theme_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texture_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture_width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: texture_width,
                height: texture_height as u32,
                depth_or_array_layers: 1,
            },
        );

        // Complete pipeline setup
        self.complete_themed_pipeline_setup(theme_texture);
    }

    /// Initialize themed pipeline with a single theme
    pub fn init_themed_pipeline(&mut self, theme: &crate::theme::Theme) {
        // Generate theme texture data
        let texture_data = theme.generate_texture_data();
        let texture_width = 256;
        let texture_height = theme.max_colors_per_token.max(1);

        // Create theme texture
        let theme_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Theme Texture"),
            size: wgpu::Extent3d {
                width: texture_width,
                height: texture_height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        // Upload texture data
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &theme_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &texture_data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(texture_width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: texture_width,
                height: texture_height as u32,
                depth_or_array_layers: 1,
            },
        );

        // Complete pipeline setup
        self.complete_themed_pipeline_setup(theme_texture);
    }

    /// Create themed pipeline with given shader module
    fn create_themed_pipeline(&self, shader: &wgpu::ShaderModule) -> Option<wgpu::RenderPipeline> {
        let themed_layout = self.themed_uniform_bind_group_layout.as_ref()?;
        let theme_layout = self.theme_bind_group_layout.as_ref()?;

        let layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Themed Pipeline Layout"),
                bind_group_layouts: &[themed_layout, &self.glyph_bind_group_layout, theme_layout],
                push_constant_ranges: &[],
            });

        Some(
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("Themed Glyph Pipeline"),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: shader,
                        entry_point: Some("vs_main"),
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<GlyphVertex>() as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &[
                                wgpu::VertexAttribute {
                                    offset: 0,
                                    shader_location: 0,
                                    format: wgpu::VertexFormat::Float32x2, // position
                                },
                                wgpu::VertexAttribute {
                                    offset: 8,
                                    shader_location: 1,
                                    format: wgpu::VertexFormat::Float32x2, // tex_coord
                                },
                                wgpu::VertexAttribute {
                                    offset: 16,
                                    shader_location: 2,
                                    format: wgpu::VertexFormat::Uint32, // token_id
                                },
                                wgpu::VertexAttribute {
                                    offset: 20,
                                    shader_location: 3,
                                    format: wgpu::VertexFormat::Float32, // relative_pos
                                },
                            ],
                        }],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: self.config.format,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                }),
        )
    }

    /// Complete themed pipeline setup with the given texture
    fn complete_themed_pipeline_setup(&mut self, theme_texture: wgpu::Texture) {
        // Create bind group layouts if not already created
        if self.themed_uniform_bind_group_layout.is_none() {
            let themed_uniform_layout =
                self.device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("Themed Uniform Bind Group Layout"),
                        entries: &[wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        }],
                    });
            self.themed_uniform_bind_group_layout = Some(themed_uniform_layout);

            // Create themed uniform buffer and bind group
            let themed_uniform_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Themed Uniform Buffer"),
                size: std::mem::size_of::<ThemedUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let themed_uniform_bind_group =
                self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Themed Uniform Bind Group"),
                    layout: self.themed_uniform_bind_group_layout.as_ref().unwrap(),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: themed_uniform_buffer.as_entire_binding(),
                    }],
                });

            self.themed_uniform_buffer = Some(themed_uniform_buffer);
            self.themed_uniform_bind_group = Some(themed_uniform_bind_group);
        }

        if self.theme_bind_group_layout.is_none() {
            let theme_layout =
                self.device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("Theme Bind Group Layout"),
                        entries: &[
                            wgpu::BindGroupLayoutEntry {
                                binding: 0,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Texture {
                                    sample_type: wgpu::TextureSampleType::Float {
                                        filterable: true,
                                    },
                                    view_dimension: wgpu::TextureViewDimension::D2,
                                    multisampled: false,
                                },
                                count: None,
                            },
                            wgpu::BindGroupLayoutEntry {
                                binding: 1,
                                visibility: wgpu::ShaderStages::FRAGMENT,
                                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                                count: None,
                            },
                        ],
                    });
            self.theme_bind_group_layout = Some(theme_layout);
        }

        // Load the themed shader
        let themed_shader_src = Self::load_shader(
            &self.shader_base_path,
            "glyph_themed.wgsl",
            include_str!("shaders/glyph_themed.wgsl"),
        );
        let themed_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("Themed Glyph Shader"),
                source: wgpu::ShaderSource::Wgsl(themed_shader_src.into()),
            });

        // Create pipeline using the new method
        if let Some(pipeline) = self.create_themed_pipeline(&themed_shader) {
            self.themed_glyph_pipeline = Some(pipeline);
        }

        // Create theme texture resources
        let theme_view = theme_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let theme_sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Theme Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Create theme bind group
        let theme_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Theme Bind Group"),
            layout: self.theme_bind_group_layout.as_ref().unwrap(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&theme_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&theme_sampler),
                },
            ],
        });

        // Store the resources
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

    /// Try to create a shader module and pipeline, returning true if successful
    fn try_reload_rect_shader(&mut self, source: String) -> bool {
        // Parse and validate WGSL before creating shader module
        match wgpu::naga::front::wgsl::parse_str(&source) {
            Ok(_module) => {
                // WGSL is valid, now create the shader
                let shader = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("Rectangle Shader (Hot Reload)"),
                        source: wgpu::ShaderSource::Wgsl(source.into()),
                    });

                // Create the new pipeline
                let new_pipeline = self.create_rect_pipeline(&shader);

                // Replace the old pipeline
                self.rect_pipeline = new_pipeline;
                eprintln!("Successfully hot-reloaded rect shader");
                true
            }
            Err(e) => {
                eprintln!("Rect shader compilation failed:");
                eprintln!("   {}", e);
                false
            }
        }
    }

    /// Try to reload the glyph shader
    fn try_reload_glyph_shader(&mut self, source: String) -> bool {
        // Parse and validate WGSL before creating shader module
        match wgpu::naga::front::wgsl::parse_str(&source) {
            Ok(_module) => {
                let shader = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("Glyph Shader (Hot Reload)"),
                        source: wgpu::ShaderSource::Wgsl(source.into()),
                    });

                let new_pipeline = self.create_glyph_pipeline(&shader);

                self.glyph_pipeline = new_pipeline;
                eprintln!("Successfully hot-reloaded glyph shader");
                true
            }
            Err(e) => {
                eprintln!("Glyph shader compilation failed:");
                eprintln!("   {}", e);
                false
            }
        }
    }

    /// Try to reload the themed shader
    fn try_reload_themed_shader(&mut self, source: String) -> bool {
        // Parse and validate WGSL before creating shader module
        match wgpu::naga::front::wgsl::parse_str(&source) {
            Ok(_module) => {
                let shader = self
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("Themed Shader (Hot Reload)"),
                        source: wgpu::ShaderSource::Wgsl(source.into()),
                    });

                if let Some(new_pipeline) = self.create_themed_pipeline(&shader) {
                    self.themed_glyph_pipeline = Some(new_pipeline);
                    eprintln!("Successfully hot-reloaded themed shader");
                    true
                } else {
                    eprintln!("Could not create themed pipeline (missing bind group layouts)");
                    false
                }
            }
            Err(e) => {
                eprintln!("Themed shader compilation failed:");
                eprintln!("   {}", e);
                false
            }
        }
    }

    /// Reload all shaders from disk and recreate pipelines
    pub fn reload_shaders(&mut self) {
        eprintln!("Hot-reloading shaders...");
        let mut any_success = false;

        // Try to reload rect shader
        let rect_src = Self::load_shader(
            &self.shader_base_path,
            "rect.wgsl",
            include_str!("shaders/rect.wgsl"),
        );
        if self.try_reload_rect_shader(rect_src) {
            any_success = true;
        } else {
            eprintln!("Keeping previous rect shader");
        }

        // Try to reload glyph shader
        let glyph_src = Self::load_shader(
            &self.shader_base_path,
            "glyph.wgsl",
            include_str!("shaders/glyph.wgsl"),
        );
        if self.try_reload_glyph_shader(glyph_src) {
            any_success = true;
        } else {
            eprintln!("Keeping previous glyph shader");
        }

        // Try to reload themed shader if initialized
        if self.theme_bind_group_layout.is_some() {
            let themed_src = Self::load_shader(
                &self.shader_base_path,
                "glyph_themed.wgsl",
                include_str!("shaders/glyph_themed.wgsl"),
            );
            if self.try_reload_themed_shader(themed_src) {
                any_success = true;
            } else {
                eprintln!("Keeping previous themed shader");
            }
        }

        if any_success {
            eprintln!("Shader hot-reload complete!");
        } else {
            eprintln!("No shaders were reloaded");
        }
    }

    pub async unsafe fn new(window: Arc<winit::window::Window>) -> Self {
        // Create instance
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });

        // Create surface
        let surface = instance.create_surface(window.clone()).unwrap();

        // Get adapter
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                // power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();

        // Create device and queue
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Tiny Editor Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
                trace: Default::default(),
            })
            .await
            .unwrap();

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        // Configure surface
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width, size.height)
            .unwrap();

        // Use linear color space to avoid double gamma correction
        // Convert sRGB format to linear equivalent
        config.format = match config.format {
            wgpu::TextureFormat::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8Unorm,
            other => other, // Keep as-is if not sRGB
        };

        surface.configure(&device, &config);

        // Get shader base path
        let shader_base_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        eprintln!("Shader base path: {:?}", shader_base_path);

        // Create shaders
        // Load shaders from files with fallback to embedded versions
        let rect_shader_src = Self::load_shader(
            &shader_base_path,
            "rect.wgsl",
            include_str!("shaders/rect.wgsl"),
        );
        let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Rectangle Shader"),
            source: wgpu::ShaderSource::Wgsl(rect_shader_src.into()),
        });

        let glyph_shader_src = Self::load_shader(
            &shader_base_path,
            "glyph.wgsl",
            include_str!("shaders/glyph.wgsl"),
        );
        let glyph_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Glyph Shader"),
            source: wgpu::ShaderSource::Wgsl(glyph_shader_src.into()),
        });

        // Create glyph texture (matches our font atlas size)
        let glyph_texture_size = wgpu::Extent3d {
            width: 2048,
            height: 2048,
            depth_or_array_layers: 1,
        };

        let glyph_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Glyph Atlas"),
            size: glyph_texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let glyph_texture_view = glyph_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Create uniform buffer for legacy/compatibility
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Uniform Buffer (Legacy)"),
            size: std::mem::size_of::<ThemedUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create rect-specific uniform buffer (8 bytes for viewport_size only)
        let rect_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Rect Uniform Buffer"),
            size: std::mem::size_of::<BasicUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout for legacy uniforms (compatibility)
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Uniform Bind Group Layout (Legacy)"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        // Create bind group layout for rect uniforms
        let rect_uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Rect Uniform Bind Group Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Uniform Bind Group (Legacy)"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let rect_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Rect Uniform Bind Group"),
            layout: &rect_uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: rect_uniform_buffer.as_entire_binding(),
            }],
        });

        // Create bind group layout for glyphs
        let glyph_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Glyph Bind Group Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let glyph_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Glyph Bind Group"),
            layout: &glyph_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&glyph_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&glyph_sampler),
                },
            ],
        });

        // Create vertex buffers
        let rect_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Rect Vertex Buffer"),
            size: 65536, // 64KB for rect vertices
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let glyph_vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Glyph Vertex Buffer"),
            size: 4 * 1024 * 1024, // 4MB for glyph vertices (supports ~68k glyphs)
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create initial pipelines directly (can't use helper methods yet)
        let rect_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Initial Rect Pipeline Layout"),
            bind_group_layouts: &[&rect_uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Initial Rect Pipeline"),
            layout: Some(&rect_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &rect_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Uint32,
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &rect_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let glyph_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Initial Glyph Pipeline Layout"),
                bind_group_layouts: &[&rect_uniform_bind_group_layout, &glyph_bind_group_layout],
                push_constant_ranges: &[],
            });

        let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Initial Glyph Pipeline"),
            layout: Some(&glyph_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &glyph_shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            offset: 0,
                            shader_location: 0,
                            format: wgpu::VertexFormat::Float32x2, // position
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2, // tex_coord
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Uint32, // token_id
                        },
                        wgpu::VertexAttribute {
                            offset: 20,
                            shader_location: 3,
                            format: wgpu::VertexFormat::Float32, // relative_pos
                        },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &glyph_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Build the renderer
        let renderer = Self {
            device: device.clone(),
            queue,
            surface,
            config,
            shader_base_path,
            glyph_bind_group_layout,
            rect_uniform_bind_group_layout,
            themed_uniform_bind_group_layout: None,
            theme_bind_group_layout: None,
            rect_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_bind_group,
            rect_uniform_buffer,
            rect_uniform_bind_group,
            themed_uniform_buffer: None,
            themed_uniform_bind_group: None,
            glyph_texture,
            glyph_bind_group,
            rect_vertex_buffer,
            glyph_vertex_buffer,
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
        };

        // Start with empty shader registry - widgets will register their own

        renderer
    }

    /// Render with widgets - combines text and widget rendering
    pub unsafe fn render_with_widgets(
        &mut self,
        tree: &crate::tree::Tree,
        viewport_rect: crate::tree::Rect,
        selections: &[crate::input::Selection],
        cpu_renderer: &mut crate::render::Renderer,
    ) {
        // Update uniform buffer - extract values we need before mutable borrow

        let physical_width = cpu_renderer.viewport.physical_size.width;
        let physical_height = cpu_renderer.viewport.physical_size.height;
        let scale_factor = cpu_renderer.viewport.scale_factor;

        let uniforms = ThemedUniforms {
            viewport_size: [physical_width as f32, physical_height as f32],
            scale_factor,
            time: self.current_time,
            theme_mode: self.current_theme_mode,
            _padding: [0.0, 0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        let output = match self.surface.get_current_texture() {
            Ok(output) => output,
            Err(e) => {
                eprintln!("Failed to get surface texture: {:?}", e);
                return;
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Render Encoder"),
            });

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.08,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            // Set GPU renderer reference for widget painting
            cpu_renderer.set_gpu_renderer(self);

            // Ensure cached doc text is updated before rendering
            cpu_renderer.cached_doc_text = Some(tree.flatten_to_string());
            cpu_renderer.cached_doc_version = tree.version;

            // Render with pass - this will paint both text and widgets
            cpu_renderer.render_with_pass(tree, viewport_rect, selections, Some(&mut render_pass));
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }

    pub fn draw_rects(
        &self,
        render_pass: &mut wgpu::RenderPass,
        instances: &[RectInstance],
        scale_factor: f32,
    ) {
        if instances.is_empty() {
            return;
        }

        // Update rect uniforms with current viewport size
        let uniforms = BasicUniforms {
            viewport_size: [self.config.width as f32, self.config.height as f32],
        };
        self.queue.write_buffer(
            &self.rect_uniform_buffer,
            0,
            bytemuck::cast_slice(&[uniforms]),
        );

        // Generate vertices for rectangles (transform view → physical)
        let mut vertices = Vec::with_capacity(instances.len() * 6);
        for (_i, rect) in instances.iter().enumerate() {
            // Apply scale factor to transform from view to physical coordinates
            let physical_x = rect.rect.x.0 * scale_factor;
            let physical_y = rect.rect.y.0 * scale_factor;
            let physical_width = rect.rect.width.0 * scale_factor;
            let physical_height = rect.rect.height.0 * scale_factor;

            let rect_verts = create_rect_vertices(
                physical_x,
                physical_y,
                physical_width,
                physical_height,
                rect.color,
            );

            vertices.extend_from_slice(&rect_verts);
        }

        // Upload vertices
        self.queue
            .write_buffer(&self.rect_vertex_buffer, 0, bytemuck::cast_slice(&vertices));

        // Draw all vertices as triangles
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));

        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Draw glyphs with styled rendering (token-based or color-based)
    pub fn draw_glyphs_styled(
        &self,
        render_pass: &mut wgpu::RenderPass,
        instances: &[GlyphInstance],
        use_styled_pipeline: bool,
    ) {
        if instances.is_empty() {
            return;
        }

        // Prefer themed pipeline over styled pipeline
        if let (Some(themed_pipeline), Some(theme_bind_group), Some(themed_bind_group)) = (
            &self.themed_glyph_pipeline,
            &self.theme_bind_group,
            &self.themed_uniform_bind_group,
        ) {
            // Use themed pipeline with theme texture
            let mut vertices = Vec::with_capacity(instances.len() * 6);
            const ATLAS_SIZE: f32 = 2048.0;

            for glyph in instances {
                let glyph_width = (glyph.tex_coords[2] - glyph.tex_coords[0]) * ATLAS_SIZE;
                let glyph_height = (glyph.tex_coords[3] - glyph.tex_coords[1]) * ATLAS_SIZE;

                let glyph_verts = create_glyph_vertices(
                    glyph.pos.x.0,
                    glyph.pos.y.0,
                    glyph_width,
                    glyph_height,
                    glyph.tex_coords,
                    glyph.token_id as u32,
                    glyph.relative_pos,
                );
                vertices.extend_from_slice(&glyph_verts);
            }

            // Upload vertices
            self.queue.write_buffer(
                &self.glyph_vertex_buffer,
                0,
                bytemuck::cast_slice(&vertices),
            );

            // Update themed uniforms with time
            let physical_width = self.config.width;
            let physical_height = self.config.height;
            let scale_factor = 1.0;

            let uniforms = ThemedUniforms {
                viewport_size: [physical_width as f32, physical_height as f32],
                scale_factor,
                time: self.current_time,
                theme_mode: self.current_theme_mode,
                _padding: [0.0, 0.0, 0.0],
            };

            if let Some(themed_uniform_buffer) = &self.themed_uniform_buffer {
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
            // Use regular color-based rendering (for widgets that don't need syntax highlighting)
            self.draw_glyphs(render_pass, instances, None);
        }
    }

    /// Draw glyphs with optional shader effects (original method)
    pub fn draw_glyphs(
        &self,
        render_pass: &mut wgpu::RenderPass,
        instances: &[GlyphInstance],
        shader_id: Option<u32>, // Pass None for default shader
    ) {
        if instances.is_empty() {
            return;
        }

        // Update basic uniforms for glyph shader
        let uniforms = BasicUniforms {
            viewport_size: [self.config.width as f32, self.config.height as f32],
        };
        self.queue.write_buffer(
            &self.rect_uniform_buffer,
            0,
            bytemuck::cast_slice(&[uniforms]),
        );

        // Choose pipeline based on shader effect
        let (pipeline, extra_bind_group) = if let Some(id) = shader_id {
            if let (Some(effect_pipeline), Some(effect_bind_group)) = (
                self.effect_pipelines.get(&id),
                self.effect_bind_groups.get(&id),
            ) {
                (effect_pipeline, Some(effect_bind_group))
            } else {
                (&self.glyph_pipeline, None)
            }
        } else {
            (&self.glyph_pipeline, None)
        };

        // Generate vertices for glyphs (already in physical coordinates after transformation)
        let mut vertices = Vec::with_capacity(instances.len() * 6);
        const ATLAS_SIZE: f32 = 2048.0;

        for glyph in instances {
            // Calculate glyph size from texture coordinates
            let glyph_width = (glyph.tex_coords[2] - glyph.tex_coords[0]) * ATLAS_SIZE;
            let glyph_height = (glyph.tex_coords[3] - glyph.tex_coords[1]) * ATLAS_SIZE;

            // Glyph positions are in physical pixels (transformed by renderer)
            let glyph_verts = create_glyph_vertices(
                glyph.pos.x.0,
                glyph.pos.y.0,
                glyph_width,
                glyph_height,
                glyph.tex_coords,
                glyph.token_id as u32,
                glyph.relative_pos,
            );
            vertices.extend_from_slice(&glyph_verts);
        }

        // Upload vertices
        self.queue.write_buffer(
            &self.glyph_vertex_buffer,
            0,
            bytemuck::cast_slice(&vertices),
        );

        // Draw with chosen pipeline
        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &self.rect_uniform_bind_group, &[]);
        render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);

        // Bind extra effect uniforms if using custom shader
        if let Some(effect_bind_group) = extra_bind_group {
            render_pass.set_bind_group(2, effect_bind_group, &[]);
        }

        render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Resize surface when window changes
    pub fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            // Ensure any pending operations complete before reconfiguring
            let _ = self.device.poll(wgpu::PollType::Wait);

            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }
}
