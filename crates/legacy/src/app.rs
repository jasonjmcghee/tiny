//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::{
    input::{self, Event, EventBus, InputAction, InputHandler},
    input_types, io,
    lsp_manager::LspManager,
    render::Renderer,
    text_editor_plugin::TextEditorPlugin,
    text_effects::TextStyleProvider,
};

pub use crate::editor_logic::EditorLogic;
use ahash::AHasher;
use std::hash::{Hash, Hasher};
#[allow(unused)]
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tiny_font::SharedFontSystem;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    platform::macos::WindowAttributesExtMacOS,
    window::{Window, WindowId},
};

// Plugin orchestration support
use tiny_core::{
    tree::{Doc, Point, Rect},
    GpuRenderer, Uniforms,
};
use tiny_sdk::{types::DocPos, Hook, LogicalPixels, Paintable as SdkPaint, Updatable as SdkUpdate};

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

    // Application-specific logic
    editor: EditorLogic,

    // Event bus for event-driven architecture
    event_bus: EventBus,

    // Plugin orchestrator (will eventually move to core)
    orchestrator: PluginOrchestrator,

    // Settings
    window_title: String,
    window_size: (f32, f32),
    font_size: f32,

    // Title bar settings
    title_bar_height: f32, // Logical pixels

    // Scroll lock settings
    scroll_lock_enabled: bool, // true = lock to one direction at a time
    current_scroll_direction: Option<ScrollDirection>, // which direction is currently locked

    // Track cursor position for clicks
    cursor_position: Option<winit::dpi::PhysicalPosition<f64>>,

    // Track modifier keys
    modifiers: input_types::Modifiers,

    // Track mouse drag
    mouse_pressed: bool,
    drag_start: Option<winit::dpi::PhysicalPosition<f64>>,

    // Track if cursor moved for scrolling
    cursor_needs_scroll: bool,

    // Continuous rendering for animations
    continuous_rendering: bool,

    // Frame time tracking for dynamic dt
    last_frame_time: std::time::Instant,
}

