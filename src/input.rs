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
use winit::keyboard::{Key, NamedKey};

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
        let (start, end) = if self.min_pos() == self.cursor {
            (self.cursor, self.anchor)
        } else {
            (self.anchor, self.cursor)
        };
        let line_height = viewport.metrics.line_height;

        // Dummy rectangle for GPU bug workaround
        let mut rects = vec![LayoutRect::new(0.0, 0.0, 0.0, 0.0)];

        if start.line == end.line {
            let start_layout = viewport.doc_to_layout(start);
            let end_layout = viewport.doc_to_layout(end);
            rects.push(LayoutRect::new(
                start_layout.x.0 - 2.0,
                start_layout.y.0,
                end_layout.x.0 - start_layout.x.0,
                line_height,
            ));
        } else {
            let start_layout = viewport.doc_to_layout(start);
            let end_layout = viewport.doc_to_layout(end);
            let viewport_right = viewport.logical_size.width.0 - viewport.margin.x.0;

            // First line
            rects.push(LayoutRect::new(
                start_layout.x.0 - 2.0,
                start_layout.y.0,
                (viewport_right - start_layout.x.0).max(0.0) + 2.0,
                line_height,
            ));

            // Middle lines
            if end.line > start.line + 1 {
                rects.push(LayoutRect::new(
                    viewport.margin.x.0,
                    start_layout.y.0 + line_height,
                    viewport.logical_size.width.0 - (viewport.margin.x.0 * 2.0),
                    (end.line - start.line - 1) as f32 * line_height,
                ));
            }

            // Last line
            rects.push(LayoutRect::new(
                viewport.margin.x.0,
                end_layout.y.0,
                end_layout.x.0 - viewport.margin.x.0 - 2.0,
                line_height,
            ));
        }
        rects
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

    /// Helper: get line text for a line
    fn get_line_text(tree: &crate::tree::Tree, line: u32) -> String {
        tree.line_to_byte(line).map_or(String::new(), |start| {
            let end = tree.line_to_byte(line + 1).unwrap_or(tree.byte_count());
            tree.get_text_slice(start..end)
        })
    }

    /// Helper: get line text without newline
    fn get_line_text_trimmed(tree: &crate::tree::Tree, line: u32) -> String {
        Self::get_line_text(tree, line)
            .trim_end_matches('\n')
            .to_string()
    }

    /// Helper: get line character count
    fn get_line_char_count(tree: &crate::tree::Tree, line: u32) -> u32 {
        Self::get_line_text_trimmed(tree, line).chars().count() as u32
    }

    /// Handle character typing with optional renderer
    fn handle_character_input(
        &mut self,
        doc: &Doc,
        ch: &str,
        renderer: Option<&mut crate::render::Renderer>,
    ) -> InputAction {
        self.goal_column = None;
        self.save_snapshot_to_history(doc);

        // Simply delete selections and insert text at cursor positions
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
                content: Content::Text(ch.to_string()),
            });
        }

        self.flush_pending_edits_with_renderer(doc, renderer);

        // Update cursor positions
        let ch_len = ch.chars().count() as u32;
        for sel in &mut self.selections {
            if !sel.is_cursor() {
                sel.cursor = sel.min_pos();
            }
            sel.cursor.column += ch_len;
            sel.anchor = sel.cursor;
        }

        InputAction::Redraw
    }

    /// Move cursor vertically (up/down)
    fn move_cursor_vertical(&mut self, doc: &Doc, direction: i32, shift_held: bool) -> InputAction {
        if self.goal_column.is_none() && !self.selections.is_empty() {
            self.goal_column = Some(self.selections[0].cursor.column);
        }

        for sel in &mut self.selections {
            let tree = doc.read();
            if direction < 0 && sel.cursor.line > 0 {
                sel.cursor.line -= 1;
            } else if direction > 0 && tree.line_to_byte(sel.cursor.line + 1).is_some() {
                sel.cursor.line += 1;
            } else {
                continue;
            }

            let line_length = Self::get_line_char_count(&tree, sel.cursor.line);
            sel.cursor.column = self
                .goal_column
                .unwrap_or(sel.cursor.column)
                .min(line_length);
            if !shift_held {
                sel.anchor = sel.cursor;
            }
        }
        InputAction::Redraw
    }

    /// Move cursor horizontally (left/right)
    fn move_cursor_horizontal(
        &mut self,
        doc: &Doc,
        direction: i32,
        shift_held: bool,
    ) -> InputAction {
        self.goal_column = None;

        for sel in &mut self.selections {
            let tree = doc.read();
            if direction < 0 {
                if sel.cursor.column > 0 {
                    sel.cursor.column -= 1;
                } else if sel.cursor.line > 0 {
                    sel.cursor.line -= 1;
                    sel.cursor.column = Self::get_line_char_count(&tree, sel.cursor.line);
                }
            } else {
                let line_length = Self::get_line_char_count(&tree, sel.cursor.line);
                if sel.cursor.column < line_length {
                    sel.cursor.column += 1;
                } else if tree.line_to_byte(sel.cursor.line + 1).is_some() {
                    sel.cursor.line += 1;
                    sel.cursor.column = 0;
                }
            }
            if !shift_held {
                sel.anchor = sel.cursor;
            }
        }
        InputAction::Redraw
    }

    /// Delete at cursor position (forward or backward)
    fn delete_at_cursor(&mut self, doc: &Doc, forward: bool) -> InputAction {
        self.save_snapshot_to_history(doc);

        if forward {
            // Delete key
            for sel in &self.selections {
                let tree = doc.read();
                let range = if !sel.is_cursor() {
                    sel.byte_range(doc)
                } else {
                    let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                    if cursor_byte < tree.byte_count() {
                        cursor_byte..cursor_byte + 1
                    } else {
                        continue;
                    }
                };
                self.pending_edits.push(Edit::Delete { range });
            }
            self.flush_pending_edits(doc);
        } else {
            // Backspace - track what we delete to update cursor properly
            let mut deleted_info = Vec::new();

            for sel in &self.selections {
                if !sel.is_cursor() {
                    self.pending_edits.push(Edit::Delete {
                        range: sel.byte_range(doc),
                    });
                    deleted_info.push(None);
                } else if sel.cursor.line > 0 || sel.cursor.column > 0 {
                    let tree = doc.read();
                    let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                    if cursor_byte > 0 {
                        let deleted = tree.get_text_slice(cursor_byte - 1..cursor_byte);
                        deleted_info.push(Some(deleted.clone()));
                        self.pending_edits.push(Edit::Delete {
                            range: cursor_byte - 1..cursor_byte,
                        });
                    } else {
                        deleted_info.push(None);
                    }
                } else {
                    deleted_info.push(None);
                }
            }

            self.flush_pending_edits(doc);

            // Update cursors after backspace
            for (sel, deleted) in self.selections.iter_mut().zip(deleted_info.iter()) {
                if !sel.is_cursor() {
                    sel.cursor = sel.min_pos();
                } else if sel.cursor.column > 0 {
                    match deleted.as_deref() {
                        Some("\t") => sel.cursor.column = ((sel.cursor.column + 3) / 4 - 1) * 4,
                        _ => sel.cursor.column -= 1,
                    }
                } else if sel.cursor.line > 0 {
                    sel.cursor.line -= 1;
                    sel.cursor.column = if deleted.as_deref() == Some("\n") {
                        Self::get_line_length(doc, sel.cursor.line)
                    } else {
                        0
                    };
                }
                sel.anchor = sel.cursor;
            }
        }

        InputAction::Redraw
    }

    /// Insert text at cursor positions
    fn insert_text(&mut self, doc: &Doc, text: &str) -> InputAction {
        self.save_snapshot_to_history(doc);

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
                content: Content::Text(text.to_string()),
            });
        }

        self.flush_pending_edits(doc);

        // Update cursor positions
        for sel in &mut self.selections {
            if !sel.is_cursor() {
                sel.cursor = sel.min_pos();
            }
            match text {
                "\n" => {
                    sel.cursor.line += 1;
                    sel.cursor.column = 0;
                }
                "\t" => sel.cursor.column = ((sel.cursor.column / 4) + 1) * 4,
                _ => sel.cursor.column += text.chars().count() as u32,
            }
            sel.anchor = sel.cursor;
        }
        InputAction::Redraw
    }

    /// Get line length for a given line number
    fn get_line_length(doc: &Doc, line: u32) -> u32 {
        let tree = doc.read();
        Self::get_line_char_count(&tree, line)
    }

    /// Move cursor to start or end of line
    fn move_to_line_edge(&mut self, doc: &Doc, to_end: bool, shift_held: bool) -> InputAction {
        self.goal_column = None;
        for sel in &mut self.selections {
            sel.cursor.column = if to_end {
                Self::get_line_length(doc, sel.cursor.line)
            } else {
                0
            };
            if !shift_held {
                sel.anchor = sel.cursor;
            }
        }
        InputAction::Redraw
    }

    /// Page navigation (up/down)
    fn page_jump(&mut self, doc: &Doc, up: bool, shift_held: bool) -> InputAction {
        self.nav_history
            .checkpoint_if_changed(self.primary_cursor_doc_pos(doc));
        if self.goal_column.is_none() && !self.selections.is_empty() {
            self.goal_column = Some(self.selections[0].cursor.column);
        }

        const PAGE_SIZE: u32 = 20;
        for sel in &mut self.selections {
            let tree = doc.read();
            let total_lines = tree.line_count();
            if up {
                sel.cursor.line = sel.cursor.line.saturating_sub(PAGE_SIZE);
            } else {
                sel.cursor.line = (sel.cursor.line + PAGE_SIZE).min(total_lines.saturating_sub(1));
            }
            let line_length = Self::get_line_length(doc, sel.cursor.line);
            sel.cursor.column = self
                .goal_column
                .unwrap_or(sel.cursor.column)
                .min(line_length);
            if !shift_held {
                sel.anchor = sel.cursor;
            }
        }
        InputAction::Redraw
    }

    /// Handle command key combinations
    fn handle_command_key(&mut self, doc: &Doc, ch: &str, shift_held: bool) -> InputAction {
        match ch {
            "z" if shift_held => InputAction::Redo,
            "z" => InputAction::Undo,
            "c" => {
                self.copy(doc);
                InputAction::None
            }
            "x" => {
                self.cut(doc);
                InputAction::Redraw
            }
            "v" => {
                self.paste(doc);
                InputAction::Redraw
            }
            "s" => InputAction::Save,
            "a" => {
                self.select_all(doc);
                InputAction::Redraw
            }
            "[" => self.navigate_history(doc, true),
            "]" => self.navigate_history(doc, false),
            _ => InputAction::None,
        }
    }

    /// Navigate through cursor history
    fn navigate_history(&mut self, doc: &Doc, back: bool) -> InputAction {
        let current_pos = self.primary_cursor_doc_pos(doc);
        let new_pos = if back {
            self.nav_history.undo(current_pos)
        } else {
            self.nav_history.redo(current_pos)
        };

        if let Some(pos) = new_pos {
            self.selections = vec![Selection {
                cursor: pos,
                anchor: pos,
                id: self.next_id,
            }];
            self.next_id += 1;
            InputAction::Redraw
        } else {
            InputAction::None
        }
    }

    /// Set the syntax highlighter for InputEdit coordination
    pub fn set_syntax_highlighter(&mut self, highlighter: Arc<SyntaxHighlighter>) {
        self.syntax_highlighter = Some(highlighter);
    }

    /// Check if we should send syntax updates
    pub fn should_flush(&self) -> bool {
        self.has_pending_syntax_update
            && self
                .last_edit_time
                .map_or(false, |t| t.elapsed().as_millis() > 100)
    }

    pub fn pending_edits_count(&self) -> usize {
        self.pending_edits.len()
    }

    pub fn get_pending_edits_for_test(&self) -> &[Edit] {
        &self.pending_edits
    }

    pub fn set_cursor_for_test(&mut self, pos: DocPos) {
        self.selections = vec![Selection {
            cursor: pos,
            anchor: pos,
            id: 0,
        }];
    }

    pub fn selections_for_test(&self) -> &[Selection] {
        &self.selections
    }

    pub fn selections_mut_for_test(&mut self) -> &mut Vec<Selection> {
        &mut self.selections
    }

    pub fn pending_edits_mut_for_test(&mut self) -> &mut Vec<Edit> {
        &mut self.pending_edits
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
            Key::Character(ch) if ch.chars().all(|c| !c.is_control()) => {
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    }
                    let tree = doc.read();
                    edits.push(Edit::Insert {
                        pos: tree.doc_pos_to_byte(sel.min_pos()),
                        content: Content::Text(ch.to_string()),
                    });
                }
            }
            Key::Named(NamedKey::Backspace) => {
                for sel in &self.selections {
                    if !sel.is_cursor() {
                        edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else if sel.cursor.column > 0 || sel.cursor.line > 0 {
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
                for sel in &self.selections {
                    let tree = doc.read();
                    if !sel.is_cursor() {
                        edits.push(Edit::Delete {
                            range: sel.byte_range(doc),
                        });
                    } else {
                        let cursor_byte = tree.doc_pos_to_byte(sel.cursor);
                        if cursor_byte < tree.byte_count() {
                            edits.push(Edit::Delete {
                                range: cursor_byte..(cursor_byte + 1),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
        edits
    }

    /// Handle key input with optional renderer for incremental updates
    pub fn on_key_with_renderer(
        &mut self,
        doc: &Doc,
        viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
        renderer: Option<&mut crate::render::Renderer>,
    ) -> InputAction {
        self.on_key_internal(doc, viewport, event, modifiers, renderer)
    }

    /// Handle keyboard input
    pub fn on_key(
        &mut self,
        doc: &Doc,
        viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &winit::event::Modifiers,
    ) -> InputAction {
        self.on_key_internal(doc, viewport, event, modifiers, None)
    }

    /// Internal key handling with optional renderer
    fn on_key_internal(
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

        let shift_held = modifiers.state().shift_key();
        #[cfg(target_os = "macos")]
        let cmd_held = modifiers.state().super_key();
        #[cfg(not(target_os = "macos"))]
        let cmd_held = modifiers.state().control_key();

        if cmd_held {
            if let Key::Character(ch) = &event.logical_key {
                return self.handle_command_key(doc, ch.to_lowercase().as_str(), shift_held);
            }
        }

        match &event.logical_key {
            Key::Character(ch) if !cmd_held => self.handle_character_input(doc, ch, renderer),
            Key::Named(NamedKey::Backspace) => self.delete_at_cursor(doc, false),
            Key::Named(NamedKey::Delete) => self.delete_at_cursor(doc, true),
            Key::Named(NamedKey::Enter) => self.insert_text(doc, "\n"),
            Key::Named(NamedKey::Tab) => self.insert_text(doc, "\t"),
            Key::Named(NamedKey::Space) => self.insert_text(doc, " "),
            Key::Named(NamedKey::ArrowLeft) => self.move_cursor_horizontal(doc, -1, shift_held),
            Key::Named(NamedKey::ArrowRight) => self.move_cursor_horizontal(doc, 1, shift_held),
            Key::Named(NamedKey::ArrowUp) => self.move_cursor_vertical(doc, -1, shift_held),
            Key::Named(NamedKey::ArrowDown) => self.move_cursor_vertical(doc, 1, shift_held),
            Key::Named(NamedKey::Home) => self.move_to_line_edge(doc, false, shift_held),
            Key::Named(NamedKey::End) => self.move_to_line_edge(doc, true, shift_held),
            Key::Named(NamedKey::PageUp) => self.page_jump(doc, true, shift_held),
            Key::Named(NamedKey::PageDown) => self.page_jump(doc, false, shift_held),
            _ => InputAction::None,
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

        let current_pos = self.primary_cursor_doc_pos(doc);
        self.goal_column = None;

        let layout_pos = LayoutPos {
            x: pos.x + viewport.scroll.x,
            y: pos.y + viewport.scroll.y,
        };
        let tree = doc.read();
        let doc_pos = viewport.layout_to_doc_with_tree(layout_pos, &tree);

        // Save to nav history if jumping >5 lines
        if current_pos.line.abs_diff(doc_pos.line) > 5 {
            self.nav_history.checkpoint_if_changed(current_pos);
        }

        if alt_held {
            self.selections.push(Selection {
                cursor: doc_pos,
                anchor: doc_pos,
                id: self.next_id,
            });
        } else {
            self.selections = vec![Selection {
                cursor: doc_pos,
                anchor: doc_pos,
                id: self.next_id,
            }];
        }
        self.next_id += 1;
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
        let tree = doc.read();
        let start_doc = viewport.layout_to_doc_with_tree(
            LayoutPos {
                x: from.x + viewport.scroll.x,
                y: from.y + viewport.scroll.y,
            },
            &tree,
        );
        let end_doc = viewport.layout_to_doc_with_tree(
            LayoutPos {
                x: to.x + viewport.scroll.x,
                y: to.y + viewport.scroll.y,
            },
            &tree,
        );

        let selection = Selection {
            cursor: end_doc,
            anchor: start_doc,
            id: self.next_id,
        };
        if alt_held {
            self.selections.push(selection);
        } else {
            self.selections = vec![selection];
        }
        self.next_id += 1;
        true
    }

    /// Copy selection to clipboard
    pub fn copy(&mut self, doc: &Doc) {
        if let Some(sel) = self.selections.first().filter(|s| !s.is_cursor()) {
            let text = doc.read().flatten_to_string();
            let range = sel.byte_range(doc);
            if range.end <= text.len() {
                let selected = &text[range];
                self.clipboard = Some(selected.to_string());
                let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(selected));
            }
        }
    }

    /// Cut selection to clipboard
    pub fn cut(&mut self, doc: &Doc) {
        self.copy(doc);
        self.save_snapshot_to_history(doc);

        for sel in &self.selections {
            if !sel.is_cursor() {
                self.pending_edits.push(Edit::Delete {
                    range: sel.byte_range(doc),
                });
            }
        }

        self.flush_pending_edits(doc);

        for sel in &mut self.selections {
            sel.cursor = sel.min_pos();
            sel.anchor = sel.cursor;
        }
    }

    /// Paste from clipboard
    pub fn paste(&mut self, doc: &Doc) {
        let text = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .or_else(|| self.clipboard.clone());

        if let Some(text) = text {
            self.save_snapshot_to_history(doc);

            for sel in &self.selections {
                if !sel.is_cursor() {
                    self.pending_edits.push(Edit::Delete {
                        range: sel.byte_range(doc),
                    });
                }
                let tree = doc.read();
                self.pending_edits.push(Edit::Insert {
                    pos: tree.doc_pos_to_byte(sel.min_pos()),
                    content: Content::Text(text.clone()),
                });
            }

            self.flush_pending_edits(doc);

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
        self.selections = vec![Selection {
            cursor: DocPos {
                byte_offset: tree.byte_count(),
                line: last_line,
                column: Self::get_line_char_count(&tree, last_line),
            },
            anchor: DocPos::default(),
            id: self.next_id,
        }];
        self.next_id += 1;
    }

    /// Get current selections
    pub fn selections(&self) -> &[Selection] {
        &self.selections
    }

    /// Clear all selections except primary
    pub fn clear_selections(&mut self) {
        if !self.selections.is_empty() {
            self.selections.truncate(1);
        }
    }

    /// Get primary cursor position in document space
    pub fn primary_cursor_doc_pos(&self, doc: &Doc) -> crate::coordinates::DocPos {
        self.selections.first().map_or(DocPos::default(), |sel| {
            let tree = doc.read();
            DocPos {
                byte_offset: tree.doc_pos_to_byte(sel.cursor),
                line: sel.cursor.line,
                column: sel.cursor.column,
            }
        })
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

        let cursor_widget = self.selections.first().map(|sel| {
            let tree = doc.read();
            let line_text = Self::get_line_text(&tree, sel.cursor.line);
            widget::cursor(viewport.doc_to_layout_with_text(sel.cursor, &line_text))
        });

        let selection_widgets: Vec<_> = self
            .selections
            .iter()
            .filter(|sel| !sel.is_cursor())
            .filter_map(|sel| {
                let rects = sel.to_rectangles(doc, viewport);
                (!rects.is_empty()).then(|| widget::selection(rects))
            })
            .collect();

        (selection_widgets, cursor_widget)
    }

    /// Save current document state to history before making an edit
    fn save_snapshot_to_history(&mut self, doc: &Doc) {
        if self.pending_edits.is_empty()
            || self
                .last_edit_time
                .map_or(true, |t| t.elapsed().as_millis() > 500)
        {
            self.history.checkpoint(DocumentSnapshot {
                tree: doc.read(),
                selections: self.selections.clone(),
            });
        }
    }

    /// Perform undo operation
    pub fn undo(&mut self, doc: &Doc) -> bool {
        self.flush_pending_edits(doc);
        let current_snapshot = DocumentSnapshot {
            tree: doc.read(),
            selections: self.selections.clone(),
        };

        if let Some(prev) = self.history.undo(current_snapshot) {
            doc.replace_tree(prev.tree.clone());
            self.selections = prev.selections;
            self.next_id = self.selections.iter().map(|s| s.id).max().unwrap_or(0) + 1;
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
        self.flush_pending_edits(doc);
        let current_snapshot = DocumentSnapshot {
            tree: doc.read(),
            selections: self.selections.clone(),
        };

        if let Some(next) = self.history.redo(current_snapshot) {
            doc.replace_tree(next.tree.clone());
            self.selections = next.selections;
            self.next_id = self.selections.iter().map(|s| s.id).max().unwrap_or(0) + 1;
            if self.syntax_highlighter.is_some() {
                self.has_pending_syntax_update = true;
                self.last_edit_time = Some(Instant::now());
            }
            return true;
        }
        false
    }
}
