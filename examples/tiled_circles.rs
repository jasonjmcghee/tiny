//! Tiled Circle Trackers - Prototype for next-generation widget system
//!
//! Two side-by-side widgets that track mouse position with circles.
//! Demonstrates widget composition, custom rendering, and event handling.

use std::sync::Arc;
use std::rc::Rc;
use std::cell::RefCell;
use std::ops::Range;
use tiny_editor::{
    coordinates::{LayoutPos, LayoutRect, LogicalPixels, LogicalSize, Viewport},
    font::SharedFontSystem,
    gpu::GpuRenderer,
    text_effects::{TextEffect, EffectType, TextStyleProvider, priority},
    tree::Doc,
    render::{Renderer, BatchedDraw, GlyphInstance, RectInstance},
    input::{InputHandler},
    widget::{Widget, WidgetId, WidgetEvent, EventResponse, LayoutConstraints, LayoutResult},
};
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting Tiled Circle Trackers...");

    let event_loop = EventLoop::new()?;
    let mut app = CircleApp::default();
    event_loop.run_app(&mut app)?;

    Ok(())
}

// === Widget System Extensions ===


/// Data passed to circle SDF shader
#[repr(C)]
#[derive(Clone, Copy)]
struct CircleShaderData {
    center: [f32; 2],
    radius: f32,
    color: u32,
}


// === Circle Tracker Widget ===

/// Widget that displays a circle following the mouse cursor
struct CircleTracker {
    id: WidgetId,
    bounds: LayoutRect,
    mouse_pos: Option<LayoutPos>,
    is_hovered: bool,
    circle_color: u32,
    render_priority: i32, // Render priority for layering

    // Custom rendering resources (created lazily, single-threaded)
    resources: Rc<RefCell<CircleResources>>,
}

struct CircleResources {
    circle_pipeline: Option<Arc<wgpu::RenderPipeline>>,
    vertex_buffer: Option<Arc<wgpu::Buffer>>,
    circle_uniform_buffer: Option<Arc<wgpu::Buffer>>,
    circle_bind_group: Option<Arc<wgpu::BindGroup>>,
    background_vertex_buffer: Option<Arc<wgpu::Buffer>>,
}

// SAFETY: CircleTracker is only used on the main thread during rendering
// RefCell ensures runtime borrow checking for safe mutation
unsafe impl Send for CircleTracker {}
unsafe impl Sync for CircleTracker {}

impl CircleTracker {
    fn new(id: WidgetId, bounds: LayoutRect, color: u32) -> Self {
        Self {
            id,
            bounds,
            mouse_pos: None,
            is_hovered: false,
            circle_color: color,
            render_priority: 0, // Default priority
            resources: Rc::new(RefCell::new(CircleResources {
                circle_pipeline: None,
                vertex_buffer: None,
                circle_uniform_buffer: None,
                circle_bind_group: None,
                background_vertex_buffer: None,
            })),
        }
    }

    fn with_priority(mut self, priority: i32) -> Self {
        self.render_priority = priority;
        self
    }

