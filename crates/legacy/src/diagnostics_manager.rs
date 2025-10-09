//! High-level diagnostics manager that encapsulates LSP, caching, and plugin integration

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use crate::lsp_service::{LspResult, LspService};
use ahash::AHashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tiny_core::plugin_loader::PluginLoader;
use tiny_sdk::{Initializable, Library, Plugin, SetupContext};
use tiny_tree::Doc;

const DEFINITION_CACHE_SAVE_DEBOUNCE_SECS: u64 = 5;

/// High-level diagnostics manager (now uses LspService for broader LSP support)
pub struct DiagnosticsManager {
    plugin: Option<Arc<Mutex<Box<dyn Plugin>>>>,
    lsp_service: LspService,
    /// Pending go-to-definition result
    pending_goto_definition: Option<Vec<crate::lsp_service::LocationRef>>,
    /// Position of user-requested goto definition (for caching)
    user_requested_goto_position: Option<(usize, usize)>,
    /// Flag indicating user wants to navigate with next goto definition result
    user_navigation_pending: bool,
    /// Current hover position with Cmd held (for go-to-definition preview)
    cmd_hover_position: Option<(usize, usize)>,
    /// Pending text edits from code actions
    pending_text_edits: Option<Vec<crate::lsp_service::TextEdit>>,
    /// Last hover request position (for matching hover responses)
    last_hover_request: Option<(usize, usize)>,
    /// Last mouse position (screen coordinates) for hover detection
    last_mouse_position: Option<(f32, f32)>,
    /// Cached go-to-definition results: (line, column) -> locations
    definition_cache: AHashMap<(usize, usize), Vec<crate::lsp_service::LocationRef>>,
    /// Document symbols for proactive definition requests
    document_symbols: Vec<lsp_types::DocumentSymbol>,
    /// Last time definition cache was modified
    definition_cache_modified: Option<Instant>,
    /// Last time definition cache was saved to disk
    definition_cache_last_saved: Option<Instant>,
}

impl DiagnosticsManager {
    /// Create a new diagnostics manager
    pub fn new() -> Self {
        Self {
            plugin: None,
            lsp_service: LspService::new(),
            pending_goto_definition: None,
            user_requested_goto_position: None,
            user_navigation_pending: false,
            cmd_hover_position: None,
            pending_text_edits: None,
            last_hover_request: None,
            last_mouse_position: None,
            definition_cache: AHashMap::new(),
            document_symbols: Vec::new(),
            definition_cache_modified: None,
            definition_cache_last_saved: None,
        }
    }

    /// Initialize the diagnostics plugin (must be called before use)
    pub fn initialize_plugin(&mut self, plugin_loader: &mut PluginLoader) -> Result<(), String> {
        match plugin_loader.create_plugin_instance("diagnostics") {
            Ok(plugin) => {
                self.plugin = Some(plugin);
                Ok(())
            }
            Err(e) => {
                eprintln!(
                    "[DIAG] Failed to create diagnostics plugin instance: {:?}",
                    e
                );
                Err(format!("Failed to create diagnostics plugin: {:?}", e))
            }
        }
    }

    /// Setup plugin with GPU resources (must be called after initialize_plugin)
    pub fn setup_plugin(&mut self, ctx: &mut SetupContext) -> Result<(), tiny_sdk::PluginError> {
        if let Some(ref plugin_arc) = self.plugin {
            if let Ok(mut plugin) = plugin_arc.lock() {
                if let Some(init) = plugin.as_initializable() {
                    init.setup(ctx)?;
                }
            }
        }
        Ok(())
    }

    /// Reinitialize plugin after hot reload
    pub fn reinitialize_plugin(&mut self, plugin_loader: &mut PluginLoader) -> Result<(), String> {
        // Drop old instance first
        self.plugin = None;

        // Create new instance from reloaded library
        match plugin_loader.create_plugin_instance("diagnostics") {
            Ok(plugin) => {
                self.plugin = Some(plugin);
                Ok(())
            }
            Err(e) => {
                eprintln!("Failed to recreate diagnostics plugin instance: {:?}", e);
                Err(format!("Failed to recreate diagnostics plugin: {:?}", e))
            }
        }
    }

    /// Check if plugin is initialized
    pub fn has_plugin(&self) -> bool {
        self.plugin.is_some()
    }

