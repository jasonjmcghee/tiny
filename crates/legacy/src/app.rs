//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::{
    accelerator::{Modifiers, MouseButton, Trigger, WheelDirection},
    coordinates::TextMetrics,
    input::{EventBus, InputAction},
    lsp_manager::LspManager,
    render::Renderer,
    scroll::{ScrollFocusManager, Scrollable, WidgetId},
    shortcuts::{ShortcutContext, ShortcutRegistry},
    tab_manager::TabManager,
    winit_adapter,
};

pub use crate::editor_logic::EditorLogic;
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tiny_font::SharedFontSystem;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::Key,
    platform::macos::WindowAttributesExtMacOS,
    window::{Window, WindowId},
};

// Plugin orchestration support
use tiny_core::{tree::Point, GpuRenderer, Uniforms};
use tiny_sdk::{Hook, LogicalPixels, Paintable as SdkPaint, Updatable as SdkUpdate};

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScrollDirection {
    Vertical,
    Horizontal,
}

/// Simple plugin orchestrator - manages plugin lifecycle
/// This will eventually move to core crate
pub struct PluginOrchestrator {
    /// Widgets that need update calls
    update_widgets: Vec<Box<dyn SdkUpdate>>,
    /// Widgets that need paint calls
    paint_widgets: Vec<Box<dyn SdkPaint>>,
    /// Hooks for transforming glyph instances
    glyph_hooks: Vec<Box<dyn Hook<tiny_sdk::GlyphInstances, Output = tiny_sdk::GlyphInstances>>>,
}

impl PluginOrchestrator {
    pub fn new() -> Self {
        Self {
            update_widgets: Vec::new(),
            paint_widgets: Vec::new(),
            glyph_hooks: Vec::new(),
        }
    }

    pub fn register_update(&mut self, widget: Box<dyn SdkUpdate>) {
        self.update_widgets.push(widget);
    }

    pub fn register_paint(&mut self, widget: Box<dyn SdkPaint>) {
        self.paint_widgets.push(widget);
    }

    pub fn register_glyph_hook(
        &mut self,
        hook: Box<dyn Hook<tiny_sdk::GlyphInstances, Output = tiny_sdk::GlyphInstances>>,
    ) {
        self.glyph_hooks.push(hook);
    }

    pub fn update_all(&mut self, dt: f32) -> Result<(), tiny_sdk::PluginError> {
        // For now, create a simple update context
        let mut ctx = tiny_sdk::UpdateContext {
            registry: tiny_sdk::PluginRegistry { _private: () },
            frame: 0,
            elapsed: 0.0,
        };

        for widget in &mut self.update_widgets {
            widget.update(dt, &mut ctx)?;
        }
        Ok(())
    }

    pub fn process_glyphs(&self, glyphs: tiny_sdk::GlyphInstances) -> tiny_sdk::GlyphInstances {
        let mut result = glyphs;
        for hook in &self.glyph_hooks {
            result = hook.process(result);
        }
        result
    }
}

/// Shared winit application that handles all GPU/font boilerplate
pub struct TinyApp {
    // Winit/GPU infrastructure
    window: Option<Arc<Window>>,
    gpu_renderer: Option<GpuRenderer>,
    font_system: Option<Arc<SharedFontSystem>>,
    cpu_renderer: Option<Renderer>,
    _shader_watcher: Option<notify::RecommendedWatcher>,
    shader_reload_pending: Arc<AtomicBool>,
    _config_watcher: Option<notify::RecommendedWatcher>,
    config_reload_pending: Arc<AtomicBool>,

    // Application-specific logic
    editor: EditorLogic,

    // Event bus for event-driven architecture
    event_bus: EventBus,

    // Shortcut registry for accelerator handling
    shortcuts: ShortcutRegistry,

    // Plugin orchestrator (will eventually move to core)
    orchestrator: PluginOrchestrator,

    // Settings
    window_title: String,
    window_size: (f32, f32),

    // Single source of truth for text metrics
    text_metrics: TextMetrics,

    // Base font size from config (before any manual zoom)
    base_font_size: f32,

    // Title bar settings
    title_bar_height: f32, // Logical pixels

    // Scroll lock settings
    scroll_lock_enabled: bool, // true = lock to one direction at a time
    current_scroll_direction: Option<ScrollDirection>, // which direction is currently locked

    // Track cursor position for clicks
    cursor_position: Option<PhysicalPosition<f64>>,

    // Track modifier keys (accelerator format)
    modifiers: Modifiers,

    // Track mouse drag
    mouse_pressed: bool,
    drag_start: Option<PhysicalPosition<f64>>,

    // Track if cursor moved for scrolling
    cursor_needs_scroll: bool,

    // Continuous rendering for animations
    continuous_rendering: bool,

    // Frame time tracking for dynamic dt
    last_frame_time: std::time::Instant,

    // Scroll focus management
    scroll_focus: ScrollFocusManager,

    // EditableTextView focus tracking (for cursor/selection routing)
    focused_editable_view_id: Option<u64>,
}

