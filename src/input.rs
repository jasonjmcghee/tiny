//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::tree::{Content, Doc, Edit, Point};
use crate::widget;
use crate::coordinates::{DocPos, Viewport};
use std::ops::Range;
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key, NamedKey};

/// Selection with cursor and anchor in document coordinates
#[derive(Clone)]
pub struct Selection {
    /// Cursor position (where we are) in document space
    pub cursor: DocPos,
    /// Anchor position (where we started) in document space
    pub anchor: DocPos,
    /// Unique ID
    pub id: u32,
}

impl Selection {
    /// Get selection as byte range (requires document access)
    pub fn byte_range(&self, doc: &Doc) -> Range<usize> {
        let tree = doc.read();
        let cursor_byte = tree.doc_pos_to_byte(self.cursor);
        let anchor_byte = tree.doc_pos_to_byte(self.anchor);

        if cursor_byte <= anchor_byte {
            cursor_byte..anchor_byte
        } else {
            anchor_byte..cursor_byte
        }
    }

    /// Check if this is just a cursor (no selection)
    pub fn is_cursor(&self) -> bool {
        self.cursor == self.anchor
    }

    /// Get the minimum document position between cursor and anchor
    pub fn min_pos(&self) -> DocPos {
        if self.cursor.line < self.anchor.line ||
           (self.cursor.line == self.anchor.line && self.cursor.column <= self.anchor.column) {
            self.cursor
        } else {
            self.anchor
        }
    }

}

