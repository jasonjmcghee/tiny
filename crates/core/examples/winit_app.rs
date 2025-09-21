//! Core runtime - event loop and plugin orchestration
//!
//! This is the thin orchestration layer that makes everything run.
//! < 500 LOC target for maximum simplicity.

use std::sync::Arc;
use std::time::Instant;
use tiny_sdk::{Plugin, PluginError};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

mod gpu;
mod orchestrator;

use orchestrator::PluginOrchestrator;

/// The main application that runs the plugin system
pub struct TinyCore {
    /// Window handle
    window: Option<Arc<Window>>,
    /// GPU surface
    surface: Option<wgpu::Surface<'static>>,
    /// Surface configuration
    config: Option<wgpu::SurfaceConfiguration>,
    /// Plugin orchestrator
    orchestrator: Option<PluginOrchestrator>,
    /// Frame timing
    last_frame: Instant,
    frame_count: u64,
    elapsed_time: f32,
}

impl TinyCore {
    /// Create new core runtime
    pub fn new() -> Self {
        Self {
            window: None,
            surface: None,
            config: None,
            orchestrator: None,
            last_frame: Instant::now(),
            frame_count: 0,
            elapsed_time: 0.0,
        }
    }

    /// Register a plugin with the system
    pub fn register_plugin(&mut self, plugin: Box<dyn Plugin>) -> Result<(), PluginError> {
        if let Some(orchestrator) = &mut self.orchestrator {
            orchestrator.register_plugin(plugin)
        } else {
            Err(PluginError::InitializeFailed(
                "Core not initialized yet".to_string(),
            ))
        }
    }

    /// Run the application
    pub fn run(mut self) -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        event_loop.run_app(&mut self)?;
        Ok(())
    }

    /// Render one frame
    fn render_frame(&mut self) {
        let Some(orchestrator) = &mut self.orchestrator else {
            return;
        };
        let Some(surface) = &mut self.surface else {
            return;
        };
        let Some(config) = &self.config else {
            return;
        };

        // Calculate frame timing
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.elapsed_time += dt;
        self.frame_count += 1;

        // Update plugins
        if let Err(e) = orchestrator.update(dt, self.elapsed_time, self.frame_count) {
            eprintln!("Plugin update error: {}", e);
        }

        // Get surface texture
        let Ok(output) = surface.get_current_texture() else {
            eprintln!("Failed to get surface texture");
            return;
        };

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Create command encoder
        let device = &orchestrator.device;
        let queue = &orchestrator.queue;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Core Render Encoder"),
        });

        // Begin render pass
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Core Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.02,
                            g: 0.02,
                            b: 0.04,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });

            // Let plugins paint
            let scale_factor = self
                .window
                .as_ref()
                .map(|w| w.scale_factor())
                .unwrap_or(1.0) as f32;
            let viewport_size = (config.width as f32, config.height as f32);
            orchestrator.paint(&mut render_pass, viewport_size, scale_factor);
        }

        // Submit commands
        queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

impl ApplicationHandler for TinyCore {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            // Create window
            let window = Arc::new(
                event_loop
                    .create_window(Window::default_attributes().with_title("Tiny Core"))
                    .expect("Failed to create window"),
            );

            // Initialize GPU
            let window_clone = window.clone();
            let (device, queue, surface) =
                unsafe { pollster::block_on(gpu::init_gpu(window_clone)) };

            // Configure surface
            let size = window.inner_size();
            let config = gpu::configure_surface(&surface, &device, size.width, size.height);

            // Create orchestrator
            let orchestrator = PluginOrchestrator::new(device, queue);

            // Store everything
            self.window = Some(window);
            self.surface = Some(surface);
            self.config = Some(config);
            self.orchestrator = Some(orchestrator);

            // TODO: Load plugins here
            // This is where we'd load plugins from plugins.toml
            // For now, they'll be registered manually

            // Request initial redraw
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

            WindowEvent::Resized(size) => {
                if let (Some(surface), Some(config), Some(orchestrator)) =
                    (&self.surface, &mut self.config, &self.orchestrator)
                {
                    config.width = size.width;
                    config.height = size.height;
                    surface.configure(&orchestrator.device, config);
                    self.render_frame();
                }
            }

            WindowEvent::RedrawRequested => {
                self.render_frame();

                // Request next frame for continuous rendering
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                // TODO: Forward to plugins
                if event.state == ElementState::Pressed {
                    println!("Key pressed: {:?}", event.logical_key);
                }
            }

            _ => {}
        }
    }
}

/// Builder pattern for creating a configured TinyCore
pub struct TinyCoreBuilder {
    plugins: Vec<Box<dyn Plugin>>,
}

impl TinyCoreBuilder {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    pub fn with_plugin(mut self, plugin: Box<dyn Plugin>) -> Self {
        self.plugins.push(plugin);
        self
    }

    pub fn build(self) -> TinyCore {
        let mut core = TinyCore::new();
        // Plugins will be registered after window creation
        // Store them for later registration
        core
    }
}
