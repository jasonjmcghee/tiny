//! Service traits and registry for plugin system
//!
//! Provides a service-oriented architecture for accessing shared resources
//! like fonts and text styling across the FFI boundary.

use ahash::AHashMap as HashMap;
use std::any::{Any, TypeId};
use std::sync::Arc;

/// Text layout result from font service
#[derive(Debug, Clone)]
pub struct TextLayout {
    pub glyphs: Vec<PositionedGlyph>,
    pub width: f32,
    pub height: f32,
}

/// Information about a positioned glyph
#[derive(Clone, Debug)]
pub struct PositionedGlyph {
    pub char: char,
    pub pos: crate::types::LayoutPos,
    pub size: crate::types::PhysicalSizeF,
    pub tex_coords: [f32; 4],
    pub color: u32,
}

/// Font service trait for text layout and rendering
pub trait FontService: Send + Sync {
    /// Layout text at logical font size
    fn layout_text(&self, text: &str, font_size: f32) -> TextLayout;

    /// Layout text with explicit scale factor for crisp rendering
    fn layout_text_scaled(&self, text: &str, font_size: f32, scale_factor: f32) -> TextLayout;

    /// Get character width coefficient for fast calculations
    fn char_width_coef(&self) -> f32;

    /// Get font atlas data for GPU upload
    fn atlas_data(&self) -> Vec<u8>;

    /// Get atlas dimensions
    fn atlas_size(&self) -> (u32, u32);

    /// Pre-rasterize ASCII characters for performance
    fn prerasterize_ascii(&self, font_size_px: f32);

    /// Hit test: find character position at x coordinate
    fn hit_test_line(
        &self,
        line_text: &str,
        font_size: f32,
        scale_factor: f32,
        target_x: f32,
    ) -> u32;
}

/// Type of text effect
#[derive(Debug, Clone)]
pub enum TextEffectType {
    /// Token ID for syntax coloring
    Token(u8),
    /// Shader effect with ID and parameters
    Shader { id: u32, params: Option<Vec<f32>> },
}

/// Text effect for syntax highlighting and shaders
#[derive(Debug, Clone)]
pub struct TextEffect {
    pub range: std::ops::Range<usize>,
    pub effect: TextEffectType,
    pub priority: i32,
}

/// Text style provider service for syntax highlighting
pub trait TextStyleService: Send + Sync {
    /// Get text effects for a byte range
    fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<TextEffect>;

    /// Request update for this provider (non-blocking)
    fn request_update(&self, text: &str, version: u64);

    /// Get provider name for debugging
    fn name(&self) -> &str;
}

/// Service registry for accessing shared services
pub struct ServiceRegistry {
    services: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl ServiceRegistry {
    /// Create a new service registry
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
        }
    }

    /// Register a service
    pub fn register<T: Any + Send + Sync + 'static>(&mut self, service: Arc<T>) {
        let type_id = TypeId::of::<T>();
        self.services.insert(type_id, service);
    }

    /// Get a service by type
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let type_id = TypeId::of::<T>();
        // eprintln!(
        //     "Looking for service with TypeId: {:?} for type: {}",
        //     type_id,
        //     std::any::type_name::<T>()
        // );
        // eprintln!("Registry contains {} services", self.services.len());
        // for (id, _) in &self.services {
        //     eprintln!("  - Service TypeId: {:?}", id);
        // }

        self.services
            .get(&type_id)
            .and_then(|service| service.clone().downcast::<T>().ok())
    }

    /// Check if a service is registered
    pub fn has<T: Any + Send + Sync + 'static>(&self) -> bool {
        self.services.contains_key(&TypeId::of::<T>())
    }
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Service for collecting and batching glyph submissions from plugins
pub trait TextRenderingService: Send + Sync {
    /// Submit glyphs to be rendered at a specific z-index
    fn submit_glyphs(&self, glyphs: Vec<crate::types::GlyphInstance>, z_index: i32);

    /// Collect all submissions sorted by z-index
    fn collect_submissions(&self) -> Vec<(Vec<crate::types::GlyphInstance>, i32)>;

    /// Clear all submissions (called after rendering)
    fn clear_submissions(&self);
}

/// Context data that can be passed through FFI boundary
/// This wraps the service registry for plugin access
#[repr(C)]
pub struct ContextData {
    /// Service registry pointer
    pub registry: *const ServiceRegistry,
}

impl ContextData {
    /// Create context data from a service registry
    pub fn from_registry(registry: &ServiceRegistry) -> Self {
        Self {
            registry: registry as *const _,
        }
    }

    /// Get the service registry
    ///
    /// # Safety
    /// Caller must ensure the registry pointer is valid and points to a live ServiceRegistry
    pub unsafe fn registry(&self) -> &ServiceRegistry {
        &*self.registry
    }
}
