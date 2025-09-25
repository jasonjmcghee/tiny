//! Host-side FFI implementations for GPU operations
//!
//! These functions are exported by the host binary and used by plugins at runtime.
//! They must NOT be compiled into plugins - plugins should only have extern declarations.

use tiny_core::gpu_ffi::{
    get_gpu_registry, BindGroupId, BindGroupLayoutId, BufferId, PipelineId, ShaderModuleId,
};
use wgpu::{BufferUsages, RenderPass};

/// Create a buffer
#[export_name = "gpu_create_buffer"]
pub extern "C" fn gpu_create_buffer(size: u64, usage: u32) -> BufferId {
    unsafe {
        // eprintln!("Host: gpu_create_buffer called with size={}, usage={:#x}", size, usage);
        if let Some(registry) = get_gpu_registry() {
            let id = registry.create_buffer(size, BufferUsages::from_bits_truncate(usage));
            // eprintln!("Host: Created buffer with ID {:?}", id);
            id
        } else {
            // eprintln!("Host: ERROR - GPU registry not initialized!");
            BufferId(0) // Invalid ID
        }
    }
}

/// Write data to a buffer
#[export_name = "gpu_write_buffer"]
pub extern "C" fn gpu_write_buffer(buffer_id: BufferId, offset: u64, data: *const u8, size: usize) {
    unsafe {
        // eprintln!("Host: gpu_write_buffer called for buffer {:?}, offset={}, size={}", buffer_id, offset, size);
        if let Some(registry) = get_gpu_registry() {
            let data = std::slice::from_raw_parts(data, size);

            // Debug: Print first few vertices
            if size >= 12 {
                let floats = std::slice::from_raw_parts(data.as_ptr() as *const f32, size / 4);
                // eprintln!("Host: First vertex - x={}, y={}, color={:#010x}",
                //          floats[0], floats[1],
                //          *(data.as_ptr().add(8) as *const u32));
            }

            registry.write_buffer(buffer_id, offset, data);
            // eprintln!("Host: Successfully wrote {} bytes to buffer {:?}", size, buffer_id);
        } else {
            // eprintln!("Host: ERROR - GPU registry not initialized!");
        }
    }
}

/// Draw with vertices (using host's rect pipeline)
#[export_name = "gpu_draw_vertices"]
pub extern "C" fn gpu_draw_vertices(
    render_pass: *mut RenderPass,
    pipeline_id: PipelineId,
    bind_group_id: BindGroupId,
    buffer_id: BufferId,
    vertex_count: u32,
) {
    unsafe {
        // eprintln!("Host: gpu_draw_vertices called with pipeline_id={:?}, bind_group_id={:?}, buffer_id={:?}, vertex_count={}",
        //          pipeline_id, bind_group_id, buffer_id, vertex_count);

        if let Some(registry) = get_gpu_registry() {
            // Cast the render pass pointer - this is critical for FFI
            let pass = &mut *(render_pass as *mut RenderPass<'_>);
            // eprintln!("Host: Successfully cast render pass pointer at {:p}", render_pass);

            if let Some(pipeline) = registry.get_pipeline(pipeline_id) {
                // eprintln!("Host: Found pipeline with ID {:?}, setting it", pipeline_id);
                pass.set_pipeline(&pipeline);
            } else {
                // eprintln!("Host: ERROR - Pipeline with ID {:?} not found in registry!", pipeline_id);
                return;
            }

            if let Some(bind_group) = registry.get_bind_group(bind_group_id) {
                // eprintln!("Host: Found bind group with ID {:?}, setting it", bind_group_id);
                pass.set_bind_group(0, &bind_group, &[]);
            } else {
                // eprintln!("Host: ERROR - Bind group with ID {:?} not found in registry!", bind_group_id);
                return;
            }

            if let Some(buffer) = registry.get_buffer(buffer_id) {
                // eprintln!("Host: Found buffer with ID {:?}, setting vertex buffer and drawing {} vertices", buffer_id, vertex_count);
                pass.set_vertex_buffer(0, buffer.slice(..));
                pass.draw(0..vertex_count, 0..1);
                // eprintln!("Host: Draw call issued successfully for {} vertices", vertex_count);
            } else {
                // eprintln!("Host: ERROR - Buffer with ID {:?} not found in registry!", buffer_id);
            }
        } else {
            // eprintln!("Host: ERROR - GPU registry not initialized!");
        }
    }
}

/// Create a shader module from WGSL source
#[export_name = "gpu_create_shader_module"]
pub extern "C" fn gpu_create_shader_module(source: *const u8, len: usize) -> ShaderModuleId {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            let source_str = std::str::from_utf8_unchecked(std::slice::from_raw_parts(source, len));
            registry.create_shader_module(source_str)
        } else {
            ShaderModuleId(0)
        }
    }
}

