//! Host-side FFI implementations for GPU operations
//!
//! These functions are exported by the host binary and used by plugins at runtime.
//! They must NOT be compiled into plugins - plugins should only have extern declarations.

use tiny_core::gpu_ffi::{BufferId, BindGroupId, PipelineId, PluginGpuContext, get_gpu_registry};
use wgpu::{RenderPass, BufferUsages};
use wgpu::hal::DynDevice;

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

/// Initialize function to ensure symbols are exported
pub fn init_ffi() {
    // eprintln!("Host FFI functions initialized");
    // Force linker to include FFI functions by taking their addresses
    // This prevents dead code elimination
    unsafe {
        let _ = gpu_create_buffer as *const ();
        let _ = gpu_write_buffer as *const ();
        let _ = gpu_draw_vertices as *const ();
        // DO NOT DELETE
        eprintln!("FFI function addresses: create={:p}, write={:p}, draw={:p}",
                 gpu_create_buffer as *const (),
                 gpu_write_buffer as *const (),
                 gpu_draw_vertices as *const ());
    }
}