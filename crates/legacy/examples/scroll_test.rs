//! Simple scrolling test

use tiny_editor::app::{AppLogic, TinyApp};
use tiny_editor::coordinates::DocPos;
use tiny_editor::tree::Doc;

struct ScrollTestApp {
    doc: Doc,
    scroll_offset: f32,
}

impl ScrollTestApp {
    fn new() -> Self {
        let mut text = String::new();
        for i in 1..=100 {
            text.push_str(&format!(
                "Line {:03}: This is line number {} out of 100 total lines\n",
                i, i
            ));
        }
        Self {
            doc: Doc::from_str(&text),
            scroll_offset: 0.0,
        }
    }
}

impl AppLogic for ScrollTestApp {
    fn doc(&self) -> &Doc {
        &self.doc
    }

    fn on_key(
        &mut self,
        event: &winit::event::KeyEvent,
        _viewport: &tiny_editor::coordinates::Viewport,
        _modifiers: &winit::event::Modifiers,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};

        if let Key::Named(NamedKey::ArrowDown) = event.logical_key {
            self.scroll_offset += 20.0;
            return true;
        }
        if let Key::Named(NamedKey::ArrowUp) = event.logical_key {
            self.scroll_offset = (self.scroll_offset - 20.0).max(0.0);
            return true;
        }
        false
    }

    fn get_cursor_doc_pos(&self) -> Option<DocPos> {
        // Force scroll by returning a position based on scroll offset
        Some(DocPos {
            byte_offset: 0,
            line: (self.scroll_offset / 20.0) as u32,
            column: 0,
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    TinyApp::new(ScrollTestApp::new())
        .with_title("Scroll Test - Use Up/Down arrows")
        .run()
}
