//! Renderer manages widget rendering and viewport transformations

use crate::{
    coordinates::Viewport,
    input, syntax, text_effects,
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
                            // Use ViewPos which is Pod/Zeroable
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
    pub text_styles: Option<Box<dyn text_effects::TextStyleProvider>>,
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
    /// Editor widget bounds (where main text renders)
    pub editor_bounds: tiny_sdk::types::LayoutRect,
    /// Accumulated glyphs for batched rendering
    accumulated_glyphs: Vec<GlyphInstance>,
    /// Line number glyphs (rendered separately)
    line_number_glyphs: Vec<GlyphInstance>,
}

unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

impl Renderer {
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        let mut viewport = Viewport::new(size.0, size.1, scale_factor);
        // Ensure margin is 0 - we use editor bounds instead
        viewport.margin = LayoutPos::new(0.0, 0.0);

        Self {
            text_styles: None,
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
            // Default editor bounds - will be updated based on layout
            editor_bounds: tiny_sdk::types::LayoutRect::new(0.0, 0.0, 800.0, 600.0),
            accumulated_glyphs: Vec::new(),
            line_number_glyphs: Vec::new(),
        }
    }

    pub fn set_font_size(&mut self, font_size: f32) {
        self.viewport.set_font_size(font_size);
        self.layout_dirty = true;

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

    pub fn set_text_styles(&mut self, provider: Box<dyn text_effects::TextStyleProvider>) {
        let adapter = crate::text_style_box_adapter::BoxedTextStyleAdapter::from_ref(&provider);
        self.service_registry.register(adapter);
        self.text_styles = Some(provider);
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

    pub fn get_gpu_renderer(&self) -> Option<*const GpuRenderer> {
        self.gpu_renderer
    }

    pub fn apply_incremental_edit(&mut self, edit: &tree::Edit) {
        self.text_renderer.apply_incremental_edit(edit);
    }

    pub fn update_viewport(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.viewport.resize(width, height, scale_factor);
        self.layout_dirty = true;

        let mut offset_x = 0.0;
        let mut offset_y = self.viewport.global_margin.y.0 + STATUS_BAR_HEIGHT;
        if let Some(plugin_ptr) = self.line_numbers_plugin {
            let plugin = unsafe { &*plugin_ptr };
            offset_x = plugin.width;

            // File explorer
            offset_x += FILE_EXPLORER_WIDTH;
        }

        // Update editor bounds based on new viewport size
        self.editor_bounds = tiny_sdk::types::LayoutRect::new(
            offset_x, // Start after line numbers
            offset_y,
            width - offset_x, // Rest of the width
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

    pub fn set_selection_plugin(&mut self, input_handler: &input::InputHandler, doc: &tree::Doc) {
        let (cursor_pos, selections) = input_handler.get_selection_data(doc, &self.viewport);

        // Cursor and selection positions are now in document layout space
        // (no global_margin included after our fix to coordinates.rs)
        // They match how text is rendered (starting at 0,0 in editor space)
        let transformed_cursor_pos = cursor_pos;

        let transformed_selections: Vec<(tiny_sdk::ViewPos, tiny_sdk::ViewPos)> = selections;

        let current_scroll = (self.viewport.scroll.x.0, self.viewport.scroll.y.0);
        let viewport_changed = current_scroll != self.last_viewport_scroll;

        // Update plugin state
        let mut state = self.plugin_state.lock().unwrap();
        let mut needs_sync = false;

        if viewport_changed {
            state.viewport_info = PluginState::from_viewport(&self.viewport);
            self.last_viewport_scroll = current_scroll;
            needs_sync = true;
        }

        if state.selections != transformed_selections {
            state.selections = transformed_selections.clone();
            needs_sync = true;
        }

        if let Some(pos) = transformed_cursor_pos {
            let view_pos = self.viewport.layout_to_view(pos);
            let cursor_changed = state.cursor_pos != Some((view_pos.x.0, view_pos.y.0));
            if cursor_changed {
                state.cursor_pos = Some((view_pos.x.0, view_pos.y.0));
                needs_sync = true;
            }
        }

        // Sync to plugins if anything changed
        if needs_sync {
            if let Some(ref loader_arc) = self.plugin_loader {
                if let Ok(mut loader) = loader_arc.lock() {
                    state.sync_all(&mut loader);
                }
            }
        }
    }

    pub fn render_with_pass_and_context(
        &mut self,
        tree: &Tree,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        if tree.version == self.last_rendered_version && !self.layout_dirty && !self.syntax_dirty {
            return;
        }

        self.prepare_render(tree);

        // Clear accumulated glyphs from previous frame
        self.accumulated_glyphs.clear();

        let visible_range = self.viewport.visible_byte_range_with_tree(tree);
        self.collect_main_text_glyphs(tree, visible_range.clone());

        if let Some(pass) = render_pass.as_deref_mut() {
            let scale = self.viewport.scale_factor;

            // === COLLECT GLYPHS ===
            // Collect line number glyphs first
            self.line_number_glyphs.clear();
            self.collect_line_number_glyphs();

            // === DRAW EDITOR CONTENT FIRST ===
            // Set scissor rect for text editor region
            pass.set_scissor_rect(
                (self.editor_bounds.x.0 * scale) as u32,
                (self.editor_bounds.y.0 * scale) as u32,
                (self.editor_bounds.width.0 * scale) as u32,
                (self.editor_bounds.height.0 * scale) as u32,
            );

            // Paint background layers (selection)
            self.paint_layers(pass, true);

            // Draw main text
            self.draw_all_accumulated_glyphs(pass);

            // Paint foreground layers (cursor)
            self.paint_layers(pass, false);

            if let Some(plugin_ptr) = self.line_numbers_plugin {
                let plugin = unsafe { &*plugin_ptr };

                let mut offset_y = self.viewport.global_margin.y.0 + STATUS_BAR_HEIGHT;

                // === DRAW LINE NUMBERS LAST (with separate buffer) ===
                // Set scissor rect for line numbers (left panel)
                pass.set_scissor_rect(
                    (FILE_EXPLORER_WIDTH * scale) as u32,
                    (offset_y * scale) as u32,
                    ((plugin.width) * scale) as u32,
                    ((self.viewport.logical_size.height.0 - offset_y) * scale) as u32,
                );

                // Draw line numbers using dedicated buffer (won't conflict with main text)
                if !self.line_number_glyphs.is_empty() {
                    if let Some(gpu) = self.gpu_renderer {
                        unsafe {
                            let gpu_renderer = &*gpu;
                            gpu_renderer.draw_line_number_glyphs(pass, &self.line_number_glyphs);
                        }
                    }
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
        // Set editor bounds on text_renderer
        self.text_renderer.set_editor_bounds(self.editor_bounds);

        if let Some(font_system) = &self.font_system {
            // Force layout update if layout is marked dirty (e.g., after font size change)
            if self.layout_dirty {
                self.text_renderer
                    .update_layout_internal(tree, font_system, &self.viewport, true);
            } else {
                self.text_renderer
                    .update_layout(tree, font_system, &self.viewport);
            }
        }

        if let Some(ref highlighter) = self.syntax_highlighter {
            let text = tree.flatten_to_string();
            let effects = highlighter.get_visible_effects(&text, 0..text.len());
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
            self.text_renderer
                .update_syntax(&tokens, highlighter.cached_version() == tree.version);
        }

        self.text_renderer
            .update_visible_range(&self.viewport, tree);

        if self.cached_doc_text.is_none() || tree.version != self.cached_doc_version {
            self.cached_doc_text = Some(tree.flatten_to_string());
            self.cached_doc_version = tree.version;
        }
    }

    pub fn paint_plugins(&mut self, pass: &mut wgpu::RenderPass, background: bool) {
        // Extract just the plugin painting logic
        if let Some(ref loader_arc) = self.plugin_loader {
            if let Ok(loader) = loader_arc.lock() {
                let z_filter = if background {
                    |z: i32| z < 0
                } else {
                    |z: i32| z >= 0
                };

                if let Some(gpu) = self.gpu_renderer {
                    let gpu_renderer = unsafe { &*gpu };

                    // Create editor-specific viewport for text-related plugins
                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: self.editor_bounds,
                        scroll: self.viewport.scroll, // Use main viewport scroll
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 2, // Editor widget ID
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

                    // Set scissor rect to editor bounds for plugin rendering
                    // This ensures selection and cursor stay within editor bounds
                    let scale = self.viewport.scale_factor;
                    // pass.set_scissor_rect(
                    //     (self.editor_bounds.x.0 * scale) as u32,
                    //     (self.editor_bounds.y.0 * scale) as u32,
                    //     (self.editor_bounds.width.0 * scale) as u32,
                    //     (self.editor_bounds.height.0 * scale) as u32,
                    // );

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

    fn paint_layers(&mut self, pass: &mut wgpu::RenderPass, background: bool) {
        // Paint other plugins (cursor, selection, etc.)
        self.paint_plugins(pass, background);

        // Paint diagnostics plugin if present
        if !background {  // Diagnostics renders on foreground (z-index 50)
            if let Some(diagnostics_ptr) = self.diagnostics_plugin {
                let diagnostics = unsafe { &*diagnostics_ptr };

                if let Some(gpu) = self.gpu_renderer {
                    let gpu_renderer = unsafe { &*gpu };

                    // Create editor-specific viewport for diagnostics
                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: self.editor_bounds,
                        scroll: self.viewport.scroll,
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 3, // Diagnostics widget ID
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

                    diagnostics.paint(&ctx, pass);
                }
            }
        }
    }

    fn walk_visible_range_with_pass(
        &mut self,
        tree: &Tree,
        byte_range: std::ops::Range<usize>,
        mut render_pass: Option<&mut wgpu::RenderPass>,
    ) {
        if let Some(pass) = render_pass.as_mut() {
            if let Some(gpu_ptr) = self.gpu_renderer {
                let gpu_renderer = unsafe { &*gpu_ptr };

                // Always use text_renderer for main document rendering
                let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

                if gpu_renderer.has_styled_pipeline() && !visible_glyphs.is_empty() {
                    let style_buffer: Vec<u32> =
                        visible_glyphs.iter().map(|g| g.token_id as u32).collect();
                    let gpu_mut = unsafe { &mut *(gpu_ptr as *mut GpuRenderer) };
                    gpu_mut.upload_style_buffer_u32(&style_buffer);
                }

                let glyph_instances: Vec<_> = visible_glyphs
                    .into_iter()
                    .enumerate()
                    .map(|(i, g)| {
                        // Debug: Print first few glyphs' texture coordinates
                        if i < 3 {
                            println!("Main text glyph {}: tex_coords={:?}", i, g.tex_coords);
                        }
                        // Transform from local editor coordinates to screen coordinates
                        let screen_pos = LayoutPos::new(
                            g.layout_pos.x.0 + self.editor_bounds.x.0,
                            g.layout_pos.y.0 + self.editor_bounds.y.0,
                        );
                        let physical = self.viewport.layout_to_physical(screen_pos);
                        GlyphInstance {
                            pos: LayoutPos::new(physical.x.0, physical.y.0),
                            tex_coords: g.tex_coords,
                            token_id: g.token_id as u8,
                            relative_pos: g.relative_pos,
                            shader_id: 0,
                            _padding: [0; 3],
                        }
                    })
                    .collect();

                if !glyph_instances.is_empty() {
                    let use_styled =
                        self.syntax_highlighter.is_some() && gpu_renderer.has_styled_pipeline();
                    println!(
                        "MAIN TEXT: Drawing {} glyphs (styled={})",
                        glyph_instances.len(),
                        use_styled
                    );
                    gpu_renderer.draw_glyphs_styled(pass, &glyph_instances, use_styled);
                }

                // Still handle inline spatial widgets
                tree.walk_visible_range(byte_range, |spans, _, _| {
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

    fn collect_line_number_glyphs(&mut self) {
        // Directly create line number glyphs without going through plugin paint
        if let Some(plugin_ptr) = self.line_numbers_plugin {
            let plugin = unsafe { &*plugin_ptr };

            // Use the collect_glyphs method to get glyphs
            let line_numbers_bounds = tiny_sdk::types::LayoutRect::new(
                self.viewport.global_margin.x.0 + FILE_EXPLORER_WIDTH,
                self.viewport.global_margin.y.0,
                plugin.width,
                self.viewport.logical_size.height.0 - self.viewport.global_margin.y.0,
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

    fn collect_main_text_glyphs(&mut self, tree: &Tree, visible_range: std::ops::Range<usize>) {
        // Collect main document glyphs without drawing
        let visible_glyphs = self.text_renderer.get_visible_glyphs_with_style();

        let glyph_instances: Vec<_> = visible_glyphs
            .into_iter()
            .map(|g| {
                // First apply scroll to get view position
                let view_x = g.layout_pos.x.0 - self.viewport.scroll.x.0;
                let view_y = g.layout_pos.y.0 - self.viewport.scroll.y.0;

                // Then add editor bounds offset and scale to physical
                let physical_x = (view_x + self.editor_bounds.x.0) * self.viewport.scale_factor;
                let physical_y = (view_y + self.editor_bounds.y.0) * self.viewport.scale_factor;

                GlyphInstance {
                    pos: LayoutPos::new(physical_x, physical_y),
                    tex_coords: g.tex_coords,
                    token_id: g.token_id as u8,
                    relative_pos: g.relative_pos,
                    shader_id: 0,
                    _padding: [0; 3],
                }
            })
            .collect();

        self.accumulated_glyphs.extend(glyph_instances);
    }

    fn collect_foreground_glyphs(&mut self) {
        // For other plugins that generate glyphs
        // Currently none, but this is where they'd go
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

                let use_styled =
                    self.syntax_highlighter.is_some() && gpu_renderer.has_styled_pipeline();
                // println!("Drawing ALL {} glyphs in one batch (styled={})",
                // self.accumulated_glyphs.len(), use_styled);

                gpu_renderer.draw_glyphs_styled(pass, &self.accumulated_glyphs, use_styled);
            }
        }
    }

    fn paint_spatial_widgets(
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

    fn walk_visible_range_no_glyphs(
        &mut self,
        tree: &Tree,
        visible_range: std::ops::Range<usize>,
        pass: &mut wgpu::RenderPass,
    ) {
        // Same as walk_visible_range_with_pass but skip the main text glyph drawing
        // (since we do that in the batched call)
        if let Some(gpu_ptr) = self.gpu_renderer {
            let gpu_renderer = unsafe { &*gpu_ptr };

            // Skip the main text glyph rendering - just do spatial widgets
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
