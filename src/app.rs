//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

use crate::{
    font::SharedFontSystem,
    gpu::GpuRenderer,
    render::Renderer,
    tree::{Doc, Rect},
};
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

/// Trait for handling application-specific logic
pub trait AppLogic {
    /// Handle keyboard input
    fn on_key(&mut self, key: &winit::event::KeyEvent) -> bool; // Returns true if should redraw

    /// Get document to render
    fn doc(&self) -> &Doc;

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

            // Setup font system with physical font size
            println!("🔤 Setting up fonts...");
            let font_system = Arc::new(SharedFontSystem::new());

            // Calculate physical font size once and store it
            let scale_factor = window.scale_factor() as f32;
            let physical_font_size = self.font_size;
            println!(
                "  Font sizes: logical={:.1}pt, physical={:.1}pt (scale={:.1}x)",
                self.font_size, physical_font_size, scale_factor
            );

            // Setup CPU renderer
            let mut cpu_renderer = Renderer::new(self.window_size, scale_factor);
            cpu_renderer.set_font_system(font_system.clone());
            cpu_renderer.set_physical_font_size(physical_font_size);

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
                    let should_redraw = self.logic.on_key(&event);
                    if should_redraw {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();
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

            // Calculate viewport dimensions
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            // Update CPU renderer viewport - this is where scale factor should be handled
            cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);

            // Define viewport for rendering
            let viewport = Rect {
                x: 0.0,
                y: 0.0,
                width: logical_width,
                height: logical_height,
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

            // Execute on GPU with physical viewport dimensions (matches surface)
            let size = window.inner_size();
            unsafe {
                gpu_renderer.render(&batches, (size.width as f32, size.height as f32));
            }
        }
    }
}

/// Helper to run a simple app with just document rendering
pub fn run_simple_app(title: &str, doc: Doc) -> Result<(), Box<dyn std::error::Error>> {
    struct SimpleApp {
        doc: Doc,
    }

    impl AppLogic for SimpleApp {
        fn on_key(&mut self, _event: &winit::event::KeyEvent) -> bool {
            false // No key handling
        }

        fn doc(&self) -> &Doc {
            &self.doc
        }
    }

    TinyApp::new(SimpleApp { doc }).with_title(title).run()
}

