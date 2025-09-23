//! Renderer manages widget rendering and viewport transformations
//!
//! Converts document tree to widgets and coordinates their GPU rendering

use std::sync::Arc;
#[allow(unused)]
use tiny_core::wgpu::hal::{DynCommandEncoder, DynDevice, DynQueue};
use tiny_core::{
    plugin_loader::PluginLoader,
    tree::{self, Rect, Tree},
    GpuRenderer,
};
use tiny_sdk::{GlyphInstance, LayoutPos, LayoutRect, LogicalSize};

use crate::coordinates::Viewport;
use crate::input;
use crate::syntax;
use crate::text_effects;
use crate::text_renderer::{self, TextRenderer};
use crate::widget::{self, PaintContext, WidgetManager};
use tiny_font;
use tiny_sdk::ServiceRegistry;

// === Renderer ===

/// Converts tree to widgets and manages rendering
pub struct Renderer {
    /// Text style provider for syntax highlighting
    pub text_styles: Option<Box<dyn text_effects::TextStyleProvider>>,
    /// Syntax highlighter for viewport queries (optional)
    pub syntax_highlighter: Option<Arc<syntax::SyntaxHighlighter>>,
    /// Font system for text rendering (shared reference)
    pub font_system: Option<std::sync::Arc<tiny_font::SharedFontSystem>>,
    /// Viewport for coordinate transformation
    pub viewport: Viewport,
    /// GPU renderer reference for widget painting
    gpu_renderer: Option<*const GpuRenderer>,
    /// Cached document text for syntax queries
    pub cached_doc_text: Option<Arc<String>>,
    /// Cached document version
    pub cached_doc_version: u64,
    /// Widget manager for overlay widgets
    pub widget_manager: WidgetManager,
    /// New decoupled text renderer
    pub text_renderer: TextRenderer,
    /// Last rendered document version for change detection
    last_rendered_version: u64,
    /// Whether layout needs updating due to viewport/font changes
    layout_dirty: bool,
    /// Whether syntax needs updating due to highlighter changes
    syntax_dirty: bool,
    /// Plugin loader for dynamic plugins
    plugin_loader: Option<std::sync::Arc<std::sync::Mutex<PluginLoader>>>,
    /// Library file watchers for hot reloading (one per plugin)
    lib_watchers: Vec<notify::RecommendedWatcher>,
    /// Config file watchers for hot reloading (one per plugin)
    config_watchers: Vec<notify::RecommendedWatcher>,
    /// Current cursor position for plugins
    cursor_position: Option<LayoutPos>,
    /// Last selection positions sent to plugin (to avoid redundant updates)
    last_selection_positions: Vec<(tiny_sdk::DocPos, tiny_sdk::DocPos)>,
    /// Last viewport scroll sent to plugin
    last_viewport_scroll: (f32, f32),
    /// Service registry for plugins (persistent)
    service_registry: ServiceRegistry,
}