    /// Open a file and set up diagnostics (with instant cached results)
    pub fn open_file(
        &mut self,
        file_path: PathBuf,
        content: String,
        text_renderer: &crate::text_renderer::TextRenderer,
    ) {
        // Clear caches and pending state for new file
        self.definition_cache.clear();
        self.document_symbols.clear();
        self.pending_goto_definition = None;
        self.user_requested_goto_position = None;
        self.user_navigation_pending = false;
        self.pending_text_edits = None;
        self.cmd_hover_position = None;

        // NOTE: We skip applying cached diagnostics here because layout isn't ready yet
        // Diagnostics will be applied when LSP responds (which happens quickly after first render)

        // Load cached definitions immediately for instant go-to-definition
        if let Some(cached_defs) = LspManager::load_cached_definitions(&file_path, &content) {
            // Convert CachedLocation to LocationRef
            for ((line, col), locations) in cached_defs {
                let location_refs: Vec<_> = locations
                    .into_iter()
                    .map(|loc| crate::lsp_service::LocationRef {
                        file_path: loc.file_path,
                        position: crate::lsp_service::DocPosition {
                            line: loc.line,
                            column: loc.column,
                        },
                        text: String::new(),
                    })
                    .collect();
                self.definition_cache.insert((line, col), location_refs);
            }
        }

        // Start LSP service (handles all LSP features)
        eprintln!(
            "DiagnosticsManager::open_file() starting LSP for {:?}",
            file_path
        );
        self.lsp_service.open_file(file_path, content);

        // Request document symbols for hover support
        self.lsp_service.request_document_symbols();
    }

    /// Handle document changes with incremental updates
    pub fn document_changed_incremental(&mut self, changes: Vec<crate::lsp_manager::TextChange>) {
        self.definition_cache.clear();
        self.lsp_service.document_changed_incremental(changes);
    }

    /// Handle document changes (legacy full text)
    pub fn document_changed(&mut self, content: String) {
        self.definition_cache.clear();
        self.lsp_service.document_changed(content);
    }

    /// Handle document save
    pub fn document_saved(&mut self, content: String) {
        // Save definition cache immediately when file is saved
        if self.definition_cache_modified.is_some() {
            self.save_definition_cache();
            self.definition_cache_last_saved = Some(Instant::now());
        }
        self.lsp_service.document_saved(content);
    }