/// Simple render pipeline creation with vertex buffer layout
/// Kept for backward compatibility - plugins should use gpu_create_render_pipeline_with_layout
#[export_name = "gpu_create_render_pipeline_simple"]
pub extern "C" fn gpu_create_render_pipeline_simple(
    vertex_shader: ShaderModuleId,
    fragment_shader: ShaderModuleId,
    vertex_buffer_layout: *const u8,
    layout_len: usize,
) -> PipelineId {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            // For now, create a simple pipeline similar to the rect pipeline
            // In the future, parse the vertex_buffer_layout for custom layouts

            let vertex_shader_module = registry.get_shader_module(vertex_shader);
            let fragment_shader_module = registry.get_shader_module(fragment_shader);

            if let (Some(vs), Some(fs)) = (vertex_shader_module, fragment_shader_module) {
                use wgpu::*;

                // Create a basic uniform bind group layout (viewport uniforms)
                let uniform_layout =
                    registry
                        .device
                        .create_bind_group_layout(&BindGroupLayoutDescriptor {
                            label: Some("Plugin Pipeline Uniform Layout"),
                            entries: &[BindGroupLayoutEntry {
                                binding: 0,
                                visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                                ty: BindingType::Buffer {
                                    ty: BufferBindingType::Uniform,
                                    has_dynamic_offset: false,
                                    min_binding_size: None,
                                },
                                count: None,
                            }],
                        });

                let pipeline_layout =
                    registry
                        .device
                        .create_pipeline_layout(&PipelineLayoutDescriptor {
                            label: Some("Plugin Pipeline Layout"),
                            bind_group_layouts: &[&uniform_layout],
                            push_constant_ranges: &[],
                        });

                // For now, assume a simple vertex layout (position + color)
                // Parse vertex_buffer_layout later for custom formats
                let pipeline = registry
                    .device
                    .create_render_pipeline(&RenderPipelineDescriptor {
                        label: Some("Plugin Render Pipeline"),
                        layout: Some(&pipeline_layout),
                        vertex: VertexState {
                            module: &vs,
                            entry_point: Some("vs_main"),
                            buffers: &[VertexBufferLayout {
                                array_stride: 12, // 2 floats + 1 u32
                                step_mode: VertexStepMode::Vertex,
                                attributes: &[
                                    VertexAttribute {
                                        offset: 0,
                                        shader_location: 0,
                                        format: VertexFormat::Float32x2,
                                    },
                                    VertexAttribute {
                                        offset: 8,
                                        shader_location: 1,
                                        format: VertexFormat::Uint32,
                                    },
                                ],
                            }],
                            compilation_options: PipelineCompilationOptions::default(),
                        },
                        fragment: Some(FragmentState {
                            module: &fs,
                            entry_point: Some("fs_main"),
                            targets: &[Some(ColorTargetState {
                                format: TextureFormat::Bgra8Unorm, // Should match surface format
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
                    });

                registry.register_pipeline(pipeline)
            } else {
                PipelineId(0)
            }
        } else {
            PipelineId(0)
        }
    }
}

/// Create a bind group layout
#[export_name = "gpu_create_bind_group_layout"]
pub extern "C" fn gpu_create_bind_group_layout(
    entries: *const u8,
    _entries_len: usize,
) -> BindGroupLayoutId {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            use wgpu::*;

            // For now, create standard uniform layout
            // TODO: Parse entries for custom layouts
            let layout = registry
                .device
                .create_bind_group_layout(&BindGroupLayoutDescriptor {
                    label: Some("Plugin Bind Group Layout"),
                    entries: &[BindGroupLayoutEntry {
                        binding: 0,
                        visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    }],
                });

            registry.register_bind_group_layout(layout)
        } else {
            BindGroupLayoutId(0)
        }
    }
}

/// Create a render pipeline with custom vertex layout
#[export_name = "gpu_create_render_pipeline_with_layout"]
pub extern "C" fn gpu_create_render_pipeline_with_layout(
    vertex_shader: ShaderModuleId,
    fragment_shader: ShaderModuleId,
    bind_group_layout: BindGroupLayoutId,
    vertex_stride: u32,
    vertex_attributes: *const u8,
    attributes_len: usize,
) -> PipelineId {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            let vertex_shader_module = registry.get_shader_module(vertex_shader);
            let fragment_shader_module = registry.get_shader_module(fragment_shader);
            let bind_layout = registry.get_bind_group_layout(bind_group_layout);

            if let (Some(vs), Some(fs), Some(layout)) =
                (vertex_shader_module, fragment_shader_module, bind_layout)
            {
                use wgpu::*;

                // Parse vertex attributes from buffer
                // Format: [offset: u32, location: u32, format: u32] repeated
                let attr_slice = std::slice::from_raw_parts(vertex_attributes, attributes_len);
                let mut attributes = Vec::new();
                let mut i = 0;
                while i + 12 <= attributes_len {
                    let offset = u32::from_le_bytes([
                        attr_slice[i],
                        attr_slice[i + 1],
                        attr_slice[i + 2],
                        attr_slice[i + 3],
                    ]);
                    let location = u32::from_le_bytes([
                        attr_slice[i + 4],
                        attr_slice[i + 5],
                        attr_slice[i + 6],
                        attr_slice[i + 7],
                    ]);
                    let format_id = u32::from_le_bytes([
                        attr_slice[i + 8],
                        attr_slice[i + 9],
                        attr_slice[i + 10],
                        attr_slice[i + 11],
                    ]);

                    // Map format ID to VertexFormat (matching wgpu enum values)
                    let format = match format_id {
                        0 => VertexFormat::Uint8,
                        1 => VertexFormat::Uint8x2,
                        2 => VertexFormat::Uint8x4,
                        3 => VertexFormat::Sint8,
                        4 => VertexFormat::Sint8x2,
                        5 => VertexFormat::Sint8x4,
                        6 => VertexFormat::Unorm8,
                        7 => VertexFormat::Unorm8x2,
                        8 => VertexFormat::Unorm8x4,
                        9 => VertexFormat::Snorm8,
                        10 => VertexFormat::Snorm8x2,
                        11 => VertexFormat::Snorm8x4,
                        12 => VertexFormat::Uint16,
                        13 => VertexFormat::Uint16x2,
                        14 => VertexFormat::Uint16x4,
                        15 => VertexFormat::Sint16,
                        16 => VertexFormat::Sint16x2,
                        17 => VertexFormat::Sint16x4,
                        18 => VertexFormat::Unorm16,
                        19 => VertexFormat::Unorm16x2,
                        20 => VertexFormat::Unorm16x4,
                        21 => VertexFormat::Snorm16,
                        22 => VertexFormat::Snorm16x2,
                        23 => VertexFormat::Snorm16x4,
                        24 => VertexFormat::Float16,
                        25 => VertexFormat::Float16x2,
                        26 => VertexFormat::Float16x4,
                        27 => VertexFormat::Float32,
                        28 => VertexFormat::Float32x2,
                        29 => VertexFormat::Float32x3,
                        30 => VertexFormat::Float32x4,
                        31 => VertexFormat::Uint32,
                        32 => VertexFormat::Uint32x2,
                        33 => VertexFormat::Uint32x3,
                        34 => VertexFormat::Uint32x4,
                        35 => VertexFormat::Sint32,
                        36 => VertexFormat::Sint32x2,
                        37 => VertexFormat::Sint32x3,
                        38 => VertexFormat::Sint32x4,
                        39 => VertexFormat::Float64,
                        40 => VertexFormat::Float64x2,
                        41 => VertexFormat::Float64x3,
                        42 => VertexFormat::Float64x4,
                        43 => VertexFormat::Unorm10_10_10_2,
                        44 => VertexFormat::Unorm8x4Bgra,
                        _ => VertexFormat::Float32x2, // Default
                    };

                    attributes.push(VertexAttribute {
                        offset: offset as u64,
                        shader_location: location,
                        format,
                    });

                    i += 12;
                }

                let pipeline_layout =
                    registry
                        .device
                        .create_pipeline_layout(&PipelineLayoutDescriptor {
                            label: Some("Plugin Custom Pipeline Layout"),
                            bind_group_layouts: &[&layout],
                            push_constant_ranges: &[],
                        });

                let pipeline = registry
                    .device
                    .create_render_pipeline(&RenderPipelineDescriptor {
                        label: Some("Plugin Custom Render Pipeline"),
                        layout: Some(&pipeline_layout),
                        vertex: VertexState {
                            module: &vs,
                            entry_point: Some("vs_main"),
                            buffers: &[VertexBufferLayout {
                                array_stride: vertex_stride as u64,
                                step_mode: VertexStepMode::Vertex,
                                attributes: &attributes,
                            }],
                            compilation_options: PipelineCompilationOptions::default(),
                        },
                        fragment: Some(FragmentState {
                            module: &fs,
                            entry_point: Some("fs_main"),
                            targets: &[Some(ColorTargetState {
                                format: TextureFormat::Bgra8Unorm,
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
                    });

                registry.register_pipeline(pipeline)
            } else {
                PipelineId(0)
            }
        } else {
            PipelineId(0)
        }
    }
}

/// Set pipeline for rendering
#[export_name = "gpu_render_set_pipeline"]
pub extern "C" fn gpu_render_set_pipeline(render_pass: *mut RenderPass, pipeline_id: PipelineId) {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            let pass = &mut *(render_pass as *mut RenderPass<'_>);
            if let Some(pipeline) = registry.get_pipeline(pipeline_id) {
                pass.set_pipeline(&pipeline);
            }
        }
    }
}

/// Set bind group for rendering
#[export_name = "gpu_render_set_bind_group"]
pub extern "C" fn gpu_render_set_bind_group(
    render_pass: *mut RenderPass,
    index: u32,
    bind_group_id: BindGroupId,
) {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            let pass = &mut *(render_pass as *mut RenderPass<'_>);
            if let Some(bind_group) = registry.get_bind_group(bind_group_id) {
                pass.set_bind_group(index, &bind_group, &[]);
            }
        }
    }
}

/// Set vertex buffer for rendering
#[export_name = "gpu_render_set_vertex_buffer"]
pub extern "C" fn gpu_render_set_vertex_buffer(
    render_pass: *mut RenderPass,
    slot: u32,
    buffer_id: BufferId,
) {
    unsafe {
        if let Some(registry) = get_gpu_registry() {
            let pass = &mut *(render_pass as *mut RenderPass<'_>);
            if let Some(buffer) = registry.get_buffer(buffer_id) {
                pass.set_vertex_buffer(slot, buffer.slice(..));
            }
        }
    }
}

/// Draw vertices
#[export_name = "gpu_render_draw"]
pub extern "C" fn gpu_render_draw(render_pass: *mut RenderPass, vertices: u32, instances: u32) {
    unsafe {
        let pass = &mut *(render_pass as *mut RenderPass<'_>);
        pass.draw(0..vertices, 0..instances);
    }
}

/// Initialize function to ensure symbols are exported
pub fn init_ffi() {
    // eprintln!("Host FFI functions initialized");
    // Force linker to include FFI functions by taking their addresses
    // This prevents dead code elimination
    let _ = gpu_create_buffer as *const ();
    let _ = gpu_write_buffer as *const ();
    let _ = gpu_draw_vertices as *const ();
    let _ = gpu_create_shader_module as *const ();
    let _ = gpu_create_render_pipeline_simple as *const ();
    let _ = gpu_create_bind_group_layout as *const ();
    let _ = gpu_create_render_pipeline_with_layout as *const ();
    let _ = gpu_render_set_pipeline as *const ();
    let _ = gpu_render_set_bind_group as *const ();
    let _ = gpu_render_set_vertex_buffer as *const ();
    let _ = gpu_render_draw as *const ();
    // DO NOT DELETE
    eprintln!("FFI function addresses: create={:p}, write={:p}, draw={:p}, shader={:p}, pipeline={:p}, bind_layout={:p}, pipeline_layout={:p}, set_pipeline={:p}, set_bind_group={:p}, set_vertex_buffer={:p}, draw={:p}",
                gpu_create_buffer as *const (),
                gpu_write_buffer as *const (),
                gpu_draw_vertices as *const (),
                gpu_create_shader_module as *const (),
                gpu_create_render_pipeline_simple as *const (),
                gpu_create_bind_group_layout as *const (),
                gpu_create_render_pipeline_with_layout as *const (),
                gpu_render_set_pipeline as *const (),
                gpu_render_set_bind_group as *const (),
                gpu_render_set_vertex_buffer as *const (),
                gpu_render_draw as *const (),
    );
}