    /// Create SDF circle pipeline and resources (called lazily)
    fn ensure_circle_pipeline(&self, ctx: &tiny_editor::widget::PaintContext<'_>) {
        let mut resources = self.resources.borrow_mut();
        if resources.circle_pipeline.is_some() {
            return; // Already created
        }

        println!("Creating circle SDF pipeline...");

        // Inline SDF circle shader with uniforms
        let shader_source = r#"
struct Uniforms {
    viewport_size: vec2<f32>,
    _padding: vec2<f32>,
}

struct CircleUniforms {
    center: vec2<f32>,
    radius: f32,
    color: u32,
}

struct VertexInput {
    @location(0) position: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec2<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var<uniform> circle_data: CircleUniforms;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var out: VertexOutput;

    // Convert to clip coordinates
    let clip_x = (input.position.x / uniforms.viewport_size.x) * 2.0 - 1.0;
    let clip_y = -((input.position.y / uniforms.viewport_size.y) * 2.0 - 1.0);

    out.clip_position = vec4<f32>(clip_x, clip_y, 0.0, 1.0);
    out.world_pos = input.position;

    return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    // Calculate distance from this fragment to the circle center
    let dist = length(input.world_pos - circle_data.center);

    // SDF circle
    let sdf = dist - circle_data.radius;
    let alpha = 1.0 - smoothstep(-1.0, 1.0, sdf);

    // Unpack color
    let r = f32((circle_data.color >> 16u) & 0xFFu) / 255.0;
    let g = f32((circle_data.color >> 8u) & 0xFFu) / 255.0;
    let b = f32(circle_data.color & 0xFFu) / 255.0;
    let a = f32((circle_data.color >> 24u) & 0xFFu) / 255.0;

    return vec4<f32>(r, g, b, a * alpha);
}
"#;

        let shader = ctx.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Circle SDF Shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source.into()),
        });

        // Create circle uniform buffer
        let circle_uniform_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Circle Uniform Buffer"),
            size: 16, // vec2<f32> + f32 + u32 = 16 bytes
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Create bind group layout for circle data
        let circle_bind_group_layout = ctx.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Circle Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Create circle bind group
        let circle_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Circle Bind Group"),
            layout: &circle_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: circle_uniform_buffer.as_entire_binding(),
            }],
        });

        // Get viewport bind group layout from existing uniform
        let viewport_bind_group_layout = ctx.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Viewport Bind Group Layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        // Create pipeline layout
        let pipeline_layout = ctx.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Circle Pipeline Layout"),
            bind_group_layouts: &[&viewport_bind_group_layout, &circle_bind_group_layout],
            push_constant_ranges: &[],
        });

        // Create render pipeline
        let pipeline = ctx.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Circle Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: 8, // vec2<f32> = 8 bytes
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[wgpu::VertexAttribute {
                        offset: 0,
                        shader_location: 0,
                        format: wgpu::VertexFormat::Float32x2, // position only
                    }],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Bgra8UnormSrgb, // Assume standard format
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // Create vertex buffer for quad
        let vertex_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Circle Vertex Buffer"),
            size: 32, // 4 vertices * 8 bytes each (just position)
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        resources.circle_pipeline = Some(Arc::new(pipeline));
        resources.vertex_buffer = Some(Arc::new(vertex_buffer));
        resources.circle_uniform_buffer = Some(Arc::new(circle_uniform_buffer));
        resources.circle_bind_group = Some(Arc::new(circle_bind_group));

        println!("Circle SDF pipeline created successfully!");
    }

    /// Draw widget background and border using the existing rect pipeline
    fn draw_background(&self, ctx: &tiny_editor::widget::PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        let mut resources = self.resources.borrow_mut();
        // Create background vertex buffer if needed
        if resources.background_vertex_buffer.is_none() {
            resources.background_vertex_buffer = Some(Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("Widget Background Buffer"),
                size: 360, // 30 vertices * 12 bytes each (plenty for background + borders)
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })));
        }
        let scale = ctx.viewport.scale_factor;
        let bg_color = if self.is_hovered { 0xFF333333 } else { 0xFF222222 };
        let border_color = if self.is_hovered { 0xFF666666 } else { 0xFF444444 };
        let border_width = 2.0;

        // Batch all rectangles (background + 4 borders) into one draw call
        let mut vertices = Vec::new();

        // Background
        self.add_rect_vertices(&mut vertices, self.bounds, bg_color, scale);

        // Top border
        self.add_rect_vertices(&mut vertices,
            LayoutRect::new(self.bounds.x.0, self.bounds.y.0, self.bounds.width.0, border_width),
            border_color, scale);

        // Bottom border
        self.add_rect_vertices(&mut vertices,
            LayoutRect::new(self.bounds.x.0, self.bounds.y.0 + self.bounds.height.0 - border_width,
                           self.bounds.width.0, border_width),
            border_color, scale);

        // Left border
        self.add_rect_vertices(&mut vertices,
            LayoutRect::new(self.bounds.x.0, self.bounds.y.0, border_width, self.bounds.height.0),
            border_color, scale);

        // Right border
        self.add_rect_vertices(&mut vertices,
            LayoutRect::new(self.bounds.x.0 + self.bounds.width.0 - border_width, self.bounds.y.0,
                           border_width, self.bounds.height.0),
            border_color, scale);

        // Upload and draw all rectangles in one call using our own buffer
        if !vertices.is_empty() {
            if let Some(bg_buffer) = &resources.background_vertex_buffer {
                ctx.queue.write_buffer(bg_buffer, 0, bytemuck::cast_slice(&vertices));
                render_pass.set_pipeline(ctx.gpu_renderer.rect_pipeline());
                render_pass.set_bind_group(0, ctx.uniform_bind_group, &[]);
                render_pass.set_vertex_buffer(0, bg_buffer.slice(..));
                render_pass.draw(0..vertices.len() as u32, 0..1);
            }
        }
    }

    /// Helper to add rectangle vertices to batch
    fn add_rect_vertices(&self, vertices: &mut Vec<tiny_editor::gpu::RectVertex>,
                        rect: LayoutRect, color: u32, scale: f32) {
        let x1 = rect.x.0 * scale;
        let y1 = rect.y.0 * scale;
        let x2 = (rect.x.0 + rect.width.0) * scale;
        let y2 = (rect.y.0 + rect.height.0) * scale;

        // Two triangles for rectangle
        vertices.extend_from_slice(&[
            tiny_editor::gpu::RectVertex { position: [x1, y1], color },
            tiny_editor::gpu::RectVertex { position: [x2, y1], color },
            tiny_editor::gpu::RectVertex { position: [x1, y2], color },
            tiny_editor::gpu::RectVertex { position: [x2, y1], color },
            tiny_editor::gpu::RectVertex { position: [x2, y2], color },
            tiny_editor::gpu::RectVertex { position: [x1, y2], color },
        ]);
    }
}

impl Widget for CircleTracker {
    fn widget_id(&self) -> WidgetId {
        self.id
    }

    fn update(&mut self, _dt: f32) -> bool {
        false // No animations for now
    }

