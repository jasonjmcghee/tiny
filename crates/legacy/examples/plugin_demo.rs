//! Demo of the plugin architecture
//!
//! Shows how widgets work as plugins with Update and Paint traits

use tiny_editor::app::{AppLogic, EditorLogic, TinyApp};
use tiny_editor::tree::Doc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Tiny Editor - Plugin Architecture Demo");
    println!("=======================================");
    println!();
    println!("The editor is now using a plugin-based architecture:");
    println!("- Widgets implement Paint and Update traits from the SDK");
    println!("- CursorWidget updates its blink animation via Update trait");
    println!("- All widgets paint via the Paint trait with raw GPU access");
    println!("- SyntaxHighlighter will transform glyphs via Hook<GlyphInstances>");
    println!();
    println!("This architecture allows for:");
    println!("- Hot-swappable plugins (future)");
    println!("- Maximum flexibility with minimal coupling");
    println!("- Type-safe plugin composition");
    println!();

    // Create demo document
    let demo_text = r#"// Plugin Architecture Demo
pub trait Paint {
    fn paint(&self, ctx: &PaintContext, pass: &mut RenderPass);
}

pub trait Update {
    fn update(&mut self, dt: f32, ctx: &mut UpdateContext) -> Result<()>;
}

pub trait Hook<T> {
    type Output;
    fn process(&self, input: T) -> Self::Output;
}

// This is true substrate software!
"#;

    let doc = Doc::from_str(demo_text);
    let editor = EditorLogic::new(doc);

    TinyApp::new(editor)
        .with_title("Plugin Architecture Demo")
        .with_size(800.0, 600.0)
        .with_font_size(16.0)
        .run()
}
