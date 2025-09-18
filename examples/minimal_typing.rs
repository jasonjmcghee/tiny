//! Minimal example to test basic typing functionality
//!
//! This creates the simplest possible editor that should:
//! 1. Display text
//! 2. Show a cursor
//! 3. Accept keyboard input
//! 4. Update the display when typing

use tiny_editor::{
    app::{EditorLogic, TinyApp},
    tree::Doc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Doc::from_str("Replace me");

    TinyApp::new(EditorLogic::new(doc))
        .with_title("Minimal Typing Test")
        .with_size(400.0, 200.0)
        .run()
}
