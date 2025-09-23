//! Core plugin traits

// === Core Plugin Traits ===

use crate::LogicalSize;

/// One-time initialization when plugin is loaded
pub trait Initializable {
    /// Called once when the plugin is first loaded
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError>;

    /// Called before plugin is unloaded (optional)
    fn cleanup(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

/// Per-frame logic updates (animations, state changes, etc.)
pub trait Updatable {
    /// Called every frame with time delta
    fn update(&mut self, dt: f32, ctx: &mut UpdateContext) -> Result<(), PluginError>;
}

/// Configuration management for plugins
pub trait Configurable {
    /// Called when configuration file changes
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError>;

    /// Get current configuration as TOML string (optional)
    fn get_config(&self) -> Option<String> {
        None
    }
}

/// Per-frame rendering
/// TODO: We should make ComputeShaders not require viewport/bounds
pub trait Paintable {
    /// Called every frame to render visual output
    fn paint(&self, ctx: &PaintContext, pass: &mut wgpu::RenderPass);
    fn z_index(&self) -> i32 {
        0
    }
}

/// Expose functionality to other plugins
pub trait Library {
    /// Library name
    fn name(&self) -> &str;

    /// Call a method with arguments (mutable for state changes)
    fn call(&mut self, method: &str, args: &[u8]) -> Result<Vec<u8>, PluginError>;
}

/// Transform data flowing through the system (type-safe event bus)
///
/// This replaces TextStyleProvider and similar patterns - plugins can
/// hook into data flows to transform them (e.g., add syntax colors to glyphs)
pub trait Hook<T> {
    /// The output type after transformation
    type Output;

    /// Process the input and return transformed output
    fn process(&self, input: T) -> Self::Output;
}

// === Context Types ===

/// Context provided during plugin setup
pub struct SetupContext {
    /// GPU device for resource creation
    pub device: std::sync::Arc<wgpu::Device>,
    /// GPU queue for command submission
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// Plugin registry for discovering other plugins
    pub registry: PluginRegistry,
}

/// Context provided during update
pub struct UpdateContext {
    /// Access to other plugins' libraries
    pub registry: PluginRegistry,
    /// Current frame number
    pub frame: u64,
    /// Total elapsed time
    pub elapsed: f32,
}

/// FFI-safe function pointer for drawing rect vertices
pub type DrawRectVerticesFn = unsafe extern "C" fn(
    pass: *mut wgpu::RenderPass,
    vertices: *const u8,
    vertices_len: usize,
    count: u32,
);

/// Context provided during paint - with full GPU access
pub struct PaintContext {
    /// GPU device
    pub device: std::sync::Arc<wgpu::Device>,
    /// GPU queue
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// Viewport information for coordinate transformations
    pub viewport: crate::types::ViewportInfo,
    /// Widget-specific viewport (if rendering within a widget)
    pub widget_viewport: Option<crate::types::WidgetViewport>,
    /// Raw GPU renderer pointer for pipeline/buffer access
    /// This gives plugins full GPU power - they can create pipelines, upload buffers, etc.
    pub gpu_renderer: *mut std::ffi::c_void,
    /// Function pointer to draw rect vertices (FFI-safe)
    pub draw_rect_vertices_fn: Option<DrawRectVerticesFn>,
    /// Any additional context data plugins might need
    /// This is opaque to the SDK but the runtime knows what it is
    pub context_data: *const std::ffi::c_void,

    pub gpu_context: Option<crate::ffi::PluginGpuContext>,
}

impl PaintContext {
    /// Create a new PaintContext with a service registry
    /// Note: The registry must outlive the PaintContext
    pub fn new(
        viewport: crate::types::ViewportInfo,
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        gpu_renderer: *mut std::ffi::c_void,
        registry: &crate::services::ServiceRegistry,
    ) -> Self {
        // eprintln!("Creating PaintContext with device: {:p}, registry pointer: {:p}", &device, registry);
        Self {
            viewport,
            widget_viewport: None,
            device,
            queue,
            gpu_renderer,
            draw_rect_vertices_fn: None, // Will be set by caller if needed
            context_data: registry as *const _ as *const std::ffi::c_void,
            gpu_context: None,
        }
    }

    /// Create a new PaintContext with widget viewport
    pub fn with_widget_viewport(mut self, widget_viewport: crate::types::WidgetViewport) -> Self {
        self.widget_viewport = Some(widget_viewport);
        self
    }

    /// Get the service registry from context data
    ///
    /// # Safety
    /// Caller must ensure the context_data pointer is valid and points to a ServiceRegistry
    pub unsafe fn services(&self) -> &crate::services::ServiceRegistry {
        (&*(self.context_data as *const crate::services::ServiceRegistry)) as _
    }
}

// === Plugin Registry ===

/// Registry for plugin discovery and type-safe API access
#[derive(Clone)]
pub struct PluginRegistry {
    // Implementation will be in core, this is just the interface
    pub _private: (),
}

impl PluginRegistry {
    /// Create empty registry (for now)
    pub fn empty() -> Self {
        Self { _private: () }
    }

    /// Get a plugin's Library API by type
    pub fn get<T: 'static>(&self) -> Option<&T> {
        // Core will implement actual lookup
        None
    }

    /// Check if a plugin provides a specific API type
    pub fn has<T: 'static>(&self) -> bool {
        self.get::<T>().is_some()
    }
}

