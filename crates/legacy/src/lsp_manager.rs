//! LSP (Language Server Protocol) manager for real-time diagnostics
//!
//! Currently supports rust-analyzer, designed to be extensible to other language servers

use ahash::AHasher;
use lsp_types::{
    ClientCapabilities, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, InitializeParams,
    InitializeResult, InitializedParams, Position,
    PublishDiagnosticsParams, Range,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem, Uri,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tiny_tree as tree;
use url::Url;

// Configuration constants
const CHANGE_DEBOUNCE_MS: u64 = 200;
const REQUEST_POLL_TIMEOUT_MS: u64 = 50;
const REQUEST_TIMEOUT_CHECK_INTERVAL_SECS: u64 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const STATE_LOG_INTERVAL_SECS: u64 = 2;

/// Unified error type for LSP operations
#[derive(Debug)]
pub enum LspError {
    Io(std::io::Error),
    JsonSerialization(serde_json::Error),
    InvalidFilePath(PathBuf),
    InvalidUri(String),
    LspNotInitialized,
    LspShutdown,
    RequestTimeout(u64),
    InvalidPosition { line: u32, character: u32 },
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::JsonSerialization(e) => write!(f, "JSON error: {}", e),
            Self::InvalidFilePath(path) => write!(f, "Invalid file path: {:?}", path),
            Self::InvalidUri(uri) => write!(f, "Invalid URI: {}", uri),
            Self::LspNotInitialized => write!(f, "LSP not initialized"),
            Self::LspShutdown => write!(f, "LSP is shut down"),
            Self::RequestTimeout(id) => write!(f, "Request {} timed out", id),
            Self::InvalidPosition { line, character } => {
                write!(f, "Invalid position: line {}, char {}", line, character)
            }
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for LspError {
    fn from(e: serde_json::Error) -> Self {
        Self::JsonSerialization(e)
    }
}

/// Represents a text change in the document
#[derive(Debug, Clone)]
pub struct TextChange {
    pub range: Range,
    pub text: String,
}

/// LSP request types for the background thread
#[derive(Debug, Clone)]
pub enum LspRequest {
    Initialize {
        file_path: PathBuf,
        text: String,
    },
    DocumentChanged {
        changes: Vec<TextChange>,
        version: u64,
    },
    DocumentSaved {
        path: PathBuf,
        text: String,
    },
    Hover {
        line: u32,
        character: u32,
    },
    DocumentSymbols,
    GotoDefinition {
        line: u32,
        character: u32,
    },
    CodeAction {
        line: u32,
        character: u32,
        diagnostics: Vec<LspDiagnostic>,
    },
    ExecuteCommand {
        command: String,
        arguments: Vec<serde_json::Value>,
    },
    ApplyWorkspaceEdit {
        edit: lsp_types::WorkspaceEdit,
    },
    CancelPendingRequests,
    Shutdown,
}

/// Diagnostic update from LSP server
#[derive(Debug, Clone)]
pub struct DiagnosticUpdate {
    pub diagnostics: Vec<ParsedDiagnostic>,
    pub version: u64,
}

/// Hover information from LSP server
#[derive(Debug, Clone)]
pub struct HoverUpdate {
    pub content: String,
    pub line: u32,
    pub character: u32,
}

/// Document symbols from LSP server
#[derive(Debug, Clone)]
pub struct SymbolsUpdate {
    pub symbols: Vec<DocumentSymbol>,
}

/// Go-to-definition result from LSP server
#[derive(Debug, Clone)]
pub struct GotoDefinitionUpdate {
    pub locations: Vec<Location>,
}

/// Code actions available at a position
#[derive(Debug, Clone)]
pub struct CodeActionUpdate {
    pub actions: Vec<lsp_types::CodeActionOrCommand>,
}

/// Text edits to apply to the current document
#[derive(Debug, Clone)]
pub struct TextEditUpdate {
    pub uri: Uri,
    pub edits: Vec<lsp_types::TextEdit>,
}

use lsp_types::Location;

/// Unified response type for all LSP responses
#[derive(Debug, Clone)]
pub enum LspResponse {
    Diagnostics(DiagnosticUpdate),
    Hover(HoverUpdate),
    Symbols(SymbolsUpdate),
    GotoDefinition(GotoDefinitionUpdate),
    CodeAction(CodeActionUpdate),
    TextEdit(TextEditUpdate),
    Error(String),
}

/// Parsed diagnostic ready for the plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedDiagnostic {
    pub line: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub message: String,
    pub severity: diagnostics_plugin::DiagnosticSeverity,
}

/// Cached diagnostics with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDiagnostics {
    pub diagnostics: Vec<ParsedDiagnostic>,
    pub file_path: PathBuf,
    pub content_hash: u64,
    pub modification_time: SystemTime,
    pub cached_at: SystemTime,
}

/// Cached definitions with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedDefinitions {
    /// Map of (line, column) to locations
    pub definitions: ahash::AHashMap<(usize, usize), Vec<CachedLocation>>,
    pub file_path: PathBuf,
    pub content_hash: u64,
    pub modification_time: SystemTime,
    pub cached_at: SystemTime,
}

/// Simplified location for caching (avoids complex LSP types)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedLocation {
    pub file_path: PathBuf,
    pub line: usize,
    pub column: usize,
}


/// Document state for tracking URI and version
#[derive(Debug, Clone)]
struct DocumentInfo {
    uri: Uri,
    version: u64,
}

/// Main LSP manager
pub struct LspManager {
    tx: mpsc::Sender<LspRequest>,
    response_rx: Arc<Mutex<mpsc::Receiver<LspResponse>>>,
    current_diagnostics: Arc<RwLock<Vec<LspDiagnostic>>>,
    /// Current document info (URI and version)
    document_info: Arc<RwLock<Option<DocumentInfo>>>,
    workspace_root: Option<PathBuf>,
}

/// Global LSP manager instance (initialized once)
///
/// NOTE: This singleton pattern is convenient but makes testing difficult.
/// For better testability, consider using `LspManager::new_for_rust()` directly
/// and managing the instance yourself. The singleton is provided for convenience
/// in applications where a single global LSP instance is sufficient.
static LSP_INSTANCE: std::sync::OnceLock<Arc<Mutex<Option<Arc<LspManager>>>>> =
    std::sync::OnceLock::new();

