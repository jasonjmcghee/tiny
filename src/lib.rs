//! Ultra-minimal text editor foundation
//!
//! Everything is a span in a tree with RCU for lock-free reads

extern crate lazy_static;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod coordinates; // Coordinate system abstraction
pub mod font;
pub mod gpu;
pub mod history;
pub mod input;
pub mod io;
pub mod render;
pub mod syntax;
pub mod text_effects;
pub mod tree;
pub mod tree_nav; // O(log n) navigation methods
pub mod widget;
pub mod app;

// Re-export core types
pub use history::History;
pub use input::{InputHandler, Selection};
pub use render::{BatchedDraw, RenderOp, Renderer};
pub use syntax::SyntaxHighlighter;
pub use tree::{Content, Doc, Edit, Point, Rect, Span, Tree};
pub use widget::{Widget, WidgetEvent, EventResponse, LayoutConstraints, LayoutResult, WidgetId};
pub use widget::{CursorWidget, SelectionWidget, StyleId, TextWidget};

use std::path::Path;

/// Complete editor state
pub struct Editor {
    /// Document tree
    pub doc: Doc,
    /// Input handler
    pub input: InputHandler,
    /// Renderer
    pub renderer: Renderer,
    /// Syntax highlighter
    pub syntax: Option<Box<dyn text_effects::TextStyleProvider>>,
    /// History for undo/redo
    pub history: History,
    /// Current file path
    pub path: Option<std::path::PathBuf>,
}

impl Editor {
    /// Create new empty editor with explicit size and scale
    pub fn new(size: (f32, f32), scale_factor: f32) -> Self {
        Self {
            doc: Doc::new(),
            input: InputHandler::new(),
            renderer: Renderer::new(size, scale_factor),
            syntax: None,
            history: History::new(),
            path: None,
        }
    }

    /// Create new empty editor with default size
    pub fn new_default() -> Self {
        Self::new((800.0, 600.0), 1.0)
    }

    /// Create editor with text and explicit size
    pub fn with_text(text: &str, size: (f32, f32), scale_factor: f32) -> Self {
        Self {
            doc: Doc::from_str(text),
            input: InputHandler::new(),
            renderer: Renderer::new(size, scale_factor),
            syntax: None,
            history: History::new(),
            path: None,
        }
    }

    /// Create editor with text and default size
    pub fn with_text_default(text: &str) -> Self {
        Self::with_text(text, (800.0, 600.0), 1.0)
    }

    /// Load file
    pub fn load(&mut self, path: &Path) -> std::io::Result<()> {
        self.doc = io::load(path)?;
        self.path = Some(path.to_path_buf());

        // Enable syntax highlighting for Rust files
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            self.syntax = Some(crate::syntax::create_rust_highlighter());
            self.update_syntax();
        }

        Ok(())
    }

    /// Save file
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            io::save(&self.doc, path)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No file path set",
            ))
        }
    }

    /// Save to specific path
    pub fn save_as(&mut self, path: &Path) -> std::io::Result<()> {
        io::save(&self.doc, path)?;
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    /// Handle key input
    pub fn on_key(&mut self, event: &winit::event::KeyEvent) {
        // Save checkpoint before edit
        self.history.checkpoint(self.doc.read());

        // Process input - create a dummy viewport for compatibility
        let dummy_viewport = coordinates::Viewport::new(800.0, 600.0, 1.0);
        let dummy_modifiers = winit::event::Modifiers::default();
        self.input.on_key(&self.doc, &dummy_viewport, event, &dummy_modifiers);

        // Update syntax if needed
        if self.syntax.is_some() {
            self.update_syntax();
        }
    }

    /// Handle mouse click
    pub fn on_mouse_click(
        &mut self,
        pos: Point,
        button: winit::event::MouseButton,
        alt_held: bool,
    ) {
        // Create a dummy viewport for compatibility
        let dummy_viewport = coordinates::Viewport::new(800.0, 600.0, 1.0);
        self.input.on_mouse_click(&self.doc, &dummy_viewport, pos, button, alt_held);
    }

    /// Undo
    pub fn undo(&mut self) {
        if let Some(_tree) = self.history.undo(self.doc.read()) {
            // Replace document tree
            self.doc = Doc::new();
            // Would need to restore from tree - simplified here
        }
    }

    /// Redo
    pub fn redo(&mut self) {
        if let Some(_tree) = self.history.redo(self.doc.read()) {
            // Replace document tree
            self.doc = Doc::new();
            // Would need to restore from tree - simplified here
        }
    }

    /// Update syntax highlighting
    fn update_syntax(&mut self) {
        if let Some(syntax) = &self.syntax {
            let text = self.doc.read().to_string();
            let version = self.doc.version();
            syntax.request_update(&text, version);
        }
    }

    /// Render to GPU commands
    pub fn render(&mut self, viewport: Rect) -> Vec<BatchedDraw> {
        self.renderer.render(&self.doc.read(), viewport, self.input.selections())
    }

    /// Get text content
    pub fn text(&self) -> String {
        self.doc.read().to_string()
    }

    /// Get cursor positions
    pub fn cursors(&self) -> Vec<usize> {
        self.input.selections().iter().map(|s| s.cursor.byte_offset).collect()
    }

    /// Get document version
    pub fn version(&self) -> u64 {
        self.doc.version()
    }

    /// Resize the editor viewport
    pub fn resize(&mut self, width: f32, height: f32, scale_factor: f32) {
        self.renderer.update_viewport(width, height, scale_factor);
    }
}

// === Builder Pattern ===

pub struct EditorBuilder {
    text: Option<String>,
    path: Option<std::path::PathBuf>,
    syntax: bool,
    size: (f32, f32),
    scale_factor: f32,
}

impl EditorBuilder {
    pub fn new() -> Self {
        Self {
            text: None,
            path: None,
            syntax: true,
            size: (800.0, 600.0), // Default window size
            scale_factor: 1.0,
        }
    }

    pub fn with_text(mut self, text: String) -> Self {
        self.text = Some(text);
        self
    }

    pub fn with_file(mut self, path: impl AsRef<Path>) -> Self {
        self.path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_syntax(mut self, enabled: bool) -> Self {
        self.syntax = enabled;
        self
    }

    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.size = (width, height);
        self
    }

    pub fn with_scale_factor(mut self, scale_factor: f32) -> Self {
        self.scale_factor = scale_factor;
        self
    }

    pub fn build(self) -> std::io::Result<Editor> {
        let mut editor = if let Some(text) = self.text {
            Editor::with_text(&text, self.size, self.scale_factor)
        } else {
            Editor::new(self.size, self.scale_factor)
        };

        if let Some(path) = self.path {
            editor.load(&path)?;
        }

        if self.syntax {
            editor.syntax = Some(crate::syntax::create_rust_highlighter());
        }

        Ok(editor)
    }
}

