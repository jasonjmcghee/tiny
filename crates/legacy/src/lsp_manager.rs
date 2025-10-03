//! LSP (Language Server Protocol) manager for real-time diagnostics
//!
//! Currently supports rust-analyzer, designed to be extensible to other language servers

use ahash::AHasher;
use tiny_tree as tree;
use lsp_types::{
    notification::{Notification, PublishDiagnostics},
    request::{DocumentSymbolRequest, Initialize, Request, Shutdown},
    ClientCapabilities, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, HoverParams, InitializeParams,
    InitializeResult, InitializedParams, MessageType, NumberOrString, Position,
    PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams, WorkspaceFolder,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use url::Url;

// Configuration constants
const CHANGE_DEBOUNCE_MS: u64 = 200;
const REQUEST_POLL_TIMEOUT_MS: u64 = 50;
const REQUEST_TIMEOUT_CHECK_INTERVAL_SECS: u64 = 5;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const REQUEST_SLOW_WARNING_MS: u64 = 500;
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

/// JSON-RPC message structure
#[derive(Debug, Serialize, Deserialize)]
struct JsonRpcMessage {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<serde_json::Value>,
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
    /// Current document tree for UTF-16 conversions
    document_tree: Arc<RwLock<Option<tree::Tree>>>,
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
            document_tree,
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
        let version = self.document_info.read()
            .ok()
            .and_then(|d| d.as_ref().map(|d| d.version + 1))
            .unwrap_or(1);
        let _ = self.tx.send(LspRequest::DocumentChanged { changes, version });
    }

    /// Notify LSP of document changes with full text (legacy)
    pub fn document_changed_full(&self, text: String) {
        let _ = self.tx.send(LspRequest::CancelPendingRequests);
        let version = self.document_info.read()
            .ok()
            .and_then(|d| d.as_ref().map(|d| d.version + 1))
            .unwrap_or(1);
        let changes = vec![TextChange {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: u32::MAX, character: u32::MAX },
            },
            text,
        }];
        let _ = self.tx.send(LspRequest::DocumentChanged { changes, version });
    }

    /// Notify LSP of document save
    pub fn document_saved(&self, path: PathBuf, text: String) {
        let _ = self.tx.send(LspRequest::DocumentSaved { path, text });
    }

    /// Poll for any LSP responses (non-blocking)
    /// Returns all pending responses
    pub fn poll_responses(&self) -> Vec<LspResponse> {
        self.response_rx.lock()
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
        let diagnostics = self.current_diagnostics.read()
            .ok()
            .map(|diags| diags.iter()
                .filter(|d| d.range.start.line <= line && line <= d.range.end.line)
                .cloned()
                .collect())
            .unwrap_or_default();
        let _ = self.tx.send(LspRequest::CodeAction { line, character, diagnostics });
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
                    let _ = self.tx.send(LspRequest::ApplyWorkspaceEdit { edit: edit.clone() });
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

        // Check if file was modified since cache
        if let Ok(metadata) = std::fs::metadata(file_path) {
            if let Ok(mod_time) = metadata.modified() {
                if let Ok(cache_content) = std::fs::read_to_string(&cache_file) {
                    if let Ok(cached) = serde_json::from_str::<CachedDiagnostics>(&cache_content) {
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
                    }
                }
            }
        }
        None
    }

    /// Save diagnostics to cache
    pub fn cache_diagnostics(file_path: &PathBuf, content: &str, diagnostics: &[ParsedDiagnostic]) {
        if let Some(cache_key) = Self::compute_cache_key(file_path, content) {
            let cache_file = Self::get_cache_path(&cache_key);

            // Create cache directory if it doesn't exist
            if let Some(parent) = cache_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            if let Ok(metadata) = std::fs::metadata(file_path) {
                if let Ok(mod_time) = metadata.modified() {
                    let cached = CachedDiagnostics {
                        diagnostics: diagnostics.to_vec(),
                        file_path: file_path.clone(),
                        content_hash: Self::hash_content(content),
                        modification_time: mod_time,
                        cached_at: SystemTime::now(),
                    };

                    if let Ok(json) = serde_json::to_string_pretty(&cached) {
                        let _ = std::fs::write(&cache_file, json);
                    }
                }
            }
        }
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

/// Stored server capabilities for capability checking
#[derive(Debug, Clone, Default)]
struct StoredCapabilities {
    hover_provider: bool,
    definition_provider: bool,
    document_symbol_provider: bool,
    code_action_provider: bool,
    execute_command_provider: bool,
}

impl StoredCapabilities {
    fn from_server_capabilities(caps: &ServerCapabilities) -> Self {
        Self {
            hover_provider: caps.hover_provider.is_some(),
            definition_provider: caps.definition_provider.is_some(),
            document_symbol_provider: caps.document_symbol_provider.is_some(),
            code_action_provider: caps.code_action_provider.is_some(),
            execute_command_provider: caps.execute_command_provider.is_some(),
        }
    }
}

// Helper functions for common operations

/// Convert UTF-8 position to UTF-16 for LSP using Tree
fn utf8_to_utf16_position(
    document_tree: &Arc<RwLock<Option<tree::Tree>>>,
    line: u32,
    character: u32,
) -> u32 {
    document_tree.read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|tree| tree.doc_pos_to_point_utf16(line, character).column))
        .unwrap_or(character)
}