impl LspManager {
    /// Create a new LSP manager for Rust files
    pub fn new_for_rust(workspace_root: Option<PathBuf>) -> Result<Self, std::io::Error> {
        let (request_tx, request_rx) = mpsc::channel::<LspRequest>();
        let (response_tx, response_rx) = mpsc::channel::<LspResponse>();
        let response_rx = Arc::new(Mutex::new(response_rx));
        let current_diagnostics = Arc::new(RwLock::new(Vec::new()));
        let document_tree = Arc::new(RwLock::new(None));
        let document_info = Arc::new(RwLock::new(None));

        // Clone before moving into thread
        let workspace_root_clone = workspace_root.clone();

        // Spawn background thread for LSP communication
        let diagnostics_clone = current_diagnostics.clone();
        let document_tree_clone = document_tree.clone();
        let document_info_clone = document_info.clone();
        thread::spawn(move || {
            if let Err(e) = run_lsp_client(
                request_rx,
                response_tx,
                workspace_root_clone,
                diagnostics_clone,
                document_tree_clone,
                document_info_clone,
            ) {
                eprintln!("LSP client error: {}", e);
            }
        });

        Ok(Self {
            tx: request_tx,
            response_rx,
            current_diagnostics,
            document_info,
            workspace_root,
        })
    }

    /// Initialize LSP for a file
    pub fn initialize(&self, file_path: PathBuf, text: String) {
        let _ = self.tx.send(LspRequest::Initialize { file_path, text });
    }

    /// Notify LSP of document changes with incremental updates
    pub fn document_changed(&self, changes: Vec<TextChange>) {
        let _ = self.tx.send(LspRequest::CancelPendingRequests);
        let version = self
            .document_info
            .read()
            .ok()
            .and_then(|d| d.as_ref().map(|d| d.version + 1))
            .unwrap_or(1);
        let _ = self
            .tx
            .send(LspRequest::DocumentChanged { changes, version });
    }

    /// Notify LSP of document changes with full text (legacy)
    pub fn document_changed_full(&self, text: String) {
        let _ = self.tx.send(LspRequest::CancelPendingRequests);
        let version = self
            .document_info
            .read()
            .ok()
            .and_then(|d| d.as_ref().map(|d| d.version + 1))
            .unwrap_or(1);
        let changes = vec![TextChange {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: u32::MAX,
                    character: u32::MAX,
                },
            },
            text,
        }];
        let _ = self
            .tx
            .send(LspRequest::DocumentChanged { changes, version });
    }

    /// Notify LSP of document save
    pub fn document_saved(&self, path: PathBuf, text: String) {
        let _ = self.tx.send(LspRequest::DocumentSaved { path, text });
    }

    /// Poll for any LSP responses (non-blocking)
    /// Returns all pending responses
    pub fn poll_responses(&self) -> Vec<LspResponse> {
        self.response_rx
            .lock()
            .ok()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    /// Request hover information at position
    pub fn request_hover(&self, line: u32, character: u32) {
        let _ = self.tx.send(LspRequest::Hover { line, character });
    }

    /// Request document symbols
    pub fn request_document_symbols(&self) {
        let _ = self.tx.send(LspRequest::DocumentSymbols);
    }

    /// Request go-to-definition at position
    pub fn request_goto_definition(&self, line: u32, character: u32) {
        let _ = self.tx.send(LspRequest::GotoDefinition { line, character });
    }

    /// Request code actions at position
    pub fn request_code_action(&self, line: u32, character: u32) {
        let diagnostics = self
            .current_diagnostics
            .read()
            .ok()
            .map(|diags| {
                diags
                    .iter()
                    .filter(|d| d.range.start.line <= line && line <= d.range.end.line)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let _ = self.tx.send(LspRequest::CodeAction {
            line,
            character,
            diagnostics,
        });
    }

    /// Execute a code action (either command or workspace edit)
    pub fn execute_code_action(&self, action: &lsp_types::CodeActionOrCommand) {
        match action {
            lsp_types::CodeActionOrCommand::Command(cmd) => {
                let _ = self.tx.send(LspRequest::ExecuteCommand {
                    command: cmd.command.clone(),
                    arguments: cmd.arguments.clone().unwrap_or_default(),
                });
            }
            lsp_types::CodeActionOrCommand::CodeAction(action) => {
                if let Some(ref edit) = action.edit {
                    let _ = self
                        .tx
                        .send(LspRequest::ApplyWorkspaceEdit { edit: edit.clone() });
                }
                if let Some(ref cmd) = action.command {
                    let _ = self.tx.send(LspRequest::ExecuteCommand {
                        command: cmd.command.clone(),
                        arguments: cmd.arguments.clone().unwrap_or_default(),
                    });
                }
            }
        }
    }

    /// Shutdown the LSP server
    pub fn shutdown(&self) {
        let _ = self.tx.send(LspRequest::Shutdown);
    }

    /// Get or create the global LSP manager instance
    ///
    /// This method uses a global singleton to ensure only one LSP instance exists
    /// per workspace. While convenient, this makes testing difficult.
    ///
    /// For testing or when you need more control, use `LspManager::new_for_rust()`
    /// instead and manage the instance lifetime yourself.
    pub fn get_or_create_global(
        workspace_root: Option<PathBuf>,
    ) -> Result<Arc<LspManager>, std::io::Error> {
        let instance_lock = LSP_INSTANCE.get_or_init(|| Arc::new(Mutex::new(None)));

        let mut instance_guard = instance_lock.lock().unwrap();

        if let Some(ref existing) = *instance_guard {
            // Check if we can reuse the existing instance (same workspace)
            if existing.workspace_root == workspace_root {
                eprintln!(
                    "LSP: Reusing existing instance for workspace: {:?}",
                    workspace_root
                );
                return Ok(existing.clone());
            }
            // Different workspace, shutdown the old one
            eprintln!("LSP: Shutting down old instance, workspace changed");
            existing.shutdown();
        }

        // Create new instance
        eprintln!(
            "LSP: Creating new LSP instance for workspace: {:?}",
            workspace_root
        );
        let new_manager = Arc::new(Self::new_for_rust(workspace_root)?);
        *instance_guard = Some(new_manager.clone());

        Ok(new_manager)
    }

    /// Pre-warm the LSP for faster startup
    pub fn prewarm_for_workspace(workspace_root: Option<PathBuf>) {
        eprintln!("LSP: Starting pre-warm for workspace: {:?}", workspace_root);
        std::thread::spawn(
            move || match Self::get_or_create_global(workspace_root.clone()) {
                Ok(_manager) => {
                    eprintln!(
                        "LSP: Successfully pre-warmed rust-analyzer for {:?}",
                        workspace_root
                    );
                }
                Err(e) => {
                    eprintln!("LSP: Failed to pre-warm: {}", e);
                }
            },
        );
    }

    /// Load cached diagnostics if available and valid
    pub fn load_cached_diagnostics(
        file_path: &PathBuf,
        content: &str,
    ) -> Option<Vec<ParsedDiagnostic>> {
        let cache_key = Self::compute_cache_key(file_path, content)?;
        let cache_file = Self::get_cache_path(&cache_key);

        if !cache_file.exists() {
            return None;
        }

        let (mod_time, cached) = std::fs::metadata(file_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mod_time| {
                std::fs::read_to_string(&cache_file)
                    .ok()
                    .and_then(|content| serde_json::from_str::<CachedDiagnostics>(&content).ok())
                    .map(|cached| (mod_time, cached))
            })?;

        // Cache is valid if file hasn't been modified and content hash matches
        if cached.modification_time <= mod_time
            && cached.content_hash == Self::hash_content(content)
        {
            eprintln!(
                "LSP: Loaded {} cached diagnostics for {:?}",
                cached.diagnostics.len(),
                file_path
            );
            return Some(cached.diagnostics);
        }
        None
    }

    /// Save diagnostics to cache
    pub fn cache_diagnostics(file_path: &PathBuf, content: &str, diagnostics: &[ParsedDiagnostic]) {
        let _ = Self::cache_diagnostics_impl(file_path, content, diagnostics);
    }

    fn cache_diagnostics_impl(
        file_path: &PathBuf,
        content: &str,
        diagnostics: &[ParsedDiagnostic],
    ) -> Option<()> {
        let cache_key = Self::compute_cache_key(file_path, content)?;
        let cache_file = Self::get_cache_path(&cache_key);

        // Create cache directory if it doesn't exist
        if let Some(parent) = cache_file.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }

        let mod_time = std::fs::metadata(file_path).ok()?.modified().ok()?;

        let cached = CachedDiagnostics {
            diagnostics: diagnostics.to_vec(),
            file_path: file_path.clone(),
            content_hash: Self::hash_content(content),
            modification_time: mod_time,
            cached_at: SystemTime::now(),
        };

        let json = serde_json::to_string_pretty(&cached).ok()?;
        std::fs::write(&cache_file, json).ok()
    }

    /// Compute cache key from file path and content
    fn compute_cache_key(file_path: &PathBuf, content: &str) -> Option<String> {
        let mut hasher = AHasher::default();
        file_path.hash(&mut hasher);
        content.hash(&mut hasher);
        let hash = hasher.finish();
        Some(format!("{:x}", hash))
    }

    /// Hash content for cache validation
    fn hash_content(content: &str) -> u64 {
        let mut hasher = AHasher::default();
        content.hash(&mut hasher);
        hasher.finish()
    }

    /// Get cache file path
    fn get_cache_path(cache_key: &str) -> PathBuf {
        let cache_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".cache")
            .join("diagnostics");
        cache_dir.join(format!("{}.json", cache_key))
    }

    /// Get cache file path for definitions
    fn get_definitions_cache_path(cache_key: &str) -> PathBuf {
        let cache_dir = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".cache")
            .join("definitions");
        cache_dir.join(format!("{}.json", cache_key))
    }

    /// Load cached definitions if available and valid
    pub fn load_cached_definitions(
        file_path: &PathBuf,
        content: &str,
    ) -> Option<ahash::AHashMap<(usize, usize), Vec<CachedLocation>>> {
        let cache_key = Self::compute_cache_key(file_path, content)?;
        let cache_file = Self::get_definitions_cache_path(&cache_key);

        if !cache_file.exists() {
            return None;
        }

        let (mod_time, cached) = std::fs::metadata(file_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mod_time| {
                std::fs::read_to_string(&cache_file)
                    .ok()
                    .and_then(|content| serde_json::from_str::<CachedDefinitions>(&content).ok())
                    .map(|cached| (mod_time, cached))
            })?;

        if cached.modification_time <= mod_time
            && cached.content_hash == Self::hash_content(content)
        {
            return Some(cached.definitions);
        }
        None
    }

    /// Save definitions to cache
    pub fn cache_definitions(
        file_path: &PathBuf,
        content: &str,
        definitions: &ahash::AHashMap<(usize, usize), Vec<CachedLocation>>,
    ) {
        let _ = Self::cache_definitions_impl(file_path, content, definitions);
    }

    fn cache_definitions_impl(
        file_path: &PathBuf,
        content: &str,
        definitions: &ahash::AHashMap<(usize, usize), Vec<CachedLocation>>,
    ) -> Option<()> {
        let cache_key = Self::compute_cache_key(file_path, content)?;
        let cache_file = Self::get_definitions_cache_path(&cache_key);

        if let Some(parent) = cache_file.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }

        let mod_time = std::fs::metadata(file_path).ok()?.modified().ok()?;

        let cached = CachedDefinitions {
            definitions: definitions.clone(),
            file_path: file_path.clone(),
            content_hash: Self::hash_content(content),
            modification_time: mod_time,
            cached_at: SystemTime::now(),
        };

        let json = serde_json::to_string_pretty(&cached).ok()?;
        std::fs::write(&cache_file, json).ok()
    }
}