impl TinyApp {
    fn physical_to_logical_point(
        &self,
        position: winit::dpi::PhysicalPosition<f64>,
    ) -> Option<Point> {
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
    fn handle_cursor_moved(&mut self, position: winit::dpi::PhysicalPosition<f64>) {
        self.cursor_position = Some(position);

        // Pre-compute logical positions to avoid borrow issues
        let logical_point = self.physical_to_logical_point(position);
        let drag_from = self
            .drag_start
            .and_then(|p| self.physical_to_logical_point(p));

        if let Some(point) = logical_point {
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

            let cmd_held = self.modifiers.state().super_key();

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

            // Mouse move
            if self.editor.on_mouse_move(point, &viewport) {
                if let Some(window) = &self.window {
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
                            use serde_json::json;
                            self.event_bus.emit(
                                "app.mouse.drag",
                                json!({
                                    "from_x": from_local.x.0,
                                    "from_y": from_local.y.0,
                                    "to_x": to_local.x.0,
                                    "to_y": to_local.y.0,
                                    "modifiers": {
                                        "shift": self.modifiers.state().shift_key(),
                                        "ctrl": self.modifiers.state().control_key(),
                                        "alt": self.modifiers.state().alt_key(),
                                        "cmd": self.modifiers.state().super_key(),
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

    /// Handle mouse button input (press/release)
    fn handle_mouse_input(
        &mut self,
        state: ElementState,
        position: Option<winit::dpi::PhysicalPosition<f64>>,
    ) {
        use serde_json::json;

        match state {
            ElementState::Pressed => {
                if let Some(position) = position {
                    // Emit press event with editor-local coordinates
                    if let Some(point) = self.physical_to_logical_point(position) {
                        // Check if click is in titlebar area
                        #[cfg(target_os = "macos")]
                        let is_in_titlebar = point.y.0 < self.title_bar_height;
                        #[cfg(not(target_os = "macos"))]
                        let is_in_titlebar = false;

                        if !is_in_titlebar {
                            // Check if click is in tab bar area (before converting coordinates)
                            let tab_bar_start = self.title_bar_height;
                            let tab_bar_end = tab_bar_start + 30.0; // TAB_BAR_HEIGHT
                            let in_tab_bar = point.y.0 >= tab_bar_start && point.y.0 <= tab_bar_end;

                            let mut handled_by_tab_bar = false;
                            if in_tab_bar {
                                // Any click in tab bar region should be blocked from reaching editor
                                // Don't set drag_start for tab bar clicks
                                handled_by_tab_bar = true;

                                // Extract viewport width before mutable borrow
                                let viewport_width = self
                                    .cpu_renderer
                                    .as_ref()
                                    .map(|r| r.viewport.logical_size.width.0);

                                // Handle tab bar clicks
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
                            }

                            // Request redraw after handling tab bar (outside the mutable borrow)
                            if handled_by_tab_bar {
                                return; // Early return - already handled
                            }

                            // Only emit mouse press event and set drag state if not handled by tab bar
                            // Set drag state for editor clicks only
                            self.mouse_pressed = true;
                            self.drag_start = Some(position);

                            // Convert to editor-local coordinates if we have a renderer
                            let editor_local = if let Some(cpu_renderer) = &self.cpu_renderer {
                                cpu_renderer.screen_to_editor_local(point)
                            } else {
                                point
                            };

                            self.event_bus.emit(
                                "app.mouse.press",
                                json!({
                                    "x": editor_local.x.0,
                                    "y": editor_local.y.0,
                                    "button": "Left",
                                    "state": "pressed",
                                    "modifiers": {
                                        "shift": self.modifiers.state().shift_key(),
                                        "ctrl": self.modifiers.state().control_key(),
                                        "alt": self.modifiers.state().alt_key(),
                                        "cmd": self.modifiers.state().super_key(),
                                    }
                                }),
                                10, // Input priority
                                "winit",
                            );
                        }
                    }
                }
            }
            ElementState::Released => {
                self.mouse_pressed = false;
                self.drag_start = None;

                // Emit release event
                self.event_bus
                    .emit("app.mouse.release", json!({}), 10, "winit");
            }
        }
    }

    /// Handle mouse wheel scrolling with scroll lock logic
    fn handle_mouse_wheel(&mut self, delta: winit::event::MouseScrollDelta) {
        // Emit mouse wheel event to the event bus
        use serde_json::json;

        let (delta_x, delta_y) = match delta {
            winit::event::MouseScrollDelta::LineDelta(x, y) => (x, y),
            winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
        };

        self.event_bus.emit(
            "app.mouse.scroll",
            json!({
                "delta_x": delta_x,
                "delta_y": delta_y,
                "type": match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, _) => "line",
                    winit::event::MouseScrollDelta::PixelDelta(_) => "pixel",
                }
            }),
            15, // Slightly lower priority than direct input
            "winit",
        );

        // Request immediate redraw to process scroll events
        self.request_redraw();

        // Apply scroll with scroll lock logic
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            let (scroll_x, scroll_y) = match delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => (
                    x * &cpu_renderer.viewport.metrics.space_width,
                    y * &cpu_renderer.viewport.metrics.line_height,
                ),
                winit::event::MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
            };

            // Apply scroll lock logic
            let (final_scroll_x, final_scroll_y) = if self.scroll_lock_enabled {
                // Determine which direction to lock to
                let new_direction = if scroll_y.abs() > scroll_x.abs() {
                    ScrollDirection::Vertical
                } else if scroll_x.abs() > 0.0 {
                    ScrollDirection::Horizontal
                } else {
                    // No movement, keep current direction
                    self.current_scroll_direction
                        .unwrap_or(ScrollDirection::Vertical)
                };

                // Update current direction if we started scrolling
                if scroll_x.abs() > 0.0 || scroll_y.abs() > 0.0 {
                    self.current_scroll_direction = Some(new_direction);
                }

                // Apply scroll lock
                match new_direction {
                    ScrollDirection::Vertical => (0.0, scroll_y), // Only vertical
                    ScrollDirection::Horizontal => (scroll_x, 0.0), // Only horizontal
                }
            } else {
                // No scroll lock - free scrolling
                (scroll_x, scroll_y)
            };

            // Update scroll in viewport
            let viewport = &mut cpu_renderer.viewport;

            // Apply the scroll amounts (note: scroll values are inverted)
            let new_scroll_y = viewport.scroll.y.0 - final_scroll_y;
            let new_scroll_x = viewport.scroll.x.0 - final_scroll_x;
            viewport.scroll.y = LogicalPixels(new_scroll_y);
            viewport.scroll.x = LogicalPixels(new_scroll_x);

            // Apply document-based scroll bounds
            let doc = self.editor.doc();
            let tree = doc.read();
            viewport.clamp_scroll_to_bounds(&tree, cpu_renderer.editor_bounds);

            if let Some(window) = &self.window {
                window.request_redraw();
            }
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

        let mut event_bus = EventBus::new();

        // Register app-level event handlers from InputHandler
        InputHandler::register_app_handlers(&mut event_bus);

        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            cpu_renderer: None,
            _shader_watcher: None,
            shader_reload_pending: Arc::new(AtomicBool::new(false)),
            editor,
            event_bus,
            orchestrator: PluginOrchestrator::new(),
            window_title: "Tiny Editor".to_string(),
            window_size: (800.0, 600.0),
            font_size: 14.0,
            title_bar_height: 28.0,    // Logical pixels
            scroll_lock_enabled: true, // Enabled by default
            current_scroll_direction: None,
            cursor_position: None,
            modifiers: input_types::Modifiers::default(),
            mouse_pressed: false,
            drag_start: None,
            cursor_needs_scroll: false,
            continuous_rendering: false,
            last_frame_time: std::time::Instant::now(),
        }
    }

    pub fn with_config(mut self, config: &crate::config::AppConfig) -> Self {
        self.window_title = config.editor.window_title.clone();
        self.window_size = (config.editor.window_width, config.editor.window_height);
        self.font_size = config.editor.font_size;
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
        self.font_size = size;
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
        self.font_size = (self.font_size + delta).clamp(6.0, 72.0); // Clamp between reasonable limits

        println!("Font size changed to: {:.1}pt", self.font_size);

        // Update CPU renderer with new font size
        if let Some(cpu_renderer) = &mut self.cpu_renderer {
            cpu_renderer.set_font_size(self.font_size);

            // Re-set font system to recalculate line height
            if let Some(font_system) = &self.font_system {
                cpu_renderer.set_font_system(font_system.clone());
            }
        }

        // Update font system with new size and clear cache
        if let Some(font_system) = &self.font_system {
            if let Some(window) = &self.window {
                let scale_factor = window.scale_factor() as f32;
                // This will clear the cache and prerasterize at the new size
                font_system.prerasterize_ascii(self.font_size * scale_factor);

                // Re-upload the font atlas to GPU
                if let Some(gpu_renderer) = &self.gpu_renderer {
                    let atlas_data = font_system.atlas_data();
                    let (atlas_width, atlas_height) = font_system.atlas_size();
                    gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
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

    fn process_event_queue(&mut self) {
        // Process all events in the queue
        loop {
            // Get events to process
            let mut events_to_process = Vec::new();
            std::mem::swap(&mut events_to_process, &mut self.event_bus.queued);

            if events_to_process.is_empty() {
                break;
            }

            // Sort by priority
            events_to_process.sort_by_key(|e| e.priority);

            // Process each event
            for event in events_to_process {
                // Handle app-level command events (from InputHandler's registered handlers)
                match event.name.as_str() {
                    "app.command.adjust_font_size" => {
                        let increase = event
                            .data
                            .get("increase")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        self.adjust_font_size(increase);
                        continue;
                    }
                    "app.command.toggle_scroll_lock" => {
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
                        continue;
                    }
                    "app.mouse.release" => {
                        self.editor.on_mouse_release();
                        continue;
                    }
                    "app.drag.scroll" => {
                        // Apply drag scroll delta
                        if let (Some(dx), Some(dy)) = (
                            event.data.get("delta_x").and_then(|v| v.as_f64()),
                            event.data.get("delta_y").and_then(|v| v.as_f64()),
                        ) {
                            // Get doc directly from editor to avoid borrow issues
                            let doc = self.editor.doc();
                            let tree = doc.read();

                            if let Some(cpu_renderer) = &mut self.cpu_renderer {
                                cpu_renderer.viewport.scroll.x.0 += dx as f32;
                                cpu_renderer.viewport.scroll.y.0 += dy as f32;

                                // Clamp scroll to bounds
                                let editor_bounds = cpu_renderer.editor_bounds;
                                cpu_renderer
                                    .viewport
                                    .clamp_scroll_to_bounds(&tree, editor_bounds);

                                self.request_redraw();
                            }
                        }
                        continue;
                    }
                    // Navigation events - handled by EditorLogic
                    "app.action.open_file_picker"
                    | "app.action.nav_back"
                    | "app.action.nav_forward"
                    | "app.action.goto_definition" => {
                        if let Some(needs_redraw) =
                            self.editor.handle_navigation_event(event.name.as_str())
                        {
                            if needs_redraw {
                                self.request_redraw();
                                self.cursor_needs_scroll = true;
                            }
                        }
                        continue;
                    }
                    _ => {}
                }

                // Handle file picker events (if visible and it's a keyboard event)
                if self.editor.handle_file_picker_event(&event) {
                    self.request_redraw();
                    continue;
                }

                // Extract viewport for input processing
                let viewport = self.cpu_renderer.as_ref().map(|r| r.viewport.clone());

                // Process input events for document editing
                if let Some(viewport) = viewport {
                    let action =
                        self.editor
                            .handle_input_event(&event, &viewport, &mut self.event_bus);
                    if action == InputAction::Redraw {
                        self.request_redraw();
                        self.update_window_title();
                        self.cursor_needs_scroll = true;
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

        // Check for pending shader reload
        if self.shader_reload_pending.load(Ordering::Relaxed) {
            if let Some(gpu_renderer) = &mut self.gpu_renderer {
                gpu_renderer.reload_shaders();
                self.shader_reload_pending.store(false, Ordering::Relaxed);
            }
        }

        let dt = self.update_frame_timing();
        self.editor.on_update();

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
                    let layout_pos = cpu_renderer.viewport.doc_to_layout(cursor_pos);
                    cpu_renderer.viewport.ensure_visible(layout_pos);
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

        let viewport = Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(logical_width),
            height: LogicalPixels(logical_height),
        };

        // Update plugins for editor
        if let Some(cpu_renderer) = self.cpu_renderer.as_mut() {
            let tab = self.editor.tab_manager.active_tab_mut();

            // Swap in the active tab's text_renderer to preserve per-tab state
            cpu_renderer.swap_text_renderer(&mut tab.text_renderer);

            // Always update selection widgets
            cpu_renderer.set_selection_plugin(&tab.plugin.input, &tab.plugin.doc);

            // Set line numbers plugin with fresh document reference
            cpu_renderer.set_line_numbers_plugin(&mut tab.line_numbers, &tab.plugin.doc);

            // Set tab bar and file picker plugins (global UI)
            cpu_renderer.set_tab_bar_plugin(&mut self.editor.tab_bar);
            cpu_renderer.set_file_picker_plugin(&mut self.editor.file_picker);

            // Mark renderer UI dirty if UI changed
            if self.editor.ui_changed {
                cpu_renderer.mark_ui_dirty();
                self.editor.ui_changed = false;
            }

            // Update diagnostics manager (handles LSP polling, caching, plugin updates)
            tab.diagnostics.update(&tab.plugin.doc);

            // Set diagnostics plugin for rendering
            cpu_renderer.set_diagnostics_plugin(tab.diagnostics.plugin_mut(), &tab.plugin.doc);

            // Initialize diagnostics plugin with GPU resources (first time only)
            static mut DIAGNOSTICS_INITIALIZED: bool = false;
            unsafe {
                if !DIAGNOSTICS_INITIALIZED {
                    if let Some(diagnostics_ptr) = cpu_renderer.diagnostics_plugin {
                        let diagnostics = &mut *diagnostics_ptr;
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
                                DIAGNOSTICS_INITIALIZED = true;
                                eprintln!("Diagnostics plugin initialized successfully");
                            }
                        }
                    }
                }
            }

            // Set up global margin (only once)
            let title_bar_height = self.title_bar_height;
            static mut GLOBAL_MARGIN_INITIALIZED: bool = false;
            unsafe {
                if !GLOBAL_MARGIN_INITIALIZED {
                    GLOBAL_MARGIN_INITIALIZED = true;
                    cpu_renderer
                        .viewport
                        .set_global_margin(0.0, title_bar_height);
                }
            }
        }

        // Upload font atlas only if dirty (atlas changed)
        if let Some(font_system) = &self.font_system {
            if font_system.is_dirty() {
                let atlas_data = font_system.atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                if let Some(gpu_renderer) = &mut self.gpu_renderer {
                    gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
                }
                font_system.clear_dirty();
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
            let pending_edits = self.editor.active_plugin_mut().input.take_renderer_edits();

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
        let tab_manager: Option<*const crate::tab_manager::TabManager> =
            Some(&self.editor.tab_manager as *const _);

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

        // Swap the text_renderer back to the tab to preserve state
        if let Some(cpu_renderer) = self.cpu_renderer.as_mut() {
            let tab = self.editor.tab_manager.active_tab_mut();
            cpu_renderer.swap_text_renderer(&mut tab.text_renderer);
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

            let mut global_margin_y = 0.0;
            // Apply macOS-specific transparent titlebar
            #[cfg(target_os = "macos")]
            {
                window_attributes = window_attributes
                    .with_titlebar_transparent(true)
                    .with_fullsize_content_view(true);
                global_margin_y = self.title_bar_height;
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

            // Get scale factor for high DPI displays
            let scale_factor = window.scale_factor() as f32;
            println!(
                "  Font size: {:.1}pt (scale={:.1}x)",
                self.font_size, scale_factor
            );

            // Prerasterize ASCII characters at physical size for crisp rendering
            font_system.prerasterize_ascii(self.font_size * scale_factor);

            // Setup CPU renderer
            let mut cpu_renderer = Renderer::new(self.window_size, scale_factor);
            cpu_renderer.set_font_size(self.font_size);
            cpu_renderer.set_font_system(font_system.clone());

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

            self.editor.on_ready();

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

            WindowEvent::KeyboardInput { event, .. } => {
                // Convert winit event to proper JSON format for event bus
                use serde_json::json;

                // Build proper key data
                let key_data = match &event.logical_key {
                    winit::keyboard::Key::Character(ch) => json!({
                        "type": "character",
                        "value": ch.to_string(),
                    }),
                    winit::keyboard::Key::Named(named) => {
                        use winit::keyboard::NamedKey;
                        let name = match named {
                            NamedKey::Enter => "Enter",
                            NamedKey::Tab => "Tab",
                            NamedKey::Backspace => "Backspace",
                            NamedKey::Delete => "Delete",
                            NamedKey::ArrowLeft => "ArrowLeft",
                            NamedKey::ArrowRight => "ArrowRight",
                            NamedKey::ArrowUp => "ArrowUp",
                            NamedKey::ArrowDown => "ArrowDown",
                            NamedKey::Home => "Home",
                            NamedKey::End => "End",
                            NamedKey::PageUp => "PageUp",
                            NamedKey::PageDown => "PageDown",
                            NamedKey::Space => "Space",
                            NamedKey::Shift => "Shift",
                            NamedKey::F12 => "F12",
                            _ => "Unknown",
                        };
                        json!({
                            "type": "named",
                            "value": name,
                        })
                    }
                    _ => json!({
                        "type": "unknown",
                        "value": null,
                    }),
                };

                self.event_bus.emit(
                    "app.keyboard.keypress",
                    json!({
                        "key": key_data,
                        "state": if event.state == ElementState::Pressed { "pressed" } else { "released" },
                        "modifiers": {
                            "shift": self.modifiers.state().shift_key(),
                            "ctrl": self.modifiers.state().control_key(),
                            "alt": self.modifiers.state().alt_key(),
                            "cmd": self.modifiers.state().super_key(),
                        }
                    }),
                    10, // Input priority
                    "winit",
                );

                // Font size and scroll lock will be handled through event handlers
                // Only emit these special events on key press (not release)
                if event.state == ElementState::Pressed {
                    #[cfg(target_os = "macos")]
                    let cmd_held = self.modifiers.state().super_key();
                    #[cfg(not(target_os = "macos"))]
                    let cmd_held = self.modifiers.state().control_key();

                    if cmd_held {
                        match &event.logical_key {
                            winit::keyboard::Key::Character(ch) if ch == "=" || ch == "+" => {
                                self.event_bus.emit(
                                    "app.action.font_increase",
                                    json!({}),
                                    5,
                                    "winit",
                                );
                            }
                            winit::keyboard::Key::Character(ch) if ch == "-" => {
                                self.event_bus.emit(
                                    "app.action.font_decrease",
                                    json!({}),
                                    5,
                                    "winit",
                                );
                            }
                            _ => {}
                        }
                    }

                    // F12 for scroll lock toggle
                    if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12) =
                        event.logical_key
                    {
                        self.event_bus
                            .emit("app.action.toggle_scroll_lock", json!({}), 5, "winit");
                    }
                }

                // Request immediate redraw to process events without waiting for timer
                self.request_redraw();
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                // Convert winit modifiers to our types
                self.modifiers = (&new_modifiers).into();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position);
            }

            WindowEvent::MouseInput { state, button, .. }
                if button == winit::event::MouseButton::Left =>
            {
                self.handle_mouse_input(state, self.cursor_position);
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_mouse_wheel(delta);
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

/// Find word boundaries at a given column position in a line
/// Returns (start_col, end_col) if a word is found, None otherwise
fn find_word_at_position(line_text: &str, column: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line_text.chars().collect();
    if column >= chars.len() {
        return None;
    }

    // Check if current character is part of an identifier
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    if !is_word_char(chars[column]) {
        return None;
    }

    // Find start of word
    let mut start = column;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }

    // Find end of word
    let mut end = column;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }

    Some((start, end))
}