/// Get current document version
fn get_document_version(document_info: &Arc<RwLock<Option<DocumentInfo>>>) -> u64 {
    document_info.read()
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
fn send_retry_request(
    stdin: &mut dyn Write,
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
        PendingRequest::CodeAction { line, character, diagnostics } => {
            let request_id = send_code_action(stdin, next_id, current_uri, *line, *character, diagnostics)?;
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

    // Store server capabilities
    let server_capabilities = Arc::new(RwLock::new(None::<StoredCapabilities>));

    // Channel for response handler to signal initialization complete
    let (init_complete_tx, init_complete_rx) = mpsc::channel::<InternalSignal>();

    // Spawn thread to handle LSP responses
    let response_tx_clone = response_tx.clone();
    let current_file_clone = current_file.clone();
    let document_info_clone = document_info.clone();
    let current_diagnostics_clone = current_diagnostics.clone();
    let pending_requests_clone = pending_requests.clone();
    let init_state_clone = init_state.clone();
    let queued_document_clone = queued_document.clone();
    let server_capabilities_clone = server_capabilities.clone();
    let response_handle = thread::spawn(move || {
        handle_lsp_responses(
            reader,
            response_tx_clone,
            current_file_clone,
            document_info_clone,
            current_diagnostics_clone,
            pending_requests_clone,
            init_state_clone,
            server_capabilities_clone,
            init_complete_tx,
        );
    });
    let mut last_text = String::new();
    let mut debouncer = ChangeDebouncer::new(CHANGE_DEBOUNCE_MS);
    let mut last_timeout_check = std::time::Instant::now();
    let mut last_state_log = std::time::Instant::now();

    // Main request processing loop
    loop {
        // Log state periodically
        if last_state_log.elapsed() > Duration::from_secs(STATE_LOG_INTERVAL_SECS) {
            let state = init_state.load(Ordering::SeqCst);
            let has_queued = queued_document.lock().unwrap().is_some();
            let state_name = match state {
                INIT_NOT_STARTED => "NotStarted",
                INIT_SENT => "InitializeSent",
                INIT_COMPLETE => "Initialized",
                _ => "Unknown",
            };
            last_state_log = Instant::now();
        }

        // Check for internal signals (initialization complete, retry requests)
        match init_complete_rx.try_recv() {
            Ok(InternalSignal::InitializeCompleted) => {
                if let Ok(mut queued) = queued_document.lock() {
                    if let Some(doc) = queued.take() {
                        send_initialized(&mut stdin)?;
                        *current_file.write().unwrap() = Some(doc.uri.clone());
                        last_text = doc.text.clone();
                        *document_tree.write().unwrap() = Some(tree::Tree::from_str(&doc.text));
                        *document_info.write().unwrap() = Some(DocumentInfo {
                            uri: doc.uri.clone(),
                            version: 1,
                        });
                        send_did_open(&mut stdin, &doc.uri, &doc.text)?;
                        send_did_save(&mut stdin, &doc.uri, Some(&doc.text))?;
                    }
                }
            }
            Ok(InternalSignal::RetryRequest(request_type)) => {
                if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                    if let Ok(current_uri_guard) = current_file.read() {
                        if let Some(ref current_uri) = *current_uri_guard {
                            let _ = send_retry_request(&mut stdin, &mut next_id, current_uri, &pending_requests, request_type);
                        }
                    }
                }
            }
            Err(_) => {
                // No signal received, continue
            }
        }

        // Check for debounced changes
        if debouncer.is_ready() {
            if let Some(final_request) = debouncer.take_final_change() {
                if let LspRequest::DocumentChanged { changes, version } = final_request {
                    if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                        if let Ok(current_uri_guard) = current_file.read() {
                            if let Some(ref current_uri) = *current_uri_guard {
                                send_did_change_incremental(&mut stdin, current_uri, &changes, version)?;

                                // Update Tree if we have a full document change
                                if changes.len() == 1
                                    && changes[0].range.start.line == 0
                                    && changes[0].range.start.character == 0
                                    && (changes[0].range.end.line == u32::MAX
                                        || changes[0].range.end.character == u32::MAX)
                                {
                                    last_text = changes[0].text.clone();
                                    *document_tree.write().unwrap() = Some(tree::Tree::from_str(&changes[0].text));
                                    if let Ok(mut info_guard) = document_info.write() {
                                        if let Some(ref mut info) = *info_guard {
                                            info.version = version;
                                        }
                                    }
                                }
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

                        let file_url = Url::from_file_path(&file_path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();

                        match state {
                            INIT_NOT_STARTED => {
                                let root_uri = workspace_root
                                    .as_ref()
                                    .and_then(|p| Url::from_file_path(p).ok())
                                    .or_else(|| file_path.parent().and_then(|p| Url::from_file_path(p).ok()))
                                    .map(|url| Uri::from_str(url.as_str()).unwrap());

                                let init_request_id = send_initialize(&mut stdin, &mut next_id, root_uri)?;
                                track_request(&pending_requests, init_request_id, PendingRequest::Initialize, 0);
                                init_state.store(INIT_SENT, Ordering::SeqCst);
                                *queued_document.lock().unwrap() = Some(QueuedDocument { uri: file_uri, text });
                            }
                            INIT_SENT => {
                                *queued_document.lock().unwrap() = Some(QueuedDocument { uri: file_uri, text });
                            }
                            INIT_COMPLETE => {
                                *current_file.write().unwrap() = Some(file_uri.clone());
                                last_text = text.clone();
                                let new_version = document_info.read()
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
                        let file_url = Url::from_file_path(&path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        send_did_save(&mut stdin, &file_uri, Some(&text))?;
                    }
                    LspRequest::Hover { line, character } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    let utf16_character = utf8_to_utf16_position(&document_tree, line, character);
                                    let request_id = send_hover(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
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
                    }
                    LspRequest::DocumentSymbols => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    let request_id = send_document_symbols(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
                                    )?;
                                    track_request(&pending_requests, request_id, PendingRequest::DocumentSymbols, 0);
                                }
                            }
                        }
                    }
                    LspRequest::GotoDefinition { line, character } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    let utf16_character = utf8_to_utf16_position(&document_tree, line, character);
                                    let request_id = send_goto_definition(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
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
                    }
                    LspRequest::CodeAction {
                        line,
                        character,
                        diagnostics,
                    } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    let utf16_character = utf8_to_utf16_position(&document_tree, line, character);
                                    let request_id = send_code_action(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
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
                    }
                    LspRequest::ExecuteCommand { command, arguments } => {
                        if init_state.load(Ordering::SeqCst) == INIT_COMPLETE {
                            let request_id = send_execute_command(
                                &mut stdin,
                                &mut next_id,
                                &command,
                                &arguments,
                            )?;
                            track_request(&pending_requests, request_id, PendingRequest::ExecuteCommand, 0);
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
                                            }
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
    server_capabilities: Arc<RwLock<Option<StoredCapabilities>>>,
    init_complete_tx: mpsc::Sender<InternalSignal>,
) {
    use std::io::{BufRead, Read};

    loop {
        let mut header = String::new();

        // Read headers
        let mut content_length: Option<usize> = None;
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => {
                    eprintln!(
                        "LSP ERROR: Failed to read from rust-analyzer (process may have crashed)"
                    );
                    return;
                }
                Ok(_) => {}
            }

            if header == "\r\n" || header == "\n" {
                // End of headers
                break;
            }

            if header.starts_with("Content-Length:") {
                let len_str = header["Content-Length:".len()..].trim();
                content_length = len_str.parse::<usize>().ok();
            }
        }

        // Read content if we have a content length
        if let Some(len) = content_length {
            let mut buffer = vec![0u8; len];
            if let Err(e) = reader.read_exact(&mut buffer) {
                eprintln!("LSP ERROR: Failed to read message body from rust-analyzer: {} (process may have crashed)", e);
                return;
            }

            // Parse and handle the message
            if let Ok(content) = String::from_utf8(buffer) {
                match serde_json::from_str::<JsonRpcMessage>(&content) {
                    Ok(msg) => {
                        handle_lsp_message(
                            msg,
                            &response_tx,
                            &current_file,
                            &document_info,
                            &current_diagnostics,
                            &pending_requests,
                            &init_state,
                            &server_capabilities,
                            &init_complete_tx,
                        );
                    }
                    Err(e) => {
                        eprintln!("LSP: Failed to parse message: {}", e);
                        eprintln!("Content: {}", content.chars().take(500).collect::<String>());
                    }
                }
            }
        }
    }
}

/// Handle a single LSP message
fn handle_lsp_message(
    msg: JsonRpcMessage,
    response_tx: &mpsc::Sender<LspResponse>,
    current_file: &Arc<RwLock<Option<Uri>>>,
    document_info: &Arc<RwLock<Option<DocumentInfo>>>,
    current_diagnostics: &Arc<RwLock<Vec<LspDiagnostic>>>,
    pending_requests: &Arc<Mutex<HashMap<u64, TrackedRequest>>>,
    init_state: &Arc<AtomicU8>,
    server_capabilities: &Arc<RwLock<Option<StoredCapabilities>>>,
    init_complete_tx: &mpsc::Sender<InternalSignal>,
) {
    // Handle notifications (messages without an ID)
    if msg.id.is_none() {
        if let Some(method) = msg.method {
            if method == "textDocument/publishDiagnostics" {
                if let Some(params) = msg.params {
                    if let Ok(publish_params) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                        let should_process = current_file.read()
                            .ok()
                            .and_then(|guard| guard.as_ref().map(|uri| publish_params.uri == *uri))
                            .unwrap_or(false);

                        if should_process {
                            if let Ok(mut diags) = current_diagnostics.write() {
                                *diags = publish_params.diagnostics.clone();
                            }
                            let diagnostics = parse_diagnostics(publish_params.diagnostics);
                            let current_version = get_document_version(document_info);
                            let _ = response_tx.send(LspResponse::Diagnostics(
                                DiagnosticUpdate { diagnostics, version: current_version }
                            ));
                        }
                    }
                }
            }
        }
        return;
    }

    // Handle responses (messages with an ID)
    if let Some(id) = msg.id {
        // Check for error responses first
        if let Some(error) = msg.error {
            let error_code = error.get("code").and_then(|c| c.as_i64());
            let error_message = error.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            let should_retry = error_code == Some(-32801);

            if let Ok(mut pending) = pending_requests.lock() {
                if let Some(tracked) = pending.remove(&id) {
                    if should_retry && tracked.retry_count < 1 {
                        let _ = init_complete_tx.send(InternalSignal::RetryRequest(tracked.request_type.clone()));
                    } else if !should_retry {
                        eprintln!("LSP ERROR: Request #{} ({:?}) failed: {} (code: {:?})",
                            id, tracked.request_type, error_message, error_code);
                    }
                }
            }
            return;
        }

        // Look up what kind of request this response is for
        let request_type = if let Ok(mut pending) = pending_requests.lock() {
            pending.remove(&id).map(|tracked| tracked.request_type)
        } else {
            None
        };

        // Process the response based on request type
        match request_type {
            Some(PendingRequest::Initialize) => {
                if let Some(result) = msg.result {
                    match serde_json::from_value::<InitializeResult>(result) {
                        Ok(init_result) => {
                            let caps = StoredCapabilities::from_server_capabilities(&init_result.capabilities);
                            if let Ok(mut stored_caps) = server_capabilities.write() {
                                *stored_caps = Some(caps);
                            }
                            init_state.store(INIT_COMPLETE, Ordering::SeqCst);
                            let _ = init_complete_tx.send(InternalSignal::InitializeCompleted);
                        }
                        Err(e) => {
                            eprintln!("LSP ERROR: Failed to parse initialize result: {}", e);
                        }
                    }
                }
            }
            Some(PendingRequest::Hover { .. }) => {
                if let Some(result) = msg.result {
                    if let Ok(hover_result) =
                        serde_json::from_value::<Option<lsp_types::Hover>>(result)
                    {
                        if let Some(hover) = hover_result {
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
            }
            Some(PendingRequest::DocumentSymbols) => {
                if let Some(result) = msg.result {
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
                if let Some(result) = msg.result {
                    if let Ok(goto_response) = serde_json::from_value::<lsp_types::GotoDefinitionResponse>(result) {
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
                            let _ = response_tx.send(LspResponse::GotoDefinition(GotoDefinitionUpdate { locations }));
                        }
                    }
                }
            }
            Some(PendingRequest::CodeAction { .. }) => {
                if let Some(result) = msg.result {
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

fn send_message(
    writer: &mut dyn Write,
    msg: &JsonRpcMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string(msg)?;
    let content_length = json.len();
    write!(writer, "Content-Length: {}\r\n\r\n{}", content_length, json)?;
    writer.flush()?;
    Ok(())
}

macro_rules! send_request {
    ($writer:expr, $next_id:expr, $method:expr, $params:expr) => {{
        let request_id = *$next_id;
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".to_string(),
            id: Some(request_id),
            method: Some($method.to_string()),
            params: Some(serde_json::to_value($params)?),
            result: None,
            error: None,
        };
        *$next_id += 1;
        send_message($writer, &msg)?;
        Ok(request_id)
    }};
}

macro_rules! send_notification {
    ($writer:expr, $method:expr, $params:expr) => {{
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some($method.to_string()),
            params: Some(serde_json::to_value($params)?),
            result: None,
            error: None,
        };
        send_message($writer, &msg)
    }};
}

#[allow(deprecated)]
fn send_initialize(
    writer: &mut dyn Write,
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
    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(request_id),
        method: Some("initialize".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };

    *next_id += 1;
    send_message(writer, &msg)?;
    Ok(request_id)
}

fn send_initialized(writer: &mut dyn Write) -> Result<(), Box<dyn std::error::Error>> {
    send_notification!(writer, "initialized", InitializedParams {})
}

fn send_did_open(
    writer: &mut dyn Write,
    uri: &Uri,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    send_notification!(writer, "textDocument/didOpen", DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "rust".to_string(),
            version: 1,
            text: text.to_string(),
        },
    })
}

fn send_did_change_incremental(
    writer: &mut dyn Write,
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
                range: if is_full_sync { None } else { Some(change.range.clone()) },
                range_length: None,
                text: change.text.clone(),
            }
        })
        .collect();

    send_notification!(writer, "textDocument/didChange", DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: version as i32,
        },
        content_changes,
    })
}

fn send_did_save(
    writer: &mut dyn Write,
    uri: &Uri,
    text: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    send_notification!(writer, "textDocument/didSave", DidSaveTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        text: text.map(|s| s.to_string()),
    })
}

fn send_hover(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "textDocument/hover", lsp_types::HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: lsp_types::Position { line, character },
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
    })
}

fn send_document_symbols(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "textDocument/documentSymbol", DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: lsp_types::PartialResultParams::default(),
    })
}

fn send_goto_definition(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "textDocument/definition", lsp_types::GotoDefinitionParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: lsp_types::Position { line, character },
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        partial_result_params: lsp_types::PartialResultParams::default(),
    })
}

fn send_code_action(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
    diagnostics: &[LspDiagnostic],
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "textDocument/codeAction", lsp_types::CodeActionParams {
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
    })
}

fn send_execute_command(
    writer: &mut dyn Write,
    next_id: &mut u64,
    command: &str,
    arguments: &[serde_json::Value],
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "workspace/executeCommand", lsp_types::ExecuteCommandParams {
        command: command.to_string(),
        arguments: arguments.to_vec(),
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
    })
}

fn send_shutdown(
    writer: &mut dyn Write,
    next_id: &mut u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    send_request!(writer, next_id, "shutdown", serde_json::Value::Null)
}

/// Send cancellation notification for a request
fn send_cancel_request(
    writer: &mut dyn Write,
    request_id: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct CancelParams {
        id: u64,
    }
    send_notification!(writer, "$/cancelRequest", CancelParams { id: request_id })
}
