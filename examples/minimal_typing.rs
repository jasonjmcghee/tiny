//! Minimal example to test basic typing functionality
//!
//! This creates the simplest possible editor that should:
//! 1. Display text
//! 2. Show a cursor
//! 3. Accept keyboard input
//! 4. Update the display when typing

use tiny_editor::tree::{Content, Doc, Edit};
use winit::keyboard::{Key, NamedKey};
use tiny_editor::app::{AppLogic, TinyApp};

struct MinimalTypingApp {
    doc: Doc,
    cursor_pos: usize,
}

impl MinimalTypingApp {
    fn new() -> Self {
        // Start with empty document
        let doc = Doc::from_str("");

        Self {
            doc,
            cursor_pos: 0,
        }
    }

    fn handle_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state != winit::event::ElementState::Pressed {
            return false;
        }

        println!("Key pressed: {:?}", event.logical_key);

        match &event.logical_key {
            Key::Character(ch) => {
                // Insert character at cursor
                println!("Inserting '{}' at position {}", ch, self.cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: self.cursor_pos,
                    content: Content::Text(ch.to_string()),
                });
                self.doc.flush();

                // Move cursor forward
                self.cursor_pos += ch.len();

                // Debug: print document content
                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos);

                return true; // Request redraw
            }
            Key::Named(NamedKey::Space) => {
                // Handle space character
                println!("Inserting space at position {}", self.cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: self.cursor_pos,
                    content: Content::Text(" ".to_string()),
                });
                self.doc.flush();

                self.cursor_pos += 1;

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos);

                return true;
            }
            Key::Named(NamedKey::Enter) => {
                // Handle enter/newline
                println!("Inserting newline at position {}", self.cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: self.cursor_pos,
                    content: Content::Text("\n".to_string()),
                });
                self.doc.flush();

                self.cursor_pos += 1;

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos);

                return true;
            }
            Key::Named(NamedKey::Tab) => {
                // Handle tab character (should render as 4 spaces width)
                println!("Inserting tab at position {}", self.cursor_pos);

                self.doc.edit(Edit::Insert {
                    pos: self.cursor_pos,
                    content: Content::Text("\t".to_string()),
                });
                self.doc.flush();

                self.cursor_pos += 1; // Tab is one character

                let text = self.doc.read().to_string();
                println!("Document now contains: '{}'", text);
                println!("Cursor at: {}", self.cursor_pos);

                return true;
            }
            Key::Named(NamedKey::Backspace) => {
                if self.cursor_pos > 0 {
                    println!("Backspace at position {}", self.cursor_pos);

                    self.doc.edit(Edit::Delete {
                        range: self.cursor_pos - 1..self.cursor_pos,
                    });
                    self.doc.flush();

                    self.cursor_pos -= 1;

                    let text = self.doc.read().to_string();
                    println!("Document now contains: '{}'", text);

                    return true; // Request redraw
                }
            }
            _ => {}
        }

        false // No redraw needed
    }
}

impl AppLogic for MinimalTypingApp {
    fn on_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        self.handle_key(event)
    }

    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn on_ready(&mut self) {
        println!("✅ Ready! Type characters to see them appear.");
        println!("Press backspace to delete.");
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting minimal typing test...");
    println!("Type characters to see them appear");
    println!("Press backspace to delete");

    let app = MinimalTypingApp::new();

    TinyApp::new(app)
        .with_title("Minimal Typing Test")
        .with_size(400.0, 200.0)
        .run()
}