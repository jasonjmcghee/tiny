//! GPU rendering implementation using wgpu
//!
//! Converts render commands to actual GPU draw calls

use crate::coordinates::Viewport;
use crate::render::{BatchedDraw, GlyphInstance, RectInstance};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
#[allow(unused)]
use wgpu::hal::{DynCommandEncoder, DynDevice, DynQueue};

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
    pub color: u32,
}

/// Uniform data for shaders
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct ShaderUniforms {
    pub viewport_size: [f32; 2],
    pub _padding: [f32; 2], // Align to 16 bytes
}

/// GPU renderer that executes batched draw commands
pub struct GpuRenderer {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,

    // Pipelines
    rect_pipeline: wgpu::RenderPipeline,
    glyph_pipeline: wgpu::RenderPipeline,

    // Text effect shader pipelines
    effect_pipelines: std::collections::HashMap<u32, wgpu::RenderPipeline>,
    effect_uniform_buffers: std::collections::HashMap<u32, wgpu::Buffer>,
    effect_bind_groups: std::collections::HashMap<u32, wgpu::BindGroup>,

    // Uniform buffer
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,

    // Glyph atlas texture
    glyph_texture: wgpu::Texture,
    #[allow(dead_code)]
    glyph_texture_view: wgpu::TextureView,
    #[allow(dead_code)]
    glyph_sampler: wgpu::Sampler,
    glyph_bind_group: wgpu::BindGroup,

    // Vertex buffers
    rect_vertex_buffer: wgpu::Buffer,
    glyph_vertex_buffer: wgpu::Buffer,
}

/// Helper to create 6 vertices (2 triangles) for a rectangle
fn create_rect_vertices(x: f32, y: f32, width: f32, height: f32, color: u32) -> [RectVertex; 6] {
    let x1 = x;
    let y1 = y;
    let x2 = x + width;
    let y2 = y + height;

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
    color: u32,
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
            color,
        },
        GlyphVertex {
            position: [x2, y1],
            tex_coord: [u1, v0],
            color,
        },
        GlyphVertex {
            position: [x1, y2],
            tex_coord: [u0, v1],
            color,
        },
        // Triangle 2
        GlyphVertex {
            position: [x2, y1],
            tex_coord: [u1, v0],
            color,
        },
        GlyphVertex {
            position: [x2, y2],
            tex_coord: [u1, v1],
            color,
        },
        GlyphVertex {
            position: [x1, y2],
            tex_coord: [u0, v1],
            color,
        },
    ]
}

impl GpuRenderer {
    /// Get device for custom widget rendering
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Get queue for custom widget rendering
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
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
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 16,
                                shader_location: 2,
                                format: wgpu::VertexFormat::Uint32,
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

        // Create shaders
        // Load shaders from files
        let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Rectangle Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/rect.wgsl").into()),
        });

        let glyph_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Glyph Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/glyph.wgsl").into()),
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

        // Create uniform buffer for viewport size
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Uniform Buffer"),
            size: std::mem::size_of::<ShaderUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout for uniforms
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Uniform Bind Group Layout"),
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

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Uniform Bind Group"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
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

        // Create pipeline layout
        let rect_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Rect Pipeline Layout"),
            bind_group_layouts: &[&uniform_bind_group_layout],
            push_constant_ranges: &[],
        });

        let glyph_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Glyph Pipeline Layout"),
                bind_group_layouts: &[&uniform_bind_group_layout, &glyph_bind_group_layout],
                push_constant_ranges: &[],
            });

        // Create rect pipeline
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Rect Pipeline"),
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

        // Create glyph pipeline
        let glyph_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Glyph Pipeline"),
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
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 8,
                            shader_location: 1,
                            format: wgpu::VertexFormat::Float32x2,
                        },
                        wgpu::VertexAttribute {
                            offset: 16,
                            shader_location: 2,
                            format: wgpu::VertexFormat::Uint32,
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

        let renderer = Self {
            device,
            queue,
            surface,
            config,
            rect_pipeline,
            glyph_pipeline,
            uniform_buffer,
            uniform_bind_group,
            glyph_texture,
            glyph_texture_view,
            glyph_sampler,
            glyph_bind_group,
            rect_vertex_buffer,
            glyph_vertex_buffer,
            effect_pipelines: std::collections::HashMap::new(),
            effect_uniform_buffers: std::collections::HashMap::new(),
            effect_bind_groups: std::collections::HashMap::new(),
        };

        // Start with empty shader registry - widgets will register their own

        renderer
    }

    /// Execute batched draw commands (transforms view → physical)
    pub unsafe fn render(&mut self, batches: &[BatchedDraw], viewport: &Viewport) {
        // Update uniform buffer with PHYSICAL viewport size
        // Since glyphs are in physical pixels, shaders need physical dimensions
        let uniforms = ShaderUniforms {
            viewport_size: [
                viewport.physical_size.width as f32,
                viewport.physical_size.height as f32,
            ],
            _padding: [0.0, 0.0],
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

            // Process each batch (transform view → physical)
            for batch in batches {
                match batch {
                    BatchedDraw::RectBatch { instances } => {
                        if !instances.is_empty() {
                            self.draw_rects(&mut render_pass, instances, viewport.scale_factor);
                        }
                    }
                    BatchedDraw::GlyphBatch { instances, .. } => {
                        if !instances.is_empty() {
                            self.draw_glyphs(
                                &mut render_pass,
                                instances,
                                viewport.scale_factor,
                                None,
                            );
                        }
                    }
                    BatchedDraw::SetClip(rect) => {
                        // Transform clip rect from view to physical
                        let physical_x = (rect.x.0 * viewport.scale_factor) as u32;
                        let physical_y = (rect.y.0 * viewport.scale_factor) as u32;
                        let physical_width = (rect.width.0 * viewport.scale_factor) as u32;
                        let physical_height = (rect.height.0 * viewport.scale_factor) as u32;

                        render_pass.set_scissor_rect(
                            physical_x,
                            physical_y,
                            physical_width,
                            physical_height,
                        );
                    }
                }
            }
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

        // Generate vertices for rectangles (transform view → physical)
        let mut vertices = Vec::with_capacity(instances.len() * 6);
        for rect in instances {
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

        // Draw
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Draw glyphs with optional shader effects
    pub fn draw_glyphs(
        &self,
        render_pass: &mut wgpu::RenderPass,
        instances: &[GlyphInstance],
        scale_factor: f32,
        shader_id: Option<u32>, // Pass None for default shader
    ) {
        if instances.is_empty() {
            return;
        }

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
                glyph.color,
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
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
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
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
        }
    }
}