impl TinyApp {
    fn physical_to_logical_point(&self, position: PhysicalPosition<f64>) -> Option<Point> {
        let window = self.window.as_ref()?;
        let scale = window.scale_factor() as f32;
        let logical_x = position.x as f32 / scale;
        let logical_y = position.y as f32 / scale;
        Some(Point {
            x: LogicalPixels(logical_x),
            y: LogicalPixels(logical_y),
        })
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn update_window_title(&self) {
        if let Some(window) = &self.window {
            window.set_title(&self.editor.title());
        }
    }

    /// Handle cursor movement (mouse move)
    fn handle_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.cursor_position = Some(position);

        // Pre-compute logical positions to avoid borrow issues
        let logical_point = self.physical_to_logical_point(position);
        let drag_from = self
            .drag_start
            .and_then(|p| self.physical_to_logical_point(p));

        if let Some(point) = logical_point {
            // Ensure file picker bounds are up to date before hit testing
            if self.editor.file_picker.visible {
                if let Some(cpu_renderer) = &self.cpu_renderer {
                    self.editor
                        .file_picker
                        .calculate_bounds(&cpu_renderer.viewport);
                }
            }

            // Update scroll focus based on mouse position and actual widget bounds
            let mut widget_bounds = vec![];

            // File picker (overlay, high z-index)
            if self.editor.file_picker.visible {
                widget_bounds.push((
                    WidgetId::FilePicker,
                    self.editor.file_picker.get_bounds(),
                    1000, // z-index
                ));
            }

            // Grep (overlay, high z-index)
            if self.editor.grep.visible {
                widget_bounds.push((
                    WidgetId::Grep,
                    self.editor.grep.get_bounds(),
                    1000, // z-index
                ));
            }

            // Editor (full screen, low z-index)
            if let Some(cpu_renderer) = &self.cpu_renderer {
                widget_bounds.push((WidgetId::Editor, cpu_renderer.editor_bounds, 0));
            }

            self.scroll_focus.update_focus(point, &widget_bounds);
            // Extract all needed data from cpu_renderer first
            let (
                editor_bounds,
                viewport_scroll,
                viewport,
                diagnostics_ptr,
                editor_local_from,
                editor_local_to,
            ) = if let Some(cpu_renderer) = &self.cpu_renderer {
                let from_local = drag_from.map(|f| cpu_renderer.screen_to_editor_local(f));
                let to_local = cpu_renderer.screen_to_editor_local(point);
                (
                    cpu_renderer.editor_bounds,
                    cpu_renderer.viewport.scroll,
                    cpu_renderer.viewport.clone(),
                    cpu_renderer.diagnostics_plugin,
                    from_local,
                    to_local,
                )
            } else {
                return;
            };

            let cmd_held = self.modifiers.cmd;

            // Check if mouse is within editor bounds
            let in_editor = point.x.0 >= editor_bounds.x.0
                && point.x.0 <= editor_bounds.x.0 + editor_bounds.width.0
                && point.y.0 >= editor_bounds.y.0
                && point.y.0 <= editor_bounds.y.0 + editor_bounds.height.0;

            // Get hover position if in editor
            let hover_position = if in_editor {
                if let Some(diagnostics_ptr) = diagnostics_ptr {
                    let diagnostics_plugin = unsafe { &mut *diagnostics_ptr };
                    let editor_viewport = tiny_sdk::types::WidgetViewport {
                        bounds: editor_bounds,
                        scroll: viewport_scroll,
                        content_margin: tiny_sdk::types::LayoutPos::new(0.0, 0.0),
                        widget_id: 3,
                    };
                    // Need to access service_registry from cpu_renderer
                    if let Some(cpu_renderer) = &self.cpu_renderer {
                        diagnostics_plugin.set_mouse_position(
                            point.x.0,
                            point.y.0,
                            Some(&editor_viewport),
                            Some(&cpu_renderer.service_registry),
                        );
                    }
                    diagnostics_plugin.get_mouse_document_position()
                } else {
                    None
                }
            } else {
                None
            };

            // Now handle editor operations without holding cpu_renderer borrow
            if let Some((line, column)) = hover_position {
                self.editor
                    .tab_manager
                    .active_tab_mut()
                    .diagnostics
                    .on_mouse_move(line, column, cmd_held);
            } else if !in_editor {
                self.editor
                    .tab_manager
                    .active_tab_mut()
                    .diagnostics
                    .on_mouse_leave();
            }

            // Emit mouse move event for overlays (file picker, grep)
            self.event_bus.emit(
                "app.mouse.move",
                json!({
                    "x": point.x.0,
                    "y": point.y.0,
                }),
                15,
                "winit",
            );

            // Process mouse move events immediately for responsive hover
            self.process_event_queue();

            // If hover changed anything, render immediately (don't wait for RedrawRequested)
            if self.editor.ui_changed || self.editor.on_mouse_move(point, &viewport) {
                if !self.continuous_rendering {
                    // Render immediately for instant hover feedback
                    self.render_frame();
                } else if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            // Mouse drag - emit event
            if self.mouse_pressed {
                if let Some(from) = drag_from {
                    // Check if drag started in titlebar area (for transparent titlebar on macOS)
                    #[cfg(target_os = "macos")]
                    let drag_started_in_titlebar = from.y.0 < self.title_bar_height;
                    #[cfg(not(target_os = "macos"))]
                    let drag_started_in_titlebar = false;

                    // Only emit drag event if drag didn't start in titlebar area
                    if !drag_started_in_titlebar {
                        if let (Some(from_local), to_local) = (editor_local_from, editor_local_to) {
                            // Emit drag event
                            self.event_bus.emit(
                                "mouse.drag",
                                json!({
                                    "from_x": from_local.x.0,
                                    "from_y": from_local.y.0,
                                    "to_x": to_local.x.0,
                                    "to_y": to_local.y.0,
                                    "alt": self.modifiers.alt,
                                }),
                                10,
                                "winit",
                            );
                        }
                    }
                }
            }
        }
    }

    /// Handle mouse wheel scrolling - routes to focused widget
    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        // Emit mouse wheel event to the event bus
        let (delta_x, delta_y) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x, y),
            MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
        };

        self.event_bus.emit(
            "app.mouse.scroll",
            json!({
                "delta_x": delta_x,
                "delta_y": delta_y,
                "type": match delta {
                    MouseScrollDelta::LineDelta(_, _) => "line",
                    MouseScrollDelta::PixelDelta(_) => "pixel",
                }
            }),
            15, // Slightly lower priority than direct input
            "winit",
        );

        // Request immediate redraw to process scroll events
        self.request_redraw();

        // Convert scroll delta to logical units
        let (scroll_x, scroll_y) = if let Some(cpu_renderer) = &self.cpu_renderer {
            match delta {
                MouseScrollDelta::LineDelta(x, y) => (
                    x * cpu_renderer.viewport.metrics.space_width,
                    y * cpu_renderer.viewport.metrics.line_height,
                ),
                MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
            }
        } else {
            return;
        };

        // Apply scroll lock logic
        let (final_scroll_x, final_scroll_y) = if self.scroll_lock_enabled {
            let new_direction = if scroll_y.abs() > scroll_x.abs() {
                ScrollDirection::Vertical
            } else if scroll_x.abs() > 0.0 {
                ScrollDirection::Horizontal
            } else {
                self.current_scroll_direction
                    .unwrap_or(ScrollDirection::Vertical)
            };

            if scroll_x.abs() > 0.0 || scroll_y.abs() > 0.0 {
                self.current_scroll_direction = Some(new_direction);
            }

            match new_direction {
                ScrollDirection::Vertical => (0.0, scroll_y),
                ScrollDirection::Horizontal => (scroll_x, 0.0),
            }
        } else {
            (scroll_x, scroll_y)
        };

        // Create scroll delta point
        let scroll_delta = Point {
            x: LogicalPixels(final_scroll_x),
            y: LogicalPixels(final_scroll_y),
        };

        // Route scroll to focused widget
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            let viewport = &cpu_renderer.viewport;
            let editor_bounds = cpu_renderer.editor_bounds;

            match self.scroll_focus.focused_widget() {
                Some(WidgetId::FilePicker) => {
                    // File picker handles scroll through event bus (see file_picker_plugin.rs)
                    // Event already emitted above, nothing more to do here
                }
                Some(WidgetId::Grep) => {
                    // Grep handles scroll through event bus (see grep_plugin.rs)
                    // Event already emitted above, nothing more to do here
                }
                Some(WidgetId::Editor) | None => {
                    // Route to active editor tab with editor bounds
                    let tab = self.editor.tab_manager.active_tab_mut();
                    tab.handle_scroll(scroll_delta, viewport, editor_bounds);

                    // Update viewport scroll for rendering
                    cpu_renderer.viewport.scroll = tab.scroll_position;
                }
                _ => {
                    // Other widgets - not yet implemented
                }
            }
        }

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub fn new(editor: EditorLogic) -> Self {
        // Pre-warm LSP in the background for faster startup
        // Look for workspace root from current directory (find deepest Cargo.toml)
        let workspace_root = std::env::current_dir().ok().and_then(|dir| {
            let mut current = dir.as_path();
            let mut found_cargo_toml = None;
            loop {
                if current.join("Cargo.toml").exists() {
                    found_cargo_toml = Some(current.to_path_buf());
                    // Keep looking for a higher-level Cargo.toml (workspace root)
                }
                match current.parent() {
                    Some(parent) => current = parent,
                    None => break,
                }
            }
            found_cargo_toml
        });
        LspManager::prewarm_for_workspace(workspace_root);

        let event_bus = EventBus::new();
        let shortcuts = ShortcutRegistry::new();

        // Default text metrics: 14pt font with 1.4x line height
        let text_metrics = TextMetrics::new(14.0);

        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            cpu_renderer: None,
            _shader_watcher: None,
            shader_reload_pending: Arc::new(AtomicBool::new(false)),
            _config_watcher: None,
            config_reload_pending: Arc::new(AtomicBool::new(false)),
            editor,
            event_bus,
            shortcuts,
            orchestrator: PluginOrchestrator::new(),
            window_title: "Tiny Editor".to_string(),
            window_size: (800.0, 600.0),
            text_metrics,
            base_font_size: 14.0,      // Default base size
            title_bar_height: 28.0,    // Logical pixels
            scroll_lock_enabled: true, // Enabled by default
            current_scroll_direction: None,
            cursor_position: None,
            modifiers: Modifiers::default(),
            mouse_pressed: false,
            drag_start: None,
            cursor_needs_scroll: false,
            continuous_rendering: false,
            last_frame_time: std::time::Instant::now(),
            scroll_focus: ScrollFocusManager::new(),
            focused_editable_view_id: None, // Will be set when first tab is created
        }
    }

    pub fn with_config(mut self, config: &crate::config::AppConfig) -> Self {
        self.window_title = config.editor.window_title.clone();
        self.window_size = (config.editor.window_width, config.editor.window_height);

        // Create text metrics from config
        let mut text_metrics = TextMetrics::new(config.editor.font_size);
        text_metrics.line_height = config.editor.font_size * config.editor.line_height;
        self.text_metrics = text_metrics;

        // Track base font size from config
        self.base_font_size = config.editor.font_size;

        self.title_bar_height = config.editor.title_bar_height;
        self.scroll_lock_enabled = config.editor.scroll_lock_enabled;
        self.continuous_rendering = config.editor.continuous_rendering;
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = title.into();
        self
    }

    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.window_size = (width, height);
        self
    }

    pub fn with_font_size(mut self, size: f32) -> Self {
        self.text_metrics = TextMetrics::new(size);
        self.base_font_size = size;
        self
    }

    pub fn with_continuous_rendering(mut self, enabled: bool) -> Self {
        self.continuous_rendering = enabled;
        self
    }

    pub fn run(mut self) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        event_loop.run_app(&mut self)?;
        Ok(())
    }

    /// Adjust font size (for Cmd+=/Cmd+-)
    fn adjust_font_size(&mut self, increase: bool) {
        let delta = if increase { 1.0 } else { -1.0 };
        let new_font_size = (self.text_metrics.font_size + delta).clamp(6.0, 72.0);

        println!("Font size changed to: {:.1}pt", new_font_size);

        // Update source of truth (render_frame will propagate to all viewports)
        let mut new_metrics = TextMetrics::new(new_font_size);
        new_metrics.line_height = new_font_size * 1.4; // Keep multiplier
        self.text_metrics = new_metrics;

        // Update CPU renderer (needed for text rendering)
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            cpu_renderer.set_font_size(new_font_size);
            if let Some(font_system) = &self.font_system {
                cpu_renderer.set_font_system(font_system.clone());
            }
            cpu_renderer.set_line_height(new_font_size * 1.4);
        }

        // Update font system rasterization (needed for GPU atlas)
        if let Some(font_system) = &self.font_system {
            if let Some(window) = &self.window {
                let scale_factor = window.scale_factor() as f32;
                font_system.prerasterize_ascii(new_font_size * scale_factor);

                // Re-upload font atlases to GPU
                if let Some(gpu_renderer) = &self.gpu_renderer {
                    let atlas_data = font_system.atlas_data();
                    let (atlas_width, atlas_height) = font_system.atlas_size();
                    gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);

                    if font_system.is_color_dirty() {
                        let color_atlas_data = font_system.color_atlas_data();
                        gpu_renderer.upload_color_font_atlas(
                            &color_atlas_data,
                            atlas_width,
                            atlas_height,
                        );
                        font_system.clear_color_dirty();
                    }
                }
            }
        }

        self.request_redraw();
    }

    fn setup_shader_watcher(&mut self) {
        use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc::channel;
        use std::time::{Duration, Instant};

        let (tx, rx) = channel();

        // Create watcher
        let mut watcher = match RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    // Only care about modifications to .wgsl files
                    if event.kind.is_modify()
                        && event
                            .paths
                            .iter()
                            .any(|p| p.extension().map_or(false, |ext| ext == "wgsl"))
                    {
                        let _ = tx.send(());
                    }
                }
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!(
                    "Failed to create file watcher: {}. Shader hot-reload disabled.",
                    e
                );
                return;
            }
        };

        // Watch the shaders directory
        let shader_path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("crates/core/src/shaders");

        if let Err(e) = watcher.watch(&shader_path, RecursiveMode::NonRecursive) {
            eprintln!(
                "Failed to watch shader directory {:?}: {}. Shader hot-reload disabled.",
                shader_path, e
            );
            return;
        }

        eprintln!("Shader hot-reload enabled! Watching: {:?}", shader_path);

        // Simple debounce thread
        let reload_flag = self.shader_reload_pending.clone();
        std::thread::spawn(move || {
            let mut last_reload = Instant::now();
            for _ in rx {
                // Simple 200ms debounce
                if last_reload.elapsed() > Duration::from_millis(50) {
                    reload_flag.store(true, Ordering::Relaxed);
                    last_reload = Instant::now();
                    eprintln!("Shader change detected, triggering reload...");
                }
            }
        });

        // Store the watcher (it needs to stay alive)
        self._shader_watcher = Some(watcher);
    }

    fn setup_config_watcher(&mut self) {
        use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
        use std::sync::mpsc::channel;
        use std::time::{Duration, Instant};

        let config_path = std::path::PathBuf::from("init.toml");

        if !config_path.exists() {
            eprintln!("No init.toml found - config hot-reload disabled");
            return;
        }

        let (tx, rx) = channel();

        let mut watcher = match RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    if event.kind.is_modify() {
                        let _ = tx.send(());
                    }
                }
            },
            notify::Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!(
                    "Failed to create config watcher: {}. Config hot-reload disabled.",
                    e
                );
                return;
            }
        };

        if let Err(e) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
            eprintln!(
                "Failed to watch init.toml: {}. Config hot-reload disabled.",
                e
            );
            return;
        }

        eprintln!("Config hot-reload enabled! Watching: {:?}", config_path);

        // Debounce thread
        let reload_flag = self.config_reload_pending.clone();
        std::thread::spawn(move || {
            let mut last_reload = Instant::now();
            for _ in rx {
                if last_reload.elapsed() > Duration::from_millis(200) {
                    reload_flag.store(true, Ordering::Relaxed);
                    last_reload = Instant::now();
                    eprintln!("Config change detected, triggering reload...");
                }
            }
        });

        self._config_watcher = Some(watcher);
    }

    fn reload_config(&mut self) {
        let config = match crate::config::AppConfig::load() {
            Ok(config) => config,
            Err(e) => {
                eprintln!("❌ Failed to parse config: {}", e);
                eprintln!("   Using previous values (config not applied)");
                return; // Keep current settings, don't crash
            }
        };

        eprintln!("✅ Reloaded config from init.toml");

        // Update window title if changed
        if self.window_title != config.editor.window_title {
            self.window_title = config.editor.window_title.clone();
            if let Some(window) = &self.window {
                window.set_title(&self.window_title);
            }
        }

        // Update font settings if BASE font size changed in config
        // Compare with base_font_size, NOT text_metrics.font_size (which may be manually zoomed)
        let base_font_size_changed = (self.base_font_size - config.editor.font_size).abs() > 0.1;

        if base_font_size_changed {
            // Calculate current zoom ratio (how much user has zoomed)
            let zoom_ratio = self.text_metrics.font_size / self.base_font_size;

            // Update base font size
            self.base_font_size = config.editor.font_size;

            // Apply zoom ratio to new base size to preserve manual zoom
            let new_font_size = config.editor.font_size * zoom_ratio;
            let new_line_height = new_font_size * config.editor.line_height;

            let mut new_metrics = TextMetrics::new(new_font_size);
            new_metrics.line_height = new_line_height;
            self.text_metrics = new_metrics;

            eprintln!(
                "Updated base font size: {:.1} -> {:.1} (current: {:.1}, zoom: {:.2}x)",
                self.base_font_size / zoom_ratio,
                config.editor.font_size,
                new_font_size,
                zoom_ratio
            );

            // Mark everything dirty to force re-layout
            if let Some(renderer) = &mut self.cpu_renderer {
                renderer.mark_ui_dirty();
            }
        } else {
            // Base didn't change, but line height multiplier might have
            let expected_line_height = self.text_metrics.font_size * config.editor.line_height;
            let line_height_changed =
                (self.text_metrics.line_height - expected_line_height).abs() > 0.1;

            if line_height_changed {
                self.text_metrics.line_height = expected_line_height;
                if let Some(renderer) = &mut self.cpu_renderer {
                    renderer.mark_ui_dirty();
                }
            }
        }

        // Update scroll lock setting
        self.scroll_lock_enabled = config.editor.scroll_lock_enabled;

        // Update title bar height
        self.title_bar_height = config.editor.title_bar_height;

        // Update font weight and italic settings
        if let Some(font_system) = &self.font_system {
            let current_weight = font_system.default_weight();
            let current_italic = font_system.default_italic();

            let weight_changed = (current_weight - config.editor.font_weight).abs() > 0.1;
            let italic_changed = current_italic != config.editor.font_italic;

            if weight_changed {
                font_system.set_default_weight(config.editor.font_weight);
                eprintln!(
                    "Updated font weight: {} -> {}",
                    current_weight, config.editor.font_weight
                );
            }

            if italic_changed {
                font_system.set_default_italic(config.editor.font_italic);
                eprintln!(
                    "Updated font italic: {} -> {}",
                    current_italic, config.editor.font_italic
                );
            }

            // If font appearance changed, force complete re-layout
            if weight_changed || italic_changed {
                if let Some(renderer) = &mut self.cpu_renderer {
                    // Clear all layout caches - text will be re-shaped with new weight/italic
                    renderer.clear_all_caches();
                    renderer.mark_ui_dirty();
                }
                // Force editor to re-layout
                self.editor.ui_changed = true;
            }
        }

        self.request_redraw();
    }

    /// Helper for simple overlay show (file picker, grep, etc.)
    fn show_overlay(
        &mut self,
        show: impl FnOnce(&mut EditorLogic),
        ctx: ShortcutContext,
        focus: WidgetId,
        editable_id: u64,
    ) {
        show(&mut self.editor);
        self.editor.ui_changed = true;
        self.shortcuts.set_context(ctx);
        self.scroll_focus.set_focus(focus);
        self.focused_editable_view_id = Some(editable_id);
        self.request_redraw();
    }

    /// Helper for navigation with optional redraw
    fn navigate(&mut self, nav: impl FnOnce(&mut EditorLogic) -> bool) {
        if nav(&mut self.editor) {
            self.request_redraw();
            self.cursor_needs_scroll = true;
        }
    }

    /// Dispatch event through subscription system
    /// Components handle events and emit follow-up events as needed
    fn dispatch_to_components(&mut self, event: &crate::input::Event) {
        use crate::input::{dispatch_event, EventSubscriber};

        // Dispatch through priority-ordered subscribers
        // Components emit follow-up events (file.open, grep.navigate, etc.)
        let mut subscribers: Vec<&mut dyn EventSubscriber> = vec![
            &mut self.editor.grep, // Priority 100
            &mut self.editor.file_picker, // Priority 100
                                   // Main editor (priority 0) doesn't claim navigate/action events
        ];
        dispatch_event(event, &mut subscribers, &mut self.event_bus);
    }

    /// Get the focused EditableTextView for event routing
    /// Returns (input_handler, doc, viewport) tuple
    fn get_focused_view_mut(
        &mut self,
    ) -> (
        &mut crate::input::InputHandler,
        &tiny_core::tree::Doc,
        crate::coordinates::Viewport,
    ) {
        let focused_id = self.focused_editable_view_id.unwrap_or(0);

        // Try file picker input first
        if self.editor.file_picker.visible && focused_id == self.editor.file_picker.input().id {
            let input = self.editor.file_picker.input_mut();
            let viewport = input.view.viewport.clone();
            return (&mut input.input, &input.view.doc, viewport);
        }

        // Try grep input
        if self.editor.grep.visible && focused_id == self.editor.grep.input().id {
            let input = self.editor.grep.input_mut();
            let viewport = input.view.viewport.clone();
            return (&mut input.input, &input.view.doc, viewport);
        }

        // Fallback to main editor
        let tab = self.editor.tab_manager.active_tab_mut();
        let viewport = self
            .cpu_renderer
            .as_ref()
            .map(|r| r.viewport.clone())
            .unwrap_or_else(|| crate::coordinates::Viewport::new(800.0, 600.0, 1.0));
        (
            &mut tab.plugin.editor.input,
            &tab.plugin.editor.view.doc,
            viewport,
        )
    }

    fn process_event_queue(&mut self) {
        // Process events in a loop to handle events emitted during event processing
        // This ensures ui.redraw events from hover handlers are processed immediately
        loop {
            let events = self.event_bus.drain_sorted();
            if events.is_empty() {
                break;
            }

            for event in events {
                use ShortcutContext::{FilePicker, Grep};
                use WidgetId::{FilePicker as FPWidget, Grep as GrepWidget};

                match event.name.as_str() {
                    // App-level
                    "app.font_increase" => self.adjust_font_size(true),
                    "app.font_decrease" => self.adjust_font_size(false),
                    "app.toggle_scroll_lock" => {
                        self.scroll_lock_enabled = !self.scroll_lock_enabled;
                        self.current_scroll_direction = None;
                        println!(
                            "Scroll lock: {}",
                            if self.scroll_lock_enabled {
                                "ENABLED"
                            } else {
                                "DISABLED"
                            }
                        );
                    }

                    // Mouse events
                    "mouse.press" => {
                        // Extract screen coordinates for overlays, editor-local for main editor
                        let screen_x = event.data.get("screen_x").and_then(|v| v.as_f64());
                        let screen_y = event.data.get("screen_y").and_then(|v| v.as_f64());
                        let editor_x = event.data.get("x").and_then(|v| v.as_f64());
                        let editor_y = event.data.get("y").and_then(|v| v.as_f64());

                        let shift = event
                            .data
                            .get("modifiers")
                            .and_then(|m| m.get("shift"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        // Check if click is in an overlay (use screen coordinates)
                        if self.editor.file_picker.visible {
                            if let (Some(x), Some(y)) = (screen_x, screen_y) {
                                use crate::filterable_dropdown::DropdownAction;
                                let action = self
                                    .editor
                                    .file_picker
                                    .picker
                                    .handle_click(x as f32, y as f32, shift);
                                match action {
                                    DropdownAction::Selected(path) => {
                                        // Open the file
                                        self.editor.file_picker.hide();
                                        self.shortcuts.set_context(ShortcutContext::Editor);
                                        self.scroll_focus.clear_focus();
                                        let _ = self.editor.tab_manager.open_file(path);
                                        if let Some(tab) = self.editor.tab_manager.active_tab() {
                                            self.focused_editable_view_id =
                                                Some(tab.plugin.editor.id);
                                        }
                                        self.cursor_needs_scroll = true;
                                    }
                                    _ => {}
                                }
                                self.request_redraw();
                            }
                        } else if self.editor.grep.visible {
                            if let (Some(x), Some(y)) = (screen_x, screen_y) {
                                use crate::filterable_dropdown::DropdownAction;
                                let action = self
                                    .editor
                                    .grep
                                    .picker
                                    .handle_click(x as f32, y as f32, shift);
                                match action {
                                    DropdownAction::Selected(result) => {
                                        // Jump to the result
                                        let (file, line, col) = (
                                            result.file_path.clone(),
                                            result.line_number.saturating_sub(1),
                                            result.column,
                                        );
                                        self.editor.grep.hide();
                                        self.shortcuts.set_context(ShortcutContext::Editor);
                                        self.scroll_focus.clear_focus();
                                        if self.editor.jump_to_location(file, line, col, true) {
                                            if let Some(tab) = self.editor.tab_manager.active_tab()
                                            {
                                                self.focused_editable_view_id =
                                                    Some(tab.plugin.editor.id);
                                            }
                                            self.cursor_needs_scroll = true;
                                        }
                                    }
                                    _ => {}
                                }
                                self.request_redraw();
                            }
                        } else if let (Some(x), Some(y)) = (editor_x, editor_y) {
                            // No overlay - route to main editor (use editor-local coordinates)
                            // Set drag state here since we're actually handling the click in the editor
                            if let (Some(phys_x), Some(phys_y)) = (
                                event.data.get("physical_x").and_then(|v| v.as_f64()),
                                event.data.get("physical_y").and_then(|v| v.as_f64()),
                            ) {
                                self.mouse_pressed = true;
                                self.drag_start =
                                    Some(winit::dpi::PhysicalPosition::new(phys_x, phys_y));
                            }

                            if let Some(viewport) =
                                self.cpu_renderer.as_ref().map(|r| r.viewport.clone())
                            {
                                let plugin = self.editor.active_plugin_mut();
                                if plugin.editor.input.handle_event(
                                    &event,
                                    &plugin.editor.view.doc,
                                    &viewport,
                                ) == InputAction::Redraw
                                {
                                    self.request_redraw();
                                    self.cursor_needs_scroll = true;
                                }
                            }
                        }
                    }
                    "mouse.release" => self.editor.on_mouse_release(),
                    "mouse.drag" => {
                        if let Some(viewport) =
                            self.cpu_renderer.as_ref().map(|r| r.viewport.clone())
                        {
                            let plugin = self.editor.active_plugin_mut();
                            plugin.editor.input.handle_event(
                                &event,
                                &plugin.editor.view.doc,
                                &viewport,
                            );
                            if let Some((dx, dy)) = plugin.editor.input.pending_scroll_delta.take()
                            {
                                self.event_bus.emit(
                                    "app.drag.scroll",
                                    json!({ "delta_x": dx, "delta_y": dy }),
                                    15,
                                    "mouse_drag",
                                );
                            }
                            self.request_redraw();
                        }
                    }
                    "app.drag.scroll" => {
                        if let (Some(dx), Some(dy)) = (
                            event.data.get("delta_x").and_then(|v| v.as_f64()),
                            event.data.get("delta_y").and_then(|v| v.as_f64()),
                        ) {
                            let tab = self.editor.tab_manager.active_tab_mut();
                            tab.scroll_position.x.0 += dx as f32;
                            tab.scroll_position.y.0 += dy as f32;
                            if let Some(cpu_renderer) = &mut self.cpu_renderer {
                                let (doc, editor_bounds) =
                                    (&tab.plugin.editor.view.doc, cpu_renderer.editor_bounds);
                                cpu_renderer.viewport.scroll = tab.scroll_position;
                                cpu_renderer
                                    .viewport
                                    .clamp_scroll_to_bounds(&doc.read(), editor_bounds);
                                tab.scroll_position = cpu_renderer.viewport.scroll;
                                self.request_redraw();
                            }
                        }
                    }

                    // Navigation
                    "navigation.goto_definition" => {
                        self.editor.goto_definition();
                        self.cursor_needs_scroll = true;
                    }
                    "navigation.back" => self.navigate(|e| e.navigate_back()),
                    "navigation.forward" => self.navigate(|e| e.navigate_forward()),

                    // Tabs
                    "tabs.close" => {
                        self.editor.tab_manager.close_active_tab();
                        // Restore focus to newly active tab
                        if let Some(tab) = self.editor.tab_manager.active_tab() {
                            self.focused_editable_view_id = Some(tab.plugin.editor.id);
                        }
                        self.editor.ui_changed = true;
                        self.request_redraw();
                    }

                    // File picker
                    "file_picker.open" => {
                        let picker_input_id = self.editor.file_picker.input().id;
                        self.show_overlay(
                            |e| e.file_picker.show(),
                            FilePicker,
                            FPWidget,
                            picker_input_id,
                        );
                    }
                    // Grep
                    "grep.open" => {
                        let grep_input_id = self.editor.grep.input().id;
                        self.show_overlay(
                            |e| e.grep.show(String::new()),
                            Grep,
                            GrepWidget,
                            grep_input_id,
                        );
                    }

                    // Component-emitted events
                    "ui.redraw" => {
                        self.editor.ui_changed = true;
                        self.request_redraw();
                    }
                    "file.open" => {
                        if let Some(path_val) = event.data.get("path") {
                            if let Some(path_str) = path_val.as_str() {
                                let path = std::path::PathBuf::from(path_str);
                                self.shortcuts.set_context(ShortcutContext::Editor);
                                self.scroll_focus.clear_focus();
                                self.editor.record_navigation();
                                if self.editor.tab_manager.open_file(path).is_ok() {
                                    if let Some(tab) = self.editor.tab_manager.active_tab() {
                                        self.focused_editable_view_id = Some(tab.plugin.editor.id);
                                    }
                                    self.editor.ui_changed = true;
                                    self.request_redraw();
                                }
                            }
                        }
                    }
                    "file.goto" => {
                        // Generic goto location (from grep, LSP, etc.)
                        if let (Some(file), Some(line), Some(col)) = (
                            event.data.get("file"),
                            event.data.get("line").and_then(|v| v.as_u64()),
                            event.data.get("column").and_then(|v| v.as_u64()),
                        ) {
                            if let Some(path_str) = file.as_str() {
                                let path = std::path::PathBuf::from(path_str);
                                self.shortcuts.set_context(ShortcutContext::Editor);
                                self.scroll_focus.clear_focus();
                                if self.editor.jump_to_location(
                                    path,
                                    line as usize,
                                    col as usize,
                                    true,
                                ) {
                                    if let Some(tab) = self.editor.tab_manager.active_tab() {
                                        self.focused_editable_view_id = Some(tab.plugin.editor.id);
                                    }
                                    self.cursor_needs_scroll = true;
                                }
                                self.request_redraw();
                            }
                        }
                    }
                    "overlay.closed" => {
                        self.shortcuts.set_context(ShortcutContext::Editor);
                        self.scroll_focus.clear_focus();
                        self.request_redraw();
                    }

                    // Editor events - delegate to components first, then main editor if not handled
                    "editor.code_action" => self.editor.handle_code_action_request(),
                    name if name.starts_with("editor.") => {
                        // First, try dispatching to overlay components (file picker, grep)
                        // They handle their own text editing and return Stop if they consumed the event
                        let mut handled = false;
                        if self.editor.file_picker.visible {
                            use crate::input::EventSubscriber;
                            if self
                                .editor
                                .file_picker
                                .handle_event(&event, &mut self.event_bus)
                                == crate::input::PropagationControl::Stop
                            {
                                handled = true;
                            }
                        }
                        if !handled && self.editor.grep.visible {
                            use crate::input::EventSubscriber;
                            if self.editor.grep.handle_event(&event, &mut self.event_bus)
                                == crate::input::PropagationControl::Stop
                            {
                                handled = true;
                            }
                        }

                        // If no overlay handled it, route to main editor
                        if !handled {
                            let (input_handler, doc, viewport) = self.get_focused_view_mut();
                            let action = input_handler.handle_event(&event, doc, &viewport);

                            match action {
                                InputAction::Save => {
                                    // Save only makes sense for main editor
                                    let _ = self
                                        .editor
                                        .save()
                                        .map_err(|e| eprintln!("Failed to save: {}", e));
                                    self.request_redraw();
                                    self.update_window_title();
                                    self.cursor_needs_scroll = true;
                                }
                                InputAction::Undo => {
                                    // Actually perform undo on the focused view's buffer
                                    let (input_handler, doc, _) = self.get_focused_view_mut();
                                    input_handler.undo(doc);
                                    self.request_redraw();
                                    self.update_window_title();
                                    self.cursor_needs_scroll = true;
                                }
                                InputAction::Redo => {
                                    // Actually perform redo on the focused view's buffer
                                    let (input_handler, doc, _) = self.get_focused_view_mut();
                                    input_handler.redo(doc);
                                    self.request_redraw();
                                    self.update_window_title();
                                    self.cursor_needs_scroll = true;
                                }
                                InputAction::Redraw => {
                                    self.request_redraw();
                                    self.update_window_title();
                                    self.cursor_needs_scroll = true;
                                }
                                InputAction::None => {
                                    // Main editor - might still need a redraw for cursor movement etc
                                    self.request_redraw();
                                }
                            }
                        }
                    }

                    _ => {
                        // Dispatch to components through subscription system
                        self.dispatch_to_components(&event);
                    }
                }
            }
        }
    }

    fn update_frame_timing(&mut self) -> f32 {
        let current_time = std::time::Instant::now();
        let frame_duration = current_time.duration_since(self.last_frame_time);
        self.last_frame_time = current_time;

        if self.continuous_rendering {
            // Use actual frame duration for smooth animations
            frame_duration.as_secs_f32().min(0.05)
        } else {
            // Use consistent 16ms (60fps) for predictable animations in retained mode
            0.016
        }
    }

    fn render_frame(&mut self) {
        // Process all queued events at the beginning of the frame
        // This ensures events are handled before rendering
        self.process_event_queue();

        // Lazy initialize EditableTextView plugins (for new tabs/views only)
        if let Some(ref cpu_renderer) = self.cpu_renderer {
            if let Some(loader_arc) = cpu_renderer.get_plugin_loader() {
                let loader = loader_arc.lock().unwrap();
                // Returns list of newly-initialized views that need GPU setup
                if let Ok(newly_initialized) = self.editor.initialize_all_plugins(&loader) {
                    if !newly_initialized.is_empty() {
                        drop(loader); // Release lock
                        if let Some(gpu_renderer) = &self.gpu_renderer {
                            let device = gpu_renderer.device_arc();
                            let queue = gpu_renderer.queue_arc();
                            let _ = self.editor.setup_plugins_for_views(
                                newly_initialized,
                                device,
                                queue,
                            );
                        }
                    }
                }
            }
        }

        // Check for pending shader reload
        if self.shader_reload_pending.load(Ordering::Relaxed) {
            if let Some(gpu_renderer) = &mut self.gpu_renderer {
                gpu_renderer.reload_shaders();
                self.shader_reload_pending.store(false, Ordering::Relaxed);
            }
        }

        // Check for pending config reload
        if self.config_reload_pending.load(Ordering::Relaxed) {
            self.reload_config();
            self.config_reload_pending.store(false, Ordering::Relaxed);
        }

        let dt = self.update_frame_timing();
        let cursor_moved = self.editor.on_update();
        if cursor_moved {
            self.cursor_needs_scroll = true;
        }

        // NOTE: diagnostics.update() is called AFTER rendering (around line 1675)
        // when layout cache is populated. Don't call it here!

        // Update plugins through orchestrator
        if let Err(e) = self.orchestrator.update_all(dt) {
            eprintln!("Plugin update error: {}", e);
        }

        // Request next frame if continuous rendering is enabled
        if self.continuous_rendering {
            self.request_redraw();
        }

        // Handle cursor scroll when selection actually changed
        if self.cursor_needs_scroll {
            self.cursor_needs_scroll = false;
            if let Some(cursor_pos) = self.editor.get_cursor_doc_pos() {
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    let tab = self.editor.tab_manager.active_tab_mut();
                    // Set viewport to current tab scroll before scrolling
                    cpu_renderer.viewport.scroll = tab.scroll_position;

                    // Get layout position with tab-aware positioning
                    let layout_pos = {
                        let tree = tab.plugin.editor.view.doc.read();
                        cpu_renderer.viewport.doc_to_layout_with_tree(cursor_pos, &tree)
                    };

                    // Center for goto-definition, otherwise just ensure visible
                    if self.editor.cursor_needs_centering {
                        self.editor.cursor_needs_centering = false;
                        cpu_renderer.viewport.center_on(layout_pos);
                    } else {
                        cpu_renderer.viewport.ensure_visible(layout_pos);
                    }

                    // Save modified scroll back to tab
                    tab.scroll_position = cpu_renderer.viewport.scroll;
                }
            }
        }

        // Check if we have all required components
        if self.window.is_none() || self.gpu_renderer.is_none() || self.cpu_renderer.is_none() {
            return;
        }

        // Get window info without holding a borrow
        let (logical_width, logical_height, scale_factor) = {
            let window = self.window.as_ref().unwrap();
            let size = window.inner_size();
            let scale = window.scale_factor() as f32;
            (size.width as f32 / scale, size.height as f32 / scale, scale)
        };

        // Update GPU renderer time
        if let Some(gpu_renderer) = &mut self.gpu_renderer {
            gpu_renderer.update_time(dt);
        }

        // Update viewport
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Update file picker bounds based on viewport (overlay mode)
            self.editor
                .file_picker
                .calculate_bounds(&cpu_renderer.viewport);

            // Update grep bounds based on viewport (overlay mode)
            self.editor.grep.calculate_bounds(&cpu_renderer.viewport);
        }

        // Setup text styles
        if let Some(text_styles) = self.editor.text_styles() {
            if let Some(syntax_hl) = text_styles
                .as_any()
                .downcast_ref::<crate::syntax::SyntaxHighlighter>()
            {
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    let highlighter = Arc::new(syntax_hl.clone());
                    cpu_renderer.set_syntax_highlighter(highlighter);
                }
            }
        }

        // Update plugins for editor
        if let Some(cpu_renderer) = self.cpu_renderer.as_mut() {
            let tab = self.editor.tab_manager.active_tab_mut();

            // Swap in the active tab's text_renderer to preserve per-tab state
            cpu_renderer.swap_text_renderer(&mut tab.text_renderer);

            // Use the active tab's scroll position for rendering
            cpu_renderer.viewport.scroll = tab.scroll_position;

            // Update main editor's TextView viewport to match actual render state
            tab.plugin.editor.view.viewport.bounds = cpu_renderer.editor_bounds;
            tab.plugin.editor.view.viewport.scroll = tab.scroll_position;
            tab.plugin.editor.view.viewport.scale_factor = cpu_renderer.viewport.scale_factor;

            // Propagate metrics from source of truth (single-direction data flow)
            tab.plugin
                .editor
                .view
                .viewport
                .update_metrics(&self.text_metrics);

            // Update space_width from font system for accurate measurement
            if let Some(ref font_system) = self.font_system {
                tab.plugin.editor.view.viewport.metrics.space_width =
                    font_system.char_width_coef() * self.text_metrics.font_size;
            }

            // Sync cursor/selection state to main editor's plugins
            tab.plugin.editor.sync_plugins();

            // Set line numbers plugin with fresh document reference
            cpu_renderer
                .set_line_numbers_plugin(&mut tab.line_numbers, &tab.plugin.editor.view.doc);

            // Set tab bar, file picker, and grep plugins (global UI)
            cpu_renderer.set_tab_bar_plugin(&mut self.editor.tab_bar);

            // Propagate metrics to file picker input
            self.editor
                .file_picker
                .input_mut()
                .view
                .viewport
                .update_metrics(&self.text_metrics);
            if let Some(ref font_system) = self.font_system {
                self.editor
                    .file_picker
                    .input_mut()
                    .view
                    .viewport
                    .metrics
                    .space_width = font_system.char_width_coef() * self.text_metrics.font_size;
            }
            cpu_renderer.set_file_picker_plugin(&mut self.editor.file_picker);

            // Propagate metrics to grep input
            self.editor
                .grep
                .input_mut()
                .view
                .viewport
                .update_metrics(&self.text_metrics);
            if let Some(ref font_system) = self.font_system {
                self.editor
                    .grep
                    .input_mut()
                    .view
                    .viewport
                    .metrics
                    .space_width = font_system.char_width_coef() * self.text_metrics.font_size;
            }
            cpu_renderer.set_grep_plugin(&mut self.editor.grep);

            // Mark renderer UI dirty if UI changed
            if self.editor.ui_changed {
                cpu_renderer.mark_ui_dirty();
                self.editor.ui_changed = false;
            }

            // Set diagnostics plugin for rendering (will be updated after layout is computed)
            cpu_renderer
                .set_diagnostics_plugin(tab.diagnostics.plugin_mut(), &tab.plugin.editor.view.doc);

            // Initialize diagnostics plugin with GPU resources if needed
            // Check if THIS specific plugin instance needs initialization
            unsafe {
                if let Some(diagnostics_ptr) = cpu_renderer.diagnostics_plugin {
                    let diagnostics = &mut *diagnostics_ptr;

                    // Check if this plugin instance has GPU resources already
                    if !diagnostics.is_initialized() {
                        if let Some(gpu) = cpu_renderer.get_gpu_renderer() {
                            let gpu_renderer = &*gpu;
                            use tiny_sdk::Initializable;
                            let mut setup_ctx = tiny_sdk::SetupContext {
                                device: gpu_renderer.device_arc(),
                                queue: gpu_renderer.queue_arc(),
                                registry: tiny_sdk::PluginRegistry::empty(),
                            };
                            if let Err(e) = diagnostics.setup(&mut setup_ctx) {
                                eprintln!("Failed to initialize diagnostics plugin: {:?}", e);
                            } else {
                                eprintln!("Diagnostics plugin initialized successfully");
                            }
                        }
                    }
                }
            }

            // Set up global margin (only once)
            static mut GLOBAL_MARGIN_INITIALIZED: bool = false;
            unsafe {
                if !GLOBAL_MARGIN_INITIALIZED {
                    GLOBAL_MARGIN_INITIALIZED = true;
                    // Title bar height is now handled by bounds calculation
                }
            }
        }

        // Upload font atlases only if dirty (atlas changed)
        if let Some(font_system) = &self.font_system {
            if font_system.is_dirty() {
                let atlas_data = font_system.atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                if let Some(gpu_renderer) = &mut self.gpu_renderer {
                    gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
                }
                font_system.clear_dirty();
            }

            // Upload color atlas if dirty
            if font_system.is_color_dirty() {
                let color_atlas_data = font_system.color_atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                if let Some(gpu_renderer) = &mut self.gpu_renderer {
                    gpu_renderer.upload_color_font_atlas(
                        &color_atlas_data,
                        atlas_width,
                        atlas_height,
                    );
                }
                font_system.clear_color_dirty();
            }
        }

        let doc = self.editor.doc();
        let doc_read = doc.read();

        // Get renderer state for uniforms
        let (viewport_size, scale_factor, current_time, theme_mode, cached_version) = {
            let cpu = self.cpu_renderer.as_ref().unwrap();
            let gpu = self.gpu_renderer.as_ref().unwrap();
            (
                [
                    cpu.viewport.physical_size.width as f32,
                    cpu.viewport.physical_size.height as f32,
                ],
                cpu.viewport.scale_factor,
                gpu.current_time,
                gpu.current_theme_mode,
                cpu.cached_doc_version,
            )
        };

        // Check if version changed without edits (undo/redo)
        let version_changed_without_edits = doc_read.version != cached_version;

        // Update cached doc state
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            cpu_renderer.cached_doc_text = Some(doc_read.flatten_to_string());
            cpu_renderer.cached_doc_version = doc_read.version;
        }

        // Apply pending renderer edits for syntax token adjustment
        // Note: text_renderer has already been swapped in from the active tab
        if let Some(cpu_renderer) = self.cpu_renderer.as_mut() {
            let pending_edits = self
                .editor
                .active_plugin_mut()
                .editor
                .input
                .take_renderer_edits();

            // If version changed without edits, it's undo/redo
            // Clear edit_deltas but KEEP stable_tokens - they'll be updated by background parse
            // This prevents white flash while keeping old (close enough) syntax visible
            if pending_edits.is_empty() && version_changed_without_edits {
                cpu_renderer.clear_edit_deltas();
                // Don't clear stable_tokens - causes white flash. Let background parse update them.
            }

            for edit in pending_edits {
                cpu_renderer.apply_incremental_edit(&edit);
            }
        }

        // Get tab_manager reference
        let tab_manager: Option<*const TabManager> = Some(&self.editor.tab_manager as *const _);

        // Prepare uniforms for GPU rendering
        let uniforms = Uniforms {
            viewport_size,
            scale_factor,
            time: current_time,
            theme_mode,
            _padding: [0.0, 0.0, 0.0],
        };

        // Set up CPU renderer state and render
        if let (Some(gpu_renderer), Some(cpu_renderer)) =
            (&mut self.gpu_renderer, &mut self.cpu_renderer)
        {
            cpu_renderer.set_gpu_renderer(gpu_renderer);

            // Just use the existing render pipeline - it was working!
            unsafe {
                let tab_manager_ref = tab_manager.map(|ptr| &*ptr);
                gpu_renderer.render_with_callback(uniforms, |render_pass| {
                    cpu_renderer.render_with_pass_and_context(
                        &doc_read,
                        Some(render_pass),
                        tab_manager_ref,
                    );
                });
            }
        }

        // Update diagnostics AFTER rendering (layout cache is now populated)
        if let Some(cpu_renderer) = self.cpu_renderer.as_mut() {
            let tab = self.editor.tab_manager.active_tab_mut();

            // Update diagnostics manager (handles LSP polling, caching, plugin updates)
            // NOTE: cpu_renderer.text_renderer now has populated layout cache from rendering
            eprintln!("🔍 [APP] Calling diagnostics.update() AFTER rendering, layout cache size: {}",
                cpu_renderer.text_renderer.layout_cache.len());
            tab.diagnostics
                .update(&tab.plugin.editor.view.doc, &cpu_renderer.text_renderer);

            // Swap the text_renderer back to the tab and save scroll position
            cpu_renderer.swap_text_renderer(&mut tab.text_renderer);
            // Save any scroll changes back to the tab
            tab.scroll_position = cpu_renderer.viewport.scroll;
        }
    }
}