    fn handle_event(&mut self, event: &WidgetEvent) -> EventResponse {
        match event {
            WidgetEvent::MouseMove(pos) => {
                println!("Widget bounds: ({:.1},{:.1}) {}x{}, mouse: ({:.1},{:.1}), contains: {}",
                         self.bounds.x.0, self.bounds.y.0, self.bounds.width.0, self.bounds.height.0,
                         pos.x.0, pos.y.0, self.contains_point(*pos));

                if self.contains_point(*pos) {
                    self.mouse_pos = Some(*pos);
                    if !self.is_hovered {
                        println!("Mouse entered widget");
                        self.is_hovered = true;
                    }
                    EventResponse::Redraw
                } else if self.is_hovered {
                    println!("Mouse left widget");
                    self.is_hovered = false;
                    // Keep mouse_pos - don't clear it! Circle should stay at last position
                    EventResponse::Redraw
                } else {
                    EventResponse::Ignored
                }
            }
            WidgetEvent::MouseEnter => {
                println!("MouseEnter event received");
                self.is_hovered = true;
                EventResponse::Redraw
            }
            WidgetEvent::MouseLeave => {
                println!("MouseLeave event received");
                self.is_hovered = false;
                // Keep mouse_pos - circle should persist at last position
                EventResponse::Redraw
            }
            WidgetEvent::MouseClick(pos, _button) => {
                if self.contains_point(*pos) {
                    println!("Circle clicked at ({:.1}, {:.1})", pos.x.0, pos.y.0);
                    EventResponse::Handled
                } else {
                    EventResponse::Ignored
                }
            },
            &WidgetEvent::KeyboardInput(_, _) => {
                EventResponse::Ignored
            }
        }
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize {
                width: self.bounds.width,
                height: self.bounds.height,
            },
        }
    }

    fn paint(&self, ctx: &tiny_editor::widget::PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        // Draw widget background using existing rect pipeline first
        self.draw_background(ctx, render_pass);

        // Then draw custom SDF circle
        self.ensure_circle_pipeline(ctx);

        // Draw SDF circle directly to GPU
        if let Some(mouse_pos) = self.mouse_pos {
            println!("Drawing circle at ({:.1}, {:.1})", mouse_pos.x.0, mouse_pos.y.0);
            let radius = 20.0;

            // Create quad vertices around mouse position (convert to physical coordinates)
            let scale = ctx.viewport.scale_factor;
            let size = radius * 2.0 + 4.0; // Padding for anti-aliasing
            let extent = size / 2.0;
            let x1 = (mouse_pos.x.0 - extent) * scale;
            let y1 = (mouse_pos.y.0 - extent) * scale;
            let x2 = (mouse_pos.x.0 + extent) * scale;
            let y2 = (mouse_pos.y.0 + extent) * scale;

            // Convert mouse position to physical coordinates for shader
            let mouse_center_x = mouse_pos.x.0 * scale;
            let mouse_center_y = mouse_pos.y.0 * scale;
            let radius_physical = radius * scale;

            // Create vertex data: just positions for triangle strip
            let vertices: [f32; 8] = [
                x1, y1, // Bottom-left
                x2, y1, // Bottom-right
                x1, y2, // Top-left
                x2, y2, // Top-right
            ];

            // Update circle uniform data
            let circle_uniform_data: [f32; 4] = [
                mouse_center_x,
                mouse_center_y,
                radius_physical,
                f32::from_bits(self.circle_color),
            ];

            // Upload vertices and uniforms
            let resources = self.resources.borrow();
            if let (Some(vertex_buffer), Some(uniform_buffer), Some(_bind_group)) =
                (&resources.vertex_buffer, &resources.circle_uniform_buffer, &resources.circle_bind_group) {

                ctx.queue.write_buffer(vertex_buffer, 0, bytemuck::cast_slice(&vertices));
                ctx.queue.write_buffer(uniform_buffer, 0, bytemuck::cast_slice(&circle_uniform_data));
            }

            // Render with our custom pipeline
            if let (Some(pipeline), Some(vertex_buffer), Some(circle_bind_group)) =
                (&resources.circle_pipeline, &resources.vertex_buffer, &resources.circle_bind_group) {

                println!("Rendering circle with custom pipeline");
                render_pass.set_pipeline(pipeline);
                render_pass.set_bind_group(0, ctx.uniform_bind_group, &[]); // Viewport uniforms
                render_pass.set_bind_group(1, circle_bind_group.as_ref(), &[]); // Circle uniforms
                render_pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                render_pass.draw(0..4, 0..1); // Triangle strip
                println!("Circle draw call completed");
            } else {
                println!("Circle pipeline not ready!");
            }
        }
    }

    fn bounds(&self) -> LayoutRect {
        self.bounds
    }

    fn priority(&self) -> i32 {
        self.render_priority
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        // Manual clone for CircleTracker
        Arc::new(CircleTracker {
            id: self.id,
            bounds: self.bounds,
            mouse_pos: self.mouse_pos,
            is_hovered: self.is_hovered,
            circle_color: self.circle_color,
            render_priority: self.render_priority,
            resources: Rc::new(RefCell::new(CircleResources {
                circle_pipeline: None, // Reset GPU resources in clone
                vertex_buffer: None,
                circle_uniform_buffer: None,
                circle_bind_group: None,
                background_vertex_buffer: None,
            })),
        })
    }
}

// === Mouse Circle Text Effect ===

/// Text effect that colors text red within circle radius of mouse
#[derive(Clone)]
struct MouseCircleTextEffect {
    mouse_pos: Option<LayoutPos>,
    circle_radius: f32,
    viewport: Viewport,
}

