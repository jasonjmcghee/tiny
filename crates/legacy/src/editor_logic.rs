use crate::{
    coordinates, file_picker_plugin, history,
    input::{self, InputAction},
    io, syntax, tab_bar_plugin, tab_manager,
    text_editor_plugin::TextEditorPlugin,
    text_effects::TextStyleProvider,
};
use ahash::AHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use tiny_core::tree::{Doc, Point};
use tiny_sdk::DocPos;

pub struct EditorLogic {
    /// Tab manager for handling multiple open files (each tab owns its own plugin + line numbers + diagnostics)
    pub tab_manager: tab_manager::TabManager,
    /// Tab bar plugin for rendering tabs (global UI)
    pub tab_bar: tab_bar_plugin::TabBarPlugin,
    /// File picker plugin for opening files (global UI)
    pub file_picker: file_picker_plugin::FilePickerPlugin,
    /// Flag to indicate widgets need updating
    widgets_dirty: bool,
    /// Extra text style providers (e.g., for effects)
    pub extra_text_styles: Vec<Box<dyn TextStyleProvider>>,
    /// Pending scroll delta from drag operations
    pub pending_scroll: Option<(f32, f32)>,
    /// Flag to indicate UI needs re-rendering (tabs, file picker, etc)
    pub ui_changed: bool,
    /// Global navigation history for cross-file navigation (Cmd+[/])
    pub global_nav_history: history::FileNavigationHistory,
    /// Flag to indicate cursor should be centered (for goto definition)
    pub cursor_needs_centering: bool,
}

impl EditorLogic {
    /// Get the active tab's plugin
    fn active_editor(&self) -> &TextEditorPlugin {
        &self.tab_manager.active_tab().expect("No active tab").plugin
    }

    /// Get the active tab's plugin mutably
    pub fn active_plugin_mut(&mut self) -> &mut TextEditorPlugin {
        &mut self.tab_manager.active_tab_mut().plugin
    }

    /// Handle code action request (Alt+Enter)
    pub fn handle_code_action_request(&mut self) {
        let tab = self.tab_manager.active_tab_mut();
        let cursor_pos = tab.plugin.input.selections()[0].cursor;

        // Convert cursor position to UTF-16
        let tree = tab.plugin.doc.read();
        let byte_offset = tree.doc_pos_to_byte(cursor_pos);
        let cursor_utf16 = tree.offset_to_point_utf16(byte_offset);

        // Send the exact cursor position - LSP will figure out what diagnostic/action applies
        tab.diagnostics
            .lsp_service()
            .request_code_action(crate::lsp_service::DocPosition {
                line: cursor_utf16.row as usize,
                column: cursor_utf16.column as usize,
            });
    }

    /// Handle tab bar click at the given position (relative to tab bar top-left)
    /// Returns true if click was handled by tab bar
    pub fn handle_tab_bar_click(
        &mut self,
        click_x: f32,
        click_y: f32,
        viewport_width: f32,
    ) -> bool {
        // Check dropdown first
        if self
            .tab_bar
            .hit_test_dropdown(click_x, click_y, viewport_width)
        {
            self.tab_bar.toggle_dropdown();
            self.ui_changed = true;
            return true;
        }
        // Check close button
        else if let Some(tab_idx) =
            self.tab_bar
                .hit_test_close_button(click_x, click_y, &self.tab_manager)
        {
            let was_last = self.tab_manager.close_tab(tab_idx);
            if was_last {
                // TODO: Fix this
                panic!("Closed last tab")
            }
            self.ui_changed = true;
            return true;
        }
        // Check tab click
        else if let Some(tab_idx) = self
            .tab_bar
            .hit_test_tab(click_x, click_y, &self.tab_manager)
        {
            self.tab_manager.switch_to(tab_idx);
            self.tab_bar.close_dropdown();
            self.ui_changed = true;

            // Trigger syntax highlighting for newly active tab
            let plugin = &self.tab_manager.active_tab().unwrap().plugin;
            if let Some(ref syntax_highlighter) = plugin.syntax_highlighter {
                let text = plugin.doc.read().flatten_to_string();
                if let Some(syntax_hl) = syntax_highlighter
                    .as_any()
                    .downcast_ref::<syntax::SyntaxHighlighter>()
                {
                    syntax_hl.request_update_with_edit(&text, plugin.doc.version(), None);
                }
            }
            return true;
        }

        false
    }

