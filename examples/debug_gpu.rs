//! Debug what happens in the actual GPU rendering

use tiny_editor::{
    font::SharedFontSystem,
    render::{BatchedDraw, Renderer},
    tree::{Doc, Rect},
    gpu::GpuRenderer,
};
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

struct DebugApp {
    window: Option<Arc<Window>>,
    renderer: Option<GpuRenderer>,
    doc: Doc,
    font_system: Arc<SharedFontSystem>,
    cpu_renderer: Renderer,
}

impl Default for DebugApp {
    fn default() -> Self {
        let doc = Doc::from_str("A"); // Single character
        let font_system = Arc::new(SharedFontSystem::new());
        let mut cpu_renderer = Renderer::new(
            (800.0, 400.0), // Window size
            1.0, // Scale factor
        );
        cpu_renderer.set_font_system(font_system.clone());

        Self {
            window: None,
            renderer: None,
            doc,
            font_system,
            cpu_renderer,
        }
    }
}

impl ApplicationHandler for DebugApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            println!("🔧 Creating window for GPU debug...");

            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("GPU Debug")
                            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 400.0)),
                    )
                    .expect("Failed to create window"),
            );

            println!("🎮 Initializing GPU renderer...");
            let gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window.clone())) };

            // Upload font atlas
            self.font_system.prerasterize_ascii(14.0);
            let atlas_data = self.font_system.atlas_data();
            let (width, height) = self.font_system.atlas_size();
            gpu_renderer.upload_font_atlas(&atlas_data, width, height);

            self.window = Some(window);
            self.renderer = Some(gpu_renderer);

            println!("✅ Setup complete - now rendering...");

            // Trigger immediate render
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
                event_loop.exit();
            }

            WindowEvent::RedrawRequested => {
                println!("\n🖼️  REDRAW REQUESTED");

                if let (Some(gpu_renderer), Some(window)) = (&mut self.renderer, &self.window) {
                    let tree = self.doc.read();
                    let viewport = Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 800.0,
                        height: 400.0,
                    };

                    println!("📊 Generating render batches...");
                    let batches = self.cpu_renderer.render(&tree, viewport);

                    println!("📈 Generated {} batches", batches.len());
                    for (i, batch) in batches.iter().enumerate() {
                        match batch {
                            BatchedDraw::GlyphBatch { instances, .. } => {
                                println!("  Batch {}: {} glyphs", i, instances.len());
                                for (j, glyph) in instances.iter().enumerate() {
                                    println!("    GPU Glyph {}: pos=({:.1}, {:.1}) tex=[{:.3}, {:.3}, {:.3}, {:.3}] color=0x{:08X}",
                                             j, glyph.x, glyph.y,
                                             glyph.tex_coords[0], glyph.tex_coords[1],
                                             glyph.tex_coords[2], glyph.tex_coords[3],
                                             glyph.color);
                                }
                            }
                            _ => println!("  Batch {}: Non-glyph", i),
                        }
                    }

                    println!("🚀 Sending to GPU...");
                    unsafe {
                        gpu_renderer.render(&batches, (800.0, 400.0));
                    }
                    println!("✅ GPU render complete");

                    // Schedule another redraw to keep testing
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let winit::keyboard::Key::Character(ch) = event.logical_key.as_ref() {
                        println!("\n⌨️  Key pressed: '{}'", ch);

                        // Add character to document
                        let current_len = self.doc.read().to_string().len();
                        self.doc.edit(tiny_editor::tree::Edit::Insert {
                            pos: current_len,
                            content: tiny_editor::tree::Content::Text(ch.to_string()),
                        });
                        self.doc.flush();

                        println!("📄 Document now: '{}'", self.doc.read().to_string());

                        if let Some(window) = &self.window {
                            window.request_redraw();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 GPU Pipeline Debug");
    println!("====================");
    println!("This will show exactly what the GPU receives and renders.");
    println!("Press keys to type and watch the GPU debug output.\n");

    let event_loop = EventLoop::new()?;
    let mut app = DebugApp::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}