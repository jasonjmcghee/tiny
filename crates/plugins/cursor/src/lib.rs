//! Cursor Plugin - Blinking text cursor with customizable appearance

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use serde::Deserialize;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::wgpu::Buffer;
use tiny_sdk::{
    Capability, Configurable, LayoutPos, PaintContext, Paintable, Plugin, PluginError,
    Updatable, UpdateContext, Library, Initializable, SetupContext,
};

/// API exposed by cursor plugin
pub struct CursorAPI {
    position: LayoutPos,
}

impl CursorAPI {
    pub fn new() -> Self {
        Self {
            position: LayoutPos::new(0.0, 0.0),
        }
    }

    pub fn set_position(&mut self, pos: LayoutPos) {
        self.position = pos;
    }

    pub fn get_position(&self) -> LayoutPos {
        self.position
    }
}

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

    // API for external access
    api: CursorAPI,

    // Current state
    blink_phase: f32,

    // Activity tracking for smart blinking
    last_position: Option<LayoutPos>,
    last_active_ms: AtomicU64,
    program_start: Instant,

    // GPU resources (created during setup)
    vertex_buffer: Option<Buffer>,
    vertex_buffer_id: Option<tiny_sdk::ffi::BufferId>,
    device: Option<std::sync::Arc<wgpu::Device>>,
    queue: Option<std::sync::Arc<wgpu::Queue>>,
}

