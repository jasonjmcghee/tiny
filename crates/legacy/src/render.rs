//! Renderer manages widget rendering and viewport transformations

use crate::{
    coordinates::Viewport, syntax,
    text_effects::{self, TextStyleProvider},
    text_renderer::{self, TextRenderer},
};
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
    pub syntax_highlighter: Option<Arc<syntax::SyntaxHighlighter>>,
    pub font_system: Option<Arc<tiny_font::SharedFontSystem>>,
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
    pub line_numbers_plugin: Option<*mut crate::line_numbers_plugin::LineNumbersPlugin>,
    pub diagnostics_plugin: Option<*mut diagnostics_plugin::DiagnosticsPlugin>,
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
            syntax_highlighter: None,
            font_system: None,
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
            line_numbers_plugin: None,
            diagnostics_plugin: None,
            tab_bar_plugin: None,
            file_picker_plugin: None,
            grep_plugin: None,
            title_bar_height,
            tab_bar_height: TAB_BAR_HEIGHT, // Will be updated dynamically
            // Default editor bounds - updated in update_viewport
            editor_bounds: tiny_sdk::types::LayoutRect::new(0.0, 0.0, 800.0, 600.0),
            accumulated_glyphs: Vec::new(),
            line_number_glyphs: Vec::new(),
            tab_bar_glyphs: Vec::new(),
            tab_bar_rects: Vec::new(),
            file_picker_glyphs: Vec::new(), // Vec of (glyphs, scissor_rect) tuples
            file_picker_rects: Vec::new(),
            file_picker_rounded_rect: None,
            grep_glyphs: Vec::new(), // Vec of (glyphs, scissor_rect) tuples
            grep_rects: Vec::new(),
            grep_rounded_rect: None,
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
        let app_config = crate::config::AppConfig::load().unwrap_or_else(|e| {
            eprintln!("Failed to load init.toml: {}, using defaults", e);
            crate::config::AppConfig::default()
        });

        let plugin_dir = std::path::PathBuf::from(&app_config.plugins.plugin_dir);
        if !plugin_dir.exists() {
            return;
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

            eprintln!("Initialized {} plugin with GPU resources", plugin_name);
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
        config: &crate::config::AppConfig,
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
        plugin_config: &crate::config::PluginConfig,
        app_config: &crate::config::AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
        gpu_renderer: &GpuRenderer,
    ) {
        let lib_path = plugin_config.lib_path(plugin_name, &app_config.plugins.plugin_dir);
        let lib_path_buf = std::path::PathBuf::from(&lib_path);
        let config_path = plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);

        let plugin_name = plugin_name.to_string();
        let device = gpu_renderer.device_arc();
        let queue = gpu_renderer.queue_arc();
        let plugin_state = self.plugin_state.clone();

        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
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
                    );
                }
            }
        });

        if let Ok(mut watcher) = watcher {
            if watcher
                .watch(&lib_path_buf, RecursiveMode::NonRecursive)
                .is_err()
            {
                if let Some(parent) = lib_path_buf.parent() {
                    if watcher.watch(parent, RecursiveMode::NonRecursive).is_ok() {
                        self.lib_watchers.push(watcher);
                    }
                }
            } else {
                self.lib_watchers.push(watcher);
            }
        }
    }

    fn handle_lib_reload(
        loader_arc: &Arc<Mutex<PluginLoader>>,
        plugin_name: &str,
        lib_path: &str,
        config_path: &str,
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        plugin_state: Arc<Mutex<PluginState>>,
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

        if let Ok(mut loader) = loader_arc.lock() {
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
                eprintln!("Successfully hot-reloaded plugin: {}", plugin_name);

                // Restore plugin state after hot-reload
                if let Ok(state) = plugin_state.lock() {
                    state.sync_to_plugin(&mut loader, plugin_name);
                    eprintln!("Restored state to {} plugin after hot-reload", plugin_name);
                }
            }
        }
    }

    fn setup_config_watcher(
        &mut self,
        plugin_name: &str,
        plugin_config: &crate::config::PluginConfig,
        app_config: &crate::config::AppConfig,
        loader_arc: Arc<Mutex<PluginLoader>>,
    ) {
        let config_path = plugin_config.config_path(plugin_name, &app_config.plugins.plugin_dir);
        let config_path_buf = std::path::PathBuf::from(&config_path);

        if !config_path_buf.exists() {
            return;
        }

        let plugin_name = plugin_name.to_string();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if let Ok(event) = res {
                if event.kind.is_modify() {
                    if let Ok(data) = std::fs::read_to_string(&event.paths[0]) {
                        if let Ok(mut loader) = loader_arc.lock() {
                            if let Some(plugin) = loader.get_plugin_mut(&plugin_name) {
                                if let Some(cfg) = plugin.instance.as_configurable() {
                                    if cfg.config_updated(&data).is_ok() {
                                        eprintln!("Updated config for plugin: {}", plugin_name);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        if let Ok(mut watcher) = watcher {
            if watcher
                .watch(&config_path_buf, RecursiveMode::NonRecursive)
                .is_ok()
            {
                self.config_watchers.push(watcher);
            }
        }
    }

    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<syntax::SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
        self.syntax_dirty = true;
    }

    pub fn set_font_system(&mut self, font_system: Arc<tiny_font::SharedFontSystem>) {
        self.viewport.set_font_system(font_system.clone());
        self.service_registry.register(font_system.clone());
        self.font_system = Some(font_system);
        self.layout_dirty = true;
    }

    pub fn set_line_numbers_plugin(
        &mut self,
        plugin: &mut crate::line_numbers_plugin::LineNumbersPlugin,
        doc: &tree::Doc,
    ) {
        plugin.set_document(doc);
        self.line_numbers_plugin = Some(plugin as *mut _);
    }

    pub fn set_diagnostics_plugin(
        &mut self,
        plugin: &mut diagnostics_plugin::DiagnosticsPlugin,
        _doc: &tree::Doc,
    ) {
        // Update the plugin's viewport info with the correct scale factor
        plugin.set_viewport_info(self.viewport.to_viewport_info());
        self.diagnostics_plugin = Some(plugin as *mut _);
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

    /// Mark everything dirty (call when swapping tabs or major changes)
    pub fn mark_all_dirty(&mut self) {
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
        self.ui_dirty = true;
    }

    /// Swap the text renderer with the active tab's renderer
    /// This preserves per-tab rendering state (syntax highlighting, layout, etc.)
    pub fn swap_text_renderer(&mut self, tab_renderer: &mut crate::text_renderer::TextRenderer) {
        std::mem::swap(&mut self.text_renderer, tab_renderer);
        // Mark glyphs dirty since we switched to different content
        self.glyphs_dirty = true;
        self.line_numbers_dirty = true;
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

    /// Clear edit deltas (called after undo/redo when tree is replaced)
    pub fn clear_edit_deltas(&mut self) {
        self.text_renderer.syntax_state.edit_deltas.clear();
    }

    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        let size_changed = self.last_viewport_size != (width, height);
        let scale_changed = self.viewport.scale_factor != scale_factor;

        // Only do expensive resize/relayout if something actually changed
        if size_changed || scale_changed {
            self.viewport.resize(width, height, scale_factor);
            self.layout_dirty = true;
            self.glyphs_dirty = true;
            self.line_numbers_dirty = true;
            self.ui_dirty = true;
            self.last_viewport_size = (width, height);
        }

        self.update_editor_bounds(width, height);
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

        // Check if scroll changed
        let current_scroll = (self.viewport.scroll.x.0, self.viewport.scroll.y.0);
        let scroll_changed = current_scroll != self.last_scroll;
        if scroll_changed {
            self.glyphs_dirty = true;
            self.last_scroll = current_scroll;
        }

        let content_changed =
            tree.version != self.last_rendered_version || self.layout_dirty || self.syntax_dirty;

        // Early exit if nothing changed at all
        if !content_changed
            && !scroll_changed
            && !self.glyphs_dirty
            && !self.line_numbers_dirty
            && !self.ui_dirty
        {
            return;
        }

        // Only prepare render if content actually changed
        if content_changed {
            self.prepare_render(tree);
        }

        // Only regenerate glyphs if something actually changed
        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        if self.glyphs_dirty || content_changed || scroll_changed {
            self.accumulated_glyphs.clear();
            self.collect_main_text_glyphs(tree, visible_range.clone());
            // Note: cursor/selection rects will be collected from app.rs when needed
            self.glyphs_dirty = false;
        }

        if let Some(pass) = render_pass.as_deref_mut() {
            let scale = self.viewport.scale_factor;

            // === COLLECT GLYPHS ===
            // Only regenerate line numbers if dirty (but scroll will still update every call)
            if self.line_numbers_dirty || scroll_changed {
                self.line_number_glyphs.clear();
                let old_editor_bounds_x = self.editor_bounds.x.0;
                self.collect_line_number_glyphs();

                // Only update editor bounds if content actually changed (not just scroll)
                if self.line_numbers_dirty {
                    // Update editor bounds in case line numbers width changed
                    let (w, h) = (self.viewport.logical_size.width.0, self.viewport.logical_size.height.0);
                    self.update_editor_bounds(w, h);
                    // If editor X position changed (line numbers width changed), regenerate main text
                    if (old_editor_bounds_x - self.editor_bounds.x.0).abs() > 0.01 {
                        self.accumulated_glyphs.clear();
                        self.collect_main_text_glyphs(tree, visible_range.clone());
                    }
                    self.line_numbers_dirty = false;
                }
            }


            // Only regenerate UI if dirty
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

            // Paint main editor's selection (background)
            if let Some(tab_mgr) = tab_manager {
                if let Some(tab) = tab_mgr.active_tab() {
                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: self.editor_bounds,
                        scroll: tiny_sdk::LayoutPos::new(0.0, 0.0), // Scroll already applied in view coords
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 2,
                    };
                    // Only paint selection (z_index < 0)
                    if let Some(gpu) = self.gpu_renderer {
                        let gpu_renderer = unsafe { &*gpu };
                        let mut ctx = PaintContext::new(
                            self.viewport.to_viewport_info(),
                            gpu_renderer.device_arc(),
                            gpu_renderer.queue_arc(),
                            gpu as *mut _,
                            &self.service_registry,
                        )
                        .with_widget_viewport(editor_viewport);
                        ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

                        // Paint selection only (background)
                        if let Some(ref plugin) = tab.plugin.editor.selection_plugin {
                            if let Some(paintable) = plugin.as_paintable() {
                                if paintable.z_index() < 0 {
                                    paintable.paint(&ctx, pass);
                                }
                            }
                        }
                    }
                }
            }

            // Draw main text
            self.draw_all_accumulated_glyphs(pass);

            // Paint main editor's cursor (foreground)
            if let Some(tab_mgr) = tab_manager {
                if let Some(tab) = tab_mgr.active_tab() {
                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: self.editor_bounds,
                        scroll: tiny_sdk::LayoutPos::new(0.0, 0.0), // Scroll already applied in view coords
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 2,
                    };

                    // Debug: Print editor bounds vs TextView bounds
                    if tab.plugin.editor.id == 0 {
                        eprintln!("PAINT: editor_bounds=({:.1}, {:.1}), TextView.bounds=({:.1}, {:.1})",
                            editor_viewport.bounds.x.0, editor_viewport.bounds.y.0,
                            tab.plugin.editor.view.viewport.bounds.x.0, tab.plugin.editor.view.viewport.bounds.y.0);
                    }

                    if let Some(gpu) = self.gpu_renderer {
                        let gpu_renderer = unsafe { &*gpu };
                        let mut ctx = PaintContext::new(
                            self.viewport.to_viewport_info(),
                            gpu_renderer.device_arc(),
                            gpu_renderer.queue_arc(),
                            gpu as *mut _,
                            &self.service_registry,
                        )
                        .with_widget_viewport(editor_viewport);
                        ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

                        // Paint cursor only (foreground)
                        if let Some(ref plugin) = tab.plugin.editor.cursor_plugin {
                            if let Some(paintable) = plugin.as_paintable() {
                                if paintable.z_index() >= 0 {
                                    paintable.paint(&ctx, pass);
                                }
                            }
                        }
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
                    self.paint_editable_view_plugins(&plugin.picker.dropdown.input, input_viewport, pass);
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
                    self.paint_editable_view_plugins(&plugin.picker.dropdown.input, input_viewport, pass);
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

        self.last_rendered_version = tree.version;
        self.layout_dirty = false;
        self.syntax_dirty = false;
    }

    fn prepare_render(&mut self, tree: &Tree) {
        // TextRenderer now outputs canonical (0,0)-relative glyphs
        // Bounds are applied when collecting glyphs for rendering

        if let Some(font_system) = &self.font_system {
            // Force layout update if layout is marked dirty (e.g., after font size change)
            self.text_renderer
                .update_layout(tree, font_system, &self.viewport, self.layout_dirty);
        }

        if let Some(ref highlighter) = self.syntax_highlighter {
            // Check if tree-sitter completed a parse since last update
            let fresh_parse =
                highlighter.cached_version() > self.text_renderer.syntax_state.stable_version;

            // Only update syntax if there's something new to apply:
            // - Fresh parse has new tokens from tree-sitter
            // - OR there are accumulated edits to adjust stable tokens with
            let needs_update =
                fresh_parse || !self.text_renderer.syntax_state.edit_deltas.is_empty();

            if needs_update {
                let tokens: Vec<_> = if fresh_parse {
                    // Tree-sitter has caught up - query it for fresh tokens
                    let effects = highlighter.get_effects_in_range(0..tree.char_count());
                    effects
                        .into_iter()
                        .filter_map(|e| match e.effect {
                            text_effects::EffectType::Token(id) => {
                                Some(text_renderer::TokenRange {
                                    byte_range: e.range,
                                    token_id: id,
                                })
                            }
                            _ => None,
                        })
                        .collect()
                } else {
                    // Use stable tokens from last parse with edit adjustment
                    self.text_renderer
                        .syntax_state
                        .stable_tokens
                        .iter()
                        .map(|t| text_renderer::TokenRange {
                            byte_range: t.byte_range.clone(),
                            token_id: t.token_id,
                        })
                        .collect()
                };

                self.text_renderer.update_syntax(&tokens, fresh_parse);
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

                if let Some(gpu) = self.gpu_renderer {
                    let gpu_renderer = unsafe { &*gpu };

                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: self.editor_bounds,
                        scroll: self.viewport.scroll,
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 2,
                    };

                    let mut ctx = PaintContext::new(
                        self.viewport.to_viewport_info(),
                        gpu_renderer.device_arc(),
                        gpu_renderer.queue_arc(),
                        gpu as *mut _,
                        &self.service_registry,
                    )
                    .with_widget_viewport(editor_viewport);
                    ctx.gpu_context = Some(gpu_renderer.get_plugin_context());

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

    fn collect_line_number_glyphs(&mut self) {
        if let Some(plugin_ptr) = self.line_numbers_plugin {
            let plugin = unsafe { &mut *plugin_ptr };

            let line_numbers_y = self.title_bar_height + self.tab_bar_height;

            let line_numbers_bounds = tiny_sdk::types::LayoutRect::new(
                FILE_EXPLORER_WIDTH,
                line_numbers_y,
                plugin.width,
                self.viewport.logical_size.height.0 - line_numbers_y,
            );

            let widget_viewport = tiny_sdk::types::WidgetViewport {
                bounds: line_numbers_bounds,
                scroll: tiny_sdk::types::LayoutPos::new(0.0, self.viewport.scroll.y.0),
                content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                widget_id: 1,
            };

            let mut collector = GlyphCollector::new(
                self.viewport.to_viewport_info(),
                &self.service_registry,
                widget_viewport,
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

            let tab_bar_bounds = tiny_sdk::types::LayoutRect::new(
                0.0,
                self.title_bar_height,
                self.viewport.logical_size.width.0,
                plugin.height,  // Use dynamic height from plugin
            );

            let widget_viewport = tiny_sdk::types::WidgetViewport {
                bounds: tab_bar_bounds,
                scroll: tiny_sdk::types::LayoutPos::new(0.0, 0.0), // No scroll for tab bar
                content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                widget_id: 10,
            };

            let mut collector = GlyphCollector::new(
                self.viewport.to_viewport_info(),
                &self.service_registry,
                widget_viewport,
            );

            plugin.collect_glyphs(&mut collector, tab_manager);
            self.tab_bar_glyphs = collector.glyphs;

            // Collect background rectangles
            let viewport_width = self.viewport.logical_size.width.0;
            let mut rects = plugin.collect_rects(tab_manager, viewport_width);
            // Transform rects to screen coordinates
            for rect in &mut rects {
                rect.rect.x.0 += tab_bar_bounds.x.0;
                rect.rect.y.0 += tab_bar_bounds.y.0;
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

    fn collect_main_text_glyphs(&mut self, tree: &Tree, visible_range: std::ops::Range<usize>) {
        let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

        let glyph_instances: Vec<_> = visible_glyphs
            .into_iter()
            .map(|g| {
                // Apply scroll to get view position
                let view_x = g.layout_pos.x.0 - self.viewport.scroll.x.0;
                let view_y = g.layout_pos.y.0 - self.viewport.scroll.y.0;

                // Add editor bounds offset and scale to physical coordinates
                let physical_x = (view_x + self.editor_bounds.x.0) * self.viewport.scale_factor;
                let physical_y = (view_y + self.editor_bounds.y.0) * self.viewport.scale_factor;

                GlyphInstance {
                    pos: LayoutPos::new(physical_x, physical_y),
                    tex_coords: g.tex_coords,
                    token_id: g.token_id as u8,
                    relative_pos: g.relative_pos,
                    shader_id: 0,
                    format: 0,
                    _padding: [0; 2],
                }
            })
            .collect();

        self.accumulated_glyphs.extend(glyph_instances);
    }

    fn draw_all_accumulated_glyphs(&mut self, pass: &mut wgpu::RenderPass) {
        if self.accumulated_glyphs.is_empty() {
            return;
        }

        if let Some(gpu) = self.gpu_renderer {
            unsafe {
                let gpu_renderer = &*(gpu);
                let gpu_mut = &mut *(gpu as *mut GpuRenderer);

                // Upload style buffer for all glyphs
                if gpu_renderer.has_styled_pipeline() {
                    let style_buffer: Vec<u32> = self
                        .accumulated_glyphs
                        .iter()
                        .map(|g| g.token_id as u32)
                        .collect();
                    gpu_mut.upload_style_buffer_u32(&style_buffer);
                }

                let use_themed =
                    self.syntax_highlighter.is_some() && gpu_renderer.has_styled_pipeline();

                gpu_mut.draw_glyphs(
                    pass,
                    &self.accumulated_glyphs,
                    tiny_core::gpu::DrawConfig {
                        buffer_name: "main_text",
                        use_themed,
                        scissor: None, // Scissor already set by caller
                    },
                );
            }
        }
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
}
