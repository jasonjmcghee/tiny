//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::coordinates::{DocPos, LogicalPixels};
use crate::{
    font::SharedFontSystem,
    gpu::GpuRenderer,
    input::{InputAction, InputHandler},
    io,
    render::Renderer,
    syntax::SyntaxHighlighter,
    text_effects::TextStyleProvider,
    tree::{Doc, Point, Rect},
};
#[allow(unused)]
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

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

    /// Handle keyboard input with optional renderer for incremental updates
    fn on_key_with_renderer(
        &mut self,
        _key: &winit::event::KeyEvent,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
        _renderer: Option<&mut crate::render::Renderer>,
    ) -> bool {
        // Default fallback to regular on_key
        self.on_key(_key, _viewport, _modifiers)
    }

    /// Handle keyboard input
    fn on_key(
        &mut self,
        _key: &winit::event::KeyEvent,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
    ) -> bool {
        // Default implementation with basic editor functionality
        false
    }

    /// Handle mouse click at logical position
    fn on_click(
        &mut self,
        _pos: Point,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
    ) -> bool {
        false
    }

    /// Handle mouse drag from start to end position
    fn on_drag(
        &mut self,
        _from: Point,
        _to: Point,
        _viewport: &crate::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
    ) -> bool {
        false
    }

    /// Handle mouse move (for tracking position)
    fn on_mouse_move(&mut self, _pos: Point, _viewport: &crate::coordinates::Viewport) -> bool {
        false
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

    /// Set cursor position (for compatibility)
    fn set_cursor_pos(&mut self, _pos: usize) {}

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

    // Settings
    window_title: String,
    window_size: (f32, f32),
    font_size: f32,

    // Scroll lock settings
    scroll_lock_enabled: bool, // true = lock to one direction at a time
    current_scroll_direction: Option<ScrollDirection>, // which direction is currently locked

    // Track cursor position for clicks
    cursor_position: Option<winit::dpi::PhysicalPosition<f64>>,

    // Track modifier keys
    modifiers: winit::event::Modifiers,

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
    pub fn new(logic: T) -> Self {
        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            cpu_renderer: None,
            _shader_watcher: None,
            shader_reload_pending: Arc::new(AtomicBool::new(false)),
            logic,
            window_title: "Tiny Editor".to_string(),
            window_size: (800.0, 600.0),
            font_size: 14.0,
            scroll_lock_enabled: true, // Enabled by default
            current_scroll_direction: None,
            cursor_position: None,
            modifiers: winit::event::Modifiers::default(),
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
}

impl<T: AppLogic> ApplicationHandler for TinyApp<T> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            // Create window
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title(&self.window_title)
                            .with_inner_size(winit::dpi::LogicalSize::new(
                                self.window_size.0,
                                self.window_size.1,
                            )),
                    )
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
            let mut gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window.clone())) };

            // Initialize theme for syntax highlighting
            // Option 1: Single theme
            // let theme = crate::theme::Themes::one_dark();
            // gpu_renderer.init_themed_pipeline(&theme);

            // Option 2: Rainbow theme with multi-color tokens
            let theme = crate::theme::Themes::one_dark(); // Load One Dark for shine effect
            gpu_renderer.init_themed_pipeline(&theme);

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
            // gpu_renderer.init_themed_interpolation(&theme1, &theme2);

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
            // Font size is now managed by viewport metrics (defaults to 14.0)

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
                        let should_redraw = self.logic.on_key_with_renderer(
                            &event,
                            &viewport,
                            &self.modifiers,
                            Some(cpu_renderer),
                        );
                        if should_redraw {
                            // Update window title if using EditorLogic
                            if let Some(editor) = self.logic.as_any().downcast_ref::<EditorLogic>()
                            {
                                if let Some(window) = &self.window {
                                    window.set_title(&editor.title());
                                }
                            }

                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers;
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(position);

                // Call on_mouse_move for tracking
                if let (Some(window), Some(cpu_renderer)) = (&self.window, &self.cpu_renderer) {
                    let scale = window.scale_factor() as f32;
                    let logical_x = position.x as f32 / scale;
                    let logical_y = position.y as f32 / scale;

                    let point = Point {
                        x: LogicalPixels(logical_x),
                        y: LogicalPixels(logical_y),
                    };

                    if self.logic.on_mouse_move(point, &cpu_renderer.viewport) {
                        window.request_redraw();
                    }
                }

                // Check for drag if mouse is pressed
                if self.mouse_pressed {
                    if let (Some(window), Some(cpu_renderer), Some(start_pos), Some(end_pos)) = (
                        &self.window,
                        &self.cpu_renderer,
                        self.drag_start,
                        Some(position),
                    ) {
                        let scale = window.scale_factor() as f32;

                        let start_logical_x = start_pos.x as f32 / scale;
                        let start_logical_y = start_pos.y as f32 / scale;
                        let end_logical_x = end_pos.x as f32 / scale;
                        let end_logical_y = end_pos.y as f32 / scale;

                        let from_point = Point {
                            x: LogicalPixels(start_logical_x),
                            y: LogicalPixels(start_logical_y),
                        };

                        let to_point = Point {
                            x: LogicalPixels(end_logical_x),
                            y: LogicalPixels(end_logical_y),
                        };

                        let should_redraw = self.logic.on_drag(
                            from_point,
                            to_point,
                            &cpu_renderer.viewport,
                            &self.modifiers,
                        );
                        if should_redraw {
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                match state {
                    ElementState::Pressed => {
                        if let (Some(window), Some(cpu_renderer), Some(position)) =
                            (&self.window, &self.cpu_renderer, self.cursor_position)
                        {
                            self.mouse_pressed = true;
                            self.drag_start = self.cursor_position;

                            let scale = window.scale_factor() as f32;
                            let logical_x = position.x as f32 / scale;
                            let logical_y = position.y as f32 / scale;

                            // Convert to document coordinates
                            let point = Point {
                                x: LogicalPixels(logical_x),
                                y: LogicalPixels(logical_y),
                            };

                            let should_redraw =
                                self.logic
                                    .on_click(point, &cpu_renderer.viewport, &self.modifiers);
                            if should_redraw {
                                window.request_redraw();
                            }
                        }
                    }
                    ElementState::Released => {
                        self.mouse_pressed = false;
                        self.drag_start = None;
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
                    gpu_renderer.resize(new_size);
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
            .join("src/shaders");

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

    fn render_frame(&mut self) {
        // Check for pending shader reload
        if self.shader_reload_pending.load(Ordering::Relaxed) {
            if let Some(gpu_renderer) = &mut self.gpu_renderer {
                gpu_renderer.reload_shaders();
                self.shader_reload_pending.store(false, Ordering::Relaxed);
            }
        }

        if let (Some(window), Some(gpu_renderer), Some(cpu_renderer)) =
            (&self.window, &mut self.gpu_renderer, &mut self.cpu_renderer)
        {
            // Measure actual frame time for dynamic dt calculation
            let current_time = std::time::Instant::now();
            let frame_duration = current_time.duration_since(self.last_frame_time);
            self.last_frame_time = current_time;

            // Update logic
            self.logic.on_update();

            // Calculate dt - use actual frame time in continuous mode, theoretical time otherwise
            let dt = if self.continuous_rendering {
                // Use actual measured frame time for smooth animations
                frame_duration.as_secs_f32().min(0.05) // Cap at 50ms to prevent huge jumps
            } else {
                // Use theoretical monitor refresh rate for consistent timing
                self.target_frame_time_ms as f32 / 1000.0
            };

            // Update widgets (for animations like cursor blinking)
            cpu_renderer.update_widgets(dt);

            // Update GPU time for theme animations
            gpu_renderer.update_time(dt);

            // Handle continuous rendering with proper winit control flow
            if self.continuous_rendering {
                // For continuous rendering, just request next redraw immediately
                window.request_redraw();
            } else if !self.animation_timer_started.load(Ordering::Relaxed) {
                // For non-continuous mode, still update occasionally for cursor blink
                self.animation_timer_started.store(true, Ordering::Relaxed);
                let window_clone = window.clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    window_clone.request_redraw();
                });
            }

            if self.just_pressed_key {
                self.just_pressed_key = false;
                // Only scroll to cursor if it moved via keyboard, not every frame
                // This prevents fighting with manual mouse wheel scrolling
                if let Some(cursor_pos) = self.logic.get_cursor_doc_pos() {
                    let layout_pos = cpu_renderer.viewport.doc_to_layout(cursor_pos);
                    cpu_renderer.viewport.ensure_visible(layout_pos);
                }
            }

            // Calculate viewport dimensions
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            // Update CPU renderer viewport - this is where scale factor should be handled
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Set up text styles/syntax highlighter
            if let Some(text_styles) = self.logic.text_styles() {
                // If it's a SyntaxHighlighter, clone it and set both fields
                if let Some(syntax_hl) = text_styles
                    .as_any()
                    .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                {
                    let highlighter = Arc::new(syntax_hl.clone());
                    cpu_renderer.set_syntax_highlighter(highlighter.clone());
                    // SyntaxHighlighter implements TextStyleProvider
                    cpu_renderer.text_styles = Some(Box::new(syntax_hl.clone()));
                }
                // For other TextStyleProvider types, we can't clone them (trait objects)
                // so we'll rely on the syntax_highlighter field if it was set
            }

            // Define viewport for rendering
            let viewport = Rect {
                x: LogicalPixels(0.0),
                y: LogicalPixels(0.0),
                width: LogicalPixels(logical_width),
                height: LogicalPixels(logical_height),
            };

            // Generate render commands using direct GPU rendering path
            let doc = self.logic.doc();
            let selections = self.logic.selections();

            // Update widget manager with current selections if EditorLogic is being used
            // This ensures cursor and selection widgets are updated
            if let Some(editor) = self.logic.as_any().downcast_ref::<EditorLogic>() {
                // For now, always update widgets (widgets_dirty = true)
                cpu_renderer.set_selection_widgets(&editor.input, &editor.doc);
            }

            // Upload atlas (in case new glyphs were rasterized)
            if let Some(font_system) = &self.font_system {
                let atlas_data = font_system.atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
            }

            // Render directly with GPU - this will paint widgets
            unsafe {
                gpu_renderer.render_with_widgets(&doc.read(), viewport, selections, cpu_renderer);
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
    /// Whether document has unsaved changes
    pub is_modified: bool,
    /// Whether to show line numbers
    pub show_line_numbers: bool,
}

impl EditorLogic {
    pub fn with_text_style(mut self, style: Box<dyn TextStyleProvider>) -> Self {
        self.extra_text_styles.push(style);
        self
    }

    pub fn with_file(mut self, path: PathBuf) -> Self {
        // Update syntax highlighter based on file extension
        if let Some(path_str) = path.to_str() {
            // Determine what language this file needs
            let desired_language =
                crate::syntax::SyntaxHighlighter::file_extension_to_language(path_str);

            // Check if current syntax highlighter already matches
            let needs_new_highlighter =
                if let Some(ref current_highlighter) = self.syntax_highlighter {
                    if let Some(syntax_hl) = current_highlighter
                        .as_any()
                        .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                    {
                        // Check if the language name matches what we need
                        syntax_hl.name() != desired_language
                    } else {
                        true // Current provider is not a SyntaxHighlighter
                    }
                } else {
                    true // No highlighter at all
                };

            if needs_new_highlighter {
                if let Some(new_highlighter) =
                    crate::syntax::SyntaxHighlighter::from_file_path(path_str)
                {
                    println!(
                        "EditorLogic: Switching to {} syntax highlighter for {}",
                        new_highlighter.name(),
                        path_str
                    );
                    let syntax_highlighter: Box<dyn TextStyleProvider> = Box::new(new_highlighter);

                    // Update the syntax highlighter
                    self.syntax_highlighter = Some(syntax_highlighter);

                    // Connect new highlighter to input handler
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
                        path_str
                    );
                }
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

            // Always request update for the new file content (whether highlighter changed or not)
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

        self.file_path = Some(path);
        self
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        if let Some(ref path) = self.file_path {
            io::autosave(&self.doc, path)?;
            self.is_modified = false;
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

        let modified_marker = if self.is_modified { " (modified)" } else { "" };
        format!("{}{} - Tiny Editor", filename, modified_marker)
    }

    pub fn new(doc: Doc) -> Self {
        // Default to Rust syntax highlighting (can be overridden by with_file)
        let syntax_highlighter: Box<dyn TextStyleProvider> =
            Box::new(SyntaxHighlighter::new_rust());

        // Request initial highlight using the new API
        let text = doc.read().flatten_to_string();
        println!(
            "EditorLogic: Requesting initial syntax highlighting for {} bytes of text",
            text.len()
        );
        // Use the new API for consistency (no edit info for initial parse)
        if let Some(syntax_hl) = syntax_highlighter
            .as_any()
            .downcast_ref::<crate::syntax::SyntaxHighlighter>()
        {
            syntax_hl.request_update_with_edit(&text, doc.version(), None);
        } else {
            syntax_highlighter.request_update(&text, doc.version());
        }

        // Background thread will parse asynchronously - no need to block startup

        // Create input handler with syntax highlighter reference
        let mut input = InputHandler::new();
        if let Some(syntax_hl) = syntax_highlighter
            .as_any()
            .downcast_ref::<crate::syntax::SyntaxHighlighter>()
        {
            // Clone the SyntaxHighlighter and wrap in Arc for sharing
            let shared_highlighter = Arc::new(syntax_hl.clone());
            input.set_syntax_highlighter(shared_highlighter);
        }

        Self {
            doc,
            input,
            syntax_highlighter: Some(syntax_highlighter),
            widgets_dirty: true,
            extra_text_styles: Vec::new(),
            file_path: None,
            is_modified: false,
            show_line_numbers: true, // Enable by default
        }
    }
}

impl AppLogic for EditorLogic {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn on_key_with_renderer(
        &mut self,
        event: &winit::event::KeyEvent,
        viewport: &crate::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
        renderer: Option<&mut crate::render::Renderer>,
    ) -> bool {
        let action = self
            .input
            .on_key_with_renderer(&self.doc, viewport, event, modifiers, renderer);

        match action {
            InputAction::Save => {
                if let Err(e) = self.save() {
                    eprintln!("Failed to save: {}", e);
                }
                true // Redraw to update title
            }
            InputAction::Undo => {
                if self.input.undo(&self.doc) {
                    self.widgets_dirty = true;
                    self.is_modified = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redo => {
                if self.input.redo(&self.doc) {
                    self.widgets_dirty = true;
                    self.is_modified = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redraw => {
                self.widgets_dirty = true;
                self.is_modified = true;
                true
            }
            InputAction::None => false,
        }
    }

    fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        viewport: &crate::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        let action = self.input.on_key(&self.doc, viewport, event, modifiers);

        match action {
            InputAction::Save => {
                if let Err(e) = self.save() {
                    eprintln!("Failed to save: {}", e);
                }
                true // Redraw to update title
            }
            InputAction::Undo => {
                if self.input.undo(&self.doc) {
                    self.widgets_dirty = true;
                    self.is_modified = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redo => {
                if self.input.redo(&self.doc) {
                    self.widgets_dirty = true;
                    self.is_modified = true;
                    true
                } else {
                    false
                }
            }
            InputAction::Redraw => {
                self.widgets_dirty = true;
                self.is_modified = true;
                true
            }
            InputAction::None => false,
        }
    }

    fn on_click(
        &mut self,
        pos: Point,
        viewport: &crate::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        // Convert to mouse click for InputHandler
        let alt_held = modifiers.state().alt_key();
        self.input.on_mouse_click(
            &self.doc,
            viewport,
            pos,
            winit::event::MouseButton::Left,
            alt_held,
        );
        self.widgets_dirty = true; // Mark widgets for update
        true
    }

    fn on_drag(
        &mut self,
        from: Point,
        to: Point,
        viewport: &crate::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        // Convert to mouse drag for InputHandler
        let alt_held = modifiers.state().alt_key();
        self.input
            .on_mouse_drag(&self.doc, viewport, from, to, alt_held);
        self.widgets_dirty = true; // Mark widgets for update
        true
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

    fn set_cursor_pos(&mut self, _pos: usize) {
        // InputHandler doesn't expose a way to set cursor position directly
        // This would need to be added to InputHandler if needed
        // For now, just clear extra selections
        self.input.clear_selections();
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

#[allow(dead_code)]
fn print_editor_info(doc: &Doc) {
    println!("\n=== Editor Info ===");
    let tree = doc.read();
    println!("Document tree version: {}", tree.version);
    println!("Document size: {} bytes", tree.flatten_to_string().len());
    println!("Line count: {}", tree.flatten_to_string().lines().count());
}

/// Helper to run a simple app with just document rendering
pub fn run_simple_app(title: &str, doc: Doc) -> Result<(), Box<dyn std::error::Error>> {
    struct SimpleApp {
        doc: Doc,
    }

    impl AppLogic for SimpleApp {
        fn on_key(
            &mut self,
            _event: &winit::event::KeyEvent,
            _viewport: &crate::coordinates::Viewport,
            _modifiers: &winit::event::Modifiers,
        ) -> bool {
            false // No key handling
        }

        fn doc(&self) -> &Doc {
            &self.doc
        }
    }

    TinyApp::new(SimpleApp { doc }).with_title(title).run()
}
