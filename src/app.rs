//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

#[allow(unused)]
use std::io::BufRead;
use crate::{
    font::SharedFontSystem,
    gpu::GpuRenderer,
    input::InputHandler,
    render::Renderer,
    tree::{Doc, Point, Rect},
};
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};
use crate::coordinates::{DocPos, LogicalPixels};

/// Trait for handling application-specific logic
pub trait AppLogic {
    /// Handle keyboard input
    fn on_key(&mut self, _key: &winit::event::KeyEvent, _viewport: &crate::coordinates::Viewport) -> bool {
        // Default implementation with basic editor functionality
        false
    }

    /// Handle mouse click at logical position
    fn on_click(&mut self, _pos: Point, _viewport: &crate::coordinates::Viewport) -> bool {
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
        None
    }

    /// Called after setup is complete
    fn on_ready(&mut self) {}

    /// Called before each render (for animations, etc.)
    fn on_update(&mut self) {}
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

    // Track cursor position for clicks
    cursor_position: Option<winit::dpi::PhysicalPosition<f64>>,
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
            cursor_position: None,
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
            println!("🪟 Creating window: {}", self.window_title);
            
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
            println!("🎮 Initializing GPU...");
            let gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window.clone())) };

            // Setup font system
            println!("🔤 Setting up fonts...");
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

            println!("✅ Setup complete!");
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
                println!("👋 Goodbye!");
                event_loop.exit();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Some(cpu_renderer) = &self.cpu_renderer {
                        let should_redraw = self.logic.on_key(&event, cpu_renderer.viewport());
                        if should_redraw {
                            if let Some(window) = &self.window {
                                window.request_redraw();
                            }
                        }
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Store cursor position for click handling
                self.cursor_position = Some(position);
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if let (Some(window), Some(cpu_renderer), Some(position)) =
                    (&self.window, &self.cpu_renderer, self.cursor_position) {

                    let scale = window.scale_factor() as f32;
                    let logical_x = position.x as f32 / scale;
                    let logical_y = position.y as f32 / scale;

                    // Convert to document coordinates
                    let point = Point {
                        x: LogicalPixels(logical_x),
                        y: LogicalPixels(logical_y),
                    };

                    let should_redraw = self.logic.on_click(point, cpu_renderer.viewport());
                    if should_redraw {
                        window.request_redraw();
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    let scroll_amount = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => {
                            y * cpu_renderer.viewport().metrics.line_height
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            pos.y as f32
                        }
                    };

                    // Update scroll in viewport
                    let viewport = cpu_renderer.viewport_mut();
                    let new_scroll_y = (viewport.scroll.y.0 - scroll_amount).max(0.0);
                    viewport.scroll.y = LogicalPixels(new_scroll_y);

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

            // Check if we need to scroll to make cursor visible
            if let Some(cursor_pos) = self.logic.get_cursor_doc_pos() {
                let layout_pos = cpu_renderer.viewport().doc_to_layout(cursor_pos);
                cpu_renderer.viewport_mut().ensure_visible(layout_pos);
            }

            // Calculate viewport dimensions
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            // Update CPU renderer viewport - this is where scale factor should be handled
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Define viewport for rendering
            let viewport = Rect {
                x: LogicalPixels(0.0),
                y: LogicalPixels(0.0),
                width: LogicalPixels(logical_width),
                height: LogicalPixels(logical_height),
            };

            // Generate render commands
            let doc = self.logic.doc();
            let batches = cpu_renderer.render(&doc.read(), viewport);

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
}

impl EditorLogic {
    pub fn new(doc: Doc) -> Self {
        Self {
            doc,
            input: InputHandler::new(),
        }
    }
}

impl AppLogic for EditorLogic {
    fn on_key(&mut self, event: &winit::event::KeyEvent, viewport: &crate::coordinates::Viewport) -> bool {
        // Delegate to InputHandler
        self.input.on_key(&self.doc, viewport, event);

        // Always redraw after keyboard input for now
        // InputHandler handles the actual logic
        true
    }

    fn on_click(&mut self, pos: Point, viewport: &crate::coordinates::Viewport) -> bool {
        // Convert to mouse click for InputHandler
        // Note: InputHandler expects alt_held for multi-cursor, we'll default to false
        // Check if Alt is held using keyboard modifiers (would need to track this)
        self.input.on_mouse_click(
            &self.doc,
            viewport,
            pos,
            winit::event::MouseButton::Left,
            false, // alt_held - would need to track keyboard state
        );
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
}

fn print_editor_info(doc: &Doc) {
    println!("\n=== Editor Info ===");
    let tree = doc.read();
    println!("Document tree version: {}", tree.version);
    println!("Document size: {} bytes", tree.to_string().len());
    println!("Line count: {}", tree.to_string().lines().count());
}

fn print_performance_stats() {
    println!("\n=== Performance Stats ===");
    println!("Architecture:");
    println!("  • Lock-free reads via ArcSwap");
    println!("  • O(log n) via sum-tree");
    println!("  • GPU accelerated rendering");
    println!("  • Zero-copy where possible");
    println!("\nEstimated performance:");
    println!("  • Read throughput: >1M ops/sec");
    println!("  • Input latency: <1ms");
    println!("  • Render: 60+ FPS");
}

/// Helper to run a simple app with just document rendering
pub fn run_simple_app(title: &str, doc: Doc) -> Result<(), Box<dyn std::error::Error>> {
    struct SimpleApp {
        doc: Doc,
    }

    impl AppLogic for SimpleApp {
        fn on_key(&mut self, _event: &winit::event::KeyEvent, _viewport: &crate::coordinates::Viewport) -> bool {
            false // No key handling
        }

        fn doc(&self) -> &Doc {
            &self.doc
        }
    }

    TinyApp::new(SimpleApp { doc }).with_title(title).run()
}

