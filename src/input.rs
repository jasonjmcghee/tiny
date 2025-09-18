//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::coordinates::{DocPos, LayoutPos, LayoutRect, Viewport};
use crate::syntax::SyntaxHighlighter;
use crate::tree::{Content, Doc, Edit, Point};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;
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
        if self.cursor.line < self.anchor.line
            || (self.cursor.line == self.anchor.line && self.cursor.column <= self.anchor.column)
        {
            self.cursor
        } else {
            self.anchor
        }
    }

    /// Generate rectangles for this selection (1-3 rectangles for multi-line)
    pub fn to_rectangles(&self, doc: &Doc, viewport: &Viewport) -> Vec<LayoutRect> {
        if self.is_cursor() {
            return Vec::new();
        }

        let tree = doc.read();
        let mut rectangles = Vec::new();

        // Get start and end positions, ensuring start comes before end
        let (start, end) = if self.cursor.line < self.anchor.line
            || (self.cursor.line == self.anchor.line && self.cursor.column < self.anchor.column)
        {
            (self.cursor, self.anchor)
        } else {
            (self.anchor, self.cursor)
        };

        let line_height = viewport.metrics.line_height;

        if start.line == end.line {
            // Single line selection - one rectangle
            let start_layout = viewport.doc_to_layout(start);
            let end_layout = viewport.doc_to_layout(end);

            // Add dummy rectangle for GPU rendering bug workaround
            rectangles.push(LayoutRect::new(0.0, 0.0, 0.0, 0.0));

            rectangles.push(LayoutRect::new(
                start_layout.x.0 - 2.0, // Align with cursor positioning (-2px shift)
                start_layout.y.0,
                end_layout.x.0 - (start_layout.x.0), // Width from shifted start to end
                line_height,
            ));
        } else {
            // Multi-line selection
            let start_layout = viewport.doc_to_layout(start);
            let end_layout = viewport.doc_to_layout(end);


            // First line: from start position to right edge of viewport
            let viewport_right = viewport.logical_size.width.0 - viewport.margin.x.0;
            let first_line_width = (viewport_right - start_layout.x.0).max(0.0);

            let first_rect = LayoutRect::new(
                start_layout.x.0 - 2.0, // Align with cursor positioning (-2px shift)
                start_layout.y.0,
                first_line_width + 2.0, // Extend width because we shifted start left by 2px
                line_height,
            );
            // Try pushing rectangles in reverse order to see if it's an ordering issue
            let mut temp_rects = Vec::new();
            temp_rects.push(first_rect);

            // Middle lines: full width rectangles (if any)
            if end.line > start.line + 1 {
                let middle_start_y = start_layout.y.0 + line_height;
                let middle_height = (end.line - start.line - 1) as f32 * line_height;
                let middle_rect = LayoutRect::new(
                    viewport.margin.x.0,
                    middle_start_y,
                    viewport.logical_size.width.0 - (viewport.margin.x.0 * 2.0),
                    middle_height,
                );
                temp_rects.push(middle_rect);
            }

            // Last line: from start of line to end position
            let last_rect = LayoutRect::new(
                viewport.margin.x.0,
                end_layout.y.0,
                end_layout.x.0 - viewport.margin.x.0 - 2.0,
                line_height,
            );
            temp_rects.push(last_rect);

            // Add a dummy 0-size rectangle at the beginning as a sacrificial first rect
            rectangles.push(LayoutRect::new(0.0, 0.0, 0.0, 0.0));

            // Now push the real rectangles in normal order
            for rect in temp_rects {
                rectangles.push(rect);
            }
        }

        rectangles
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
    /// Goal column for vertical navigation (None means use current column)
    goal_column: Option<u32>,
    /// Pending edits that haven't been flushed yet
    pending_edits: Vec<Edit>,
    /// Time of last edit for debouncing
    last_edit_time: Option<Instant>,
    /// Syntax highlighter reference for InputEdit coordination
    syntax_highlighter: Option<Arc<SyntaxHighlighter>>,
    /// Syntax color at cursor when typing started (for color extension)
    typing_color: Option<u32>,
    /// Accumulated TextEdits for tree-sitter (sent on debounce)
    pending_text_edits: Vec<crate::syntax::TextEdit>,
    /// Whether we have unflushed syntax updates
    has_pending_syntax_update: bool,
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
            goal_column: None,
            pending_edits: Vec::new(),
            last_edit_time: None,
            syntax_highlighter: None,
            typing_color: None,
            pending_text_edits: Vec::new(),
            has_pending_syntax_update: false,
        }
    }

    /// Set the syntax highlighter for InputEdit coordination
    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
    }

    /// Check if we should send syntax updates
    pub fn should_flush(&self) -> bool {
        if !self.has_pending_syntax_update {
            return false;
        }

        // Send syntax update after 100ms of no typing
        if let Some(last_time) = self.last_edit_time {
            last_time.elapsed().as_millis() > 100
        } else {
            false
        }
    }

    /// Send accumulated syntax updates to tree-sitter
    pub fn flush_syntax_updates(&mut self, doc: &Doc) {
        if !self.has_pending_syntax_update || self.pending_text_edits.is_empty() {
            return;
        }

        println!(
            "SYNTAX_FLUSH: Sending {} accumulated TextEdits to tree-sitter",
            self.pending_text_edits.len()
        );

        if let Some(ref syntax_hl) = self.syntax_highlighter {
            let text_after = doc.read().flatten_to_string();

            // Send the first edit (TODO: properly batch multiple edits)
            if let Some(first_edit) = self.pending_text_edits.first() {
                println!(
                    "SYNTAX_FLUSH: InputEdit - start_byte={}, old_end={}, new_end={}",
                    first_edit.start_byte, first_edit.old_end_byte, first_edit.new_end_byte
                );
                syntax_hl.request_update_with_edit(
                    &text_after,
                    doc.version(),
                    Some(first_edit.clone()),
                );
            }
        }

        // Clear the pending syntax updates
        self.pending_text_edits.clear();
        self.has_pending_syntax_update = false;
        self.last_edit_time = None;
    }

    /// Flush pending edits to document immediately (for visibility)
    /// but DON'T update syntax yet - keep visual consistency
    pub fn flush_pending_edits(&mut self, doc: &Doc) -> bool {
        if self.pending_edits.is_empty() {
            return false;
        }

        println!(
            "FLUSH: Applying {} pending edits to document",
            self.pending_edits.len()
        );

        // Capture tree state BEFORE applying edits
        let tree_before = doc.read();

        // Collect TextEdits for LATER syntax update
        for edit in &self.pending_edits {
            if self.syntax_highlighter.is_some() {
                let text_edit = crate::syntax::create_text_edit(&tree_before, edit);
                self.pending_text_edits.push(text_edit);
                self.has_pending_syntax_update = true;
            }
        }

        // Apply all pending edits
        for edit in self.pending_edits.drain(..) {
            doc.edit(edit);
        }

        // Flush document to create new tree snapshot
        doc.flush();

        // DON'T update syntax immediately - wait for debounce
        // This keeps visual consistency while typing

        // Update metadata
        self.last_edit_time = Some(Instant::now());

        // Return true to indicate redraw needed
        true
    }

    /// Get what edits would be applied for a key event (without applying them)
    pub fn get_pending_edits(
        &self,
        doc: &Doc,
        _viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
    ) -> Vec<crate::tree::Edit> {
        use crate::tree::{Content, Edit};

        if event.state != ElementState::Pressed {
            return Vec::new();
        }

        let mut edits = Vec::new();
        println!(
            "INPUT: get_pending_edits called for key: {:?}",
            event.logical_key
        );

        match &event.logical_key {
            Key::Character(ch) => {
                // Only count printable characters as text modifications
                if ch.chars().all(|c| !c.is_control()) {
                    // Preview edits for typing character at all cursors
                    for sel in &self.selections {
                        if !sel.is_cursor() {
                            // Would delete selection first
                            edits.push(Edit::Delete {
                                range: sel.byte_range(doc),
                            });
                        }
                        // Would insert character
                        let tree = doc.read();
                        let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                        edits.push(Edit::Insert {
                            pos: insert_pos,
                            content: Content::Text(ch.to_string()),
                        });
                    }
                }
            }
            Key::Named(NamedKey::Backspace) => {
                // Preview backspace edits
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else if sel.cursor.column > 0 || sel.cursor.line > 0 {
                        // Would delete previous character
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte > 0 {
                            edits.push(Edit::Delete {
                                range: (cursor_byte - 1)..cursor_byte,
                            });
                        }
                    }
                }
            }
            Key::Named(NamedKey::Delete) => {
                // Preview delete edits
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte < tree.byte_count() {
                            edits.push(Edit::Delete {
                                range: cursor_byte..(cursor_byte + 1),
                            });
                        }
                    }
                }
            }
            _ => {
                // Other keys don't produce edits we can preview
            }
        }

        edits
    }

    /// Handle keyboard input
    pub fn on_key(
        &mut self,
        doc: &Doc,
        _viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
    ) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }

        let shift_held = modifiers.state().shift_key();

        match &event.logical_key {
            Key::Character(ch) => {
                // Type character at all cursors
                self.goal_column = None; // Reset goal column when typing

                // Prepare edits
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        // Delete selection first
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    }
                    // Convert cursor position to byte offset for insertion
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    self.pending_edits.push(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text(ch.to_string()),
                    });
                }

                // Flush immediately WITH syntax coordination
                let _needs_redraw = self.flush_pending_edits(doc);

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

                // Return true to trigger redraw
                return true;
            }
            Key::Named(NamedKey::Backspace) => {
                // Delete before cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else if sel.cursor.line > 0 || sel.cursor.column > 0 {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte > 0 {
                            self.pending_edits.push(Edit::Delete {
                                range: cursor_byte - 1..cursor_byte,
                            });
                        }
                    }
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);

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
                            let line_end = tree
                                .line_to_byte(sel.cursor.line + 1)
                                .unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            // Strip trailing newline before counting characters
                            let line_text_trimmed = line_text.trim_end_matches('\n');
                            sel.cursor.column = line_text_trimmed.chars().count() as u32;
                        }
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Delete) => {
                // Delete after cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        let text_len = tree.byte_count();
                        if cursor_byte < text_len {
                            self.pending_edits.push(Edit::Delete {
                                range: cursor_byte..cursor_byte + 1,
                            });
                        }
                    }
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);
            }
            Key::Named(NamedKey::ArrowLeft) => {
                // Move left in document space
                self.goal_column = None; // Reset goal column for horizontal movement
                for sel in &mut self.selections {
                    if sel.cursor.column > 0 {
                        sel.cursor.column -= 1;
                    } else if sel.cursor.line > 0 {
                        // Move to end of previous line
                        sel.cursor.line -= 1;
                        let tree = doc.read();
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree
                                .line_to_byte(sel.cursor.line + 1)
                                .unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            // Strip trailing newline before counting characters
                            let line_text_trimmed = line_text.trim_end_matches('\n');
                            sel.cursor.column = line_text_trimmed.chars().count() as u32;
                        }
                    }
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowRight) => {
                // Move right in document space
                self.goal_column = None; // Reset goal column for horizontal movement
                for sel in &mut self.selections {
                    let tree = doc.read();
                    // Get current line info
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree
                            .line_to_byte(sel.cursor.line + 1)
                            .unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        // Strip trailing newline before counting characters
                        let line_text_trimmed = line_text.trim_end_matches('\n');
                        let line_length = line_text_trimmed.chars().count() as u32;

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
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                // Move up in document space
                // Set goal column if not already set
                if self.goal_column.is_none() && !self.selections.is_empty() {
                    self.goal_column = Some(self.selections[0].cursor.column);
                }

                for sel in &mut self.selections {
                    if sel.cursor.line > 0 {
                        sel.cursor.line -= 1;
                        // Use goal column but clamp to line length
                        let tree = doc.read();
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree
                                .line_to_byte(sel.cursor.line + 1)
                                .unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            // Strip trailing newline before counting characters
                            let line_text_trimmed = line_text.trim_end_matches('\n');
                            let line_length = line_text_trimmed.chars().count() as u32;
                            // Use goal column if set, otherwise current column
                            let target_column = self.goal_column.unwrap_or(sel.cursor.column);
                            sel.cursor.column = target_column.min(line_length);
                        }
                    }
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                // Move down in document space
                // Set goal column if not already set
                if self.goal_column.is_none() && !self.selections.is_empty() {
                    self.goal_column = Some(self.selections[0].cursor.column);
                }

                for sel in &mut self.selections {
                    let tree = doc.read();
                    println!(
                        "ARROW_DOWN: current_line={}, total_lines={}",
                        sel.cursor.line,
                        tree.line_count()
                    );
                    // Check if next line exists
                    if tree.line_to_byte(sel.cursor.line + 1).is_some() {
                        sel.cursor.line += 1;
                        println!("  -> moved to line {}", sel.cursor.line);
                        // Use goal column but clamp to line length
                        if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                            let line_end = tree
                                .line_to_byte(sel.cursor.line + 1)
                                .unwrap_or(tree.byte_count());
                            let line_text = tree.get_text_slice(line_start..line_end);
                            // Strip trailing newline before counting characters
                            let line_text_trimmed = line_text.trim_end_matches('\n');
                            let line_length = line_text_trimmed.chars().count() as u32;
                            // Use goal column if set, otherwise current column
                            let target_column = self.goal_column.unwrap_or(sel.cursor.column);
                            sel.cursor.column = target_column.min(line_length);
                        }
                    }
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Home) => {
                // Move to line start
                self.goal_column = None; // Reset goal column
                for sel in &mut self.selections {
                    sel.cursor.column = 0;
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::End) => {
                // Move to line end
                self.goal_column = None; // Reset goal column
                for sel in &mut self.selections {
                    let tree = doc.read();
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree
                            .line_to_byte(sel.cursor.line + 1)
                            .unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        sel.cursor.column = line_text.chars().count() as u32;
                    }
                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Enter) => {
                // Insert newline
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    self.pending_edits.push(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text("\n".to_string()),
                    });
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);

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
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    self.pending_edits.push(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text("\t".to_string()),
                    });
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);

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
                // Set goal column if not already set
                if self.goal_column.is_none() && !self.selections.is_empty() {
                    self.goal_column = Some(self.selections[0].cursor.column);
                }

                for sel in &mut self.selections {
                    // Move up approximately 20 lines (would use viewport)
                    sel.cursor.line = sel.cursor.line.saturating_sub(20);

                    // Use goal column but clamp to line length
                    let tree = doc.read();
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree
                            .line_to_byte(sel.cursor.line + 1)
                            .unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        let line_length = line_text.chars().count() as u32;
                        let target_column = self.goal_column.unwrap_or(sel.cursor.column);
                        sel.cursor.column = target_column.min(line_length);
                    } else {
                        sel.cursor.column = 0;
                    }

                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::PageDown) => {
                // Move cursor down by viewport height worth of lines
                // Set goal column if not already set
                if self.goal_column.is_none() && !self.selections.is_empty() {
                    self.goal_column = Some(self.selections[0].cursor.column);
                }

                for sel in &mut self.selections {
                    let tree = doc.read();
                    let total_lines = tree.line_count();
                    // Move down approximately 20 lines (would use viewport)
                    sel.cursor.line = (sel.cursor.line + 20).min(total_lines.saturating_sub(1));

                    // Use goal column but clamp to line length
                    if let Some(line_start) = tree.line_to_byte(sel.cursor.line) {
                        let line_end = tree
                            .line_to_byte(sel.cursor.line + 1)
                            .unwrap_or(tree.byte_count());
                        let line_text = tree.get_text_slice(line_start..line_end);
                        let line_length = line_text.chars().count() as u32;
                        let target_column = self.goal_column.unwrap_or(sel.cursor.column);
                        sel.cursor.column = target_column.min(line_length);
                    } else {
                        sel.cursor.column = 0;
                    }

                    if !shift_held {
                        sel.anchor = sel.cursor;
                    }
                }
            }
            Key::Named(NamedKey::Space) => {
                // Handle space explicitly since it's not in Key::Character
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    }
                    let tree = doc.read();
                    let insert_pos = tree.doc_pos_to_byte(sel.min_pos());
                    self.pending_edits.push(Edit::Insert {
                        pos: insert_pos,
                        content: Content::Text(" ".to_string()),
                    });
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);

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

        // Return true to indicate potential scrolling needed
        true
    }

    /// Handle mouse click
    pub fn on_mouse_click(
        &mut self,
        doc: &Doc,
        viewport: &Viewport,
        pos: Point,
        button: MouseButton,
        alt_held: bool,
    ) -> bool {
        if button != MouseButton::Left {
            return false;
        }

        // Flush any pending syntax updates before handling click
        self.flush_syntax_updates(doc);

        // Reset goal column when clicking
        self.goal_column = None;

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

        true
    }

    /// Handle mouse drag
    pub fn on_mouse_drag(
        &mut self,
        doc: &Doc,
        viewport: &Viewport,
        from: Point,
        to: Point,
        alt_held: bool,
    ) -> bool {
        // Flush any pending syntax updates before handling drag
        self.flush_syntax_updates(doc);
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

        true
    }

    /// Update selection widgets in tree
    fn update_selection_widgets(&self, _doc: &Doc) {
        // No-op: selections are now rendered as overlays, not document content
    }

    /// Copy selection to clipboard
    pub fn copy(&mut self, doc: &Doc) {
        // Flush any pending syntax updates first
        self.flush_syntax_updates(doc);
        if let Some(sel) = self.selections.first() {
            if !sel.is_cursor() {
                let text = doc.read().flatten_to_string();
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
        // Flush any pending syntax updates first
        self.flush_syntax_updates(doc);

        self.copy(doc);

        // Delete selection
        for sel in &self.selections {
            if !sel.is_cursor() {
                doc.edit(Edit::Delete {
                    range: sel.byte_range(doc),
                });
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
        // Flush any pending syntax updates first
        self.flush_syntax_updates(doc);

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
                    doc.edit(Edit::Delete {
                        range: sel.byte_range(doc),
                    });
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
        // Flush any pending syntax updates first
        self.flush_syntax_updates(doc);
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

    /// Create widget instances for current selections and cursor
    pub fn create_widgets(
        &self,
        doc: &Doc,
        viewport: &Viewport,
    ) -> (Vec<Arc<dyn crate::widget::Widget>>, Option<Arc<dyn crate::widget::Widget>>) {
        use crate::widget;

        let mut selection_widgets = Vec::new();
        let mut cursor_widget = None;

        for (i, selection) in self.selections.iter().enumerate() {
            // Always create cursor at the active end of selection (cursor position)
            // Only create it for the primary selection (index 0)
            if i == 0 {
                let tree = doc.read();
                // Get the line text for accurate cursor positioning
                let line_text = if let Some(line_start) = tree.line_to_byte(selection.cursor.line)
                {
                    let line_end = tree
                        .line_to_byte(selection.cursor.line + 1)
                        .unwrap_or(tree.byte_count());
                    tree.get_text_slice(line_start..line_end)
                } else {
                    String::new()
                };

                // Use accurate text-based positioning
                let layout_pos = viewport.doc_to_layout_with_text(selection.cursor, &line_text);

                // Position cursor directly at calculated position
                cursor_widget = Some(widget::cursor(layout_pos));
            }

            // Create selection widget if it's not just a cursor
            if !selection.is_cursor() {
                // Create selection widget with 1-3 rectangles
                let rectangles = selection.to_rectangles(doc, viewport);
                if !rectangles.is_empty() {
                    selection_widgets.push(widget::selection(rectangles));
                }
            }
        }

        (selection_widgets, cursor_widget)
    }
}