// === Error Handling ===

/// Plugin error type
#[derive(Debug)]
pub enum PluginError {
    /// Setup failed
    InitializeFailed(String),
    /// Update failed
    UpdateFailed(String),
    /// Paint failed
    PaintFailed(String),
    /// Missing dependency
    MissingDependency(String),
    /// Generic error
    Other(Box<dyn std::error::Error>),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::InitializeFailed(msg) => write!(f, "Setup failed: {}", msg),
            PluginError::UpdateFailed(msg) => write!(f, "Update failed: {}", msg),
            PluginError::PaintFailed(msg) => write!(f, "Paint failed: {}", msg),
            PluginError::MissingDependency(dep) => write!(f, "Missing dependency: {}", dep),
            PluginError::Other(err) => write!(f, "Plugin error: {}", err),
        }
    }
}

impl std::error::Error for PluginError {}

// === Plugin Metadata ===

/// Plugin capabilities for dependency resolution
#[derive(Debug, Clone)]
pub enum Capability {
    Initializable,
    /// Plugin can paint
    Paintable(String),
    /// Plugin provides a library API
    Library(std::any::TypeId),
    /// Plugin hooks into a data type
    Hook(std::any::TypeId),
    /// Plugin needs updates
    Updatable,
}

/// Base plugin trait for metadata
pub trait Plugin: Send + Sync {
    /// Plugin name
    fn name(&self) -> &str;

    /// Plugin version
    fn version(&self) -> &str;

    /// Declare plugin capabilities
    fn capabilities(&self) -> Vec<Capability>;

    /// Get Setup trait if implemented
    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        None
    }

    /// Get Update trait if implemented
    fn as_updatable(&mut self) -> Option<&mut dyn Updatable> {
        None
    }

    /// Get Paint trait if implemented
    fn as_paintable(&self) -> Option<&dyn Paintable> {
        None
    }

    /// Get Hook for GlyphInstances if implemented
    fn as_glyph_hook(
        &self,
    ) -> Option<&dyn Hook<crate::GlyphInstances, Output = crate::GlyphInstances>> {
        None
    }

    /// Get mutable Library trait if implemented
    fn as_library_mut(&mut self) -> Option<&mut dyn Library> {
        None
    }

    /// Get Configurable trait if implemented
    fn as_configurable(&mut self) -> Option<&mut dyn Configurable> {
        None
    }
}

// Something that has some size
pub trait Spatial: Paintable {
    fn measure(&self) -> LogicalSize;
}