impl ApplicationHandler for TinyApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            // Create window attributes
            let mut window_attributes = Window::default_attributes()
                .with_title(&self.window_title)
                .with_inner_size(winit::dpi::LogicalSize::new(
                    self.window_size.0,
                    self.window_size.1,
                ))
                .with_theme(Some(winit::window::Theme::Dark));

            // Apply macOS-specific transparent titlebar
            #[cfg(target_os = "macos")]
            {
                window_attributes = window_attributes
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true);
            }

            let window = Arc::new(
                event_loop
                    .create_window(window_attributes)
                    .expect("Failed to create window"),
            );

            // Setup GPU renderer
            let mut gpu_renderer = {
                let window_clone = window.clone();
                let inner_size = window_clone.inner_size();
                let size = tiny_sdk::PhysicalSize {
                    width: inner_size.width,
                    height: inner_size.height,
                };
                unsafe { pollster::block_on(GpuRenderer::new(window_clone, size)) }
            };

            // Initialize theme for syntax highlighting
            // Option 1: Single theme
            // let theme = crate::theme::Themes::one_dark();
            // gpu_renderer.init_themed_pipeline(&theme.generate_texture_data(), theme.max_colors_per_token.max(1) as u32);

            // Option 2: Rainbow theme with multi-color tokens
            let theme = crate::theme::Themes::one_dark(); // Load One Dark for shine effect
            gpu_renderer.init_themed_pipeline(
                &theme.generate_texture_data(),
                theme.max_colors_per_token.max(1) as u32,
            );

            // Set theme mode:
            // 0 = Pastel Rainbow
            // 1 = Vibrant Rainbow
            // 2 = Theme with Shine (One Dark with shine effect)
            // 3 = Static Theme
            // 4 = Theme Interpolation
            gpu_renderer.set_theme_mode(2); // Use shine effect!

            // Option 3: Interpolate between two themes (animated!)
            let _theme1 = crate::theme::Themes::monokai();
            let _theme2 = crate::theme::Themes::one_dark();
            // let texture_data = crate::theme::Theme::merge_for_interpolation(theme1, theme2);
            // let max_colors = theme1
            //     .max_colors_per_token
            //     .max(theme2.max_colors_per_token)
            //     .max(1) as u32;
            // gpu_renderer.init_themed_interpolation(texture_data, max_colors);

            // Register any custom shaders from the app logic
            for (shader_id, shader_source, uniform_size) in self.editor.register_shaders() {
                gpu_renderer.register_text_effect_shader(shader_id, shader_source, uniform_size);
            }

            // Setup font system
            let font_system = Arc::new(SharedFontSystem::new());

            // Apply font config (weight and italic)
            let config = crate::config::AppConfig::load().unwrap_or_default();
            font_system.set_default_weight(config.editor.font_weight);
            font_system.set_default_italic(config.editor.font_italic);

            // Get scale factor for high DPI displays
            let scale_factor = window.scale_factor() as f32;
            println!(
                "  Font size: {:.1}pt (scale={:.1}x, weight={}, italic={})",
                self.text_metrics.font_size,
                scale_factor,
                config.editor.font_weight,
                config.editor.font_italic
            );

            // Prerasterize ASCII characters at physical size for crisp rendering
            font_system.prerasterize_ascii(self.text_metrics.font_size * scale_factor);

            // Setup CPU renderer
            let mut cpu_renderer =
                Renderer::new(self.window_size, scale_factor, self.title_bar_height);
            cpu_renderer.set_font_size(self.text_metrics.font_size);
            cpu_renderer.set_font_system(font_system.clone());
            // Set line height after font_system (to override auto-calculated height)
            cpu_renderer.set_line_height(self.text_metrics.line_height);
            // Set theme for decoration color lookup
            cpu_renderer.set_theme(theme.clone());

            // Clone window for background threads before storing
            let window_for_events = window.clone();
            let window_for_cursor = window.clone();

            // Store everything
            self.window = Some(window);
            self.gpu_renderer = Some(gpu_renderer);
            self.font_system = Some(font_system);
            self.cpu_renderer = Some(cpu_renderer);

            // Setup shader hot-reloading
            self.setup_shader_watcher();

            // Setup config hot-reloading
            self.setup_config_watcher();

            self.editor.on_ready();

            // Initialize cursor/selection plugins for all EditableTextViews
            if let Some(ref cpu_renderer) = self.cpu_renderer {
                if let Some(loader_arc) = cpu_renderer.get_plugin_loader() {
                    let loader = loader_arc.lock().unwrap();
                    match self.editor.initialize_all_plugins(&loader) {
                        Ok(newly_initialized) => {
                            // Setup GPU resources for newly initialized views
                            drop(loader); // Release lock before setup
                            if !newly_initialized.is_empty() {
                                if let Some(gpu_renderer) = &self.gpu_renderer {
                                    let device = gpu_renderer.device_arc();
                                    let queue = gpu_renderer.queue_arc();
                                    if let Err(e) = self.editor.setup_plugins_for_views(
                                        newly_initialized,
                                        device,
                                        queue,
                                    ) {
                                        eprintln!(
                                            "Failed to setup EditableTextView plugins: {:?}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to initialize EditableTextView plugins: {}", e);
                        }
                    }
                }
            }

            // Set initial focus to main editor
            if let Some(tab) = self.editor.tab_manager.active_tab() {
                self.focused_editable_view_id = Some(tab.plugin.editor.id);
            }

            // Set initial window title
            self.update_window_title();

            // Set up event bus to wake the main thread (request redraw) when events arrive
            // This allows LSP background threads to emit events and trigger redraws
            self.event_bus.set_wake_notifier(move || {
                window_for_events.request_redraw();
            });

            // Start cursor blink timer (500ms intervals)
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                window_for_cursor.request_redraw();
            });

            // Initial render
            self.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                println!("Goodbye!");
                event_loop.exit();
            }

            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                // Handle key releases for modifier sequences like "shift shift"
                if key_event.state == ElementState::Released {
                    if let Some(trigger) = winit_adapter::convert_key(&key_event.logical_key) {
                        let is_modifier_key = matches!(
                            &trigger,
                            Trigger::Named(name)
                                if name == "Shift" || name == "Ctrl" || name == "Alt" || name == "Cmd"
                        );

                        if is_modifier_key {
                            // Feed modifier release to matcher for sequences like "shift shift"
                            let event_names =
                                self.shortcuts.match_input(&Modifiers::default(), &trigger);
                            if !event_names.is_empty() {
                                for event_name in event_names {
                                    self.event_bus.emit(event_name, json!({}), 10, "shortcuts");
                                }
                            }
                        }
                    }
                    return;
                }

                // Only handle key presses below
                if key_event.state == ElementState::Pressed {
                    // Capture original character BEFORE lowercasing (preserves shift for case/symbols)
                    let original_char = if let Key::Character(ch) = &key_event.logical_key {
                        if ch.len() == 1 {
                            Some(ch.as_str())
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some(trigger) = winit_adapter::convert_key(&key_event.logical_key) {
                        // Modifier keys as chords (for sequences like "shift shift") require
                        // press+release. Only feed them to the matcher if this is a release event
                        // (we'll track releases separately).
                        // Regular keys with modifiers (like Cmd+Shift+F) are matched immediately.
                        let is_modifier_key = matches!(
                            &trigger,
                            Trigger::Named(name)
                                if name == "Shift" || name == "Ctrl" || name == "Alt" || name == "Cmd"
                        );

                        // For regular (non-modifier) keys, use current modifier state
                        // For modifier keys themselves, we need to track release
                        let event_names = if !is_modifier_key {
                            self.shortcuts.match_input(&self.modifiers, &trigger)
                        } else {
                            // Skip modifier key presses - we'll handle them on release
                            Vec::new()
                        };

                        if !event_names.is_empty() {
                            // Shortcut matched - emit events
                            for event_name in event_names {
                                self.event_bus.emit(event_name, json!({}), 10, "shortcuts");
                            }
                        } else {
                            // No shortcut matched - check for plain character input
                            if let Some(ch) = original_char {
                                // Plain character with no cmd/ctrl/alt (shift is OK)
                                if !self.modifiers.cmd
                                    && !self.modifiers.ctrl
                                    && !self.modifiers.alt
                                {
                                    // Emit event for focused view to handle
                                    self.event_bus.emit(
                                        "editor.insert_char",
                                        json!({ "char": ch }),
                                        10,
                                        "keyboard",
                                    );
                                }
                            }
                        }
                    }
                }

                // Request immediate redraw to process events
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                // Convert winit modifiers to accelerator format
                self.modifiers = winit_adapter::convert_modifiers(&new_modifiers);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position);
            }

            WindowEvent::MouseInput { state, button, .. } => {
                // Convert button to trigger
                let trigger = match button {
                    WinitMouseButton::Left => Trigger::MouseButton(MouseButton::Left),
                    WinitMouseButton::Right => Trigger::MouseButton(MouseButton::Right),
                    WinitMouseButton::Middle => Trigger::MouseButton(MouseButton::Middle),
                    _ => return, // Ignore other buttons
                };

                match state {
                    ElementState::Pressed => {
                        // Try to match shortcuts first
                        let event_names = self.shortcuts.match_input(&self.modifiers, &trigger);

                        if !event_names.is_empty() {
                            // Shortcut matched (e.g., "cmd+click" or "click click")
                            for event_name in event_names {
                                self.event_bus.emit(event_name, json!({}), 10, "shortcuts");
                            }
                        } else {
                            // No shortcut - emit default mouse press event
                            if let Some(position) = self.cursor_position {
                                if let Some(point) = self.physical_to_logical_point(position) {
                                    // Check titlebar and tab bar
                                    #[cfg(target_os = "macos")]
                                    let is_in_titlebar = point.y.0 < self.title_bar_height;
                                    #[cfg(not(target_os = "macos"))]
                                    let is_in_titlebar = false;

                                    if !is_in_titlebar {
                                        let tab_bar_start = self.title_bar_height;
                                        let tab_bar_end = tab_bar_start + 30.0;
                                        let in_tab_bar =
                                            point.y.0 >= tab_bar_start && point.y.0 <= tab_bar_end;

                                        if in_tab_bar {
                                            let viewport_width = self
                                                .cpu_renderer
                                                .as_ref()
                                                .map(|r| r.viewport.logical_size.width.0);
                                            if let Some(viewport_width) = viewport_width {
                                                let click_x = point.x.0;
                                                let click_y = point.y.0 - tab_bar_start;
                                                if self.editor.handle_tab_bar_click(
                                                    click_x,
                                                    click_y,
                                                    viewport_width,
                                                ) {
                                                    self.request_redraw();
                                                }
                                            }
                                        } else {
                                            // Mouse click - emit event (drag state set in handler if needed)
                                            // Calculate both coordinate systems
                                            let editor_local =
                                                if let Some(cpu_renderer) = &self.cpu_renderer {
                                                    cpu_renderer.screen_to_editor_local(point)
                                                } else {
                                                    point
                                                };

                                            let button_name = match button {
                                                WinitMouseButton::Left => "Left",
                                                WinitMouseButton::Right => "Right",
                                                WinitMouseButton::Middle => "Middle",
                                                _ => "Unknown",
                                            };

                                            // Emit both screen and editor-local coordinates
                                            // Include physical position for drag state management
                                            self.event_bus.emit(
                                                "mouse.press",
                                                json!({
                                                    "x": editor_local.x.0,
                                                    "y": editor_local.y.0,
                                                    "screen_x": point.x.0,
                                                    "screen_y": point.y.0,
                                                    "physical_x": position.x,
                                                    "physical_y": position.y,
                                                    "button": button_name,
                                                    "modifiers": {
                                                        "shift": self.modifiers.shift,
                                                        "ctrl": self.modifiers.ctrl,
                                                        "alt": self.modifiers.alt,
                                                        "cmd": self.modifiers.cmd,
                                                    }
                                                }),
                                                10,
                                                "winit",
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ElementState::Released => {
                        self.mouse_pressed = false;
                        self.drag_start = None;
                        self.event_bus.emit("mouse.release", json!({}), 10, "winit");
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Determine wheel direction for accelerator matching
                let (delta_x, delta_y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x, y),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
                };

                // Determine primary direction
                let trigger = if delta_y.abs() > delta_x.abs() {
                    if delta_y > 0.0 {
                        Trigger::MouseWheel(WheelDirection::Up)
                    } else {
                        Trigger::MouseWheel(WheelDirection::Down)
                    }
                } else if delta_x > 0.0 {
                    Trigger::MouseWheel(WheelDirection::Right)
                } else if delta_x < 0.0 {
                    Trigger::MouseWheel(WheelDirection::Left)
                } else {
                    return; // No scroll
                };

                // Try to match shortcuts
                let event_names = self.shortcuts.match_input(&self.modifiers, &trigger);

                if !event_names.is_empty() {
                    // Shortcut matched
                    for event_name in event_names {
                        self.event_bus.emit(event_name, json!({}), 15, "shortcuts");
                    }
                } else {
                    // No shortcut - do default scroll behavior
                    self.handle_mouse_wheel(delta);
                }
            }

            WindowEvent::Resized(new_size) => {
                if let Some(gpu_renderer) = &mut self.gpu_renderer {
                    gpu_renderer.resize(tiny_sdk::PhysicalSize {
                        width: new_size.width,
                        height: new_size.height,
                    });
                }
                // Render immediately to prevent stretching during resize
                self.render_frame();
            }

            _ => {}
        }
    }
}
