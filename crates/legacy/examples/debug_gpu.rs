//! Debug what happens in the actual GPU rendering

use tiny_editor::{
    app::{EditorLogic, TinyApp},
    tree::Doc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 GPU Pipeline Debug");
    println!("====================");
    println!("This will show exactly what the GPU receives and renders.");
    println!("Press keys to type and watch the GPU debug output.\n");

    let doc = Doc::from_str("A"); // Single character

    // Use EditorLogic which already has all the key handling
    TinyApp::new(EditorLogic::new(doc))
        .with_title("GPU Debug")
        .with_size(800.0, 400.0)
        .run()
}
