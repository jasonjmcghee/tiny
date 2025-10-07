//! EditableTextView - TextView + Editing Capabilities
//!
//! Builds on TextView's interactive capabilities to add:
//! - Cursor rendering and management
//! - Text editing (insert, delete, replace)
//! - Undo/redo history
//! - Advanced clipboard (cut, paste)
//! - Edit modes (single-line, multi-line, read-only)
//!
//! TextView already provides:
//! - Selection, mouse interaction, keyboard navigation
//! - Copy, select all, focus management
//! - Scrolling, syntax highlighting

use crate::{
    coordinates::Viewport,
    input::{InputHandler, Selection},
    text_view::TextView,
};
use tiny_core::tree::Point;
use tiny_sdk::LayoutPos;
use tiny_ui::{ArrowDirection, TextViewCapabilities};

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

/// Unique ID for an editable text view (for focus tracking)
static NEXT_VIEW_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

    /// Unique ID for focus tracking
    pub id: u64,

    /// Cursor rendering plugin (owned by this view)
    pub cursor_plugin: Option<Box<dyn tiny_sdk::Plugin>>,

    /// Selection rendering plugin (owned by this view)
    pub selection_plugin: Option<Box<dyn tiny_sdk::Plugin>>,
}

impl EditableTextView {
    /// Create a new editable text view
    pub fn new(view: TextView, mode: EditMode) -> Self {
        // Ensure TextView has appropriate capabilities for the mode
        let mut view = view;
        match mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                // Editable modes need full editing capabilities
                view.set_capabilities(TextViewCapabilities::editable());
                view.set_focused(true); // Auto-focus editable views
            }
            EditMode::ReadOnly { allow_selection } => {
                // Read-only mode: selectable or completely read-only
                let caps = if allow_selection {
                    TextViewCapabilities::selectable()
                } else {
                    TextViewCapabilities::read_only()
                };
                view.set_capabilities(caps);
            }
        }

        Self {
            view,
            input: InputHandler::new(),
            mode,
            show_cursor: true,
            on_submit: None,
            id: NEXT_VIEW_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            cursor_plugin: None,
            selection_plugin: None,
        }
    }

    /// Create a single-line input
    pub fn single_line(viewport: Viewport) -> Self {
        let view = TextView::with_capabilities(
            tiny_core::tree::Doc::new(),
            viewport,
            TextViewCapabilities::editable(),
        )
        .with_padding(8.0); // Default padding for input fields

        Self::new(view, EditMode::SingleLine)
    }

    /// Create a multi-line editor
    pub fn multi_line(viewport: Viewport) -> Self {
        let view = TextView::with_capabilities(
            tiny_core::tree::Doc::new(),
            viewport,
            TextViewCapabilities::editable(),
        );

        Self::new(view, EditMode::MultiLine)
    }

    /// Create a read-only view with selection
    pub fn read_only(viewport: Viewport, allow_selection: bool) -> Self {
        let caps = if allow_selection {
            TextViewCapabilities::selectable()
        } else {
            TextViewCapabilities::read_only()
        };

        let view = TextView::with_capabilities(
            tiny_core::tree::Doc::new(),
            viewport,
            caps,
        );

        Self::new(view, EditMode::ReadOnly { allow_selection })
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
    /// screen → local: subtract bounds, subtract padding, add scroll → layout space
    fn screen_to_local(&self, screen_pos: Point) -> Point {
        Point {
            x: tiny_sdk::LogicalPixels(
                screen_pos.x.0 - self.view.viewport.bounds.x.0 - self.view.padding_x + self.view.viewport.scroll.x.0,
            ),
            y: tiny_sdk::LogicalPixels(
                screen_pos.y.0 - self.view.viewport.bounds.y.0 - self.view.padding_y + self.view.viewport.scroll.y.0,
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

    /// Get cursor position in screen coordinates
    pub fn cursor_screen_pos(&self) -> Option<Point> {
        let cursor_doc = self.input.primary_cursor_doc_pos(&self.view.doc);

        // Use Viewport's one-step transform (includes padding)
        let screen_pos = self.view.viewport.doc_to_screen(cursor_doc, self.view.padding_x, self.view.padding_y);

        Some(Point {
            x: screen_pos.x,
            y: screen_pos.y,
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

    // === Plugin Management ===

    /// Check if plugins are initialized
    pub fn has_plugins(&self) -> bool {
        self.cursor_plugin.is_some() && self.selection_plugin.is_some()
    }

    /// Initialize cursor and selection plugins for this view
    /// Call this once after creating the view, passing the global plugin loader
    pub fn initialize_plugins(&mut self, plugin_loader: &tiny_core::plugin_loader::PluginLoader) -> Result<(), String> {
        // Skip if already initialized
        if self.has_plugins() {
            return Ok(());
        }
        // Create cursor plugin instance
        match plugin_loader.create_plugin_instance("cursor") {
            Ok(plugin) => {
                self.cursor_plugin = Some(plugin);
            }
            Err(e) => {
                eprintln!("Failed to create cursor plugin instance: {:?}", e);
                return Err(format!("Failed to create cursor plugin: {:?}", e));
            }
        }

        // Create selection plugin instance
        match plugin_loader.create_plugin_instance("selection") {
            Ok(plugin) => {
                self.selection_plugin = Some(plugin);
            }
            Err(e) => {
                eprintln!("Failed to create selection plugin instance: {:?}", e);
                return Err(format!("Failed to create selection plugin: {:?}", e));
            }
        }

        Ok(())
    }

    /// Setup plugins with GPU resources (must be called after initialize_plugins)
    pub fn setup_plugins(&mut self, ctx: &mut tiny_sdk::SetupContext) -> Result<(), tiny_sdk::PluginError> {
        if let Some(ref mut plugin) = self.cursor_plugin {
            if let Some(init) = plugin.as_initializable() {
                init.setup(ctx)?;
            }
        }
        if let Some(ref mut plugin) = self.selection_plugin {
            if let Some(init) = plugin.as_initializable() {
                init.setup(ctx)?;
            }
        }
        Ok(())
    }

    /// Sync cursor and selection state to plugins
    /// Call this whenever cursor/selection changes or viewport updates
    /// Note: Plugins get ViewportInfo from PaintContext during paint(), we just send positions
    pub fn sync_plugins(&mut self) {
        // Update cursor plugin with VIEW coordinates (layout - scroll)
        if let Some(ref mut plugin) = self.cursor_plugin {
            if let Some(library) = plugin.as_library_mut() {
                if let Some(sel) = self.input.selections().first() {
                    let tree = self.view.doc.read();
                    let line_text = tree.line_text(sel.cursor.line);

                    eprintln!("🔧 sync_plugins: cursor at line={}, column={}, line_text={:?}",
                        sel.cursor.line, sel.cursor.column, line_text);

                    // Get layout position (0,0 relative to content)
                    let layout_pos = self.view.viewport.doc_to_layout_with_text(sel.cursor, &line_text);

                    eprintln!("🔧 sync_plugins: layout_pos=({}, {})", layout_pos.x.0, layout_pos.y.0);

                    // Convert to view coordinates (subtract scroll, add padding to match text rendering)
                    // Text is rendered at: bounds.origin + padding + view_pos (see TextView::collect_glyphs)
                    // So plugins should receive: padding + view_pos
                    let view_x = layout_pos.x.0 - self.view.viewport.scroll.x.0 + self.view.padding_x;
                    let view_y = layout_pos.y.0 - self.view.viewport.scroll.y.0 + self.view.padding_y;

                    // Send as LayoutPos type (confusing naming, but it's view coords)
                    let view_pos = tiny_sdk::LayoutPos::new(view_x, view_y);
                    let _ = library.call("set_position", tiny_sdk::bytemuck::bytes_of(&view_pos));
                }
            }
        }

        // Update selection plugin with VIEW coordinates (including padding)
        if let Some(ref mut plugin) = self.selection_plugin {
            if let Some(library) = plugin.as_library_mut() {
                // Get selections in view coordinates (scroll applied, need to add padding)
                let (_, selections) = self.input.get_selection_data(&self.view.doc, &self.view.viewport);

                // Encode selections for plugin, adding padding to match text rendering
                let mut args = Vec::new();
                let len = selections.len() as u32;
                args.extend_from_slice(tiny_sdk::bytemuck::bytes_of(&len));
                for (start, end) in selections {
                    // Add padding to match text rendering (see TextView::collect_glyphs)
                    let start_with_padding = tiny_sdk::ViewPos {
                        x: tiny_sdk::LogicalPixels(start.x.0 + self.view.padding_x),
                        y: tiny_sdk::LogicalPixels(start.y.0 + self.view.padding_y),
                    };
                    let end_with_padding = tiny_sdk::ViewPos {
                        x: tiny_sdk::LogicalPixels(end.x.0 + self.view.padding_x),
                        y: tiny_sdk::LogicalPixels(end.y.0 + self.view.padding_y),
                    };
                    args.extend_from_slice(tiny_sdk::bytemuck::bytes_of(&start_with_padding));
                    args.extend_from_slice(tiny_sdk::bytemuck::bytes_of(&end_with_padding));
                }
                let _ = library.call("set_selections", &args);
            }
        }
    }

    /// Paint cursor and selection plugins
    /// Call this during rendering with the appropriate PaintContext
    pub fn paint_plugins(&self, ctx: &tiny_sdk::PaintContext, render_pass: &mut wgpu::RenderPass) {
        // Paint selection first (behind text)
        if let Some(ref plugin) = self.selection_plugin {
            if let Some(paintable) = plugin.as_paintable() {
                paintable.paint(ctx, render_pass);
            }
        }

        // Paint cursor second (in front of text)
        if self.show_cursor {
            if let Some(ref plugin) = self.cursor_plugin {
                if let Some(paintable) = plugin.as_paintable() {
                    paintable.paint(ctx, render_pass);
                }
            }
        }
    }

    // === Undo/Redo Operations ===

    /// Undo last edit
    /// Returns true if undo was performed
    pub fn handle_undo(&mut self) -> bool {
        if !self.view.capabilities.undo_redo {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => self.input.undo(&self.view.doc),
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Redo last undone edit
    /// Returns true if redo was performed
    pub fn handle_redo(&mut self) -> bool {
        if !self.view.capabilities.undo_redo {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => self.input.redo(&self.view.doc),
            EditMode::ReadOnly { .. } => false,
        }
    }

    // === Clipboard Operations ===

    /// Copy selected text to clipboard
    /// Returns true if text was copied
    pub fn handle_copy(&mut self) -> bool {
        if !self.view.capabilities.clipboard {
            return false;
        }

        self.input.copy(&self.view.doc);
        true
    }

    /// Cut selected text to clipboard (copy + delete)
    /// Returns true if text was cut
    pub fn handle_cut(&mut self) -> bool {
        if !self.view.capabilities.clipboard || !self.view.capabilities.editing {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.cut(&self.view.doc);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Paste text from clipboard
    /// Returns true if text was pasted
    pub fn handle_paste(&mut self) -> bool {
        if !self.view.capabilities.clipboard || !self.view.capabilities.editing {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.paste(&self.view.doc);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Select all text
    /// Returns true if selection was performed
    pub fn handle_select_all(&mut self) -> bool {
        if !self.view.capabilities.selection {
            return false;
        }

        self.input.select_all(&self.view.doc);
        true
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

    // === Text Editing Operations ===

    /// Insert text at cursor(s)
    /// Returns true if text was inserted
    pub fn insert_text(&mut self, text: &str) -> bool {
        if !self.view.capabilities.editing {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.insert_text(&self.view.doc, text);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Delete character at cursor (backspace or delete)
    /// forward: true for Delete key, false for Backspace
    /// Returns true if deletion occurred
    pub fn delete_char(&mut self, forward: bool) -> bool {
        if !self.view.capabilities.editing {
            return false;
        }

        match self.mode {
            EditMode::SingleLine | EditMode::MultiLine => {
                self.input.delete_at_cursor(&self.view.doc, forward);
                true
            }
            EditMode::ReadOnly { .. } => false,
        }
    }

    /// Move cursor to a specific position
    pub fn set_cursor(&mut self, pos: tiny_sdk::DocPos) {
        self.input.set_cursor(pos);
    }

    /// Get primary cursor position
    pub fn cursor_pos(&self) -> tiny_sdk::DocPos {
        self.input.primary_cursor_doc_pos(&self.view.doc)
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
                    // Transform to screen coordinates (logical)
                    // Note: padding is already in viewport.bounds (set by renderer)
                    let screen_x = self.view.viewport.bounds.x.0 + rect.x.0
                        - self.view.viewport.scroll.x.0;
                    let screen_y = self.view.viewport.bounds.y.0 + rect.y.0
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

impl Default for EditableTextView {
    fn default() -> Self {
        let viewport = Viewport::new(800.0, 600.0, 1.0);
        Self::multi_line(viewport)
    }
}