/// Tracks pending LSP requests to correlate responses
#[derive(Debug, Clone)]
enum PendingRequest {
    Initialize,
    Hover {
        line: u32,
        character: u32,
    },
    DocumentSymbols,
    GotoDefinition {
        line: u32,
        character: u32,
    },
    CodeAction {
        line: u32,
        character: u32,
        diagnostics: Vec<LspDiagnostic>,
    },
    ExecuteCommand,
    Shutdown,
}

/// Pending request with timestamp for timeout detection
#[derive(Debug)]
struct TrackedRequest {
    request_type: PendingRequest,
    sent_at: Instant,
    retry_count: u8,
}

/// Initialization state (using u8 for atomic access)
const INIT_NOT_STARTED: u8 = 0;
const INIT_SENT: u8 = 1;
const INIT_COMPLETE: u8 = 2;

/// Queued document to open after initialization completes
#[derive(Debug, Clone)]
struct QueuedDocument {
    uri: Uri,
    text: String,
}

/// Internal signal to main loop that initialize completed
///
/// # Initialization Flow
///
/// The LSP initialization is complex due to the handshake protocol:
///
/// 1. **Client sends `initialize` request** with capabilities
/// 2. **Server responds** with its capabilities
/// 3. **Client sends `initialized` notification** (no response expected)
/// 4. **Client can now send other requests** (didOpen, etc.)
///
/// The complexity in our implementation comes from needing to:
/// - Send initialize from main loop (has stdin access)
/// - Wait for response in response thread
/// - Signal main loop to send 'initialized' notification
/// - Open queued document after 'initialized' is sent
///
/// This requires coordination between threads using:
/// - `init_state` atomic for tracking state (NOT_STARTED -> SENT -> COMPLETE)
/// - `queued_document` mutex for the document to open after init
/// - `init_complete_tx/rx` channel to signal main loop
///
/// Alternative designs that were considered:
/// - Send 'initialized' from response thread: Can't access stdin
/// - Queue all requests until initialized: Adds latency for simple operations
/// - Block on initialize: Defeats purpose of async design
enum InternalSignal {
    InitializeCompleted,
    RetryRequest(PendingRequest),
}

