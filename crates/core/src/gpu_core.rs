//! Minimal GPU Core - Just holds the wgpu-core Global and IDs
//!
//! This is passed to plugins so they can use wgpu-core directly.

use std::sync::Arc;
use wgpu_core::{
    global::Global,
    id::{AdapterId, DeviceId, QueueId, SurfaceId},
};

/// Holds the wgpu-core Global instance and resource IDs
pub struct GpuCore {
    /// The wgpu-core global instance
    pub global: Arc<Global>,

    /// Adapter ID
    pub adapter_id: AdapterId,

    /// Device ID
    pub device_id: DeviceId,

    /// Queue ID
    pub queue_id: QueueId,

    /// Surface ID (if using a window)
    pub surface_id: Option<SurfaceId>,
}

impl GpuCore {
    /// Create from existing wgpu instance
    ///
    /// NOTE: In production, we would extract the actual Global and IDs
    /// from the wgpu instance. For now, we create a new Global.
    pub fn new() -> Self {
        // Create the global instance
        let global = Arc::new(Global::new(
            "wgpu",
            wgpu_types::InstanceDescriptor {
                backends: wgpu_types::Backends::PRIMARY,
                flags: wgpu_types::InstanceFlags::from_build_config(),
                dx12_shader_compiler: wgpu_types::Dx12Compiler::Fxc,
                gles_minor_version: wgpu_types::Gles3MinorVersion::Automatic,
            },
        ));

        // These would be extracted from the actual wgpu device/queue
        // For now, use the first device (index 0, backend 1)
        let adapter_id = AdapterId::zip(0, 1, wgpu_core::identity::Backend::Empty);
        let device_id = DeviceId::zip(0, 1, wgpu_core::identity::Backend::Empty);
        let queue_id = QueueId::zip(0, 1, wgpu_core::identity::Backend::Empty);

        Self {
            global,
            adapter_id,
            device_id,
            queue_id,
            surface_id: None,
        }
    }

    /// Get a raw pointer to the Global that can be passed to plugins
    pub fn global_ptr(&self) -> *const Global {
        Arc::as_ptr(&self.global)
    }

    /// Convert device ID to raw u64 for FFI
    pub fn device_id_raw(&self) -> u64 {
        let (index, epoch, _backend) = self.device_id.unzip();
        ((index as u64) << 32) | (epoch as u64)
    }

    /// Convert queue ID to raw u64 for FFI
    pub fn queue_id_raw(&self) -> u64 {
        let (index, epoch, _backend) = self.queue_id.unzip();
        ((index as u64) << 32) | (epoch as u64)
    }
}