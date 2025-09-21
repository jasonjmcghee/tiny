//! Cursor Plugin - Blinking text cursor with customizable appearance

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tiny_core::GpuRenderer;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::{BindGroup, BindGroupLayout, Buffer, RenderPipeline};
use tiny_sdk::{
    Capability, LayoutPos, PaintContext, Paintable, Plugin, PluginError, Updatable, UpdateContext,
};

/// Cursor appearance configuration
#[derive(Debug, Clone)]
pub struct CursorStyle {
    pub color: u32,
    pub width: f32,
    pub height_scale: f32,
    pub x_offset: f32,
}

/// Configuration loaded from plugin.toml
#[derive(Debug, Clone)]
pub struct CursorConfig {
    pub blink_enabled: bool,
    pub blink_rate: f32,
    pub solid_duration_ms: u64,
    pub style: CursorStyle,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            blink_enabled: true,
            blink_rate: 2.0,
            solid_duration_ms: 500,
            style: CursorStyle {
                color: 0xFFFFFFFF,
                width: 2.0,
                height_scale: 1.0,
                x_offset: -2.0,
            },
        }
    }
}

/// Main cursor plugin struct
pub struct CursorPlugin {
    // Configuration
    config: CursorConfig,

    // Current state
    position: LayoutPos,
    blink_phase: f32,

    // Activity tracking for smart blinking
    last_position: Option<LayoutPos>,
    last_active_ms: AtomicU64,
    program_start: Instant,

    // GPU resources (created during setup if needed)
    vertex_buffer: Option<Buffer>,
    pipeline: Option<RenderPipeline>,
    bind_group_layout: Option<BindGroupLayout>,
    bind_group: Option<BindGroup>,
}

impl CursorPlugin {
    /// Create a new cursor plugin with default configuration
    pub fn new() -> Self {
        Self {
            config: CursorConfig::default(),
            position: LayoutPos::new(0.0, 0.0),
            blink_phase: 0.0,
            last_position: None,
            last_active_ms: AtomicU64::new(0),
            program_start: Instant::now(),
            vertex_buffer: None,
            pipeline: None,
            bind_group_layout: None,
            bind_group: None,
        }
    }

    /// Update cursor position
    pub fn set_position(&mut self, x: f32, y: f32) {
        let new_pos = LayoutPos::new(x, y);

        // Check if cursor moved
        if self
            .last_position
            .map_or(true, |p| p.x.0 != new_pos.x.0 || p.y.0 != new_pos.y.0)
        {
            self.last_position = Some(new_pos);
            // Update last activity time
            let now_ms = self.program_start.elapsed().as_millis() as u64;
            self.last_active_ms.store(now_ms, Ordering::Relaxed);
        }

        self.position = new_pos;
    }

    /// Calculate current cursor visibility based on blink state
    fn calculate_visibility(&self) -> bool {
        if !self.config.blink_enabled {
            return true;
        }

        let now_ms = self.program_start.elapsed().as_millis() as u64;
        let last_active = self.last_active_ms.load(Ordering::Relaxed);
        let ms_since_activity = now_ms.saturating_sub(last_active);

        if ms_since_activity < self.config.solid_duration_ms {
            // Solid cursor after activity
            true
        } else {
            // Blinking
            let blink_period_ms = (1000.0 / self.config.blink_rate) as u64;
            let blink_phase = (now_ms / (blink_period_ms / 2)) % 2;
            blink_phase == 0
        }
    }

    /// Create vertex data for cursor rectangle
    fn create_vertices(&self, viewport: &tiny_sdk::ViewportInfo) -> Vec<CursorVertex> {
        let visible = self.calculate_visibility();
        let color = if visible {
            self.config.style.color
        } else {
            0x00000000
        };

        // Use viewport's line height
        let line_height = viewport.line_height * self.config.style.height_scale;

        // Apply position offset
        let x = self.position.x.0 + self.config.style.x_offset;
        let y = self.position.y.0;
        let w = self.config.style.width;
        let h = line_height;

        // Create two triangles for a quad
        vec![
            CursorVertex {
                position: [x, y],
                color,
            },
            CursorVertex {
                position: [x + w, y],
                color,
            },
            CursorVertex {
                position: [x, y + h],
                color,
            },
            CursorVertex {
                position: [x + w, y],
                color,
            },
            CursorVertex {
                position: [x + w, y + h],
                color,
            },
            CursorVertex {
                position: [x, y + h],
                color,
            },
        ]
    }
}

/// Vertex data for cursor rendering
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct CursorVertex {
    position: [f32; 2],
    color: u32,
}

// === Plugin Trait Implementation ===

impl Plugin for CursorPlugin {
    fn name(&self) -> &str {
        "cursor"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Updatable,
            Capability::Paintable("cursor".to_string()),
        ]
    }
}

// === Update Trait Implementation ===

impl Updatable for CursorPlugin {
    fn update(&mut self, dt: f32, _ctx: &mut UpdateContext) -> Result<(), PluginError> {
        // Update blink animation phase
        if self.config.blink_enabled {
            self.blink_phase += dt * self.config.blink_rate * 2.0 * std::f32::consts::PI;
            if self.blink_phase > std::f32::consts::TAU {
                self.blink_phase -= std::f32::consts::TAU;
            }
        }

        Ok(())
    }
}

// === Paint Trait Implementation ===

impl Paintable for CursorPlugin {
    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Create vertices for current frame
        let vertices = self.create_vertices(&ctx.viewport);

        // Skip rendering if cursor is invisible
        if vertices.is_empty() || vertices[0].color == 0x00000000 {
            return;
        }

        // Cast gpu_renderer pointer to access the rect pipeline
        let gpu_renderer = ctx.gpu_renderer as *mut GpuRenderer;

        // Create vertex buffer for this frame
        let vertex_data = bytemuck::cast_slice(&vertices);
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Cursor Vertex Buffer"),
            size: vertex_data.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        ctx.queue.write_buffer(&vertex_buffer, 0, vertex_data);

        // Use the GPU's rect pipeline for rendering
        unsafe {
            render_pass.set_pipeline((*gpu_renderer).rect_pipeline());
            render_pass.set_bind_group(0, (*gpu_renderer).uniform_bind_group(), &[]);
        }
        render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        render_pass.draw(0..vertices.len() as u32, 0..1);
    }
}

// === Plugin Entry Point (for dynamic loading) ===

/// Create a new cursor plugin instance
/// This is the entry point for dynamic library loading
#[no_mangle]
pub extern "C" fn cursor_plugin_create() -> Box<dyn Plugin> {
    Box::new(CursorPlugin::new())
}

// === Public API for direct usage ===

impl CursorPlugin {
    /// Load configuration from plugin.toml values
    pub fn with_config(mut self, config: CursorConfig) -> Self {
        self.config = config;
        self
    }

    /// Get current cursor position
    pub fn position(&self) -> LayoutPos {
        self.position
    }

    /// Check if cursor is currently visible
    pub fn is_visible(&self) -> bool {
        self.calculate_visibility()
    }
}
