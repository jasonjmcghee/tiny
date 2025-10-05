//! EditableTextView - TextView + Input Handling
//!
//! Adds interaction to TextView:
//! - Keyboard input (typing, navigation, clipboard)
//! - Mouse input (click, drag, selection)
//! - Edit modes (single-line, multi-line, read-only)
//! - Cursor and selection management

use crate::{
    coordinates::Viewport,
    input::{InputHandler, Selection},
    text_view::TextView,
};
use tiny_core::tree::Point;
use tiny_sdk::LayoutPos;

/// Edit mode for text view
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EditMode {
    /// Single line input (Enter submits)
    SingleLine,
    /// Multi-line editor (Enter inserts newline)
    MultiLine,
    /// Read-only view with optional selection support
    ReadOnly { allow_selection: bool },
}

/// TextView with input handling
pub struct EditableTextView {
    /// The view (rendering + layout)
    pub view: TextView,

    /// Input handler (cursor, selection, editing)
    pub input: InputHandler,

    /// Edit mode
    pub mode: EditMode,

    /// Whether to show cursor
    pub show_cursor: bool,

    /// Submit callback for single-line mode
    pub on_submit: Option<Box<dyn Fn(String) + Send + Sync>>,
}

impl EditableTextView {
    /// Create a new editable text view
    pub fn new(view: TextView, mode: EditMode) -> Self {
        Self {
            view,
            input: InputHandler::new(),
            mode,
            show_cursor: true,
            on_submit: None,
        }
    }

    /// Create a single-line input
    pub fn single_line(viewport: Viewport) -> Self {
        Self::new(
            TextView::empty(viewport).with_padding(8.0), // Default padding for input fields
            EditMode::SingleLine,
        )
    }

    /// Create a multi-line editor
    pub fn multi_line(viewport: Viewport) -> Self {
        Self::new(TextView::empty(viewport), EditMode::MultiLine)
    }

