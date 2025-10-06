//! Cursor Plugin - Blinking text cursor with customizable appearance

use ahash::AHasher;
use serde::Deserialize;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tiny_sdk::bytemuck;
use tiny_sdk::bytemuck::{Pod, Zeroable};
use tiny_sdk::wgpu;
use tiny_sdk::{
    ffi::{
        BindGroupId, BindGroupLayoutId, BufferId, PipelineId, ShaderModuleId,
        VertexAttributeDescriptor, VertexFormat,
    },
    CachedBuffer, Capability, Configurable, Initializable, LayoutPos, Library, PaintContext,
    Paintable, Plugin, PluginError, SetupContext, Updatable, UpdateContext, ViewportInfo,
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
            blink_rate: 1.0,
            solid_duration_ms: 500,
            style: CursorStyle {
                color: 0xE1E1E1FF,
                width: 2.0,
                height_scale: 1.0,
                x_offset: 0.0,
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

    // Viewport info for proper positioning
    viewport: ViewportInfo,

    // GPU resources (created during setup)
    vertex_buffer: Option<CachedBuffer>,
    uniform_buffer: Option<BufferId>,
    uniform_bind_group: Option<BindGroupId>,
    custom_pipeline_id: Option<PipelineId>,

    // Uniform cache to avoid redundant writes
    last_visibility: std::sync::atomic::AtomicBool,
    last_viewport_size: std::sync::atomic::AtomicU64, // packed width|height as u32s
    last_color: std::sync::atomic::AtomicU32,
}

impl CursorPlugin {
    /// Create a new cursor plugin with default configuration
    pub fn new() -> Self {
        use tiny_sdk::{LogicalSize, PhysicalSize};

        Self {
            config: CursorConfig::default(),
            api: CursorAPI::new(),
            blink_phase: 0.0,
            last_position: None,
            last_active_ms: AtomicU64::new(0),
            program_start: Instant::now(),
            viewport: ViewportInfo {
                scroll: LayoutPos::new(0.0, 0.0),
                logical_size: LogicalSize::new(800.0, 600.0),
                physical_size: PhysicalSize {
                    width: 800,
                    height: 600,
                },
                scale_factor: 1.0,
                line_height: 20.0,
                font_size: 14.0,
                margin: LayoutPos::new(0.0, 0.0),
                global_margin: LayoutPos::new(0.0, 0.0),
            },
            vertex_buffer: None,
            uniform_buffer: None,
            uniform_bind_group: None,
            custom_pipeline_id: None,
            last_visibility: std::sync::atomic::AtomicBool::new(false),
            last_viewport_size: std::sync::atomic::AtomicU64::new(0),
            last_color: std::sync::atomic::AtomicU32::new(0),
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
    fn create_vertices_at_position(
        &self,
        viewport: &tiny_sdk::ViewportInfo,
        position: tiny_sdk::LayoutPos,
    ) -> Vec<CursorVertex> {
        // Use viewport's line height (in logical pixels)
        let line_height = viewport.line_height * self.config.style.height_scale;

        // Position from host is in logical VIEW pixels (already accounts for scroll)
        // Shader expects physical pixels, so we need to scale
        let scale = viewport.scale_factor;

        let x = (position.x.0 + self.config.style.x_offset) * scale;
        let y = position.y.0 * scale;
        let w = self.config.style.width * scale;
        let h = line_height * scale;

        // Create two triangles for a quad (no color - it's in uniforms)
        vec![
            CursorVertex { position: [x, y] },
            CursorVertex {
                position: [x + w, y],
            },
            CursorVertex {
                position: [x, y + h],
            },
            CursorVertex {
                position: [x + w, y],
            },
            CursorVertex {
                position: [x + w, y + h],
            },
            CursorVertex {
                position: [x, y + h],
            },
        ]
    }
}

/// Vertex data for cursor rendering (position only - color is uniform)
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct CursorVertex {
    position: [f32; 2],
}

/// Cursor uniforms for shader
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
#[bytemuck(crate = "self::bytemuck")]
struct CursorUniforms {
    viewport_size: [f32; 2],
    color: u32,
    alpha: f32,
}

// === Plugin Trait Implementation ===

tiny_sdk::plugin! {
    CursorPlugin {
        name: "cursor",
        version: "0.1.0",
        z_index: 10,
        traits: [Init, Update, Paint, Library, Config],
        defaults: [],  // All custom implementations
    }
}

// === Initializable Trait Implementation ===

impl Initializable for CursorPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        // Create vertex buffer with caching built-in (6 vertices for a quad)
        let vertex_size = std::mem::size_of::<CursorVertex>();
        let buffer_size = (vertex_size * 6) as u64;
        self.vertex_buffer = Some(CachedBuffer::new(
            buffer_size,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        ));

        // Create uniform buffer and bind group
        let uniform_size = std::mem::size_of::<CursorUniforms>() as u64;
        let uniform_buffer = BufferId::create(
            uniform_size,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let uniform_layout = BindGroupLayoutId::create_uniform();
        let uniform_bind_group = BindGroupId::create_with_buffer(uniform_layout, uniform_buffer);
        self.uniform_buffer = Some(uniform_buffer);
        self.uniform_bind_group = Some(uniform_bind_group);

        // Create pipeline
        let shader_source = include_str!("shader.wgsl");
        let shader = ShaderModuleId::create_from_wgsl(shader_source);
        let pipeline_layout = BindGroupLayoutId::create_uniform();
        self.custom_pipeline_id = Some(PipelineId::create_with_layout(
            shader,
            shader,
            pipeline_layout,
            8, // stride
            &[VertexAttributeDescriptor {
                offset: 0,
                location: 0,
                format: VertexFormat::Float32x2,
            }],
        ));

        eprintln!("Cursor plugin setup complete");

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
                if args.len() == std::mem::size_of::<LayoutPos>() {
                    let pos: &LayoutPos = bytemuck::from_bytes(args);
                    self.set_position(pos.x.0, pos.y.0);
                    Ok(Vec::new())
                } else {
                    Err(PluginError::Other("Invalid args for set_position".into()))
                }
            }
            "set_viewport_info" => {
                if args.len() >= std::mem::size_of::<ViewportInfo>() {
                    let viewport: &ViewportInfo =
                        bytemuck::from_bytes(&args[..std::mem::size_of::<ViewportInfo>()]);
                    self.viewport = *viewport;
                    Ok(Vec::new())
                } else {
                    Err(PluginError::Other(
                        "Invalid args for set_viewport_info".into(),
                    ))
                }
            }
            _ => Err(PluginError::Other("Unknown method".into())),
        }
    }
}