    /// Update diagnostics (call this every frame)
    pub fn update(
        &mut self,
        doc: &Doc,
        text_renderer: &crate::text_renderer::TextRenderer,
        editor_viewport: &tiny_sdk::types::WidgetViewport,
        mouse_screen_pos: Option<(f32, f32)>,
        line_height: f32,
    ) {
        // Update document reference in plugin for overview ruler
        if let Some(ref plugin_arc) = self.plugin {
            if let Ok(mut plugin) = plugin_arc.lock() {
                if let Some(library) = plugin.as_library_mut() {
                    // Set document pointer
                    let doc_ptr = doc as *const Doc as u64;
                    let args = doc_ptr.to_le_bytes();
                    let _ = library.call("set_document", &args);
                }
            }
        }

        // Calculate document position from mouse if available
        let mouse_pos = mouse_screen_pos.or(self.last_mouse_position);
        if let Some((mouse_x, mouse_y)) = mouse_pos {
            // Convert screen coordinates to document coordinates
            let local_x = mouse_x - editor_viewport.bounds.x.0;
            let local_y = mouse_y - editor_viewport.bounds.y.0;

            let doc_x = local_x + editor_viewport.scroll.x.0;
            let doc_y = local_y + editor_viewport.scroll.y.0;

            // Simple calculation: line from Y, column from X
            let line = (doc_y / line_height) as usize;

            // Find column by searching layout cache for glyphs on this line
            let column = if line < text_renderer.line_cache.len() {
                let line_info = &text_renderer.line_cache[line];
                let mut col = 0;
                for glyph in &text_renderer.layout_cache[line_info.char_range.clone()] {
                    if glyph.layout_pos.x.0 > doc_x {
                        break;
                    }
                    if glyph.char != '\n' {
                        col += 1;
                    }
                }
                col
            } else {
                0
            };

            // Send calculated position to plugin
            if let Some(ref plugin_arc) = self.plugin {
                if let Ok(mut plugin) = plugin_arc.lock() {
                    if let Some(library) = plugin.as_library_mut() {
                        #[repr(C)]
                        #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
                        struct DetectHoverArgs {
                            line: u32,
                            column: u32,
                            layout_x: f32,
                        }

                        let args_struct = DetectHoverArgs {
                            line: line as u32,
                            column: column as u32,
                            layout_x: doc_x,
                        };

                        library
                            .call("detect_hover", bytemuck::bytes_of(&args_struct))
                            .expect("detect_hover Library call failed");
                    }
                }
            }
        }

        // Check if plugin needs hover info (500ms timer elapsed)
        // The plugin's update() checks hover timers internally
        if let Some(ref plugin_arc) = self.plugin {
            if let Ok(mut plugin) = plugin_arc.lock() {
                // Call Updatable::update() if plugin implements it
                if let Some(updatable) = plugin.as_updatable() {
                    let mut update_ctx = tiny_sdk::UpdateContext {
                        registry: tiny_sdk::PluginRegistry::empty(),
                        frame: 0,     // Not used by diagnostics
                        elapsed: 0.0, // Not used by diagnostics
                    };
                    let _ = updatable.update(0.016, &mut update_ctx);
                }

                // Now check if plugin wants to request hover via Library trait
                if let Some(library) = plugin.as_library_mut() {
                    // Call "check_hover_request" to see if plugin wants hover info
                    // Returns 8 bytes: line (u32), column (u32) or empty if no request
                    if let Ok(result) = library.call("check_hover_request", &[]) {
                        if result.len() == 8 {
                            let line =
                                u32::from_le_bytes(result[0..4].try_into().unwrap()) as usize;
                            let column =
                                u32::from_le_bytes(result[4..8].try_into().unwrap()) as usize;

                            // Store this so we can send it back with hover content
                            self.last_hover_request = Some((line, column));

                            // Request hover from LSP
                            self.lsp_service
                                .request_hover(crate::lsp_service::DocPosition { line, column });
                        }
                    }
                }
            }
        }

        // Check if we should save definition cache (debounced)
        if let Some(modified_time) = self.definition_cache_modified {
            let should_save = self
                .definition_cache_last_saved
                .map(|saved_time| modified_time > saved_time)
                .unwrap_or(true);

            if should_save
                && modified_time.elapsed().as_secs() >= DEFINITION_CACHE_SAVE_DEBOUNCE_SECS
            {
                self.save_definition_cache();
                self.definition_cache_last_saved = Some(Instant::now());
            }
        }

        for result in self.lsp_service.poll_results() {
            match result {
                LspResult::Diagnostics(diagnostics) => {
                    self.apply_diagnostics(&diagnostics, text_renderer);
                    let content = doc.read().flatten_to_string();
                    if let Some(file_path) = self.lsp_service.current_file() {
                        LspManager::cache_diagnostics(file_path, &content, &diagnostics);
                    }
                }
                LspResult::Hover(Some(hover)) => {
                    // Send hover content to plugin with the position we requested
                    if let Some((line, column)) = self.last_hover_request {
                        if let Some(ref plugin_arc) = self.plugin {
                            if let Ok(mut plugin) = plugin_arc.lock() {
                                if let Some(library) = plugin.as_library_mut() {
                                    // Format: line (u32), column (u32), content_len (u32), content (bytes)
                                    let content_bytes = hover.contents.as_bytes();

                                    let mut args = Vec::new();
                                    args.extend_from_slice(&(line as u32).to_le_bytes());
                                    args.extend_from_slice(&(column as u32).to_le_bytes());
                                    args.extend_from_slice(
                                        &(content_bytes.len() as u32).to_le_bytes(),
                                    );
                                    args.extend_from_slice(content_bytes);

                                    let _ = library.call("set_hover_content", &args);
                                }
                            }
                        }
                    } else {
                        eprintln!(
                            "[DIAG] No last_hover_request stored, can't match hover response!"
                        );
                    }
                }
                LspResult::DocumentSymbols(symbols) => {
                    // Store symbols for proactive definition requests
                    self.document_symbols = symbols.clone();

                    // Send symbols to plugin via Library trait
                    if let Some(ref plugin_arc) = self.plugin {
                        if let Ok(mut plugin) = plugin_arc.lock() {
                            if let Some(library) = plugin.as_library_mut() {
                                // Format: count (u32), then for each symbol:
                                //   line (u32), col_start (u32), col_end (u32), start_x (f32), end_x (f32),
                                //   kind_len (u32), kind, name_len (u32), name
                                let plugin_symbols: Vec<_> = symbols
                                    .iter()
                                    .filter_map(|symbol| {
                                        let start_x = text_renderer.get_x_at_line_col(
                                            symbol.range.start.line,
                                            symbol.range.start.character as usize,
                                        )?;
                                        let end_x = text_renderer.get_x_at_line_col(
                                            symbol.range.end.line,
                                            symbol.range.end.character as usize,
                                        )?;

                                        Some((symbol, start_x, end_x))
                                    })
                                    .collect();

                                let mut args = Vec::new();
                                args.extend_from_slice(
                                    &(plugin_symbols.len() as u32).to_le_bytes(),
                                );

                                for (symbol, start_x, end_x) in plugin_symbols {
                                    let line = symbol.range.start.line as u32;
                                    let col_start = symbol.range.start.character as u32;
                                    let col_end = symbol.range.end.character as u32;
                                    let kind = format!("{:?}", symbol.kind);
                                    let kind_bytes = kind.as_bytes();
                                    let name_bytes = symbol.name.as_bytes();

                                    args.extend_from_slice(&line.to_le_bytes());
                                    args.extend_from_slice(&col_start.to_le_bytes());
                                    args.extend_from_slice(&col_end.to_le_bytes());
                                    args.extend_from_slice(&start_x.to_le_bytes());
                                    args.extend_from_slice(&end_x.to_le_bytes());
                                    args.extend_from_slice(
                                        &(kind_bytes.len() as u32).to_le_bytes(),
                                    );
                                    args.extend_from_slice(kind_bytes);
                                    args.extend_from_slice(
                                        &(name_bytes.len() as u32).to_le_bytes(),
                                    );
                                    args.extend_from_slice(name_bytes);
                                }

                                let _ = library.call("set_symbols", &args);
                            }
                        }
                    }

                    // Proactively request definitions for top-level symbols to warm cache
                    self.warm_definition_cache();
                }
                LspResult::GoToDefinition(locations) if !locations.is_empty() => {
                    // Check if user wants to navigate (flag is set by request_goto_definition)
                    if self.user_navigation_pending {
                        // User-requested navigation - trigger it
                        self.pending_goto_definition = Some(locations.clone());

                        // Also cache at the requested position if we know it
                        if let Some((line, col)) = self.user_requested_goto_position {
                            self.definition_cache.insert((line, col), locations.clone());
                            self.definition_cache_modified = Some(Instant::now());
                            self.user_requested_goto_position = None;
                        }
                    } else {
                        // Cache warming - just try to cache the result
                        // We can't reliably determine the position, so skip caching
                        // The warm_definition_cache results will naturally populate on user navigation
                    }
                }
                LspResult::CodeActions(actions) => {
                    if let Some(action) = actions
                        .iter()
                        .find(|a| a.is_preferred)
                        .or_else(|| actions.first())
                    {
                        self.lsp_service.execute_code_action(action);
                    }
                }
                LspResult::TextEdits(edits) => {
                    self.pending_text_edits = Some(edits);
                }
                _ => {}
            }
        }
    }

