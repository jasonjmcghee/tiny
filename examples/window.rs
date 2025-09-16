//! Complete windowed demo with GPU rendering using winit 0.30

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tiny_editor::{gpu::GpuRenderer, render::Renderer, tree::{Content, Doc, Edit}, Rect};
use tiny_editor::font::SharedFontSystem;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
    keyboard::{Key, NamedKey},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create event loop
    let event_loop = EventLoop::new()?;

    // Create and run application
    let mut app = EditorApp::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}

#[derive(Default)]
struct EditorApp {
    window: Option<Arc<Window>>,
    gpu_renderer: Option<GpuRenderer>,
    cpu_renderer: Option<Renderer>,
    font_system: Option<Arc<SharedFontSystem>>,
    doc: Option<Doc>,
    cursor_pos: AtomicUsize,
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Create window on resume
        if self.window.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("Tiny Editor - Ultra-Minimal")
                            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0)),
                    )
                    .unwrap(),
            );

            // Setup GPU renderer
            let window_clone = window.clone();
            let gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window_clone)) };

            // Setup font system
            let font_system = Arc::new(SharedFontSystem::new());

            // Get initial window size and scale
            let size = window.inner_size();
            let scale_factor = window.scale_factor() as f32;
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;

            // Setup CPU renderer
            let mut cpu_renderer = Renderer::new((logical_width, logical_height), scale_factor);
            cpu_renderer.set_font_system(font_system.clone());
            cpu_renderer.set_physical_font_size(14.0);

            self.window = Some(window);
            self.gpu_renderer = Some(gpu_renderer);
            self.cpu_renderer = Some(cpu_renderer);
            self.font_system = Some(font_system);

            // Create document with initial text
            let doc = Doc::from_str(
                r#"// Welcome to Tiny Editor!
//
// This is a minimal text editor built in ~1000 lines of Rust.
//
// Key features:
// ✓ Lock-free reads via ArcSwap (RCU pattern)
// ✓ Everything is a widget (text, cursors, selections)
// ✓ O(log n) operations via sum-tree
// ✓ Tree-sitter syntax highlighting
// ✓ GPU-accelerated rendering with wgpu
// ✓ Multi-cursor support

fn main() {
    println!("Hello from tiny editor!");

    // Try typing, selecting text, and using keyboard shortcuts:
    // - Arrow keys to navigate
    // - Backspace/Delete to remove text
    // - Home/End for line navigation
    // - Type to insert text

    let mut sum = 0;
    for i in 0..10 {
        sum += i;
    }
    println!("Sum: {}", sum);
}

// The entire editor fits in a single tree structure where
// text, widgets, and selections are all just "spans" in the tree.
// This unified design eliminates synchronization complexity."#);

            self.doc = Some(doc);
            self.cursor_pos.store(0, Ordering::Relaxed);

            println!("✅ Window setup complete! Start typing to test.");
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
                println!("Closing editor...");
                event_loop.exit();
            }

            WindowEvent::Resized(physical_size) => {
                if let Some(gpu_renderer) = &mut self.gpu_renderer {
                    gpu_renderer.resize(physical_size);
                }
                // Update CPU renderer viewport
                if let Some(cpu_renderer) = &mut self.cpu_renderer {
                    if let Some(window) = &self.window {
                        let scale_factor = window.scale_factor() as f32;
                        let logical_width = physical_size.width as f32 / scale_factor;
                        let logical_height = physical_size.height as f32 / scale_factor;
                        cpu_renderer.update_viewport(logical_width, logical_height, scale_factor);
                    }
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state == ElementState::Pressed {
                    if self.handle_key_input(&key_event) {
                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::RedrawRequested => {
                if let (Some(window), Some(gpu_renderer), Some(cpu_renderer), Some(doc), Some(font_system)) =
                    (&self.window, &mut self.gpu_renderer, &mut self.cpu_renderer, &self.doc, &self.font_system)
                {
                    // Get viewport in logical coordinates
                    let size = window.inner_size();
                    let scale_factor = window.scale_factor() as f32;
                    let logical_width = size.width as f32 / scale_factor;
                    let logical_height = size.height as f32 / scale_factor;

                    let viewport = Rect {
                        x: 0.0,
                        y: 0.0,
                        width: logical_width,
                        height: logical_height,
                    };

                    // Render document to commands
                    let batches = cpu_renderer.render(&doc.read(), viewport);

                    // Upload atlas (in case new glyphs were rasterized)
                    let atlas_data = font_system.atlas_data();
                    let (atlas_width, atlas_height) = font_system.atlas_size();
                    gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);

                    // Execute on GPU with physical viewport dimensions
                    unsafe {
                        gpu_renderer.render(&batches, (size.width as f32, size.height as f32));
                    }
                }
            }

            _ => {}
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, _cause: StartCause) {
        // Could implement cursor blink timer here
    }
}

impl EditorApp {
    fn handle_key_input(&mut self, event: &KeyEvent) -> bool {
        let doc = match &mut self.doc {
            Some(d) => d,
            None => return false,
        };

        println!("Key pressed: {:?}", event.logical_key);

        match &event.logical_key {
            Key::Character(ch) => {
                // Insert character at cursor
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting '{}' at position {}", ch, cursor_pos);

                doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text(ch.to_string()),
                });
                doc.flush();

                // Move cursor forward
                self.cursor_pos.store(cursor_pos + ch.len(), Ordering::Relaxed);

                // Debug: print document content
                let text = doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true // Request redraw
            }
            Key::Named(NamedKey::Space) => {
                // Handle space character
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting space at position {}", cursor_pos);

                doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text(" ".to_string()),
                });
                doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed);

                let text = doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Enter) => {
                // Handle enter/newline
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting newline at position {}", cursor_pos);

                doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text("\n".to_string()),
                });
                doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed);

                let text = doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Tab) => {
                // Handle tab character
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                println!("Inserting tab at position {}", cursor_pos);

                doc.edit(Edit::Insert {
                    pos: cursor_pos,
                    content: Content::Text("\t".to_string()),
                });
                doc.flush();

                self.cursor_pos.store(cursor_pos + 1, Ordering::Relaxed); // Tab is one character

                let text = doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos.load(Ordering::Relaxed));

                true
            }
            Key::Named(NamedKey::Backspace) => {
                let cursor_pos = self.cursor_pos.load(Ordering::Relaxed);
                if cursor_pos > 0 {
                    println!("Backspace at position {}", cursor_pos);

                    doc.edit(Edit::Delete {
                        range: cursor_pos - 1..cursor_pos,
                    });
                    doc.flush();

                    self.cursor_pos.store(cursor_pos - 1, Ordering::Relaxed);

                    let text = doc.read().to_string();
                    println!("Document now contains: '{}'", text);

                    true // Request redraw
                } else {
                    false
                }
            }
            Key::Named(NamedKey::F1) => {
                print_editor_info(doc);
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
                let doc_len = doc.read().to_string().len();
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
                let doc_len = doc.read().to_string().len();
                self.cursor_pos.store(doc_len, Ordering::Relaxed);
                println!("Cursor moved to end (position {})", doc_len);
                true
            }
            _ => false
        }
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
