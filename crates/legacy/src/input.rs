//! Input handling and selection management
//!
//! Handles keyboard, mouse, and multi-cursor selections

use crate::coordinates::Viewport;
use crate::history::{DocumentHistory, DocumentSnapshot, SelectionHistory};
use crate::input_types::MouseButton;
use crate::lsp_manager::TextChange;
use crate::syntax::SyntaxHighlighter;
use crate::text_editor_plugin::TextEditorPlugin;
use arboard::Clipboard;
use serde_json::Value;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_core::tree::{Content, Doc, Edit, Point, SearchOptions};
use tiny_sdk::{DocPos, LayoutPos, LayoutRect};

/// Actions that can be triggered by input
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputAction {
    None,
    Redraw,
    Save,
    Undo,
    Redo,
}

/// Input mode (for vim-like modal editing support)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputMode {
    /// Default mode - all standard editor shortcuts work
    Insert,
    /// Vim-like normal mode - navigation and commands
    Normal,
    /// Vim-like visual mode - text selection
    Visual,
    /// Custom modes for extensions
    Custom(&'static str),
}

impl Default for InputMode {
    fn default() -> Self {
        Self::Insert
    }
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
            let viewport_right = viewport.bounds.width.0;

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
                    0.0,
                    start_layout.y.0 + line_height,
                    viewport.bounds.width.0,
                    (end.line - start.line - 1) as f32 * line_height,
                ));
            }

            // Last line
            rects.push(LayoutRect::new(
                0.0,
                end_layout.y.0,
                end_layout.x.0 - 2.0,
                line_height,
            ));
        }
        rects
    }
}

/// Event with JSON data payload
#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub data: Value,
    pub priority: i32,
    pub timestamp: Instant,
    pub source: String,
}

/// Controls event propagation through the subscription chain
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropagationControl {
    /// Continue delivering to next subscriber
    Continue,
    /// Stop propagation (event handled)
    Stop,
}

/// Trait for components that subscribe to events
/// Components self-filter based on visibility/focus instead of explicit routing
pub trait EventSubscriber {
    /// Handle an event, can emit follow-up events, return whether to stop propagation
    fn handle_event(&mut self, event: &Event, event_bus: &mut EventBus) -> PropagationControl;

    /// Subscription priority (higher = earlier delivery)
    /// Overlays typically have priority 100+, main editor priority 0
    fn priority(&self) -> i32;

    /// Whether this subscriber is currently active (visible + focused)
    fn is_active(&self) -> bool;
}

/// Simple event bus for queuing and prioritizing events
/// Components subscribe and handle events based on priority
pub struct EventBus {
    pub queued: Vec<Event>,
    /// Callback to wake up the main thread when events are emitted (e.g., request redraw)
    wake_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl EventBus {
    /// Create a new event bus
    pub fn new() -> Self {
        Self {
            queued: Vec::new(),
            wake_notifier: None,
        }
    }

    /// Set callback to wake up the main thread when events are emitted
    pub fn set_wake_notifier<F>(&mut self, notifier: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.wake_notifier = Some(Arc::new(notifier));
    }

    /// Emit an event to the queue
    pub fn emit(
        &mut self,
        name: impl Into<String>,
        data: Value,
        priority: i32,
        source: impl Into<String>,
    ) {
        self.queued.push(Event {
            name: name.into(),
            data,
            priority,
            timestamp: Instant::now(),
            source: source.into(),
        });

        // Wake up the main thread to process this event
        if let Some(notifier) = &self.wake_notifier {
            notifier();
        }
    }

    /// Get and consume all queued events, sorted by priority
    /// Lower priority number = higher priority (processed first)
    pub fn drain_sorted(&mut self) -> Vec<Event> {
        let mut events = std::mem::take(&mut self.queued);
        events.sort_by_key(|e| e.priority);
        events
    }

    /// Check if there are pending events
    pub fn has_events(&self) -> bool {
        !self.queued.is_empty()
    }
}

/// Dispatch an event to subscribers in priority order
/// Stops delivery when a subscriber returns PropagationControl::Stop
/// Only delivers to active subscribers
/// Subscribers can emit follow-up events via event_bus
pub fn dispatch_event(event: &Event, subscribers: &mut [&mut dyn EventSubscriber], event_bus: &mut EventBus) {
    // Sort by priority (higher priority = earlier delivery)
    let mut indexed: Vec<(usize, i32)> = subscribers
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.priority()))
        .collect();
    indexed.sort_by(|a, b| b.1.cmp(&a.1)); // Descending order

    // Deliver to subscribers in priority order
    for (index, _) in indexed {
        let subscriber = &mut subscribers[index];
        if subscriber.is_active() {
            if subscriber.handle_event(event, event_bus) == PropagationControl::Stop {
                break;
            }
        }
    }
}

