//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::{
    input::{InputAction, InputHandler},
    input_types, io,
    render::Renderer,
    syntax::SyntaxHighlighter,
    text_effects::TextStyleProvider,
};
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

/// Trait for handling application-specific logic
pub trait AppLogic: 'static {
    /// Get as Any for downcasting
    fn as_any(&self) -> &dyn std::any::Any {
        // Default implementation returns empty reference
        &()
    }

    /// Get as mutable Any for downcasting
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        // Default implementation returns empty reference
        unreachable!("as_any_mut not implemented")
    }

    /// Handle keyboard input with optional renderer for incremental updates
    fn on_key_with_renderer(
        &mut self,
        _key: &input_types::KeyEvent,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &input_types::Modifiers,
        _renderer: Option<&mut crate::render::Renderer>,
    ) -> bool {
        // Default fallback to regular on_key
        self.on_key(_key, _viewport, _modifiers)
    }

    /// Handle keyboard input
    fn on_key(
        &mut self,
        _key: &input_types::KeyEvent,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &input_types::Modifiers,
    ) -> bool {
        // Default implementation with basic editor functionality
        false
    }

    /// Handle mouse click at logical position
    fn on_click(
        &mut self,
        _pos: Point,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &input_types::Modifiers,
    ) -> bool {
        false
    }

    /// Handle mouse drag from start to end position
    fn on_drag(
        &mut self,
        _from: Point,
        _to: Point,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &input_types::Modifiers,
    ) -> bool {
        false
    }

    /// Handle mouse move (for tracking position)
    fn on_mouse_move(&mut self, _pos: Point, _viewport: &crate::coordinates::Viewport) -> bool {
        false
    }

    /// Handle mouse button release (for cleaning up drag state)
    fn on_mouse_release(&mut self) {
        // Default implementation does nothing
    }

    /// Get document to render
    fn doc(&self) -> &Doc;

    /// Get mutable document for editing
    fn doc_mut(&mut self) -> &mut Doc {
        panic!("This AppLogic implementation doesn't support editing")
    }

    /// Get cursor position (for compatibility)
    fn cursor_pos(&self) -> usize {
        0
    }

    /// Get cursor document position for scrolling (returns None if no scrolling needed)
    fn get_cursor_doc_pos(&self) -> Option<DocPos> {
        None // Return None unless cursor actually moved
    }

    /// Get current selections for rendering
    fn selections(&self) -> &[crate::input::Selection] {
        &[] // Default to no selections
    }

    /// Get text style provider for syntax highlighting or other effects
    fn text_styles(&self) -> Option<&dyn TextStyleProvider> {
        None // Default to no text styles
    }

    /// Called after setup is complete
    fn on_ready(&mut self) {}

    /// Register custom text effect shaders (shader_id, shader_source, uniform_size)
    fn register_shaders(&self) -> Vec<(u32, &'static str, u64)> {
        vec![]
    }

    /// Called before each render (for animations, etc.)
    fn on_update(&mut self) {
        // Default implementation - subclasses can override
    }
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
pub struct TinyApp<T: AppLogic> {
    // Winit/GPU infrastructure
    window: Option<Arc<Window>>,
    gpu_renderer: Option<GpuRenderer>,
    font_system: Option<Arc<SharedFontSystem>>,
    cpu_renderer: Option<Renderer>,
    _shader_watcher: Option<notify::RecommendedWatcher>,
    shader_reload_pending: Arc<AtomicBool>,

    // Application-specific logic
    logic: T,

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

    // Key track
    just_pressed_key: bool,

    // Animation timer
    animation_timer_started: Arc<AtomicBool>,

    // Continuous rendering for animations
    continuous_rendering: bool,

    // Monitor refresh rate (cached frame time in milliseconds)
    target_frame_time_ms: u64,

    // Frame time tracking for dynamic dt
    last_frame_time: std::time::Instant,
}