impl MouseCircleTextEffect {
    fn new(viewport: Viewport) -> Self {
        Self {
            mouse_pos: None,
            circle_radius: 20.0,
            viewport,
        }
    }

    fn update_mouse_pos(&mut self, pos: Option<LayoutPos>) {
        self.mouse_pos = pos;
    }
}

// SAFETY: MouseCircleTextEffect is only used on main thread during rendering
unsafe impl Send for MouseCircleTextEffect {}
unsafe impl Sync for MouseCircleTextEffect {}

impl TextStyleProvider for MouseCircleTextEffect {
    fn get_effects_in_range(&self, range: Range<usize>) -> Vec<TextEffect> {
        println!("MouseCircleTextEffect::get_effects_in_range called for range {}..{}, mouse_pos={:?}",
                 range.start, range.end, self.mouse_pos);

        if let Some(mouse_pos) = self.mouse_pos {
            // Use shader effect with mouse position parameters
            let effect = TextEffect {
                range: range.clone(),
                effect: EffectType::Shader {
                    id: 1, // Circle SDF shader ID
                    params: Arc::new([
                        mouse_pos.x.0,          // Mouse X
                        mouse_pos.y.0,          // Mouse Y
                        self.circle_radius,     // Circle radius
                        0.0,                    // Padding
                    ]),
                },
                priority: priority::SELECTION,
            };
            println!("  Returning shader effect for mouse at ({:.1}, {:.1})", mouse_pos.x.0, mouse_pos.y.0);
            vec![effect]
        } else {
            println!("  No mouse position, returning no effects");
            vec![]
        }
    }

    fn request_update(&self, _text: &str, _version: u64) {
        // No async updates needed for mouse effects
    }

    fn name(&self) -> &str {
        "mouse_circle_effect"
    }
}

// === Document Editor Widget ===

/// Full document editor widget with text effects
struct DocumentEditorWidget {
    id: WidgetId,
    bounds: LayoutRect,
    doc: Doc,
    renderer: Rc<RefCell<Renderer>>,
    input: InputHandler,
    text_effect: MouseCircleTextEffect,
    render_priority: i32,

    // Custom glyph shader with SDF circle effects
    circle_text_pipeline: Option<Arc<wgpu::RenderPipeline>>,
    mouse_uniform_buffer: Option<Arc<wgpu::Buffer>>,
    mouse_bind_group: Option<Arc<wgpu::BindGroup>>,
}

impl DocumentEditorWidget {
    fn new(id: WidgetId, bounds: LayoutRect, text: &str, viewport: Viewport) -> Self {
        let doc = Doc::from_str(text);
        let renderer = Rc::new(RefCell::new(Renderer::new((bounds.width.0, bounds.height.0), viewport.scale_factor)));

        // Don't set text effects on renderer - we'll pass them per-frame
        let text_effect = MouseCircleTextEffect::new(viewport.clone());

        Self {
            id,
            bounds,
            doc,
            renderer,
            input: InputHandler::new(),
            text_effect,
            render_priority: 0,
            circle_text_pipeline: None,
            mouse_uniform_buffer: None,
            mouse_bind_group: None,
        }
    }

    fn with_priority(mut self, priority: i32) -> Self {
        self.render_priority = priority;
        self
    }

    /// Create custom glyph pipeline with SDF circle effects
    fn ensure_circle_text_pipeline(&mut self, ctx: &mut tiny_editor::widget::PaintContext<'_>) {
        if self.circle_text_pipeline.is_some() {
            return;
        }

        println!("Creating custom glyph shader with SDF circle effects...");

        // Custom glyph shader based on existing one but with SDF circle effect
        let shader_source = r#"
// Based on existing glyph.wgsl but with SDF circle effects

struct Uniforms {
    viewport_size: vec2<f32>,
}

struct MouseUniforms {
    mouse_pos: vec2<f32>,
    radius: f32,
    _padding: f32,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var t_glyph: texture_2d<f32>;
@group(1) @binding(1)
var s_glyph: sampler;

@group(2) @binding(0)
var<uniform> mouse_data: MouseUniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) tex_coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world_pos: vec2<f32>,
}

@vertex
fn vs_main(
    @location(0) position: vec2<f32>,
    @location(1) tex_coord: vec2<f32>,
    @location(2) color: u32,
) -> VertexOutput {
    var out: VertexOutput;

    // Same as original glyph shader
    out.clip_position = vec4<f32>(
        (position.x / (uniforms.viewport_size.x * 0.5)) - 1.0,
        1.0 - (position.y / (uniforms.viewport_size.y * 0.5)),
        0.0,
        1.0
    );

    out.tex_coord = tex_coord;
    out.world_pos = position; // Pass world position for SDF calculation

    // Convert color from packed u32 to vec4
    let r = f32((color >> 24u) & 0xFFu) / 255.0;
    let g = f32((color >> 16u) & 0xFFu) / 255.0;
    let b = f32((color >> 8u) & 0xFFu) / 255.0;
    let a = f32(color & 0xFFu) / 255.0;
    out.color = vec4<f32>(r, g, b, a);

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Sample glyph texture (alpha channel) - same as original
    let glyph_alpha = textureSample(t_glyph, s_glyph, in.tex_coord).r;

    // Calculate SDF circle effect
    let dist = length(in.world_pos - mouse_data.mouse_pos);
    let sdf = dist - mouse_data.radius;
    let circle_alpha = 1.0 - smoothstep(-1.0, 1.0, sdf);

    // Blend original text color with red circle effect
    let base_color = in.color.rgb;
    let circle_color = vec3<f32>(1.0, 0.0, 0.0); // Red
    let final_color = mix(base_color, circle_color, circle_alpha);

    return vec4<f32>(final_color, in.color.a * glyph_alpha);
}
"#;