// SAFETY: Renderer is Send + Sync because the GPU renderer pointer
// is only used during render calls which happen on the same thread
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        // Create service registry
        let service_registry = ServiceRegistry::new();

        // Plugin loader will be initialized later when we have GPU resources
        let plugin_loader = None;

        Self {
            text_styles: None,
            syntax_highlighter: None,
            font_system: None,
            viewport: Viewport::new(size.0, size.1, scale_factor),
            gpu_renderer: None,
            cached_doc_text: None,
            cached_doc_version: 0,
            widget_manager: WidgetManager::new(),
            text_renderer: TextRenderer::new(),
            last_rendered_version: 0,
            layout_dirty: true, // Start dirty to ensure first render happens
            syntax_dirty: false,
            plugin_loader,
            lib_watchers: Vec::new(),
            config_watchers: Vec::new(),
            cursor_position: None,
            last_selection_positions: Vec::new(),
            last_viewport_scroll: (0.0, 0.0),
            service_registry,
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.viewport.set_font_size(font_size);
        self.layout_dirty = true; // Layout needs updating when font size changes
    }

    /// Set GPU renderer reference for widget painting and initialize theme
    pub fn set_gpu_renderer(&mut self, gpu_renderer: &GpuRenderer) {
        // Only set if not already set to avoid reinitializing
        if self.gpu_renderer.is_none() {
            self.gpu_renderer = Some(gpu_renderer as *const _);
            // Theme initialization is now handled in app.rs

            // Now that we have GPU, initialize plugins if not already done
            if self.plugin_loader.is_none() {
                self.initialize_plugins(gpu_renderer);
            }
        }
    }

    /// Initialize plugins with GPU resources
    fn initialize_plugins(&mut self, gpu_renderer: &GpuRenderer) {
        // Load configuration from init.toml
        let app_config = match crate::config::AppConfig::load() {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Failed to load init.toml: {}, using defaults", e);
                crate::config::AppConfig::default()
            }
        };

        let plugin_dir = std::path::PathBuf::from(&app_config.plugins.plugin_dir);
        if !plugin_dir.exists() {
            // eprintln!("Plugin directory not found: {}", app_config.plugins.plugin_dir);
            return;
        }

        let mut loader = PluginLoader::new(plugin_dir.clone());

        // Load all enabled plugins using explicit paths from config
        for plugin_name in &app_config.plugins.enabled {
            // eprintln!("Loading plugin: {}", plugin_name);

            // Check if we have explicit config for this plugin
            if let Some(plugin_config) = app_config.plugins.plugins.get(plugin_name) {
                let lib_path = plugin_config.lib_path(plugin_name, &app_config.plugins.plugin_dir);
                let config_path =
                    plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);

                println!(
                    "Using explicit paths - lib: {}, config: {}",
                    lib_path, config_path
                );

                match loader.load_plugin_from_path(plugin_name, &lib_path, &config_path) {
                    Ok(_) => {
                        // eprintln!("Loaded {} plugin from explicit path", plugin_name);

                        // Initialize with GPU resources
                        let device = gpu_renderer.device_arc();
                        let queue = gpu_renderer.queue_arc();

                        match loader.initialize_plugin(plugin_name, device, queue) {
                            Ok(_) => {
                                eprintln!("Initialized {} plugin with GPU resources", plugin_name);

                                // Send initial viewport info to selection plugin
                                if plugin_name == "selection" {
                                    if let Some(selection_plugin) = loader.get_plugin_mut("selection") {
                                        if let Some(library) = selection_plugin.instance.as_library_mut() {
                                            let mut viewport_args = Vec::new();
                                            viewport_args.extend_from_slice(&self.viewport.metrics.line_height.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.logical_size.width.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.margin.x.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.margin.y.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scale_factor.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scroll.x.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scroll.y.0.to_le_bytes());

                                            match library.call("set_viewport_info", &viewport_args) {
                                                Ok(_) => eprintln!("Sent initial viewport info to selection plugin"),
                                                Err(e) => eprintln!("Failed to send initial viewport info: {}", e),
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to initialize {} plugin: {}", plugin_name, e)
                            }
                        }
                    }
                    Err(e) => eprintln!(
                        "Failed to load {} plugin from explicit path: {}",
                        plugin_name, e
                    ),
                }
            } else {
                // Use default paths
                match loader.load_plugin(plugin_name) {
                    Ok(_) => {
                        eprintln!("Loaded {} plugin with default paths", plugin_name);

                        // Initialize with GPU resources
                        let device = gpu_renderer.device_arc();
                        let queue = gpu_renderer.queue_arc();

                        match loader.initialize_plugin(plugin_name, device, queue) {
                            Ok(_) => {
                                eprintln!("Initialized {} plugin with GPU resources", plugin_name);

                                // Send initial viewport info to selection plugin
                                if plugin_name == "selection" {
                                    if let Some(selection_plugin) = loader.get_plugin_mut("selection") {
                                        if let Some(library) = selection_plugin.instance.as_library_mut() {
                                            let mut viewport_args = Vec::new();
                                            viewport_args.extend_from_slice(&self.viewport.metrics.line_height.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.logical_size.width.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.margin.x.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.margin.y.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scale_factor.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scroll.x.0.to_le_bytes());
                                            viewport_args.extend_from_slice(&self.viewport.scroll.y.0.to_le_bytes());

                                            match library.call("set_viewport_info", &viewport_args) {
                                                Ok(_) => eprintln!("Sent initial viewport info to selection plugin"),
                                                Err(e) => eprintln!("Failed to send initial viewport info: {}", e),
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to initialize {} plugin: {}", plugin_name, e)
                            }
                        }
                    }
                    Err(e) => eprintln!("Failed to load {} plugin: {}", plugin_name, e),
                }
            }
        }

        // Store loader in Arc for sharing with watchers
        let loader_arc = std::sync::Arc::new(std::sync::Mutex::new(loader));

        // Set up hot-reload watching for enabled plugins' library files
        // eprintln!("Enabled plugins: {:?}", app_config.plugins.enabled);
        // eprintln!("Plugin configs: {:?}", app_config.plugins.plugins.keys().collect::<Vec<_>>());
        for plugin_name in &app_config.plugins.enabled {
            // eprintln!("Checking plugin {} for hot-reload", plugin_name);
            if let Some(plugin_config) = app_config.plugins.plugins.get(plugin_name) {
                // eprintln!("Found config for {}, auto_reload = {}", plugin_name, plugin_config.auto_reload);
                if plugin_config.auto_reload {
                    let lib_path =
                        plugin_config.lib_path(plugin_name, &app_config.plugins.plugin_dir);
                    let lib_path_buf = std::path::PathBuf::from(&lib_path);

                    eprintln!(
                        "Setting up hot-reload for {} watching specific file: {:?}",
                        plugin_name, lib_path
                    );
                    eprintln!("Library file exists: {}", lib_path_buf.exists());
                    if lib_path_buf.exists() {
                        eprintln!(
                            "Library file metadata: {:?}",
                            std::fs::metadata(&lib_path_buf).ok()
                        );
                    }

                    // Create watcher for the specific library file
                    use notify::{Event, RecursiveMode, Watcher};
                    let loader_for_lib = loader_arc.clone();
                    let plugin_name_for_lib = plugin_name.clone();
                    let device = gpu_renderer.device_arc();
                    let queue = gpu_renderer.queue_arc();
                    let lib_path_for_reload = lib_path.clone();
                    let config_path_for_reload =
                        plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);

                    let lib_watcher =
                        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                            if let Ok(event) = res {
                                eprintln!(
                                    "Watcher event for {}: kind={:?}, paths={:?}",
                                    plugin_name_for_lib, event.kind, event.paths
                                );

                                // Only reload on Create or Modify, not Remove (file is being replaced during build)
                                if event.kind.is_create() || event.kind.is_modify() {
                                    eprintln!(
                                        "Library file changed for {}: {:?}",
                                        plugin_name_for_lib, event.paths
                                    );

                                    // Quick check if file exists and is not empty (cargo watch might still be writing)
                                    let mut retries = 0;
                                    while retries < 10 {
                                        if let Ok(metadata) =
                                            std::fs::metadata(&lib_path_for_reload)
                                        {
                                            if metadata.len() > 0 {
                                                // File exists and has content, safe to reload
                                                break;
                                            }
                                        }
                                        retries += 1;
                                        std::thread::sleep(std::time::Duration::from_millis(10));
                                    }

                                    if retries == 10 {
                                        eprintln!("File not ready after 100ms, skipping reload");
                                        return;
                                    }

                                    if let Ok(mut loader) = loader_for_lib.lock() {
                                        // First unload the plugin
                                        if let Err(e) = loader.unload_plugin(&plugin_name_for_lib) {
                                            eprintln!(
                                                "Failed to unload plugin {}: {}",
                                                plugin_name_for_lib, e
                                            );
                                            return;
                                        }

                                        // Use the original configured paths for reload
                                        eprintln!(
                                            "Reloading plugin {} from lib={}, config={}",
                                            plugin_name_for_lib,
                                            lib_path_for_reload,
                                            config_path_for_reload
                                        );

                                        if let Err(e) = loader.load_plugin_from_path(
                                            &plugin_name_for_lib,
                                            &lib_path_for_reload,
                                            &config_path_for_reload,
                                        ) {
                                            eprintln!(
                                                "Failed to reload plugin {}: {}",
                                                plugin_name_for_lib, e
                                            );
                                            return;
                                        }

                                        // Re-initialize with GPU resources
                                        if let Err(e) = loader.initialize_plugin(
                                            &plugin_name_for_lib,
                                            device.clone(),
                                            queue.clone(),
                                        ) {
                                            eprintln!(
                                                "Failed to reinitialize plugin {}: {}",
                                                plugin_name_for_lib, e
                                            );
                                        } else {
                                            eprintln!(
                                                "Successfully hot-reloaded plugin: {}",
                                                plugin_name_for_lib
                                            );
                                        }
                                    }
                                }
                            }
                        });

                    if let Ok(mut watcher) = lib_watcher {
                        // First try to watch the specific library file
                        let watch_result =
                            watcher.watch(&lib_path_buf, RecursiveMode::NonRecursive);

                        if let Err(e) = watch_result {
                            eprintln!(
                                "Failed to watch lib file directly {}: {}, trying parent directory",
                                lib_path, e
                            );

                            // Fallback: watch parent directory (required on some macOS versions)
                            if let Some(parent_dir) = lib_path_buf.parent() {
                                if let Err(e2) =
                                    watcher.watch(parent_dir, RecursiveMode::NonRecursive)
                                {
                                    eprintln!(
                                        "Failed to watch parent directory {:?}: {}",
                                        parent_dir, e2
                                    );
                                } else {
                                    eprintln!(
                                        "Watching parent directory for library: {:?}",
                                        parent_dir
                                    );
                                    self.lib_watchers.push(watcher);
                                }
                            }
                        } else {
                            eprintln!("Watching library file: {}", lib_path);
                            self.lib_watchers.push(watcher);
                        }
                    }
                }
            }
        }

        // Set up TOML config watching for specific plugin config files
        for plugin_name in &app_config.plugins.enabled {
            if let Some(plugin_config) = app_config.plugins.plugins.get(plugin_name) {
                let config_path =
                    plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);
                let config_path_buf = std::path::PathBuf::from(&config_path);

                // Only watch if the config file exists
                if config_path_buf.exists() {
                    // eprintln!("Setting up config watching for {} at: {:?}", plugin_name, config_path);

                    use notify::{Event, RecursiveMode, Watcher};
                    let loader_for_config = loader_arc.clone();
                    let plugin_name_for_config = plugin_name.clone();

                    let config_watcher =
                        notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
                            if let Ok(event) = res {
                                if event.kind.is_modify() {
                                    // eprintln!("Config file changed for {}: {:?}", plugin_name_for_config, event.paths);

                                    // Read the new config
                                    if let Ok(config_data) =
                                        std::fs::read_to_string(&event.paths[0])
                                    {
                                        // Send to plugin
                                        if let Ok(mut loader) = loader_for_config.lock() {
                                            if let Some(plugin) =
                                                loader.get_plugin_mut(&plugin_name_for_config)
                                            {
                                                if let Some(configurable) =
                                                    plugin.instance.as_configurable()
                                                {
                                                    if let Err(e) =
                                                        configurable.config_updated(&config_data)
                                                    {
                                                        eprintln!(
                                                            "Failed to update config for {}: {}",
                                                            plugin_name_for_config, e
                                                        );
                                                    } else {
                                                        eprintln!(
                                                            "Updated config for plugin: {}",
                                                            plugin_name_for_config
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        });

                    if let Ok(mut watcher) = config_watcher {
                        // Watch the specific config file only
                        if let Err(e) = watcher.watch(&config_path_buf, RecursiveMode::NonRecursive)
                        {
                            eprintln!("Failed to watch config file {}: {}", config_path, e);
                        } else {
                            eprintln!("Watching config file: {}", config_path);
                            self.config_watchers.push(watcher);
                        }
                    }
                }
            }
        }

        // Store the loader Arc
        self.plugin_loader = Some(loader_arc);
    }

    /// Set text style provider (takes ownership)
    pub fn set_text_styles(&mut self, provider: Box<dyn text_effects::TextStyleProvider>) {
        // Register text styles adapter in service registry
        let adapter = crate::text_style_box_adapter::BoxedTextStyleAdapter::from_ref(&provider);
        self.service_registry.register(adapter);
        self.text_styles = Some(provider);
    }

    /// Set syntax highlighter for viewport queries
    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<syntax::SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
        self.syntax_dirty = true; // Syntax needs updating when highlighter changes
    }

    /// Set font system (takes shared reference)
    pub fn set_font_system(&mut self, font_system: std::sync::Arc<tiny_font::SharedFontSystem>) {
        // Set font system on viewport for accurate measurements
        self.viewport.set_font_system(font_system.clone());
        // Register font in service registry
        self.service_registry.register(font_system.clone());
        self.font_system = Some(font_system);
        self.layout_dirty = true; // Layout needs updating when font system changes
    }

    /// Handle incremental edit for stable typing experience
    pub fn apply_incremental_edit(&mut self, edit: &tree::Edit) {
        self.text_renderer.apply_incremental_edit(edit);
    }

    /// Update viewport size
    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.viewport.resize(width, height, scale_factor);
        self.layout_dirty = true; // Layout needs updating when viewport changes
    }

    /// Set selections and cursor plugins data
    pub fn set_selection_widgets(&mut self, input_handler: &input::InputHandler, doc: &tree::Doc) {
        // Get selection data from input handler
        let (cursor_pos, selection_positions) = input_handler.get_selection_data(doc, &self.viewport);

        // Check if viewport scroll has changed
        let current_scroll = (self.viewport.scroll.x.0, self.viewport.scroll.y.0);
        let viewport_changed = current_scroll != self.last_viewport_scroll;

        // Check if selections have changed
        let selections_changed = selection_positions != self.last_selection_positions;

        if !viewport_changed && !selections_changed {
            // Nothing changed, skip update
            return;
        }

        // Update selection plugin via library API
        if let Some(ref loader_arc) = self.plugin_loader {
            if let Ok(mut loader) = loader_arc.lock() {
                // Update viewport info if it changed OR if this is the first selection
                if viewport_changed {
                    if let Some(selection_plugin) = loader.get_plugin_mut("selection") {
                        if let Some(library) = selection_plugin.instance.as_library_mut() {
                            // Send viewport info (including scroll offsets)
                            let mut viewport_args = Vec::new();
                            viewport_args.extend_from_slice(&self.viewport.metrics.line_height.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.logical_size.width.0.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.margin.x.0.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.margin.y.0.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.scale_factor.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.scroll.x.0.to_le_bytes());
                            viewport_args.extend_from_slice(&self.viewport.scroll.y.0.to_le_bytes());

                            match library.call("set_viewport_info", &viewport_args) {
                                Ok(_) => eprintln!("Updated selection plugin viewport info"),
                                Err(e) => eprintln!("Failed to update selection viewport: {}", e),
                            }
                        }
                    }
                    self.last_viewport_scroll = current_scroll;
                }

                // Update selections if they changed
                if selections_changed {
                    if let Some(selection_plugin) = loader.get_plugin_mut("selection") {
                        if let Some(library) = selection_plugin.instance.as_library_mut() {
                            // Format: count (u32), then for each selection:
                            //   start_line, start_column, end_line, end_column (u32 each)

                            let mut args = Vec::new();
                            args.extend_from_slice(&(selection_positions.len() as u32).to_le_bytes());

                            for (start, end) in &selection_positions {
                                args.extend_from_slice(&start.line.to_le_bytes());
                                args.extend_from_slice(&start.column.to_le_bytes());
                                args.extend_from_slice(&end.line.to_le_bytes());
                                args.extend_from_slice(&end.column.to_le_bytes());
                            }

                            match library.call("set_selections", &args) {
                                Ok(_) => {},
                                Err(e) => eprintln!("Failed to update selection plugin: {}", e),
                            }
                        }
                    }
                    self.last_selection_positions = selection_positions;
                }

                // Update cursor plugin position
                if let Some(pos) = cursor_pos {
                    // Convert layout position to view position (accounting for scroll)
                    let view_pos = self.viewport.layout_to_view(pos);
                    self.cursor_position = Some(pos);

                    if let Some(cursor_plugin) = loader.get_plugin_mut("cursor") {
                        if let Some(library) = cursor_plugin.instance.as_library_mut() {
                            // Call set_position method with binary-encoded VIEW position
                            let x_bytes = view_pos.x.0.to_le_bytes();
                            let y_bytes = view_pos.y.0.to_le_bytes();
                            let mut args = Vec::new();
                            args.extend_from_slice(&x_bytes);
                            args.extend_from_slice(&y_bytes);

                            match library.call("set_position", &args) {
                                Ok(_) => {}
                                Err(e) => eprintln!("Failed to update cursor position: {}", e),
                            }
                        }
                    }
                } else {
                    // eprintln!("No cursor position from input handler");
                }
            }
        }
    }

    /// Render tree with direct GPU render pass and optional widget paint context
    pub fn render_with_pass_and_context(
        &mut self,
        tree: &Tree,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        // Early exit if nothing has changed - skip all expensive operations
        if tree.version == self.last_rendered_version && !self.layout_dirty && !self.syntax_dirty {
            return;
        }
        // Initialize TextRenderer - this MUST happen before walk_visible_range_with_pass
        // Update layout cache if text changed
        if let Some(font_system) = &self.font_system {
            self.text_renderer
                .update_layout(tree, font_system, &self.viewport);
        }

        // Update syntax highlighting
        if let Some(ref highlighter) = self.syntax_highlighter {
            // Check if syntax has caught up to document version
            let syntax_version = highlighter.cached_version();
            let doc_version = tree.version;
            let fresh_parse = syntax_version == doc_version;

            // Convert tree-sitter effects to token ranges
            let text = tree.flatten_to_string();
            let effects = highlighter.get_visible_effects(&text, 0..text.len());

            let mut tokens = Vec::new();
            for effect in effects {
                if let text_effects::EffectType::Token(token_id) = effect.effect {
                    tokens.push(text_renderer::TokenRange {
                        byte_range: effect.range.clone(),
                        token_id,
                    });
                }
            }

            // Pass fresh_parse flag so text_renderer knows whether to shift tokens
            self.text_renderer.update_syntax(&tokens, fresh_parse);
        }

        // Update visible range for culling
        self.text_renderer
            .update_visible_range(&self.viewport, tree);

        // Update cached doc text for syntax queries if it changed
        if self.cached_doc_text.is_none() || tree.version != self.cached_doc_version {
            self.cached_doc_text = Some(tree.flatten_to_string());
            self.cached_doc_version = tree.version;
        }

        // Paint selections BEFORE text
        if let Some(pass) = render_pass.as_deref_mut() {
            let widgets = self.widget_manager.widgets_in_order();
            if let (Some(gpu), Some(font)) = (self.gpu_renderer, &self.font_system) {
                let gpu_renderer = unsafe { &*gpu };

                let mut ctx = PaintContext::new(
                    self.viewport.to_viewport_info(),
                    gpu_renderer.device_arc(),
                    gpu_renderer.queue_arc(),
                    gpu as *mut _,
                    &self.service_registry,
                );

                // Set the GPU context for plugins
                ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

                // Don't need draw function for regular widgets
                for widget in widgets {
                    if widget.priority() < 0 {
                        widget.paint(&ctx, pass);
                    }
                }

                // Paint plugins
                if let Some(ref loader_arc) = self.plugin_loader {
                    // eprintln!("Have plugin loader (2nd location)");
                    if let Ok(loader) = loader_arc.lock() {
                        let plugins = loader.list_plugins();
                        for plugin_key in plugins {
                            if let Some(plugin) = loader.get_plugin(&plugin_key) {
                                if let Some(paintable) = plugin.instance.as_paintable() {
                                    let z = paintable.z_index();
                                    if z < 0 {
                                        paintable.paint(&ctx, pass);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Walk visible range
        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        self.walk_visible_range_with_pass(tree, visible_range, render_pass.as_deref_mut());

        // Paint cursor and overlays AFTER text
        if let Some(pass) = render_pass.as_deref_mut() {
            if let (Some(gpu), Some(font)) = (self.gpu_renderer, &self.font_system) {
                let gpu_renderer = unsafe { &*gpu };

                let mut ctx = PaintContext::new(
                    self.viewport.to_viewport_info(),
                    gpu_renderer.device_arc(),
                    gpu_renderer.queue_arc(),
                    gpu as *mut _,
                    &self.service_registry,
                );

                // Set the GPU context for plugins
                ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

                // TODO: Move all this to plugins
                let widgets = self.widget_manager.widgets_in_order();
                for widget in widgets {
                    if widget.priority() >= 0 {
                        widget.paint(&ctx, pass);
                    }
                }

                // Ensure render state is properly set up for plugins
                let (width, height) = gpu_renderer.viewport_size();
                gpu_renderer.update_uniforms(width, height);

                // Paint plugins
                if let Some(ref loader_arc) = self.plugin_loader {
                    // eprintln!("Have plugin loader (2nd location)");
                    if let Ok(loader) = loader_arc.lock() {
                        for plugin_key in loader.list_plugins() {
                            if let Some(plugin) = loader.get_plugin(&plugin_key) {
                                if let Some(paintable) = plugin.instance.as_paintable() {
                                    if paintable.z_index() >= 0 {
                                        paintable.paint(&ctx, pass);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Update version tracking and clear dirty flags after successful render
        self.last_rendered_version = tree.version;
        self.layout_dirty = false;
        self.syntax_dirty = false;
    }

    /// Walk visible range with direct GPU rendering using new TextRenderer
    fn walk_visible_range_with_pass(
        &mut self,
        tree: &Tree,
        byte_range: std::ops::Range<usize>,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        // Use the new TextRenderer for all text rendering
        if let Some(pass) = render_pass.as_mut() {
            if let (Some(gpu_renderer_ptr), Some(font_system)) =
                (self.gpu_renderer, &self.font_system)
            {
                let gpu_renderer = unsafe { &*gpu_renderer_ptr };

                // Get visible glyphs from TextRenderer with their token IDs and relative positions
                let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

                // Create a style buffer with ONLY the visible glyph token IDs (as u32 for shader)
                let visible_style_buffer: Vec<u32> =
                    visible_glyphs.iter().map(|g| g.token_id as u32).collect();

                // Upload the visible-only style buffer to GPU
                if gpu_renderer.has_styled_pipeline() {
                    let gpu_renderer_mut = unsafe { &mut *(gpu_renderer_ptr as *mut GpuRenderer) };
                    gpu_renderer_mut.upload_style_buffer_u32(&visible_style_buffer);
                }

                // Convert to GlyphInstances for GPU
                let mut glyph_instances = Vec::new();
                for glyph in visible_glyphs {
                    // Transform from layout to physical coordinates
                    let physical_pos = self.viewport.layout_to_physical(glyph.layout_pos);

                    glyph_instances.push(GlyphInstance {
                        pos: LayoutPos::new(physical_pos.x.0, physical_pos.y.0),
                        tex_coords: glyph.tex_coords,
                        token_id: glyph.token_id as u8,
                        relative_pos: glyph.relative_pos,
                        shader_id: None,
                    });
                }

                // Render glyphs with styled pipeline if available
                if !glyph_instances.is_empty() {
                    let use_styled =
                        self.syntax_highlighter.is_some() && gpu_renderer.has_styled_pipeline();
                    gpu_renderer.draw_glyphs_styled(pass, &glyph_instances, use_styled);
                }

                // Still handle widgets separately
                tree.walk_visible_range(byte_range.clone(), |spans, _, _| {
                    for span in spans {
                        if let tree::Span::Spatial(widget) = span {
                            // Use persistent service registry
                            let ctx = PaintContext::new(
                                self.viewport.to_viewport_info(),
                                gpu_renderer.device_arc(),
                                gpu_renderer.queue_arc(),
                                gpu_renderer_ptr as *mut _,
                                &self.service_registry,
                            );
                            widget.paint(&ctx, pass);
                        }
                    }
                });
            }
        }
    }

    /// Update animation for overlay widgets
    pub fn update_widgets(&mut self, dt: f32) -> bool {
        self.widget_manager.update_all(dt)
    }

    /// Get widget manager for manual widget painting
    pub fn widget_manager(&self) -> &WidgetManager {
        &self.widget_manager
    }

    /// Get mutable widget manager for manual widget painting
    pub fn widget_manager_mut(&mut self) -> &mut WidgetManager {
        &mut self.widget_manager
    }
}

// === Viewport Effects Provider (simplified) ===
struct ViewportEffectsProvider {
    effects: Vec<text_effects::TextEffect>,
    byte_offset: usize,
}

impl text_effects::TextStyleProvider for ViewportEffectsProvider {
    fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<text_effects::TextEffect> {
        let doc_range = (range.start + self.byte_offset)..(range.end + self.byte_offset);
        self.effects
            .iter()
            .filter(|e| e.range.start < doc_range.end && e.range.end > doc_range.start)
            .map(|e| text_effects::TextEffect {
                range: e.range.start.saturating_sub(self.byte_offset)
                    ..e.range.end.saturating_sub(self.byte_offset),
                effect: e.effect.clone(),
                priority: e.priority,
            })
            .collect()
    }
    fn request_update(&self, _: &str, _: u64) {}
    fn name(&self) -> &str {
        "viewport_effects"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
