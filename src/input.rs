//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::tree::{Content, Doc, Edit, Point};
use crate::widget;
use std::ops::Range;
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key, NamedKey};

/// Selection with cursor and anchor
#[derive(Clone)]
pub struct Selection {
    /// Cursor position (where we are)
    pub cursor: usize,
    /// Anchor position (where we started)
    pub anchor: usize,
    /// Unique ID
    pub id: u32,
}

impl Selection {
    /// Get selection as byte range
    pub fn range(&self) -> Range<usize> {
        if self.cursor <= self.anchor {
            self.cursor..self.anchor
        } else {
            self.anchor..self.cursor
        }
    }

    /// Check if this is just a cursor (no selection)
    pub fn is_cursor(&self) -> bool {
        self.cursor == self.anchor
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
                cursor: 0,
                anchor: 0,
                id: 0,
            }],
            next_id: 1,
            clipboard: None,
        }
    }

    /// Handle keyboard input
    pub fn on_key(&mut self, doc: &Doc, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }

        match &event.logical_key {
            Key::Character(ch) => {
                // Type character at all cursors
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        // Delete selection first
                        doc.edit(Edit::Delete { range: sel.range() });
                    }
                    doc.edit(Edit::Insert {
                        pos: sel.cursor.min(sel.anchor),
                        content: Content::Text(ch.to_string()),
                    });
                }
                doc.flush();

                // Advance cursors
                for sel in &mut self.selections {
                    let advance = ch.len();
                    if !sel.is_cursor() {
                        sel.cursor = sel.cursor.min(sel.anchor) + advance;
                        sel.anchor = sel.cursor;
                    } else {
                        sel.cursor += advance;
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Backspace) => {
                // Delete before cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.range() });
                    } else if sel.cursor > 0 {
                        doc.edit(Edit::Delete {
                            range: sel.cursor - 1..sel.cursor,
                        });
                    }
                }
                doc.flush();

                // Move cursors back
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.cursor.min(sel.anchor);
                        sel.anchor = sel.cursor;
                    } else if sel.cursor > 0 {
                        sel.cursor -= 1;
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Delete) => {
                // Delete after cursor
                for sel in &self.selections {
                    let text_len = doc.read().byte_count();
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.range() });
                    } else if sel.cursor < text_len {
                        doc.edit(Edit::Delete {
                            range: sel.cursor..sel.cursor + 1,
                        });
                    }
                }
                doc.flush();
            }
            Key::Named(NamedKey::ArrowLeft) => {
                // Move left
                for sel in &mut self.selections {
                    if sel.cursor > 0 {
                        sel.cursor -= 1;
                        if !event.repeat {
                            sel.anchor = sel.cursor;
                        }
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                // Move right
                let text_len = doc.read().byte_count();
                for sel in &mut self.selections {
                    if sel.cursor < text_len {
                        sel.cursor += 1;
                        if !event.repeat {
                            sel.anchor = sel.cursor;
                        }
                    }
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                // Move up (simplified - would use layout info)
                for sel in &mut self.selections {
                    // Find previous line
                    let text = doc.read().to_string();
                    if let Some(prev_line_start) = text[..sel.cursor].rfind('\n') {
                        sel.cursor = prev_line_start;
                        if !event.repeat {
                            sel.anchor = sel.cursor;
                        }
                    }
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                // Move down
                for sel in &mut self.selections {
                    let text = doc.read().to_string();
                    if let Some(next_line_start) = text[sel.cursor..].find('\n') {
                        sel.cursor += next_line_start + 1;
                        if !event.repeat {
                            sel.anchor = sel.cursor;
                        }
                    }
                }
            }
            Key::Named(NamedKey::Home) => {
                // Move to line start
                for sel in &mut self.selections {
                    let text = doc.read().to_string();
                    let line_start = text[..sel.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
                    sel.cursor = line_start;
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::End) => {
                // Move to line end
                for sel in &mut self.selections {
                    let text = doc.read().to_string();
                    let line_end = text[sel.cursor..]
                        .find('\n')
                        .map(|i| sel.cursor + i)
                        .unwrap_or(text.len());
                    sel.cursor = line_end;
                    if !event.repeat {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Enter) => {
                // Insert newline
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        doc.edit(Edit::Delete { range: sel.range() });
                    }
                    doc.edit(Edit::Insert {
                        pos: sel.cursor.min(sel.anchor),
                        content: Content::Text("\n".to_string()),
                    });
                }
                doc.flush();

                // Advance cursors
                for sel in &mut self.selections {
                    if !sel.is_cursor() {
                        sel.cursor = sel.cursor.min(sel.anchor) + 1;
                    } else {
                        sel.cursor += 1;
                    }
                    sel.anchor = sel.cursor;
                }
            }
            _ => {}
        }

        // Update selection widgets in tree
        self.update_selection_widgets(doc);
    }

    /// Handle mouse click
    pub fn on_mouse_click(&mut self, doc: &Doc, pos: Point, button: MouseButton, alt_held: bool) {
        if button != MouseButton::Left {
            return;
        }

        // Find byte position from click point
        let tree = doc.read();
        let tree_pos = tree.find_at_point(pos);
        let byte_pos = self.tree_pos_to_byte(&tree, tree_pos);

        if alt_held {
            // Alt+click adds new cursor
            self.selections.push(Selection {
                cursor: byte_pos,
                anchor: byte_pos,
                id: self.next_id,
            });
            self.next_id += 1;
        } else {
            // Regular click sets single cursor
            self.selections.clear();
            self.selections.push(Selection {
                cursor: byte_pos,
                anchor: byte_pos,
                id: self.next_id,
            });
            self.next_id += 1;
        }

        self.update_selection_widgets(doc);
    }

    /// Handle mouse drag
    pub fn on_mouse_drag(&mut self, doc: &Doc, from: Point, to: Point, alt_held: bool) {
        let tree = doc.read();

        // Find byte positions
        let start_pos = tree.find_at_point(from);
        let end_pos = tree.find_at_point(to);
        let start_byte = self.tree_pos_to_byte(&tree, start_pos);
        let end_byte = self.tree_pos_to_byte(&tree, end_pos);

        if alt_held {
            // Alt+drag for column selection (simplified)
            // Would create multiple cursors for each line
            self.selections.push(Selection {
                cursor: end_byte,
                anchor: start_byte,
                id: self.next_id,
            });
            self.next_id += 1;
        } else {
            // Regular drag
            self.selections.clear();
            self.selections.push(Selection {
                cursor: end_byte,
                anchor: start_byte,
                id: self.next_id,
            });
            self.next_id += 1;
        }

        self.update_selection_widgets(doc);
    }

    /// Update selection widgets in tree
    fn update_selection_widgets(&self, doc: &Doc) {
        // Remove old selection widgets and add new ones
        // This is simplified - in real implementation we'd track widget IDs

        for sel in &self.selections {
            if sel.is_cursor() {
                // Insert cursor widget
                doc.edit(Edit::Insert {
                    pos: sel.cursor,
                    content: Content::Widget(widget::cursor()),
                });
            } else {
                // Insert selection widget
                doc.edit(Edit::Insert {
                    pos: sel.range().start,
                    content: Content::Widget(widget::selection(sel.range())),
                });
            }
        }

        doc.flush();
    }

    /// Convert tree position to byte offset
    fn tree_pos_to_byte(&self, _tree: &crate::tree::Tree, pos: crate::tree::TreePos) -> usize {
        // Simplified - would walk tree to calculate actual byte position
        pos.offset_in_span
    }

    /// Copy selection to clipboard
    pub fn copy(&mut self, doc: &Doc) {
        if let Some(sel) = self.selections.first() {
            if !sel.is_cursor() {
                let text = doc.read().to_string();
                let range = sel.range();
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
                doc.edit(Edit::Delete { range: sel.range() });
            }
        }
        doc.flush();

        // Collapse selections
        for sel in &mut self.selections {
            sel.cursor = sel.cursor.min(sel.anchor);
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
                    doc.edit(Edit::Delete { range: sel.range() });
                }
                doc.edit(Edit::Insert {
                    pos: sel.cursor.min(sel.anchor),
                    content: Content::Text(text.clone()),
                });
            }
            doc.flush();

            // Advance cursors
            let advance = text.len();
            for sel in &mut self.selections {
                sel.cursor = sel.cursor.min(sel.anchor) + advance;
                sel.anchor = sel.cursor;
            }
        }
    }

    /// Select all text
    pub fn select_all(&mut self, doc: &Doc) {
        let text_len = doc.read().byte_count();
        self.selections.clear();
        self.selections.push(Selection {
            cursor: text_len,
            anchor: 0,
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
}
