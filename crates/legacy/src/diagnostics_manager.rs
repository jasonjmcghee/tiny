//! High-level diagnostics manager that encapsulates LSP, caching, and plugin integration

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use crate::lsp_service::{LspService, LspResult};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tiny_tree::Doc;

/// High-level diagnostics manager (now uses LspService for broader LSP support)
pub struct DiagnosticsManager {
    plugin: diagnostics_plugin::DiagnosticsPlugin,
    lsp_service: LspService,
    last_hover_request: Option<(usize, usize, Instant)>, // (line, col, time) for debouncing
    current_hover_position: Option<(usize, usize)>, // Current hover position for tracking
    /// Pending go-to-definition result
    pending_goto_definition: Option<Vec<crate::lsp_service::LocationRef>>,
    /// Current hover position with Cmd held (for go-to-definition preview)
    cmd_hover_position: Option<(usize, usize)>,
}

impl DiagnosticsManager {
    /// Create a new diagnostics manager
    pub fn new() -> Self {
        Self {
            plugin: diagnostics_plugin::DiagnosticsPlugin::new(),
            lsp_service: LspService::new(),
            last_hover_request: None,
            current_hover_position: None,
            pending_goto_definition: None,
            cmd_hover_position: None,
        }
    }

    /// Open a file and set up diagnostics (with instant cached results)
    pub fn open_file(&mut self, file_path: PathBuf, content: String) {
        // 1. INSTANT: Load cached diagnostics immediately
        if let Some(cached_diagnostics) = LspManager::load_cached_diagnostics(&file_path, &content) {
            self.apply_diagnostics(&cached_diagnostics, &content);
        }

        // 2. BACKGROUND: Start LSP service (handles all LSP features)
        self.lsp_service.open_file(file_path, content);

        // 3. Request document symbols for hover support
        self.lsp_service.request_document_symbols();
    }

    /// Handle document changes with incremental updates
    pub fn document_changed_incremental(&mut self, changes: Vec<crate::lsp_manager::TextChange>) {
        self.lsp_service.document_changed_incremental(changes);
    }

    /// Handle document changes (legacy full text)
    pub fn document_changed(&mut self, content: String) {
        self.lsp_service.document_changed(content);
    }

    /// Handle document save
    pub fn document_saved(&mut self, content: String) {
        self.lsp_service.document_saved(content);
    }

    /// Update diagnostics (call this every frame)
    pub fn update(&mut self, doc: &Doc) {
        // Check if plugin needs hover info (500ms timer elapsed)
        if let Some((line, column)) = self.plugin.update() {
            // Request hover from LSP
            self.lsp_service.request_hover(crate::lsp_service::DocPosition { line, column });
        }

        // Poll for any LSP results
        let results = self.lsp_service.poll_results();

        for result in results {
            match result {
                LspResult::Diagnostics(diagnostics) => {
                    let content = doc.read().flatten_to_string();

                    // Apply fresh diagnostics
                    self.apply_diagnostics(&diagnostics, &content);

                    // Cache them for next time
                    if let Some(file_path) = self.lsp_service.current_file() {
                        LspManager::cache_diagnostics(file_path, &content, &diagnostics);
                    }
                }
                LspResult::Hover(hover_info) => {
                    // Send hover content to plugin
                    if let Some(hover) = hover_info {
                        if let Some((line, col)) = self.current_hover_position {
                            self.plugin.set_hover_content(hover.contents, line, col);
                        }
                    }
                }
                LspResult::DocumentSymbols(symbols) => {
                    // Convert LSP symbols to plugin symbols
                    let mut plugin_symbols = Vec::new();
                    for symbol in symbols {
                        // Convert line/character positions to our format
                        plugin_symbols.push(diagnostics_plugin::Symbol {
                            name: symbol.name,
                            line: symbol.range.start.line as usize,
                            column_range: (
                                symbol.range.start.character as usize,
                                symbol.range.end.character as usize,
                            ),
                            kind: format!("{:?}", symbol.kind),
                        });
                    }
                    self.plugin.set_symbols(plugin_symbols);

                    // Request symbols again if file changed
                    // This ensures we always have up-to-date symbols
                }
                LspResult::GoToDefinition(locations) => {
                    // Store for app.rs to consume
                    self.pending_goto_definition = Some(locations);
                }
                LspResult::FindReferences(_) => {
                    // TODO: Handle find references results
                }
            }
        }
    }

    /// Handle mouse movement (requests hover info from LSP with debouncing)
    pub fn on_mouse_move(&mut self, line: usize, column: usize, cmd_held: bool) {
        let now = Instant::now();
        self.current_hover_position = Some((line, column));

        // Track Cmd+hover for go-to-definition preview
        if cmd_held {
            self.cmd_hover_position = Some((line, column));
        } else {
            self.cmd_hover_position = None;
        }

        // Debounce hover requests - only send if position changed and enough time passed
        let should_request = if let Some((last_line, last_col, last_time)) = self.last_hover_request {
            // Different position or enough time has passed
            (line != last_line || column != last_col) && now.duration_since(last_time).as_millis() > 100
        } else {
            true // First request
        };

        if should_request && self.lsp_service.is_ready() {
            self.lsp_service.request_hover(crate::lsp_service::DocPosition { line, column });
            self.last_hover_request = Some((line, column, now));
        }
    }

    /// Clear hover info when mouse leaves text area
    pub fn on_mouse_leave(&mut self) {
        self.plugin.clear_symbols();
        self.last_hover_request = None;
        self.current_hover_position = None;
        self.cmd_hover_position = None;
    }

    /// Get Cmd+hover position for go-to-definition preview
    pub fn cmd_hover_position(&self) -> Option<(usize, usize)> {
        self.cmd_hover_position
    }

    /// Request hover information at cursor position (for future use)
    pub fn request_hover(&self, line: usize, column: usize) {
        self.lsp_service.request_hover(crate::lsp_service::DocPosition { line, column });
    }

    /// Request go-to-definition at cursor position
    pub fn request_goto_definition(&self, line: usize, column: usize) {
        eprintln!("DEBUG: DiagnosticsManager requesting goto_definition at line {}, col {}", line, column);
        self.lsp_service.request_goto_definition(crate::lsp_service::DocPosition { line, column });
    }

    /// Take pending go-to-definition result (consumes it)
    pub fn take_goto_definition(&mut self) -> Option<Vec<crate::lsp_service::LocationRef>> {
        self.pending_goto_definition.take()
    }

    /// Poll for LSP results and update plugin state
    pub fn poll_lsp_results(&mut self) {
        self.lsp_service.poll_results();
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
        eprintln!("DIAG: Clearing old diagnostics and applying {} new ones", diagnostics.len());
        self.plugin.clear_diagnostics();

        let lines: Vec<&str> = content.lines().collect();
        for diag in diagnostics {
            if let Some(line_text) = lines.get(diag.line) {
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
}