    /// Handle mouse click at logical position
    pub fn on_click(
        &mut self,
        pos: Point,
        viewport: &coordinates::Viewport,
        cmd_held: bool,
        alt_held: bool,
        shift_held: bool,
    ) -> bool {
        // Cmd+Click triggers go-to-definition
        if cmd_held {
            eprintln!("DEBUG: Cmd+Click detected at {:?}", pos);
            // Get document position at click location
            let tab = self.tab_manager.active_tab();
            if let Some(tab) = tab {
                let doc_pos = viewport.layout_to_doc(pos);
                eprintln!("DEBUG: Cmd+Click at doc pos: {:?}", doc_pos);

                // Request go-to-definition at click location
                self.goto_definition();
                self.widgets_dirty = true;
                return true;
            }
        }

        // Normal click handling
        let plugin = self.active_plugin_mut();
        plugin.input.on_mouse_click(
            &plugin.doc,
            viewport,
            pos,
            crate::input_types::MouseButton::Left,
            alt_held,
            shift_held,
        );
        self.widgets_dirty = true;
        true
    }

    /// Handle mouse move (for tracking position)
    pub fn on_mouse_move(&mut self, _pos: Point, _viewport: &coordinates::Viewport) -> bool {
        false
    }

    /// Handle mouse button release (for cleaning up drag state)
    pub fn on_mouse_release(&mut self) {
        self.active_plugin_mut().input.clear_drag_anchor();
        self.pending_scroll = None;
    }

    /// Get document to render
    pub fn doc(&self) -> &Doc {
        &self.active_editor().doc
    }

    /// Get cursor document position for scrolling
    pub fn get_cursor_doc_pos(&self) -> Option<DocPos> {
        self.active_editor().get_cursor_doc_pos()
    }

    /// Get current selections for rendering
    pub fn selections(&self) -> &[input::Selection] {
        self.active_editor().selections()
    }

    /// Get text style provider for syntax highlighting
    pub fn text_styles(&self) -> Option<&dyn TextStyleProvider> {
        self.active_editor().syntax_highlighter.as_deref()
    }

    /// Called after setup is complete
    pub fn on_ready(&mut self) {}

    /// Register custom text effect shaders
    pub fn register_shaders(&self) -> Vec<(u32, &'static str, u64)> {
        vec![]
    }

    /// Called before each render (for animations, LSP polling, etc.)
    /// Returns true if cursor moved (requiring scroll update)
    pub fn on_update(&mut self) -> bool {
        let mut cursor_moved = false;
        let plugin = self.active_plugin_mut();
        // Check if we should send pending syntax updates (debounce timer expired)
        if plugin.input.should_flush() {
            println!("DEBOUNCE: Sending pending syntax updates after idle timeout");
            plugin.input.flush_syntax_updates(&plugin.doc);
        }

        // LSP results are now handled in tab.diagnostics.update() in app.rs
        let tab = self.tab_manager.active_tab_mut();

        // Apply pending text edits from code actions
        if let Some(edits) = tab.diagnostics.take_text_edits() {
            eprintln!("DEBUG: Applying {} text edits to document", edits.len());

            // Sort edits by position (reverse order to apply from end to start)
            let mut sorted_edits = edits.clone();
            sorted_edits.sort_by(|a, b| {
                b.range_utf16
                    .0
                    .line
                    .cmp(&a.range_utf16.0.line)
                    .then(b.range_utf16.0.column.cmp(&a.range_utf16.0.column))
            });

            // Convert UTF-16 positions to byte offsets and apply edits
            for edit in sorted_edits {
                let tree = tab.plugin.doc.read();

                let start_utf16 = tiny_tree::PointUtf16::new(
                    edit.range_utf16.0.line as u32,
                    edit.range_utf16.0.column as u32,
                );
                let end_utf16 = tiny_tree::PointUtf16::new(
                    edit.range_utf16.1.line as u32,
                    edit.range_utf16.1.column as u32,
                );

                let start_byte = tree.point_utf16_to_byte(start_utf16);
                let end_byte = tree.point_utf16_to_byte(end_utf16);
                drop(tree);

                eprintln!(
                    "DEBUG: Edit range UTF-16 ({},{}) to ({},{}) = bytes {} to {}",
                    edit.range_utf16.0.line,
                    edit.range_utf16.0.column,
                    edit.range_utf16.1.line,
                    edit.range_utf16.1.column,
                    start_byte,
                    end_byte
                );

                // Apply the edit
                if start_byte == end_byte {
                    // Insert
                    tab.plugin.doc.edit(tiny_tree::Edit::Insert {
                        pos: start_byte,
                        content: tiny_tree::Content::Text(edit.new_text),
                    });
                } else {
                    // Replace
                    tab.plugin.doc.edit(tiny_tree::Edit::Replace {
                        range: start_byte..end_byte,
                        content: tiny_tree::Content::Text(edit.new_text),
                    });
                }
            }

            tab.plugin.doc.flush();
            self.ui_changed = true;
        }

        // Check for go-to-definition results
        let tab = self.tab_manager.active_tab_mut();
        if let Some(locations) = tab.diagnostics.take_goto_definition() {
            eprintln!(
                "DEBUG: on_update got {} goto_definition location(s)",
                locations.len()
            );
            if let Some(location) = locations.first() {
                eprintln!(
                    "DEBUG: Navigating to {:?} at line {}, UTF-16 col {}",
                    location.file_path, location.position.line, location.position.column
                );

                // Store location for conversion after file is opened
                let location_utf16 = location.clone();

                // Canonicalize path to ensure it matches existing tabs
                let canonical_path = std::fs::canonicalize(&location.file_path)
                    .unwrap_or_else(|_| location.file_path.clone());

                // Open file first (will switch to existing tab if already open)
                if let Ok(_) = self.tab_manager.open_file(canonical_path) {
                    // Now convert UTF-16 position to byte-based DocPos using the opened file's Tree
                    let (line, byte_column) = {
                        let tab = self.tab_manager.active_tab().expect("No active tab");
                        let tree = tab.plugin.doc.read();
                        let utf16_point = tiny_tree::PointUtf16::new(
                            location_utf16.position.line as u32,
                            location_utf16.position.column as u32,
                        );
                        let result = tree.point_utf16_to_doc_pos(utf16_point);
                        eprintln!(
                            "DEBUG: Converted UTF-16 ({}, {}) to byte-based ({}, {})",
                            location_utf16.position.line,
                            location_utf16.position.column,
                            result.0,
                            result.1
                        );
                        result
                    };

                    // Set cursor to the converted position
                    let plugin = self.active_plugin_mut();
                    plugin.input.set_cursor(tiny_sdk::DocPos {
                        line,
                        column: byte_column,
                        byte_offset: 0,
                    });
                    self.ui_changed = true;
                    self.cursor_needs_centering = true;
                    cursor_moved = true;
                } else {
                    eprintln!("DEBUG: Failed to open file!");
                }
            }
        }

        // Update cmd_hover_range for underline rendering
        let tab = self.tab_manager.active_tab_mut();
        if let Some((line, column)) = tab.diagnostics.cmd_hover_position() {
            // Find word boundaries at hover position
            let doc = &tab.plugin.doc;
            let tree = doc.read();
            let line_text = tree.line_text(line as u32);
            let word_range = find_word_at_position(&line_text, column);
            if let Some((start, end)) = word_range {
                tab.plugin.cmd_hover_range = Some((line as u32, start as u32, end as u32));
                self.ui_changed = true;
            } else {
                tab.plugin.cmd_hover_range = None;
            }
        } else {
            let tab = self.tab_manager.active_tab_mut();
            if tab.plugin.cmd_hover_range.is_some() {
                tab.plugin.cmd_hover_range = None;
                self.ui_changed = true;
            }
        }

        cursor_moved
    }

