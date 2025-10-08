use crate::{
    coordinates, file_picker_plugin, grep_plugin, history,
    input::{self},
    io, syntax, tab_bar_plugin, tab_manager,
    text_editor_plugin::TextEditorPlugin,
    text_effects::TextStyleProvider,
};
use ahash::AHasher;
use anyhow::{Context, Result};
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
    /// Grep plugin for full codebase search (global UI)
    pub grep: grep_plugin::GrepPlugin,
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
    /// Initialize cursor/selection plugins for all EditableTextViews
    /// Safe to call multiple times - will skip already-initialized views
    /// Returns list of newly-initialized views that need GPU setup
    pub fn initialize_all_plugins(&mut self, plugin_loader: &tiny_core::plugin_loader::PluginLoader) -> Result<Vec<*mut crate::editable_text_view::EditableTextView>, String> {
        let mut newly_initialized = Vec::new();

        // Initialize plugins for all tabs (including newly opened ones)
        for tab in self.tab_manager.tabs_mut() {
            if !tab.plugin.editor.has_plugins() {
                tab.plugin.initialize_plugins(plugin_loader)?;
                newly_initialized.push(&mut tab.plugin.editor as *mut _);
            }
        }

        // Initialize plugins for file picker input
        if !self.file_picker.picker.dropdown.input.has_plugins() {
            self.file_picker.picker.dropdown.input.initialize_plugins(plugin_loader)?;
            newly_initialized.push(&mut self.file_picker.picker.dropdown.input as *mut _);
        }

        // Initialize plugins for grep input
        if !self.grep.picker.dropdown.input.has_plugins() {
            self.grep.picker.dropdown.input.initialize_plugins(plugin_loader)?;
            newly_initialized.push(&mut self.grep.picker.dropdown.input as *mut _);
        }

        Ok(newly_initialized)
    }

    /// Setup plugins with GPU resources for specific EditableTextViews
    pub fn setup_plugins_for_views(&mut self, views: Vec<*mut crate::editable_text_view::EditableTextView>, device: std::sync::Arc<wgpu::Device>, queue: std::sync::Arc<wgpu::Queue>) -> Result<(), tiny_sdk::PluginError> {
        if views.is_empty() {
            return Ok(());
        }

        let registry = tiny_sdk::PluginRegistry::empty();
        let mut ctx = tiny_sdk::SetupContext {
            device,
            queue,
            registry,
        };

        for view_ptr in views {
            let view = unsafe { &mut *view_ptr };
            view.setup_plugins(&mut ctx)?;
        }

        Ok(())
    }

    /// Get the active tab's plugin
    fn active_editor(&self) -> Result<&TextEditorPlugin> {
        Ok(&self.tab_manager.active_tab().context("No active tab")?.plugin)
    }

    /// Get the active tab's plugin mutably
    pub fn active_plugin_mut(&mut self) -> &mut TextEditorPlugin {
        &mut self.tab_manager.active_tab_mut().plugin
    }

    /// Handle code action request (Alt+Enter)
    pub fn handle_code_action_request(&mut self) -> Result<()> {
        let tab = self.tab_manager.active_tab_mut();
        let cursor_pos = tab.plugin.editor.input.selections()
            .first()
            .context("No active selection")?
            .cursor;

        // Convert cursor position to UTF-16
        let tree = tab.plugin.editor.view.doc.read();
        let byte_offset = tree.doc_pos_to_byte(cursor_pos);
        let cursor_utf16 = tree.offset_to_point_utf16(byte_offset);

        // Send the exact cursor position - LSP will figure out what diagnostic/action applies
        tab.diagnostics
            .lsp_service()
            .request_code_action(crate::lsp_service::DocPosition {
                line: cursor_utf16.row as usize,
                column: cursor_utf16.column as usize,
            });

        Ok(())
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
                .hit_test_close_button(click_x, click_y, &self.tab_manager, viewport_width)
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
        else if let Some(tab_idx) =
            self.tab_bar
                .hit_test_tab(click_x, click_y, &self.tab_manager, viewport_width)
        {
            self.tab_manager.switch_to(tab_idx);
            self.tab_bar.close_dropdown();

            // Ensure the active tab is visible
            let num_tabs = self.tab_manager.tabs().len();
            self.tab_bar
                .scroll_to_tab(tab_idx, viewport_width, num_tabs);
            self.ui_changed = true;

            // Trigger syntax highlighting for newly active tab
            let plugin = &self.tab_manager.active_tab().unwrap().plugin;
            if let Some(ref syntax_highlighter) = plugin.syntax_highlighter {
                let text = plugin.editor.view.doc.read().flatten_to_string();
                if let Some(syntax_hl) = syntax_highlighter
                    .as_any()
                    .downcast_ref::<syntax::SyntaxHighlighter>()
                {
                    syntax_hl.request_update_with_edit(&text, plugin.editor.view.doc.version(), None);
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

            if self.tab_manager.active_tab().is_some() {
                let doc_pos = viewport.layout_to_doc(pos);
                eprintln!("DEBUG: Cmd+Click at doc pos: {:?}", doc_pos);

                // Request go-to-definition at click location
                let _ = self.goto_definition();
                self.widgets_dirty = true;
                return true;
            }
        }

        // Normal click handling
        let plugin = self.active_plugin_mut();
        plugin.editor.input.on_mouse_click(
            &plugin.editor.view.doc,
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
        self.active_plugin_mut().editor.input.clear_drag_anchor();
        self.pending_scroll = None;
    }

    /// Get document to render
    pub fn doc(&self) -> Result<&Doc> {
        Ok(&self.active_editor()?.editor.view.doc)
    }

    /// Get cursor document position for scrolling
    pub fn get_cursor_doc_pos(&self) -> Result<Option<DocPos>> {
        Ok(self.active_editor()?.get_cursor_doc_pos())
    }

    /// Get current selections for rendering
    pub fn selections(&self) -> Result<&[input::Selection]> {
        Ok(self.active_editor()?.selections())
    }

    /// Get text style provider for syntax highlighting
    pub fn text_styles(&self) -> Result<Option<&dyn TextStyleProvider>> {
        Ok(self.active_editor()?.syntax_highlighter.as_deref())
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
        if plugin.editor.input.should_flush() {
            println!("DEBOUNCE: Sending pending syntax updates after idle timeout");
            plugin.editor.input.flush_syntax_updates(&plugin.editor.view.doc);
        }

        // LSP results are now handled in tab.diagnostics.update() in app.rs
        let tab = self.tab_manager.active_tab_mut();

        // Apply pending text edits from code actions
        let edits = match tab.diagnostics.take_text_edits() {
            Some(e) => e,
            None => return cursor_moved,
        };

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
            let tree = tab.plugin.editor.view.doc.read();

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
                tab.plugin.editor.view.doc.edit(tiny_tree::Edit::Insert {
                    pos: start_byte,
                    content: tiny_tree::Content::Text(edit.new_text),
                });
            } else {
                // Replace
                tab.plugin.editor.view.doc.edit(tiny_tree::Edit::Replace {
                    range: start_byte..end_byte,
                    content: tiny_tree::Content::Text(edit.new_text),
                });
            }
        }

        tab.plugin.editor.view.doc.flush();

        // Notify LSP of document changes
        let updated_text = tab.plugin.editor.view.doc.read().flatten_to_string();
        tab.diagnostics.document_changed(updated_text.to_string());

        // Trigger syntax highlighting update
        if let Some(ref syntax_highlighter) = tab.plugin.syntax_highlighter {
            if let Some(syntax_hl) = syntax_highlighter
                .as_any()
                .downcast_ref::<syntax::SyntaxHighlighter>()
            {
                syntax_hl.request_update_with_edit(
                    &updated_text,
                    tab.plugin.editor.view.doc.version(),
                    None
                );
            }
        }

        self.ui_changed = true;

        // Check for go-to-definition results
        let tab = self.tab_manager.active_tab_mut();
        let locations = match tab.diagnostics.take_goto_definition() {
            Some(locs) => locs,
            None => {
                // Continue with cmd_hover_range update
                let tab = self.tab_manager.active_tab_mut();
                if let Some((line, column)) = tab.diagnostics.cmd_hover_position() {
                    // Find word boundaries at hover position
                    let doc = &tab.plugin.editor.view.doc;
                    let tree = doc.read();
                    let line_text = tree.line_text(line as u32);
                    let word_range = find_word_at_position(&line_text, column);

                    tab.plugin.cmd_hover_range = word_range.map(|(start, end)| {
                        (line as u32, start as u32, end as u32)
                    });

                    self.ui_changed = tab.plugin.cmd_hover_range.is_some();
                } else if tab.plugin.cmd_hover_range.is_some() {
                    tab.plugin.cmd_hover_range = None;
                    self.ui_changed = true;
                }

                return cursor_moved;
            }
        };

        eprintln!(
            "DEBUG: on_update got {} goto_definition location(s)",
            locations.len()
        );

        let location = match locations.first() {
            Some(loc) => loc,
            None => return cursor_moved,
        };

        eprintln!(
            "DEBUG: Navigating to {:?} at line {}, UTF-16 col {}",
            location.file_path, location.position.line, location.position.column
        );

        // Use the unified jump_to_location_utf16 method
        if self.jump_to_location_utf16(
            location.file_path.clone(),
            location.position.line as u32,
            location.position.column as u32,
            true, // center on screen
        ) {
            cursor_moved = true;
        }

        cursor_moved
    }

    /// Record current location in global navigation history
    pub fn record_navigation(&mut self) -> Result<()> {
        let plugin = self.active_editor()?;
        let location = history::FileLocation {
            path: plugin.file_path.clone(),
            position: plugin.editor.input.primary_cursor_doc_pos(&plugin.editor.view.doc),
        };
        self.global_nav_history.checkpoint_if_changed(location);
        Ok(())
    }

    /// Navigate back in global history (across files)
    pub fn navigate_back(&mut self) -> Result<bool> {
        let current_location = history::FileLocation {
            path: self.active_editor()?.file_path.clone(),
            position: self
                .active_editor()?
                .editor
                .input
                .primary_cursor_doc_pos(&self.active_editor()?.editor.view.doc),
        };

        let target = self.global_nav_history.undo(current_location)
            .context("No previous location in history")?;

        self.navigate_to_location(target)
    }

    /// Navigate forward in global history (across files)
    pub fn navigate_forward(&mut self) -> Result<bool> {
        let current_location = history::FileLocation {
            path: self.active_editor()?.file_path.clone(),
            position: self
                .active_editor()?
                .editor
                .input
                .primary_cursor_doc_pos(&self.active_editor()?.editor.view.doc),
        };

        let target = self.global_nav_history.redo(current_location)
            .context("No next location in history")?;

        self.navigate_to_location(target)
    }

    /// Navigate to a specific file and position
    fn navigate_to_location(&mut self, location: history::FileLocation) -> Result<bool> {
        // Open file if needed (without recording - we're already in a navigation)
        if let Some(ref path) = location.path {
            self.tab_manager.open_file(path.clone())
                .context("Failed to open file for navigation")?;
        }

        // Set cursor position in active tab
        let plugin = self.active_plugin_mut();
        plugin.editor.input.set_cursor(location.position);
        self.ui_changed = true;
        Ok(true)
    }

    /// Jump to a specific location (file path + line + column in UTF-8)
    /// This is the unified method for goto_definition, grep results, etc.
    pub fn jump_to_location(
        &mut self,
        file_path: PathBuf,
        line: usize,
        column: usize,
        center: bool,
    ) -> Result<()> {
        // Record current location before jumping
        self.record_navigation()?;

        // Canonicalize path to ensure it matches existing tabs
        let canonical_path = std::fs::canonicalize(&file_path)
            .unwrap_or_else(|_| file_path.clone());

        // Open file (will switch to existing tab if already open)
        self.tab_manager.open_file(canonical_path)
            .context("Failed to open file for jump_to_location")?;

        // Set cursor to the position (already in UTF-8 doc coords)
        let plugin = self.active_plugin_mut();
        plugin.editor.input.set_cursor(tiny_sdk::DocPos {
            line: line as u32,
            column: column as u32,
            byte_offset: 0,
        });

        self.ui_changed = true;
        if center {
            self.cursor_needs_centering = true;
        }

        Ok(())
    }

    /// Jump to a location specified in UTF-16 coordinates (for LSP)
    pub fn jump_to_location_utf16(
        &mut self,
        file_path: PathBuf,
        line_utf16: u32,
        column_utf16: u32,
        center: bool,
    ) -> bool {
        // Record current location before jumping
        let _ = self.record_navigation();

        // Canonicalize path to ensure it matches existing tabs
        let canonical_path = std::fs::canonicalize(&file_path)
            .unwrap_or_else(|_| file_path.clone());

        // Open file (will switch to existing tab if already open)
        if let Err(e) = self.tab_manager.open_file(canonical_path) {
            eprintln!("Failed to open file for jump_to_location_utf16: {}", e);
            return false;
        }

        // Convert UTF-16 position to byte-based DocPos
        let (line, byte_column) = {
            let tab = match self.tab_manager.active_tab() {
                Some(t) => t,
                None => {
                    eprintln!("No active tab available");
                    return false;
                }
            };
            let tree = tab.plugin.editor.view.doc.read();
            let utf16_point = tiny_tree::PointUtf16::new(line_utf16, column_utf16);
            tree.point_utf16_to_doc_pos(utf16_point)
        };

        // Set cursor to the converted position
        let plugin = self.active_plugin_mut();
        plugin.editor.input.set_cursor(tiny_sdk::DocPos {
            line: line as u32,
            column: byte_column as u32,
            byte_offset: 0,
        });

        self.ui_changed = true;
        if center {
            self.cursor_needs_centering = true;
        }

        true
    }

    /// Go to definition at current cursor position
    pub fn goto_definition(&mut self) -> Result<()> {
        self.record_navigation()?;

        let tab = self.tab_manager.active_tab_mut();
        let plugin = &tab.plugin;
        let cursor_pos = plugin.editor.input.primary_cursor_doc_pos(&plugin.editor.view.doc);

        // Convert cursor position to UTF-16
        let tree = plugin.editor.view.doc.read();
        let byte_offset = tree.doc_pos_to_byte(cursor_pos);
        let cursor_utf16 = tree.offset_to_point_utf16(byte_offset);

        eprintln!(
            "DEBUG: Cursor DocPos ({}, {}) -> byte {} -> UTF-16 ({}, {})",
            cursor_pos.line, cursor_pos.column, byte_offset, cursor_utf16.row, cursor_utf16.column
        );

        // Send the exact cursor position - let rust-analyzer figure out the identifier
        tab.diagnostics
            .request_goto_definition(cursor_utf16.row as usize, cursor_utf16.column as usize);

        Ok(())
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
                    if self.tab_manager.tabs().get(0)
                        .and_then(|t| t.path())
                        .is_none()
                    {
                        // Close the empty tab (index 0)
                        self.tab_manager.close_tab(0);
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
        self.active_editor().map(|e| e.is_modified()).unwrap_or(false)
    }

    pub fn save(&mut self) -> Result<()> {
        let tab = self.tab_manager.active_tab_mut();
        let plugin = &mut tab.plugin;

        let path = plugin.file_path.as_ref()
            .context("No file path set")?;

        io::autosave(&plugin.editor.view.doc, path)
            .context("Failed to save file")?;

        // Update saved content hash
        let current_text = plugin.editor.view.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        current_text.hash(&mut hasher);
        plugin.last_saved_content_hash = hasher.finish();

        // Notify diagnostics manager of save
        tab.diagnostics.document_saved(current_text.to_string());

        Ok(())
    }

    pub fn title(&self) -> String {
        let plugin = match self.active_editor() {
            Ok(p) => p,
            Err(_) => return "No active tab".to_string(),
        };

        let filename = plugin.file_path.as_ref()
            .and_then(|path| path.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
            .to_string();

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
        let initial_text = plugin.editor.view.doc.read().flatten_to_string();
        let mut hasher = AHasher::default();
        initial_text.hash(&mut hasher);
        plugin.last_saved_content_hash = hasher.finish();

        // Create initial tab with the plugin (tab owns line numbers + diagnostics)
        let initial_tab = tab_manager::Tab::new(plugin);
        let tab_manager = tab_manager::TabManager::with_initial_tab(initial_tab);

        // Create global UI plugins
        let tab_bar = tab_bar_plugin::TabBarPlugin::new();
        let file_picker = file_picker_plugin::FilePickerPlugin::new();
        let grep = grep_plugin::GrepPlugin::new();

        Self {
            tab_manager,
            tab_bar,
            file_picker,
            grep,
            widgets_dirty: true,
            extra_text_styles: Vec::new(),
            pending_scroll: None,
            ui_changed: true,
            global_nav_history: history::FileNavigationHistory::with_max_size(50),
            cursor_needs_centering: false,
        }
    }
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
