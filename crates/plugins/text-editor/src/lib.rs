//! Text Editor Plugin - Core text rendering and editing functionality

use ahash::HashMap;
use serde::Deserialize;
use std::sync::Arc;
use tiny_sdk::{
    Capability, Configurable, Initializable, LayoutPos, LayoutRect, Library, PaintContext,
    Paintable, Plugin, PluginError, SetupContext, Updatable, UpdateContext, wgpu,
};

mod api;
mod layout;
mod renderer;

pub use api::TextEditorAPI;
use layout::LayoutCache;
use renderer::TextRenderer;

/// Configuration loaded from plugin.toml
#[derive(Debug, Clone, Deserialize)]
pub struct TextEditorConfig {
    #[serde(default = "default_font_size")]
    font_size: f32,
    #[serde(default = "default_line_height")]
    line_height: f32,
    #[serde(default = "default_layout_cache_enabled")]
    layout_cache_enabled: bool,
    #[serde(default = "default_incremental_rendering")]
    incremental_rendering: bool,
    #[serde(default = "default_show_line_numbers")]
    show_line_numbers: bool,
    #[serde(default = "default_tab_width")]
    tab_width: u32,
    #[serde(default = "default_soft_wrap")]
    soft_wrap: bool,
}

fn default_font_size() -> f32 { 14.0 }
fn default_line_height() -> f32 { 19.6 }
fn default_layout_cache_enabled() -> bool { true }
fn default_incremental_rendering() -> bool { true }
fn default_show_line_numbers() -> bool { false }
fn default_tab_width() -> u32 { 4 }
fn default_soft_wrap() -> bool { false }

impl Default for TextEditorConfig {
    fn default() -> Self {
        Self {
            font_size: default_font_size(),
            line_height: default_line_height(),
            layout_cache_enabled: default_layout_cache_enabled(),
            incremental_rendering: default_incremental_rendering(),
            show_line_numbers: default_show_line_numbers(),
            tab_width: default_tab_width(),
            soft_wrap: default_soft_wrap(),
        }
    }
}

/// Main text editor plugin struct
pub struct TextEditorPlugin {
    // Configuration
    config: TextEditorConfig,

    // API for external access
    api: TextEditorAPI,

    // Core components
    renderer: TextRenderer,
    layout_cache: LayoutCache,

    // Document state
    document_text: Arc<String>,
    document_version: u64,

    // Viewport information
    viewport_info: Option<tiny_sdk::ViewportInfo>,

    // GPU resources
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,

    // Dependencies (loaded at runtime)
    cursor_library: Option<*const dyn Library>,
    selection_library: Option<*const dyn Library>,
}

impl TextEditorPlugin {
    /// Create a new text editor plugin
    pub fn new() -> Self {
        Self {
            config: TextEditorConfig::default(),
            api: TextEditorAPI::new(),
            renderer: TextRenderer::new(),
            layout_cache: LayoutCache::new(),
            document_text: Arc::new(String::new()),
            document_version: 0,
            viewport_info: None,
            device: None,
            queue: None,
            cursor_library: None,
            selection_library: None,
        }
    }

    /// Set document text
    pub fn set_document_text(&mut self, text: String, version: u64) {
        self.document_text = Arc::new(text);
        self.document_version = version;

        // Update layout cache
        if let Some(ref viewport) = self.viewport_info {
            self.layout_cache.update_text(
                &self.document_text,
                viewport,
                self.config.font_size,
                self.config.line_height,
            );
        }

        // Update API with new layout info
        self.api.update_from_layout(&self.layout_cache);

        // Notify dependencies about document change
        self.notify_dependencies();
    }

    /// Notify cursor and selection plugins about document changes
    fn notify_dependencies(&self) {
        // This will be called to update cursor/selection positions
        // when the document changes
    }
}

// === Plugin Trait Implementation ===

impl Plugin for TextEditorPlugin {
    fn name(&self) -> &str {
        "text-editor"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Updatable,
            Capability::Paintable("text-editor".to_string()),
            Capability::Library(std::any::TypeId::of::<TextEditorAPI>()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_updatable(&mut self) -> Option<&mut dyn Updatable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }

    fn as_library(&self) -> Option<&dyn Library> {
        Some(self)
    }

    fn as_library_mut(&mut self) -> Option<&mut dyn Library> {
        Some(self)
    }

    fn as_configurable(&mut self) -> Option<&mut dyn Configurable> {
        Some(self)
    }
}

// === Initializable Implementation ===

impl Initializable for TextEditorPlugin {
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        eprintln!("TextEditorPlugin::setup called");

        // Store GPU resources
        self.device = Some(ctx.device.clone());
        self.queue = Some(ctx.queue.clone());

        // Initialize renderer with GPU resources
        self.renderer.setup(ctx)?;

        // Note: Dependencies will be injected after all plugins are loaded
        // The host will call set_dependencies via the Library trait

        eprintln!("Text editor plugin initialized");
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), PluginError> {
        self.renderer.cleanup();
        Ok(())
    }
}

// === Updatable Implementation ===

impl Updatable for TextEditorPlugin {
    fn update(&mut self, _dt: f32, _ctx: &mut UpdateContext) -> Result<(), PluginError> {
        // Update any animations or time-based effects
        Ok(())
    }
}

// === Paintable Implementation ===

impl Paintable for TextEditorPlugin {
    fn z_index(&self) -> i32 {
        0 // Text renders at base level
    }

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Render text using the renderer
        if !self.document_text.is_empty() {
            self.renderer.paint_text(
                ctx,
                render_pass,
                &self.document_text,
                &self.layout_cache,
                &self.config,
            );
        }
    }
}

// === Library Implementation ===

impl Library for TextEditorPlugin {
    fn name(&self) -> &str {
        "text_editor_api"
    }

    fn call(&mut self, method: &str, args: &[u8]) -> Result<Vec<u8>, PluginError> {
        self.api.handle_call(method, args, &self.layout_cache)
    }
}

// === Configurable Implementation ===

impl Configurable for TextEditorPlugin {
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError> {
        #[derive(Deserialize)]
        struct PluginToml {
            config: TextEditorConfig,
        }

        match toml::from_str::<PluginToml>(config_data) {
            Ok(plugin_toml) => {
                self.config = plugin_toml.config;

                // Re-layout with new config
                if let Some(ref viewport) = self.viewport_info {
                    self.layout_cache.update_text(
                        &self.document_text,
                        viewport,
                        self.config.font_size,
                        self.config.line_height,
                    );
                    self.api.update_from_layout(&self.layout_cache);
                }

                eprintln!("Text editor config updated");
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse text editor config: {}", e);
                Err(PluginError::Other(format!("Config parse error: {}", e).into()))
            }
        }
    }
}

// === Plugin Entry Point ===

/// Create a new text editor plugin instance
#[no_mangle]
pub extern "C" fn text_editor_plugin_create() -> Box<dyn Plugin> {
    Box::new(TextEditorPlugin::new())
}

// === Safe Dependency Access ===

unsafe impl Send for TextEditorPlugin {}
unsafe impl Sync for TextEditorPlugin {}