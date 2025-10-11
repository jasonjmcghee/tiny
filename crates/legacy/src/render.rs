//! Renderer manages widget rendering and viewport transformations

use crate::{
    config::{AppConfig, PluginConfig},
    coordinates::Viewport,
    syntax,
    text_effects::{self, TextStyleProvider},
    text_renderer::{self, TextRenderer},
};
use anyhow::{bail, Context, Result};
use bytemuck;
use notify::{Event, RecursiveMode, Watcher};
use std::sync::{Arc, Mutex};
use tiny_core::{
    plugin_loader::PluginLoader,
    tree::{self, Tree},
    GpuRenderer,
};
use tiny_sdk::{GlyphInstance, LayoutPos, ServiceRegistry};
use tiny_sdk::{PaintContext, Paintable};

const FILE_EXPLORER_WIDTH: f32 = 0.0;
const STATUS_BAR_HEIGHT: f32 = 0.0;
const TAB_BAR_HEIGHT: f32 = 30.0;
const FOO: f32 = 0.0;

// Plugin state synchronization
#[derive(Clone, Debug)]
struct PluginState {
    viewport_info: Vec<u8>,
    selections: Vec<(tiny_sdk::ViewPos, tiny_sdk::ViewPos)>,
    cursor_pos: Option<(f32, f32)>,
}

impl PluginState {
    fn new() -> Self {
        Self {
            viewport_info: Vec::new(),
            selections: Vec::new(),
            cursor_pos: None,
        }
    }

    fn from_viewport(viewport: &Viewport) -> Vec<u8> {
        // Use ViewportInfo which is already Pod/Zeroable
        let info = viewport.to_viewport_info();
        bytemuck::bytes_of(&info).to_vec()
    }

    fn encode_selections(selections: &[(tiny_sdk::ViewPos, tiny_sdk::ViewPos)]) -> Vec<u8> {
        let mut args = Vec::new();
        let len = selections.len() as u32;
        args.extend_from_slice(bytemuck::bytes_of(&len));
        // ViewPos is Pod/Zeroable, we can directly serialize the pairs
        for (start, end) in selections {
            args.extend_from_slice(bytemuck::bytes_of(start));
            args.extend_from_slice(bytemuck::bytes_of(end));
        }
        args
    }

