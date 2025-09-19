//! Test if GPU can render a simple colored rectangle
//!
//! This eliminates font/texture complexity to test basic GPU pipeline

use std::sync::Arc;
use tiny_editor::coordinates::{LayoutRect, LogicalSize};
use tiny_editor::{
    app::{AppLogic, TinyApp},
    tree::Doc,
    widget::{LayoutConstraints, LayoutResult, PaintContext, Widget, WidgetEvent, EventResponse, WidgetId},
};

/// Simple red rectangle widget for testing
#[derive(Clone)]
struct TestRect {
    bounds: LayoutRect,
    color: u32,
}

impl TestRect {
    fn new(x: f32, y: f32, width: f32, height: f32, color: u32) -> Self {
        Self {
            bounds: LayoutRect::new(x, y, width, height),
            color,
        }
    }
}

impl Widget for TestRect {
    fn widget_id(&self) -> WidgetId {
        3 // Unique ID for test rect
    }

    fn handle_event(&mut self, _event: &WidgetEvent) -> EventResponse {
        EventResponse::Ignored
    }

    fn layout(&mut self, _constraints: LayoutConstraints) -> LayoutResult {
        LayoutResult {
            size: LogicalSize::new(self.bounds.width.0, self.bounds.height.0),
        }
    }

    fn paint(&self, ctx: &PaintContext<'_>, render_pass: &mut wgpu::RenderPass) {
        println!(
            "TestRect::paint - rendering {}x{} red rectangle",
            self.bounds.width.0, self.bounds.height.0
        );

        // Create rectangle instance for GPU rendering
        let rect_instance = tiny_editor::render::RectInstance {
            rect: tiny_editor::tree::Rect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: self.bounds.width,
                height: self.bounds.height,
            },
            color: self.color,
        };

        // Use the GPU renderer to draw the rectangle
        let gpu = ctx.gpu();
        gpu.draw_rects(render_pass, &[rect_instance], ctx.viewport.scale_factor);
    }

    fn bounds(&self) -> LayoutRect {
        self.bounds
    }

    fn priority(&self) -> i32 {
        0 // Normal priority
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

struct RectTestApp {
    doc: Doc,
}

impl AppLogic for RectTestApp {
    fn on_key(
        &mut self,
        _event: &winit::event::KeyEvent,
        _viewport: &tiny_editor::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
    ) -> bool {
        false // No interaction needed
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn on_ready(&mut self) {
        println!("Ready! You should see a red 100x50 rectangle on screen.");
        println!("If you see it, GPU pipeline works and the issue is font/texture related.");
        println!("If you don't see it, the GPU pipeline itself is broken.");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🟥 Rectangle Rendering Test");
    println!("===========================");

    // Create document with a test rectangle widget
    let doc = Doc::new();
    doc.edit(tiny_editor::tree::Edit::Insert {
        pos: 0,
        content: tiny_editor::tree::Content::Widget(Arc::new(TestRect::new(
            50.0, 25.0, // x, y position
            100.0, 50.0, // width, height
            0xFF0000FF, // Red with full alpha
        ))),
    });
    doc.flush();

    let app = RectTestApp { doc };

    TinyApp::new(app)
        .with_title("Rectangle Test - GPU Pipeline Check")
        .with_size(400.0, 300.0)
        .run()
}