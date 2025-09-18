//! Complete windowed demo with GPU rendering using the TinyApp abstraction

use tiny_editor::{
    app::{EditorLogic, TinyApp},
    tree::Doc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let doc = Doc::from_str(
        r#"// Welcome to Tiny Editor!
//
// This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust. This is a minimal text editor built in ~1000 lines of Rust.
//
// Key features:
// [x] Lock-free reads via ArcSwap (RCU pattern)
// [x] Everything is a widget (text, cursors, selections)
// [x] O(log n) operations via sum-tree
// [x] Tree-sitter syntax highlighting
// [x] GPU-accelerated rendering with wgpu
// [x] Multi-cursor support

fn main() {
    println!("Hello from tiny editor!");

    // Try typing, selecting text, and using keyboard shortcuts:
    // - Arrow keys to navigate
    // - Backspace/Delete to remove text
    // - Home/End for line navigation
    // - Type to insert text

    let mut sum = 0;
    for i in 0..10 {
        sum += i;
    }
    println!("Sum: {}", sum);
}

// The entire editor fits in a single tree structure where
// text, widgets, and selections are all just "spans" in the tree.
// This unified design eliminates synchronization complexity."#,
    );

    TinyApp::new(EditorLogic::new(doc))
        .with_title("Tiny Editor - Ultra-Minimal")
        .with_size(800.0, 600.0)
        .with_font_size(14.0)
        .run()
}

