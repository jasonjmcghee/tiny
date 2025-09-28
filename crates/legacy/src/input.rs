//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::coordinates::Viewport;
use crate::history::{DocumentHistory, DocumentSnapshot, SelectionHistory};
use crate::input_types::{ElementState, Key, KeyEvent, Modifiers, MouseButton, NamedKey};
use crate::lsp_manager::TextChange;
use crate::syntax::SyntaxHighlighter;
use arboard::Clipboard;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_core::tree::{Content, Doc, Edit, Point, SearchOptions};
use tiny_sdk::{DocPos, LayoutPos, LayoutRect};

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
    pub fn to_rectangles(&self, _doc: &Doc, viewport: &Viewport) -> Vec<LayoutRect> {
        if self.is_cursor() {
            return Vec::new();
        }

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
    /// Accumulated LSP text changes for incremental updates
    pending_lsp_changes: Vec<TextChange>,
    /// History for undo/redo (document + selections)
    history: DocumentHistory,
    /// Navigation history for cursor positions (Cmd+[/])
    nav_history: SelectionHistory,
    /// Drag anchor in document coordinates (set when drag starts)
    drag_anchor: Option<DocPos>,
    /// Selection anchor - when set, cursor movements extend selection from this point
    /// Set when entering selection mode (shift pressed), cleared when leaving selection mode
    selection_anchor: Option<DocPos>,
    /// Track click count and timing for double/triple click detection
    last_click_time: Option<Instant>,
    last_click_pos: Option<DocPos>,
    click_count: u32,
    /// Ignore drag events after multi-click to prevent selection loss
    ignore_next_drag: bool,
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
            pending_lsp_changes: Vec::new(),
            history: DocumentHistory::new(),
            nav_history: SelectionHistory::with_max_size(50),
            drag_anchor: None,
            selection_anchor: None,
            last_click_time: None,
            last_click_pos: None,
            click_count: 0,
            ignore_next_drag: false,
        }
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

    /// Move cursor to a new position, handling selection based on current state
    fn move_cursor_to(&mut self, new_position: DocPos, extending_selection: bool) {
        if extending_selection {
            // When extending, use selection anchor if available, or current cursor if not
            if self.selection_anchor.is_none() {
                if let Some(sel) = self.selections.first() {
                    self.selection_anchor = Some(sel.anchor);
                }
            }

            // Update cursor while keeping anchor
            for sel in &mut self.selections {
                sel.cursor = new_position;
                if let Some(anchor) = self.selection_anchor {
                    sel.anchor = anchor;
                }
            }
        } else {
            // Not extending - move cursor and collapse selection
            for sel in &mut self.selections {
                sel.cursor = new_position;
                sel.anchor = new_position;
            }
            // Clear selection anchor when making a non-extending movement
            self.selection_anchor = None;
        }
    }

    /// Unified cursor movement
    fn move_cursor(
        &mut self,
        doc: &Doc,
        dx: i32,
        dy: i32,
        extending_selection: bool,
    ) -> InputAction {
        let tree = doc.read();

        // Handle vertical movement
        if dy != 0 {
            if self.goal_column.is_none() && !self.selections.is_empty() {
                self.goal_column = Some(self.selections[0].cursor.column);
            }

            // Get current cursor position
            let mut new_pos = self
                .selections
                .first()
                .map(|s| s.cursor)
                .unwrap_or_default();

            if dy < 0 && new_pos.line > 0 {
                new_pos.line -= 1;
            } else if dy > 0 && tree.line_to_byte(new_pos.line + 1).is_some() {
                new_pos.line += 1;
            }

            let line_length = tree.line_char_count(new_pos.line) as u32;
            new_pos.column = self.goal_column.unwrap_or(new_pos.column).min(line_length);
            new_pos.byte_offset = 0;
            self.move_cursor_to(new_pos, extending_selection);
        }

        // Handle horizontal movement
        if dx != 0 {
            self.goal_column = None;

            // Get current cursor position
            let mut new_pos = self
                .selections
                .first()
                .map(|s| s.cursor)
                .unwrap_or_default();

            if dx < 0 {
                if new_pos.column > 0 {
                    new_pos.column -= 1;
                } else if new_pos.line > 0 {
                    new_pos.line -= 1;
                    new_pos.column = tree.line_char_count(new_pos.line) as u32;
                }
            } else {
                let line_length = tree.line_char_count(new_pos.line) as u32;
                if new_pos.column < line_length {
                    new_pos.column += 1;
                } else if tree.line_to_byte(new_pos.line + 1).is_some() {
                    new_pos.line += 1;
                    new_pos.column = 0;
                }
            }
            new_pos.byte_offset = 0;
            self.move_cursor_to(new_pos, extending_selection);
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
                    // Simply move back one character, regardless of what it was
                    sel.cursor.column -= 1;
                } else if sel.cursor.line > 0 {
                    sel.cursor.line -= 1;
                    sel.cursor.column = if deleted.as_deref() == Some("\n") {
                        doc.read().line_char_count(sel.cursor.line) as u32
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
                // Tab is just one character in the document
                _ => sel.cursor.column += text.chars().count() as u32,
            }
            sel.anchor = sel.cursor;
        }
        InputAction::Redraw
    }

    /// Move cursor to start or end of line
    fn move_to_line_edge(
        &mut self,
        doc: &Doc,
        to_end: bool,
        extending_selection: bool,
    ) -> InputAction {
        self.goal_column = None;
        let tree = doc.read();

        if let Some(sel) = self.selections.first() {
            let new_pos = DocPos {
                line: sel.cursor.line,
                column: if to_end {
                    tree.line_char_count(sel.cursor.line) as u32
                } else {
                    0
                },
                byte_offset: 0,
            };
            self.move_cursor_to(new_pos, extending_selection);
        }

        InputAction::Redraw
    }

    /// Page navigation (up/down)
    fn page_jump(&mut self, doc: &Doc, up: bool, extending_selection: bool) -> InputAction {
        self.nav_history
            .checkpoint_if_changed(self.primary_cursor_doc_pos(doc));
        if self.goal_column.is_none() && !self.selections.is_empty() {
            self.goal_column = Some(self.selections[0].cursor.column);
        }

        const PAGE_SIZE: u32 = 20;

        if let Some(sel) = self.selections.first() {
            let tree = doc.read();
            let total_lines = tree.line_count();

            let new_line = if up {
                sel.cursor.line.saturating_sub(PAGE_SIZE)
            } else {
                (sel.cursor.line + PAGE_SIZE).min(total_lines.saturating_sub(1))
            };

            let line_length = tree.line_char_count(new_line) as u32;
            let new_pos = DocPos {
                line: new_line,
                column: self
                    .goal_column
                    .unwrap_or(sel.cursor.column)
                    .min(line_length),
                byte_offset: 0,
            };

            self.move_cursor_to(new_pos, extending_selection);
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
                .map_or(false, |t| t.elapsed().as_millis() > 50)
    }

    /// Get and clear pending LSP changes for incremental updates
    pub fn take_lsp_changes(&mut self) -> Vec<TextChange> {
        std::mem::take(&mut self.pending_lsp_changes)
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

        // Collect TextEdits for LATER syntax update and LSP changes
        for edit in &self.pending_edits {
            if self.syntax_highlighter.is_some() {
                let text_edit = crate::syntax::create_text_edit(&tree_before, edit);
                self.pending_text_edits.push(text_edit);
                self.has_pending_syntax_update = true;
            }

            // Track LSP changes for incremental updates
            let lsp_change = self.create_lsp_change(&tree_before, edit);
            self.pending_lsp_changes.push(lsp_change);

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
        _modifiers: &Modifiers,
    ) -> Vec<tiny_core::tree::Edit> {
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
        modifiers: &Modifiers,
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
        modifiers: &Modifiers,
    ) -> InputAction {
        self.on_key_internal(doc, viewport, event, modifiers, None)
    }

    /// Internal key handling with optional renderer
    fn on_key_internal(
        &mut self,
        doc: &Doc,
        _viewport: &Viewport,
        event: &KeyEvent,
        modifiers: &Modifiers,
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
            InputAction::None
        } else {
            match &event.logical_key {
                // Ignore modifier keys pressed alone
                Key::Named(NamedKey::Shift)
                | Key::Named(NamedKey::Control)
                | Key::Named(NamedKey::Alt)
                | Key::Named(NamedKey::Super) => InputAction::None,
                Key::Character(ch) if ch.chars().all(|c| !c.is_control()) => {
                    self.handle_character_input(doc, ch, renderer)
                }
                Key::Named(NamedKey::Backspace) => self.delete_at_cursor(doc, false),
                Key::Named(NamedKey::Delete) => self.delete_at_cursor(doc, true),
                Key::Named(NamedKey::Enter) => self.insert_text(doc, "\n"),
                Key::Named(NamedKey::Tab) => self.insert_text(doc, "\t"),
                Key::Named(NamedKey::Space) => self.insert_text(doc, " "),
                Key::Named(NamedKey::ArrowLeft) => self.move_cursor(doc, -1, 0, shift_held),
                Key::Named(NamedKey::ArrowRight) => self.move_cursor(doc, 1, 0, shift_held),
                Key::Named(NamedKey::ArrowUp) => self.move_cursor(doc, 0, -1, shift_held),
                Key::Named(NamedKey::ArrowDown) => self.move_cursor(doc, 0, 1, shift_held),
                Key::Named(NamedKey::Home) => self.move_to_line_edge(doc, false, shift_held),
                Key::Named(NamedKey::End) => self.move_to_line_edge(doc, true, shift_held),
                Key::Named(NamedKey::PageUp) => self.page_jump(doc, true, shift_held),
                Key::Named(NamedKey::PageDown) => self.page_jump(doc, false, shift_held),
                _ => InputAction::None,
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
        shift_held: bool,
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

        // Detect double/triple click
        const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(500);
        const CLICK_POS_TOLERANCE: u32 = 2; // Allow 2 character tolerance for position

        let now = Instant::now();
        let is_multi_click = if let (Some(last_time), Some(last_pos)) = (self.last_click_time, self.last_click_pos) {
            now.duration_since(last_time) < DOUBLE_CLICK_TIME
                && last_pos.line == doc_pos.line
                && last_pos.column.abs_diff(doc_pos.column) <= CLICK_POS_TOLERANCE
        } else {
            false
        };

        if is_multi_click {
            self.click_count += 1;
        } else {
            self.click_count = 1;
        }

        self.last_click_time = Some(now);
        self.last_click_pos = Some(doc_pos);

        // Handle multi-click selection
        if self.click_count == 2 && !shift_held && !alt_held {
            // Double-click: select word
            self.select_word_at(doc, doc_pos);
            self.ignore_next_drag = true; // Don't let drag events override the word selection
            return true;
        } else if self.click_count >= 3 && !shift_held && !alt_held {
            // Triple-click: select line
            self.select_line_at(doc, doc_pos);
            self.ignore_next_drag = true; // Don't let drag events override the line selection
            return true;
        }

        // Clear ignore flag on regular single click
        if self.click_count == 1 {
            self.ignore_next_drag = false;
        }

        // Normal click handling
        if shift_held {
            // Shift-click: extend selection from current position to click point
            if let Some(sel) = self.selections.first() {
                // Use existing anchor or cursor as the selection start
                let anchor = if sel.anchor != sel.cursor {
                    // Already have a selection, keep its anchor
                    sel.anchor
                } else {
                    // No selection yet, use current cursor as anchor
                    sel.cursor
                };
                // Don't set selection_anchor here - that's only for keyboard-based selection extension
                self.selections = vec![Selection {
                    cursor: doc_pos,
                    anchor,
                    id: self.next_id,
                }];
            } else {
                // No existing selection, create one
                self.selections = vec![Selection {
                    cursor: doc_pos,
                    anchor: doc_pos,
                    id: self.next_id,
                }];
            }
        } else if alt_held {
            // Alt-click: add a new cursor
            self.selections.push(Selection {
                cursor: doc_pos,
                anchor: doc_pos,
                id: self.next_id,
            });
        } else {
            // Regular click: start fresh selection at click point
            self.selection_anchor = None; // Clear any existing selection mode
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
        _from: Point, // Unused - we use the stored drag_anchor
        to: Point,
        alt_held: bool,
    ) -> (bool, Option<(f32, f32)>) {
        // Ignore drag events if we just did a double/triple click
        if self.ignore_next_drag {
            return (true, None);
        }

        // Use the stored drag anchor, or calculate it if missing
        let anchor_doc = self.drag_anchor.unwrap_or_else(|| {
            self.selections
                .first()
                .map(|s| s.anchor)
                .unwrap_or_default()
        });

        // Calculate cursor position (where we're dragging to) in DOCUMENT coordinates
        // This is crucial - we convert to document space so the position doesn't drift with scroll
        let tree = doc.read();
        let end_doc = viewport.layout_to_doc_with_tree(
            LayoutPos {
                x: to.x + viewport.scroll.x,
                y: to.y + viewport.scroll.y,
            },
            &tree,
        );

        // Update selection using the anchor we stored at click time
        // This ensures consistent selection behavior regardless of drag direction
        let selection = Selection {
            cursor: end_doc,
            anchor: anchor_doc, // Always use the original click position as anchor
            id: self.next_id,
        };

        if alt_held {
            self.selections.push(selection);
        } else {
            self.selections = vec![selection];
        }
        self.next_id += 1;

        // Calculate scroll delta based on mouse position relative to VIEWPORT edges
        // Only scroll when mouse is outside the text area
        let mut scroll_delta = (0.0, 0.0);

        // Check if mouse is outside viewport boundaries for scrolling
        let margin_x = viewport.margin.x.0;
        let text_area_right = viewport.logical_size.width.0 - margin_x;

        // Vertical scrolling - only when mouse is outside viewport vertically
        if to.y.0 < 0.0 {
            // Above viewport - scroll up
            scroll_delta.1 = -3.0; // Fixed scroll speed
        } else if to.y.0 > viewport.logical_size.height.0 {
            // Below viewport - scroll down
            scroll_delta.1 = 3.0; // Fixed scroll speed
        }

        // Horizontal scrolling - only when mouse is outside text area horizontally
        if to.x.0 < margin_x {
            // Left of text area - scroll left
            scroll_delta.0 = -3.0; // Fixed scroll speed
        } else if to.x.0 > text_area_right {
            // Right of text area - scroll right
            scroll_delta.0 = 3.0; // Fixed scroll speed
        }

        let needs_scroll = scroll_delta.0 != 0.0 || scroll_delta.1 != 0.0;
        (
            true,
            if needs_scroll {
                Some(scroll_delta)
            } else {
                None
            },
        )
    }

    /// Copy selection to clipboard
    pub fn copy(&mut self, doc: &Doc) {
        if let Some(sel) = self.selections.first().filter(|s| !s.is_cursor()) {
            let text = doc.read().flatten_to_string();
            let range = sel.byte_range(doc);
            if range.end <= text.len() {
                let selected = &text[range];
                self.clipboard = Some(selected.to_string());
                let _ = Clipboard::new().and_then(|mut c| c.set_text(selected));
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
        let text = Clipboard::new()
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
                column: tree.line_char_count(last_line) as u32,
            },
            anchor: DocPos::default(),
            id: self.next_id,
        }];
        self.next_id += 1;
    }

    /// Select word at the given position (for double-click)
    fn select_word_at(&mut self, doc: &Doc, click_pos: DocPos) {
        let tree = doc.read();
        let click_byte = tree.doc_pos_to_byte(click_pos);

        // Define word boundary characters
        let word_boundaries = " \t\n()[]{}\"'`,;:.!?<>@#$%^&*+=|\\~-/";

        // Search backwards for word start
        let mut word_start_byte = click_byte;
        for boundary in word_boundaries.chars() {
            let pattern = boundary.to_string();
            let options = SearchOptions::default();
            if let Some(prev_match) = tree.search_prev(&pattern, click_byte, options) {
                if prev_match.byte_range.end > word_start_byte || word_start_byte == click_byte {
                    word_start_byte = prev_match.byte_range.end;
                }
            }
        }

        // Search forwards for word end
        let mut word_end_byte = click_byte;
        for boundary in word_boundaries.chars() {
            let pattern = boundary.to_string();
            let options = SearchOptions::default();
            if let Some(next_match) = tree.search_next(&pattern, click_byte, options) {
                if word_end_byte == click_byte || next_match.byte_range.start < word_end_byte {
                    word_end_byte = next_match.byte_range.start;
                }
            }
        }

        // If no boundaries found, use document bounds
        if word_start_byte == click_byte && word_end_byte == click_byte {
            // Check beginning of document
            if click_byte == 0 || word_start_byte == 0 {
                word_start_byte = 0;
            }
            // Check end of document
            if word_end_byte == click_byte {
                word_end_byte = tree.byte_count();
            }
        }

        // Convert byte positions back to DocPos
        let word_start_line = tree.byte_to_line(word_start_byte);
        let word_start_line_byte = tree.line_to_byte(word_start_line).unwrap_or(0);
        let word_start_column = tree.get_text_slice(word_start_line_byte..word_start_byte).chars().count() as u32;
        let word_start = DocPos {
            line: word_start_line,
            column: word_start_column,
            byte_offset: 0,
        };

        let word_end_line = tree.byte_to_line(word_end_byte);
        let word_end_line_byte = tree.line_to_byte(word_end_line).unwrap_or(0);
        let word_end_column = tree.get_text_slice(word_end_line_byte..word_end_byte).chars().count() as u32;
        let word_end = DocPos {
            line: word_end_line,
            column: word_end_column,
            byte_offset: 0,
        };

        // Create selection from word start to end
        self.selections = vec![Selection {
            cursor: word_end,
            anchor: word_start,
            id: self.next_id,
        }];
        self.next_id += 1;
    }

    /// Select entire line at the given position (for triple-click)
    fn select_line_at(&mut self, doc: &Doc, click_pos: DocPos) {
        let tree = doc.read();

        // Get the start of the line
        let line_start = DocPos {
            line: click_pos.line,
            column: 0,
            byte_offset: 0,
        };

        // Get the end of the line (including newline if present)
        let line_char_count = tree.line_char_count(click_pos.line) as u32;
        let mut line_end = DocPos {
            line: click_pos.line,
            column: line_char_count,
            byte_offset: 0,
        };

        // If not the last line, include the newline character
        if click_pos.line < tree.line_count() - 1 {
            // Move to start of next line to include the newline
            line_end.line += 1;
            line_end.column = 0;
        }

        // Create selection for the entire line
        self.selections = vec![Selection {
            cursor: line_end,
            anchor: line_start,
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

    /// Clear the drag anchor (called when mouse button is released)
    pub fn clear_drag_anchor(&mut self) {
        self.drag_anchor = None;
        // Clear the ignore flag when mouse is released
        self.ignore_next_drag = false;
    }

    /// Get primary cursor position in document space
    pub fn primary_cursor_doc_pos(&self, doc: &Doc) -> DocPos {
        self.selections.first().map_or(DocPos::default(), |sel| {
            let tree = doc.read();
            DocPos {
                byte_offset: tree.doc_pos_to_byte(sel.cursor),
                line: sel.cursor.line,
                column: sel.cursor.column,
            }
        })
    }

    /// Get selection data for plugins: cursor position and selection positions
    pub fn get_selection_data(
        &self,
        doc: &Doc,
        viewport: &Viewport,
    ) -> (
        Option<tiny_sdk::LayoutPos>,
        Vec<(tiny_sdk::ViewPos, tiny_sdk::ViewPos)>, // Selection start/end positions in view coordinates
    ) {
        let cursor_pos = self.selections.first().map(|sel| {
            let tree = doc.read();
            let line_text = tree.line_text(sel.cursor.line);
            viewport.doc_to_layout_with_text(sel.cursor, &line_text)
        });

        // Collect selection positions in view coordinates (with scroll applied)
        let tree = doc.read();
        let selection_positions: Vec<(tiny_sdk::ViewPos, tiny_sdk::ViewPos)> = self
            .selections
            .iter()
            .filter(|sel| !sel.is_cursor())
            .map(|sel| {
                // Return normalized start/end positions
                let (start, end) = if sel.min_pos() == sel.cursor {
                    (sel.cursor, sel.anchor)
                } else {
                    (sel.anchor, sel.cursor)
                };

                // Convert to layout positions with proper font metrics
                let start_line_text = tree.line_text(start.line);
                let end_line_text = tree.line_text(end.line);
                let start_layout = viewport.doc_to_layout_with_text(start, &start_line_text);
                let end_layout = viewport.doc_to_layout_with_text(end, &end_line_text);

                // Convert to view positions (apply scroll)
                let start_view = viewport.layout_to_view(start_layout);
                let end_view = viewport.layout_to_view(end_layout);

                (start_view, end_view)
            })
            .collect();

        (cursor_pos, selection_positions)
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

    /// Create an LSP TextChange from a document edit
    fn create_lsp_change(&self, tree: &tiny_core::tree::Tree, edit: &Edit) -> TextChange {
        use lsp_types::{Position, Range as LspRange};

        match edit {
            Edit::Insert { pos, content } => {
                // For insert, the range is just the insertion point
                let start_line = tree.byte_to_line(*pos);
                let line_start_byte = tree.line_to_byte(start_line).unwrap_or(0);
                let start_char = tree.get_text_slice(line_start_byte..*pos).chars().count() as u32;

                TextChange {
                    range: LspRange {
                        start: Position {
                            line: start_line,
                            character: start_char,
                        },
                        end: Position {
                            line: start_line,
                            character: start_char,
                        },
                    },
                    text: match content {
                        Content::Text(s) => s.clone(),
                        Content::Spatial(_) => String::new(), // Shouldn't happen for LSP
                    },
                }
            }
            Edit::Delete { range } => {
                // For delete, provide the range being deleted
                let start_line = tree.byte_to_line(range.start);
                let start_line_byte = tree.line_to_byte(start_line).unwrap_or(0);
                let start_char = tree.get_text_slice(start_line_byte..range.start).chars().count() as u32;

                let end_line = tree.byte_to_line(range.end);
                let end_line_byte = tree.line_to_byte(end_line).unwrap_or(0);
                let end_char = tree.get_text_slice(end_line_byte..range.end).chars().count() as u32;

                TextChange {
                    range: LspRange {
                        start: Position {
                            line: start_line,
                            character: start_char,
                        },
                        end: Position {
                            line: end_line,
                            character: end_char,
                        },
                    },
                    text: String::new(), // Empty string for deletion
                }
            }
            Edit::Replace { range, content } => {
                // For replace, provide the range being replaced and the new text
                let start_line = tree.byte_to_line(range.start);
                let start_line_byte = tree.line_to_byte(start_line).unwrap_or(0);
                let start_char = tree.get_text_slice(start_line_byte..range.start).chars().count() as u32;

                let end_line = tree.byte_to_line(range.end);
                let end_line_byte = tree.line_to_byte(end_line).unwrap_or(0);
                let end_char = tree.get_text_slice(end_line_byte..range.end).chars().count() as u32;

                TextChange {
                    range: LspRange {
                        start: Position {
                            line: start_line,
                            character: start_char,
                        },
                        end: Position {
                            line: end_line,
                            character: end_char,
                        },
                    },
                    text: match content {
                        Content::Text(s) => s.clone(),
                        Content::Spatial(_) => String::new(), // Shouldn't happen for LSP
                    },
                }
            }
        }
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