impl CursorPlugin {
    /// Create a new cursor plugin with default configuration
    pub fn new() -> Self {
        Self {
            config: CursorConfig::default(),
            api: CursorAPI::new(),
            blink_phase: 0.0,
            last_position: None,
            last_active_ms: AtomicU64::new(0),
            program_start: Instant::now(),
            vertex_buffer: None,
            vertex_buffer_id: None,
            device: None,
            queue: None,
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

        self.api.set_position(new_pos);
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

    /// Create vertex data for cursor rectangle at a specific position
    fn create_vertices_at_position(&self, viewport: &tiny_sdk::ViewportInfo, position: tiny_sdk::LayoutPos) -> Vec<CursorVertex> {
        let visible = self.calculate_visibility();
        let color = if visible {
            self.config.style.color
        } else {
            0x00000000
        };

        // Use viewport's line height (in logical pixels)
        let line_height = viewport.line_height * self.config.style.height_scale;

        // Position from host is in logical VIEW pixels (already accounts for scroll)
        // Shader expects physical pixels, so we need to scale
        let scale = viewport.scale_factor;

        let x = (position.x.0 + self.config.style.x_offset) * scale;
        let y = position.y.0 * scale;
        let w = self.config.style.width * scale;
        let h = line_height * scale;

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
            Capability::Initializable,
            Capability::Updatable,
            Capability::Paintable("cursor".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_updatable(&mut self) -> Option<&mut dyn Updatable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }

    fn as_library_mut(&mut self) -> Option<&mut dyn Library> {
        Some(self)
    }

    fn as_configurable(&mut self) -> Option<&mut dyn Configurable> {
        Some(self)
    }
}

// === Initializable Trait Implementation ===

impl Initializable for CursorPlugin {
    fn setup(&mut self, ctx: &mut SetupContext) -> Result<(), PluginError> {
        // eprintln!("CursorPlugin::setup called");

        // Store device and queue for later use
        self.device = Some(ctx.device.clone());
        self.queue = Some(ctx.queue.clone());

        // Create vertex buffer with reasonable initial size (6 vertices for a quad)
        let vertex_size = std::mem::size_of::<CursorVertex>();
        let buffer_size = (vertex_size * 6) as u64;

        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Cursor Plugin Vertex Buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        self.vertex_buffer = Some(vertex_buffer);

        // Also create an FFI buffer ID for reuse
        use tiny_sdk::ffi::BufferId;
        let buffer_id = BufferId::create(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST
        );
        self.vertex_buffer_id = Some(buffer_id);

        Ok(())
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

// === Library Trait Implementation ===

impl Library for CursorPlugin {
    fn name(&self) -> &str {
        "cursor_api"
    }

    fn call(&mut self, method: &str, args: &[u8]) -> Result<Vec<u8>, PluginError> {
        match method {
            "set_position" => {
                if args.len() == 8 {
                    let x = f32::from_le_bytes([args[0], args[1], args[2], args[3]]);
                    let y = f32::from_le_bytes([args[4], args[5], args[6], args[7]]);
                    // Call the plugin's set_position method to update activity
                    self.set_position(x, y);
                    Ok(Vec::new())
                } else {
                    Err(PluginError::Other("Invalid args for set_position".into()))
                }
            }
            _ => Err(PluginError::Other("Unknown method".into()))
        }
    }
}

// === Paint Trait Implementation ===

impl Paintable for CursorPlugin {
    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Get cursor position from our API
        let pos = self.api.get_position();

        // Create vertices for current frame
        let vertices = self.create_vertices_at_position(&ctx.viewport, pos);
        if vertices.is_empty() {
            return;
        }

        // eprintln!("Rendering {} vertices with color {:#010x}", vertices.len(), vertices[0].color);

        let vertex_data = bytemuck::cast_slice(&vertices);
        let vertex_count = vertices.len() as u32;

        // Reuse the existing FFI buffer created during setup
        if let Some(buffer_id) = self.vertex_buffer_id {
            // Write updated vertex data to the existing buffer
            buffer_id.write(0, vertex_data);
            // eprintln!("Updated reusable buffer via FFI");

            // Draw using the host's rect pipeline via FFI
            if let Some(ref gpu_ctx) = ctx.gpu_context {
                gpu_ctx.draw_vertices(render_pass, buffer_id, vertex_count);
                // eprintln!("Drew vertices via FFI");
            } else {
                eprintln!("No GPU context - cannot use FFI draw");
            }
        } else {
            eprintln!("No FFI buffer ID available - cursor not properly initialized");
        }
    }
}

// === Configurable Trait Implementation ===

impl Configurable for CursorPlugin {
    fn config_updated(&mut self, config_data: &str) -> Result<(), PluginError> {
        // Parse the full plugin.toml structure
        #[derive(Deserialize)]
        struct PluginToml {
            config: PluginConfig,
        }

        #[derive(Deserialize)]
        struct PluginConfig {
            #[serde(default = "default_blink_enabled")]
            blink_enabled: bool,
            #[serde(default = "default_blink_rate")]
            blink_rate: f32,
            #[serde(default = "default_solid_duration_ms")]
            solid_duration_ms: u64,
            #[serde(default = "default_width")]
            width: f32,
            #[serde(default = "default_color")]
            color: u32,
            #[serde(default = "default_height_scale")]
            height_scale: f32,
            #[serde(default = "default_x_offset")]
            x_offset: f32,
        }

        fn default_blink_enabled() -> bool { true }
        fn default_blink_rate() -> f32 { 2.0 }
        fn default_solid_duration_ms() -> u64 { 500 }
        fn default_width() -> f32 { 2.0 }
        fn default_color() -> u32 { 0xFFFFFFFF }
        fn default_height_scale() -> f32 { 1.0 }
        fn default_x_offset() -> f32 { -2.0 }

        match toml::from_str::<PluginToml>(config_data) {
            Ok(plugin_toml) => {
                // Update our config from the parsed values
                self.config.blink_enabled = plugin_toml.config.blink_enabled;
                self.config.blink_rate = plugin_toml.config.blink_rate;
                self.config.solid_duration_ms = plugin_toml.config.solid_duration_ms;
                self.config.style.width = plugin_toml.config.width;
                self.config.style.color = plugin_toml.config.color;
                self.config.style.height_scale = plugin_toml.config.height_scale;
                self.config.style.x_offset = plugin_toml.config.x_offset;

                eprintln!("Cursor plugin config updated: width={}, color={:#010x}, blink_rate={}",
                         self.config.style.width, self.config.style.color, self.config.blink_rate);

                // Reset blink phase when config changes
                self.blink_phase = 0.0;
                self.last_active_ms.store(0, Ordering::Relaxed);

                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to parse cursor config: {}", e);
                Err(PluginError::Other(format!("Config parse error: {}", e).into()))
            }
        }
    }

    fn get_config(&self) -> Option<String> {
        // Convert current config back to TOML
        format!("[config]\nblink_enabled = {}\nblink_rate = {}\nsolid_duration_ms = {}\nwidth = {}\ncolor = {:#010x}\nheight_scale = {}\nx_offset = {}",
                self.config.blink_enabled,
                self.config.blink_rate,
                self.config.solid_duration_ms,
                self.config.style.width,
                self.config.style.color,
                self.config.style.height_scale,
                self.config.style.x_offset).into()
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
        self.api.get_position()
    }

    /// Check if cursor is currently visible
    pub fn is_visible(&self) -> bool {
        self.calculate_visibility()
    }
}
