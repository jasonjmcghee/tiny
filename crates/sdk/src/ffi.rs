//! FFI-safe types for plugin GPU access

use std::ffi::c_void;

/// FFI-safe buffer ID
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BufferId(pub u64);

/// FFI-safe pipeline ID
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PipelineId(pub u64);

/// FFI-safe bind group ID
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BindGroupId(pub u64);

/// Plugin GPU context with FFI-safe IDs
#[repr(C)]
pub struct PluginGpuContext {
    pub rect_pipeline_id: PipelineId,
    pub uniform_bind_group_id: BindGroupId,
    pub render_pass: *mut c_void,
}

// External C functions that plugins call
// These are provided by the host at runtime
extern "C" {
    pub fn gpu_create_buffer(size: u64, usage: u32) -> BufferId;
    pub fn gpu_write_buffer(buffer_id: BufferId, offset: u64, data: *const u8, size: usize);
    pub fn gpu_draw_vertices(
        render_pass: *mut c_void,
        pipeline_id: PipelineId,
        bind_group_id: BindGroupId,
        buffer_id: BufferId,
        vertex_count: u32,
    );
}

// Safe wrappers for plugins to use
impl BufferId {
    pub fn create(size: u64, usage: wgpu::BufferUsages) -> Self {
        unsafe { gpu_create_buffer(size, usage.bits()) }
    }

    pub fn write(&self, offset: u64, data: &[u8]) {
        unsafe { gpu_write_buffer(*self, offset, data.as_ptr(), data.len()) }
    }
}

impl PluginGpuContext {
    pub fn draw_vertices(&self, render_pass: &mut wgpu::RenderPass, buffer_id: BufferId, vertex_count: u32) {
        unsafe {
            gpu_draw_vertices(
                render_pass as *mut _ as *mut c_void,
                self.rect_pipeline_id,
                self.uniform_bind_group_id,
                buffer_id,
                vertex_count,
            );
        }
    }
}