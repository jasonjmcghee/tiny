//! GPU rendering implementation using wgpu
//!
//! Provides GPU resources and methods for widget rendering

use crate::{gpu_ffi, gpu_resources::*};
use ahash::HashMap;
use bytemuck::{Pod, Zeroable};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_sdk::{
    types::{RectInstance, RoundedRectInstance},
    GlyphInstance, PhysicalSize,
};
use wgpu::{
    naga, AddressMode, Backends, BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout,
    BindGroupLayoutDescriptor, BindGroupLayoutEntry, BindingResource, BindingType, Buffer,
    BufferAddress, BufferBindingType, BufferDescriptor, BufferUsages, Color,
    CommandEncoderDescriptor, Device, DeviceDescriptor, Extent3d, Features, FilterMode, Instance,
    InstanceDescriptor, Limits, LoadOp, Operations, Origin3d, PollType, PowerPreference, Queue,
    RenderPass, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RequestAdapterOptions, Sampler, SamplerBindingType, SamplerDescriptor, ShaderModule,
    ShaderModuleDescriptor, ShaderSource, ShaderStages, StoreOp, Surface, SurfaceConfiguration,
    SurfaceTarget, TexelCopyBufferLayout, TexelCopyTextureInfo, Texture, TextureAspect,
    TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
    TextureView, TextureViewDescriptor, TextureViewDimension, VertexAttribute, VertexFormat,
    VertexStepMode,
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

/// GPU renderer that executes batched draw commands
pub struct GpuRenderer {
    device: Arc<Device>,
    queue: Arc<Queue>,
    surface: Surface<'static>,
    config: SurfaceConfiguration,

    // Shader paths for hot-reloading
    shader_base_path: PathBuf,

    // Resource managers
    buffers: BufferRegistry,
    uniforms: UniformManager,

    // Cached bind group layouts (these don't change when shaders reload)
    glyph_bind_group_layout: BindGroupLayout,
    theme_bind_group_layout: Option<BindGroupLayout>,
    style_bind_group_layout: Option<BindGroupLayout>,

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
    pub current_time: f32,
    pub current_theme_mode: u32,

    // Glyph atlas texture
    glyph_texture: Texture,
    glyph_bind_group: BindGroup,

    // Store registered IDs for plugin context
    rect_pipeline_id: gpu_ffi::PipelineId,
    uniform_bind_group_id: gpu_ffi::BindGroupId,
}

/// Configuration for drawing operations
pub struct DrawConfig {
    pub buffer_name: &'static str,
    pub use_themed: bool,
    pub scissor: Option<(u32, u32, u32, u32)>,
}

impl Default for DrawConfig {
    fn default() -> Self {
        Self {
            buffer_name: "default",
            use_themed: false,
            scissor: None,
        }
    }
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

/// Generate vertices from glyph instances
fn instances_to_vertices(instances: &[GlyphInstance]) -> Vec<GlyphVertex> {
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

// Old PipelineBuilder removed - using enhanced version from gpu_resources

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

    // Pipeline creation helpers removed - using PipelineBuilder directly

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
        self.uniforms
            .bind_group("uniform")
            .expect("uniform bind group not initialized")
    }

    /// Get surface for custom rendering
    pub fn surface(&mut self) -> &mut Surface<'static> {
        &mut self.surface
    }

    /// Get uniform buffer for custom rendering
    pub fn uniform_buffer(&self) -> &Buffer {
        self.uniforms
            .buffer("uniform")
            .expect("uniform buffer not initialized")
    }

    /// Get rect pipeline for widget backgrounds
    pub fn rect_pipeline(&self) -> &RenderPipeline {
        &self.rect_pipeline
    }

    /// Get rect vertex buffer for widget backgrounds
    pub fn rect_vertex_buffer(&self) -> &Buffer {
        self.buffers
            .get("rect_vertex")
            .expect("rect vertex buffer not initialized")
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
        let buffer = self
            .buffers
            .get("rect_vertex")
            .expect("rect vertex buffer not initialized");
        self.queue.write_buffer(buffer, 0, data);
    }

    /// Draw vertices directly for plugins (avoids passing Buffer objects)
    pub fn draw_plugin_vertices(
        &self,
        render_pass: &mut RenderPass,
        vertex_data: &[u8],
        vertex_count: u32,
    ) {
        let rect_buffer = self
            .buffers
            .get("rect_vertex")
            .expect("rect vertex buffer not initialized");
        let rect_uniform_bg = self
            .uniforms
            .bind_group("rect_uniform")
            .expect("rect uniform not initialized");

        self.queue.write_buffer(rect_buffer, 0, vertex_data);

        // Set up pipeline
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, rect_uniform_bg, &[]);
        render_pass.set_vertex_buffer(0, rect_buffer.slice(..));
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

        let pipeline = PipelineBuilder::new(&self.device, self.config.format)
            .label(format!("Text Effect Pipeline {}", shader_id))
            .shader(&shader)
            .bind_group_layout(&viewport_layout)
            .bind_group_layout(&glyph_layout)
            .bind_group_layout(&effect_layout)
            .vertex_buffer(
                std::mem::size_of::<GlyphVertex>() as BufferAddress,
                VertexStepMode::Vertex,
                &glyph_vertex_attributes(),
            )
            .build();

        self.effect_pipelines.insert(shader_id, pipeline);
        self.effect_uniform_buffers
            .insert(shader_id, effect_uniform_buffer);
        self.effect_bind_groups.insert(shader_id, effect_bind_group);
    }

    /// Upload style buffer as u32 (for shader compatibility)
    pub fn upload_style_buffer_u32(&mut self, style_data: &[u32]) {
        let buffer_size = (style_data.len() * 4) as u64;

        // Create or recreate buffer if size changed
        let buffer_recreated = self
            .style_buffer
            .as_ref()
            .map(|b| b.size() != buffer_size)
            .unwrap_or(true);

        if buffer_recreated {
            self.style_buffer = Some(self.create_buffer(
                "Style Buffer",
                buffer_size,
                BufferUsages::STORAGE | BufferUsages::COPY_DST,
            ));
        }

        // Write data (GPU driver handles caching)
        if let Some(buffer) = &self.style_buffer {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(style_data));
        }

        // Recreate bind group when buffer was recreated or it doesn't exist
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
        let themed_uniform_layout = self.uniforms.layout("themed_uniform")?;
        let theme_layout = self.theme_bind_group_layout.as_ref()?;

        Some(
            PipelineBuilder::new(&self.device, self.config.format)
                .label("Themed Glyph Pipeline")
                .shader(shader)
                .bind_group_layout(themed_uniform_layout)
                .bind_group_layout(&self.glyph_bind_group_layout)
                .bind_group_layout(theme_layout)
                .vertex_buffer(
                    std::mem::size_of::<GlyphVertex>() as BufferAddress,
                    VertexStepMode::Vertex,
                    &glyph_vertex_attributes(),
                )
                .build(),
        )
    }

    /// Complete themed pipeline setup with the given texture
    fn complete_themed_pipeline_setup(&mut self, theme_texture: Texture) {
        // Create themed uniform if not already created
        if self.uniforms.bind_group("themed_uniform").is_none() {
            self.uniforms.create(
                "themed_uniform",
                std::mem::size_of::<Uniforms>() as u64,
                ShaderStages::VERTEX | ShaderStages::FRAGMENT,
            );
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
            let rect_uniform_layout = self
                .uniforms
                .layout("rect_uniform")
                .expect("rect_uniform layout not found");
            self.rect_pipeline = PipelineBuilder::new(&self.device, self.config.format)
                .label("Rect Pipeline (Instanced)")
                .shader(&shader)
                .bind_group_layout(rect_uniform_layout)
                .vertex_buffer(
                    std::mem::size_of::<RectVertex>() as BufferAddress,
                    VertexStepMode::Vertex,
                    &rect_vertex_attributes(),
                )
                .vertex_buffer(
                    std::mem::size_of::<RectInstanceData>() as BufferAddress,
                    VertexStepMode::Instance,
                    &rect_instance_attributes(),
                )
                .build();
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
            let rect_uniform_layout = self
                .uniforms
                .layout("rect_uniform")
                .expect("rect_uniform layout not found");
            self.glyph_pipeline = PipelineBuilder::new(&self.device, self.config.format)
                .label("Glyph Pipeline")
                .shader(&shader)
                .bind_group_layout(rect_uniform_layout)
                .bind_group_layout(&self.glyph_bind_group_layout)
                .vertex_buffer(
                    std::mem::size_of::<GlyphVertex>() as BufferAddress,
                    VertexStepMode::Vertex,
                    &glyph_vertex_attributes(),
                )
                .build();
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
        let rounded_rect_shader = create_shader(
            "Rounded Rectangle Shader",
            include_str!("shaders/rounded_rect.wgsl"),
        );
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

        // Create uniform manager and initialize uniforms
        let mut uniforms = UniformManager::new(device.clone());
        uniforms.create(
            "uniform",
            std::mem::size_of::<Uniforms>() as u64,
            ShaderStages::VERTEX | ShaderStages::FRAGMENT,
        );
        uniforms.create(
            "rect_uniform",
            std::mem::size_of::<Uniforms>() as u64,
            ShaderStages::VERTEX | ShaderStages::FRAGMENT,
        );

        let rect_uniform_bind_group_layout = uniforms.layout("rect_uniform").unwrap().clone();

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

        // Create buffer registry
        let mut buffers = BufferRegistry::new(device.clone());

        // Create unit quad vertex buffer (static - never changes)
        // This is the only buffer we pre-create since it's used by all rect rendering
        let unit_quad = create_unit_quad();
        let rect_vertex_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("Rect Vertex Buffer (Unit Quad)"),
            size: std::mem::size_of_val(&unit_quad) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        rect_vertex_buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(bytemuck::cast_slice(&unit_quad));
        rect_vertex_buffer.unmap();
        buffers
            .buffers
            .insert("rect_vertex".to_string(), rect_vertex_buffer);

        // All other buffers are created on-demand when first used

        // Use enhanced PipelineBuilder for cleaner pipeline creation
        let rect_pipeline = PipelineBuilder::new(&device, config.format)
            .label("Rect Pipeline (Instanced)")
            .shader(&rect_shader)
            .bind_group_layout(&rect_uniform_bind_group_layout)
            .vertex_buffer(
                std::mem::size_of::<RectVertex>() as BufferAddress,
                VertexStepMode::Vertex,
                &rect_vertex_attributes(),
            )
            .vertex_buffer(
                std::mem::size_of::<RectInstanceData>() as BufferAddress,
                VertexStepMode::Instance,
                &rect_instance_attributes(),
            )
            .build();

        let rounded_rect_pipeline = PipelineBuilder::new(&device, config.format)
            .label("Rounded Rect Pipeline (Instanced)")
            .shader(&rounded_rect_shader)
            .bind_group_layout(&rect_uniform_bind_group_layout)
            .vertex_buffer(
                std::mem::size_of::<RectVertex>() as BufferAddress,
                VertexStepMode::Vertex,
                &rect_vertex_attributes(),
            )
            .vertex_buffer(
                std::mem::size_of::<RoundedRectInstanceData>() as BufferAddress,
                VertexStepMode::Instance,
                &rounded_rect_instance_attributes(),
            )
            .build();

        let glyph_pipeline = PipelineBuilder::new(&device, config.format)
            .label("Glyph Pipeline")
            .shader(&glyph_shader)
            .bind_group_layout(&rect_uniform_bind_group_layout)
            .bind_group_layout(&glyph_bind_group_layout)
            .vertex_buffer(
                std::mem::size_of::<GlyphVertex>() as BufferAddress,
                VertexStepMode::Vertex,
                &glyph_vertex_attributes(),
            )
            .build();

        // Initialize the FFI registry for plugins
        let ffi_registry =
            unsafe { Some(gpu_ffi::init_gpu_registry(device.clone(), queue.clone())) };

        // Register existing resources so plugins can use them and store IDs
        let (rect_pipeline_id, uniform_bind_group_id) = if let Some(ref registry) = ffi_registry {
            let pipeline_id = registry.register_pipeline(rect_pipeline.clone());
            let bind_group_id =
                registry.register_bind_group(uniforms.bind_group("rect_uniform").unwrap().clone());
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
            buffers,
            uniforms,
            glyph_bind_group_layout,
            theme_bind_group_layout: None,
            style_bind_group_layout: None,
            rect_pipeline,
            rounded_rect_pipeline,
            glyph_pipeline,
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
            glyph_texture,
            glyph_bind_group,
            rect_pipeline_id,
            uniform_bind_group_id,
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
        // Update all uniforms every frame (GPU driver handles caching)
        let rect_uniforms = Uniforms {
            viewport_size: [self.config.width as f32, self.config.height as f32],
            scale_factor: 1.0,
            time: self.current_time,
            theme_mode: self.current_theme_mode,
            _padding: [0.0, 0.0, 0.0],
        };
        if let Some(buffer) = self.uniforms.buffer("rect_uniform") {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[rect_uniforms]));
        }

        if let Some(buffer) = self.uniforms.buffer("themed_uniform") {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[rect_uniforms]));
        }

        // Legacy uniform buffer update
        if let Some(buffer) = self.uniforms.buffer("uniform") {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[uniforms]));
        }

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
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[RectInstance],
        scale_factor: f32,
    ) {
        if instances.is_empty() {
            return;
        }

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

        // Create buffer on demand
        self.buffers.create_or_get(
            "rect_instance",
            RECT_BUFFER_SIZE,
            BufferUsages::VERTEX | BufferUsages::COPY_DST,
        );

        // Get buffers (after create_or_get to avoid borrow issues)
        let rect_instance_buf = self
            .buffers
            .get("rect_instance")
            .expect("rect_instance buffer not found");
        let rect_vertex_buf = self
            .buffers
            .get("rect_vertex")
            .expect("rect_vertex buffer not found");
        let rect_uniform_bg = self
            .uniforms
            .bind_group("rect_uniform")
            .expect("rect_uniform not found");

        // Write instance data
        self.queue
            .write_buffer(rect_instance_buf, 0, bytemuck::cast_slice(&instance_data));

        // Draw
        render_pass.set_pipeline(&self.rect_pipeline);
        render_pass.set_bind_group(0, rect_uniform_bg, &[]);
        render_pass.set_vertex_buffer(0, rect_vertex_buf.slice(..));
        render_pass.set_vertex_buffer(1, rect_instance_buf.slice(..));
        render_pass.draw(0..6, 0..instances.len() as u32);
    }

    pub fn draw_rounded_rects(
        &mut self,
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

        // Create buffer on demand
        self.buffers.create_or_get(
            "rounded_rect_instance",
            RECT_BUFFER_SIZE,
            BufferUsages::VERTEX | BufferUsages::COPY_DST,
        );

        // Get buffers
        let rounded_instance_buf = self
            .buffers
            .get("rounded_rect_instance")
            .expect("rounded_rect_instance buffer not found");
        let rect_vertex_buf = self
            .buffers
            .get("rect_vertex")
            .expect("rect_vertex buffer not found");
        let rect_uniform_bg = self
            .uniforms
            .bind_group("rect_uniform")
            .expect("rect_uniform not found");

        // Write instance data
        self.queue.write_buffer(
            rounded_instance_buf,
            0,
            bytemuck::cast_slice(&instance_data),
        );

        // Draw
        render_pass.set_pipeline(&self.rounded_rect_pipeline);
        render_pass.set_bind_group(0, rect_uniform_bg, &[]);
        render_pass.set_vertex_buffer(0, rect_vertex_buf.slice(..));
        render_pass.set_vertex_buffer(1, rounded_instance_buf.slice(..));
        render_pass.draw(0..6, 0..instances.len() as u32);
    }

    /// Unified method for drawing glyphs with any configuration
    /// Replaces: draw_glyphs, draw_glyphs_styled, draw_ui_glyphs, draw_ui_glyphs_batched
    pub fn draw_glyphs(
        &mut self,
        render_pass: &mut RenderPass,
        instances: &[GlyphInstance],
        config: DrawConfig,
    ) {
        if instances.is_empty() {
            return;
        }

        // Create buffer on demand if it doesn't exist
        let buffer = self.buffers.create_or_get(
            config.buffer_name,
            GLYPH_BUFFER_SIZE,
            BufferUsages::VERTEX | BufferUsages::COPY_DST,
        );

        // Generate and write vertices (no caching - GPU driver handles this)
        let vertices = instances_to_vertices(instances);
        self.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(&vertices));

        // Set scissor if provided
        if let Some((x, y, w, h)) = config.scissor {
            render_pass.set_scissor_rect(x, y, w, h);
        }

        // Choose pipeline and bind groups
        let has_themed = self.themed_glyph_pipeline.is_some()
            && self.theme_bind_group.is_some()
            && self.uniforms.bind_group("themed_uniform").is_some();

        if config.use_themed && has_themed {
            let themed_uniform_bg = self.uniforms.bind_group("themed_uniform").unwrap();
            render_pass.set_pipeline(self.themed_glyph_pipeline.as_ref().unwrap());
            render_pass.set_bind_group(0, themed_uniform_bg, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            render_pass.set_bind_group(2, self.theme_bind_group.as_ref().unwrap(), &[]);
        } else {
            let rect_uniform_bg = self
                .uniforms
                .bind_group("rect_uniform")
                .expect("rect_uniform not found");
            render_pass.set_pipeline(&self.glyph_pipeline);
            render_pass.set_bind_group(0, rect_uniform_bg, &[]);
            render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
        }

        render_pass.set_vertex_buffer(0, buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }

    /// Draw batched glyphs with per-batch scissor rects
    /// Used for UI components like file picker with multiple views
    pub fn draw_glyphs_batched(
        &mut self,
        render_pass: &mut RenderPass,
        batches: &[(Vec<GlyphInstance>, (u32, u32, u32, u32))],
        buffer_name: &'static str,
        use_themed: bool,
    ) {
        if batches.is_empty() {
            return;
        }

        // Combine all batches into one buffer write
        let mut all_vertices = Vec::new();
        let mut batch_ranges = Vec::new(); // (start_vertex, vertex_count, scissor)

        for (instances, scissor) in batches {
            if instances.is_empty() {
                continue;
            }
            let start_vertex = all_vertices.len() as u32;
            let vertices = instances_to_vertices(instances);
            let vertex_count = vertices.len() as u32;
            all_vertices.extend(vertices);
            batch_ranges.push((start_vertex, vertex_count, *scissor));
        }

        if all_vertices.is_empty() {
            return;
        }

        // Create buffer on demand and write all vertices
        let buffer = self.buffers.create_or_get(
            buffer_name,
            GLYPH_BUFFER_SIZE,
            BufferUsages::VERTEX | BufferUsages::COPY_DST,
        );
        self.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(&all_vertices));

        // Draw each batch with its own scissor
        let has_themed = self.themed_glyph_pipeline.is_some()
            && self.theme_bind_group.is_some()
            && self.uniforms.bind_group("themed_uniform").is_some();

        for (start_vertex, vertex_count, (x, y, w, h)) in batch_ranges {
            render_pass.set_scissor_rect(x, y, w, h);

            if use_themed && has_themed {
                let themed_uniform_bg = self.uniforms.bind_group("themed_uniform").unwrap();
                render_pass.set_pipeline(self.themed_glyph_pipeline.as_ref().unwrap());
                render_pass.set_bind_group(0, themed_uniform_bg, &[]);
                render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
                render_pass.set_bind_group(2, self.theme_bind_group.as_ref().unwrap(), &[]);
            } else {
                let rect_uniform_bg = self
                    .uniforms
                    .bind_group("rect_uniform")
                    .expect("rect_uniform not found");
                render_pass.set_pipeline(&self.glyph_pipeline);
                render_pass.set_bind_group(0, rect_uniform_bg, &[]);
                render_pass.set_bind_group(1, &self.glyph_bind_group, &[]);
            }

            render_pass.set_vertex_buffer(0, buffer.slice(..));
            render_pass.draw(start_vertex..start_vertex + vertex_count, 0..1);
        }
    }

    /// Get current viewport size
    pub fn viewport_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    /// Update uniforms helper
    pub fn update_uniforms(&self, viewport_width: f32, viewport_height: f32) {
        let uniforms = Uniforms {
            viewport_size: [viewport_width, viewport_height],
            scale_factor: 1.0,
            time: self.current_time,
            theme_mode: self.current_theme_mode,
            _padding: [0.0, 0.0, 0.0],
        };
        if let Some(buffer) = self.uniforms.buffer("rect_uniform") {
            self.queue
                .write_buffer(buffer, 0, bytemuck::cast_slice(&[uniforms]));
        }
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