        // Create the shader, pipeline, uniforms, etc.
        // [Implementation continues...]
        let _ = shader_source; // For now, just store the source
        println!("Circle text shader source ready!");
    }
}

// SAFETY: DocumentEditorWidget is only used on main thread during rendering
// Rc<RefCell<>> provides single-threaded interior mutability
unsafe impl Send for DocumentEditorWidget {}
unsafe impl Sync for DocumentEditorWidget {}

impl Widget for DocumentEditorWidget {
    fn widget_id(&self) -> WidgetId {
        self.id
    }

    fn update(&mut self, _dt: f32) -> bool {
        false
    }

    fn handle_event(&mut self, event: &WidgetEvent) -> EventResponse {
        match event {
            WidgetEvent::MouseMove(pos) => {
                if self.contains_point(*pos) {
                    // Update text effect with mouse position
                    self.text_effect.update_mouse_pos(Some(*pos));

                    // Request text style update to regenerate effects
                    let text = self.doc.read().to_string();
                    let version = self.doc.version();
                    self.text_effect.request_update(&text, version);

                    EventResponse::Redraw
                } else {
                    self.text_effect.update_mouse_pos(None);

                    // Request text style update to clear effects
                    let text = self.doc.read().to_string();
                    let version = self.doc.version();
                    self.text_effect.request_update(&text, version);

                    EventResponse::Redraw
                }
            }
            WidgetEvent::MouseClick(pos, button) => {
                if self.contains_point(*pos) {
                    // Handle click in document coordinates
                    self.input.on_mouse_click(&self.doc, self.renderer.borrow().viewport(), *pos, *button, false);
                    EventResponse::Redraw
                } else {
                    EventResponse::Ignored
                }
            }
            WidgetEvent::KeyboardInput(key_event, modifiers) => {
                // Handle keyboard input using the existing InputHandler
                self.input.on_key(&self.doc, self.renderer.borrow().viewport(), key_event, modifiers);
                EventResponse::Redraw
            }
            _ => EventResponse::Ignored,
        }
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        // Update renderer viewport to match widget bounds
        {
            let mut renderer = self.renderer.borrow_mut();
            let scale_factor = renderer.viewport().scale_factor;
            renderer.update_viewport(self.bounds.width.0, self.bounds.height.0, scale_factor);
        }
        LayoutResult {
            size: LogicalSize {
                width: self.bounds.width,
                height: self.bounds.height,
            },
        }
    }

    fn paint(&self, ctx: &tiny_editor::widget::PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        // Set font system and GPU renderer on embedded renderer
        {
            let mut renderer = self.renderer.borrow_mut();
            renderer.set_font_system(ctx.font_system.clone());
            renderer.set_gpu_renderer(ctx.gpu_renderer);
        }

        // Upload font atlas for this render pass
        let atlas_data = ctx.font_system.atlas_data();
        let (atlas_width, atlas_height) = ctx.font_system.atlas_size();
        ctx.gpu_renderer.upload_font_atlas(&atlas_data, atlas_width, atlas_height);

        // Create viewport rect for this widget (relative to widget bounds)
        let widget_viewport = tiny_editor::tree::Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: self.bounds.width,
            height: self.bounds.height,
        };

        // Always enable Rust syntax highlighting combined with mouse effects
        let syntax_highlighter = tiny_editor::syntax::SyntaxHighlighter::new_rust();
        let text = self.doc.read().to_string();
        syntax_highlighter.request_update(&text, self.doc.version());

        // Create a combined text style provider
        struct CombinedTextStyles {
            syntax: tiny_editor::syntax::SyntaxHighlighter,
            mouse_effect: MouseCircleTextEffect,
        }

        impl tiny_editor::text_effects::TextStyleProvider for CombinedTextStyles {
            fn get_effects_in_range(&self, range: std::ops::Range<usize>) -> Vec<tiny_editor::text_effects::TextEffect> {
                // Get syntax highlighting first
                let mut effects = self.syntax.get_effects_in_range(range.clone());
                // Then overlay mouse effects
                effects.extend(self.mouse_effect.get_effects_in_range(range));
                effects
            }

            fn request_update(&self, text: &str, version: u64) {
                self.syntax.request_update(text, version);
            }

            fn name(&self) -> &str {
                "combined_styles"
            }
        }

        let combined_styles = CombinedTextStyles {
            syntax: syntax_highlighter,
            mouse_effect: self.text_effect.clone(),
        };

        self.renderer.borrow_mut().set_text_styles(Box::new(combined_styles));

        // Use the new hybrid rendering with direct widget painting
        let selections = self.input.selections();
        let batches = self.renderer.borrow_mut().render_with_pass(&self.doc.read(), widget_viewport, selections, Some(render_pass));

