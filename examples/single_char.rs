//! Absolute minimal example: render a single "A" character
//!
//! If this doesn't work, nothing will work

use tiny_editor::app::run_simple_app;
use tiny_editor::tree::Doc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔤 Single Character Rendering Test");
    println!("==================================");
    println!("This should show a single 'A' character on screen.");
    println!("If you don't see it, the rendering pipeline is broken.\n");

    // Create document with just "A"
    let doc = Doc::from_str("ABCD\nEFGH");

    // Run with shared app infrastructure (no boilerplate!)
    run_simple_app("Single Character Test", doc)
}