    /// Create a read-only view with selection
    pub fn read_only(viewport: Viewport, allow_selection: bool) -> Self {
        Self::new(
            TextView::empty(viewport),
            EditMode::ReadOnly { allow_selection },
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

    /// Handle character input
    pub fn handle_char(&mut self, ch: char) -> bool {
        match self.mode {
            EditMode::SingleLine => {
                // Single line: ignore newlines, submit on Enter
                if ch == '\n' || ch == '\r' {
                    return self.handle_submit();
                }
                self.insert_char(ch);
                true
            }
            EditMode::MultiLine => {
                self.insert_char(ch);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Insert a character at cursor
    fn insert_char(&mut self, ch: char) {
        let text = ch.to_string();
        self.input.insert_text(&self.view.doc, &text);
    }

    /// Handle backspace
    pub fn handle_backspace(&mut self) -> bool {
        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.delete_at_cursor(&self.view.doc, false);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Handle delete
    pub fn handle_delete(&mut self) -> bool {
        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.delete_at_cursor(&self.view.doc, true);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Handle submit (Enter in single-line mode)
    pub fn handle_submit(&mut self) -> bool {
        if let EditMode::SingleLine = self.mode {
            if let Some(callback) = &self.on_submit {
                let text = self.view.text();
                callback(text.as_ref().clone());
            }
            true
        } else {
            false
        }
    }

    /// Handle arrow keys
    pub fn handle_arrow(&mut self, direction: ArrowDirection, shift: bool) -> bool {
        // Create a minimal viewport for InputHandler (just metrics, no positioning)
        let minimal_viewport = self.create_minimal_viewport();

        let (dx, dy) = match direction {
            ArrowDirection::Up => (0, -1),
            ArrowDirection::Down => (0, 1),
            ArrowDirection::Left => (-1, 0),
            ArrowDirection::Right => (1, 0),
        };

        self.input.move_cursor(&self.view.doc, dx, dy, shift);
        true
    }

    /// Handle mouse click
    /// Transforms screen position → local position → document position
    pub fn handle_click(&mut self, screen_pos: Point, shift: bool, alt: bool) -> bool {
        match self.mode {
            EditMode::ReadOnly {
                allow_selection: false,
            } => false,
            _ => {
                // Transform: screen → local → doc
                let local_pos = self.screen_to_local(screen_pos);

                // Create minimal viewport for InputHandler
                let minimal_viewport = self.create_minimal_viewport();

                self.input.on_mouse_click(
                    &self.view.doc,
                    &minimal_viewport,
                    local_pos,
                    crate::input_types::MouseButton::Left,
                    alt,
                    shift,
                );
                true
            }
        }
    }

    /// Handle mouse drag
    pub fn handle_drag(&mut self, from: Point, to: Point, alt: bool) -> bool {
        match self.mode {
            EditMode::ReadOnly {
                allow_selection: false,
            } => false,
            _ => {
                // Transform: screen → local
                let local_from = self.screen_to_local(from);
                let local_to = self.screen_to_local(to);

                // Create minimal viewport for InputHandler
                let minimal_viewport = self.create_minimal_viewport();

                self.input.on_mouse_drag(
                    &self.view.doc,
                    &minimal_viewport,
                    local_from,
                    local_to,
                    alt,
                );
                true
            }
        }
    }

    /// Transform screen position to local (document-relative) position
    /// screen → local: subtract bounds offset, add scroll
    fn screen_to_local(&self, screen_pos: Point) -> Point {
        Point {
            x: tiny_sdk::LogicalPixels(
                screen_pos.x.0 - self.view.viewport.bounds.x.0 + self.view.viewport.scroll.x.0,
            ),
            y: tiny_sdk::LogicalPixels(
                screen_pos.y.0 - self.view.viewport.bounds.y.0 + self.view.viewport.scroll.y.0,
            ),
        }
    }

    /// Create a minimal viewport for InputHandler (just metrics, no positioning)
    /// InputHandler only needs metrics, not bounds/scroll (we handle coordinate transform)
    fn create_minimal_viewport(&self) -> Viewport {
        let mut minimal = self.view.viewport.clone();
        minimal.bounds = tiny_sdk::types::LayoutRect::new(
            0.0,
            0.0,
            self.view.viewport.bounds.width.0,
            self.view.viewport.bounds.height.0,
        );
        minimal.scroll = LayoutPos::new(0.0, 0.0);
        minimal
    }

    /// Handle undo
    pub fn handle_undo(&mut self) -> bool {
        self.input.undo(&self.view.doc)
    }

    /// Handle redo
    pub fn handle_redo(&mut self) -> bool {
        self.input.redo(&self.view.doc)
    }

    /// Handle copy
    pub fn handle_copy(&mut self) {
        self.input.copy(&self.view.doc);
    }

    /// Handle cut
    pub fn handle_cut(&mut self) {
        self.input.cut(&self.view.doc);
    }

    /// Handle paste
    pub fn handle_paste(&mut self) {
        self.input.paste(&self.view.doc);
    }

    /// Handle select all
    pub fn handle_select_all(&mut self) {
        self.input.select_all(&self.view.doc);
    }

    /// Get cursor position in screen coordinates
    pub fn cursor_screen_pos(&self) -> Option<Point> {
        let cursor_doc = self.input.primary_cursor_doc_pos(&self.view.doc);

        // Transform: doc → canonical layout → local → screen (with padding)
        let canonical_pos = self.view.viewport.doc_to_layout(cursor_doc);
        let local_x = canonical_pos.x.0 - self.view.viewport.scroll.x.0;
        let local_y = canonical_pos.y.0 - self.view.viewport.scroll.y.0;
        let screen_x = self.view.viewport.bounds.x.0 + self.view.padding + local_x;
        let screen_y = self.view.viewport.bounds.y.0 + self.view.padding + local_y;

        Some(Point {
            x: tiny_sdk::LogicalPixels(screen_x),
            y: tiny_sdk::LogicalPixels(screen_y),
        })
    }

    /// Get selections
    pub fn selections(&self) -> &[Selection] {
        self.input.selections()
    }

    /// Set focus state (controls cursor visibility)
    pub fn set_focused(&mut self, focused: bool) {
        self.show_cursor = focused;
    }

    /// Check if focused
    pub fn is_focused(&self) -> bool {
        self.show_cursor
    }

    /// Get current text
    pub fn text(&self) -> std::sync::Arc<String> {
        self.view.text()
    }

    /// Set text (replaces all content)
    pub fn set_text(&mut self, text: &str) {
        self.view.set_text(text);
        self.input = InputHandler::new(); // Reset cursor
    }

    /// Clear text
    pub fn clear(&mut self) {
        self.view.clear();
        self.input = InputHandler::new(); // Reset cursor
    }

    /// Get line count
    pub fn line_count(&self) -> usize {
        self.view.line_count()
    }

    /// Flush pending edits to document
    pub fn flush_edits(&mut self) {
        self.input.flush_pending_edits(&self.view.doc);
    }

    /// Collect background rects (selections, highlights)
    pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        use tiny_sdk::types::RectInstance;
        let mut rects = self.view.collect_background_rects();

        if !self.view.visible {
            return rects;
        }

        // Add selection rectangles
        for sel in self.selections() {
            if !sel.is_cursor() {
                let sel_rects = sel.to_rectangles(&self.view.doc, &self.view.viewport);
                for rect in sel_rects {
                    // Transform to screen coordinates (logical) with padding
                    let screen_x = self.view.viewport.bounds.x.0 + self.view.padding + rect.x.0
                        - self.view.viewport.scroll.x.0;
                    let screen_y = self.view.viewport.bounds.y.0 + self.view.padding + rect.y.0
                        - self.view.viewport.scroll.y.0;

                    // Convert to physical pixels
                    let physical_rect = tiny_core::tree::Rect {
                        x: tiny_sdk::LogicalPixels(screen_x * self.view.viewport.scale_factor),
                        y: tiny_sdk::LogicalPixels(screen_y * self.view.viewport.scale_factor),
                        width: tiny_sdk::LogicalPixels(
                            rect.width.0 * self.view.viewport.scale_factor,
                        ),
                        height: tiny_sdk::LogicalPixels(
                            rect.height.0 * self.view.viewport.scale_factor,
                        ),
                    };

                    rects.push(RectInstance {
                        rect: physical_rect,
                        color: 0x40404080, // Semi-transparent gray for selection (RGBA)
                    });
                }
            }
        }

        rects
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

impl Default for EditableTextView {
    fn default() -> Self {
        let viewport = Viewport::new(800.0, 600.0, 1.0);
        Self::multi_line(viewport)
    }
}
