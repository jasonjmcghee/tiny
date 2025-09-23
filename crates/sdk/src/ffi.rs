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

/// FFI-safe shader module ID
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ShaderModuleId(pub u64);

/// FFI-safe bind group layout ID
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BindGroupLayoutId(pub u64);

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

    // Shader and pipeline creation
    pub fn gpu_create_shader_module(source: *const u8, len: usize) -> ShaderModuleId;
    pub fn gpu_create_render_pipeline_simple(
        vertex_shader: ShaderModuleId,
        fragment_shader: ShaderModuleId,
        vertex_buffer_layout: *const u8,
        layout_len: usize,
    ) -> PipelineId;

    // Advanced pipeline creation
    pub fn gpu_create_bind_group_layout(
        entries: *const u8,
        entries_len: usize,
    ) -> BindGroupLayoutId;
    pub fn gpu_create_render_pipeline_with_layout(
        vertex_shader: ShaderModuleId,
        fragment_shader: ShaderModuleId,
        bind_group_layout: BindGroupLayoutId,
        vertex_stride: u32,
        vertex_attributes: *const u8,
        attributes_len: usize,
    ) -> PipelineId;

    // Atomic render operations
    pub fn gpu_render_set_pipeline(render_pass: *mut c_void, pipeline_id: PipelineId);
    pub fn gpu_render_set_bind_group(
        render_pass: *mut c_void,
        index: u32,
        bind_group_id: BindGroupId,
    );
    pub fn gpu_render_set_vertex_buffer(
        render_pass: *mut c_void,
        slot: u32,
        buffer_id: BufferId,
    );
    pub fn gpu_render_draw(
        render_pass: *mut c_void,
        vertices: u32,
        instances: u32,
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

impl ShaderModuleId {
    pub fn create_from_wgsl(source: &str) -> Self {
        unsafe { gpu_create_shader_module(source.as_ptr(), source.len()) }
    }
}

impl BindGroupLayoutId {
    pub fn create_uniform() -> Self {
        // Create standard uniform bind group layout
        unsafe { gpu_create_bind_group_layout(std::ptr::null(), 0) }
    }
}

/// Vertex attribute descriptor for pipeline creation
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VertexAttributeDescriptor {
    pub offset: u32,
    pub location: u32,
    pub format: VertexFormat,
}

/// Vertex format enum matching wgpu
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
pub enum VertexFormat {
    Uint8 = 0,
    Uint8x2 = 1,
    Uint8x4 = 2,
    Sint8 = 3,
    Sint8x2 = 4,
    Sint8x4 = 5,
    Unorm8 = 6,
    Unorm8x2 = 7,
    Unorm8x4 = 8,
    Snorm8 = 9,
    Snorm8x2 = 10,
    Snorm8x4 = 11,
    Uint16 = 12,
    Uint16x2 = 13,
    Uint16x4 = 14,
    Sint16 = 15,
    Sint16x2 = 16,
    Sint16x4 = 17,
    Unorm16 = 18,
    Unorm16x2 = 19,
    Unorm16x4 = 20,
    Snorm16 = 21,
    Snorm16x2 = 22,
    Snorm16x4 = 23,
    Float16 = 24,
    Float16x2 = 25,
    Float16x4 = 26,
    Float32 = 27,
    Float32x2 = 28,
    Float32x3 = 29,
    Float32x4 = 30,
    Uint32 = 31,
    Uint32x2 = 32,
    Uint32x3 = 33,
    Uint32x4 = 34,
    Sint32 = 35,
    Sint32x2 = 36,
    Sint32x3 = 37,
    Sint32x4 = 38,
    Float64 = 39,
    Float64x2 = 40,
    Float64x3 = 41,
    Float64x4 = 42,
    Unorm10_10_10_2 = 43,
    Unorm8x4Bgra = 44,
}

impl PipelineId {
    pub fn create_simple(vertex_shader: ShaderModuleId, fragment_shader: ShaderModuleId) -> Self {
        // For now, pass empty layout data - the host will use defaults
        unsafe { gpu_create_render_pipeline_simple(vertex_shader, fragment_shader, std::ptr::null(), 0) }
    }

    pub fn create_with_layout(
        vertex_shader: ShaderModuleId,
        fragment_shader: ShaderModuleId,
        bind_group_layout: BindGroupLayoutId,
        vertex_stride: u32,
        attributes: &[VertexAttributeDescriptor],
    ) -> Self {
        // Serialize attributes to bytes
        let mut attr_bytes = Vec::with_capacity(attributes.len() * 12);
        for attr in attributes {
            attr_bytes.extend_from_slice(&attr.offset.to_le_bytes());
            attr_bytes.extend_from_slice(&attr.location.to_le_bytes());
            attr_bytes.extend_from_slice(&(attr.format as u32).to_le_bytes());
        }

        unsafe {
            gpu_create_render_pipeline_with_layout(
                vertex_shader,
                fragment_shader,
                bind_group_layout,
                vertex_stride,
                attr_bytes.as_ptr(),
                attr_bytes.len(),
            )
        }
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

    // Atomic render operations
    pub fn set_pipeline(&self, render_pass: &mut wgpu::RenderPass, pipeline_id: PipelineId) {
        unsafe {
            gpu_render_set_pipeline(render_pass as *mut _ as *mut c_void, pipeline_id);
        }
    }

    pub fn set_bind_group(&self, render_pass: &mut wgpu::RenderPass, index: u32, bind_group_id: BindGroupId) {
        unsafe {
            gpu_render_set_bind_group(render_pass as *mut _ as *mut c_void, index, bind_group_id);
        }
    }

    pub fn set_vertex_buffer(&self, render_pass: &mut wgpu::RenderPass, slot: u32, buffer_id: BufferId) {
        unsafe {
            gpu_render_set_vertex_buffer(render_pass as *mut _ as *mut c_void, slot, buffer_id);
        }
    }

    pub fn draw(&self, render_pass: &mut wgpu::RenderPass, vertices: u32, instances: u32) {
        unsafe {
            gpu_render_draw(render_pass as *mut _ as *mut c_void, vertices, instances);
        }
    }
}