        // The hybrid renderer handles widget painting directly now
        // We just need to handle our custom mouse circle shader effect
        if let Some(mouse_pos) = self.text_effect.mouse_pos {
            // Update mouse uniform data in the GPU renderer's effect buffer
            let mouse_data: [f32; 4] = [
                (mouse_pos.x.0 + self.bounds.x.0) * ctx.viewport.scale_factor, // Convert to physical + widget offset
                (mouse_pos.y.0 + self.bounds.y.0) * ctx.viewport.scale_factor,
                self.text_effect.circle_radius * ctx.viewport.scale_factor,
                0.0, // padding
            ];

            // Update the effect uniform buffer directly
            if let Some(effect_buffer) = ctx.gpu_renderer.effect_uniform_buffer(1) {
                ctx.queue.write_buffer(effect_buffer, 0, bytemuck::cast_slice(&mouse_data));
            }
        }

        println!("Document editor widget painted using hybrid renderer with {} batches!", batches.len());
    }

    fn bounds(&self) -> LayoutRect {
        self.bounds
    }

    fn priority(&self) -> i32 {
        self.render_priority
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        // For now, DocumentEditorWidget cannot be cloned due to non-cloneable fields
        // This would need refactoring for a real implementation
        panic!("DocumentEditorWidget cloning not supported yet")
    }
}


// === Tiled Layout Widget ===

/// Simple horizontal tiling layout
struct TiledLayout {
    id: WidgetId,
    bounds: LayoutRect,
    text_widget: Option<DocumentEditorWidget>, // Left widget: document editor with effects
    circle_widget: Option<CircleTracker>, // Right widget: circle tracker
}

impl TiledLayout {
    fn new(id: WidgetId, bounds: LayoutRect) -> Self {
        Self {
            id,
            bounds,
            text_widget: None,
            circle_widget: None,
        }
    }

    fn set_text_widget(&mut self, widget: DocumentEditorWidget) {
        self.text_widget = Some(widget);
    }

    fn set_circle_widget(&mut self, widget: CircleTracker) {
        self.circle_widget = Some(widget);
    }
}

// SAFETY: TiledLayout is only used on main thread during rendering
unsafe impl Send for TiledLayout {}
unsafe impl Sync for TiledLayout {}

impl Widget for TiledLayout {
    fn widget_id(&self) -> WidgetId {
        self.id
    }

    fn update(&mut self, dt: f32) -> bool {
        let mut needs_redraw = false;

        // Update text widget
        if let Some(text_widget) = &mut self.text_widget {
            if text_widget.update(dt) {
                needs_redraw = true;
            }
        }

        // Update circle widget
        if let Some(circle_widget) = &mut self.circle_widget {
            if circle_widget.update(dt) {
                needs_redraw = true;
            }
        }

        needs_redraw
    }

    fn handle_event(&mut self, event: &WidgetEvent) -> EventResponse {
        let mut handled = false;
        let mut needs_redraw = false;

        // Route to text widget
        if let Some(text_widget) = &mut self.text_widget {
            match text_widget.handle_event(event) {
                EventResponse::Handled => handled = true,
                EventResponse::Redraw => needs_redraw = true,
                EventResponse::Ignored => {}
            }
        }

        // Route to circle widget
        if let Some(circle_widget) = &mut self.circle_widget {
            match circle_widget.handle_event(event) {
                EventResponse::Handled => handled = true,
                EventResponse::Redraw => needs_redraw = true,
                EventResponse::Ignored => {}
            }
        }

        if handled {
            EventResponse::Handled
        } else if needs_redraw {
            EventResponse::Redraw
        } else {
            EventResponse::Ignored
        }
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        // Divide width in half
        let half_width = self.bounds.width.0 / 2.0;
        let widget_constraints = LayoutConstraints {
            max_width: LogicalPixels(half_width),
            max_height: self.bounds.height,
        };

        // Layout text widget on left
        if let Some(text_widget) = &mut self.text_widget {
            text_widget.bounds = LayoutRect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: LogicalPixels(half_width),
                height: self.bounds.height,
            };
            text_widget.layout(widget_constraints);
        }

        // Layout circle widget on right
        if let Some(circle_widget) = &mut self.circle_widget {
            circle_widget.bounds = LayoutRect {
                x: self.bounds.x + LogicalPixels(half_width),
                y: self.bounds.y,
                width: LogicalPixels(half_width),
                height: self.bounds.height,
            };
            circle_widget.layout(widget_constraints);
        }

