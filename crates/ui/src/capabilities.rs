//! Capability system for TextView/EditableTextView
//!
//! Allows fine-grained control over what features are enabled.
//! Think of it like vim's 'modifiable', 'readonly', etc. options.

/// Configuration for TextView/EditableTextView capabilities
#[derive(Debug, Clone, Copy)]
pub struct TextViewCapabilities {
    /// Allow text selection (mouse drag, shift+arrows)
    pub selection: bool,
    /// Show and manipulate cursor (EditableTextView only)
    pub cursor: bool,
    /// Allow text editing (typing, paste, delete)
    pub editing: bool,
    /// Enable undo/redo history
    pub undo_redo: bool,
    /// Enable syntax highlighting
    pub syntax: bool,
    /// Auto-scroll to keep cursor/selection visible
    pub auto_scroll: bool,
    /// Enable clipboard operations (copy/cut/paste)
    pub clipboard: bool,
    /// Enable mouse interaction (click, drag, hover)
    pub mouse: bool,
    /// Enable keyboard navigation (arrows, page up/down, home/end)
    pub keyboard_nav: bool,
    /// Enable file persistence (save/load)
    pub file_persistence: bool,
    /// Enable LSP integration
    pub lsp: bool,
}

impl TextViewCapabilities {
    /// Completely read-only view - no interaction at all
    /// Like viewing a log file or help text
    pub fn read_only() -> Self {
        Self {
            selection: false,
            cursor: false,
            editing: false,
            undo_redo: false,
            syntax: false,
            auto_scroll: false,
            clipboard: false,
            mouse: false,
            keyboard_nav: false,
            file_persistence: false,
            lsp: false,
        }
    }

    /// Selectable read-only view - can select and copy
    /// Like a terminal buffer with mouse selection
    pub fn selectable() -> Self {
        Self {
            selection: true,
            cursor: false,
            editing: false,
            undo_redo: false,
            syntax: false,
            auto_scroll: true,
            clipboard: true,
            mouse: true,
            keyboard_nav: true,
            file_persistence: false,
            lsp: false,
        }
    }

    /// Basic editable view - like a simple text input
    /// Has cursor, editing, clipboard, but no undo or fancy features
    pub fn basic_editable() -> Self {
        Self {
            selection: true,
            cursor: true,
            editing: true,
            undo_redo: false,
            syntax: false,
            auto_scroll: true,
            clipboard: true,
            mouse: true,
            keyboard_nav: true,
            file_persistence: false,
            lsp: false,
        }
    }

    /// Full editable view with undo/redo - like a text editor
    pub fn editable() -> Self {
        Self {
            selection: true,
            cursor: true,
            editing: true,
            undo_redo: true,
            syntax: true,
            auto_scroll: true,
            clipboard: true,
            mouse: true,
            keyboard_nav: true,
            file_persistence: false,
            lsp: false,
        }
    }

    /// Full editor with file persistence and LSP
    pub fn full_editor() -> Self {
        Self {
            selection: true,
            cursor: true,
            editing: true,
            undo_redo: true,
            syntax: true,
            auto_scroll: true,
            clipboard: true,
            mouse: true,
            keyboard_nav: true,
            file_persistence: true,
            lsp: true,
        }
    }

    /// Builder pattern: enable selection
    pub fn with_selection(mut self) -> Self {
        self.selection = true;
        self
    }

    /// Builder pattern: enable cursor
    pub fn with_cursor(mut self) -> Self {
        self.cursor = true;
        self
    }

    /// Builder pattern: enable editing
    pub fn with_editing(mut self) -> Self {
        self.editing = true;
        self.clipboard = true; // Editing implies clipboard
        self
    }

    /// Builder pattern: enable undo/redo
    pub fn with_undo_redo(mut self) -> Self {
        self.undo_redo = true;
        self
    }

    /// Builder pattern: enable syntax highlighting
    pub fn with_syntax(mut self) -> Self {
        self.syntax = true;
        self
    }

    /// Builder pattern: enable file persistence
    pub fn with_file_persistence(mut self) -> Self {
        self.file_persistence = true;
        self
    }

    /// Builder pattern: enable LSP
    pub fn with_lsp(mut self) -> Self {
        self.lsp = true;
        self
    }

    /// Check if any interactive capability is enabled
    pub fn is_interactive(&self) -> bool {
        self.selection || self.cursor || self.editing || self.mouse || self.keyboard_nav
    }

    /// Check if view should handle keyboard input
    pub fn handles_keyboard(&self) -> bool {
        self.editing || self.keyboard_nav || self.selection
    }

    /// Check if view should handle mouse input
    pub fn handles_mouse(&self) -> bool {
        self.mouse || self.selection || self.editing
    }
}

impl Default for TextViewCapabilities {
    fn default() -> Self {
        // Default to selectable read-only
        Self::selectable()
    }
}
