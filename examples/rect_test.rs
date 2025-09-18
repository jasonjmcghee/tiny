//! Test if GPU can render a simple colored rectangle
//!
//! This eliminates font/texture complexity to test basic GPU pipeline

use std::sync::Arc;
use tiny_editor::coordinates::{LogicalPixels, LogicalSize};
use tiny_editor::{
    app::{AppLogic, TinyApp},
    render::RenderOp,
    tree::{Doc, PaintContext, Point, Rect, Widget},
};

/// Simple red rectangle widget for testing
#[derive(Clone)]
struct TestRect {
    width: f32,
    height: f32,
    color: u32,
}

impl Widget for TestRect {
    fn measure(&self) -> LogicalSize {
        LogicalSize::new(self.width, self.height)
    }

    fn z_index(&self) -> i32 {
        0
    }

    fn hit_test(&self, _pt: Point) -> bool {
        false
    }

    fn paint(&self, ctx: &mut PaintContext<'_>) {
        println!(
            "TestRect::paint at ({:.1}, {:.1})",
            ctx.layout_pos.x, ctx.layout_pos.y
        );

        ctx.commands.push(RenderOp::Rect {
            rect: Rect {
                x: ctx.layout_pos.x,
                y: ctx.layout_pos.y,
                width: LogicalPixels(self.width),
                height: LogicalPixels(self.height),
            },
            color: self.color,
        });
    }

    fn clone_box(&self) -> Arc<dyn Widget> {
        Arc::new(self.clone())
    }
}

struct RectTestApp {
    doc: Doc,
}

impl AppLogic for RectTestApp {
    fn on_key(&mut self, _event: &winit::event::KeyEvent) -> bool {
        false // No interaction needed
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn on_ready(&mut self) {
        println!("✅ Ready! You should see a red 100x50 rectangle on screen.");
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
        content: tiny_editor::tree::Content::Widget(Arc::new(TestRect {
            width: 100.0,
            height: 50.0,
            color: 0xFF0000FF, // Red with full alpha
        })),
    });
    doc.flush();

    let app = RectTestApp { doc };

    TinyApp::new(app)
        .with_title("Rectangle Test - GPU Pipeline Check")
        .with_size(400.0, 300.0)
        .run()
}