        LayoutResult {
            size: LogicalSize {
                width: self.bounds.width,
                height: self.bounds.height,
            },
        }
    }

    fn paint(&self, ctx: &tiny_editor::widget::PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        // Paint widgets directly without collecting mutable references
        // Paint text widget first (lower priority)
        if let Some(text_widget) = &self.text_widget {
            if text_widget.clips_to_bounds() {
                let bounds = text_widget.bounds();
                let scale = ctx.viewport.scale_factor;
                let scissor_x = (bounds.x.0 * scale) as u32;
                let scissor_y = (bounds.y.0 * scale) as u32;
                let scissor_width = (bounds.width.0 * scale) as u32;
                let scissor_height = (bounds.height.0 * scale) as u32;
                render_pass.set_scissor_rect(scissor_x, scissor_y, scissor_width, scissor_height);
            }
            text_widget.paint(ctx, render_pass);
            if text_widget.clips_to_bounds() {
                let viewport_width = ctx.viewport.physical_size.width;
                let viewport_height = ctx.viewport.physical_size.height;
                render_pass.set_scissor_rect(0, 0, viewport_width, viewport_height);
            }
        }

        // Paint circle widget second (higher priority)
        if let Some(circle_widget) = &self.circle_widget {
            if circle_widget.clips_to_bounds() {
                let bounds = circle_widget.bounds();
                let scale = ctx.viewport.scale_factor;
                let scissor_x = (bounds.x.0 * scale) as u32;
                let scissor_y = (bounds.y.0 * scale) as u32;
                let scissor_width = (bounds.width.0 * scale) as u32;
                let scissor_height = (bounds.height.0 * scale) as u32;
                render_pass.set_scissor_rect(scissor_x, scissor_y, scissor_width, scissor_height);
            }
            circle_widget.paint(ctx, render_pass);
            if circle_widget.clips_to_bounds() {
                let viewport_width = ctx.viewport.physical_size.width;
                let viewport_height = ctx.viewport.physical_size.height;
                render_pass.set_scissor_rect(0, 0, viewport_width, viewport_height);
            }
        }

    }

    fn bounds(&self) -> LayoutRect {
        self.bounds
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        // TiledLayout cannot be cloned due to non-cloneable child widgets
        panic!("TiledLayout cloning not supported yet")
    }
}


// === Main Application ===

struct CircleApp {
    window: Option<Arc<Window>>,
    gpu_renderer: Option<GpuRenderer>,
    font_system: Option<Arc<SharedFontSystem>>,
    viewport: Option<Viewport>,

    // Widget system
    root_widget: Option<TiledLayout>,
    cursor_position: Option<winit::dpi::PhysicalPosition<f64>>,
    last_hovered_widget: Option<usize>, // Track which widget was hovered last
    modifiers: winit::event::Modifiers, // Track modifier keys

    // Custom circle rendering
    circle_pipeline: Option<wgpu::RenderPipeline>,
    circle_vertex_buffer: Option<wgpu::Buffer>,
    device: Option<Arc<wgpu::Device>>,
    queue: Option<Arc<wgpu::Queue>>,

}

impl Default for CircleApp {
    fn default() -> Self {
        Self {
            window: None,
            gpu_renderer: None,
            font_system: None,
            viewport: None,
            root_widget: None,
            cursor_position: None,
            last_hovered_widget: None,
            modifiers: winit::event::Modifiers::default(),
            circle_pipeline: None,
            circle_vertex_buffer: None,
            device: None,
            queue: None,
        }
    }
}