    fn sync_to_plugin(&self, loader: &mut PluginLoader, plugin_name: &str) {
        if let Some(plugin) = loader.get_plugin_mut(plugin_name) {
            if let Some(library) = plugin.instance.as_library_mut() {
                // Send viewport info to plugins that need it
                if !self.viewport_info.is_empty()
                    && (plugin_name == "selection" || plugin_name == "cursor")
                {
                    let _ = library.call("set_viewport_info", &self.viewport_info);
                }

                // Plugin-specific data
                match plugin_name {
                    "selection" => {
                        // Always send selections, even when empty (to clear them)
                        let args = Self::encode_selections(&self.selections);
                        let _ = library.call("set_selections", &args);
                    }
                    "cursor" => {
                        if let Some((x, y)) = self.cursor_pos {
                            let pos = tiny_sdk::ViewPos::new(x, y);
                            let args = bytemuck::bytes_of(&pos);
                            let _ = library.call("set_position", args);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn sync_all(&self, loader: &mut PluginLoader) {
        self.sync_to_plugin(loader, "selection");
        self.sync_to_plugin(loader, "cursor");
    }
}

pub struct Renderer {
    pub font_system: Option<Arc<tiny_font::SharedFontSystem>>,
    pub theme: Option<Arc<tiny_ui::theme::Theme>>,
    pub viewport: Viewport,
    gpu_renderer: Option<*const GpuRenderer>,
    pub cached_doc_text: Option<Arc<String>>,
    pub cached_doc_version: u64,
    pub text_renderer: TextRenderer,
    last_rendered_version: u64,
    layout_dirty: bool,
    syntax_dirty: bool,
    plugin_loader: Option<Arc<Mutex<PluginLoader>>>,
    lib_watchers: Vec<notify::RecommendedWatcher>,
    config_watchers: Vec<notify::RecommendedWatcher>,
    plugin_state: Arc<Mutex<PluginState>>,
    last_viewport_scroll: (f32, f32),
    pub service_registry: ServiceRegistry,
    /// Callback to request redraw (used by config watchers)
    redraw_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Channel to send plugin events from watcher threads
    plugin_event_tx: Option<std::sync::mpsc::Sender<String>>,
    /// Track which tab we have swapped (to avoid marking dirty on swap back)
    current_tab_renderer_id: Option<u64>,
    pub line_numbers_plugin: Option<*mut crate::line_numbers_plugin::LineNumbersPlugin>,
    pub tab_bar_plugin: Option<*mut crate::tab_bar_plugin::TabBarPlugin>,
    pub file_picker_plugin: Option<*mut crate::file_picker_plugin::FilePickerPlugin>,
    pub grep_plugin: Option<*mut crate::grep_plugin::GrepPlugin>,
    /// Title bar height (logical pixels, for macOS transparent titlebar)
    title_bar_height: f32,
    /// Tab bar height (logical pixels, calculated dynamically based on font size)
    tab_bar_height: f32,
    /// Editor widget bounds (where main text renders)
    pub editor_bounds: tiny_sdk::types::LayoutRect,
    /// Accumulated glyphs for batched rendering
    accumulated_glyphs: Vec<GlyphInstance>,
    /// Text decoration rectangles (underline, strikethrough)
    text_decoration_rects: Vec<tiny_sdk::types::RectInstance>,
    /// Line number glyphs (rendered separately)
    line_number_glyphs: Vec<GlyphInstance>,
    /// Tab bar glyphs (rendered separately)
    tab_bar_glyphs: Vec<GlyphInstance>,
    /// Tab bar background rectangles
    tab_bar_rects: Vec<tiny_sdk::types::RectInstance>,
    /// File picker glyphs with their scissor rects
    file_picker_glyphs: Vec<(Vec<GlyphInstance>, (u32, u32, u32, u32))>,
    /// File picker background rectangle
    file_picker_rects: Vec<tiny_sdk::types::RectInstance>,
    /// File picker rounded rect frame
    file_picker_rounded_rect: Option<tiny_sdk::types::RoundedRectInstance>,
    /// Grep glyphs with their scissor rects
    grep_glyphs: Vec<(Vec<GlyphInstance>, (u32, u32, u32, u32))>,
    /// Grep background rectangle
    grep_rects: Vec<tiny_sdk::types::RectInstance>,
    /// Grep rounded rect frame
    grep_rounded_rect: Option<tiny_sdk::types::RoundedRectInstance>,
    /// Scrollbar plugin for main editor
    pub scrollbar_plugin: crate::scrollbar_plugin::ScrollbarPlugin,
    /// Scrollbar rounded rects
    scrollbar_rects: Vec<tiny_sdk::types::RoundedRectInstance>,

    /// Dirty flags to track what needs regeneration
    glyphs_dirty: bool,
    line_numbers_dirty: bool,
    ui_dirty: bool,
    last_scroll: (f32, f32),
    last_viewport_size: (f32, f32),

    /// Track visibility changes to auto-set ui_dirty (prevents stale rendering)
    last_file_picker_visible: bool,
    last_grep_visible: bool,
}

unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32, title_bar_height: f32) -> Self {
        let viewport = Viewport::new(size.0, size.1, scale_factor);

        Self {
            font_system: None,
            theme: None,
            viewport,
            gpu_renderer: None,
            cached_doc_text: None,
            cached_doc_version: 0,
            text_renderer: TextRenderer::new(),
            last_rendered_version: 0,
            layout_dirty: true,
            syntax_dirty: false,
            plugin_loader: None,
            lib_watchers: Vec::new(),
            config_watchers: Vec::new(),
            plugin_state: Arc::new(Mutex::new(PluginState::new())),
            last_viewport_scroll: (0.0, 0.0),
            service_registry: ServiceRegistry::new(),
            redraw_notifier: None,
            plugin_event_tx: None,
            current_tab_renderer_id: None,
            line_numbers_plugin: None,
            tab_bar_plugin: None,
            file_picker_plugin: None,
            grep_plugin: None,
            title_bar_height,
            tab_bar_height: TAB_BAR_HEIGHT, // Will be updated dynamically
            // Default editor bounds - updated in update_viewport
            editor_bounds: tiny_sdk::types::LayoutRect::new(0.0, 0.0, 800.0, 600.0),
            accumulated_glyphs: Vec::new(),
            text_decoration_rects: Vec::new(),
            line_number_glyphs: Vec::new(),
            tab_bar_glyphs: Vec::new(),
            tab_bar_rects: Vec::new(),
            file_picker_glyphs: Vec::new(), // Vec of (glyphs, scissor_rect) tuples
            file_picker_rects: Vec::new(),
            file_picker_rounded_rect: None,
            grep_glyphs: Vec::new(), // Vec of (glyphs, scissor_rect) tuples
            grep_rects: Vec::new(),
            grep_rounded_rect: None,
            scrollbar_plugin: crate::scrollbar_plugin::ScrollbarPlugin::new(),
            scrollbar_rects: Vec::new(),
            glyphs_dirty: true,
            line_numbers_dirty: true,
            ui_dirty: true,
            last_scroll: (0.0, 0.0),
            last_viewport_size: (0.0, 0.0),
            last_file_picker_visible: false,
            last_grep_visible: false,
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.viewport.set_font_size(font_size);

        // Update baseline from font metrics when font size changes
        if let Some(ref font_system) = self.font_system {
            self.viewport.metrics.baseline = font_system
                .get_baseline(self.viewport.metrics.font_size, self.viewport.scale_factor);
        }

        self.layout_dirty = true;
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
        self.ui_dirty = true;

        // Notify plugins about the viewport change
        let mut state = self.plugin_state.lock().unwrap();
        state.viewport_info = PluginState::from_viewport(&self.viewport);

        if let Some(ref loader_arc) = self.plugin_loader {
            if let Ok(mut loader) = loader_arc.lock() {
                state.sync_all(&mut loader);
            }
        }
    }

    pub fn set_line_height(&mut self, line_height: f32) {
        self.viewport.metrics.line_height = line_height;
        self.layout_dirty = true;
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
        self.ui_dirty = true;

        // Notify plugins about the viewport change
        let mut state = self.plugin_state.lock().unwrap();
        state.viewport_info = PluginState::from_viewport(&self.viewport);

        if let Some(ref loader_arc) = self.plugin_loader {
            if let Ok(mut loader) = loader_arc.lock() {
                state.sync_all(&mut loader);
            }
        }
    }

    pub fn set_gpu_renderer(&mut self, gpu_renderer: &GpuRenderer) {
        if self.gpu_renderer.is_none() {
            self.gpu_renderer = Some(gpu_renderer as *const _);
            if self.plugin_loader.is_none() {
                self.initialize_plugins(gpu_renderer);
            }
        }
    }

    fn initialize_plugins(&mut self, gpu_renderer: &GpuRenderer) {
        let app_config = AppConfig::load().unwrap_or_else(|e| {
            eprintln!("Failed to load init.toml: {}, using defaults", e);
            AppConfig::default()
        });

        let plugin_dir = std::path::PathBuf::from(&app_config.plugins.plugin_dir);
        if !plugin_dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&plugin_dir) {
                eprintln!("Failed to create plugin directory: {}", e);
                return;
            }
        }

        let mut loader = PluginLoader::new(plugin_dir.clone());
        let device = gpu_renderer.device_arc();
        let queue = gpu_renderer.queue_arc();

        for plugin_name in &app_config.plugins.enabled {
            let result = if let Some(cfg) = app_config.plugins.plugins.get(plugin_name) {
                let lib = cfg.lib_path(plugin_name, &app_config.plugins.plugin_dir);
                let config = cfg.config_path(plugin_name, &app_config.plugins.plugin_dir);
                println!("Using explicit paths - lib: {}, config: {}", lib, config);
                loader.load_plugin_from_path(plugin_name, &lib, &config)
            } else {
                loader.load_plugin(plugin_name)
            };

            if let Err(e) = result {
                eprintln!("Failed to load {} plugin: {}", plugin_name, e);
                continue;
            }

            if let Err(e) = loader.initialize_plugin(plugin_name, device.clone(), queue.clone()) {
                eprintln!("Failed to initialize {} plugin: {}", plugin_name, e);
                continue;
            }

            if plugin_name == "selection" || plugin_name == "cursor" {
                let mut state = self.plugin_state.lock().unwrap();
                state.viewport_info = PluginState::from_viewport(&self.viewport);
                state.sync_to_plugin(&mut loader, plugin_name);
            }
        }

        let loader_arc = Arc::new(Mutex::new(loader));
        self.setup_hot_reload(&app_config, &loader_arc, gpu_renderer);
        self.plugin_loader = Some(loader_arc);
    }

    fn setup_hot_reload(
        &mut self,
        config: &AppConfig,
        loader_arc: &Arc<Mutex<PluginLoader>>,
        gpu_renderer: &GpuRenderer,
    ) {
        for plugin_name in &config.plugins.enabled {
            if let Some(plugin_config) = config.plugins.plugins.get(plugin_name) {
                if plugin_config.auto_reload {
                    self.setup_lib_watcher(
                        plugin_name,
                        plugin_config,
                        config,
                        loader_arc.clone(),
                        gpu_renderer,
                    );
                }
                self.setup_config_watcher(plugin_name, plugin_config, config, loader_arc.clone());
            }
        }
    }

    fn setup_lib_watcher(
        &mut self,
        plugin_name: &str,
        plugin_config: &PluginConfig,
        app_config: &AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
        gpu_renderer: &GpuRenderer,
    ) {
        let _ = self.setup_lib_watcher_impl(
            plugin_name,
            plugin_config,
            app_config,
            loader_arc,
            gpu_renderer,
        );
    }

    fn setup_lib_watcher_impl(
        &mut self,
        plugin_name: &str,
        plugin_config: &PluginConfig,
        app_config: &AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
        gpu_renderer: &GpuRenderer,
    ) -> Result<()> {
        let lib_path = plugin_config.lib_path(plugin_name, &app_config.plugins.plugin_dir);
        let lib_path_buf = std::path::PathBuf::from(&lib_path);
        let config_path = plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);

        let plugin_name = plugin_name.to_string();
        let device = gpu_renderer.device_arc();
        let queue = gpu_renderer.queue_arc();
        let plugin_state = self.plugin_state.clone();
        let redraw_notifier = self.redraw_notifier.clone();
        let event_tx = self.plugin_event_tx.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    if event.kind.is_create() || event.kind.is_modify() {
                        Self::handle_lib_reload(
                            &loader_arc,
                            &plugin_name,
                            &lib_path,
                            &config_path,
                            device.clone(),
                            queue.clone(),
                            plugin_state.clone(),
                            redraw_notifier.clone(),
                            event_tx.clone(),
                        );
                    }
                }
            })
            .context("Failed to create lib watcher")?;

        // Try watching the file directly first, fallback to parent directory
        if watcher
            .watch(&lib_path_buf, RecursiveMode::NonRecursive)
            .is_err()
        {
            let parent = lib_path_buf.parent().context("No parent directory")?;
            watcher
                .watch(parent, RecursiveMode::NonRecursive)
                .context("Failed to watch parent directory")?;
        }