    /// Handle mouse movement for Cmd+hover go-to-definition preview
    pub fn on_mouse_move(&mut self, line: usize, column: usize, cmd_held: bool) {
        self.cmd_hover_position = if cmd_held { Some((line, column)) } else { None };
    }

    /// Store mouse screen position for hover detection
    pub fn set_mouse_screen_pos(&mut self, x: f32, y: f32) {
        self.last_mouse_position = Some((x, y));
    }

    /// Clear hover info when mouse leaves text area
    pub fn on_mouse_leave(&mut self) {
        self.last_mouse_position = None;
        self.cmd_hover_position = None;
    }

    /// Get Cmd+hover position for go-to-definition preview
    pub fn cmd_hover_position(&self) -> Option<(usize, usize)> {
        self.cmd_hover_position
    }

    /// Request go-to-definition at cursor position (checks cache first)
    pub fn request_goto_definition(&mut self, line: usize, column: usize) {
        // Mark this as a user-requested goto definition
        self.user_requested_goto_position = Some((line, column));
        self.user_navigation_pending = true;

        // Check cache first for instant response
        if let Some(cached_locations) = self.definition_cache.get(&(line, column)) {
            self.pending_goto_definition = Some(cached_locations.clone());
            // Keep the flag set - it will be cleared when navigation happens
            return;
        }

        // Cancel any pending cache warming requests to ensure the next response is ours
        self.lsp_service.cancel_pending_requests();

        // Request from LSP (flag will cause navigation when response arrives)
        self.lsp_service
            .request_goto_definition(crate::lsp_service::DocPosition { line, column });
    }

    /// Take pending go-to-definition result (consumes it)
    pub fn take_goto_definition(&mut self) -> Option<Vec<crate::lsp_service::LocationRef>> {
        let result = self.pending_goto_definition.take();
        if result.is_some() {
            self.user_navigation_pending = false; // Clear flag after navigation
        }
        result
    }

    /// Take pending text edits (consumes them)
    pub fn take_text_edits(&mut self) -> Option<Vec<crate::lsp_service::TextEdit>> {
        self.pending_text_edits.take()
    }

