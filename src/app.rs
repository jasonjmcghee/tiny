//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::coordinates::{DocPos, LogicalPixels};
use crate::{
    font::SharedFontSystem,
    gpu::GpuRenderer,
    input::InputHandler,
    render::Renderer,
    syntax::SyntaxHighlighter,
    text_effects::TextStyleProvider,
    tree::{Doc, Point, Rect},
};
#[allow(unused)]
use std::io::BufRead;
use std::sync::Arc;
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
}

impl<T: AppLogic> TinyApp<T> {
    pub fn new(logic: T) -> Self {
        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            cpu_renderer: None,
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

            // Setup GPU renderer
            let gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window.clone())) };

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
            cpu_renderer.set_font_system(font_system.clone());
            // Font size is now managed by viewport metrics (defaults to 14.0)

            // Store everything
            self.window = Some(window);
            self.gpu_renderer = Some(gpu_renderer);
            self.font_system = Some(font_system);
            self.cpu_renderer = Some(cpu_renderer);

            self.logic.on_ready();

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

                    if let Some(cpu_renderer) = &self.cpu_renderer {
                        let should_redraw =
                            self.logic
                                .on_key(&event, cpu_renderer.viewport(), &self.modifiers);
                        if should_redraw {
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
                            cpu_renderer.viewport(),
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

                            let should_redraw = self.logic.on_click(
                                point,
                                cpu_renderer.viewport(),
                                &self.modifiers,
                            );
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
                            x * cpu_renderer.viewport().metrics.space_width,
                            y * cpu_renderer.viewport().metrics.line_height,
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
                    let viewport = cpu_renderer.viewport_mut();

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
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}

impl<T: AppLogic> TinyApp<T> {
    fn render_frame(&mut self) {
        if let (Some(window), Some(gpu_renderer), Some(cpu_renderer)) =
            (&self.window, &mut self.gpu_renderer, &mut self.cpu_renderer)
        {
            // Update logic
            self.logic.on_update();

            if self.just_pressed_key {
                self.just_pressed_key = false;
                // Only scroll to cursor if it moved via keyboard, not every frame
                // This prevents fighting with manual mouse wheel scrolling
                if let Some(cursor_pos) = self.logic.get_cursor_doc_pos() {
                    let layout_pos = cpu_renderer.viewport().doc_to_layout(cursor_pos);
                    cpu_renderer.viewport_mut().ensure_visible(layout_pos);
                }
            }

            // Calculate viewport dimensions
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            // Update CPU renderer viewport - this is where scale factor should be handled
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Set up syntax highlighter for InputEdit-aware rendering
            if let Some(text_styles) = self.logic.text_styles() {
                if let Some(syntax_hl) = text_styles
                    .as_any()
                    .downcast_ref::<crate::syntax::SyntaxHighlighter>()
                {
                    // Use the same syntax highlighter instance that gets InputEdit updates
                    let shared_highlighter = Arc::new(syntax_hl.clone());
                    cpu_renderer.set_syntax_highlighter(shared_highlighter);

                    // Also set as backup (renderer will prioritize syntax_highlighter)
                    cpu_renderer.set_text_styles_ref(text_styles);
                    println!("APP: Set InputEdit-aware syntax highlighter with fallback");
                } else {
                    // Fallback for other text style providers
                    cpu_renderer.set_text_styles_ref(text_styles);
                    println!("APP: Using legacy text styles provider");
                }
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

            // Use render_with_pass(None) to go through the working viewport query path
            let batches = cpu_renderer.render_with_pass(&doc.read(), viewport, selections, None);

            // Upload atlas (in case new glyphs were rasterized)
            if let Some(font_system) = &self.font_system {
                let atlas_data = font_system.atlas_data();
                let (atlas_width, atlas_height) = font_system.atlas_size();
                gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
            }

            // Execute on GPU with viewport for proper transformations
            unsafe {
                gpu_renderer.render(&batches, cpu_renderer.viewport());
            }
        }
    }
}

/// Basic editor with cursor and text editing
pub struct EditorLogic {
    pub doc: Doc,
    pub input: InputHandler,
    pub syntax_highlighter: Option<Box<dyn TextStyleProvider>>,
}

impl EditorLogic {
    pub fn new(doc: Doc) -> Self {
        // Always enable Rust syntax highlighting
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

        // Give the background thread a moment to parse (reduced debounce is 10ms)
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Create input handler with syntax highlighter reference
        let mut input = InputHandler::new();
        if let Some(syntax_hl) = syntax_highlighter
            .as_any()
            .downcast_ref::<crate::syntax::SyntaxHighlighter>()
        {
            // Clone the SyntaxHighlighter and wrap in Arc for sharing
            let shared_highlighter = Arc::new(syntax_hl.clone());
            input.set_syntax_highlighter(shared_highlighter);
            println!("EditorLogic: Connected syntax highlighter to InputHandler");
        }

        Self {
            doc,
            input,
            syntax_highlighter: Some(syntax_highlighter),
        }
    }
}

// Methods moved to AppLogic trait implementation

impl AppLogic for EditorLogic {
    fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        viewport: &crate::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        // Simply pass the key to InputHandler - it handles all coordination now
        let input_handled = self.input.on_key(&self.doc, viewport, event, modifiers);

        // The InputHandler now manages edit buffering and InputEdit coordination
        // We don't need to do anything here anymore!

        // Return whether we handled the input
        input_handled
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
