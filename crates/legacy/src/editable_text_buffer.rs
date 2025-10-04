//! EditableTextBuffer - Input Handling + Editing
//!
//! Adds interaction to TextBufferView:
//! - Cursor and selection management
//! - Keyboard input (typing, arrows, etc)
//! - Mouse interaction (click, drag)
//! - Edit modes (single-line, multi-line, read-only)

use crate::text_buffer_view::{TextBufferView, SizeMode};
use crate::text_buffer::TextBuffer;
use crate::input::{InputHandler, InputAction, Selection, Event};
use crate::coordinates::Viewport;
use crate::input_types::Modifiers;
use tiny_core::tree::Point;
use serde_json::json;
use std::time::Instant;

/// Edit mode for text buffer
#[derive(Clone)]
pub enum EditMode {
    /// Single line input with submit callback
    SingleLine,
    /// Multi-line editor
    MultiLine,
    /// Read-only with optional selection support
    ReadOnly {
        allow_selection: bool,
    },
}

/// Editable text buffer with input handling
pub struct EditableTextBuffer {
    /// The view (rendering + scrolling)
    pub view: TextBufferView,

    /// Input handler (cursor, selection, editing)
    pub input: InputHandler,

    /// Edit mode
    pub mode: EditMode,

    /// Whether to show cursor
    pub show_cursor: bool,

    /// Submit callback for single-line mode
    pub on_submit: Option<Box<dyn Fn(String) + Send + Sync>>,
}

impl EditableTextBuffer {
    /// Create a new editable text buffer
    pub fn new(buffer: TextBuffer, size_mode: SizeMode, mode: EditMode) -> Self {
        Self {
            view: TextBufferView::new(buffer, size_mode),
            input: InputHandler::new(),
            mode,
            show_cursor: true,
            on_submit: None,
        }
    }

    /// Create a single-line input
    pub fn single_line() -> Self {
        Self::new(
            TextBuffer::new(),
            SizeMode::Fixed { lines: 1 },
            EditMode::SingleLine,
        )
    }

    /// Create a multi-line editor
    pub fn multi_line(lines: usize) -> Self {
        Self::new(
            TextBuffer::new(),
            SizeMode::Fixed { lines },
            EditMode::MultiLine,
        )
    }

    /// Create a read-only view with selection
    pub fn read_only(size_mode: SizeMode) -> Self {
        Self::new(
            TextBuffer::new(),
            size_mode,
            EditMode::ReadOnly { allow_selection: true },
        )
    }

    /// Set submit callback for single-line mode
    pub fn with_on_submit<F>(mut self, callback: F) -> Self
    where
        F: Fn(String) + Send + Sync + 'static,
    {
        self.on_submit = Some(Box::new(callback));
        self
    }

