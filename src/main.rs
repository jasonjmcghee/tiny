//! Ultra-minimal text editor - main entry point
//!
//! Demonstrates the complete system working together

use std::sync::Arc;
use tiny_editor::font::SharedFontSystem;
use tiny_editor::{gpu::GpuRenderer, Editor, Rect};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Tiny Editor...");

    // Create event loop
    let event_loop = EventLoop::new()?;

    // Create and run application
    let mut app = TinyEditorApp::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}

#[derive(Default)]
struct TinyEditorApp {
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    editor: Option<Editor>,
    font_system: Option<Arc<SharedFontSystem>>,
}

impl ApplicationHandler for TinyEditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Create window when app resumes
        if self.window.is_none() {
            println!("Creating window...");

            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("Tiny Editor - Ultra-Minimal Text Editor")
                            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0)),
                    )
                    .expect("Failed to create window"),
            );

            // Setup GPU renderer
            println!("Initializing GPU renderer...");
            let window_clone = window.clone();
            let renderer = unsafe { pollster::block_on(GpuRenderer::new(window_clone)) };

            // Create consolidated font system (shared across components)
            println!("Setting up font system...");
            let font_system = Arc::new(SharedFontSystem::new());

            // Get initial scale factor for display
            let scale_factor = window.scale_factor() as f32;
            println!("Initial scale_factor: {}", scale_factor);

            // Pre-rasterize ASCII characters at physical size (base * scale)
            font_system.prerasterize_ascii(14.0 * scale_factor);

            // Upload atlas to GPU
            let atlas_data = font_system.atlas_data();
            let (atlas_width, atlas_height) = font_system.atlas_size();

            // Debug: Check if atlas has actual data
            let non_zero_pixels = atlas_data.iter().filter(|&&p| p > 0).count();
            println!("Atlas has {} non-zero pixels out of {} total",
                     non_zero_pixels, atlas_data.len());

            renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);
            println!("Uploaded font atlas to GPU");

            self.window = Some(window);
            self.renderer = Some(renderer);
            self.font_system = Some(font_system);

            // Create editor with sample text
            println!("Creating editor...");

            // Get window size for editor initialization
            let window_ref = self.window.as_ref().unwrap();
            let size = window_ref.inner_size();
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            let mut editor = Editor::with_text(
                r#"// Welcome to Tiny Editor!
//
// This is an ultra-minimal text editor built from scratch.
// Everything is a span in a tree with lock-free reads!

fn main() {
    println!("Hello from tiny editor!");

    // Features:
    // - Lock-free reads via RCU pattern
    // - O(log n) operations via sum-tree
    // - GPU-accelerated rendering
    // - Tree-sitter syntax highlighting
    // - Everything is a widget

    let mut sum = 0;
    for i in 0..10 {
        sum += i;
    }
    println!("Sum: {}", sum);
}

// The entire editor architecture fits in ~3000 lines of Rust.
// Text, cursors, selections - all just spans in the tree!"#,
                (logical_width, logical_height),
                scale_factor,
            );

            // Enable Rust syntax highlighting
            editor.syntax = Some(tiny_editor::syntax::create_rust_highlighter());

            self.editor = Some(editor);

            println!("Editor ready!");
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

            WindowEvent::Resized(physical_size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(physical_size);
                }
                if let Some(window) = &self.window {
                    // Get scale factor and update renderer viewport
                    let scale_factor = window.scale_factor() as f32;
                    let logical_width = physical_size.width as f32 / scale_factor;
                    let logical_height = physical_size.height as f32 / scale_factor;

                    println!("Window resized: {:?}, scale_factor: {}", physical_size, scale_factor);

                    // Update editor viewport
                    if let Some(editor) = &mut self.editor {
                        editor.resize(logical_width, logical_height, scale_factor);
                    }

                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state == ElementState::Pressed {
                    if let Some(editor) = &mut self.editor {
                        // Handle the key input
                        editor.on_key(&key_event);

                        // Debug: print current text
                        if matches!(
                            key_event.logical_key,
                            winit::keyboard::Key::Named(winit::keyboard::NamedKey::F1)
                        ) {
                            println!("\n=== Current Text ===");
                            println!("{}", editor.text());
                            println!("===================\n");
                        }
                    }
                    // Request redraw
                    if let Some(window) = &self.window {
                        window.request_redraw();
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                if let (Some(renderer), Some(editor), Some(font_system)) =
                    (&mut self.renderer, &mut self.editor, &self.font_system)
                {
                    // Get window size and scale factor
                    let window = self.window.as_ref().unwrap();
                    let physical_size = window.inner_size();
                    let scale_factor = window.scale_factor() as f32;

                    // Convert to logical size for viewport
                    let logical_width = physical_size.width as f32 / scale_factor;
                    let logical_height = physical_size.height as f32 / scale_factor;

                    let viewport = Rect {
                        x: 0.0,
                        y: 0.0,
                        width: logical_width,
                        height: logical_height,
                    };

                    // Update renderer viewport with proper scale factor
                    editor.renderer.update_viewport(logical_width, logical_height, scale_factor);

                    // Set up renderer with font system
                    editor.renderer.set_font_system(font_system.clone());

                    // Render document to commands
                    let batches = editor.render(viewport);

                    // Re-upload atlas texture after potential new glyphs were rasterized
                    let atlas_data = font_system.atlas_data();
                    let (atlas_width, atlas_height) = font_system.atlas_size();
                    renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);

                    // Execute on GPU with logical viewport dimensions
                    unsafe {
                        renderer.render(&batches, (logical_width, logical_height));
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Could implement mouse selection here
                let _ = position;
            }

            WindowEvent::MouseInput { state, button, .. } => {
                // Could implement click-to-position cursor here
                let _ = (state, button);
            }

            _ => {}
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        // Trigger redraws for cursor blinking
        if matches!(cause, StartCause::Poll) {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }
}

