//! Test scrolling with a large document

use tiny_editor::app::{EditorLogic, TinyApp};
use tiny_editor::tree::Doc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a large document with 100 lines
    let mut text = String::new();
    for i in 1..=100 {
        text.push_str(&format!(
            "Line {:03}: This is a test line with some content to make it wider\n",
            i
        ));
    }

    let doc = Doc::from_str(&text);
    let editor = EditorLogic::new(doc);

    TinyApp::new(editor)
        .with_title("Scrolling Test - Use arrows, Page Up/Down, mouse wheel")
        .with_size(800.0, 600.0)
        .run()
}
