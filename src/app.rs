//! Shared winit application abstraction
//!
//! Eliminates boilerplate across examples - focus on rendering logic

#[allow(unused)]
use std::io::BufRead;
use crate::{
    font::SharedFontSystem,
    gpu::GpuRenderer,
    render::Renderer,
    tree::{Content, Doc, Edit, Rect},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

/// Trait for handling application-specific logic
pub trait AppLogic {
    /// Handle keyboard input
    fn on_key(&mut self, _key: &winit::event::KeyEvent) -> bool {
        // Default implementation with basic editor functionality
        false
    }

    /// Get document to render
    fn doc(&self) -> &Doc;

    /// Get mutable document for editing
    fn doc_mut(&mut self) -> &mut Doc {
        panic!("This AppLogic implementation doesn't support editing")
    }

    /// Get cursor position
    fn cursor_pos(&self) -> usize {
        0
    }

    /// Set cursor position
    fn set_cursor_pos(&mut self, _pos: usize) {}

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
    pub cursor_pos: AtomicUsize,
}

impl EditorLogic {
    pub fn new(doc: Doc) -> Self {
        Self {
            doc,
            cursor_pos: AtomicUsize::new(0),
        }
    }
}

impl AppLogic for EditorLogic {
    fn on_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        println!("Key pressed: {:?}", event.logical_key);

        match &event.logical_key {
            Key::Character(ch) => {
                // Insert character at cursor
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting '{}' at position {}", ch, cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text(ch.to_string()),
                });
                self.doc.flush();

                // Move cursor forward
                self.cursor_pos.store(cursor_pos + ch.len(), Ordering::Relaxed);

                // Debug: print document content
                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true // Request redraw
            }
            Key::Named(NamedKey::Space) => {
                // Handle space character
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting space at position {}", cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text(" ".to_string()),
                });
                self.doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed);

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Enter) => {
                // Handle enter/newline
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting newline at position {}", cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text("\n".to_string()),
                });
                self.doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed);

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Tab) => {
                // Handle tab character
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting tab at position {}", cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text("\t".to_string()),
                });
                self.doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed); // Tab is one character

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Backspace) => {
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                if cursor_pos > 0 {
                    println!("Backspace at position {}", cursor_pos);

                    self.doc.edit(Edit::Delete {
                        range: cursor_pos - 1..cursor_pos,
                    });
                    self.doc.flush();

                    self.cursor_pos.store(cursor_pos - 1, Ordering::Relaxed);

                    let text = self.doc.read().to_string();
                    println!("Document now contains: '{}'", text);

                    true // Request redraw
                } else {
                    false
                }
            }
            Key::Named(NamedKey::F1) => {
                print_editor_info(&self.doc);
                false
            }
            Key::Named(NamedKey::F2) => {
                print_performance_stats();
                false
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                if cursor_pos > 0 {
                    self.cursor_pos.store(cursor_pos - 1, Ordering::Relaxed);
                    println!("Cursor moved left to position {}", cursor_pos - 1);
                    true
                } else {
                    false
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                let doc_len = self.doc.read().to_string().len();
                if cursor_pos < doc_len {
                    self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed);
                    println!("Cursor moved right to position {}", cursor_pos + 1);
                    true
                } else {
                    false
                }
            }
            Key::Named(NamedKey::Home) => {
                self.cursor_pos.store(0, Ordering::Relaxed);
                println!("Cursor moved to start (position 0)");
                true
            }
            Key::Named(NamedKey::End) => {
                let doc_len = self.doc.read().to_string().len();
                self.cursor_pos.store(doc_len, Ordering::Relaxed);
                println!("Cursor moved to end (position {})", doc_len);
                true
            }
            _ => false,
        }
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn doc_mut(&mut self) -> &mut Doc {
        &mut self.doc
    }

    fn cursor_pos(&self) -> usize {
        self.cursor_pos.load(Ordering::Relaxed)
    }

    fn set_cursor_pos(&mut self, pos: usize) {
        self.cursor_pos.store(pos, Ordering::Relaxed);
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
        fn on_key(&mut self, _event: &winit::event::KeyEvent) -> bool {
            false // No key handling
        }

        fn doc(&self) -> &Doc {
            &self.doc
        }
    }

    TinyApp::new(SimpleApp { doc }).with_title(title).run()
}

