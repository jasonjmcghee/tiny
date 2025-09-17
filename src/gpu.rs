//! GPU rendering implementation using wgpu
//!
//! Converts render commands to actual GPU draw calls

use crate::render::{BatchedDraw, GlyphInstance, RectInstance};
use bytemuck::{Pod, Zeroable};
use std::sync::Arc;
#[allow(unused)]
use wgpu::hal::{DynCommandEncoder, DynDevice, DynQueue};
use crate::coordinates::Viewport;

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

impl GpuRenderer {
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
        let config = surface
            .get_default_config(&adapter, size.width, size.height)
            .unwrap();
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
            size: 262144, // 256KB for glyph vertices
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
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
        }
    }

    /// Execute batched draw commands (transforms view → physical)
    pub unsafe fn render(&mut self, batches: &[BatchedDraw], viewport: &Viewport) {
        println!("GPU::render called with {} batches, viewport: logical={:.0}x{:.0}, scale={:.1}",
                 batches.len(), viewport.logical_size.width, viewport.logical_size.height, viewport.scale_factor);

        // Update uniform buffer with PHYSICAL viewport size
        // Since glyphs are in physical pixels, shaders need physical dimensions
        let uniforms = ShaderUniforms {
            viewport_size: [viewport.physical_size.width as f32, viewport.physical_size.height as f32],
            _padding: [0.0, 0.0],
        };
        println!("  Sending physical viewport_size [{:.0}, {:.0}] to shaders",
                 viewport.physical_size.width as f32, viewport.physical_size.height as f32);
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));

        let output = match self.surface.get_current_texture() {
            Ok(output) => {
                println!("  ✅ Got surface texture: {}x{}", output.texture.width(), output.texture.height());
                output
            },
            Err(e) => {
                eprintln!("  ❌ Failed to get surface texture: {:?}", e);
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
                            self.draw_glyphs(&mut render_pass, instances, viewport.scale_factor);
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

    fn draw_rects(&self, render_pass: &mut wgpu::RenderPass, instances: &[RectInstance], scale_factor: f32) {
        if instances.is_empty() {
            return;
        }

        println!("GPU: draw_rects called with {} rectangles, scale={:.1}", instances.len(), scale_factor);

        // Generate vertices for rectangles (transform view → physical)
        let mut vertices = Vec::new();
        for (i, rect) in instances.iter().enumerate() {
            // Apply scale factor to transform from view to physical coordinates
            let physical_x = rect.rect.x.0 * scale_factor;
            let physical_y = rect.rect.y.0 * scale_factor;
            let physical_width = rect.rect.width.0 * scale_factor;
            let physical_height = rect.rect.height.0 * scale_factor;

            if i == 0 {
                println!("  First rect: view=({:.1}, {:.1}) {}x{} → physical=({:.1}, {:.1}) {}x{}",
                         rect.rect.x.0, rect.rect.y.0, rect.rect.width.0, rect.rect.height.0,
                         physical_x, physical_y, physical_width, physical_height);
            }

            // Two triangles for each rectangle (in physical coordinates)
            let x1 = physical_x;
            let y1 = physical_y;
            let x2 = physical_x + physical_width;
            let y2 = physical_y + physical_height;

            // Triangle 1
            vertices.push(RectVertex {
                position: [x1, y1],
                color: rect.color,
            });
            vertices.push(RectVertex {
                position: [x2, y1],
                color: rect.color,
            });
            vertices.push(RectVertex {
                position: [x1, y2],
                color: rect.color,
            });

            // Triangle 2
            vertices.push(RectVertex {
                position: [x2, y1],
                color: rect.color,
            });
            vertices.push(RectVertex {
                position: [x2, y2],
                color: rect.color,
            });
            vertices.push(RectVertex {
                position: [x1, y2],
                color: rect.color,
            });

            println!("    Vertices: ({:.1},{:.1}) ({:.1},{:.1}) ({:.1},{:.1}) ...",
                     x1, y1, x2, y1, x1, y2);
        }

        println!("  Generated {} vertices for {} rectangles", vertices.len(), instances.len());

        // Upload vertices
        self.queue
            .write_buffer(&self.rect_vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        println!("  Uploaded {} bytes to vertex buffer", vertices.len() * std::mem::size_of::<RectVertex>());

        // Draw
        println!("  Setting pipeline and drawing...");
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.rect_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
        println!("  Draw call completed with {} vertices", vertices.len());
    }

    fn draw_glyphs(&self, render_pass: &mut wgpu::RenderPass, instances: &[GlyphInstance], scale_factor: f32) {
        if instances.is_empty() {
            return;
        }

        println!("GPU: draw_glyphs called with {} glyphs, scale={:.1}", instances.len(), scale_factor);

        // Generate vertices for glyphs (already in physical coordinates from font system)
        let mut vertices = Vec::new();
        for (i, glyph) in instances.iter().enumerate() {
            // Calculate glyph size from texture coordinates
            // Assuming atlas is 2048x2048
            let atlas_size = 2048.0;
            let glyph_width = (glyph.tex_coords[2] - glyph.tex_coords[0]) * atlas_size;
            let glyph_height = (glyph.tex_coords[3] - glyph.tex_coords[1]) * atlas_size;

            // Glyph positions and dimensions are already in physical pixels from font system
            // No need to apply scale factor
            let physical_x = glyph.pos.x.0;
            let physical_y = glyph.pos.y.0;
            let physical_width = glyph_width;
            let physical_height = glyph_height;

            if i == 0 {
                println!("  First glyph: physical=({:.1}, {:.1}), size={:.1}x{:.1}",
                         physical_x, physical_y, physical_width, physical_height);
                println!("    Tex coords: [{:.3}, {:.3}, {:.3}, {:.3}]",
                         glyph.tex_coords[0], glyph.tex_coords[1], glyph.tex_coords[2], glyph.tex_coords[3]);
            }

            // Quad for each glyph (already in physical coordinates)
            let x1 = physical_x;
            let y1 = physical_y;
            let x2 = physical_x + physical_width;
            let y2 = physical_y + physical_height;

            // Extract texture coordinates from glyph
            let u0 = glyph.tex_coords[0];
            let v0 = glyph.tex_coords[1];
            let u1 = glyph.tex_coords[2];
            let v1 = glyph.tex_coords[3];

            // Triangle 1
            vertices.push(GlyphVertex {
                position: [x1, y1],
                tex_coord: [u0, v0],
                color: glyph.color,
            });
            vertices.push(GlyphVertex {
                position: [x2, y1],
                tex_coord: [u1, v0],
                color: glyph.color,
            });
            vertices.push(GlyphVertex {
                position: [x1, y2],
                tex_coord: [u0, v1],
                color: glyph.color,
            });

            // Triangle 2
            vertices.push(GlyphVertex {
                position: [x2, y1],
                tex_coord: [u1, v0],
                color: glyph.color,
            });
            vertices.push(GlyphVertex {
                position: [x2, y2],
                tex_coord: [u1, v1],
                color: glyph.color,
            });
            vertices.push(GlyphVertex {
                position: [x1, y2],
                tex_coord: [u0, v1],
                color: glyph.color,
            });
        }

        println!("  Generated {} vertices for {} glyphs", vertices.len(), instances.len());

        // Upload vertices
        self.queue.write_buffer(
            &self.glyph_vertex_buffer,
            0,
            bytemuck::cast_slice(&vertices),
        );
        println!("  Uploaded {} bytes to glyph vertex buffer", vertices.len() * std::mem::size_of::<GlyphVertex>());

        // Draw
        println!("  Setting glyph pipeline and binding texture...");
        render_pass.set_pipeline(&self.glyph_pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.glyph_vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
        println!("  Glyph draw call completed with {} vertices", vertices.len());
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