/// Debouncer for document changes
struct ChangeDebouncer {
    pending_changes: Vec<LspRequest>,
    last_change_time: Instant,
    debounce_duration: Duration,
}

impl ChangeDebouncer {
    fn new(debounce_ms: u64) -> Self {
        Self {
            pending_changes: Vec::new(),
            last_change_time: Instant::now(),
            debounce_duration: Duration::from_millis(debounce_ms),
        }
    }

    /// Add a change request to the debouncer
    fn add_change(&mut self, request: LspRequest) {
        self.pending_changes.push(request);
        self.last_change_time = Instant::now();
    }

    /// Check if changes are ready to be sent (debounce period has elapsed)
    fn is_ready(&self) -> bool {
        !self.pending_changes.is_empty() && self.last_change_time.elapsed() > self.debounce_duration
    }

    /// Get the final change to send and clear pending changes
    fn take_final_change(&mut self) -> Option<LspRequest> {
        let result = self.pending_changes.last().cloned();
        self.pending_changes.clear();
        result
    }

    /// Clear all pending changes (e.g., when canceling)
    fn clear(&mut self) {
        self.pending_changes.clear();
    }

    /// Check if there are pending changes
    fn has_pending(&self) -> bool {
        !self.pending_changes.is_empty()
    }
}

// Helper functions for common operations

/// Convert UTF-8 position to UTF-16 for LSP using Tree
fn utf8_to_utf16_position(
    document_tree: &Arc<RwLock<Option<tree::Tree>>>,
    line: u32,
    character: u32,
) -> u32 {
    document_tree
        .read()
        .ok()
        .and_then(|guard| {
            guard
                .as_ref()
                .map(|tree| tree.doc_pos_to_point_utf16(line, character).column)
        })
        .unwrap_or(character)
}

/// Get current document version
fn get_document_version(document_info: &Arc<RwLock<Option<DocumentInfo>>>) -> u64 {
    document_info
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|d| d.version))
        .unwrap_or(0)
}

/// Track a pending request
fn track_request(
    pending_requests: &Arc<Mutex<HashMap<u64, TrackedRequest>>>,
    request_id: u64,
    request_type: PendingRequest,
    retry_count: u8,
) {
    if let Ok(mut pending) = pending_requests.lock() {
        pending.insert(
            request_id,
            TrackedRequest {
                request_type,
                sent_at: Instant::now(),
                retry_count,
            },
        );
    }
}

/// Helper to send a retry request (for Hover, GotoDefinition, CodeAction)
fn send_retry_request<W: std::io::Write>(
    stdin: &mut W,
    next_id: &mut u64,
    current_uri: &Uri,
    pending_requests: &Arc<Mutex<HashMap<u64, TrackedRequest>>>,
    request_type: PendingRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    match &request_type {
        PendingRequest::Hover { line, character } => {
            let request_id = send_hover(stdin, next_id, current_uri, *line, *character)?;
            track_request(pending_requests, request_id, request_type, 1);
        }
        PendingRequest::GotoDefinition { line, character } => {
            let request_id = send_goto_definition(stdin, next_id, current_uri, *line, *character)?;
            track_request(pending_requests, request_id, request_type, 1);
        }
        PendingRequest::CodeAction {
            line,
            character,
            diagnostics,
        } => {
            let request_id =
                send_code_action(stdin, next_id, current_uri, *line, *character, diagnostics)?;
            track_request(pending_requests, request_id, request_type, 1);
        }
        _ => {}
    }
    Ok(())
}

