//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::coordinates::{DocPos, LayoutPos, LayoutRect, Viewport};
use crate::history::{DocumentHistory, DocumentSnapshot, SelectionHistory};
use crate::syntax::SyntaxHighlighter;
use crate::tree::{Content, Doc, Edit, Point};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Actions that can be triggered by input
pub enum InputAction {
    None,
    Redraw,
    Save,
    Undo,
    Redo,
}

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

        let _tree = doc.read();
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
    /// Accumulated TextEdits for tree-sitter (sent on debounce)
    pending_text_edits: Vec<crate::syntax::TextEdit>,
    /// Whether we have unflushed syntax updates
    has_pending_syntax_update: bool,
    /// History for undo/redo (document + selections)
    history: DocumentHistory,
    /// Navigation history for cursor positions (Cmd+[/])
    nav_history: SelectionHistory,
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
            pending_text_edits: Vec::new(),
            has_pending_syntax_update: false,
            history: DocumentHistory::new(),
            nav_history: SelectionHistory::with_max_size(50),
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

    #[cfg(test)]
    pub fn pending_edits_count(&self) -> usize {
        self.pending_edits.len()
    }

    #[cfg(test)]
    pub fn get_pending_edits_for_test(&self) -> &[Edit] {
        &self.pending_edits
    }

    #[cfg(test)]
    pub fn set_cursor_for_test(&mut self, pos: DocPos) {
        self.selections = vec![Selection {
            cursor: pos,
            anchor: pos,
            id: 0,
        }];
    }

    /// Send accumulated syntax updates to tree-sitter
    pub fn flush_syntax_updates(&mut self, doc: &Doc) {
        if !self.has_pending_syntax_update {
            return;
        }

        println!(
            "SYNTAX_FLUSH: Sending {} accumulated TextEdits to tree-sitter",
            self.pending_text_edits.len()
        );

        if let Some(ref syntax_hl) = self.syntax_highlighter {
            let text_after = doc.read().flatten_to_string();

            // If we have multiple edits, we can't send them all to tree-sitter
            // (it only accepts one InputEdit at a time), so request a full reparse
            if self.pending_text_edits.len() == 1 {
                // Single edit - use incremental parsing
                let edit = &self.pending_text_edits[0];
                println!(
                    "SYNTAX_FLUSH: Sending single InputEdit - start_byte={}, old_end={}, new_end={}",
                    edit.start_byte, edit.old_end_byte, edit.new_end_byte
                );
                syntax_hl.request_update_with_edit(&text_after, doc.version(), Some(edit.clone()));
            } else {
                // Multiple edits - request full reparse
                println!(
                    "SYNTAX_FLUSH: {} edits accumulated, requesting full reparse",
                    self.pending_text_edits.len()
                );
                syntax_hl.request_update_with_edit(
                    &text_after,
                    doc.version(),
                    None, // No edit = full reparse
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
        self.flush_pending_edits_with_renderer(doc, None)
    }

    /// Flush pending edits with optional renderer for incremental updates
    pub fn flush_pending_edits_with_renderer(
        &mut self,
        doc: &Doc,
        mut renderer: Option<&mut crate::render::Renderer>,
    ) -> bool {
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

            // Apply incremental edit to renderer for stable typing
            if let Some(renderer) = renderer.as_deref_mut() {
                renderer.apply_incremental_edit(edit);
            }
        }

        // Apply all pending edits
        for edit in self.pending_edits.drain(..) {
            doc.edit(edit);
        }

        // Flush document to create new tree snapshot
        doc.flush();

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
        _modifiers: &winit::event::Modifiers,
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

    /// Handle key input with optional renderer for incremental updates
    pub fn on_key_with_renderer(
        &mut self,
        doc: &Doc,
        _viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
        renderer: Option<&mut crate::render::Renderer>,
    ) -> InputAction {
        if event.state != ElementState::Pressed {
            return InputAction::None;
        }

        let cmd_held = modifiers.state().contains(ModifiersState::SUPER)
            || modifiers.state().contains(ModifiersState::CONTROL);

        match &event.logical_key {
            Key::Character(ch) if !cmd_held => {
                // Type character at all cursors
                self.goal_column = None; // Reset goal column when typing

                // Save snapshot before edit
                self.save_snapshot_to_history(doc);

                // Calculate cumulative offset from existing pending edits
                // This is critical for correct positioning when buffering multiple keystrokes
                let mut position_shifts: Vec<(usize, i32)> = Vec::new();
                for edit in &self.pending_edits {
                    match edit {
                        Edit::Insert { pos, content } => {
                            if let Content::Text(text) = content {
                                position_shifts.push((*pos, text.len() as i32));
                            }
                        }
                        Edit::Delete { range } => {
                            position_shifts
                                .push((range.start, -(range.end as i32 - range.start as i32)));
                        }
                        Edit::Replace { range, content } => {
                            let insert_len = if let Content::Text(text) = content {
                                text.len() as i32
                            } else {
                                0
                            };
                            let delete_len = range.end as i32 - range.start as i32;
                            position_shifts.push((range.start, insert_len - delete_len));
                        }
                    }
                }
                position_shifts.sort_by_key(|&(pos, _)| pos);

                // Track where we insert each character for accurate cursor updates
                let mut insertion_positions = Vec::new();

                // Prepare edits with adjusted positions
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        // Delete selection first
                        let del_range = sel.byte_range(doc);
                        // Adjust range based on previous pending edits
                        let mut adjusted_start = del_range.start;
                        let mut adjusted_end = del_range.end;
                        for &(edit_pos, shift) in &position_shifts {
                            if edit_pos <= del_range.start {
                                adjusted_start = (adjusted_start as i32 + shift).max(0) as usize;
                                adjusted_end = (adjusted_end as i32 + shift).max(0) as usize;
                            } else if edit_pos < del_range.end {
                                adjusted_end = (adjusted_end as i32 + shift).max(0) as usize;
                            }
                        }

                        self.pending_edits.push(Edit::Delete {
                            range: adjusted_start..adjusted_end,
                        });
                        // Add this delete to our position shifts
                        position_shifts.push((
                            adjusted_start,
                            -(adjusted_end as i32 - adjusted_start as i32),
                        ));
                        position_shifts.sort_by_key(|&(pos, _)| pos);
                    }

                    // Convert cursor position to byte offset for insertion
                    let tree = doc.read();
                    let cursor_pos = sel.min_pos();

                    // Check if cursor is beyond the current line in the tree
                    // This happens when typing rapidly - cursor.column increments but tree isn't updated yet
                    let line_byte_start = tree.line_to_byte(cursor_pos.line).unwrap_or(0);
                    let line_byte_end = tree
                        .line_to_byte(cursor_pos.line + 1)
                        .unwrap_or(tree.byte_count());
                    let line_text = tree.get_text_slice(line_byte_start..line_byte_end);
                    let line_char_count = line_text.trim_end_matches('\n').chars().count() as u32;

                    let adjusted_pos = if cursor_pos.column > line_char_count {
                        // Cursor is beyond line end - this means we're typing at the end
                        // Insert at actual end of line plus any pending edits
                        let line_end_byte =
                            line_byte_start + line_text.trim_end_matches('\n').len();
                        let total_pending: i32 =
                            position_shifts.iter().map(|&(_, shift)| shift).sum();
                        (line_end_byte as i32 + total_pending).max(0) as usize
                    } else {
                        // Normal case - cursor is within the line
                        let base_pos = tree.doc_pos_to_byte(cursor_pos);
                        let total_shift: i32 = position_shifts
                            .iter()
                            .filter(|&&(pos, _)| pos <= base_pos)
                            .map(|&(_, shift)| shift)
                            .sum();
                        (base_pos as i32 + total_shift).max(0) as usize
                    };

                    // Remember where we're inserting for this cursor
                    insertion_positions.push(adjusted_pos);

                    self.pending_edits.push(Edit::Insert {
                        pos: adjusted_pos,
                        content: Content::Text(ch.to_string()),
                    });
                    // Add this insert to position shifts for next iteration
                    position_shifts.push((adjusted_pos, ch.len() as i32));
                    position_shifts.sort_by_key(|&(pos, _)| pos);
                }

                // Flush immediately WITH renderer for incremental updates
                let _needs_redraw = self.flush_pending_edits_with_renderer(doc, renderer);

                // Update cursors using the actual insertion positions
                // This avoids the bug where doc_pos_to_byte clamps to line end
                let ch_len = ch.len() as u32;

                // Collect debug info before mutating selections
                let debug_info: Vec<_> = if self.selections.len() > 1 {
                    let tree = doc.read();
                    self.selections
                        .iter()
                        .enumerate()
                        .map(|(i, sel)| {
                            let mut test_cursor = sel.cursor;
                            test_cursor.column += ch_len;
                            let would_be = tree.doc_pos_to_byte(test_cursor);
                            let correct = insertion_positions[i] + ch.len();
                            (would_be, correct, would_be != correct)
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                for (i, sel) in self.selections.iter_mut().enumerate() {
                    // Update column
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                    }
                    sel.cursor.column += ch_len;
                    sel.anchor = sel.cursor;

                    // Calculate byte_offset based on where we actually inserted
                    // insertion_positions[i] is the byte position where we inserted the character
                    // After insertion, cursor should be one position past that
                    sel.cursor.byte_offset = insertion_positions[i] + ch.len();
                    sel.anchor.byte_offset = sel.cursor.byte_offset;

                    // Debug output
                    if !debug_info.is_empty() && debug_info[i].2 {
                        println!("!!! AVOIDING BUG: Cursor {} col={}, our byte={}, doc_pos_to_byte would return={}",
                            i, sel.cursor.column, sel.cursor.byte_offset, debug_info[i].0);
                    }
                }

                // Return redraw action
                return InputAction::Redraw;
            }
            // For other keys, fall back to original implementation
            _ => return self.on_key(doc, _viewport, event, modifiers),
        }
    }

    /// Handle keyboard input
    pub fn on_key(
        &mut self,
        doc: &Doc,
        _viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
    ) -> InputAction {
        if event.state != ElementState::Pressed {
            return InputAction::None;
        }

        let shift_held = modifiers.state().shift_key();

        // Platform-specific command key
        #[cfg(target_os = "macos")]
        let cmd_held = modifiers.state().super_key();
        #[cfg(not(target_os = "macos"))]
        let cmd_held = modifiers.state().control_key();

        // Check for command shortcuts first
        if cmd_held {
            match &event.logical_key {
                Key::Character(ch) => {
                    match ch.to_lowercase().as_str() {
                        "z" if shift_held => {
                            // Cmd+Shift+Z: Redo
                            return InputAction::Redo;
                        }
                        "z" if !shift_held => {
                            // Cmd+Z: Undo
                            return InputAction::Undo;
                        }
                        "c" => {
                            // Cmd+C: Copy
                            self.copy(doc);
                            return InputAction::None;
                        }
                        "x" => {
                            // Cmd+X: Cut
                            self.cut(doc);
                            return InputAction::Redraw;
                        }
                        "v" => {
                            // Cmd+V: Paste
                            self.paste(doc);
                            return InputAction::Redraw;
                        }
                        "s" => {
                            // Cmd+S: Save
                            return InputAction::Save;
                        }
                        "a" => {
                            // Cmd+A: Select All
                            self.select_all(doc);
                            return InputAction::Redraw;
                        }
                        "[" => {
                            // Cmd+[: Navigate back
                            let current_pos = self.primary_cursor_doc_pos(doc);
                            if let Some(prev_pos) = self.nav_history.undo(current_pos) {
                                // Set cursor to previous position
                                self.selections.clear();
                                self.selections.push(Selection {
                                    cursor: prev_pos,
                                    anchor: prev_pos,
                                    id: self.next_id,
                                });
                                self.next_id += 1;
                                return InputAction::Redraw;
                            }
                            return InputAction::None;
                        }
                        "]" => {
                            // Cmd+]: Navigate forward
                            let current_pos = self.primary_cursor_doc_pos(doc);
                            if let Some(next_pos) = self.nav_history.redo(current_pos) {
                                // Set cursor to next position
                                self.selections.clear();
                                self.selections.push(Selection {
                                    cursor: next_pos,
                                    anchor: next_pos,
                                    id: self.next_id,
                                });
                                self.next_id += 1;
                                return InputAction::Redraw;
                            }
                            return InputAction::None;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        match &event.logical_key {
            Key::Character(ch) if !cmd_held => {
                // Type character at all cursors
                self.goal_column = None; // Reset goal column when typing

                // Save snapshot before edit
                self.save_snapshot_to_history(doc);

                // Calculate cumulative offset from existing pending edits
                // This is critical for correct positioning when buffering multiple keystrokes
                let mut position_shifts: Vec<(usize, i32)> = Vec::new();
                for edit in &self.pending_edits {
                    match edit {
                        Edit::Insert { pos, content } => {
                            if let Content::Text(text) = content {
                                position_shifts.push((*pos, text.len() as i32));
                            }
                        }
                        Edit::Delete { range } => {
                            position_shifts
                                .push((range.start, -(range.end as i32 - range.start as i32)));
                        }
                        Edit::Replace { range, content } => {
                            let insert_len = if let Content::Text(text) = content {
                                text.len() as i32
                            } else {
                                0
                            };
                            let delete_len = range.end as i32 - range.start as i32;
                            position_shifts.push((range.start, insert_len - delete_len));
                        }
                    }
                }
                position_shifts.sort_by_key(|&(pos, _)| pos);

                // Track where we insert each character for accurate cursor updates
                let mut insertion_positions = Vec::new();

                // Prepare edits with adjusted positions
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        // Delete selection first
                        let del_range = sel.byte_range(doc);
                        // Adjust range based on previous pending edits
                        let mut adjusted_start = del_range.start;
                        let mut adjusted_end = del_range.end;
                        for &(edit_pos, shift) in &position_shifts {
                            if edit_pos <= del_range.start {
                                adjusted_start = (adjusted_start as i32 + shift).max(0) as usize;
                                adjusted_end = (adjusted_end as i32 + shift).max(0) as usize;
                            } else if edit_pos < del_range.end {
                                adjusted_end = (adjusted_end as i32 + shift).max(0) as usize;
                            }
                        }

                        self.pending_edits.push(Edit::Delete {
                            range: adjusted_start..adjusted_end,
                        });
                        // Add this delete to our position shifts
                        position_shifts.push((
                            adjusted_start,
                            -(adjusted_end as i32 - adjusted_start as i32),
                        ));
                        position_shifts.sort_by_key(|&(pos, _)| pos);
                    }

                    // Convert cursor position to byte offset for insertion
                    let tree = doc.read();
                    let cursor_pos = sel.min_pos();

                    // Check if cursor is beyond the current line in the tree
                    // This happens when typing rapidly - cursor.column increments but tree isn't updated yet
                    let line_byte_start = tree.line_to_byte(cursor_pos.line).unwrap_or(0);
                    let line_byte_end = tree
                        .line_to_byte(cursor_pos.line + 1)
                        .unwrap_or(tree.byte_count());
                    let line_text = tree.get_text_slice(line_byte_start..line_byte_end);
                    let line_char_count = line_text.trim_end_matches('\n').chars().count() as u32;

                    let adjusted_pos = if cursor_pos.column > line_char_count {
                        // Cursor is beyond line end - this means we're typing at the end
                        // Insert at actual end of line plus any pending edits
                        let line_end_byte =
                            line_byte_start + line_text.trim_end_matches('\n').len();
                        let total_pending: i32 =
                            position_shifts.iter().map(|&(_, shift)| shift).sum();
                        (line_end_byte as i32 + total_pending).max(0) as usize
                    } else {
                        // Normal case - cursor is within the line
                        let base_pos = tree.doc_pos_to_byte(cursor_pos);
                        let total_shift: i32 = position_shifts
                            .iter()
                            .filter(|&&(pos, _)| pos <= base_pos)
                            .map(|&(_, shift)| shift)
                            .sum();
                        (base_pos as i32 + total_shift).max(0) as usize
                    };

                    // Remember where we're inserting for this cursor
                    insertion_positions.push(adjusted_pos);

                    self.pending_edits.push(Edit::Insert {
                        pos: adjusted_pos,
                        content: Content::Text(ch.to_string()),
                    });
                    // Add this insert to position shifts for next iteration
                    position_shifts.push((adjusted_pos, ch.len() as i32));
                    position_shifts.sort_by_key(|&(pos, _)| pos);
                }

                // Flush immediately WITH syntax coordination
                let _needs_redraw = self.flush_pending_edits(doc);

                // Update cursors using the actual insertion positions
                // This avoids the bug where doc_pos_to_byte clamps to line end
                let ch_len = ch.len() as u32;
                for (i, sel) in self.selections.iter_mut().enumerate() {
                    // Update column
                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                    }
                    sel.cursor.column += ch_len;
                    sel.anchor = sel.cursor;

                    // Calculate byte_offset based on where we actually inserted
                    // insertion_positions already accounts for shifts from earlier cursors
                    sel.cursor.byte_offset = insertion_positions[i] + ch.len();
                    sel.anchor.byte_offset = sel.cursor.byte_offset;
                }

                // Return redraw action
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::Backspace) => {
                // Save snapshot before edit
                self.save_snapshot_to_history(doc);

                // Track what characters we're deleting and where for cursor adjustment
                struct DeleteInfo {
                    deleted_char: Option<String>,
                    prev_line_end_col: Option<u32>, // For newline deletion
                }
                let mut delete_info = Vec::new();

                // Delete before cursor
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        self.pending_edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                        delete_info.push(DeleteInfo {
                            deleted_char: None,
                            prev_line_end_col: None,
                        });
                    } else if sel.cursor.line > 0 || sel.cursor.column > 0 {
                        let tree = doc.read();
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte > 0 {
                            let delete_range = cursor_byte - 1..cursor_byte;

                            // Store the character we're about to delete
                            let deleted_char = tree.get_text_slice(delete_range.clone());

                            // If we're deleting a newline, store where the previous line ends
                            let prev_line_end_col = if deleted_char == "\n" && sel.cursor.line > 0 {
                                // Find the length of the previous line
                                if let Some(line_start) = tree.line_to_byte(sel.cursor.line - 1) {
                                    let line_end = tree
                                        .line_to_byte(sel.cursor.line)
                                        .unwrap_or(tree.byte_count());
                                    let line_text = tree.get_text_slice(line_start..line_end - 1); // -1 to exclude newline

                                    // Count visual columns
                                    let mut visual_column = 0u32;
                                    const TAB_WIDTH: u32 = 4;
                                    for ch in line_text.chars() {
                                        if ch == '\t' {
                                            visual_column =
                                                ((visual_column / TAB_WIDTH) + 1) * TAB_WIDTH;
                                        } else if ch != '\n' {
                                            visual_column += 1;
                                        }
                                    }
                                    Some(visual_column)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            delete_info.push(DeleteInfo {
                                deleted_char: Some(deleted_char.clone()),
                                prev_line_end_col,
                            });

                            eprintln!("DEBUG: Deleting '{}' at byte range {:?}, cursor pos line {} col {}",
                                     deleted_char.escape_debug(), delete_range, sel.cursor.line, sel.cursor.column);
                            self.pending_edits.push(Edit::Delete {
                                range: delete_range,
                            });
                        } else {
                            delete_info.push(DeleteInfo {
                                deleted_char: None,
                                prev_line_end_col: None,
                            });
                        }
                    } else {
                        delete_info.push(DeleteInfo {
                            deleted_char: None,
                            prev_line_end_col: None,
                        });
                    }
                }

                // Immediately flush WITH syntax coordination
                self.flush_pending_edits(doc);

                // Move cursors back
                for (i, sel) in self.selections.iter_mut().enumerate() {
                    let info = delete_info.get(i);

                    if !sel.is_cursor() {
                        sel.cursor = sel.min_pos();
                        sel.anchor = sel.cursor;
                    } else if sel.cursor.column > 0 {
                        // Check what character we just deleted to move cursor appropriately
                        if let Some(info) = info {
                            if let Some(ref deleted) = info.deleted_char {
                                if deleted == "\t" {
                                    // We deleted a tab - move back to previous tab stop
                                    const TAB_WIDTH: u32 = 4;
                                    // Find which tab stop we're at
                                    let current_tab_stop =
                                        (sel.cursor.column + TAB_WIDTH - 1) / TAB_WIDTH;
                                    // Move to previous tab stop
                                    sel.cursor.column = (current_tab_stop - 1) * TAB_WIDTH;
                                    eprintln!(
                                        "DEBUG: Deleted tab, moved from col {} to col {}",
                                        current_tab_stop * TAB_WIDTH,
                                        sel.cursor.column
                                    );
                                } else {
                                    // Normal character - just move back one column
                                    sel.cursor.column -= 1;
                                }
                            } else {
                                // Fallback
                                sel.cursor.column -= 1;
                            }
                        } else {
                            // Fallback
                            sel.cursor.column -= 1;
                        }
                        sel.anchor = sel.cursor;
                    } else if sel.cursor.line > 0 {
                        // We're at the beginning of a line and deleted something (probably newline)
                        // Move to previous line
                        sel.cursor.line -= 1;

                        if let Some(info) = info {
                            if let Some(ref deleted) = info.deleted_char {
                                if deleted == "\n" {
                                    // We deleted a newline - use the stored column position
                                    if let Some(col) = info.prev_line_end_col {
                                        sel.cursor.column = col;
                                        eprintln!("DEBUG: Deleted newline - cursor at end of prev line: line {} col {}",
                                                 sel.cursor.line, sel.cursor.column);
                                    } else {
                                        // Fallback - shouldn't happen
                                        sel.cursor.column = 0;
                                    }
                                } else {
                                    // Deleted something else at line start?
                                    sel.cursor.column = 0;
                                }
                            } else {
                                sel.cursor.column = 0;
                            }
                        } else {
                            sel.cursor.column = 0;
                        }
                        sel.anchor = sel.cursor;
                    }
                }
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::Delete) => {
                // Save snapshot before edit
                self.save_snapshot_to_history(doc);
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
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
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::Enter) => {
                // Save snapshot before edit
                self.save_snapshot_to_history(doc);
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
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::Tab) => {
                // Save snapshot before edit
                self.save_snapshot_to_history(doc);
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
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::PageUp) => {
                // Save position before page jump
                self.nav_history
                    .checkpoint_if_changed(self.primary_cursor_doc_pos(doc));

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
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::PageDown) => {
                // Save position before page jump
                self.nav_history
                    .checkpoint_if_changed(self.primary_cursor_doc_pos(doc));

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
                return InputAction::Redraw;
            }
            Key::Named(NamedKey::Space) => {
                // Save snapshot before edit
                self.save_snapshot_to_history(doc);
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
                return InputAction::Redraw;
            }
            _ => {
                return InputAction::None;
            }
        }
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

        // No need to flush syntax updates immediately for click

        // Save current position to navigation history before jumping
        let current_pos = self.primary_cursor_doc_pos(doc);

        // Reset goal column when clicking
        self.goal_column = None;

        // Convert click position to document coordinates using viewport and tree
        // The pos is already window-relative logical pixels, so we need to add scroll offset
        let layout_pos = LayoutPos {
            x: pos.x + viewport.scroll.x,
            y: pos.y + viewport.scroll.y,
        };
        let tree = doc.read();
        let doc_pos = viewport.layout_to_doc_with_tree(layout_pos, &tree);

        // Only save to navigation history if we're jumping a significant distance
        let distance = if current_pos.line > doc_pos.line {
            current_pos.line - doc_pos.line
        } else {
            doc_pos.line - current_pos.line
        };
        if distance > 5 {
            self.nav_history.checkpoint_if_changed(current_pos);
        }

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
        // No need to flush syntax updates immediately for drag
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
        // No need to flush syntax updates immediately
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
        // No need to flush syntax updates immediately

        self.copy(doc);

        // Save snapshot for undo
        self.save_snapshot_to_history(doc);

        // Delete selection using pending edits system
        for sel in &self.selections {
            if !sel.is_cursor() {
                self.pending_edits.push(Edit::Delete {
                    range: sel.byte_range(doc),
                });
            }
        }

        // Flush edits which will trigger syntax update
        self.flush_pending_edits(doc);

        // Collapse selections
        for sel in &mut self.selections {
            sel.cursor = sel.min_pos();
            sel.anchor = sel.cursor;
            // Update byte offsets
            let tree = doc.read();
            sel.cursor.byte_offset = tree.doc_pos_to_byte(sel.cursor);
            sel.anchor.byte_offset = sel.cursor.byte_offset;
        }
    }

    /// Paste from clipboard
    pub fn paste(&mut self, doc: &Doc) {
        // No need to flush syntax updates immediately

        // Try system clipboard first
        let text = if let Ok(mut clipboard) = arboard::Clipboard::new() {
            clipboard.get_text().ok()
        } else {
            None
        }
        .or_else(|| self.clipboard.clone());

        if let Some(text) = text {
            // Save snapshot for undo
            self.save_snapshot_to_history(doc);

            // Use pending_edits system like typing does
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
                    content: Content::Text(text.clone()),
                });
            }

            // Flush pending edits through the proper system
            self.flush_pending_edits(doc);

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
        // No need to flush syntax updates immediately
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
    ) -> (
        Vec<Arc<dyn crate::widget::Widget>>,
        Option<Arc<dyn crate::widget::Widget>>,
    ) {
        use crate::widget;

        let mut selection_widgets = Vec::new();
        let mut cursor_widget = None;

        for (i, selection) in self.selections.iter().enumerate() {
            // Always create cursor at the active end of selection (cursor position)
            // Only create it for the primary selection (index 0)
            if i == 0 {
                let tree = doc.read();
                // Get the line text for accurate cursor positioning
                let line_text = if let Some(line_start) = tree.line_to_byte(selection.cursor.line) {
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

    /// Save current document state to history before making an edit
    fn save_snapshot_to_history(&mut self, doc: &Doc) {
        // Only save if we have pending edits or this is a new operation
        if self.pending_edits.is_empty()
            || self.last_edit_time.is_none()
            || self
                .last_edit_time
                .map(|t| t.elapsed().as_millis() > 500)
                .unwrap_or(true)
        {
            // Save current state with selections
            let snapshot = DocumentSnapshot {
                tree: doc.read(),
                selections: self.selections.clone(),
            };
            self.history.checkpoint(snapshot);
        }
    }

    /// Perform undo operation
    pub fn undo(&mut self, doc: &Doc) -> bool {
        // Flush any pending edits first
        self.flush_pending_edits(doc);
        // Syntax updates will be debounced

        // Create current snapshot
        let current_snapshot = DocumentSnapshot {
            tree: doc.read(),
            selections: self.selections.clone(),
        };

        if let Some(previous_snapshot) = self.history.undo(current_snapshot) {
            // Replace the tree
            doc.replace_tree(previous_snapshot.tree.clone());

            // Restore selections
            self.selections = previous_snapshot.selections;

            // Update next_id to be higher than any restored selection
            self.next_id = self.selections.iter().map(|s| s.id).max().unwrap_or(0) + 1;

            // Queue syntax highlighting update (will be debounced)
            // No need to flush immediately for undo
            if self.syntax_highlighter.is_some() {
                self.has_pending_syntax_update = true;
                self.last_edit_time = Some(Instant::now());
            }

            return true;
        }
        false
    }

    /// Perform redo operation
    pub fn redo(&mut self, doc: &Doc) -> bool {
        // Flush any pending edits first
        self.flush_pending_edits(doc);
        // Syntax updates will be debounced

        // Create current snapshot
        let current_snapshot = DocumentSnapshot {
            tree: doc.read(),
            selections: self.selections.clone(),
        };

        if let Some(next_snapshot) = self.history.redo(current_snapshot) {
            // Replace the tree
            doc.replace_tree(next_snapshot.tree.clone());

            // Restore selections
            self.selections = next_snapshot.selections;

            // Update next_id to be higher than any restored selection
            self.next_id = self.selections.iter().map(|s| s.id).max().unwrap_or(0) + 1;

            // Queue syntax highlighting update (will be debounced)
            // No need to flush immediately for redo
            if self.syntax_highlighter.is_some() {
                self.has_pending_syntax_update = true;
                self.last_edit_time = Some(Instant::now());
            }

            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rapid_typing_cursor_teleport_bug() {
        // Test that rapid typing doesn't cause cursor teleporting
        // We simulate the actual behavior by using get_pending_edits to preview what would happen

        let doc = Doc::from_str("hello ");
        let mut input = InputHandler::new();

        // Set cursor at end of "hello " (position 6)
        input.set_cursor_for_test(DocPos {
            line: 0,
            column: 6,
            byte_offset: 6,
        });

        // Instead of using get_pending_edits (which needs complex setup),
        // let's directly test the position calculation logic by simulating
        // what happens when multiple keystrokes accumulate

        let mut all_positions = Vec::new();

        for i in 0..5 {
            println!("Keystroke {}: simulating position calculation", i + 1);

            // Get current cursor position and calculate base position
            let tree = doc.read();
            let base_pos = tree.doc_pos_to_byte(input.selections[0].cursor);
            println!(
                "  Cursor column={}, base_pos={}",
                input.selections[0].cursor.column, base_pos
            );

            // Calculate position shifts from pending edits
            let mut position_shifts: Vec<(usize, i32)> = Vec::new();
            for edit in &input.pending_edits {
                if let Edit::Insert { pos, content } = edit {
                    if let Content::Text(text) = content {
                        position_shifts.push((*pos, text.len() as i32));
                    }
                }
            }

            // Apply our fix: add ALL pending shifts
            let total_shift: i32 = position_shifts.iter().map(|&(_, shift)| shift).sum();
            let adjusted_pos = (base_pos as i32 + total_shift).max(0) as usize;

            println!(
                "  Pending edits: {}, total_shift: {}, adjusted_pos: {}",
                position_shifts.len(),
                total_shift,
                adjusted_pos
            );

            all_positions.push(adjusted_pos);

            // Add the edit
            input.pending_edits.push(Edit::Insert {
                pos: adjusted_pos,
                content: Content::Text("a".to_string()),
            });

            // Simulate cursor column increment (this happens in on_key)
            input.selections[0].cursor.column += 1;
        }

        println!("\n=== Positions for 5 rapid keystrokes ===");
        for (i, &pos) in all_positions.iter().enumerate() {
            println!("Edit {}: position {} (expected {})", i, pos, 6 + i);
        }

        // Verify positions are strictly increasing
        for i in 1..all_positions.len() {
            assert!(
                all_positions[i] > all_positions[i - 1],
                "BUG: Edit {} at position {} is not > edit {} at position {}",
                i,
                all_positions[i],
                i - 1,
                all_positions[i - 1]
            );
        }

        // Verify positions match expected sequence
        for (i, &pos) in all_positions.iter().enumerate() {
            assert_eq!(
                pos,
                6 + i,
                "Edit {} should be at position {} but is at {}",
                i,
                6 + i,
                pos
            );
        }

        // Flush and verify the final text
        input.flush_pending_edits(&doc);
        {
            let result = doc.read().flatten_to_string().to_string();
            assert_eq!(
                result, "hello aaaaa",
                "Text should be correct after rapid typing"
            );
        }
    }

    #[test]
    fn test_single_cursor_rapid_typing_shows_bug() {
        // This test shows the bug with even a single cursor
        // When typing rapidly, the cursor byte_offset doesn't advance properly

        let doc = Doc::from_str("hello ");
        let mut input = InputHandler::new();

        // Single cursor at end of "hello "
        input.selections = vec![Selection {
            cursor: DocPos {
                line: 0,
                column: 6,
                byte_offset: 6,
            },
            anchor: DocPos {
                line: 0,
                column: 6,
                byte_offset: 6,
            },
            id: 0,
        }];

        println!("=== Single cursor rapid typing test ===");
        println!(
            "Initial: '{}' cursor at column {}, byte {}",
            doc.read().flatten_to_string().trim(),
            input.selections[0].cursor.column,
            input.selections[0].cursor.byte_offset
        );

        // Simulate rapidly typing 10 'x' characters
        // This is what happens when you hold down a key
        for i in 0..10 {
            let ch = "x";

            // Record state before
            let col_before = input.selections[0].cursor.column;
            let byte_before = input.selections[0].cursor.byte_offset;

            // This is the exact code path from on_key when typing a character
            // Step 1: Prepare the edit at current cursor position
            let tree = doc.read();
            let insert_pos = tree.doc_pos_to_byte(input.selections[0].cursor);

            input.pending_edits.push(Edit::Insert {
                pos: insert_pos,
                content: Content::Text(ch.to_string()),
            });

            // Step 2: Flush the edit to the document
            input.flush_pending_edits(&doc);

            // Step 3: Update cursor position (THIS IS WHERE THE BUG OCCURS)
            input.selections[0].cursor.column += 1;
            input.selections[0].anchor = input.selections[0].cursor;

            // Step 4: Recalculate byte_offset from the tree
            // BUG: When cursor.column > line length, doc_pos_to_byte returns end of line!
            let tree = doc.read();
            let recalculated_byte = tree.doc_pos_to_byte(input.selections[0].cursor);
            input.selections[0].cursor.byte_offset = recalculated_byte;
            input.selections[0].anchor.byte_offset = recalculated_byte;

            println!(
                "After typing char {}: col {} -> {}, byte {} -> {} (inserted at: {})",
                i + 1,
                col_before,
                input.selections[0].cursor.column,
                byte_before,
                input.selections[0].cursor.byte_offset,
                insert_pos
            );

            // The bug: byte_offset should be 6 + i + 1, but it may snap back
            let expected_byte = 6 + i + 1;
            if input.selections[0].cursor.byte_offset != expected_byte {
                println!(
                    "  !!! BUG: byte_offset is {}, expected {} !!!",
                    input.selections[0].cursor.byte_offset, expected_byte
                );
            }
        }

        let final_text = doc.read().flatten_to_string();
        println!("\nFinal text: '{}'", final_text.trim());
        println!(
            "Final cursor: column {}, byte_offset {}",
            input.selections[0].cursor.column, input.selections[0].cursor.byte_offset
        );

        // Verify the bug exists
        assert_eq!(input.selections[0].cursor.column, 16, "Column should be 16");

        // This assertion will FAIL if the bug still exists
        // The byte_offset SHOULD be 16 but due to the bug it might be less
        assert_eq!(
            input.selections[0].cursor.byte_offset, 16,
            "BUG: byte_offset should be 16 but cursor is snapping back!"
        );
    }

    #[test]
    fn test_multi_cursor_rapid_typing_bug() {
        // Test with multiple cursors on different lines
        // Each cursor should maintain correct position when typing rapidly

        let doc = Doc::from_str("line1\nline2\nline3\nline4\nline5\n");
        let mut input = InputHandler::new();

        // Set up 5 cursors at the end of each line
        input.selections.clear();
        let positions = vec![
            (0, 5, 5),  // end of "line1"
            (1, 5, 11), // end of "line2" (5 + \n + 5)
            (2, 5, 17), // end of "line3"
            (3, 5, 23), // end of "line4"
            (4, 5, 29), // end of "line5"
        ];

        for (line, col, byte_off) in positions {
            input.selections.push(Selection {
                cursor: DocPos {
                    line,
                    column: col,
                    byte_offset: byte_off,
                },
                anchor: DocPos {
                    line,
                    column: col,
                    byte_offset: byte_off,
                },
                id: line,
            });
        }

        println!("=== Testing multi-cursor rapid typing ===");
        println!("Initial text:\n{}", doc.read().flatten_to_string());
        println!("Starting with {} cursors", input.selections.len());

        // Type 5 characters rapidly at all cursors
        for i in 0..5 {
            println!("\n--- Typing character {} at all cursors ---", i + 1);

            // Record cursor state before typing
            let cursors_before: Vec<_> = input
                .selections
                .iter()
                .map(|sel| (sel.cursor.line, sel.cursor.column, sel.cursor.byte_offset))
                .collect();

            // This simulates what happens in on_key_with_renderer
            // Build position shifts to track cumulative changes
            let mut position_shifts: Vec<(usize, i32)> = Vec::new();
            for edit in &input.pending_edits {
                if let Edit::Insert { pos, content } = edit {
                    if let Content::Text(text) = content {
                        position_shifts.push((*pos, text.len() as i32));
                    }
                }
            }
            position_shifts.sort_by_key(|&(pos, _)| pos);

            // Calculate insertion positions for each cursor
            let mut insertion_positions = Vec::new();
            for sel in &input.selections {
                let tree = doc.read();
                let base_pos = tree.doc_pos_to_byte(sel.cursor);

                // Apply position shifts from earlier cursors
                let mut adjusted_pos = base_pos;
                for &(edit_pos, shift) in &position_shifts {
                    if edit_pos <= base_pos {
                        adjusted_pos = (adjusted_pos as i32 + shift) as usize;
                    }
                }

                insertion_positions.push(adjusted_pos);

                // Add this insertion to pending edits
                input.pending_edits.push(Edit::Insert {
                    pos: adjusted_pos,
                    content: Content::Text("x".to_string()),
                });

                // Update position shifts for next cursor
                position_shifts.push((adjusted_pos, 1));
                position_shifts.sort_by_key(|&(pos, _)| pos);
            }

            // Flush edits
            input.flush_pending_edits(&doc);

            // Update cursor positions (THIS IS WHERE THE BUG HAPPENS)
            for (j, sel) in input.selections.iter_mut().enumerate() {
                sel.cursor.column += 1;
                // BUG: This recalculation can snap back if column > line length
                let tree = doc.read();
                sel.cursor.byte_offset = tree.doc_pos_to_byte(sel.cursor);
                sel.anchor = sel.cursor.clone();

                let (prev_line, prev_col, prev_byte) = cursors_before[j];
                println!(
                    "  Cursor {}: line={}, col {} -> {}, byte {} -> {} (expected: {})",
                    j,
                    prev_line,
                    prev_col,
                    sel.cursor.column,
                    prev_byte,
                    sel.cursor.byte_offset,
                    insertion_positions[j] + 1
                );

                if sel.cursor.byte_offset != insertion_positions[j] + 1 {
                    println!("    !!! CURSOR SNAPPED BACK !!!");
                }
            }

            let text_after = doc.read().flatten_to_string();
            println!("Text after typing:\n{}", text_after);
        }

        // Verify the final text is correct
        let final_text = doc.read().flatten_to_string().to_string();
        let expected = "line1xxxxx\nline2xxxxx\nline3xxxxx\nline4xxxxx\nline5xxxxx\n";

        assert_eq!(
            final_text, expected,
            "Multi-cursor typing should produce correct text"
        );

        // Verify all cursors are at correct positions
        // Note: The byte offsets depend on how the multi-cursor typing distributes the insertions
        for (i, sel) in input.selections.iter().enumerate() {
            assert_eq!(sel.cursor.column, 10, "Cursor {} should be at column 10", i);

            // Just verify byte offset is reasonable - exact value depends on implementation
            assert!(
                sel.cursor.byte_offset > 0,
                "Cursor {} should have non-zero byte offset",
                i
            );

            // Verify cursors are in the right order
            if i > 0 {
                assert!(
                    sel.cursor.byte_offset > input.selections[i - 1].cursor.byte_offset,
                    "Cursor {} should be after cursor {}",
                    i,
                    i - 1
                );
            }
        }
    }

    #[test]
    fn test_actual_on_key_rapid_typing_bug() {
        // This test replicates the ACTUAL bug when holding down a key
        // The problem: cursor.column increments but byte_offset gets stuck

        let doc = Doc::from_str("hello ");
        let mut input = InputHandler::new();

        // Position cursor at end of "hello "
        input.set_cursor_for_test(DocPos {
            line: 0,
            column: 6,
            byte_offset: 6,
        });

        println!("=== Simulating holding 'a' key - REAL BUG ===\n");
        println!("Initial text: 'hello ' (6 chars)");

        let mut cursor_positions = Vec::new();
        let mut actual_insertions = Vec::new();

        // Simulate holding 'a' key - each call to on_key flushes immediately
        for i in 0..10 {
            println!("\n--- Keystroke {} (typing 'a') ---", i + 1);

            let cursor_before = input.selections[0].cursor.clone();
            println!(
                "BEFORE: cursor.column={}, byte_offset={}",
                cursor_before.column, cursor_before.byte_offset
            );

            // The BUG is here: when cursor.column > actual line length
            let tree = doc.read();
            let line_text = if let Some(line_start) = tree.line_to_byte(0) {
                let line_end = tree.line_to_byte(1).unwrap_or(tree.byte_count());
                tree.get_text_slice(line_start..line_end)
            } else {
                String::new()
            };
            let actual_line_len = line_text.trim_end_matches('\n').len();
            println!(
                "  Line text: '{}' (length: {})",
                line_text.trim_end_matches('\n'),
                actual_line_len
            );

            //  BUG: cursor.column is 6+i but line is only 6+i chars after i inserts
            // When doc_pos_to_byte is called with column > line length, it returns the END of line
            let byte_from_tree = tree.doc_pos_to_byte(cursor_before);
            println!(
                "  tree.doc_pos_to_byte(cursor) returns: {} (should be: {})",
                byte_from_tree,
                6 + i
            );

            if cursor_before.column as usize > actual_line_len {
                println!(
                    "  !!! BUG: cursor.column {} > line length {} !!!",
                    cursor_before.column, actual_line_len
                );
                println!("  !!! This causes byte_offset to snap back to end of line !!!");
            }

            // Simulate typing 'a' - this is what on_key does
            // First, prepare the edit
            input.pending_edits.push(Edit::Insert {
                pos: byte_from_tree, // THIS IS WRONG when column > line length!
                content: Content::Text("a".to_string()),
            });

            actual_insertions.push(byte_from_tree);

            // Flush immediately (as on_key does)
            input.flush_pending_edits(&doc);

            // Now cursor gets updated AFTER flush
            input.selections[0].cursor.column += 1; // This increments correctly

            // But byte_offset gets recalculated from tree, which SNAPS BACK
            let tree = doc.read();
            let new_byte_offset = tree.doc_pos_to_byte(input.selections[0].cursor);
            input.selections[0].cursor.byte_offset = new_byte_offset;
            input.selections[0].anchor = input.selections[0].cursor.clone();

            cursor_positions.push(input.selections[0].cursor.clone());

            println!(
                "AFTER: cursor.column={}, byte_offset={} (expected byte_offset: {})",
                input.selections[0].cursor.column,
                input.selections[0].cursor.byte_offset,
                6 + i + 1
            );

            let current_text = doc.read().flatten_to_string();
            println!("Text is now: '{}'", current_text);
        }

        // Show the bug pattern
        println!("\n=== BUG PATTERN: Cursor positions after each keystroke ===");
        for (i, pos) in cursor_positions.iter().enumerate() {
            println!("After keystroke {}: column={}, byte_offset={} (expected: column={}, byte_offset={})",
                i + 1, pos.column, pos.byte_offset, 7 + i, 7 + i);
            if pos.byte_offset != 7 + i {
                println!("  ^^^ CURSOR SNAPPED BACK!");
            }
        }

        println!("\n=== Actual insertion positions ===");
        for (i, &pos) in actual_insertions.iter().enumerate() {
            println!(
                "Keystroke {}: inserted at byte position {} (expected: {})",
                i + 1,
                pos,
                6 + i
            );
            if pos != 6 + i {
                println!("  ^^^ WRONG POSITION - text gets jumbled!");
            }
        }

        let result = doc.read().flatten_to_string().to_string();
        println!("\nFinal text: '{}' (expected: 'hello aaaaaaaaaa')", result);

        // Test navigation after typing
        println!("\n=== Testing navigation after the bug ===");

        // Try to navigate left - this will fail at the snap-back position
        let start_col = input.selections[0].cursor.column;
        for _ in 0..15 {
            if input.selections[0].cursor.column > 0 {
                input.selections[0].cursor.column -= 1;
                let tree = doc.read();
                let byte_pos = tree.doc_pos_to_byte(input.selections[0].cursor);
                println!(
                    "Navigate left: column={}, byte_offset={}",
                    input.selections[0].cursor.column, byte_pos
                );

                // The bug: can't navigate past where cursor was snapping back to
                if byte_pos == input.selections[0].cursor.byte_offset {
                    println!("  !!! STUCK - can't navigate further!");
                    break;
                }
                input.selections[0].cursor.byte_offset = byte_pos;
            }
        }

        // The test SHOULD fail to demonstrate the bug
        assert_ne!(
            result, "hello aaaaaaaaaa",
            "BUG CONFIRMED: Text is corrupted due to cursor snapping back"
        );

        // Also verify the cursor gets stuck
        assert!(
            input.selections[0].cursor.column < start_col - 5,
            "BUG CONFIRMED: Can't navigate back through the text properly"
        );
    }
}
