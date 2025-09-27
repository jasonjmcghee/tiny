//! High-level diagnostics manager that encapsulates LSP, caching, and plugin integration

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use crate::lsp_service::{LspService, LspResult};
use std::path::PathBuf;
use std::sync::Arc;
use tiny_tree::Doc;

/// High-level diagnostics manager (now uses LspService for broader LSP support)
pub struct DiagnosticsManager {
    plugin: diagnostics_plugin::DiagnosticsPlugin,
    lsp_service: LspService,
}

impl DiagnosticsManager {
    /// Create a new diagnostics manager
    pub fn new() -> Self {
        Self {
            plugin: diagnostics_plugin::DiagnosticsPlugin::new(),
            lsp_service: LspService::new(),
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
    }

    /// Handle document changes
    pub fn document_changed(&mut self, content: String) {
        self.lsp_service.document_changed(content);
    }

    /// Handle document save
    pub fn document_saved(&mut self, content: String) {
        self.lsp_service.document_saved(content);
    }

    /// Update diagnostics (call this every frame)
    pub fn update(&mut self, doc: &Doc) {
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
                LspResult::Hover(_) => {
                    // TODO: Handle hover results when we implement hover UI
                }
                LspResult::GoToDefinition(_) => {
                    // TODO: Handle goto definition results
                }
                LspResult::FindReferences(_) => {
                    // TODO: Handle find references results
                }
            }
        }
    }

    /// Request hover information at cursor position (for future use)
    pub fn request_hover(&self, line: usize, column: usize) {
        self.lsp_service.request_hover(crate::lsp_service::DocPosition { line, column });
    }

    /// Request go-to-definition at cursor position (for future use)
    pub fn request_goto_definition(&self, line: usize, column: usize) {
        self.lsp_service.request_goto_definition(crate::lsp_service::DocPosition { line, column });
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