impl<T: AppLogic> TinyApp<T> {
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
        if let Some(editor) = self.logic.as_any().downcast_ref::<EditorLogic>() {
            if let Some(window) = &self.window {
                window.set_title(&editor.title());
            }
        }
    }

    pub fn new(logic: T) -> Self {
        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            cpu_renderer: None,
            _shader_watcher: None,
            shader_reload_pending: Arc::new(AtomicBool::new(false)),
            logic,
            orchestrator: PluginOrchestrator::new(),
            window_title: "Tiny Editor".to_string(),
            window_size: (800.0, 600.0),
            font_size: 14.0,
            title_bar_height: 20.0,    // Logical pixels
            scroll_lock_enabled: true, // Enabled by default
            current_scroll_direction: None,
            cursor_position: None,
            modifiers: input_types::Modifiers::default(),
            mouse_pressed: false,
            drag_start: None,
            just_pressed_key: false,
            animation_timer_started: Arc::new(AtomicBool::new(false)),
            continuous_rendering: false,
            target_frame_time_ms: 16, // Default to 16ms (60fps), will be updated based on monitor
            last_frame_time: std::time::Instant::now(),
        }
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
}

impl<T: AppLogic> ApplicationHandler for TinyApp<T> {
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
                global_margin_y = 20.0;
            }

            let window = Arc::new(
                event_loop
                    .create_window(window_attributes)
                    .expect("Failed to create window"),
            );

            // Get monitor refresh rate for proper frame timing
            if let Some(monitor) = window.current_monitor() {
                if let Some(video_mode) = monitor.video_modes().next() {
                    let refresh_rate_hz = video_mode.refresh_rate_millihertz() / 1000;
                    if refresh_rate_hz > 0 {
                        // Calculate target frame time in milliseconds
                        self.target_frame_time_ms = 1000 / refresh_rate_hz as u64;
                        println!(
                            "Monitor refresh rate: {}Hz, target frame time: {}ms",
                            refresh_rate_hz, self.target_frame_time_ms
                        );
                    } else {
                        println!("Invalid refresh rate detected, using default 16ms (60Hz)");
                    }
                } else {
                    println!("No video modes available, using default 16ms (60Hz)");
                }
            } else {
                println!("No current monitor detected, using default 16ms (60Hz)");
            }

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
            for (shader_id, shader_source, uniform_size) in self.logic.register_shaders() {
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

            // Store everything
            self.window = Some(window);
            self.gpu_renderer = Some(gpu_renderer);
            self.font_system = Some(font_system);
            self.cpu_renderer = Some(cpu_renderer);

            // Setup shader hot-reloading
            self.setup_shader_watcher();

            self.logic.on_ready();

            // Set initial window title if using EditorLogic
            if let Some(editor) = self.logic.as_any().downcast_ref::<EditorLogic>() {
                if let Some(window) = &self.window {
                    window.set_title(&editor.title());
                }
            }

            // Initial render
            if let Some(window) = &self.window {
                window.request_redraw();
            }
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
                if event.state == ElementState::Pressed {
                    self.just_pressed_key = true;

                    // Check for font size adjustment (Cmd+= and Cmd+-)
                    #[cfg(target_os = "macos")]
                    let cmd_held = self.modifiers.state().super_key();
                    #[cfg(not(target_os = "macos"))]
                    let cmd_held = self.modifiers.state().control_key();

                    if cmd_held {
                        match &event.logical_key {
                            winit::keyboard::Key::Character(ch) if ch == "=" || ch == "+" => {
                                self.adjust_font_size(true);
                                return;
                            }
                            winit::keyboard::Key::Character(ch) if ch == "-" => {
                                self.adjust_font_size(false);
                                return;
                            }
                            _ => {}
                        }
                    }

                    // Check for scroll lock toggle (F12 key)
                    if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12) =
                        event.logical_key
                    {
                        self.scroll_lock_enabled = !self.scroll_lock_enabled;
                        self.current_scroll_direction = None; // Reset direction
                        println!(
                            "Scroll lock: {}",
                            if self.scroll_lock_enabled {
                                "ENABLED"
                            } else {
                                "DISABLED"
                            }
                        );
                        return;
                    }

                    if let Some(cpu_renderer) = &mut self.cpu_renderer {
                        // Extract viewport to avoid borrow conflicts
                        let viewport = cpu_renderer.viewport.clone();
                        // Convert winit event to our types
                        let key_event: input_types::KeyEvent = (&event).into();
                        let should_redraw = self.logic.on_key_with_renderer(
                            &key_event,
                            &viewport,
                            &self.modifiers,
                            Some(cpu_renderer),
                        );
                        if should_redraw {
                            self.update_window_title();
                            self.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                // Convert winit modifiers to our types
                self.modifiers = (&new_modifiers).into();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(position);

                // Pre-compute logical positions to avoid borrow issues
                let logical_point = self.physical_to_logical_point(position);
                let drag_from = self
                    .drag_start
                    .and_then(|p| self.physical_to_logical_point(p));

                if let Some(point) = logical_point {
                    if let Some(cpu_renderer) = &mut self.cpu_renderer {
                        // Mouse move
                        if self.logic.on_mouse_move(point, &cpu_renderer.viewport) {
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }

                        // Mouse drag
                        if self.mouse_pressed {
                            if let Some(from) = drag_from {
                                // Check if drag started in titlebar area (for transparent titlebar on macOS)
                                #[cfg(target_os = "macos")]
                                let drag_started_in_titlebar = from.y.0 < self.title_bar_height;
                                #[cfg(not(target_os = "macos"))]
                                let drag_started_in_titlebar = false;

                                // Only pass drag to editor if drag didn't start in titlebar area
                                if !drag_started_in_titlebar {
                                    if self.logic.on_drag(
                                        from,
                                        point,
                                        &cpu_renderer.viewport,
                                        &self.modifiers,
                                    ) {
                                        // Apply pending scroll from EditorLogic
                                        if let Some(editor) =
                                            self.logic.as_any_mut().downcast_mut::<EditorLogic>()
                                        {
                                            if let Some((dx, dy)) = editor.pending_scroll {
                                                cpu_renderer.viewport.scroll.x.0 += dx;
                                                cpu_renderer.viewport.scroll.y.0 += dy;

                                                // Clamp scroll to bounds using doc from editor directly
                                                let tree = editor.doc.read();
                                                cpu_renderer.viewport.clamp_scroll_to_bounds(&tree);

                                                // Clear the scroll so it doesn't keep applying
                                                editor.pending_scroll = None;
                                            }
                                        }

                                        if let Some(window) = &self.window {
                                            window.request_redraw();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. }
                if button == winit::event::MouseButton::Left =>
            {
                match state {
                    ElementState::Pressed => {
                        if let Some(position) = self.cursor_position {
                            self.mouse_pressed = true;
                            self.drag_start = Some(position);

                            if let Some(point) = self.physical_to_logical_point(position) {
                                // Check if click is in titlebar area (for transparent titlebar on macOS)
                                #[cfg(target_os = "macos")]
                                let is_in_titlebar = point.y.0 < self.title_bar_height;
                                #[cfg(not(target_os = "macos"))]
                                let is_in_titlebar = false;

                                // Only pass click to editor if not in titlebar area
                                if !is_in_titlebar {
                                    if let Some(cpu_renderer) = &mut self.cpu_renderer {
                                        if self.logic.on_click(
                                            point,
                                            &cpu_renderer.viewport,
                                            &self.modifiers,
                                        ) {
                                            self.request_redraw();
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ElementState::Released => {
                        self.mouse_pressed = false;
                        self.drag_start = None;

                        // Clear drag state in editor
                        self.logic.on_mouse_release();
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    let (scroll_x, scroll_y) = match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => (
                            x * &cpu_renderer.viewport.metrics.space_width,
                            y * &cpu_renderer.viewport.metrics.line_height,
                        ),
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            (pos.x as f32, pos.y as f32)
                        }
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
                    let doc = self.logic.doc();
                    let tree = doc.read();
                    viewport.clamp_scroll_to_bounds(&tree);

                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
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

impl<T: AppLogic> TinyApp<T> {
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

    fn update_frame_timing(&mut self) -> f32 {
        let current_time = std::time::Instant::now();
        let frame_duration = current_time.duration_since(self.last_frame_time);
        self.last_frame_time = current_time;

        if self.continuous_rendering {
            frame_duration.as_secs_f32().min(0.05) // Cap at 50ms to prevent huge jumps
        } else {
            self.target_frame_time_ms as f32 / 1000.0
        }
    }

    fn render_frame(&mut self) {
        // Check for pending shader reload
        if self.shader_reload_pending.load(Ordering::Relaxed) {
            if let Some(gpu_renderer) = &mut self.gpu_renderer {
                gpu_renderer.reload_shaders();
                self.shader_reload_pending.store(false, Ordering::Relaxed);
            }
        }

        let dt = self.update_frame_timing();
        self.logic.on_update();

        // Update plugins through orchestrator
        if let Err(e) = self.orchestrator.update_all(dt) {
            eprintln!("Plugin update error: {}", e);
        }

        // Setup continuous rendering
        if let Some(window) = &self.window {
            if self.continuous_rendering {
                window.request_redraw();
            } else if !self.animation_timer_started.load(Ordering::Relaxed) {
                self.animation_timer_started.store(true, Ordering::Relaxed);
                let window_clone = window.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    window_clone.request_redraw();
                });
            }
        }

        // Handle cursor scroll
        if self.just_pressed_key {
            self.just_pressed_key = false;
            if let Some(cursor_pos) = self.logic.get_cursor_doc_pos() {
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    let layout_pos = cpu_renderer.viewport.doc_to_layout(cursor_pos);
                    cpu_renderer.viewport.ensure_visible(layout_pos);
                }
            }
        }

        if let (Some(window), Some(gpu_renderer), Some(cpu_renderer)) =
            (&self.window, &mut self.gpu_renderer, &mut self.cpu_renderer)
        {
            cpu_renderer.update_widgets(dt);
            gpu_renderer.update_time(dt);

            // Update viewport
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Setup text styles
            if let Some(text_styles) = self.logic.text_styles() {
                if let Some(syntax_hl) = text_styles
                    .as_any()
                    .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                {
                    let highlighter = Arc::new(syntax_hl.clone());
                    cpu_renderer.set_syntax_highlighter(highlighter);
                    cpu_renderer.text_styles = Some(Box::new(syntax_hl.clone()));
                }
            }

            let viewport = Rect {
                x: LogicalPixels(0.0),
                y: LogicalPixels(0.0),
                width: LogicalPixels(logical_width),
                height: LogicalPixels(logical_height),
            };

            // Update widgets if EditorLogic
            if let Some(editor) = self.logic.as_any().downcast_ref::<EditorLogic>() {
                // Always update selection widgets
                cpu_renderer.set_selection_plugin(&editor.input, &editor.doc);

                // Set up global margin (only once)
                static mut GLOBAL_MARGIN_INITIALIZED: bool = false;
                unsafe {
                    if !GLOBAL_MARGIN_INITIALIZED {
                        GLOBAL_MARGIN_INITIALIZED = true;

                        // Set global margin for UI chrome space
                        cpu_renderer
                            .viewport
                            .set_global_margin(0.0, self.title_bar_height);
                    }
                }
            }

            // Upload font atlas
            if let Some(font_system) = &self.font_system {
                let atlas_data = font_system.atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
            }

            let doc = self.logic.doc();
            let selections = self.logic.selections();

            // Prepare uniforms for GPU rendering
            let uniforms = Uniforms {
                viewport_size: [
                    cpu_renderer.viewport.physical_size.width as f32,
                    cpu_renderer.viewport.physical_size.height as f32,
                ],
                scale_factor: cpu_renderer.viewport.scale_factor,
                time: gpu_renderer.current_time,
                theme_mode: gpu_renderer.current_theme_mode,
                _padding: [0.0, 0.0, 0.0],
            };

            // Set up CPU renderer state
            cpu_renderer.set_gpu_renderer(gpu_renderer);
            let doc_read = doc.read();
            cpu_renderer.cached_doc_text = Some(doc_read.flatten_to_string());
            cpu_renderer.cached_doc_version = doc_read.version;

            // Render using the callback API
            unsafe {
                gpu_renderer.render_with_callback(uniforms, |render_pass| {
                    cpu_renderer.render_with_pass_and_context(&doc_read, Some(render_pass));
                });
            }
        }
    }
}

/// Basic editor with cursor and text editing
pub struct EditorLogic {
    pub doc: Doc,
    pub input: InputHandler,
    pub syntax_highlighter: Option<Box<dyn TextStyleProvider>>,
    /// Flag to indicate widgets need updating
    widgets_dirty: bool,
    /// Extra text style providers (e.g., for effects)
    pub extra_text_styles: Vec<Box<dyn TextStyleProvider>>,
    /// File path if loaded from file
    pub file_path: Option<PathBuf>,
    /// Content hash when document was last saved
    pub last_saved_content_hash: u64,
    /// Whether to show line numbers
    pub show_line_numbers: bool,
    /// Pending scroll delta from drag operations
    pub pending_scroll: Option<(f32, f32)>,
}

impl EditorLogic {
    fn needs_syntax_highlighter_update(&self, path: &str) -> bool {
        let desired_language = crate::syntax::SyntaxHighlighter::file_extension_to_language(path);

        if let Some(ref current_highlighter) = self.syntax_highlighter {
            if let Some(syntax_hl) = current_highlighter
                .as_any()
                .downcast_ref::<crate::syntax::SyntaxHighlighter>()
            {
                syntax_hl.name() != desired_language
            } else {
                true
            }
        } else {
            true
        }
    }

    fn setup_syntax_highlighter(&mut self, path: &str) {
        if let Some(new_highlighter) = crate::syntax::SyntaxHighlighter::from_file_path(path) {
            println!(
                "EditorLogic: Switching to {} syntax highlighter for {}",
                new_highlighter.name(),
                path
            );
            let syntax_highlighter: Box<dyn TextStyleProvider> = Box::new(new_highlighter);
            self.syntax_highlighter = Some(syntax_highlighter);

            if let Some(ref syntax_highlighter) = self.syntax_highlighter {
                if let Some(syntax_hl) = syntax_highlighter
                    .as_any()
                    .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                {
                    let shared_highlighter = Arc::new(syntax_hl.clone());
                    self.input.set_syntax_highlighter(shared_highlighter);
                }
            }
        } else {
            println!(
                "EditorLogic: No syntax highlighter available for {}, keeping existing",
                path
            );
        }
    }

    fn request_syntax_update(&self) {
        if let Some(ref syntax_highlighter) = self.syntax_highlighter {
            let text = self.doc.read().flatten_to_string();
            if let Some(syntax_hl) = syntax_highlighter
                .as_any()
                .downcast_ref::<crate::syntax::SyntaxHighlighter>()
            {
                syntax_hl.request_update_with_edit(&text, self.doc.version(), None);
            }
        }
    }

    pub fn with_text_style(mut self, style: Box<dyn TextStyleProvider>) -> Self {
        self.extra_text_styles.push(style);
        self
    }

    pub fn with_file(mut self, path: PathBuf) -> Self {
        if let Some(path_str) = path.to_str() {
            if self.needs_syntax_highlighter_update(path_str) {
                self.setup_syntax_highlighter(path_str);
            } else {
                println!(
                    "EditorLogic: Keeping existing {} syntax highlighter for {}",
                    self.syntax_highlighter
                        .as_ref()
                        .unwrap()
                        .as_any()
                        .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                        .unwrap()
                        .name(),
                    path_str
                );
            }
            self.request_syntax_update();
        }

        self.file_path = Some(path);
        self
    }

    /// Check if document has unsaved changes by comparing content hash
    pub fn is_modified(&self) -> bool {
        let current_text = self.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        current_text.hash(&mut hasher);
        let current_hash = hasher.finish();

        current_hash != self.last_saved_content_hash
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if let Some(ref path) = self.file_path {
            io::autosave(&self.doc, path)?;

            // Update saved content hash
            let current_text = self.doc.read().flatten_to_string();
            let mut hasher = AHasher::default();
            current_text.hash(&mut hasher);
            self.last_saved_content_hash = hasher.finish();

            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No file path set",
            ))
        }
    }

    pub fn title(&self) -> String {
        let filename = if let Some(ref path) = self.file_path {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled")
                .to_string()
        } else {
            "Demo Text".to_string()
        };

        let modified_marker = if self.is_modified() {
            " (modified)"
        } else {
            ""
        };
        format!("{}{}", filename, modified_marker)
    }

    pub fn new(doc: Doc) -> Self {
        let syntax_highlighter: Box<dyn TextStyleProvider> =
            Box::new(SyntaxHighlighter::new_rust());

        let text = doc.read().flatten_to_string();
        println!(
            "EditorLogic: Requesting initial syntax highlighting for {} bytes of text",
            text.len()
        );

        if let Some(syntax_hl) = syntax_highlighter
            .as_any()
            .downcast_ref::<crate::syntax::SyntaxHighlighter>()
        {
            syntax_hl.request_update_with_edit(&text, doc.version(), None);
        } else {
            panic!("Syntax highlighter could not be used to update")
        }

        let mut input = InputHandler::new();
        if let Some(syntax_hl) = syntax_highlighter
            .as_any()
            .downcast_ref::<crate::syntax::SyntaxHighlighter>()
        {
            let shared_highlighter = Arc::new(syntax_hl.clone());
            input.set_syntax_highlighter(shared_highlighter);
        }

        // Calculate initial content hash
        let initial_text = doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        initial_text.hash(&mut hasher);
        let initial_hash = hasher.finish();

        Self {
            doc,
            input,
            syntax_highlighter: Some(syntax_highlighter),
            widgets_dirty: true,
            extra_text_styles: Vec::new(),
            file_path: None,
            last_saved_content_hash: initial_hash,
            show_line_numbers: true,
            pending_scroll: None,
        }
    }
}

impl EditorLogic {
    fn handle_input_action(&mut self, action: InputAction) -> bool {
        match action {
            InputAction::Save => {
                if let Err(e) = self.save() {
                    eprintln!("Failed to save: {}", e);
                }
                true
            }
            InputAction::Undo => {
                if self.input.undo(&self.doc) {
                    self.widgets_dirty = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redo => {
                if self.input.redo(&self.doc) {
                    self.widgets_dirty = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redraw => {
                self.widgets_dirty = true;
                true
            }
            InputAction::None => false,
        }
    }
}

impl AppLogic for EditorLogic {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn on_key_with_renderer(
        &mut self,
        event: &input_types::KeyEvent,
        viewport: &crate::coordinates::Viewport,
        modifiers: &input_types::Modifiers,
        renderer: Option<&mut crate::render::Renderer>,
    ) -> bool {
        let action = self
            .input
            .on_key_with_renderer(&self.doc, viewport, event, modifiers, renderer);
        self.handle_input_action(action)
    }

    fn on_key(
        &mut self,
        event: &input_types::KeyEvent,
        viewport: &crate::coordinates::Viewport,
        modifiers: &input_types::Modifiers,
    ) -> bool {
        let action = self.input.on_key(&self.doc, viewport, event, modifiers);
        self.handle_input_action(action)
    }

    fn on_click(
        &mut self,
        pos: Point,
        viewport: &crate::coordinates::Viewport,
        modifiers: &input_types::Modifiers,
    ) -> bool {
        // Convert to mouse click for InputHandler
        let alt_held = modifiers.state().alt_key();
        let shift_held = modifiers.state().shift_key();
        self.input.on_mouse_click(
            &self.doc,
            viewport,
            pos,
            input_types::MouseButton::Left,
            alt_held,
            shift_held,
        );
        self.widgets_dirty = true; // Mark widgets for update
        true
    }

    fn on_drag(
        &mut self,
        from: Point,
        to: Point,
        viewport: &crate::coordinates::Viewport,
        modifiers: &input_types::Modifiers,
    ) -> bool {
        // Convert to mouse drag for InputHandler
        let alt_held = modifiers.state().alt_key();
        let (_redraw, scroll_delta) = self
            .input
            .on_mouse_drag(&self.doc, viewport, from, to, alt_held);

        // Store scroll delta to be applied in render loop
        if scroll_delta.is_some() {
            self.pending_scroll = scroll_delta;
        }

        self.widgets_dirty = true; // Mark widgets for update
        true
    }

    fn on_mouse_release(&mut self) {
        self.input.clear_drag_anchor();
        self.pending_scroll = None;
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn doc_mut(&mut self) -> &mut Doc {
        &mut self.doc
    }

    fn cursor_pos(&self) -> usize {
        // Return first selection's cursor byte position for compatibility
        self.input
            .selections()
            .first()
            .map(|s| s.cursor.byte_offset)
            .unwrap_or(0)
    }

    fn get_cursor_doc_pos(&self) -> Option<DocPos> {
        Some(self.input.primary_cursor_doc_pos(&self.doc))
    }

    fn selections(&self) -> &[crate::input::Selection] {
        self.input.selections()
    }

    fn text_styles(&self) -> Option<&dyn TextStyleProvider> {
        self.syntax_highlighter.as_deref()
    }

    fn on_update(&mut self) {
        // Check if we should send pending syntax updates (debounce timer expired)
        if self.input.should_flush() {
            println!("DEBOUNCE: Sending pending syntax updates after idle timeout");
            self.input.flush_syntax_updates(&self.doc);
        }
    }
}