        self.lib_watchers.push(watcher);
        Ok(())
    }

    fn handle_lib_reload(
        loader_arc: &Arc<Mutex<PluginLoader>>,
        plugin_name: &str,
        lib_path: &str,
        config_path: &str,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        plugin_state: Arc<Mutex<PluginState>>,
        redraw_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
        event_tx: Option<std::sync::mpsc::Sender<String>>,
    ) {
        // Wait for file to be ready
        for _ in 0..10 {
            if let Ok(meta) = std::fs::metadata(lib_path) {
                if meta.len() > 0 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Use try_lock to avoid deadlock if loader is busy during initialization
        if let Ok(mut loader) = loader_arc.try_lock() {
            if loader.unload_plugin(plugin_name).is_err() {
                return;
            }
            if loader
                .load_plugin_from_path(plugin_name, lib_path, config_path)
                .is_err()
            {
                return;
            }
            if loader.initialize_plugin(plugin_name, device, queue).is_ok() {
                if let Ok(state) = plugin_state.lock() {
                    state.sync_to_plugin(&mut loader, plugin_name);
                }
                // Drop loader lock before sending events
                drop(loader);

                // Send event for this specific plugin reload
                eprintln!("📦 Plugin library reloaded: {} - sending event", plugin_name);
                if let Some(ref tx) = event_tx {
                    let event_name = format!("plugin.{}.reloaded", plugin_name);
                    let _ = tx.send(event_name);
                }

                // Request redraw after successful reload
                if let Some(ref notifier) = redraw_notifier {
                    eprintln!("🔄 Requesting redraw for lib reload: {}", plugin_name);
                    notifier();
                }
            }
        }
    }

    fn setup_config_watcher(
        &mut self,
        plugin_name: &str,
        plugin_config: &PluginConfig,
        app_config: &AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
    ) {
        let _ = self.setup_config_watcher_impl(plugin_name, plugin_config, app_config, loader_arc);
    }

    fn setup_config_watcher_impl(
        &mut self,
        plugin_name: &str,
        plugin_config: &PluginConfig,
        app_config: &AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
    ) -> Result<()> {
        let config_path = plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);
        let config_path_buf = std::path::PathBuf::from(&config_path);

        if !config_path_buf.exists() {
            bail!("Config path does not exist");
        }

        eprintln!("📁 Setting up config watcher for '{}': {}", plugin_name, config_path);

        let plugin_name = plugin_name.to_string();
        let plugin_state = self.plugin_state.clone();
        let redraw_notifier = self.redraw_notifier.clone();
        let event_tx = self.plugin_event_tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    // Handle both modify and create events (editors often use atomic writes)
                    if event.kind.is_modify() || event.kind.is_create() {
                        eprintln!("Config file changed (kind: {:?}): {:?}", event.kind, event.paths);
                        if let Some(path) = event.paths.first() {
                            if let Ok(data) = std::fs::read_to_string(path) {
                                // Use try_lock to avoid deadlock if loader is busy during initialization
                                if let Ok(mut loader) = loader_arc.try_lock() {
                                    // Store config data so new instances get it
                                    loader.store_plugin_config(&plugin_name, data.clone());

                                    // Update the loader's main instance
                                    let success = if let Some(plugin) = loader.get_plugin_mut(&plugin_name) {
                                        if let Some(cfg) = plugin.instance.as_configurable() {
                                            match cfg.config_updated(&data) {
                                                Ok(_) => {
                                                    eprintln!("✅ Config updated successfully for {}", plugin_name);
                                                    // Sync plugin state
                                                    if let Ok(state) = plugin_state.lock() {
                                                        state.sync_to_plugin(&mut loader, &plugin_name);
                                                    }
                                                    true
                                                }
                                                Err(e) => {
                                                    eprintln!("❌ Config update failed for {}: {:?}", plugin_name, e);
                                                    false
                                                }
                                            }
                                        } else {
                                            eprintln!("⚠️  Plugin {} doesn't implement Configurable", plugin_name);
                                            false
                                        }
                                    } else {
                                        eprintln!("⚠️  Plugin {} not found in loader", plugin_name);
                                        false
                                    };

                                    // Drop loader lock before emitting events
                                    drop(loader);

                                    if success {
                                        // Send event for config update (plugin-agnostic)
                                        eprintln!("📦 Plugin config updated: {} - sending event", plugin_name);
                                        if let Some(ref tx) = event_tx {
                                            let event_name = format!("plugin.{}.config_updated", plugin_name);
                                            let _ = tx.send(event_name);
                                        }

                                        // Request redraw
                                        if let Some(ref notifier) = redraw_notifier {
                                            eprintln!("🔄 Requesting redraw for config update: {}", plugin_name);
                                            notifier();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .context("Failed to create config watcher")?;

        watcher
            .watch(&config_path_buf, RecursiveMode::NonRecursive)
            .context("Failed to watch config file")?;

        self.config_watchers.push(watcher);
        Ok(())
    }

    pub fn set_font_system(&mut self, font_system: Arc<tiny_font::SharedFontSystem>) {
        self.viewport.set_font_system(font_system.clone());
        self.service_registry.register(font_system.clone());

        // Update baseline from font metrics
        self.viewport.metrics.baseline =
            font_system.get_baseline(self.viewport.metrics.font_size, self.viewport.scale_factor);

        self.font_system = Some(font_system);
        self.layout_dirty = true;
    }

    pub fn set_theme(&mut self, theme: tiny_ui::theme::Theme) {
        self.theme = Some(Arc::new(theme));
    }

    /// Set callback to request redraw (for config watchers and async operations)
    pub fn set_redraw_notifier<F>(&mut self, notifier: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.redraw_notifier = Some(Arc::new(notifier));
    }

    /// Set channel sender for plugin events
    pub fn set_plugin_event_channel(&mut self, tx: std::sync::mpsc::Sender<String>) {
        self.plugin_event_tx = Some(tx);
    }

    /// Get receiver for plugin events (call this to create the channel)
    pub fn create_plugin_event_channel() -> (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<String>) {
        std::sync::mpsc::channel()
    }

    pub fn set_line_numbers_plugin(
        &mut self,
        plugin: &mut crate::line_numbers_plugin::LineNumbersPlugin,
        doc: &tree::Doc,
    ) {
        plugin.set_document(doc);
        self.line_numbers_plugin = Some(plugin as *mut _);
    }

    pub fn set_tab_bar_plugin(&mut self, plugin: &mut crate::tab_bar_plugin::TabBarPlugin) {
        self.tab_bar_plugin = Some(plugin as *mut _);
    }

    pub fn set_file_picker_plugin(
        &mut self,
        plugin: &mut crate::file_picker_plugin::FilePickerPlugin,
    ) {
        self.file_picker_plugin = Some(plugin as *mut _);
    }

    pub fn set_grep_plugin(&mut self, plugin: &mut crate::grep_plugin::GrepPlugin) {
        self.grep_plugin = Some(plugin as *mut _);
    }

    /// Mark UI as dirty (call when tabs change, file picker opens, etc.)
    pub fn mark_ui_dirty(&mut self) {
        self.ui_dirty = true;
    }

    /// Swap the text renderer with the active tab's renderer
    /// This preserves per-tab rendering state (syntax highlighting, layout, etc.)
    pub fn swap_text_renderer(&mut self, tab_renderer: &mut crate::text_renderer::TextRenderer) {
        // Track renderer by pointer address to detect actual tab switches
        let incoming_id = tab_renderer as *const _ as u64;
        let switching_tabs = self.current_tab_renderer_id != Some(incoming_id);

        std::mem::swap(&mut self.text_renderer, tab_renderer);

        // Only mark dirty when switching to a different tab, not on swap-back
        if switching_tabs {
            self.current_tab_renderer_id = Some(incoming_id);
            self.glyphs_dirty = true;
            self.line_numbers_dirty = true;
            // Force layout update for new tabs by resetting last_rendered_version
            self.last_rendered_version = u64::MAX;
        }
    }

    pub fn get_gpu_renderer(&self) -> Option<*const GpuRenderer> {
        self.gpu_renderer
    }

    pub fn get_plugin_loader(&self) -> Option<&Arc<Mutex<PluginLoader>>> {
        self.plugin_loader.as_ref()
    }

    pub fn apply_incremental_edit(&mut self, edit: &tree::Edit) {
        self.text_renderer.apply_incremental_edit(edit);
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
    }

    /// Clear all caches (called when font size/metrics change)
    pub fn clear_all_caches(&mut self) {
        self.layout_dirty = true;
        self.syntax_dirty = true;
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
        self.ui_dirty = true;
        self.last_rendered_version = 0;
        self.cached_doc_text = None;
        self.cached_doc_version = 0;
    }

    pub fn clear_edit_deltas(&mut self) {
        self.text_renderer.syntax_state.edit_deltas.clear();
    }

    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        let size_changed = self.last_viewport_size != (width, height);
        let scale_changed = self.viewport.scale_factor != scale_factor;

        if size_changed || scale_changed {
            self.viewport.resize(width, height, scale_factor);
            self.update_editor_bounds(width, height);
            self.last_viewport_size = (width, height);

            // Only mark layout/glyphs dirty if scale changed (DPI change)
            // Pure size changes don't affect content positions
            if scale_changed {
                self.layout_dirty = true;
                self.glyphs_dirty = true;
                self.line_numbers_dirty = true;
                self.ui_dirty = true;
            }
        }
    }

    /// Update editor bounds based on current line numbers width and other UI elements
    fn update_editor_bounds(&mut self, width: f32, height: f32) {
        let mut offset_x = 0.0;
        let offset_y = self.title_bar_height + STATUS_BAR_HEIGHT + self.tab_bar_height;

        if let Some(plugin_ptr) = self.line_numbers_plugin {
            let plugin = unsafe { &*plugin_ptr };
            offset_x = plugin.width;

            // File explorer
            offset_x += FILE_EXPLORER_WIDTH;
        }

        // Update editor bounds with padding baked in
        self.editor_bounds = tiny_sdk::types::LayoutRect::new(
            offset_x,
            offset_y,
            width - offset_x,
            height - offset_y,
        );
    }

    /// Convert screen coordinates to editor-local coordinates
    /// This accounts for the editor bounds offset (line numbers area, title bar, etc)
    pub fn screen_to_editor_local(&self, screen_pos: crate::Point) -> crate::Point {
        crate::Point {
            x: tiny_sdk::LogicalPixels(screen_pos.x.0 - self.editor_bounds.x.0),
            y: tiny_sdk::LogicalPixels(screen_pos.y.0 - self.editor_bounds.y.0),
        }
    }

    /// Collect all glyphs to trigger rasterization (call BEFORE starting render pass)
    pub fn collect_all_glyphs(
        &mut self,
        tree: &Tree,
        tab_manager: Option<&crate::tab_manager::TabManager>,
    ) {
        let tree_version_changed = tree.version != self.last_rendered_version;

        // Check if syntax highlighter has new results (async parse completed)
        let syntax_updated = self
            .text_renderer
            .syntax_highlighter
            .as_ref()
            .map(|h| h.cached_version() > self.text_renderer.syntax_state.stable_version)
            .unwrap_or(false);

        let content_changed =
            tree_version_changed || self.layout_dirty || self.syntax_dirty || syntax_updated;

        let current_scroll = (self.viewport.scroll.x.0, self.viewport.scroll.y.0);
        let scroll_changed = current_scroll != self.last_scroll;
        if scroll_changed {
            self.glyphs_dirty = true;
            self.last_scroll = current_scroll;
        }

        // Early exit if nothing needs collection
        if !content_changed
            && !scroll_changed
            && !self.glyphs_dirty
            && !self.line_numbers_dirty
            && !self.ui_dirty
        {
            return;
        }

        if content_changed {
            self.prepare_render(tree);
        }

        // Update visible range for culling
        if content_changed || scroll_changed {
            self.text_renderer
                .update_visible_range(&self.viewport, tree);
        }

        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        if self.glyphs_dirty || content_changed || scroll_changed {
            self.accumulated_glyphs.clear();
            self.text_decoration_rects.clear();
            self.collect_main_text_glyphs(tree, visible_range.clone());
            self.glyphs_dirty = false;
        }

        if self.line_numbers_dirty || scroll_changed {
            self.line_number_glyphs.clear();
            let old_editor_bounds_x = self.editor_bounds.x.0;
            self.collect_line_number_glyphs();
            if self.line_numbers_dirty {
                let (w, h) = (
                    self.viewport.logical_size.width.0,
                    self.viewport.logical_size.height.0,
                );
                self.update_editor_bounds(w, h);
                if (old_editor_bounds_x - self.editor_bounds.x.0).abs() > 0.01 {
                    self.accumulated_glyphs.clear();
                    self.text_decoration_rects.clear();
                    self.collect_main_text_glyphs(tree, visible_range.clone());
                }
                self.line_numbers_dirty = false;
            }
        }

        if self.ui_dirty {
            self.tab_bar_glyphs.clear();
            self.tab_bar_rects.clear();
            if let Some(tab_mgr) = tab_manager {
                self.collect_tab_bar_glyphs(tab_mgr);
            }
            self.file_picker_glyphs.clear();
            self.collect_file_picker_glyphs();
            self.grep_glyphs.clear();
            self.collect_grep_glyphs();
            self.ui_dirty = false;
        }

        // Collect scrollbar (always, since it depends on scroll position)
        self.collect_scrollbar_rects(tree);

        self.last_rendered_version = tree.version;
        self.layout_dirty = false;
        self.syntax_dirty = false;
    }

    pub fn render_with_pass_and_context(
        &mut self,
        tree: &Tree,
        mut render_pass: Option<&mut wgpu::RenderPass>,
        tab_manager: Option<&crate::tab_manager::TabManager>,
    ) {
        // Auto-detect visibility changes and AGGRESSIVELY clear to prevent stale rendering
        let file_picker_visible = self
            .file_picker_plugin
            .map(|ptr| unsafe { (*ptr).visible })
            .unwrap_or(false);
        let grep_visible = self
            .grep_plugin
            .map(|ptr| unsafe { (*ptr).visible })
            .unwrap_or(false);

        // When file picker becomes hidden, immediately clear all its render data
        if self.last_file_picker_visible && !file_picker_visible {
            self.file_picker_glyphs.clear();
            self.file_picker_rects.clear();
            self.file_picker_rounded_rect = None;
        }

        // When grep becomes hidden, immediately clear all its render data
        if self.last_grep_visible && !grep_visible {
            self.grep_glyphs.clear();
            self.grep_rects.clear();
            self.grep_rounded_rect = None;
        }

        if file_picker_visible != self.last_file_picker_visible
            || grep_visible != self.last_grep_visible
        {
            self.ui_dirty = true;
            self.last_file_picker_visible = file_picker_visible;
            self.last_grep_visible = grep_visible;
        }

        let visible_range = self.viewport.visible_byte_range_with_tree(tree);

        if let Some(pass) = render_pass.as_deref_mut() {
            let scale = self.viewport.scale_factor;

            // === DRAW EDITOR CONTENT FIRST ===
            // Set scissor rect - using consistent rounding to avoid off-by-one errors
            // Small margin helps catch glyphs at edges (GPU handles final clipping)
            let scissor_margin = 2.0;
            let scissor_x = ((self.editor_bounds.x.0 - scissor_margin) * scale)
                .round()
                .max(0.0) as u32;
            let scissor_y = ((self.editor_bounds.y.0 - scissor_margin) * scale)
                .round()
                .max(0.0) as u32;
            let scissor_w = ((self.editor_bounds.width.0 + scissor_margin * 2.0) * scale)
                .round()
                .max(1.0) as u32;
            let scissor_h = ((self.editor_bounds.height.0 + scissor_margin * 2.0) * scale)
                .round()
                .max(1.0) as u32;

            // Clamp scissor rect to render target bounds to prevent overflow
            let (target_w, target_h) = (
                self.viewport.physical_size.width,
                self.viewport.physical_size.height,
            );
            let scissor_w = scissor_w.min(target_w.saturating_sub(scissor_x));
            let scissor_h = scissor_h.min(target_h.saturating_sub(scissor_y));

            pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);

            // === PAINT EDITOR IN Z-INDEX ORDER ===
            // Generic plugin rendering: collect all paintables, sort by z_index, paint

            // Collect (z_index, paint_fn) tuples
            let mut paint_ops: Vec<(i32, Box<dyn FnOnce(&mut wgpu::RenderPass)>)> = Vec::new();

            // Text decorations (z_index = -5)
            if !self.text_decoration_rects.is_empty() {
                let gpu_ptr = self.gpu_renderer;
                let rects = self.text_decoration_rects.clone();
                let scale = self.viewport.scale_factor;
                paint_ops.push((-5, Box::new(move |pass| {
                    if let Some(gpu_ptr) = gpu_ptr {
                        let gpu_mut = unsafe { &mut *(gpu_ptr as *mut GpuRenderer) };
                        gpu_mut.draw_rects(pass, &rects, scale);
                    }
                })));
            }

            // Main text (z_index = 0)
            if !self.accumulated_glyphs.is_empty() {
                let gpu_ptr = self.gpu_renderer;
                let glyphs = self.accumulated_glyphs.clone();
                let has_syntax = self.text_renderer.syntax_highlighter.is_some();
                paint_ops.push((0, Box::new(move |pass| {
                    if let Some(gpu) = gpu_ptr {
                        unsafe {
                            let gpu_renderer = &*(gpu);
                            let gpu_mut = &mut *(gpu as *mut GpuRenderer);

                            if gpu_renderer.has_styled_pipeline() {
                                let style_buffer: Vec<u32> = glyphs.iter().map(|g| g.token_id as u32).collect();
                                gpu_mut.upload_style_buffer_u32(&style_buffer);
                            }

                            let use_themed = has_syntax && gpu_renderer.has_styled_pipeline();

                            gpu_mut.draw_glyphs(
                                pass,
                                &glyphs,
                                tiny_core::gpu::DrawConfig {
                                    buffer_name: "main_text",
                                    use_themed,
                                    scissor: None,
                                },
                            );
                        }
                    }
                })));
            }

            // Collect ALL plugins from active tab (generic - works with any plugin structure)
            if let Some(tab_mgr) = tab_manager {
                if let Some(tab) = tab_mgr.active_tab() {
                    // Ask tab to provide all its paintable plugins
                    tab.collect_paint_ops(
                        &mut paint_ops,
                        self.editor_bounds,
                        self.viewport.scroll,
                        |viewport| self.make_paint_ctx(viewport),
                    );
                }
            }

            // Sort by z_index and paint
            paint_ops.sort_by_key(|(z, _)| *z);
            for (_, paint_fn) in paint_ops {
                paint_fn(pass);
            }

            // === PAINT DIAGNOSTICS (separate due to lifetime constraints) ===
            if let Some(tab_mgr) = tab_manager {
                if let Some(tab) = tab_mgr.active_tab() {
                    // Get font system from service registry (works on host side)
                    let font_system = self.service_registry.get::<tiny_font::SharedFontSystem>()
                        .expect("SharedFontSystem must be registered before painting diagnostics");

                    tab.paint_diagnostics(
                        pass,
                        self.editor_bounds,
                        self.viewport.scroll,
                        |viewport| self.make_paint_ctx(viewport),
                        &font_system,
                    );
                }
            }

            // === DRAW SCROLLBAR ===
            if !self.scrollbar_rects.is_empty() {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rounded_rects(pass, &self.scrollbar_rects, scale);
                    }
                }
            }

            // === DRAW TAB BAR BACKGROUNDS ===
            if !self.tab_bar_rects.is_empty() {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rects(pass, &self.tab_bar_rects, scale);
                    }
                }
            }

            // === DRAW LINE NUMBERS ===
            if !self.line_number_glyphs.is_empty() {
                if let Some(plugin_ptr) = self.line_numbers_plugin {
                    let plugin = unsafe { &*plugin_ptr };
                    let line_numbers_y = self.title_bar_height + self.tab_bar_height;
                    let line_numbers_bounds = tiny_sdk::types::LayoutRect::new(
                        FILE_EXPLORER_WIDTH,
                        line_numbers_y,
                        plugin.width,
                        self.viewport.logical_size.height.0 - line_numbers_y,
                    );

                    let scissor_x = (line_numbers_bounds.x.0 * scale).round().max(0.0) as u32;
                    let scissor_y = (line_numbers_bounds.y.0 * scale).round().max(0.0) as u32;
                    let scissor_w = (line_numbers_bounds.width.0 * scale).round().max(1.0) as u32;
                    let scissor_h = (line_numbers_bounds.height.0 * scale).round().max(1.0) as u32;

                    pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);

                    if let Some(gpu) = self.gpu_renderer {
                        unsafe {
                            let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                            gpu_renderer.draw_glyphs(
                                pass,
                                &self.line_number_glyphs,
                                tiny_core::gpu::DrawConfig {
                                    buffer_name: "line_numbers",
                                    use_themed: true,
                                    scissor: Some((scissor_x, scissor_y, scissor_w, scissor_h)),
                                },
                            );
                        }
                    }
                }
            }

            // === DRAW TAB BAR ===
            if !self.tab_bar_glyphs.is_empty() {
                let tab_bar_bounds = tiny_sdk::types::LayoutRect::new(
                    0.0,
                    self.title_bar_height,
                    self.viewport.logical_size.width.0,
                    self.tab_bar_height, // Use dynamically calculated height
                );

                let scissor_x = (tab_bar_bounds.x.0 * scale).round().max(0.0) as u32;
                let scissor_y = (tab_bar_bounds.y.0 * scale).round().max(0.0) as u32;
                let scissor_w = (tab_bar_bounds.width.0 * scale).round().max(1.0) as u32;
                let scissor_h = (tab_bar_bounds.height.0 * scale).round().max(1.0) as u32;

                pass.set_scissor_rect(scissor_x, scissor_y, scissor_w, scissor_h);

                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_glyphs(
                            pass,
                            &self.tab_bar_glyphs,
                            tiny_core::gpu::DrawConfig {
                                buffer_name: "tab_bar",
                                use_themed: true,
                                scissor: Some((scissor_x, scissor_y, scissor_w, scissor_h)),
                            },
                        );
                    }
                }
            }

            // === DRAW FILE PICKER OVERLAY (on top of everything) ===
            // Render rounded frame with border first
            if let Some(rounded_rect) = self.file_picker_rounded_rect {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rounded_rects(pass, &[rounded_rect], scale);
                    }
                }
            }
            // Render background rects (input/results backgrounds)
            if !self.file_picker_rects.is_empty() {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rects(pass, &self.file_picker_rects, scale);
                    }
                }
            }
            // Draw file picker text with proper scissor rects for each view
            if !self.file_picker_glyphs.is_empty() {
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_glyphs_batched(
                            pass,
                            &self.file_picker_glyphs,
                            "file_picker",
                            true,
                        );
                    }
                }
            }
            // Paint file picker input's cursor/selection plugins
            if let Some(plugin_ptr) = self.file_picker_plugin {
                let plugin = unsafe { &mut *plugin_ptr };
                if plugin.visible {
                    // Sync plugin state right before painting
                    plugin.input_mut().sync_plugins();

                    let input_bounds = plugin.picker.dropdown.input.view.viewport.bounds;
                    let input_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: input_bounds,
                        scroll: tiny_sdk::LayoutPos::new(0.0, 0.0), // Scroll already applied in view coords
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 100,
                    };
                    pass.set_scissor_rect(0, 0, target_w, target_h);
                    self.paint_editable_view_plugins(
                        &plugin.picker.dropdown.input,
                        input_viewport,
                        pass,
                    );
                }
            }

            // === DRAW GREP OVERLAY (on top of everything) ===
            // Render rounded frame with border first
            if let Some(rounded_rect) = self.grep_rounded_rect {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rounded_rects(pass, &[rounded_rect], scale);
                    }
                }
            }
            // Render background rects (input/results backgrounds)
            if !self.grep_rects.is_empty() {
                pass.set_scissor_rect(0, 0, target_w, target_h);
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_rects(pass, &self.grep_rects, scale);
                    }
                }
            }
            // Draw grep text with proper scissor rects for each view
            if !self.grep_glyphs.is_empty() {
                if let Some(gpu) = self.gpu_renderer {
                    unsafe {
                        let gpu_renderer = &mut *(gpu as *mut GpuRenderer);
                        gpu_renderer.draw_glyphs_batched(pass, &self.grep_glyphs, "grep", true);
                    }
                }
            }
            // Paint grep input's cursor/selection plugins
            if let Some(plugin_ptr) = self.grep_plugin {
                let plugin = unsafe { &mut *plugin_ptr };
                if plugin.visible {
                    // Sync plugin state right before painting
                    plugin.input_mut().sync_plugins();

                    let input_bounds = plugin.picker.dropdown.input.view.viewport.bounds;
                    let input_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: input_bounds,
                        scroll: tiny_sdk::LayoutPos::new(0.0, 0.0), // Scroll already applied in view coords
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 101,
                    };
                    pass.set_scissor_rect(0, 0, target_w, target_h);
                    self.paint_editable_view_plugins(
                        &plugin.picker.dropdown.input,
                        input_viewport,
                        pass,
                    );
                }
            }
        }

        // Update uniforms if needed
        if let Some(pass) = render_pass.as_deref_mut() {
            if let Some(gpu) = self.gpu_renderer {
                let gpu_renderer = unsafe { &*gpu };
                let (width, height) = gpu_renderer.viewport_size();
                gpu_renderer.update_uniforms(width, height);
            }

            // Draw any remaining spatial widgets
            self.walk_visible_range_no_glyphs(tree, visible_range, pass);
        }
    }

    fn prepare_render(&mut self, tree: &Tree) {
        if let Some(font_system) = &self.font_system {
            self.text_renderer
                .update_layout(tree, font_system, &self.viewport, self.layout_dirty);
            // Clear layout_dirty after using it - otherwise it stays true forever!
            self.layout_dirty = false;
        }

        if let Some(ref highlighter) = self.text_renderer.syntax_highlighter {
            // Check if tree-sitter completed a parse since last update
            let fresh_parse =
                highlighter.cached_version() > self.text_renderer.syntax_state.stable_version;

            // Only update syntax if there's something new to apply:
            // - Fresh parse has new tokens from tree-sitter
            // - OR there are accumulated edits to adjust stable tokens with
            let needs_update =
                fresh_parse || !self.text_renderer.syntax_state.edit_deltas.is_empty();

            // For new files, apply default styling if tree-sitter hasn't parsed yet
            let is_initial_render = self.text_renderer.syntax_state.stable_version == 0
                && self.text_renderer.syntax_state.stable_tokens.is_empty();

            if needs_update || is_initial_render {
                // Always try querying highlighter - it might have tokens ready even if cached_version is 0
                let effects = highlighter.get_effects_in_range(0..tree.char_count());
                let tokens: Vec<_> = effects
                    .into_iter()
                    .filter_map(|e| match e.effect {
                        text_effects::EffectType::Token(id) => Some(text_renderer::TokenRange {
                            byte_range: e.range,
                            token_id: id,
                        }),
                        _ => None,
                    })
                    .collect();

                let tokens_to_apply = if tokens.is_empty() {
                    // No tokens yet - apply default token to all text so it's visible
                    vec![text_renderer::TokenRange {
                        byte_range: 0..tree.byte_count(),
                        token_id: 0, // Default token (should be white/visible)
                    }]
                } else {
                    tokens
                };

                // Apply syntax with theme for style attributes (weight, italic, underline, strikethrough)
                let language = highlighter.language();
                self.text_renderer.update_syntax_with_theme(
                    &tokens_to_apply,
                    self.theme.as_ref().map(|t| t.as_ref()),
                    fresh_parse,
                    Some(tree),
                    self.font_system.as_ref().map(|fs| fs.as_ref()),
                    Some(&self.viewport),
                    Some(language),
                );
            }
        }

        self.text_renderer
            .update_visible_range(&self.viewport, tree);

        if self.cached_doc_text.is_none() || tree.version != self.cached_doc_version {
            self.cached_doc_text = Some(tree.flatten_to_string());
            self.cached_doc_version = tree.version;
        }
    }

    pub fn paint_plugins(&mut self, pass: &mut wgpu::RenderPass, background: bool) {
        if let Some(ref loader_arc) = self.plugin_loader {
            if let Ok(loader) = loader_arc.lock() {
                let z_filter = if background {
                    |z: i32| z < 0
                } else {
                    |z: i32| z >= 0
                };

                let editor_viewport = tiny_sdk::types::WidgetViewport {
                    bounds: self.editor_bounds,
                    scroll: self.viewport.scroll,
                    content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                    widget_id: 2,
                };

                if let Some(ctx) = self.make_paint_ctx(editor_viewport) {
                    for key in loader.list_plugins() {
                        if let Some(plugin) = loader.get_plugin(&key) {
                            if let Some(paintable) = plugin.instance.as_paintable() {
                                let z_idx = paintable.z_index();
                                if z_filter(z_idx) {
                                    paintable.paint(&ctx, pass);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Paint an EditableTextView's owned plugins (cursor/selection)
    /// Must be called with appropriate widget_viewport for the view
    fn paint_editable_view_plugins(
        &self,
        editable_view: &crate::editable_text_view::EditableTextView,
        widget_viewport: tiny_sdk::types::WidgetViewport,
        pass: &mut wgpu::RenderPass,
    ) {
        if let Some(gpu) = self.gpu_renderer {
            let gpu_renderer = unsafe { &*gpu };

            let mut ctx = PaintContext::new(
                self.viewport.to_viewport_info(),
                gpu_renderer.device_arc(),
                gpu_renderer.queue_arc(),
                gpu as *mut _,
                &self.service_registry,
            )
            .with_widget_viewport(widget_viewport);
            ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

            // Paint the view's plugins
            editable_view.paint_plugins(&ctx, pass);
        }
    }

    /// Helper to create GlyphCollector with common setup
    fn make_collector(
        &self,
        bounds: tiny_sdk::types::LayoutRect,
        scroll: tiny_sdk::LayoutPos,
        widget_id: u64,
    ) -> GlyphCollector {
        GlyphCollector::new(
            self.viewport.to_viewport_info(),
            &self.service_registry,
            tiny_sdk::types::WidgetViewport {
                bounds,
                scroll,
                content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                widget_id,
            },
        )
    }

    /// Helper to create PaintContext with common setup
    fn make_paint_ctx(
        &self,
        widget_viewport: tiny_sdk::types::WidgetViewport,
    ) -> Option<PaintContext> {
        let gpu = self.gpu_renderer?;
        let gpu_renderer = unsafe { &*gpu };
        let mut ctx = PaintContext::new(
            self.viewport.to_viewport_info(),
            gpu_renderer.device_arc(),
            gpu_renderer.queue_arc(),
            gpu as *mut _,
            &self.service_registry,
        )
        .with_widget_viewport(widget_viewport);
        ctx.gpu_context = Some(gpu_renderer.get_plugin_context());
        Some(ctx)
    }

    fn collect_line_number_glyphs(&mut self) {
        if let Some(plugin_ptr) = self.line_numbers_plugin {
            let plugin = unsafe { &mut *plugin_ptr };
            let y = self.title_bar_height + self.tab_bar_height;
            let bounds = tiny_sdk::types::LayoutRect::new(
                FILE_EXPLORER_WIDTH,
                y,
                plugin.width,
                self.viewport.logical_size.height.0 - y,
            );
            let mut collector = self.make_collector(
                bounds,
                tiny_sdk::types::LayoutPos::new(0.0, self.viewport.scroll.y.0),
                1,
            );
            plugin.collect_glyphs(&mut collector);
            self.line_number_glyphs = collector.glyphs;
        }
    }

    fn collect_tab_bar_glyphs(&mut self, tab_manager: &crate::tab_manager::TabManager) {
        if let Some(plugin_ptr) = self.tab_bar_plugin {
            let plugin = unsafe { &mut *plugin_ptr };

            // Calculate height first (hug contents based on line height)
            plugin.calculate_height(self.viewport.metrics.line_height);

            // Store the calculated height for use in draw code
            self.tab_bar_height = plugin.height;

            let bounds = tiny_sdk::types::LayoutRect::new(
                0.0,
                self.title_bar_height,
                self.viewport.logical_size.width.0,
                plugin.height,
            );
            let mut collector =
                self.make_collector(bounds, tiny_sdk::types::LayoutPos::new(0.0, 0.0), 10);

            plugin.collect_glyphs(&mut collector, tab_manager);
            self.tab_bar_glyphs = collector.glyphs;

            // Collect background rectangles
            let mut rects = plugin.collect_rects(tab_manager, self.viewport.logical_size.width.0);
            // Transform rects to screen coordinates
            for rect in &mut rects {
                rect.rect.x.0 += bounds.x.0;
                rect.rect.y.0 += bounds.y.0;
            }
            self.tab_bar_rects = rects;
        }
    }

    fn collect_file_picker_glyphs(&mut self) {
        if let Some(plugin_ptr) = self.file_picker_plugin {
            let plugin = unsafe { &mut *plugin_ptr };

            if !plugin.visible {
                self.file_picker_glyphs.clear();
                self.file_picker_rects.clear();
                self.file_picker_rounded_rect = None;
                return;
            }

            // Calculate bounds before collecting glyphs
            plugin.calculate_bounds(&self.viewport);

            let bounds = plugin.get_bounds();

            // Get rounded rect frame with border
            self.file_picker_rounded_rect = plugin.get_frame_rounded_rect();

            // Collect text buffer background rects (includes highlight)
            self.file_picker_rects = plugin.collect_background_rects();

            // Get font system for glyph collection
            let font_system = self
                .font_system
                .as_ref()
                .expect("Font system not initialized - call set_font_system first");

            // Collect glyphs with per-view scissor rects
            self.file_picker_glyphs = plugin.collect_glyphs(font_system);
        }
    }

    fn collect_grep_glyphs(&mut self) {
        if let Some(plugin_ptr) = self.grep_plugin {
            let plugin = unsafe { &mut *plugin_ptr };

            if !plugin.visible {
                self.grep_glyphs.clear();
                self.grep_rects.clear();
                self.grep_rounded_rect = None;
                return;
            }

            // Poll for async search results
            plugin.poll_results();

            // Calculate bounds before collecting glyphs
            plugin.calculate_bounds(&self.viewport);

            let bounds = plugin.get_bounds();

            // Get rounded rect frame with border
            self.grep_rounded_rect = plugin.get_frame_rounded_rect();

            // Collect text buffer background rects (includes highlight)
            self.grep_rects = plugin.collect_background_rects();

            // Get font system for glyph collection
            let font_system = self
                .font_system
                .as_ref()
                .expect("Font system not initialized - call set_font_system first");

            // Collect glyphs with per-view scissor rects
            self.grep_glyphs = plugin.collect_glyphs(font_system);
        }
    }

    /// Get theme color for a token_id as packed u32 (RGBA8)
    fn get_token_color(&self, token_id: u8) -> u32 {
        if let Some(ref theme) = self.theme {
            if let Some(style) = theme.get_token_style(token_id) {
                if let Some(color) = style.colors.first() {
                    // Convert [f32; 4] RGBA to packed u32
                    let r = (color[0] * 255.0) as u8;
                    let g = (color[1] * 255.0) as u8;
                    let b = (color[2] * 255.0) as u8;
                    let a = (color[3] * 255.0) as u8;
                    return ((r as u32) << 24)
                        | ((g as u32) << 16)
                        | ((b as u32) << 8)
                        | (a as u32);
                }
            }
        }
        0xFFFFFFFF // Default white if no theme
    }

    fn collect_text_decorations(
        &self,
        glyphs: &[&tiny_ui::text_renderer::UnifiedGlyph],
    ) -> Vec<tiny_sdk::types::RectInstance> {
        let mut decoration_rects = Vec::new();
        use tiny_sdk::types::{LayoutRect, RectInstance};

        // Group glyphs by line and decoration type, then create rect spans
        // Use line's canonical Y position (not glyph Y, which varies per glyph)
        let mut current_line_idx: Option<usize> = None;
        let mut last_search_idx = 0usize; // Hint: start search near last position (locality of reference)
        let mut underline_start_x: Option<f32> = None;
        let mut underline_end_x: Option<f32> = None;
        let mut underline_token_id: u8 = 0;
        let mut strike_start_x: Option<f32> = None;
        let mut strike_end_x: Option<f32> = None;
        let mut strike_token_id: u8 = 0;

        // Thickness: 10% of font size, minimum 1px (in logical pixels)
        let thickness_logical =
            (self.viewport.metrics.font_size * 0.1).max(1.0 / self.viewport.scale_factor);
        let underline_thickness = thickness_logical;
        let strike_thickness = thickness_logical;

        for glyph in glyphs.iter() {
            // Work in logical pixels (LayoutRect expects logical coordinates)
            let view_x = glyph.layout_pos.x.0 - self.viewport.scroll.x.0;
            let screen_x = view_x + self.editor_bounds.x.0;
            let glyph_width_logical = glyph.physical_width / self.viewport.scale_factor;

            // Find which line this glyph belongs to using locality hint
            // Glyphs come in visual order, so hint-based search is O(1) amortized
            let glyph_line_idx = self
                .text_renderer
                .line_cache
                .find_by_y_position_hint(glyph.layout_pos.y.0, last_search_idx);

            if let Some(idx) = glyph_line_idx {
                last_search_idx = idx; // Update hint for next glyph
            }

            // Check if we moved to a new line
            if current_line_idx != glyph_line_idx {
                // Flush previous line's decorations using line's canonical Y
                if let Some(prev_line_idx) = current_line_idx {
                    if let Some(line_info) = self.text_renderer.line_cache.get(prev_line_idx) {
                        let line_view_y = line_info.y_position - self.viewport.scroll.y.0;
                        let line_screen_y = line_view_y + self.editor_bounds.y.0;

                        if let (Some(start), Some(end)) = (underline_start_x, underline_end_x) {
                            // Underline: baseline + small offset (10% of font size)
                            let underline_y = line_screen_y
                                + self.viewport.metrics.baseline
                                + self.viewport.metrics.font_size * 0.1;

                            // Get color from theme based on token_id
                            let color = self.get_token_color(underline_token_id);

                            decoration_rects.push(RectInstance {
                                rect: LayoutRect::new(
                                    start,
                                    underline_y,
                                    end - start,
                                    underline_thickness,
                                ),
                                color,
                            });
                        }
                        if let (Some(start), Some(end)) = (strike_start_x, strike_end_x) {
                            // Strikethrough: middle of line height
                            let strike_y = line_screen_y + self.viewport.metrics.line_height * 0.5;

                            // Get color from theme based on token_id
                            let color = self.get_token_color(strike_token_id);

                            decoration_rects.push(RectInstance {
                                rect: LayoutRect::new(
                                    start,
                                    strike_y,
                                    end - start,
                                    strike_thickness,
                                ),
                                color,
                            });
                        }
                    }
                }

                // Reset for new line
                current_line_idx = glyph_line_idx;
                underline_start_x = None;
                underline_end_x = None;
                strike_start_x = None;
                strike_end_x = None;
            }

            // Track underline spans
            if glyph.underline {
                if underline_start_x.is_none() {
                    underline_start_x = Some(screen_x);
                    underline_token_id = glyph.token_id as u8;
                }
                // Extend span to include this glyph's full width
                underline_end_x = Some(screen_x + glyph_width_logical);
            } else if underline_start_x.is_some() {
                // End of underline span within same line - flush it
                if let Some(line_idx) = current_line_idx {
                    if let Some(line_info) = self.text_renderer.line_cache.get(line_idx) {
                        let line_view_y = line_info.y_position - self.viewport.scroll.y.0;
                        let line_screen_y = line_view_y + self.editor_bounds.y.0;
                        let underline_y = line_screen_y
                            + self.viewport.metrics.baseline
                            + self.viewport.metrics.font_size * 0.1;
                        let color = self.get_token_color(underline_token_id);
                        decoration_rects.push(RectInstance {
                            rect: LayoutRect::new(
                                underline_start_x.unwrap(),
                                underline_y,
                                underline_end_x.unwrap() - underline_start_x.unwrap(),
                                underline_thickness,
                            ),
                            color,
                        });
                    }
                }
                underline_start_x = None;
                underline_end_x = None;
            }

            // Track strikethrough spans
            if glyph.strikethrough {
                if strike_start_x.is_none() {
                    strike_start_x = Some(screen_x);
                    strike_token_id = glyph.token_id as u8;
                }
                strike_end_x = Some(screen_x + glyph_width_logical);
            } else if strike_start_x.is_some() {
                // End of strike span within same line - flush it
                if let Some(line_idx) = current_line_idx {
                    if let Some(line_info) = self.text_renderer.line_cache.get(line_idx) {
                        let line_view_y = line_info.y_position - self.viewport.scroll.y.0;
                        let line_screen_y = line_view_y + self.editor_bounds.y.0;
                        let strike_y = line_screen_y + self.viewport.metrics.line_height * 0.5;
                        let color = self.get_token_color(strike_token_id);
                        decoration_rects.push(RectInstance {
                            rect: LayoutRect::new(
                                strike_start_x.unwrap(),
                                strike_y,
                                strike_end_x.unwrap() - strike_start_x.unwrap(),
                                strike_thickness,
                            ),
                            color,
                        });
                    }
                }
                strike_start_x = None;
                strike_end_x = None;
            }
        }

        // Flush any remaining decorations from last line
        if let Some(line_idx) = current_line_idx {
            if let Some(line_info) = self.text_renderer.line_cache.get(line_idx) {
                let line_view_y = line_info.y_position - self.viewport.scroll.y.0;
                let line_screen_y = line_view_y + self.editor_bounds.y.0;

                if let (Some(start), Some(end)) = (underline_start_x, underline_end_x) {
                    let underline_y = line_screen_y
                        + self.viewport.metrics.baseline
                        + self.viewport.metrics.font_size * 0.1;
                    let color = self.get_token_color(underline_token_id);
                    decoration_rects.push(RectInstance {
                        rect: LayoutRect::new(start, underline_y, end - start, underline_thickness),
                        color,
                    });
                }
                if let (Some(start), Some(end)) = (strike_start_x, strike_end_x) {
                    let strike_y = line_screen_y + self.viewport.metrics.line_height * 0.5;
                    let color = self.get_token_color(strike_token_id);
                    decoration_rects.push(RectInstance {
                        rect: LayoutRect::new(start, strike_y, end - start, strike_thickness),
                        color,
                    });
                }
            }
        }

        decoration_rects
    }

    fn collect_main_text_glyphs(&mut self, tree: &Tree, visible_range: std::ops::Range<usize>) {
        let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

        // Collect decoration rectangles (underline, strikethrough)
        let decoration_rects = self.collect_text_decorations(&visible_glyphs);
        self.text_decoration_rects.extend(decoration_rects);

        let glyph_instances: Vec<_> = visible_glyphs
            .iter()
            .map(|&g| {
                // Apply scroll to get view position
                let view_x = g.layout_pos.x.0 - self.viewport.scroll.x.0;
                let view_y = g.layout_pos.y.0 - self.viewport.scroll.y.0;

                // Add editor bounds offset and scale to physical coordinates
                let physical_x = (view_x + self.editor_bounds.x.0) * self.viewport.scale_factor;
                let physical_y = (view_y + self.editor_bounds.y.0) * self.viewport.scale_factor;

                // Don't set format flags - decorations are now drawn as separate rects
                GlyphInstance {
                    pos: LayoutPos::new(physical_x, physical_y),
                    tex_coords: g.tex_coords,
                    token_id: g.token_id as u8,
                    relative_pos: g.relative_pos,
                    shader_id: 0,
                    format: 0, // Decorations drawn as separate rects
                    atlas_index: g.atlas_index,
                    _padding: 0,
                }
            })
            .collect();

        self.accumulated_glyphs.extend(glyph_instances);
    }


    fn walk_visible_range_no_glyphs(
        &mut self,
        tree: &Tree,
        visible_range: std::ops::Range<usize>,
        pass: &mut wgpu::RenderPass,
    ) {
        if let Some(gpu_ptr) = self.gpu_renderer {
            let gpu_renderer = unsafe { &*gpu_ptr };

            tree.walk_visible_range(visible_range, |spans, _, _| {
                for span in spans {
                    if let tree::Span::Spatial(widget) = span {
                        let ctx = PaintContext::new(
                            self.viewport.to_viewport_info(),
                            gpu_renderer.device_arc(),
                            gpu_renderer.queue_arc(),
                            gpu_ptr as *mut _,
                            &self.service_registry,
                        );
                        widget.paint(&ctx, pass);
                    }
                }
            });
        }
    }

    fn collect_scrollbar_rects(&mut self, tree: &Tree) {
        self.scrollbar_rects.clear();

        // Scrollbar visibility is controlled by hover state (updated in app.rs)

        // Calculate content height
        let content_height =
            self.text_renderer.line_cache.len() as f32 * self.viewport.metrics.line_height;

        // Convert editor_bounds to Rect
        let editor_rect = tiny_core::tree::Rect {
            x: tiny_sdk::LogicalPixels(self.editor_bounds.x.0),
            y: tiny_sdk::LogicalPixels(self.editor_bounds.y.0),
            width: tiny_sdk::LogicalPixels(self.editor_bounds.width.0),
            height: tiny_sdk::LogicalPixels(self.editor_bounds.height.0),
        };

        // Collect scrollbar rects
        let rects = self.scrollbar_plugin.collect_rounded_rects(
            &self.viewport,
            &editor_rect,
            content_height,
        );

        self.scrollbar_rects = rects;

        // If scrollbar needs a redraw soon (for timeout), mark ui_dirty to keep rendering
        if self.scrollbar_plugin.needs_redraw_soon() {
            self.ui_dirty = true;
        }
    }
}

// Helper struct for collecting glyphs from plugins
pub struct GlyphCollector {
    pub glyphs: Vec<GlyphInstance>,
    pub viewport: tiny_sdk::ViewportInfo,
    pub widget_viewport: Option<tiny_sdk::types::WidgetViewport>,
    services: *const ServiceRegistry,
}

impl GlyphCollector {
    fn new(
        viewport: tiny_sdk::ViewportInfo,
        services: &ServiceRegistry,
        widget_viewport: tiny_sdk::types::WidgetViewport,
    ) -> Self {
        Self {
            glyphs: Vec::new(),
            viewport,
            widget_viewport: Some(widget_viewport),
            services: services as *const _,
        }
    }

    pub fn add_glyphs(&mut self, glyphs: Vec<GlyphInstance>) {
        self.glyphs.extend(glyphs);
    }

    pub fn services(&self) -> &ServiceRegistry {
        unsafe { &*self.services }
    }

    /// Configure a viewport with current font metrics and font system
    /// Call this instead of manually setting font_size/line_height/font_system
    pub fn configure_viewport(&self, viewport: &mut Viewport) {
        viewport.metrics.font_size = self.viewport.font_size;
        viewport.metrics.line_height = self.viewport.line_height;
        viewport.scale_factor = self.viewport.scale_factor;
        if let Some(font_system) = self.services().get::<tiny_font::SharedFontSystem>() {
            viewport.set_font_system(font_system.clone());
        }
    }
}
