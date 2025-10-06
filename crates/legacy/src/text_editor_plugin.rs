//! TextEditor as a plugin - unified editing experience
//!
//! This replaces the widget system with a single cohesive plugin

use crate::{
    coordinates::Viewport,
    editable_text_view::{EditMode, EditableTextView},
    input::{Event, EventSubscriber, InputAction, PropagationControl, Selection},
    syntax::SyntaxHighlighter,
    text_effects::TextStyleProvider,
};
use std::path::PathBuf;
use std::sync::Arc;
use tiny_core::tree::{Doc, Point};
use tiny_sdk::{
    Initializable, LayoutPos, Plugin, PluginError,
    SetupContext, Updatable, UpdateContext,
};
use tiny_ui::{TextView, TextViewCapabilities};

/// The main text editor plugin - handles everything
pub struct TextEditorPlugin {
    // Core editing view with full capabilities
    pub editor: EditableTextView,

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
        // Create viewport (will be updated by renderer)
        let viewport = Viewport::new(800.0, 600.0, 1.0);

        // Create TextView with full editor capabilities
        let text_view = TextView::with_capabilities(
            doc,
            viewport,
            TextViewCapabilities::full_editor(),
        );

        // Wrap in EditableTextView for editing support
        let editor = EditableTextView::new(text_view, EditMode::MultiLine);

        Self {
            editor,
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
        let current_text = self.editor.view.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        current_text.hash(&mut hasher);
        let current_hash = hasher.finish();
        current_hash != self.last_saved_content_hash
    }

    /// Initialize plugins for the editor (must be called after construction)
    pub fn initialize_plugins(&mut self, plugin_loader: &tiny_core::plugin_loader::PluginLoader) -> Result<(), String> {
        self.editor.initialize_plugins(plugin_loader)
    }

    /// Setup plugins with GPU resources
    pub fn setup_plugins(&mut self, ctx: &mut tiny_sdk::SetupContext) -> Result<(), tiny_sdk::PluginError> {
        self.editor.setup_plugins(ctx)
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
                editor.editor.input.set_syntax_highlighter(shared_highlighter);

                // Request initial syntax highlighting
                syntax_hl.request_update_with_edit(&content, editor.editor.view.doc.version(), None);
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
        self.editor.handle_click(
            pos,
            modifiers.state().shift_key(),
            modifiers.state().alt_key(),
        )
    }

    /// Handle mouse drag
    pub fn on_drag(
        &mut self,
        from: Point,
        to: Point,
        viewport: &Viewport,
        modifiers: &crate::input_types::Modifiers,
    ) -> bool {
        self.editor.handle_drag(from, to, modifiers.state().alt_key())
    }

    pub fn save(&mut self) -> Result<(), std::io::Error> {
        if let Some(ref path) = self.file_path {
            let content = self.editor.view.doc.read().flatten_to_string();
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
            InputAction::Undo => self.editor.handle_undo(),
            InputAction::Redo => self.editor.handle_redo(),
            InputAction::Redraw => true,
            InputAction::None => false,
        }
    }

    /// Get cursor position for scrolling
    pub fn get_cursor_doc_pos(&self) -> Option<tiny_sdk::DocPos> {
        Some(self.editor.cursor_pos())
    }

    /// Get primary cursor layout position for rendering
    pub fn get_primary_cursor_layout_pos(
        &self,
        doc: &Doc,
        viewport: &crate::coordinates::Viewport,
    ) -> Option<LayoutPos> {
        let cursor = self.editor.cursor_pos();
        Some(viewport.doc_to_layout(cursor))
    }

    /// Get selections for rendering
    pub fn selections(&self) -> &[Selection] {
        self.editor.selections()
    }
}

// === Plugin Trait Implementations ===

impl EventSubscriber for TextEditorPlugin {
    fn handle_event(&mut self, event: &Event, _event_bus: &mut crate::input::EventBus) -> PropagationControl {
        // Main editor doesn't claim navigation events (overlays handle those)
        // editor.* events are handled by app.rs's explicit editor event handling
        PropagationControl::Continue
    }

    fn priority(&self) -> i32 {
        0 // Low priority (overlays handle events first)
    }

    fn is_active(&self) -> bool {
        true // Main editor is always active (unless overlay is focused)
    }
}

tiny_sdk::plugin! {
    TextEditorPlugin {
        name: "text_editor",
        version: "1.0.0",
        z_index: 0,
        traits: [Init, Update, Paint],
        defaults: [Paint],  // Custom impl for Init and Update
    }
}

impl Initializable for TextEditorPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        if let Some(ref mut highlighter) = self.syntax_highlighter {
            if let Some(syntax_hl) = highlighter.as_any().downcast_ref::<SyntaxHighlighter>() {
                let text = self.editor.view.doc.read().flatten_to_string();
                syntax_hl.request_update_with_edit(&text, self.editor.view.doc.version(), None);
            }
        }
        Ok(())
    }
}

impl Updatable for TextEditorPlugin {
    fn update(&mut self, _dt: f32, _ctx: &mut UpdateContext) -> Result<(), PluginError> {
        Ok(())
    }
}