    /// Record current location in global navigation history
    pub fn record_navigation(&mut self) {
        let plugin = self.active_editor();
        let location = history::FileLocation {
            path: plugin.file_path.clone(),
            position: plugin.input.primary_cursor_doc_pos(&plugin.doc),
        };
        self.global_nav_history.checkpoint_if_changed(location);
    }

    /// Navigate back in global history (across files)
    pub fn navigate_back(&mut self) -> bool {
        let current_location = history::FileLocation {
            path: self.active_editor().file_path.clone(),
            position: self
                .active_editor()
                .input
                .primary_cursor_doc_pos(&self.active_editor().doc),
        };

        if let Some(target) = self.global_nav_history.undo(current_location) {
            self.navigate_to_location(target)
        } else {
            false
        }
    }

    /// Navigate forward in global history (across files)
    pub fn navigate_forward(&mut self) -> bool {
        let current_location = history::FileLocation {
            path: self.active_editor().file_path.clone(),
            position: self
                .active_editor()
                .input
                .primary_cursor_doc_pos(&self.active_editor().doc),
        };

        if let Some(target) = self.global_nav_history.redo(current_location) {
            self.navigate_to_location(target)
        } else {
            false
        }
    }

    /// Navigate to a specific file and position
    fn navigate_to_location(&mut self, location: history::FileLocation) -> bool {
        // Open file if needed (without recording - we're already in a navigation)
        if let Some(ref path) = location.path {
            match self.tab_manager.open_file(path.clone()) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Failed to open file for navigation: {}", e);
                    return false;
                }
            }
        }

        // Set cursor position in active tab
        let plugin = self.active_plugin_mut();
        plugin.input.set_cursor(location.position);
        self.ui_changed = true;
        true
    }

    /// Go to definition at current cursor position
    pub fn goto_definition(&mut self) {
        self.record_navigation();

        let tab = self.tab_manager.active_tab_mut();
        let plugin = &tab.plugin;
        let cursor_pos = plugin.input.primary_cursor_doc_pos(&plugin.doc);

        // Convert cursor position to UTF-16
        let tree = plugin.doc.read();
        let byte_offset = tree.doc_pos_to_byte(cursor_pos);
        let cursor_utf16 = tree.offset_to_point_utf16(byte_offset);

        eprintln!(
            "DEBUG: Cursor DocPos ({}, {}) -> byte {} -> UTF-16 ({}, {})",
            cursor_pos.line, cursor_pos.column, byte_offset, cursor_utf16.row, cursor_utf16.column
        );

        // Send the exact cursor position - let rust-analyzer figure out the identifier
        tab.diagnostics
            .request_goto_definition(cursor_utf16.row as usize, cursor_utf16.column as usize);
    }
}