    /// Handle keyboard input
    pub fn handle_char(&mut self, ch: char, viewport: &Viewport) -> bool {
        match &self.mode {
            EditMode::SingleLine => {
                // Single line: ignore newlines
                if ch == '\n' || ch == '\r' {
                    return self.handle_submit();
                }
                let event = Event {
                    name: "editor.insert_char".to_string(),
                    data: json!({ "char": ch.to_string() }),
                    priority: 0,
                    timestamp: Instant::now(),
                    source: "text_buffer".to_string(),
                };
                self.input.handle_event(&event, &self.view.buffer.doc, viewport);
                true
            }
            EditMode::MultiLine => {
                let event = Event {
                    name: "editor.insert_char".to_string(),
                    data: json!({ "char": ch.to_string() }),
                    priority: 0,
                    timestamp: Instant::now(),
                    source: "text_buffer".to_string(),
                };
                self.input.handle_event(&event, &self.view.buffer.doc, viewport);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Handle backspace
    pub fn handle_backspace(&mut self, viewport: &Viewport) -> bool {
        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                let event = Event {
                    name: "editor.delete_backward".to_string(),
                    data: json!({}),
                    priority: 0,
                    timestamp: Instant::now(),
                    source: "text_buffer".to_string(),
                };
                self.input.handle_event(&event, &self.view.buffer.doc, viewport);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Handle delete
    pub fn handle_delete(&mut self, viewport: &Viewport) -> bool {
        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                let event = Event {
                    name: "editor.delete_forward".to_string(),
                    data: json!({}),
                    priority: 0,
                    timestamp: Instant::now(),
                    source: "text_buffer".to_string(),
                };
                self.input.handle_event(&event, &self.view.buffer.doc, viewport);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Handle submit (Enter in single-line mode)
    pub fn handle_submit(&mut self) -> bool {
        if let EditMode::SingleLine = self.mode {
            if let Some(callback) = &self.on_submit {
                let text = self.view.buffer.text();
                callback(text.as_ref().clone());
            }
            true
        } else {
            false
        }
    }

    /// Handle arrow keys
    pub fn handle_arrow(&mut self, direction: ArrowDirection, modifiers: &Modifiers, viewport: &Viewport) -> bool {
        let shift = modifiers.state().shift_key();
        let event_name = match (direction, shift) {
            (ArrowDirection::Up, false) => "editor.move_up",
            (ArrowDirection::Up, true) => "editor.extend_up",
            (ArrowDirection::Down, false) => "editor.move_down",
            (ArrowDirection::Down, true) => "editor.extend_down",
            (ArrowDirection::Left, false) => "editor.move_left",
            (ArrowDirection::Left, true) => "editor.extend_left",
            (ArrowDirection::Right, false) => "editor.move_right",
            (ArrowDirection::Right, true) => "editor.extend_right",
        };

        let event = Event {
            name: event_name.to_string(),
            data: json!({}),
            priority: 0,
            timestamp: Instant::now(),
            source: "text_buffer".to_string(),
        };
        self.input.handle_event(&event, &self.view.buffer.doc, viewport);
        true
    }

    /// Handle mouse click
    pub fn handle_click(&mut self, pos: Point, viewport: &Viewport, modifiers: &Modifiers) -> bool {
        match &self.mode {
            EditMode::ReadOnly { allow_selection: false } => false,
            _ => {
                use crate::input_types::MouseButton;
                self.input.on_mouse_click(
                    &self.view.buffer.doc,
                    viewport,
                    pos,
                    MouseButton::Left,
                    modifiers.state().alt_key(),
                    modifiers.state().shift_key(),
                );
                true
            }
        }
    }

    /// Handle mouse drag
    pub fn handle_drag(&mut self, from: Point, to: Point, viewport: &Viewport, modifiers: &Modifiers) -> bool {
        match &self.mode {
            EditMode::ReadOnly { allow_selection: false } => false,
            _ => {
                self.input.on_mouse_drag(
                    &self.view.buffer.doc,
                    viewport,
                    from,
                    to,
                    modifiers.state().alt_key(),
                );
                true
            }
        }
    }

    /// Handle undo
    pub fn handle_undo(&mut self, viewport: &Viewport) -> bool {
        let event = Event {
            name: "editor.undo".to_string(),
            data: json!({}),
            priority: 0,
            timestamp: Instant::now(),
            source: "text_buffer".to_string(),
        };
        let action = self.input.handle_event(&event, &self.view.buffer.doc, viewport);
        action != InputAction::None
    }

    /// Handle redo
    pub fn handle_redo(&mut self, viewport: &Viewport) -> bool {
        let event = Event {
            name: "editor.redo".to_string(),
            data: json!({}),
            priority: 0,
            timestamp: Instant::now(),
            source: "text_buffer".to_string(),
        };
        let action = self.input.handle_event(&event, &self.view.buffer.doc, viewport);
        action != InputAction::None
    }

    /// Get cursor position for scrolling
    pub fn cursor_pos(&self) -> Option<tiny_sdk::DocPos> {
        self.input.selections().first().map(|sel| sel.cursor)
    }

    /// Get selections for rendering
    pub fn selections(&self) -> &[Selection] {
        self.input.selections()
    }

    /// Clear text
    pub fn clear(&mut self) {
        self.view.buffer.clear();
        self.input = InputHandler::new(); // Reset cursor
    }

    /// Set text (replaces all content)
    pub fn set_text(&mut self, text: &str) {
        self.view.buffer.set_text(text);
        self.input = InputHandler::new(); // Reset cursor
    }

    /// Get current text
    pub fn text(&self) -> Arc<String> {
        self.view.buffer.text()
    }

    /// Get mutable view reference
    pub fn view_mut(&mut self) -> &mut TextBufferView {
        &mut self.view
    }

    /// Get view reference
    pub fn view(&self) -> &TextBufferView {
        &self.view
    }

    // === Focus Management ===

    /// Set focus state (controls cursor visibility)
    pub fn set_focused(&mut self, focused: bool) {
        self.show_cursor = focused;
    }

    /// Check if buffer is focused
    pub fn is_focused(&self) -> bool {
        self.show_cursor
    }

    // === Line Management ===

    /// Get the line number of the cursor
    pub fn get_cursor_line(&self) -> usize {
        if let Some(cursor) = self.cursor_pos() {
            cursor.line as usize
        } else {
            0
        }
    }

    /// Get total line count
    pub fn line_count(&self) -> usize {
        self.view.buffer.line_count()
    }

    /// Set which line should be highlighted
    pub fn set_highlight_line(&mut self, line: Option<usize>) {
        self.view.highlighted_line = line;
    }

    /// Get currently highlighted line
    pub fn get_highlight_line(&self) -> Option<usize> {
        self.view.highlighted_line
    }

    /// Move cursor to start of specified line
    pub fn move_cursor_to_line(&mut self, line: usize, _viewport: &Viewport) {
        let tree = self.view.buffer.doc.read();
        if let Some(byte_offset) = tree.line_to_byte(line as u32) {
            // Convert byte offset to DocPos
            let doc_line = tree.byte_to_line(byte_offset);
            let doc_pos = tiny_sdk::DocPos {
                line: doc_line,
                column: 0,
                byte_offset: 0,
            };

            // Directly set cursor position
            if let Some(sel) = self.input.selections_mut_for_test().first_mut() {
                sel.cursor = doc_pos;
                sel.anchor = doc_pos;
            }
        }
    }

    /// Select entire line at cursor
    pub fn select_current_line(&mut self) {
        let line = self.get_cursor_line();
        self.select_line(line);
    }

    /// Select entire line by index
    pub fn select_line(&mut self, line: usize) {
        let tree = self.view.buffer.doc.read();
        if let Some(start_byte) = tree.line_to_byte(line as u32) {
            let end_byte = tree.find_next_newline(start_byte)
                .unwrap_or(tree.byte_count());

            // Convert byte offsets to DocPos
            let start_line = tree.byte_to_line(start_byte);
            let start_pos = tiny_sdk::DocPos {
                line: start_line,
                column: 0,
                byte_offset: 0,
            };

            let end_line = tree.byte_to_line(end_byte);
            let end_line_start = tree.line_to_byte(end_line).unwrap_or(0);
            let end_column = tree
                .get_text_slice(end_line_start..end_byte)
                .chars()
                .count() as u32;
            let end_pos = tiny_sdk::DocPos {
                line: end_line,
                column: end_column,
                byte_offset: 0,
            };

            // Set selection
            if let Some(sel) = self.input.selections_mut_for_test().first_mut() {
                sel.anchor = start_pos;
                sel.cursor = end_pos;
            }
        }
    }

    /// Get text of specified line
    pub fn get_line_text(&self, line: usize) -> Option<String> {
        let tree = self.view.buffer.doc.read();
        let start_byte = tree.line_to_byte(line as u32)?;
        let end_byte = tree.find_next_newline(start_byte)
            .unwrap_or(tree.byte_count());
        Some(tree.get_text_slice(start_byte..end_byte))
    }
}

/// Arrow direction for keyboard navigation
#[derive(Debug, Clone, Copy)]
pub enum ArrowDirection {
    Up,
    Down,
    Left,
    Right,
}

impl Default for EditableTextBuffer {
    fn default() -> Self {
        Self::multi_line(10)
    }
}

use std::sync::Arc;
