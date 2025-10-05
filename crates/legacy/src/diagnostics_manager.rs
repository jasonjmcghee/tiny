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
            cmd_hover_position: None,
            pending_text_edits: None,
            definition_cache: AHashMap::new(),
            document_symbols: Vec::new(),
            definition_cache_modified: None,
            definition_cache_last_saved: None,
        }
    }

    /// Open a file and set up diagnostics (with instant cached results)
    pub fn open_file(&mut self, file_path: PathBuf, content: String) {
        // Clear caches for new file
        self.definition_cache.clear();
        self.document_symbols.clear();

        // Load cached diagnostics immediately
        if let Some(cached_diagnostics) = LspManager::load_cached_diagnostics(&file_path, &content)
        {
            self.apply_diagnostics(&cached_diagnostics, &content);
        }

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
    pub fn update(&mut self, doc: &Doc) {
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
                    let content = doc.read().flatten_to_string();
                    self.apply_diagnostics(&diagnostics, &content);
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
                        .map(|symbol| diagnostics_plugin::Symbol {
                            name: symbol.name.clone(),
                            line: symbol.range.start.line as usize,
                            column_range: (
                                symbol.range.start.character as usize,
                                symbol.range.end.character as usize,
                            ),
                            kind: format!("{:?}", symbol.kind),
                        })
                        .collect();
                    self.plugin.set_symbols(plugin_symbols);

                    // Proactively request definitions for top-level symbols to warm cache
                    self.warm_definition_cache();
                }
                LspResult::GoToDefinition(locations) if !locations.is_empty() => {
                    // Store in cache for instant future lookups
                    if let Some((line, col)) = self.plugin.get_mouse_document_position() {
                        self.definition_cache.insert((line, col), locations.clone());
                        self.definition_cache_modified = Some(Instant::now());
                    }
                    self.pending_goto_definition = Some(locations);
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
        // Check cache first for instant response
        if let Some(cached_locations) = self.definition_cache.get(&(line, column)) {
            self.pending_goto_definition = Some(cached_locations.clone());
            return;
        }

        // Not in cache, request from LSP
        self.lsp_service
            .request_goto_definition(crate::lsp_service::DocPosition { line, column });
    }

    /// Take pending go-to-definition result (consumes it)
    pub fn take_goto_definition(&mut self) -> Option<Vec<crate::lsp_service::LocationRef>> {
        self.pending_goto_definition.take()
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
    fn apply_diagnostics(&mut self, diagnostics: &[ParsedDiagnostic], content: &str) {
        self.plugin.clear_diagnostics();
        let lines: Vec<&str> = content.lines().collect();
        for diag in diagnostics {
            if let Some(&line_text) = lines.get(diag.line) {
                self.plugin.add_diagnostic_with_line_text(
                    diagnostics_plugin::Diagnostic {
                        line: diag.line,
                        column_range: (diag.column_start, diag.column_end),
                        byte_range: None,
                        message: diag.message.clone(),
                        severity: diag.severity,
                    },
                    line_text.to_string(),
                );
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
