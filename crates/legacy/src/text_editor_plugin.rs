//! TextEditor as a plugin - unified editing experience
//!
//! This replaces the widget system with a single cohesive plugin

use crate::{
    coordinates::Viewport,
    input::{InputAction, InputHandler, Selection},
    syntax::SyntaxHighlighter,
    text_effects::TextStyleProvider,
};
use std::path::PathBuf;
use std::sync::Arc;
use tiny_core::tree::{Doc, Point};
use tiny_sdk::{
    Capability, Initializable, LayoutPos, PaintContext, Paintable, Plugin,
    PluginError, SetupContext, Updatable, UpdateContext,
};

/// The main text editor plugin - handles everything
pub struct TextEditorPlugin {
    // Core document and editing
    pub doc: Doc,
    pub input: InputHandler,

    // Rendering state
    pub syntax_highlighter: Option<Box<dyn TextStyleProvider>>,
    pub show_line_numbers: bool,

    // File management
    pub file_path: Option<PathBuf>,
    pub last_saved_content_hash: u64,

    // Cmd+hover for go-to-definition preview (line, start_col, end_col)
    pub cmd_hover_range: Option<(u32, u32, u32)>,
}

impl TextEditorPlugin {
    pub fn new(doc: Doc) -> Self {
        Self {
            doc,
            input: InputHandler::new(),
            syntax_highlighter: None,
            show_line_numbers: true,
            file_path: None,
            last_saved_content_hash: 0,
            cmd_hover_range: None,
        }
    }

    /// Check if document has unsaved changes
    pub fn is_modified(&self) -> bool {
        use ahash::AHasher;
        use std::hash::{Hash, Hasher};
        let current_text = self.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        current_text.hash(&mut hasher);
        let current_hash = hasher.finish();
        current_hash != self.last_saved_content_hash
    }

    pub fn from_file(path: PathBuf) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(&path)?;
        let doc = Doc::from_str(&content);
        let mut editor = Self::new(doc);
        editor.file_path = Some(path.clone());

        // Calculate saved content hash (file was just loaded)
        use ahash::AHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = AHasher::default();
        content.hash(&mut hasher);
        editor.last_saved_content_hash = hasher.finish();

        // Setup syntax highlighter based on file extension
        if let Some(highlighter) = SyntaxHighlighter::from_file_path(path.to_str().unwrap_or("")) {
            let syntax_highlighter: Box<dyn TextStyleProvider> = Box::new(highlighter);

            // Set up shared highlighter for InputHandler
            if let Some(syntax_hl) = syntax_highlighter
                .as_any()
                .downcast_ref::<crate::syntax::SyntaxHighlighter>()
            {
                let shared_highlighter = Arc::new(syntax_hl.clone());
                editor.input.set_syntax_highlighter(shared_highlighter);

                // Request initial syntax highlighting
                syntax_hl.request_update_with_edit(&content, editor.doc.version(), None);
            }

            editor.syntax_highlighter = Some(syntax_highlighter);
        }

        Ok(editor)
    }

    /// Handle mouse click
    pub fn on_click(
        &mut self,
        pos: Point,
        viewport: &Viewport,
        modifiers: &crate::input_types::Modifiers,
    ) -> bool {
        use crate::input_types::MouseButton;
        self.input.on_mouse_click(
            &self.doc,
            viewport,
            pos,
            MouseButton::Left,
            modifiers.state().alt_key(),
            modifiers.state().shift_key(),
        );
        true
    }

    /// Handle mouse drag
    pub fn on_drag(
        &mut self,
        from: Point,
        to: Point,
        viewport: &Viewport,
        modifiers: &crate::input_types::Modifiers,
    ) -> bool {
        let (_redraw, _scroll) =
            self.input
                .on_mouse_drag(&self.doc, viewport, from, to, modifiers.state().alt_key());
        true
    }

    pub fn save(&mut self) -> Result<(), std::io::Error> {
        if let Some(ref path) = self.file_path {
            let content = self.doc.read().flatten_to_string();
            std::fs::write(path, content.as_bytes())?;

            // Update saved content hash
            use ahash::AHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = AHasher::default();
            content.hash(&mut hasher);
            self.last_saved_content_hash = hasher.finish();
        }
        Ok(())
    }

    pub fn handle_input_action(&mut self, action: InputAction) -> bool {
        match action {
            InputAction::Save => {
                if let Err(e) = self.save() {
                    eprintln!("Failed to save: {}", e);
                }
                true
            }
            InputAction::Undo => self.input.undo(&self.doc),
            InputAction::Redo => self.input.redo(&self.doc),
            InputAction::Redraw => true,
            InputAction::None => false,
        }
    }

    /// Get cursor position for scrolling
    pub fn get_cursor_doc_pos(&self) -> Option<tiny_sdk::DocPos> {
        // Get primary cursor position
        self.input.selections().first().map(|sel| sel.cursor)
    }

    /// Get primary cursor layout position for rendering
    pub fn get_primary_cursor_layout_pos(
        &self,
        doc: &Doc,
        viewport: &crate::coordinates::Viewport,
    ) -> Option<LayoutPos> {
        self.input
            .selections()
            .first()
            .map(|sel| viewport.doc_to_layout(sel.cursor))
    }

    /// Get selections for rendering
    pub fn selections(&self) -> &[Selection] {
        self.input.selections()
    }
}

// === Plugin Trait Implementations ===

impl Plugin for TextEditorPlugin {
    fn name(&self) -> &str {
        "text_editor"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Updatable,
            Capability::Paintable("text_editor".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_updatable(&mut self) -> Option<&mut dyn Updatable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for TextEditorPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        // Initialize syntax highlighter if needed
        if let Some(ref mut highlighter) = self.syntax_highlighter {
            if let Some(syntax_hl) = highlighter.as_any().downcast_ref::<SyntaxHighlighter>() {
                let text = self.doc.read().flatten_to_string();
                syntax_hl.request_update_with_edit(&text, self.doc.version(), None);
            }
        }
        Ok(())
    }
}

impl Updatable for TextEditorPlugin {
    fn update(&mut self, _dt: f32, _ctx: &mut UpdateContext) -> Result<(), PluginError> {
        // Nothing to update - cursor and selection handled by plugins
        Ok(())
    }
}

impl Paintable for TextEditorPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {
        // TextEditorPlugin no longer renders anything directly!
        // The main renderer handles document text rendering through text_renderer
        // Line numbers will be handled by a separate LineNumbersPlugin
        // This plugin only manages the document and handles input
    }

    fn z_index(&self) -> i32 {
        0 // Main editor renders at base layer
    }
}