impl EditorLogic {
    pub fn with_text_style(mut self, style: Box<dyn TextStyleProvider>) -> Self {
        self.extra_text_styles.push(style);
        self
    }

    pub fn with_file(mut self, path: PathBuf) -> Self {
        // Replace the initial tab with a tab for this file
        match self.tab_manager.open_file(path.clone()) {
            Ok(_) => {
                // Remove the empty initial tab if it exists
                if self.tab_manager.len() > 1 {
                    // Find and remove the first tab if it's untitled and empty
                    if let Some(first_tab) = self.tab_manager.tabs().get(0) {
                        if first_tab.path().is_none() {
                            // Close the empty tab (index 0)
                            self.tab_manager.close_tab(0);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to open initial file: {}", e);
            }
        }
        self
    }

    /// Check if document has unsaved changes by comparing content hash
    pub fn is_modified(&self) -> bool {
        self.active_editor().is_modified()
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        let tab = self.tab_manager.active_tab_mut();
        let plugin = &mut tab.plugin;
        if let Some(ref path) = plugin.file_path {
            io::autosave(&plugin.doc, path)?;

            // Update saved content hash
            let current_text = plugin.doc.read().flatten_to_string();
            let mut hasher = AHasher::default();
            current_text.hash(&mut hasher);
            plugin.last_saved_content_hash = hasher.finish();

            // Notify diagnostics manager of save
            tab.diagnostics.document_saved(current_text.to_string());

            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "No file path set",
            ))
        }
    }

    pub fn title(&self) -> String {
        let plugin = self.active_editor();
        let filename = if let Some(ref path) = plugin.file_path {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled")
                .to_string()
        } else {
            "Demo Text".to_string()
        };

        let modified_marker = if self.is_modified() {
            " (modified)"
        } else {
            ""
        };
        format!("{}{}", filename, modified_marker)
    }

    pub fn new(doc: Doc) -> Self {
        let mut plugin = TextEditorPlugin::new(doc);

        // Calculate initial content hash
        let initial_text = plugin.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        initial_text.hash(&mut hasher);
        plugin.last_saved_content_hash = hasher.finish();

        // Create initial tab with the plugin (tab owns line numbers + diagnostics)
        let initial_tab = tab_manager::Tab::new(plugin);
        let tab_manager = tab_manager::TabManager::with_initial_tab(initial_tab);

        // Create global UI plugins
        let tab_bar = tab_bar_plugin::TabBarPlugin::new();
        let file_picker = file_picker_plugin::FilePickerPlugin::new();

        Self {
            tab_manager,
            tab_bar,
            file_picker,
            widgets_dirty: true,
            extra_text_styles: Vec::new(),
            pending_scroll: None,
            ui_changed: true,
            global_nav_history: history::FileNavigationHistory::with_max_size(50),
            cursor_needs_centering: false,
        }
    }
}

/// Find the start of an identifier at the given column position
/// Returns the column where the identifier starts (handles cursor anywhere in identifier)
fn find_identifier_start(line_text: &str, column: usize) -> usize {
    let chars: Vec<char> = line_text.chars().collect();

    // If past end of line, return column as-is
    if column >= chars.len() {
        return column;
    }

    // Standard identifier: alphanumeric and underscore only
    let is_identifier_char = |c: char| c.is_alphanumeric() || c == '_';

    // If not on an identifier, return column as-is
    if !is_identifier_char(chars[column]) {
        return column;
    }

    // Find start of identifier (go backwards)
    let mut start = column;
    while start > 0 && is_identifier_char(chars[start - 1]) {
        start -= 1;
    }

    start
}

/// Find word boundaries at the given column position in a line of text
fn find_word_at_position(line_text: &str, column: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = line_text.chars().collect();
    if column >= chars.len() {
        return None;
    }
    // Check if current character is part of an identifier
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    if !is_word_char(chars[column]) {
        return None;
    }
    // Find start of word
    let mut start = column;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    // Find end of word
    let mut end = column;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }
    Some((start, end))
}
