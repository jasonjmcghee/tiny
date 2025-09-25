//! FFI-safe GPU API using ID-based handles
//!
//! Instead of passing wgpu objects across FFI (which corrupts them),
//! we pass integer IDs and maintain a registry on the host side.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use wgpu::*;

pub use tiny_sdk::ffi::{
    BindGroupId, BindGroupLayoutId, BufferId, PipelineId, PluginGpuContext, ShaderModuleId,
};

/// GPU resource registry - maintains the actual wgpu objects
pub struct GpuRegistry {
    next_id: AtomicU64,

    // The actual wgpu objects, indexed by ID
    buffers: RwLock<HashMap<u64, Buffer>>,
    pipelines: RwLock<HashMap<u64, RenderPipeline>>,
    bind_groups: RwLock<HashMap<u64, BindGroup>>,
    bind_group_layouts: RwLock<HashMap<u64, BindGroupLayout>>,
    shader_modules: RwLock<HashMap<u64, ShaderModule>>,

    // Core objects (not created by plugins, just referenced)
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
}

impl GpuRegistry {
    pub fn new(device: Arc<Device>, queue: Arc<Queue>) -> Self {
        Self {
            next_id: AtomicU64::new(1), // Start at 1, 0 means invalid
            buffers: RwLock::new(HashMap::new()),
            pipelines: RwLock::new(HashMap::new()),
            bind_groups: RwLock::new(HashMap::new()),
            bind_group_layouts: RwLock::new(HashMap::new()),
            shader_modules: RwLock::new(HashMap::new()),
            device,
            queue,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn create_buffer(&self, size: u64, usage: BufferUsages) -> BufferId {
        let buffer = self.device.create_buffer(&BufferDescriptor {
            label: Some("Plugin Buffer"),
            size,
            usage,
            mapped_at_creation: false,
        });

        let id = self.next_id();
        self.buffers.write().unwrap().insert(id, buffer);
        BufferId(id)
    }

    pub fn write_buffer(&self, buffer_id: BufferId, offset: u64, data: &[u8]) {
        if let Some(buffer) = self.buffers.read().unwrap().get(&buffer_id.0) {
            self.queue.write_buffer(buffer, offset, data);
        }
    }

    pub fn get_buffer(&self, id: BufferId) -> Option<Buffer> {
        self.buffers.read().unwrap().get(&id.0).cloned()
    }

    pub fn create_render_pipeline(&self, desc: &RenderPipelineDescriptor) -> PipelineId {
        let pipeline = self.device.create_render_pipeline(desc);
        let id = self.next_id();
        self.pipelines.write().unwrap().insert(id, pipeline);
        PipelineId(id)
    }

    pub fn get_pipeline(&self, id: PipelineId) -> Option<RenderPipeline> {
        self.pipelines.read().unwrap().get(&id.0).cloned()
    }

    // For existing resources from the host (like rect pipeline)
    pub fn register_pipeline(&self, pipeline: RenderPipeline) -> PipelineId {
        let id = self.next_id();
        self.pipelines.write().unwrap().insert(id, pipeline);
        PipelineId(id)
    }

    pub fn register_bind_group(&self, bind_group: BindGroup) -> BindGroupId {
        let id = self.next_id();
        self.bind_groups.write().unwrap().insert(id, bind_group);
        BindGroupId(id)
    }

    pub fn get_bind_group(&self, id: BindGroupId) -> Option<BindGroup> {
        self.bind_groups.read().unwrap().get(&id.0).cloned()
    }

    // Shader module management
    pub fn create_shader_module(&self, source: &str) -> ShaderModuleId {
        let shader = self.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Plugin Shader"),
            source: ShaderSource::Wgsl(source.into()),
        });
        let id = self.next_id();
        self.shader_modules.write().unwrap().insert(id, shader);
        ShaderModuleId(id)
    }

    pub fn get_shader_module(&self, id: ShaderModuleId) -> Option<ShaderModule> {
        self.shader_modules.read().unwrap().get(&id.0).cloned()
    }

    // Bind group layout management
    pub fn register_bind_group_layout(&self, layout: BindGroupLayout) -> BindGroupLayoutId {
        let id = self.next_id();
        self.bind_group_layouts.write().unwrap().insert(id, layout);
        BindGroupLayoutId(id)
    }

    pub fn get_bind_group_layout(&self, id: BindGroupLayoutId) -> Option<BindGroupLayout> {
        self.bind_group_layouts.read().unwrap().get(&id.0).cloned()
    }
}

/// Global registry instance - this is what plugins interact with
static mut GPU_REGISTRY: Option<Arc<GpuRegistry>> = None;

/// Initialize the global registry (called by host once)
pub unsafe fn init_gpu_registry(device: Arc<Device>, queue: Arc<Queue>) -> Arc<GpuRegistry> {
    let registry = Arc::new(GpuRegistry::new(device, queue));
    GPU_REGISTRY = Some(registry.clone());
    registry
}

/// Get the global registry
pub unsafe fn get_gpu_registry() -> Option<Arc<GpuRegistry>> {
    GPU_REGISTRY.clone()
}

// NOTE: The C API implementations are in the host binary (crates/legacy/src/gpu_ffi_host.rs)
// This file only contains the registry types and management.
// Plugins will link to the host's implementations at runtime.
