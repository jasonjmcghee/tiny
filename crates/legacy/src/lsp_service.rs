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

/// Position in a document (line, column in UTF-16 code units for LSP compatibility)
#[derive(Debug, Clone, Copy)]
pub struct DocPosition {
    pub line: usize,
    pub column: usize, // UTF-16 code units
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

/// Code action available at a position
#[derive(Debug, Clone)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub action: lsp_types::CodeActionOrCommand,
}

/// Text edit to apply to document
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub range_utf16: (DocPosition, DocPosition), // Start and end positions in UTF-16
    pub new_text: String,
}

/// LSP feature results
#[derive(Debug, Clone)]
pub enum LspResult {
    Diagnostics(Vec<ParsedDiagnostic>),
    Hover(Option<HoverInfo>),
    GoToDefinition(Vec<LocationRef>),
    FindReferences(Vec<LocationRef>),
    DocumentSymbols(Vec<lsp_types::DocumentSymbol>),
    CodeActions(Vec<CodeAction>),
    TextEdits(Vec<TextEdit>),
}

/// Comprehensive LSP service
pub struct LspService {
    lsp_manager: Option<Arc<LspManager>>,
    current_file: Option<PathBuf>,
}

impl LspService {
    /// Create a new LSP service
    pub fn new() -> Self {
        Self {
            lsp_manager: None,
            current_file: None,
        }
    }

    /// Open a file and initialize LSP
    pub fn open_file(&mut self, file_path: PathBuf, content: String) {
        // Only handle Rust files for now
        if !file_path.to_string_lossy().ends_with(".rs") {
            self.current_file = Some(file_path);
            return;
        }

        let abs_path = std::fs::canonicalize(&file_path)
            .unwrap_or_else(|_| file_path.clone());

        // Skip LSP for dependency/library files (read-only, not part of current workspace)
        let path_str = abs_path.to_string_lossy();
        if path_str.contains("/.cargo/registry/")
            || path_str.contains("/target/")
            || path_str.contains("/.rustup/")
        {
            self.current_file = Some(abs_path);
            return;
        }

        self.current_file = Some(abs_path.clone());

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

    /// Handle document changes with incremental updates
    pub fn document_changed_incremental(&self, changes: Vec<crate::lsp_manager::TextChange>) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.document_changed(changes);
        }
    }

    /// Handle document changes (legacy full text)
    pub fn document_changed(&self, content: String) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.document_changed_full(content);
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

        if let Some(ref lsp) = self.lsp_manager {
            for response in lsp.poll_responses() {
                match response {
                    crate::lsp_manager::LspResponse::Diagnostics(diagnostic_update) => {
                        results.push(LspResult::Diagnostics(diagnostic_update.diagnostics));
                    }
                    crate::lsp_manager::LspResponse::Hover(hover_update) => {
                        results.push(LspResult::Hover(Some(HoverInfo {
                            contents: hover_update.content,
                            range: None, // TODO: Track hover range
                        })));
                    }
                    crate::lsp_manager::LspResponse::Symbols(symbols_update) => {
                        results.push(LspResult::DocumentSymbols(symbols_update.symbols));
                    }
                    crate::lsp_manager::LspResponse::GotoDefinition(goto_def_update) => {
                        let location_refs: Vec<LocationRef> = goto_def_update
                            .locations
                            .into_iter()
                            .filter_map(|loc| {
                                // Convert URI path to PathBuf
                                let uri_str = loc.uri.as_str();
                                let file_path = if uri_str.starts_with("file://") {
                                    PathBuf::from(&uri_str[7..])
                                } else {
                                    return None;
                                };

                                Some(LocationRef {
                                    file_path,
                                    position: DocPosition {
                                        line: loc.range.start.line as usize,
                                        column: loc.range.start.character as usize,
                                    },
                                    text: String::new(), // We don't have the text yet
                                })
                            })
                            .collect();
                        results.push(LspResult::GoToDefinition(location_refs));
                    }
                    crate::lsp_manager::LspResponse::CodeAction(code_action_update) => {
                        let actions: Vec<CodeAction> = code_action_update
                            .actions
                            .into_iter()
                            .map(|action| {
                                let (title, kind, is_preferred) = match &action {
                                    lsp_types::CodeActionOrCommand::Command(cmd) => {
                                        (cmd.title.clone(), None, false)
                                    }
                                    lsp_types::CodeActionOrCommand::CodeAction(ca) => {
                                        (
                                            ca.title.clone(),
                                            ca.kind.as_ref().map(|k| k.as_str().to_string()),
                                            ca.is_preferred.unwrap_or(false),
                                        )
                                    }
                                };
                                CodeAction {
                                    title,
                                    kind,
                                    is_preferred,
                                    action,
                                }
                            })
                            .collect();
                        results.push(LspResult::CodeActions(actions));
                    }
                    crate::lsp_manager::LspResponse::TextEdit(text_edit_update) => {
                        let edits: Vec<TextEdit> = text_edit_update
                            .edits
                            .into_iter()
                            .map(|edit| TextEdit {
                                range_utf16: (
                                    DocPosition {
                                        line: edit.range.start.line as usize,
                                        column: edit.range.start.character as usize,
                                    },
                                    DocPosition {
                                        line: edit.range.end.line as usize,
                                        column: edit.range.end.character as usize,
                                    },
                                ),
                                new_text: edit.new_text,
                            })
                            .collect();
                        results.push(LspResult::TextEdits(edits));
                    }
                    crate::lsp_manager::LspResponse::Error(err) => {
                        eprintln!("LSP error: {}", err);
                    }
                }
            }
        }

        results
    }

    /// Request hover information at position
    pub fn request_hover(&self, position: DocPosition) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.request_hover(position.line as u32, position.column as u32);
        }
    }

    /// Request go-to-definition at position
    pub fn request_goto_definition(&self, position: DocPosition) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.request_goto_definition(position.line as u32, position.column as u32);
        }
    }

    /// Request document symbols
    pub fn request_document_symbols(&self) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.request_document_symbols();
        }
    }

    /// Request find references at position
    pub fn request_find_references(&self, position: DocPosition) {
        // TODO: Send find references request to LSP
    }

    /// Request code actions at position (for auto-fix)
    pub fn request_code_action(&self, position: DocPosition) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.request_code_action(position.line as u32, position.column as u32);
        }
    }

    /// Execute a code action (for auto-fix)
    pub fn execute_code_action(&self, action: &CodeAction) {
        if let Some(ref lsp) = self.lsp_manager {
            lsp.execute_code_action(&action.action);
        }
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