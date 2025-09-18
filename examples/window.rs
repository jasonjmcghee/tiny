//! Complete windowed demo with GPU rendering using the TinyApp abstraction

use tiny_editor::{
    app::{EditorLogic, TinyApp},
    tree::Doc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Doc::from_str(include_str!("../assets/sample.rs"));

    TinyApp::new(EditorLogic::new(doc))
        .with_title("Tiny Editor - Ultra-Minimal")
        .with_size(800.0, 600.0)
        .with_font_size(14.0)
        .run()
}