    /// Get mutable access to the plugin for rendering setup
    pub fn plugin_arc(&mut self) -> Option<&Arc<Mutex<Box<dyn Plugin>>>> {
        self.plugin.as_ref()
    }

    /// Get immutable access to the plugin for rendering
    pub fn plugin(&self) -> Option<&Arc<Mutex<Box<dyn Plugin>>>> {
        self.plugin.as_ref()
    }

    /// Apply diagnostics to the plugin (internal helper)
    /// REQUIRES: TextRenderer layout cache must be populated
    fn apply_diagnostics(
        &mut self,
        diagnostics: &[ParsedDiagnostic],
        text_renderer: &crate::text_renderer::TextRenderer,
    ) {
        // Skip if layout isn't ready yet (can happen during startup)
        if text_renderer.layout_cache.is_empty() {
            return;
        }

        // Clear diagnostics first
        if let Some(ref plugin_arc) = self.plugin {
            if let Ok(mut plugin) = plugin_arc.lock() {
                if let Some(library) = plugin.as_library_mut() {
                    let _ = library.call("clear_diagnostics", &[]);
                }
            }
        }

        // Add each diagnostic via Library trait
        for diag in diagnostics {
            // Get precise positions from layout cache
            let start_x = text_renderer
                .get_x_at_line_col(diag.line as u32, diag.column_start)
                .expect(&format!(
                    "Failed to get X position for diagnostic at line {}, col {}. \
                     Layout has {} lines in cache.",
                    diag.line,
                    diag.column_start,
                    text_renderer.line_cache.len()
                ));
            let end_x = text_renderer
                .get_x_at_line_col(diag.line as u32, diag.column_end)
                .expect(&format!(
                    "Failed to get X position for diagnostic at line {}, col {}. \
                     Layout has {} lines in cache.",
                    diag.line,
                    diag.column_end,
                    text_renderer.line_cache.len()
                ));

            // Call plugin via Library trait using proper bytemuck serialization
            if let Some(ref plugin_arc) = self.plugin {
                if let Ok(mut plugin) = plugin_arc.lock() {
                    if let Some(library) = plugin.as_library_mut() {
                        #[repr(C)]
                        #[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
                        struct DiagnosticHeader {
                            line: u32,
                            col_start: u32,
                            col_end: u32,
                            severity: u8,
                            _pad: [u8; 3], // Padding for alignment
                            start_x: f32,
                            end_x: f32,
                            message_len: u32,
                        }

                        let message_bytes = diag.message.as_bytes();
                        let header = DiagnosticHeader {
                            line: diag.line as u32,
                            col_start: diag.column_start as u32,
                            col_end: diag.column_end as u32,
                            severity: diag.severity as u8,
                            _pad: [0; 3],
                            start_x,
                            end_x,
                            message_len: message_bytes.len() as u32,
                        };

                        let mut args = Vec::new();
                        args.extend_from_slice(bytemuck::bytes_of(&header));
                        args.extend_from_slice(message_bytes);

                        let _ = library.call("add_diagnostic", &args);
                    }
                }
            }
        }
    }

    /// Get the LSP service for advanced features (hover, goto-def, etc.)
    pub fn lsp_service(&self) -> &LspService {
        &self.lsp_service
    }

    /// Get mutable LSP service for advanced features
    pub fn lsp_service_mut(&mut self) -> &mut LspService {
        &mut self.lsp_service
    }

    /// Proactively request definitions for all symbols to warm cache
    fn warm_definition_cache(&self) {
        for symbol in &self.document_symbols {
            let line = symbol.selection_range.start.line as usize;
            let column = symbol.selection_range.start.character as usize;

            if !self.definition_cache.contains_key(&(line, column)) {
                self.lsp_service
                    .request_goto_definition(crate::lsp_service::DocPosition { line, column });
            }
        }
    }

    /// Save definition cache to disk
    fn save_definition_cache(&self) {
        if let Some(file_path) = self.lsp_service.current_file() {
            if let Ok(content) = std::fs::read_to_string(file_path) {
                let mut cached_defs = AHashMap::new();
                for ((line, col), locations) in &self.definition_cache {
                    let cached_locations: Vec<_> = locations
                        .iter()
                        .map(|loc| crate::lsp_manager::CachedLocation {
                            file_path: loc.file_path.clone(),
                            line: loc.position.line,
                            column: loc.position.column,
                        })
                        .collect();
                    cached_defs.insert((*line, *col), cached_locations);
                }
                LspManager::cache_definitions(file_path, &content, &cached_defs);
            }
        }
    }
}