/// Input handler with multi-cursor support
pub struct InputHandler {
    /// All active selections
    selections: Vec<Selection>,
    /// Next selection ID
    next_id: u32,
    /// Clipboard contents
    clipboard: Option<String>,
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            selections: vec![Selection {
                cursor: DocPos::default(),
                anchor: DocPos::default(),
                id: 0,
            }],
            next_id: 1,
            clipboard: None,
        }
    }

    /// Handle keyboard input
    pub fn on_key(&mut self, doc: &Doc, _viewport: &Viewport, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }

        match &event.logical_key {
            Key::Character(ch) => {
                // Type character at all cursors
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        // Delete selection first
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    }
                    // Convert cursor position to byte offset for insertion
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    doc.edit(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text(ch.to_string()),
                    });
                }
                doc.flush();

                // Advance cursors in document space
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        // Collapse selection to minimum position
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    }
                    // Move cursor forward by one character in document space
                    sel.cursor.column += 1;
                    sel.anchor = sel.cursor;
                }
            }
            Key::Named(NamedKey::Backspace) => {
                // Delete before cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    } else if sel.cursor.line > 0 || sel.cursor.column > 0 {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte > 0 {
                            doc.edit(Edit::Delete {
                                range: cursor_byte - 1..cursor_byte,
                            });
                        }
                    }
                }
                doc.flush();

                // Move cursors back
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    } else if sel.cursor.column > 0 {
                        sel.cursor.column -= 1;
                        sel.anchor = sel.cursor;
                    } else if sel.cursor.line > 0 {
                        // Move to end of previous line
                        sel.cursor.line -= 1;
                        let tree = doc.read();
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            sel.cursor.column = line_text.chars().count() as u32;
                        }
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Delete) => {
                // Delete after cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    } else {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        let text_len = tree.byte_count();
                        if cursor_byte < text_len {
                            doc.edit(Edit::Delete {
                                range: cursor_byte..cursor_byte + 1,
                            });
                        }
                    }
                }
                doc.flush();
            }
            Key::Named(NamedKey::ArrowLeft) => {
                // Move left in document space
                for sel in &mut self.selections {
                    if sel.cursor.column > 0 {
                        sel.cursor.column -= 1;
                    } else if sel.cursor.line > 0 {
                        // Move to end of previous line
                        sel.cursor.line -= 1;
                        let tree = doc.read();
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            sel.cursor.column = line_text.chars().count() as u32;
                        }
                    }
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                // Move right in document space
                for sel in &mut self.selections {
                    let tree = doc.read();
                    // Get current line info
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        let line_length = line_text.chars().count() as u32;

                        if sel.cursor.column < line_length {
                            sel.cursor.column += 1;
                        } else {
                            // Move to start of next line
                            sel.cursor.line += 1;
                            sel.cursor.column = 0;
                            // Check if next line exists
                            if tree.line_to_byte(sel.cursor.line).is_none() {
                                // No next line, stay at end of current line
                                sel.cursor.line -= 1;
                                sel.cursor.column = line_length;
                            }
                        }
                    }
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                // Move up in document space
                for sel in &mut self.selections {
                    if sel.cursor.line > 0 {
                        sel.cursor.line -= 1;
                        // Keep same column, but clamp to line length
                        let tree = doc.read();
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            let line_length = line_text.chars().count() as u32;
                            sel.cursor.column = sel.cursor.column.min(line_length);
                        }
                    }
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                // Move down in document space
                for sel in &mut self.selections {
                    let tree = doc.read();
                    // Check if next line exists
                    if tree.line_to_byte(sel.cursor.line + 1).is_some() {
                        sel.cursor.line += 1;
                        // Keep same column, but clamp to line length
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            let line_length = line_text.chars().count() as u32;
                            sel.cursor.column = sel.cursor.column.min(line_length);
                        }
                    }
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Home) => {
                // Move to line start
                for sel in &mut self.selections {
                    sel.cursor.column = 0;
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::End) => {
                // Move to line end
                for sel in &mut self.selections {
                    let tree = doc.read();
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        sel.cursor.column = line_text.chars().count() as u32;
                    }
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Enter) => {
                // Insert newline
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    doc.edit(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text("\n".to_string()),
                    });
                }
                doc.flush();

                // Advance cursors - move to next line, column 0
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    }
                    sel.cursor.line += 1;
                    sel.cursor.column = 0;
                    sel.anchor = sel.cursor;
                }
            }
            Key::Named(NamedKey::Tab) => {
                // Insert tab character
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    doc.edit(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text("\t".to_string()),
                    });
                }
                doc.flush();

                // Advance cursors - tab moves to next tab stop
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    }
                    // Tab advances to next tab stop (4 spaces)
                    sel.cursor.column = ((sel.cursor.column / 4) + 1) * 4;
                    sel.anchor = sel.cursor;
                }
            }
            Key::Named(NamedKey::PageUp) => {
                // Move cursor up by viewport height worth of lines
                // This is simplified - would use actual viewport metrics
                for sel in &mut self.selections {
                    // Move up approximately 20 lines (would use viewport)
                    sel.cursor.line = sel.cursor.line.saturating_sub(20);

                    // Clamp column to line length
                    let tree = doc.read();
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        let line_length = line_text.chars().count() as u32;
                        sel.cursor.column = sel.cursor.column.min(line_length);
                    } else {
                        sel.cursor.column = 0;
                    }

                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::PageDown) => {
                // Move cursor down by viewport height worth of lines
                for sel in &mut self.selections {
                    let tree = doc.read();
                    let total_lines = tree.line_count();
                    // Move down approximately 20 lines (would use viewport)
                    sel.cursor.line = (sel.cursor.line + 20).min(total_lines.saturating_sub(1));

                    // Clamp column to line length
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree.line_to_byte(sel.cursor.line + 1).unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        let line_length = line_text.chars().count() as u32;
                        sel.cursor.column = sel.cursor.column.min(line_length);
                    } else {
                        sel.cursor.column = 0;
                    }

                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Space) => {
                // Handle space explicitly since it's not in Key::Character
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    doc.edit(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text(" ".to_string()),
                    });
                }
                doc.flush();

                // Advance cursors
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    }
                    sel.cursor.column += 1;
                    sel.anchor = sel.cursor;
                }
            }
            _ => {}
        }

        // Update selection widgets in tree
        self.update_selection_widgets(doc);

        // Return true to indicate potential scrolling needed
        true
    }

    /// Handle mouse click
    pub fn on_mouse_click(&mut self, doc: &Doc, viewport: &Viewport, pos: Point, button: MouseButton, alt_held: bool) -> bool {
        if button != MouseButton::Left {
            return false;
        }

        // Convert click position to document coordinates using viewport and tree
        // The pos is already window-relative logical pixels, so we need to add scroll offset
        let layout_pos = crate::coordinates::LayoutPos {
            x: pos.x + viewport.scroll.x,
            y: pos.y + viewport.scroll.y,
        };
        let tree = doc.read();
        let doc_pos = viewport.layout_to_doc_with_tree(layout_pos, &tree);

        if alt_held {
            // Alt+click adds new cursor
            self.selections.push(Selection {
                cursor: doc_pos,
                anchor: doc_pos,
                id: self.next_id,
            });
            self.next_id += 1;
        } else {
            // Regular click sets single cursor
            self.selections.clear();
            self.selections.push(Selection {
                cursor: doc_pos,
                anchor: doc_pos,
                id: self.next_id,
            });
            self.next_id += 1;
        }

        self.update_selection_widgets(doc);
        true
    }

    /// Handle mouse drag
    pub fn on_mouse_drag(&mut self, doc: &Doc, viewport: &Viewport, from: Point, to: Point, alt_held: bool) -> bool {
        // Convert positions to document coordinates using tree-based hit testing
        // The positions are already window-relative logical pixels, so we add scroll offset
        let start_layout = crate::coordinates::LayoutPos {
            x: from.x + viewport.scroll.x,
            y: from.y + viewport.scroll.y,
        };
        let end_layout = crate::coordinates::LayoutPos {
            x: to.x + viewport.scroll.x,
            y: to.y + viewport.scroll.y,
        };
        let tree = doc.read();
        let start_doc = viewport.layout_to_doc_with_tree(start_layout, &tree);
        let end_doc = viewport.layout_to_doc_with_tree(end_layout, &tree);

        if alt_held {
            // Alt+drag for column selection (simplified)
            // Would create multiple cursors for each line
            self.selections.push(Selection {
                cursor: end_doc,
                anchor: start_doc,
                id: self.next_id,
            });
            self.next_id += 1;
        } else {
            // Regular drag
            self.selections.clear();
            self.selections.push(Selection {
                cursor: end_doc,
                anchor: start_doc,
                id: self.next_id,
            });
            self.next_id += 1;
        }

        self.update_selection_widgets(doc);
        true
    }

    /// Update selection widgets in tree
    fn update_selection_widgets(&self, doc: &Doc) {
        // Remove old selection widgets and add new ones
        // This is simplified - in real implementation we'd track widget IDs

        for sel in &self.selections {
            if sel.is_cursor() {
                // Insert cursor widget
                let tree = doc.read();
                let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                doc.edit(Edit::Insert {
                    pos: cursor_byte,
                    content: Content::Widget(widget::cursor()),
                });
            } else {
                // Insert selection widget
                let byte_range = sel.byte_range(doc);
                doc.edit(Edit::Insert {
                    pos: byte_range.start,
                    content: Content::Widget(widget::selection(byte_range)),
                });
            }
        }

        doc.flush();
    }


    /// Copy selection to clipboard
    pub fn copy(&mut self, doc: &Doc) {
        if let Some(sel) = self.selections.first() {
            if !sel.is_cursor() {
                let text = doc.read().to_string();
                let range = sel.byte_range(doc);
                if range.end <= text.len() {
                    let selected = &text[range];
                    self.clipboard = Some(selected.to_string());

                    // Also copy to system clipboard
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(selected);
                    }
                }
            }
        }
    }

    /// Cut selection to clipboard
    pub fn cut(&mut self, doc: &Doc) {
        self.copy(doc);

        // Delete selection
        for sel in &self.selections {
            if !sel.is_cursor() {
                doc.edit(Edit::Delete { range: sel.byte_range(doc) });
            }
        }
        doc.flush();

        // Collapse selections
        for sel in &mut self.selections {
            sel.cursor = sel.min_pos();
            sel.anchor = sel.cursor;
        }
    }

    /// Paste from clipboard
    pub fn paste(&mut self, doc: &Doc) {
        // Try system clipboard first
        let text = if let Ok(mut clipboard) = arboard::Clipboard::new() {
            clipboard.get_text().ok()
        } else {
            None
        }
        .or_else(|| self.clipboard.clone());

        if let Some(text) = text {
            for sel in &self.selections {
                if !sel.is_cursor() {
                    doc.edit(Edit::Delete { range: sel.byte_range(doc) });
                }
                let tree = doc.read();
                let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                doc.edit(Edit::Insert {
                    pos: insert_pos,
                    content: Content::Text(text.clone()),
                });
            }
            doc.flush();

            // Advance cursors by text length in columns
            let advance_chars = text.chars().count() as u32;
            for sel in &mut self.selections {
                sel.cursor = sel.min_pos();
                sel.cursor.column += advance_chars;
                sel.anchor = sel.cursor;
            }
        }
    }

    /// Select all text
    pub fn select_all(&mut self, doc: &Doc) {
        let tree = doc.read();
        let last_line = tree.line_count().saturating_sub(1);
        let last_line_start = tree.line_to_byte(last_line).unwrap_or(0);
        let last_line_end = tree.byte_count();
        let last_line_text = tree.get_text_slice(last_line_start..last_line_end);
        let last_column = last_line_text.chars().count() as u32;

        self.selections.clear();
        self.selections.push(Selection {
            cursor: DocPos {
                byte_offset: tree.byte_count(),
                line: last_line,
                column: last_column,
            },
            anchor: DocPos::default(),
            id: self.next_id,
        });
        self.next_id += 1;

        self.update_selection_widgets(doc);
    }

    /// Get current selections
    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    /// Clear all selections except primary
    pub fn clear_selections(&mut self) {
        if !self.selections.is_empty() {
            let primary = self.selections[0].clone();
            self.selections.clear();
            self.selections.push(primary);
        }
    }

    /// Get primary cursor position in document space
    pub fn primary_cursor_doc_pos(&self, doc: &Doc) -> crate::coordinates::DocPos {
        if let Some(sel) = self.selections.first() {
            // The selection already has DocPos - just return it with updated byte_offset
            let tree = doc.read();
            let byte_offset = tree.doc_pos_to_byte(sel.cursor);

            crate::coordinates::DocPos {
                byte_offset,
                line: sel.cursor.line,
                column: sel.cursor.column,
            }
        } else {
            crate::coordinates::DocPos::default()
        }
    }
}
