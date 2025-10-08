//! High-level diagnostics manager that encapsulates LSP, caching, and plugin integration

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use crate::lsp_service::{LspResult, LspService};
use ahash::AHashMap;
use std::path::PathBuf;
use std::time::Instant;
use tiny_tree::Doc;

const DEFINITION_CACHE_SAVE_DEBOUNCE_SECS: u64 = 5;

/// High-level diagnostics manager (now uses LspService for broader LSP support)
pub struct DiagnosticsManager {
    plugin: diagnostics_plugin::DiagnosticsPlugin,
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
            plugin: diagnostics_plugin::DiagnosticsPlugin::new(),
            lsp_service: LspService::new(),
            pending_goto_definition: None,
            user_requested_goto_position: None,
            user_navigation_pending: false,
            cmd_hover_position: None,
            pending_text_edits: None,
            definition_cache: AHashMap::new(),
            document_symbols: Vec::new(),
            definition_cache_modified: None,
            definition_cache_last_saved: None,
        }
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
    pub fn update(&mut self, doc: &Doc, text_renderer: &crate::text_renderer::TextRenderer) {
        // Check if plugin needs hover info (500ms timer elapsed)
        if let Some((line, column)) = self.plugin.update() {
            self.lsp_service
                .request_hover(crate::lsp_service::DocPosition { line, column });
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
                    if let Some((line, col)) = self.plugin.get_mouse_document_position() {
                        self.plugin.set_hover_content(hover.contents, line, col);
                    }
                }
                LspResult::DocumentSymbols(symbols) => {
                    // Store symbols for proactive definition requests
                    self.document_symbols = symbols.clone();

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

                            Some(diagnostics_plugin::Symbol {
                                name: symbol.name.clone(),
                                line: symbol.range.start.line as usize,
                                column_range: (
                                    symbol.range.start.character as usize,
                                    symbol.range.end.character as usize,
                                ),
                                start_x,
                                end_x,
                                kind: format!("{:?}", symbol.kind),
                            })
                        })
                        .collect();
                    self.plugin.set_symbols(plugin_symbols);

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

    /// Clear hover info when mouse leaves text area
    pub fn on_mouse_leave(&mut self) {
        self.plugin.clear_symbols();
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
    pub fn plugin_mut(&mut self) -> &mut diagnostics_plugin::DiagnosticsPlugin {
        &mut self.plugin
    }

    /// Get immutable access to the plugin for rendering
    pub fn plugin(&self) -> &diagnostics_plugin::DiagnosticsPlugin {
        &self.plugin
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

        self.plugin.clear_diagnostics();

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

            self.plugin.add_diagnostic_with_positions(
                diag.line,
                (diag.column_start, diag.column_end),
                diag.message.clone(),
                diag.severity,
                start_x,
                end_x,
            );
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
