// Welcome to Tiny Editor!
//
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
// This unified design eliminates synchronization complexity.
