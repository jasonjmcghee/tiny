//! Test different theme modes in the editor

use tiny_editor::{Doc, Tree};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a test document with some Rust code
    let test_code = r#"// Theme test: Watch the colors!
use std::collections::HashMap;

fn main() {
    let message = "Hello, themed world!";
    let numbers = vec![1, 2, 3, 4, 5];

    for num in numbers {
        println!("Number: {}", num);
    }

    let mut map = HashMap::new();
    map.insert("key", "value");

    let result = calculate_sum(10, 20);
    println!("Sum is: {}", result);
}

fn calculate_sum(a: i32, b: i32) -> i32 {
    // This is a comment
    let sum = a + b;
    return sum;
}

struct Person {
    name: String,
    age: u32,
}

impl Person {
    fn new(name: String, age: u32) -> Self {
        Self { name, age }
    }

    fn greet(&self) {
        println!("Hello, I'm {} and I'm {} years old", self.name, self.age);
    }
}
"#;

    let tree = Tree::new(test_code.as_bytes().to_vec());
    let doc = Doc::new(tree);

    tiny_editor::app::TinyApp::new(ExampleApp { doc })
        .with_title("Rotating Rainbow Theme - Watch the colors flow!")
        .with_size(1000, 700)
        .with_continuous_rendering(true)  // Enable smooth animations!
        .run()
}

struct ExampleApp {
    doc: Doc,
}

impl tiny_editor::app::AppLogic for ExampleApp {
    fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        _viewport: &tiny_editor::coordinates::Viewport,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{KeyCode, PhysicalKey};

        if !event.state.is_pressed() {
            return false;
        }

        // Allow quitting with Cmd+Q or Ctrl+Q
        if modifiers.state().super_key() || modifiers.state().control_key() {
            if let PhysicalKey::Code(KeyCode::KeyQ) = event.physical_key {
                std::process::exit(0);
            }
        }

        false
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn doc_mut(&mut self) -> &mut Doc {
        &mut self.doc
    }
}