// === Paint Trait Implementation ===

impl Paintable for CursorPlugin {
    fn z_index(&self) -> i32 {
        10
    }

    fn paint(&self, ctx: &PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Get cursor position from our API (in editor-local coordinates)
        let mut pos = self.api.get_position();

        // Transform to screen coordinates by adding widget viewport bounds (already includes padding)
        if let Some(ref widget_viewport) = ctx.widget_viewport {
            pos = LayoutPos::new(
                pos.x.0 + widget_viewport.bounds.x.0,
                pos.y.0 + widget_viewport.bounds.y.0,
            );
        }

        // Create cache key from geometry (not visibility - that's in uniforms)
        let cache_key = (
            pos.x.0.to_bits(),
            pos.y.0.to_bits(),
            self.config.style.width.to_bits(),
            self.config.style.height_scale.to_bits(),
            self.config.style.x_offset.to_bits(),
            ctx.viewport.scale_factor.to_bits(),
            ctx.viewport.line_height.to_bits(),
        );

        // Write vertices only if geometry changed - simplified to one line!
        if let Some(ref vertex_buffer) = self.vertex_buffer {
            let vertices = self.create_vertices_at_position(&ctx.viewport, pos);
            if vertices.is_empty() {
                return;
            }
            vertex_buffer.write_if_changed(bytemuck::cast_slice(&vertices), &cache_key);
        } else {
            eprintln!("Cursor vertex buffer not initialized");
            return;
        }

        // Update uniforms only when they change (viewport, color, visibility alpha)
        let visible = self.calculate_visibility();
        let viewport_width = ctx.viewport.physical_size.width;
        let viewport_height = ctx.viewport.physical_size.height;
        let color = self.config.style.color;

        // Pack viewport size for atomic comparison
        let packed_viewport = ((viewport_width as u64) << 32) | (viewport_height as u64);

        // Check if any uniform values changed
        let visibility_changed = self.last_visibility.load(Ordering::Relaxed) != visible;
        let viewport_changed = self.last_viewport_size.load(Ordering::Relaxed) != packed_viewport;
        let color_changed = self.last_color.load(Ordering::Relaxed) != color;

        if visibility_changed || viewport_changed || color_changed {
            let alpha = if visible { 1.0 } else { 0.0 };

            let uniforms = CursorUniforms {
                viewport_size: [viewport_width as f32, viewport_height as f32],
                color,
                alpha,
            };

            if let Some(uniform_buffer) = self.uniform_buffer {
                uniform_buffer.write(0, bytemuck::cast_slice(&[uniforms]));
            }

            // Update cached values
            self.last_visibility.store(visible, Ordering::Relaxed);
            self.last_viewport_size
                .store(packed_viewport, Ordering::Relaxed);
            self.last_color.store(color, Ordering::Relaxed);
        }

        // Always draw
        let vertex_count = 6; // 2 triangles = 6 vertices

        // Draw with our custom pipeline and bind group
        if let Some(ref vertex_buffer) = self.vertex_buffer {
            if let (Some(uniform_bind_group), Some(pipeline_id)) =
                (self.uniform_bind_group, self.custom_pipeline_id)
            {
                if let Some(ref gpu_ctx) = ctx.gpu_context {
                    // Use our custom pipeline
                    gpu_ctx.set_pipeline(render_pass, pipeline_id);
                    // Use our own uniform bind group (with color/alpha)
                    gpu_ctx.set_bind_group(render_pass, 0, uniform_bind_group);
                    // Set our vertex buffer
                    gpu_ctx.set_vertex_buffer(render_pass, 0, vertex_buffer.buffer_id());
                    // Draw!
                    gpu_ctx.draw(render_pass, vertex_count, 1);
                } else {
                    eprintln!("No GPU context - cannot use FFI draw");
                }
            }
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

        #[derive(Default, Deserialize)]
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

        fn default_blink_enabled() -> bool {
            true
        }
        fn default_blink_rate() -> f32 {
            1.0
        }
        fn default_solid_duration_ms() -> u64 {
            500
        }
        fn default_width() -> f32 {
            2.0
        }
        fn default_color() -> u32 {
            0xE1E1E1FF // RGBA format (shader expects alpha in low byte)
        }
        fn default_height_scale() -> f32 {
            1.0
        }
        fn default_x_offset() -> f32 {
            0.0
        }

        // Parse TOML value first (handles syntax errors gracefully)
        let toml_value: toml::Value = match toml::from_str(config_data) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("❌ TOML syntax error in cursor plugin.toml: {}", e);
                eprintln!("   Keeping previous configuration");
                return Ok(()); // Don't fail, just keep current config
            }
        };

