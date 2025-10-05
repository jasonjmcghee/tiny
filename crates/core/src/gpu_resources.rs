//! GPU resource management abstractions to reduce boilerplate

use ahash::HashMap;
use wgpu::*;

/// Manages named GPU buffers with automatic creation
pub struct BufferRegistry {
    pub buffers: HashMap<String, Buffer>,
    device: std::sync::Arc<Device>,
}

impl BufferRegistry {
    pub fn new(device: std::sync::Arc<Device>) -> Self {
        Self {
            buffers: HashMap::default(),
            device,
        }
    }

    pub fn create_or_get(&mut self, name: &str, size: u64, usage: BufferUsages) -> &Buffer {
        self.buffers.entry(name.to_string()).or_insert_with(|| {
            self.device.create_buffer(&BufferDescriptor {
                label: Some(name),
                size,
                usage,
                mapped_at_creation: false,
            })
        })
    }

    pub fn get(&self, name: &str) -> Option<&Buffer> {
        self.buffers.get(name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Buffer> {
        self.buffers.get_mut(name)
    }
}

/// Manages uniform buffer + bind group pairs
pub struct UniformManager {
    entries: HashMap<String, UniformEntry>,
    device: std::sync::Arc<Device>,
}

struct UniformEntry {
    buffer: Buffer,
    bind_group: BindGroup,
    layout: BindGroupLayout,
}

impl UniformManager {
    pub fn new(device: std::sync::Arc<Device>) -> Self {
        Self {
            entries: HashMap::default(),
            device,
        }
    }

    pub fn create(
        &mut self,
        name: &str,
        size: u64,
        visibility: ShaderStages,
    ) -> (&Buffer, &BindGroup, &BindGroupLayout) {
        let buffer = self.device.create_buffer(&BufferDescriptor {
            label: Some(&format!("{} Buffer", name)),
            size,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = self
            .device
            .create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: Some(&format!("{} Layout", name)),
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
            });

        let bind_group = self.device.create_bind_group(&BindGroupDescriptor {
            label: Some(&format!("{} Bind Group", name)),
            layout: &layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        let entry = UniformEntry {
            buffer,
            bind_group,
            layout,
        };

        self.entries.insert(name.to_string(), entry);
        let e = self.entries.get(name).unwrap();
        (&e.buffer, &e.bind_group, &e.layout)
    }

    pub fn get(&self, name: &str) -> Option<(&Buffer, &BindGroup, &BindGroupLayout)> {
        self.entries
            .get(name)
            .map(|e| (&e.buffer, &e.bind_group, &e.layout))
    }

    pub fn buffer(&self, name: &str) -> Option<&Buffer> {
        self.entries.get(name).map(|e| &e.buffer)
    }

    pub fn bind_group(&self, name: &str) -> Option<&BindGroup> {
        self.entries.get(name).map(|e| &e.bind_group)
    }

    pub fn layout(&self, name: &str) -> Option<&BindGroupLayout> {
        self.entries.get(name).map(|e| &e.layout)
    }
}

/// Helper to create bind group layouts with less boilerplate
pub struct BindGroupLayoutBuilder<'a> {
    device: &'a Device,
    label: Option<String>,
    entries: Vec<BindGroupLayoutEntry>,
}

impl<'a> BindGroupLayoutBuilder<'a> {
    pub fn new(device: &'a Device) -> Self {
        Self {
            device,
            label: None,
            entries: Vec::new(),
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn uniform(mut self, binding: u32, visibility: ShaderStages) -> Self {
        self.entries.push(BindGroupLayoutEntry {
            binding,
            visibility,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        self
    }

    pub fn texture(
        mut self,
        binding: u32,
        visibility: ShaderStages,
        view_dimension: TextureViewDimension,
    ) -> Self {
        self.entries.push(BindGroupLayoutEntry {
            binding,
            visibility,
            ty: BindingType::Texture {
                sample_type: TextureSampleType::Float { filterable: true },
                view_dimension,
                multisampled: false,
            },
            count: None,
        });
        self
    }

    pub fn sampler(mut self, binding: u32, visibility: ShaderStages) -> Self {
        self.entries.push(BindGroupLayoutEntry {
            binding,
            visibility,
            ty: BindingType::Sampler(SamplerBindingType::Filtering),
            count: None,
        });
        self
    }

    pub fn storage(mut self, binding: u32, visibility: ShaderStages, read_only: bool) -> Self {
        self.entries.push(BindGroupLayoutEntry {
            binding,
            visibility,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
        self
    }

    pub fn build(self) -> BindGroupLayout {
        self.device
            .create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: self.label.as_deref(),
                entries: &self.entries,
            })
    }
}

/// Enhanced pipeline builder with more fluent API
pub struct PipelineBuilder<'a> {
    device: &'a Device,
    format: TextureFormat,
    label: Option<String>,
    shader: Option<&'a ShaderModule>,
    bind_group_layouts: Vec<&'a BindGroupLayout>,
    vertex_buffers: Vec<VertexBufferLayout<'a>>,
}

impl<'a> PipelineBuilder<'a> {
    pub fn new(device: &'a Device, format: TextureFormat) -> Self {
        Self {
            device,
            format,
            label: None,
            shader: None,
            bind_group_layouts: Vec::new(),
            vertex_buffers: Vec::new(),
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn shader(mut self, shader: &'a ShaderModule) -> Self {
        self.shader = Some(shader);
        self
    }

    pub fn bind_group_layout(mut self, layout: &'a BindGroupLayout) -> Self {
        self.bind_group_layouts.push(layout);
        self
    }

    pub fn vertex_buffer(
        mut self,
        stride: BufferAddress,
        step_mode: VertexStepMode,
        attributes: &'a [VertexAttribute],
    ) -> Self {
        self.vertex_buffers.push(VertexBufferLayout {
            array_stride: stride,
            step_mode,
            attributes,
        });
        self
    }

    pub fn build(self) -> RenderPipeline {
        let shader = self.shader.expect("Shader not set");

        let layout = self
            .device
            .create_pipeline_layout(&PipelineLayoutDescriptor {
                label: self
                    .label
                    .as_ref()
                    .map(|l| format!("{} Layout", l))
                    .as_deref(),
                bind_group_layouts: &self.bind_group_layouts,
                push_constant_ranges: &[],
            });

        self.device
            .create_render_pipeline(&RenderPipelineDescriptor {
                label: self.label.as_deref(),
                layout: Some(&layout),
                vertex: VertexState {
                    module: shader,
                    entry_point: Some("vs_main"),
                    buffers: &self.vertex_buffers,
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

/// Helper for scissor rect calculations
pub struct ScissorRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ScissorRect {
    /// Create scissor rect from logical bounds with scale factor
    /// Adds optional margin and clamps to render target size
    pub fn from_logical_bounds(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        scale_factor: f32,
        margin: f32,
        target_width: u32,
        target_height: u32,
    ) -> Self {
        let scissor_x = ((x - margin) * scale_factor).round().max(0.0) as u32;
        let scissor_y = ((y - margin) * scale_factor).round().max(0.0) as u32;
        let scissor_w = ((width + margin * 2.0) * scale_factor).round().max(1.0) as u32;
        let scissor_h = ((height + margin * 2.0) * scale_factor).round().max(1.0) as u32;

        // Clamp to render target bounds
        let scissor_w = scissor_w.min(target_width.saturating_sub(scissor_x));
        let scissor_h = scissor_h.min(target_height.saturating_sub(scissor_y));

        Self {
            x: scissor_x,
            y: scissor_y,
            width: scissor_w,
            height: scissor_h,
        }
    }

    pub fn apply(&self, pass: &mut RenderPass) {
        pass.set_scissor_rect(self.x, self.y, self.width, self.height);
    }
}
