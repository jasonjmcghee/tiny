//! Host-side FFI implementations for GPU operations
//!
//! These functions are exported by the host binary and used by plugins at runtime.
//! They must NOT be compiled into plugins - plugins should only have extern declarations.

use tiny_core::gpu_ffi::{
    get_gpu_registry, BindGroupId, BindGroupLayoutId, BufferId, PipelineId, PluginGpuContext,
    ShaderModuleId,
};
use wgpu::hal::DynDevice;
use wgpu::{BufferUsages, RenderPass};

/// Create a buffer
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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

/// Set pipeline for rendering
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
#[no_mangle]
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
    unsafe {
        let _ = gpu_create_buffer as *const ();
        let _ = gpu_write_buffer as *const ();
        let _ = gpu_draw_vertices as *const ();
        let _ = gpu_create_shader_module as *const ();
        let _ = gpu_create_render_pipeline_simple as *const ();
        let _ = gpu_render_set_pipeline as *const ();
        let _ = gpu_render_set_bind_group as *const ();
        let _ = gpu_render_set_vertex_buffer as *const ();
        let _ = gpu_render_draw as *const ();
        // DO NOT DELETE
        eprintln!("FFI function addresses: create={:p}, write={:p}, draw={:p}, shader={:p}, pipeline={:p}, set_pipeline={:p}, set_bind_group={:p}, set_vertex_buffer={:p}, draw={:p}",
                 gpu_create_buffer as *const (),
                 gpu_write_buffer as *const (),
                 gpu_draw_vertices as *const (),
                 gpu_create_shader_module as *const (),
                 gpu_create_render_pipeline_simple as *const (),
                 gpu_render_set_pipeline as *const (),
                 gpu_render_set_bind_group as *const (),
                 gpu_render_set_vertex_buffer as *const (),
                 gpu_render_draw as *const (),
        );
    }
}