        // Extract [config] section and parse fields individually
        if let Some(config_table) = toml_value.get("config").and_then(|v| v.as_table()) {
            // Parse each field with tiny_sdk::parse_fields! macro
            // Bad fields use defaults, good fields work
            let mut temp_config = PluginConfig::default();
            tiny_sdk::parse_fields!(temp_config, config_table, {
                blink_enabled: default_blink_enabled(),
                blink_rate: default_blink_rate(),
                solid_duration_ms: default_solid_duration_ms(),
                width: default_width(),
                color: default_color(),
                height_scale: default_height_scale(),
                x_offset: default_x_offset(),
            });

            // Apply parsed values
            self.config.blink_enabled = temp_config.blink_enabled;
            self.config.blink_rate = temp_config.blink_rate;
            self.config.solid_duration_ms = temp_config.solid_duration_ms;
            self.config.style.width = temp_config.width;
            self.config.style.color = temp_config.color;
            self.config.style.height_scale = temp_config.height_scale;
            self.config.style.x_offset = temp_config.x_offset;

            eprintln!(
                "Cursor: plugin config updated: width={}, color={:#010x}, blink_rate={}",
                self.config.style.width, self.config.style.color, self.config.blink_rate
            );

            // Reset blink phase when config changes
            self.blink_phase = 0.0;
            self.last_active_ms.store(0, Ordering::Relaxed);
        }

        Ok(())
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