/// Run the LSP client in a background thread
///
/// This function manages the entire lifecycle of the LSP process:
/// 1. Spawns rust-analyzer
/// 2. Sets up message framing (Content-Length headers)
/// 3. Manages request/response correlation
/// 4. Handles initialization handshake
/// 5. Debounces document changes
///
/// # Crash Recovery
///
/// If the LSP process crashes, this function will return an error and the thread
/// will exit. Currently, there is no automatic restart mechanism. To implement
/// crash recovery, you would need to:
/// 1. Monitor the background thread (e.g., with a handle)
/// 2. Detect when it exits unexpectedly
/// 3. Create a new LspManager instance
/// 4. Re-initialize with the current document state
///
/// The LSP process may crash due to:
/// - Bugs in rust-analyzer
/// - OOM conditions
/// - Invalid LSP messages
/// - File system errors
fn run_lsp_client(
    request_rx: mpsc::Receiver<LspRequest>,
    response_tx: mpsc::Sender<LspResponse>,
    workspace_root: Option<PathBuf>,
    current_diagnostics: Arc<RwLock<Vec<LspDiagnostic>>>,
    document_tree: Arc<RwLock<Option<tree::Tree>>>,
    document_info: Arc<RwLock<Option<DocumentInfo>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Start rust-analyzer process
    eprintln!("LSP: Starting rust-analyzer process...");
    let mut child = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    eprintln!(
        "LSP: rust-analyzer process started with PID: {:?}",
        child.id()
    );

    let mut stdin = child.stdin.take().expect("Failed to get stdin");
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let reader = BufReader::new(stdout);

    // Message ID counter
    let mut next_id = 1u64;
    let init_state = Arc::new(AtomicU8::new(INIT_NOT_STARTED));
    let current_file = Arc::new(RwLock::new(None::<Uri>));

    // Track pending requests for response correlation
    let pending_requests = Arc::new(Mutex::new(HashMap::<u64, TrackedRequest>::new()));

    // Queue documents to open after initialization
    let queued_document = Arc::new(Mutex::new(None::<QueuedDocument>));

    // Channel for response handler to signal initialization complete
    let (init_complete_tx, init_complete_rx) = mpsc::channel::<InternalSignal>();

    // Spawn thread to handle LSP responses
    let response_tx_clone = response_tx.clone();
    let current_file_clone = current_file.clone();
    let document_info_clone = document_info.clone();
    let current_diagnostics_clone = current_diagnostics.clone();
    let pending_requests_clone = pending_requests.clone();
    let init_state_clone = init_state.clone();
    let response_handle = thread::spawn(move || {
        handle_lsp_responses(
            reader,
            response_tx_clone,
            current_file_clone,
            document_info_clone,
            current_diagnostics_clone,
            pending_requests_clone,
            init_state_clone,
            init_complete_tx,
        );
    });
    let mut debouncer = ChangeDebouncer::new(CHANGE_DEBOUNCE_MS);
    let mut last_timeout_check = std::time::Instant::now();
    let mut last_state_log = std::time::Instant::now();

    // Main request processing loop
    loop {
        // Log state periodically
        if last_state_log.elapsed() > Duration::from_secs(STATE_LOG_INTERVAL_SECS) {
            last_state_log = Instant::now();
        }

        // Check for internal signals (initialization complete, retry requests)
        match init_complete_rx.try_recv() {
            Ok(InternalSignal::InitializeCompleted) => {
                if let Some(doc) = queued_document.lock().ok().and_then(|mut q| q.take()) {
                    send_initialized(&mut stdin)?;
                    *current_file.write().unwrap() = Some(doc.uri.clone());
                    *document_tree.write().unwrap() = Some(tree::Tree::from_str(&doc.text));
                    *document_info.write().unwrap() = Some(DocumentInfo {
                        uri: doc.uri.clone(),
                        version: 1,
                    });
                    send_did_open(&mut stdin, &doc.uri, &doc.text)?;
                    send_did_save(&mut stdin, &doc.uri, Some(&doc.text))?;
                }
            }
            Ok(InternalSignal::RetryRequest(request_type)) => {
                if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                    if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                        let _ = send_retry_request(
                            &mut stdin,
                            &mut next_id,
                            &current_uri,
                            &pending_requests,
                            request_type,
                        );
                    }
                }
            }
            Err(_) => {
                // No signal received, continue
            }
        }

        // Check for debounced changes
        if debouncer.is_ready() {
            if let Some(LspRequest::DocumentChanged { changes, version }) = debouncer.take_final_change() {
                if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                    if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                        send_did_change_incremental(&mut stdin, &current_uri, &changes, version)?;

                        // Update Tree if we have a full document change
                        if changes.len() == 1
                            && changes[0].range.start.line == 0
                            && changes[0].range.start.character == 0
                            && (changes[0].range.end.line == u32::MAX
                                || changes[0].range.end.character == u32::MAX)
                        {
                            *document_tree.write().unwrap() =
                                Some(tree::Tree::from_str(&changes[0].text));
                            if let Some(info) = document_info.write().ok().as_mut().and_then(|g| g.as_mut()) {
                                info.version = version;
                            }
                        }
                    }
                }
            }
        }

        // Process requests with timeout
        match request_rx.recv_timeout(Duration::from_millis(REQUEST_POLL_TIMEOUT_MS)) {
            Ok(request) => {
                match request {
                    LspRequest::Initialize { file_path, text } => {
                        let state = init_state.load(Ordering::SeqCst);
                        if debouncer.has_pending() {
                            debouncer.clear();
                        }

                        let file_url =
                            Url::from_file_path(&file_path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();

                        match state {
                            INIT_NOT_STARTED => {
                                let root_uri = workspace_root
                                    .as_ref()
                                    .and_then(|p| Url::from_file_path(p).ok())
                                    .or_else(|| {
                                        file_path.parent().and_then(|p| Url::from_file_path(p).ok())
                                    })
                                    .map(|url| Uri::from_str(url.as_str()).unwrap());

                                let init_request_id =
                                    send_initialize(&mut stdin, &mut next_id, root_uri)?;
                                track_request(
                                    &pending_requests,
                                    init_request_id,
                                    PendingRequest::Initialize,
                                    0,
                                );
                                init_state.store(INIT_SENT, Ordering::SeqCst);
                                *queued_document.lock().unwrap() = Some(QueuedDocument {
                                    uri: file_uri,
                                    text,
                                });
                            }
                            INIT_SENT => {
                                *queued_document.lock().unwrap() = Some(QueuedDocument {
                                    uri: file_uri,
                                    text,
                                });
                            }
                            INIT_COMPLETE => {
                                *current_file.write().unwrap() = Some(file_uri.clone());
                                let new_version = document_info
                                    .read()
                                    .ok()
                                    .and_then(|d| d.as_ref().map(|d| d.version + 1))
                                    .unwrap_or(1);
                                *document_tree.write().unwrap() = Some(tree::Tree::from_str(&text));
                                *document_info.write().unwrap() = Some(DocumentInfo {
                                    uri: file_uri.clone(),
                                    version: new_version,
                                });
                                send_did_open(&mut stdin, &file_uri, &text)?;
                                send_did_save(&mut stdin, &file_uri, Some(&text))?;
                            }
                            _ => {}
                        }
                    }
                    LspRequest::DocumentChanged { changes, version } => {
                        debouncer.add_change(LspRequest::DocumentChanged { changes, version });
                    }
                    LspRequest::DocumentSaved { path, text } => {
                        let file_url =
                            Url::from_file_path(&path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        send_did_save(&mut stdin, &file_uri, Some(&text))?;
                    }
                    LspRequest::Hover { line, character } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                                let utf16_character =
                                    utf8_to_utf16_position(&document_tree, line, character);
                                let request_id = send_hover(
                                    &mut stdin,
                                    &mut next_id,
                                    &current_uri,
                                    line,
                                    utf16_character,
                                )?;
                                track_request(
                                    &pending_requests,
                                    request_id,
                                    PendingRequest::Hover {
                                        line,
                                        character: utf16_character,
                                    },
                                    0,
                                );
                            }
                        }
                    }
                    LspRequest::DocumentSymbols => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                                let request_id = send_document_symbols(
                                    &mut stdin,
                                    &mut next_id,
                                    &current_uri,
                                )?;
                                track_request(
                                    &pending_requests,
                                    request_id,
                                    PendingRequest::DocumentSymbols,
                                    0,
                                );
                            }
                        }
                    }
                    LspRequest::GotoDefinition { line, character } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                                let utf16_character =
                                    utf8_to_utf16_position(&document_tree, line, character);
                                let request_id = send_goto_definition(
                                    &mut stdin,
                                    &mut next_id,
                                    &current_uri,
                                    line,
                                    utf16_character,
                                )?;
                                track_request(
                                    &pending_requests,
                                    request_id,
                                    PendingRequest::GotoDefinition {
                                        line,
                                        character: utf16_character,
                                    },
                                    0,
                                );
                            }
                        }
                    }
                    LspRequest::CodeAction {
                        line,
                        character,
                        diagnostics,
                    } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Some(current_uri) = current_file.read().ok().and_then(|g| g.clone()) {
                                let utf16_character =
                                    utf8_to_utf16_position(&document_tree, line, character);
                                let request_id = send_code_action(
                                    &mut stdin,
                                    &mut next_id,
                                    &current_uri,
                                    line,
                                    utf16_character,
                                    &diagnostics,
                                )?;
                                track_request(
                                    &pending_requests,
                                    request_id,
                                    PendingRequest::CodeAction {
                                        line,
                                        character: utf16_character,
                                        diagnostics: diagnostics.clone(),
                                    },
                                    0,
                                );
                            }
                        }
                    }
                    LspRequest::ExecuteCommand { command, arguments } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            let request_id = send_execute_command(
                                &mut stdin,
                                &mut next_id,
                                &command,
                                &arguments,
                            )?;
                            track_request(
                                &pending_requests,
                                request_id,
                                PendingRequest::ExecuteCommand,
                                0,
                            );
                        }
                    }
                    LspRequest::CancelPendingRequests => {
                        if let Ok(mut pending) = pending_requests.lock() {
                            for (request_id, _tracked) in pending.iter() {
                                let _ = send_cancel_request(&mut stdin, *request_id);
                            }
                            pending.clear();
                        }
                    }
                    LspRequest::ApplyWorkspaceEdit { edit } => {
                        if let Some(ref changes) = edit.changes {
                            if let Ok(doc_info_guard) = document_info.read() {
                                if let Some(ref doc_info) = *doc_info_guard {
                                    if let Some(edits) = changes.get(&doc_info.uri) {
                                        let _ = response_tx.send(LspResponse::TextEdit(
                                            TextEditUpdate {
                                                uri: doc_info.uri.clone(),
                                                edits: edits.clone(),
                                            },
                                        ));
                                    }
                                }
                            }
                        }

                        // Also handle document_changes format
                        if let Some(ref doc_changes) = edit.document_changes {
                            use lsp_types::DocumentChanges;
                            match doc_changes {
                                DocumentChanges::Edits(text_doc_edits) => {
                                    for text_doc_edit in text_doc_edits {
                                        let _ = response_tx.send(LspResponse::TextEdit(
                                            TextEditUpdate {
                                                uri: text_doc_edit.text_document.uri.clone(),
                                                edits: text_doc_edit
                                                    .edits
                                                    .iter()
                                                    .map(|e| match e {
                                                        lsp_types::OneOf::Left(te) => te.clone(),
                                                        lsp_types::OneOf::Right(ate) => {
                                                            ate.text_edit.clone()
                                                        }
                                                    })
                                                    .collect(),
                                            },
                                        ));
                                    }
                                }
                                DocumentChanges::Operations(ops) => {
                                    for op in ops {
                                        if let lsp_types::DocumentChangeOperation::Edit(
                                            text_doc_edit,
                                        ) = op
                                        {
                                            let _ = response_tx.send(LspResponse::TextEdit(
                                                TextEditUpdate {
                                                    uri: text_doc_edit.text_document.uri.clone(),
                                                    edits: text_doc_edit
                                                        .edits
                                                        .iter()
                                                        .map(|e| match e {
                                                            lsp_types::OneOf::Left(te) => {
                                                                te.clone()
                                                            }
                                                            lsp_types::OneOf::Right(ate) => {
                                                                ate.text_edit.clone()
                                                            }
                                                        })
                                                        .collect(),
                                                },
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    LspRequest::Shutdown => {
                        let request_id = send_shutdown(&mut stdin, &mut next_id)?;
                        track_request(&pending_requests, request_id, PendingRequest::Shutdown, 0);
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Check for stuck requests periodically
                if last_timeout_check.elapsed()
                    > Duration::from_secs(REQUEST_TIMEOUT_CHECK_INTERVAL_SECS)
                {
                    if let Ok(pending) = pending_requests.lock() {
                        let now = Instant::now();
                        for (id, tracked) in pending.iter() {
                            let elapsed = now.duration_since(tracked.sent_at);
                            if elapsed > Duration::from_secs(REQUEST_TIMEOUT_SECS) {
                                eprintln!(
                                    "LSP WARNING: Request {} ({:?}) has been pending for {:?}",
                                    id, tracked.request_type, elapsed
                                );
                            }
                        }
                    }
                    last_timeout_check = Instant::now();
                }
                // Continue to check for debounced changes
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    // Clean up
    let _ = child.kill();
    let _ = response_handle.join();

    Ok(())
}

/// Handle responses from the LSP server
fn handle_lsp_responses(
    mut reader: BufReader<std::process::ChildStdout>,
    response_tx: mpsc::Sender<LspResponse>,
    current_file: Arc<RwLock<Option<Uri>>>,
    document_info: Arc<RwLock<Option<DocumentInfo>>>,
    current_diagnostics: Arc<RwLock<Vec<LspDiagnostic>>>,
    pending_requests: Arc<Mutex<HashMap<u64, TrackedRequest>>>,
    init_state: Arc<AtomicU8>,
    init_complete_tx: mpsc::Sender<InternalSignal>,
) {
    loop {
        match lsp_server::Message::read(&mut reader) {
            Ok(Some(msg)) => {
                handle_lsp_message(
                    msg,
                    &response_tx,
                    &current_file,
                    &document_info,
                    &current_diagnostics,
                    &pending_requests,
                    &init_state,
                    &init_complete_tx,
                );
            }
            Ok(None) => {
                eprintln!("LSP: rust-analyzer closed connection");
                return;
            }
            Err(e) => {
                eprintln!("LSP ERROR: Failed to read from rust-analyzer: {} (process may have crashed)", e);
                return;
            }
        }
    }
}

/// Handle a single LSP message
fn handle_lsp_message(
    msg: lsp_server::Message,
    response_tx: &mpsc::Sender<LspResponse>,
    current_file: &Arc<RwLock<Option<Uri>>>,
    document_info: &Arc<RwLock<Option<DocumentInfo>>>,
    current_diagnostics: &Arc<RwLock<Vec<LspDiagnostic>>>,
    pending_requests: &Arc<Mutex<HashMap<u64, TrackedRequest>>>,
    init_state: &Arc<AtomicU8>,
    init_complete_tx: &mpsc::Sender<InternalSignal>,
) {
    match msg {
        lsp_server::Message::Notification(notif) => {
            if notif.method == "textDocument/publishDiagnostics" {
                if let Ok(publish_params) =
                    serde_json::from_value::<PublishDiagnosticsParams>(notif.params)
                {
                    let should_process = current_file
                        .read()
                        .ok()
                        .and_then(|guard| guard.as_ref().map(|uri| publish_params.uri == *uri))
                        .unwrap_or(false);

                    if should_process {
                        if let Ok(mut diags) = current_diagnostics.write() {
                            *diags = publish_params.diagnostics.clone();
                        }
                        let diagnostics = parse_diagnostics(publish_params.diagnostics);
                        let current_version = get_document_version(document_info);
                        let _ = response_tx.send(LspResponse::Diagnostics(DiagnosticUpdate {
                            diagnostics,
                            version: current_version,
                        }));
                    }
                }
            }
        }
        lsp_server::Message::Response(resp) => {
            // Extract numeric ID from RequestId
            let id_str = resp.id.to_string();
            let id: u64 = match id_str.parse() {
                Ok(id) => id,
                Err(_) => return, // Skip non-numeric IDs
            };

            // Check for error responses first
            if let Some(error) = resp.error {
                let error_code = error.code;
                let error_message = &error.message;
                let should_retry = error_code == -32801;

                if let Ok(mut pending) = pending_requests.lock() {
                    if let Some(tracked) = pending.remove(&id) {
                        if should_retry && tracked.retry_count < 1 {
                            let _ = init_complete_tx
                                .send(InternalSignal::RetryRequest(tracked.request_type.clone()));
                        } else if !should_retry {
                            eprintln!(
                                "LSP ERROR: Request #{} ({:?}) failed: {} (code: {})",
                                id, tracked.request_type, error_message, error_code
                            );
                        }
                    }
                }
                return;
            }

            // Look up what kind of request this response is for
            let request_type = pending_requests
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&id).map(|tracked| tracked.request_type));

            // Process the response based on request type
            match request_type {
                Some(PendingRequest::Initialize) => {
                    if let Some(result) = resp.result {
                        if let Ok(_init_result) =
                            serde_json::from_value::<InitializeResult>(result)
                        {
                            init_state.store(INIT_COMPLETE, Ordering::SeqCst);
                            let _ = init_complete_tx.send(InternalSignal::InitializeCompleted);
                        }
                    }
                }
                Some(PendingRequest::Hover { .. }) => {
                    if let Some(result) = resp.result {
                        if let Ok(Some(hover)) =
                            serde_json::from_value::<Option<lsp_types::Hover>>(result)
                        {
                            let content = match hover.contents {
                                lsp_types::HoverContents::Scalar(marked_string) => {
                                    match marked_string {
                                        lsp_types::MarkedString::String(s) => s,
                                        lsp_types::MarkedString::LanguageString(ls) => ls.value,
                                    }
                                }
                                lsp_types::HoverContents::Array(arr) => arr
                                    .iter()
                                    .map(|ms| match ms {
                                        lsp_types::MarkedString::String(s) => s.clone(),
                                        lsp_types::MarkedString::LanguageString(ls) => {
                                            ls.value.clone()
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n\n"),
                                lsp_types::HoverContents::Markup(markup) => markup.value,
                            };
                            let _ = response_tx.send(LspResponse::Hover(HoverUpdate {
                                content,
                                line: 0,
                                character: 0,
                            }));
                        }
                    }
                }
                Some(PendingRequest::DocumentSymbols) => {
                    if let Some(result) = resp.result {
                        if let Ok(symbols_response) =
                            serde_json::from_value::<DocumentSymbolResponse>(result)
                        {
                            let symbols = match symbols_response {
                                DocumentSymbolResponse::Flat(symbols) => symbols
                                    .into_iter()
                                    .map(|s| DocumentSymbol {
                                        name: s.name,
                                        detail: None,
                                        kind: s.kind,
                                        tags: s.tags,
                                        #[allow(deprecated)]
                                        deprecated: s.deprecated,
                                        range: s.location.range,
                                        selection_range: s.location.range,
                                        children: None,
                                    })
                                    .collect(),
                                DocumentSymbolResponse::Nested(symbols) => symbols,
                            };
                            let _ = response_tx.send(LspResponse::Symbols(SymbolsUpdate { symbols }));
                        }
                    }
                }
                Some(PendingRequest::GotoDefinition { .. }) => {
                    if let Some(result) = resp.result {
                        if let Ok(goto_response) =
                            serde_json::from_value::<lsp_types::GotoDefinitionResponse>(result)
                        {
                            let locations = match goto_response {
                                lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
                                lsp_types::GotoDefinitionResponse::Array(locs) => locs,
                                lsp_types::GotoDefinitionResponse::Link(links) => links
                                    .into_iter()
                                    .map(|link| Location {
                                        uri: link.target_uri,
                                        range: link.target_selection_range,
                                    })
                                    .collect(),
                            };
                            if !locations.is_empty() {
                                let _ = response_tx.send(LspResponse::GotoDefinition(
                                    GotoDefinitionUpdate { locations },
                                ));
                            }
                        }
                    }
                }
                Some(PendingRequest::CodeAction { .. }) => {
                    if let Some(result) = resp.result {
                        if let Ok(code_actions) =
                            serde_json::from_value::<Vec<lsp_types::CodeActionOrCommand>>(result)
                        {
                            if !code_actions.is_empty() {
                                let _ = response_tx.send(LspResponse::CodeAction(CodeActionUpdate {
                                    actions: code_actions,
                                }));
                            }
                        }
                    }
                }
                Some(PendingRequest::ExecuteCommand) | Some(PendingRequest::Shutdown) => {
                    // These don't need special handling
                }
                None => {
                    // Unexpected response - no pending request found
                    eprintln!("LSP: Received response for unknown request ID: {}", id);
                }
            }
        }
        lsp_server::Message::Request(_) => {
            // We don't handle requests from the server in this implementation
        }
    }
}

/// Parse LSP diagnostics into our format
fn parse_diagnostics(lsp_diagnostics: Vec<LspDiagnostic>) -> Vec<ParsedDiagnostic> {
    lsp_diagnostics
        .into_iter()
        .flat_map(|d| {
            let severity = match d.severity {
                Some(DiagnosticSeverity::ERROR) => diagnostics_plugin::DiagnosticSeverity::Error,
                Some(DiagnosticSeverity::WARNING) => {
                    diagnostics_plugin::DiagnosticSeverity::Warning
                }
                _ => diagnostics_plugin::DiagnosticSeverity::Info,
            };

            let mut diagnostics = Vec::new();

            // Enhanced message with source and code
            let mut enhanced_message = d.message.clone();
            if let Some(source) = d.source {
                enhanced_message = format!("[{}] {}", source, enhanced_message);
            }
            if let Some(code) = d.code {
                match code {
                    lsp_types::NumberOrString::Number(n) => {
                        enhanced_message = format!("{} ({})", enhanced_message, n);
                    }
                    lsp_types::NumberOrString::String(s) => {
                        enhanced_message = format!("{} ({})", enhanced_message, s);
                    }
                }
            }

            // Add related information to the message if available
            if let Some(related_info) = d.related_information {
                if !related_info.is_empty() {
                    enhanced_message.push_str("\n\nRelated:");
                    for info in related_info {
                        enhanced_message.push_str(&format!(
                            "\n• {} (line {})",
                            info.message,
                            info.location.range.start.line + 1
                        ));
                    }
                }
            }

            // Main diagnostic
            diagnostics.push(ParsedDiagnostic {
                line: d.range.start.line as usize,
                column_start: d.range.start.character as usize,
                column_end: d.range.end.character as usize,
                message: enhanced_message,
                severity,
            });

            diagnostics
        })
        .collect()
}

// === JSON-RPC Message Senders ===

#[allow(deprecated)]
fn send_initialize<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    root_uri: Option<Uri>,
) -> Result<u64, Box<dyn std::error::Error>> {
    // Fast initialization with minimal capabilities
    let mut capabilities = ClientCapabilities::default();

    // Only enable what we need for diagnostics
    capabilities.text_document = Some(lsp_types::TextDocumentClientCapabilities {
        diagnostic: Some(lsp_types::DiagnosticClientCapabilities {
            dynamic_registration: Some(false),
            related_document_support: Some(false),
        }),
        publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
            related_information: Some(true),
            version_support: Some(false),
            tag_support: None,
            data_support: Some(false),
            code_description_support: Some(false),
        }),
        synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
            dynamic_registration: Some(false),
            will_save: Some(false),
            will_save_wait_until: Some(false),
            did_save: Some(false),
        }),
        ..Default::default()
    });

    // Disable expensive features for faster startup
    let init_options = serde_json::json!({
        "diagnostics": {
            "enable": true,
            "disabled": [],
            "enableExperimental": false
        },
        "checkOnSave": {
            "enable": true,
            "command": "check"
        },
        "completion": {
            "enable": false  // We don't need completion yet
        },
        "hover": {
            "enable": true  // Needed for hover info and code actions
        },
        "inlayHints": {
            "enable": false
        },
        "lens": {
            "enable": false
        }
    });

    let params = InitializeParams {
        process_id: Some(std::process::id()),
        initialization_options: Some(init_options),
        capabilities,
        trace: Some(lsp_types::TraceValue::Off),
        workspace_folders: root_uri.as_ref().map(|uri| {
            let path_str = uri.to_string();
            let name = path_str
                .rsplit('/')
                .next()
                .unwrap_or("workspace")
                .to_string();
            vec![lsp_types::WorkspaceFolder {
                uri: uri.clone(),
                name,
            }]
        }),
        client_info: Some(lsp_types::ClientInfo {
            name: "tiny-editor".to_string(),
            version: Some("0.1.0".to_string()),
        }),
        locale: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
        ..Default::default()
    };

    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "initialize".to_string(),
        params: serde_json::to_value(params)?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_initialized<W: std::io::Write>(writer: &mut W) -> Result<(), Box<dyn std::error::Error>> {
    let msg = lsp_server::Message::Notification(lsp_server::Notification {
        method: "initialized".to_string(),
        params: serde_json::to_value(InitializedParams {})?,
    });
    msg.write(writer)?;
    Ok(())
}

fn send_did_open<W: std::io::Write>(
    writer: &mut W,
    uri: &Uri,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = lsp_server::Message::Notification(lsp_server::Notification {
        method: "textDocument/didOpen".to_string(),
        params: serde_json::to_value(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "rust".to_string(),
                version: 1,
                text: text.to_string(),
            },
        })?,
    });
    msg.write(writer)?;
    Ok(())
}

fn send_did_change_incremental<W: std::io::Write>(
    writer: &mut W,
    uri: &Uri,
    changes: &[TextChange],
    version: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let content_changes: Vec<TextDocumentContentChangeEvent> = changes
        .iter()
        .map(|change| {
            let is_full_sync = change.range.start.line == 0
                && change.range.start.character == 0
                && (change.range.end.line == u32::MAX || change.range.end.character == u32::MAX);

            TextDocumentContentChangeEvent {
                range: if is_full_sync {
                    None
                } else {
                    Some(change.range.clone())
                },
                range_length: None,
                text: change.text.clone(),
            }
        })
        .collect();

    let msg = lsp_server::Message::Notification(lsp_server::Notification {
        method: "textDocument/didChange".to_string(),
        params: serde_json::to_value(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: version as i32,
            },
            content_changes,
        })?,
    });
    msg.write(writer)?;
    Ok(())
}

fn send_did_save<W: std::io::Write>(
    writer: &mut W,
    uri: &Uri,
    text: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = lsp_server::Message::Notification(lsp_server::Notification {
        method: "textDocument/didSave".to_string(),
        params: serde_json::to_value(DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            text: text.map(|s| s.to_string()),
        })?,
    });
    msg.write(writer)?;
    Ok(())
}

