//! Core plugin traits

// === Core Plugin Traits ===

use crate::LogicalSize;

/// One-time initialization when plugin is loaded
pub trait Initializable {
    /// Called once when the plugin is first loaded
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError>;
}

/// Per-frame logic updates (animations, state changes, etc.)
pub trait Updatable {
    /// Called every frame with time delta
    fn update(&mut self, dt: f32, ctx: &mut UpdateContext) -> Result<(), PluginError>;
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
    /// The type-safe API this plugin exposes
    type API;

    /// Get reference to this plugin's API
    fn api(&self) -> &Self::API;
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

/// Context provided during paint - with full GPU access
pub struct PaintContext {
    /// GPU device
    pub device: std::sync::Arc<wgpu::Device>,
    /// GPU queue
    pub queue: std::sync::Arc<wgpu::Queue>,
    /// Viewport information for coordinate transformations
    pub viewport: crate::types::ViewportInfo,
    /// Raw GPU renderer pointer for pipeline/buffer access
    /// This gives plugins full GPU power - they can create pipelines, upload buffers, etc.
    pub gpu_renderer: *mut std::ffi::c_void,
    /// Any additional context data plugins might need
    /// This is opaque to the SDK but the runtime knows what it is
    pub context_data: *const std::ffi::c_void,
}

// === Plugin Registry ===

/// Registry for plugin discovery and type-safe API access
pub struct PluginRegistry {
    // Implementation will be in core, this is just the interface
    pub _private: (),
}

impl PluginRegistry {
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
}

// Something that has some size
pub trait Spatial: Paintable {
    fn measure(&self) -> LogicalSize;
}