/// Thread-safe wrapper for EventBus (for plugin access)
pub struct SharedEventBus {
    inner: Arc<Mutex<EventBus>>,
}

impl SharedEventBus {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bus)),
        }
    }

    pub fn emit(
        &self,
        name: impl Into<String>,
        data: Value,
        priority: i32,
        source: impl Into<String>,
    ) {
        self.inner
            .lock()
            .unwrap()
            .emit(name, data, priority, source);
    }

    pub fn set_wake_notifier<F>(&self, notifier: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.inner.lock().unwrap().set_wake_notifier(notifier);
    }

    pub fn with_bus<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut EventBus) -> R,
    {
        let mut bus = self.inner.lock().unwrap();
        f(&mut bus)
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
    /// Accumulated edits for syntax token adjustment
    pending_renderer_edits: Vec<tiny_core::tree::Edit>,
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
    /// Time of last undo checkpoint for grouping edits
    last_checkpoint_time: Option<Instant>,
    /// Current input mode (for vim-like modal editing)
    current_mode: InputMode,
    /// Pending scroll delta from drag operations (to be consumed by app)
    pub pending_scroll_delta: Option<(f32, f32)>,
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
            pending_renderer_edits: Vec::new(),
            history: DocumentHistory::new(),
            nav_history: SelectionHistory::with_max_size(50),
            drag_anchor: None,
            selection_anchor: None,
            last_click_time: None,
            last_click_pos: None,
            click_count: 0,
            ignore_next_drag: false,
            last_checkpoint_time: None,
            current_mode: InputMode::default(),
            pending_scroll_delta: None,
        }
    }

    /// Get current input mode
    pub fn current_mode(&self) -> InputMode {
        self.current_mode
    }

    /// Set input mode (for external mode switching)
    pub fn set_mode(&mut self, mode: InputMode) {
        self.current_mode = mode;
    }

    /// Helper to convert byte offset to DocPos
    fn byte_to_doc_pos(&self, tree: &tiny_core::tree::Tree, byte_offset: usize) -> DocPos {
        let line = tree.byte_to_line(byte_offset);
        let line_start_byte = tree.line_to_byte(line).unwrap_or(0);
        let column = tree
            .get_text_slice(line_start_byte..byte_offset)
            .chars()
            .count() as u32;
        DocPos {
            line,
            column,
            byte_offset: 0,
        }
    }

    /// Handle an event (new event system)
    /// Returns the action that should be taken
    pub fn handle_event(&mut self, event: &Event, doc: &Doc, viewport: &Viewport) -> InputAction {
        match event.name.as_str() {
            // Text insertion
            "editor.insert_char" => {
                if let Some(ch) = event.data.get("char").and_then(|v| v.as_str()) {
                    self.insert_text(doc, ch)
                } else {
                    InputAction::None
                }
            }
            "editor.insert_newline" => self.insert_text(doc, "\n"),
            "editor.insert_tab" => self.insert_text(doc, "\t"),
            "editor.insert_space" => self.insert_text(doc, " "),

            // Deletion
            "editor.delete_backward" => self.delete_at_cursor(doc, false),
            "editor.delete_forward" => self.delete_at_cursor(doc, true),

            // Clipboard
            "editor.copy" => {
                self.copy(doc);
                InputAction::None
            }
            "editor.cut" => {
                self.cut(doc);
                InputAction::Redraw
            }
            "editor.paste" => {
                self.paste(doc);
                InputAction::Redraw
            }
            "editor.select_all" => {
                self.select_all(doc);
                InputAction::Redraw
            }

            // Cursor movement
            "editor.move_left" => self.move_cursor(doc, -1, 0, false),
            "editor.move_right" => self.move_cursor(doc, 1, 0, false),
            "editor.move_up" => self.move_cursor(doc, 0, -1, false),
            "editor.move_down" => self.move_cursor(doc, 0, 1, false),
            "editor.extend_left" => self.move_cursor(doc, -1, 0, true),
            "editor.extend_right" => self.move_cursor(doc, 1, 0, true),
            "editor.extend_up" => self.move_cursor(doc, 0, -1, true),
            "editor.extend_down" => self.move_cursor(doc, 0, 1, true),
            "editor.move_line_start" => self.move_to_line_edge(doc, false, false),
            "editor.move_line_end" => self.move_to_line_edge(doc, true, false),
            "editor.extend_line_start" => self.move_to_line_edge(doc, false, true),
            "editor.extend_line_end" => self.move_to_line_edge(doc, true, true),
            "editor.page_up" => self.page_jump(doc, true, false),
            "editor.page_down" => self.page_jump(doc, false, false),
            "editor.extend_page_up" => self.page_jump(doc, true, true),
            "editor.extend_page_down" => self.page_jump(doc, false, true),

            // History
            "editor.undo" => InputAction::Undo,
            "editor.redo" => InputAction::Redo,

            // File operations
            "editor.save" => InputAction::Save,

            // Mouse events
            "mouse.press" => self.handle_mouse_press(event, doc, viewport),
            "mouse.drag" => self.handle_mouse_drag_event(event, doc, viewport),
            "mouse.release" => {
                self.clear_drag_anchor();
                InputAction::None
            }

            _ => InputAction::None,
        }
    }

    /// Handle mouse press events
    fn handle_mouse_press(&mut self, event: &Event, doc: &Doc, viewport: &Viewport) -> InputAction {
        let (x, y) = match (
            event.data.get("x").and_then(|v| v.as_f64()),
            event.data.get("y").and_then(|v| v.as_f64()),
        ) {
            (Some(x), Some(y)) => (x, y),
            _ => return InputAction::None,
        };

        let modifiers = event.data.get("modifiers");
        let shift_held = modifiers
            .and_then(|m| m.get("shift"))
            .and_then(|s| s.as_bool())
            .unwrap_or(false);
        let alt_held = modifiers
            .and_then(|m| m.get("alt"))
            .and_then(|a| a.as_bool())
            .unwrap_or(false);

        let pos = Point {
            x: tiny_sdk::LogicalPixels(x as f32),
            y: tiny_sdk::LogicalPixels(y as f32),
        };

        // Store drag anchor in document coordinates (properly clamped to document bounds)
        let tree = doc.read();
        self.drag_anchor = Some(viewport.layout_to_doc_with_tree(
            tiny_sdk::LayoutPos {
                x: tiny_sdk::LogicalPixels(x as f32 + viewport.scroll.x.0),
                y: tiny_sdk::LogicalPixels(y as f32 + viewport.scroll.y.0),
            },
            &tree,
        ));
        drop(tree);

        // Handle the click
        self.on_mouse_click(doc, viewport, pos, MouseButton::Left, alt_held, shift_held);

        InputAction::Redraw
    }

    /// Handle mouse drag events
    fn handle_mouse_drag_event(
        &mut self,
        event: &Event,
        doc: &Doc,
        viewport: &Viewport,
    ) -> InputAction {
        let (from_x, from_y, to_x, to_y) = match (
            event.data.get("from_x").and_then(|v| v.as_f64()),
            event.data.get("from_y").and_then(|v| v.as_f64()),
            event.data.get("to_x").and_then(|v| v.as_f64()),
            event.data.get("to_y").and_then(|v| v.as_f64()),
        ) {
            (Some(fx), Some(fy), Some(tx), Some(ty)) => (fx, fy, tx, ty),
            _ => return InputAction::None,
        };

        let alt_held = event
            .data
            .get("alt")
            .and_then(|a| a.as_bool())
            .unwrap_or(false);

        let from = Point {
            x: tiny_sdk::LogicalPixels(from_x as f32),
            y: tiny_sdk::LogicalPixels(from_y as f32),
        };
        let to = Point {
            x: tiny_sdk::LogicalPixels(to_x as f32),
            y: tiny_sdk::LogicalPixels(to_y as f32),
        };

        let (redraw, scroll_delta) = self.on_mouse_drag(doc, viewport, from, to, alt_held);

        // Store scroll delta for app to consume
        self.pending_scroll_delta = scroll_delta;

        if redraw {
            InputAction::Redraw
        } else {
            InputAction::None
        }
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
    pub fn move_cursor(
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
    pub fn delete_at_cursor(&mut self, doc: &Doc, forward: bool) -> InputAction {
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
                        // When at column 0, we're deleting a newline - capture the previous line's length NOW
                        let target_column =
                            if sel.cursor.column == 0 && deleted == "\n" && sel.cursor.line > 0 {
                                Some(tree.line_char_count(sel.cursor.line - 1) as u32)
                            } else {
                                None
                            };
                        deleted_info.push(Some((deleted.clone(), target_column)));
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
            for (sel, deleted_info) in self.selections.iter_mut().zip(deleted_info.iter()) {
                if !sel.is_cursor() {
                    sel.cursor = sel.min_pos();
                } else if sel.cursor.column > 0 {
                    // Simply move back one character, regardless of what it was
                    sel.cursor.column -= 1;
                } else if sel.cursor.line > 0 {
                    sel.cursor.line -= 1;
                    sel.cursor.column = if let Some((deleted, target_column)) = deleted_info {
                        if deleted == "\n" {
                            // Use the pre-captured column position
                            target_column.unwrap_or(0)
                        } else {
                            0
                        }
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
    pub fn insert_text(&mut self, doc: &Doc, text: &str) -> InputAction {
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

    /// Get and clear pending renderer edits for syntax token adjustment
    pub fn take_renderer_edits(&mut self) -> Vec<tiny_core::tree::Edit> {
        std::mem::take(&mut self.pending_renderer_edits)
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

        if let Some(ref syntax_hl) = self.syntax_highlighter {
            let text_after = doc.read().flatten_to_string();

            // If we have multiple edits, we can't send them all to tree-sitter
            // (it only accepts one InputEdit at a time), so reset the tree and do a fresh parse
            if self.pending_text_edits.len() == 1 {
                // Single edit - use incremental parsing
                let edit = &self.pending_text_edits[0];
                syntax_hl.request_update_with_edit(&text_after, doc.version(), Some(edit.clone()));
            } else {
                // Multiple edits - reset tree and do fresh parse
                syntax_hl.request_update_with_reset(&text_after, doc.version(), None, true);
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

            // Track edits for renderer (syntax token adjustment)
            self.pending_renderer_edits.push(edit.clone());

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
        const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(300);
        const CLICK_POS_TOLERANCE: u32 = 2; // Allow 2 character tolerance for position

        let now = Instant::now();
        let is_multi_click = if let (Some(last_time), Some(last_pos)) =
            (self.last_click_time, self.last_click_pos)
        {
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
            self.drag_anchor = None; // Clear any leftover drag anchor
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

        // Use the stored drag anchor if available, otherwise fallback to current selection anchor
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
        let text_area_right = viewport.bounds.width.0;

        // Vertical scrolling - only when mouse is outside viewport vertically
        if to.y.0 < 0.0 {
            // Above viewport - scroll up
            scroll_delta.1 = -3.0; // Fixed scroll speed
        } else if to.y.0 > viewport.logical_size.height.0 {
            // Below viewport - scroll down
            scroll_delta.1 = 3.0; // Fixed scroll speed
        }

        // Horizontal scrolling - only when mouse is outside text area horizontally
        if to.x.0 < 0.0 {
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

        // Convert byte positions back to DocPos using helper
        let word_start = self.byte_to_doc_pos(&tree, word_start_byte);
        let word_end = self.byte_to_doc_pos(&tree, word_end_byte);

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

    /// Set cursor position (for navigation)
    pub fn set_cursor(&mut self, pos: DocPos) {
        self.selections = vec![Selection {
            cursor: pos,
            anchor: pos,
            id: self.next_id,
        }];
        self.next_id += 1;
        self.goal_column = None;
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
    /// Uses a 1 second debounce to group rapid edits together for undo/redo
    fn save_snapshot_to_history(&mut self, doc: &Doc) {
        // Only save checkpoint if this is the first edit OR enough time has passed since last checkpoint
        // This groups rapid edits (typing) into a single undo/redo group
        if self
            .last_checkpoint_time
            .map_or(true, |t| t.elapsed().as_millis() > 1000)
        {
            self.history.checkpoint(DocumentSnapshot {
                tree: doc.read(),
                selections: self.selections.clone(),
            });
            // Update checkpoint time (not edit time - we want to measure time since last checkpoint)
            self.last_checkpoint_time = Some(Instant::now());
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

            // Clear accumulated renderer edits - they're invalid for the undone tree
            self.pending_renderer_edits.clear();

            // Reset checkpoint time so next edit starts a new undo group
            self.last_checkpoint_time = None;

            // Request syntax update after undo (tree has changed significantly)
            if let Some(ref syntax_hl) = self.syntax_highlighter {
                let text = doc.read().flatten_to_string();
                syntax_hl.request_update_with_reset(&text, doc.version(), None, true);
            }

            return true;
        }
        false
    }

    /// Helper to convert byte offset to LSP Position
    fn byte_to_lsp_position(
        &self,
        tree: &tiny_core::tree::Tree,
        byte_offset: usize,
    ) -> lsp_types::Position {
        let line = tree.byte_to_line(byte_offset);
        let line_start_byte = tree.line_to_byte(line).unwrap_or(0);
        let character = tree
            .get_text_slice(line_start_byte..byte_offset)
            .chars()
            .count() as u32;
        lsp_types::Position { line, character }
    }

    /// Create an LSP TextChange from a document edit
    fn create_lsp_change(&self, tree: &tiny_core::tree::Tree, edit: &Edit) -> TextChange {
        use lsp_types::Range as LspRange;

        match edit {
            Edit::Insert { pos, content } => {
                let position = self.byte_to_lsp_position(tree, *pos);
                TextChange {
                    range: LspRange {
                        start: position,
                        end: position,
                    },
                    text: match content {
                        Content::Text(s) => s.clone(),
                        Content::Spatial(_) => String::new(),
                    },
                }
            }
            Edit::Delete { range } => TextChange {
                range: LspRange {
                    start: self.byte_to_lsp_position(tree, range.start),
                    end: self.byte_to_lsp_position(tree, range.end),
                },
                text: String::new(),
            },
            Edit::Replace { range, content } => TextChange {
                range: LspRange {
                    start: self.byte_to_lsp_position(tree, range.start),
                    end: self.byte_to_lsp_position(tree, range.end),
                },
                text: match content {
                    Content::Text(s) => s.clone(),
                    Content::Spatial(_) => String::new(),
                },
            },
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

            // Clear accumulated renderer edits - they're invalid for the redone tree
            self.pending_renderer_edits.clear();

            // Reset checkpoint time so next edit starts a new undo group
            self.last_checkpoint_time = None;

            // Request syntax update after redo (tree has changed significantly)
            if let Some(ref syntax_hl) = self.syntax_highlighter {
                let text = doc.read().flatten_to_string();
                syntax_hl.request_update_with_reset(&text, doc.version(), None, true);
            }

            return true;
        }
        false
    }
}

/// Handle input actions at the plugin level
/// Returns true if the action was handled and requires a redraw
/// Note: InputAction::Save should be handled by the caller since it needs EditorLogic
pub fn handle_input_action(action: InputAction, plugin: &mut TextEditorPlugin) -> bool {
    match action {
        InputAction::Save => {
            // Save should be handled by caller (needs EditorLogic)
            eprintln!("Warning: Save action should be handled by caller");
            false
        }
        InputAction::Undo => plugin.editor.input.undo(&plugin.editor.view.doc),
        InputAction::Redo => plugin.editor.input.redo(&plugin.editor.view.doc),
        InputAction::Redraw => true,
        InputAction::None => false,
    }
}
