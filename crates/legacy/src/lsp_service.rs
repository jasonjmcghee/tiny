//! Comprehensive LSP service providing all language server features
//!
//! Supports diagnostics, hover, go-to-definition, find references, etc.

use crate::lsp_manager::{LspManager, ParsedDiagnostic};
use lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, Position, Range,
    ReferenceParams, TextDocumentIdentifier, TextDocumentPositionParams,
};
use std::path::PathBuf;
use std::sync::Arc;
use tiny_tree::Doc;

/// Position in a document (line, column)
#[derive(Debug, Clone, Copy)]
pub struct DocPosition {
    pub line: usize,
    pub column: usize,
}

/// Location reference (file + position)
#[derive(Debug, Clone)]
pub struct LocationRef {
    pub file_path: PathBuf,
    pub position: DocPosition,
    pub text: String, // The text at this location
}

/// Hover information
#[derive(Debug, Clone)]
pub struct HoverInfo {
    pub contents: String,
    pub range: Option<(DocPosition, DocPosition)>,
}

/// LSP feature results
#[derive(Debug, Clone)]
pub enum LspResult {
    Diagnostics(Vec<ParsedDiagnostic>),
    Hover(Option<HoverInfo>),
    GoToDefinition(Vec<LocationRef>),
    FindReferences(Vec<LocationRef>),
}

/// Comprehensive LSP service
pub struct LspService {
    lsp_manager: Option<Arc<LspManager>>,
    current_file: Option<PathBuf>,
    results_rx: std::sync::mpsc::Receiver<LspResult>,
    _results_tx: std::sync::mpsc::Sender<LspResult>, // Keep sender alive
}

impl LspService {
    /// Create a new LSP service
    pub fn new() -> Self {
        let (results_tx, results_rx) = std::sync::mpsc::channel();

        Self {
            lsp_manager: None,
            current_file: None,
            results_rx,
            _results_tx: results_tx,
        }
    }

    /// Open a file and initialize LSP
    pub fn open_file(&mut self, file_path: PathBuf, content: String) {
        self.current_file = Some(file_path.clone());

        // Only handle Rust files for now
        if !file_path.to_string_lossy().ends_with(".rs") {
            return;
        }

        // Find workspace root (same logic as before)
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

    /// Handle document changes
    pub fn document_changed(&self, content: String) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.document_changed(content);
        }
    }

    /// Handle document save
    pub fn document_saved(&self, content: String) {
        if let (Some(ref lsp), Some(ref file_path)) = (&self.lsp_manager, &self.current_file) {
            lsp.document_saved(file_path.clone(), content);
        }
    }

    /// Poll for any LSP results (diagnostics, hover, etc.)
    pub fn poll_results(&self) -> Vec<LspResult> {
        let mut results = Vec::new();

        // Poll diagnostics
        if let Some(ref lsp) = self.lsp_manager {
            if let Some(diagnostic_update) = lsp.poll_diagnostics() {
                results.push(LspResult::Diagnostics(diagnostic_update.diagnostics));
            }
        }

        // Poll other LSP results from the channel
        while let Ok(result) = self.results_rx.try_recv() {
            results.push(result);
        }

        results
    }

    /// Request hover information at position
    pub fn request_hover(&self, position: DocPosition) {
        // TODO: Send hover request to LSP
        // This would involve:
        // 1. Converting DocPosition to LSP Position
        // 2. Sending textDocument/hover request
        // 3. Parsing response and sending via results channel
    }

    /// Request go-to-definition at position
    pub fn request_goto_definition(&self, position: DocPosition) {
        // TODO: Send goto definition request to LSP
    }

    /// Request find references at position
    pub fn request_find_references(&self, position: DocPosition) {
        // TODO: Send find references request to LSP
    }

    /// Check if LSP is ready for requests
    pub fn is_ready(&self) -> bool {
        self.lsp_manager.is_some()
    }

    /// Get current file path
    pub fn current_file(&self) -> Option<&PathBuf> {
        self.current_file.as_ref()
    }
}

/// Helper to convert editor positions to LSP positions
impl From<DocPosition> for Position {
    fn from(pos: DocPosition) -> Self {
        Position {
            line: pos.line as u32,
            character: pos.column as u32,
        }
    }
}

/// Helper to convert LSP positions to editor positions
impl From<Position> for DocPosition {
    fn from(pos: Position) -> Self {
        DocPosition {
            line: pos.line as usize,
            column: pos.character as usize,
        }
    }
}