impl ApplicationHandler for CircleApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            println!("🪟 Creating window...");

            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("Tiled Circle Trackers - Widget Prototype")
                            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0)),
                    )
                    .expect("Failed to create window"),
            );

            // Setup GPU renderer
            println!("🎮 Initializing GPU...");
            let mut gpu_renderer = unsafe { pollster::block_on(GpuRenderer::new(window.clone())) };

            // Setup font system (needed for existing render pipeline)
            println!("🔤 Setting up fonts...");
            let font_system = Arc::new(SharedFontSystem::new());
            let scale_factor = window.scale_factor() as f32;
            font_system.prerasterize_ascii(14.0 * scale_factor);

            // Setup viewport
            let size = window.inner_size();
            let logical_width = size.width as f32 / scale_factor;
            let logical_height = size.height as f32 / scale_factor;
            let mut viewport = Viewport::new(logical_width, logical_height, scale_factor);
            viewport.set_font_system(font_system.clone());


            // Create widget layout
            let mut root_widget = TiledLayout::new(0, LayoutRect {
                x: LogicalPixels(0.0),
                y: LogicalPixels(0.0),
                width: LogicalPixels(logical_width),
                height: LogicalPixels(logical_height),
            });

            // Register circle text effect shader with GPU renderer
            let shader_source = include_str!("../src/shaders/circle_glyph.wgsl");
            gpu_renderer.register_text_effect_shader(1, shader_source, 16);

            // Add document editor widget on left with mouse effects
            root_widget.set_text_widget(DocumentEditorWidget::new(
                1, // Widget ID
                LayoutRect::new(0.0, 0.0, logical_width / 2.0, logical_height),
                r#"Hello, World!

This is a document editor widget with mouse-based text effects.

Move your mouse over this text to see the circle effect applied to the text rendering.

The text uses the existing TextEffect system with a custom TextStyleProvider that colors text based on mouse position.

You can also type to edit this text!

Line 1: The quick brown fox jumps over the lazy dog.
Line 2: ABCDEFGHIJKLMNOPQRSTUVWXYZ
Line 3: abcdefghijklmnopqrstuvwxyz
Line 4: 0123456789 !@#$%^&*()
Line 5: This demonstrates widgets can be full editors.
Line 6: Each widget has its own document and renderer.
Line 7: Text effects are applied per-widget.
Line 8: Pretty cool architecture, right?

fn main() {
    println!("Code highlighting works too!");
    let x = 42;
    let y = "Hello, world!";
}

More text to make the document longer...
Even more text...
And more...
Keep going..."#,
                viewport.clone(),
            ).with_priority(1));

            // Add circle widget on right
            root_widget.set_circle_widget(CircleTracker::new(
                2, // Widget ID
                LayoutRect::new(logical_width / 2.0, 0.0, logical_width / 2.0, logical_height),
                0xFFE24A4A, // Red
            ).with_priority(2));

            // Layout the widgets
            root_widget.layout(LayoutConstraints {
                max_width: LogicalPixels(logical_width),
                max_height: LogicalPixels(logical_height),
            });

            // Store everything
            self.window = Some(window);
            self.gpu_renderer = Some(gpu_renderer);
            self.font_system = Some(font_system);
            self.viewport = Some(viewport);
            self.root_widget = Some(root_widget);

            println!("✅ Setup complete!");

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
                    if let Some(root_widget) = &mut self.root_widget {
                        // Send keyboard event to widgets
                        let widget_event = WidgetEvent::KeyboardInput(event, self.modifiers);
                        match root_widget.handle_event(&widget_event) {
                            EventResponse::Redraw | EventResponse::Handled => {
                                if let Some(window) = &self.window {
                                    window.request_redraw();
                                }
                            }
                            EventResponse::Ignored => {}
                        }
                    }
                }
            }

            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers;
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_position = Some(position);

                if let (Some(window), Some(root_widget)) = (&self.window, &mut self.root_widget) {
                    let scale = window.scale_factor() as f32;
                    let logical_x = position.x as f32 / scale;
                    let logical_y = position.y as f32 / scale;

                    let layout_pos = LayoutPos::new(logical_x, logical_y);

                    // Send mouse move event to all widgets
                    let event = WidgetEvent::MouseMove(layout_pos);
                    match root_widget.handle_event(&event) {
                        EventResponse::Redraw | EventResponse::Handled => {
                            window.request_redraw();
                        }
                        EventResponse::Ignored => {}
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                if let (Some(window), Some(root_widget), Some(position)) =
                    (&self.window, &mut self.root_widget, self.cursor_position) {

                    let scale = window.scale_factor() as f32;
                    let logical_x = position.x as f32 / scale;
                    let logical_y = position.y as f32 / scale;

                    let layout_pos = LayoutPos::new(logical_x, logical_y);
                    let event = WidgetEvent::MouseClick(layout_pos, button);

                    match root_widget.handle_event(&event) {
                        EventResponse::Handled | EventResponse::Redraw => {
                            window.request_redraw();
                        }
                        EventResponse::Ignored => {}
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

                // Update viewport and widget bounds
                if let (Some(window), Some(viewport), Some(root_widget)) =
                    (&self.window, &mut self.viewport, &mut self.root_widget) {

                    let scale_factor = window.scale_factor() as f32;
                    let logical_width = new_size.width as f32 / scale_factor;
                    let logical_height = new_size.height as f32 / scale_factor;

                    viewport.resize(logical_width, logical_height, scale_factor);

                    root_widget.bounds = LayoutRect {
                        x: LogicalPixels(0.0),
                        y: LogicalPixels(0.0),
                        width: LogicalPixels(logical_width),
                        height: LogicalPixels(logical_height),
                    };

                    root_widget.layout(LayoutConstraints {
                        max_width: LogicalPixels(logical_width),
                        max_height: LogicalPixels(logical_height),
                    });

                    window.request_redraw();
                }
            }

            _ => {}
        }
    }
}

impl CircleApp {
    fn render_frame(&mut self) {
        if let (Some(_window), Some(gpu_renderer), Some(viewport), Some(font_system), Some(root_widget)) =
            (&self.window, &mut self.gpu_renderer, &self.viewport, &self.font_system, &mut self.root_widget) {

            // Update widgets
            root_widget.update(0.016); // Assume 60fps

            // Simple single-pass rendering
            let output = gpu_renderer.surface().get_current_texture().unwrap();
            let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = gpu_renderer.device().create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Widget Render Encoder"),
            });

            // Update viewport uniforms
            let uniforms = tiny_editor::gpu::ShaderUniforms {
                viewport_size: [viewport.physical_size.width as f32, viewport.physical_size.height as f32],
                _padding: [0.0, 0.0],
            };
            gpu_renderer.queue().write_buffer(
                gpu_renderer.uniform_buffer(),
                0,
                bytemuck::cast_slice(&[uniforms])
            );

            {
                let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Widget Render Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.05,
                                g: 0.05,
                                b: 0.08,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });

                let paint_ctx = tiny_editor::widget::PaintContext {
                    viewport,
                    device: gpu_renderer.device(),
                    queue: gpu_renderer.queue(),
                    uniform_bind_group: gpu_renderer.uniform_bind_group(),
                    gpu_renderer,
                    font_system: &font_system,
                    text_styles: None, // No global text styles, widgets provide their own
                    layout_pos: LayoutPos::new(0.0, 0.0), // Legacy field
                };

                // Let widgets paint directly to the render pass
                root_widget.paint(&paint_ctx, &mut render_pass);
            }

            gpu_renderer.queue().submit(std::iter::once(encoder.finish()));
            output.present();
        }
    }
}