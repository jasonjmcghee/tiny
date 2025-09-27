//! High-level diagnostics manager that encapsulates LSP, caching, and plugin integration

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use std::path::PathBuf;
use std::sync::Arc;
use tiny_tree::Doc;

/// High-level diagnostics manager
pub struct DiagnosticsManager {
    plugin: diagnostics_plugin::DiagnosticsPlugin,
    lsp_manager: Option<Arc<LspManager>>,
    current_file: Option<PathBuf>,
}

impl DiagnosticsManager {
    /// Create a new diagnostics manager
    pub fn new() -> Self {
        Self {
            plugin: diagnostics_plugin::DiagnosticsPlugin::new(),
            lsp_manager: None,
            current_file: None,
        }
    }

    /// Open a file and set up diagnostics (with instant cached results)
    pub fn open_file(&mut self, file_path: PathBuf, content: String) {
        self.current_file = Some(file_path.clone());

        // Only handle Rust files for now
        if !file_path.to_string_lossy().ends_with(".rs") {
            return;
        }

        // 1. INSTANT: Load cached diagnostics immediately
        if let Some(cached_diagnostics) = LspManager::load_cached_diagnostics(&file_path, &content) {
            self.apply_diagnostics(&cached_diagnostics, &content);
        }

        // 2. BACKGROUND: Start fresh LSP analysis
        self.start_lsp_analysis(file_path, content);
    }

    /// Handle document changes
    pub fn document_changed(&mut self, content: String) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.document_changed(content);
        }
    }

    /// Handle document save
    pub fn document_saved(&mut self, content: String) {
        if let (Some(ref lsp), Some(ref file_path)) = (&self.lsp_manager, &self.current_file) {
            lsp.document_saved(file_path.clone(), content);
        }
    }

    /// Update diagnostics (call this every frame)
    pub fn update(&mut self, doc: &Doc) {
        // Poll for fresh LSP diagnostics
        if let Some(ref lsp) = self.lsp_manager {
            if let Some(update) = lsp.poll_diagnostics() {
                let content = doc.read().flatten_to_string();

                // Apply fresh diagnostics
                self.apply_diagnostics(&update.diagnostics, &content);

                // Cache them for next time
                if let Some(ref file_path) = self.current_file {
                    LspManager::cache_diagnostics(file_path, &content, &update.diagnostics);
                }
            }
        }
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

    /// Start LSP analysis for a file (internal helper)
    fn start_lsp_analysis(&mut self, file_path: PathBuf, content: String) {
        // Find workspace root
        let abs_path = std::fs::canonicalize(&file_path)
            .unwrap_or_else(|_| file_path.clone());

        let workspace_root = abs_path
            .parent()
            .and_then(|p| {
                let mut current = p;
                let mut found_cargo_toml = None;
                loop {
                    if current.join("Cargo.toml").exists() {
                        found_cargo_toml = Some(current.to_path_buf());
                    }
                    match current.parent() {
                        Some(parent) => current = parent,
                        None => break,
                    }
                }
                found_cargo_toml
            });

        // Get or create LSP manager
        match LspManager::get_or_create_global(workspace_root) {
            Ok(manager) => {
                manager.initialize(abs_path, content);
                self.lsp_manager = Some(manager);
            }
            Err(e) => {
                eprintln!("Failed to start LSP: {}", e);
            }
        }
    }
}