fn send_hover<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "textDocument/hover".to_string(),
        params: serde_json::to_value(lsp_types::HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        })?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_document_symbols<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    uri: &Uri,
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "textDocument/documentSymbol".to_string(),
        params: serde_json::to_value(DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
        })?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_goto_definition<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "textDocument/definition".to_string(),
        params: serde_json::to_value(lsp_types::GotoDefinitionParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
        })?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_code_action<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
    diagnostics: &[LspDiagnostic],
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "textDocument/codeAction".to_string(),
        params: serde_json::to_value(lsp_types::CodeActionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            range: lsp_types::Range {
                start: lsp_types::Position { line, character },
                end: lsp_types::Position { line, character },
            },
            context: lsp_types::CodeActionContext {
                diagnostics: diagnostics.to_vec(),
                only: None,
                trigger_kind: Some(lsp_types::CodeActionTriggerKind::INVOKED),
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
        })?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_execute_command<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
    command: &str,
    arguments: &[serde_json::Value],
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "workspace/executeCommand".to_string(),
        params: serde_json::to_value(lsp_types::ExecuteCommandParams {
            command: command.to_string(),
            arguments: arguments.to_vec(),
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        })?,
    });

    msg.write(writer)?;
    Ok(request_id)
}

fn send_shutdown<W: std::io::Write>(
    writer: &mut W,
    next_id: &mut u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let request_id = *next_id;
    *next_id += 1;

    let msg = lsp_server::Message::Request(lsp_server::Request {
        id: lsp_server::RequestId::from(request_id as i32),
        method: "shutdown".to_string(),
        params: serde_json::Value::Null,
    });

    msg.write(writer)?;
    Ok(request_id)
}

/// Send cancellation notification for a request
fn send_cancel_request<W: std::io::Write>(
    writer: &mut W,
    request_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct CancelParams {
        id: u64,
    }

    let msg = lsp_server::Message::Notification(lsp_server::Notification {
        method: "$/cancelRequest".to_string(),
        params: serde_json::to_value(CancelParams { id: request_id })?,
    });

    msg.write(writer)?;
    Ok(())
}
