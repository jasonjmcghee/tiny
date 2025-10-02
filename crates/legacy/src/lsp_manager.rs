//! LSP (Language Server Protocol) manager for real-time diagnostics
//!
//! Currently supports rust-analyzer, designed to be extensible to other language servers

use ahash::AHasher;
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
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};
use url::Url;

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

use lsp_types::Location;

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

/// Main LSP manager
pub struct LspManager {
    tx: mpsc::Sender<LspRequest>,
    diagnostics_rx: Arc<Mutex<mpsc::Receiver<DiagnosticUpdate>>>,
    hover_rx: Arc<Mutex<mpsc::Receiver<HoverUpdate>>>,
    symbols_rx: Arc<Mutex<mpsc::Receiver<SymbolsUpdate>>>,
    goto_definition_rx: Arc<Mutex<mpsc::Receiver<GotoDefinitionUpdate>>>,
    current_version: Arc<AtomicU64>,
    workspace_root: Option<PathBuf>,
}

/// Global LSP manager instance (initialized once)
static LSP_INSTANCE: std::sync::OnceLock<Arc<Mutex<Option<Arc<LspManager>>>>> =
    std::sync::OnceLock::new();

impl LspManager {
    /// Create a new LSP manager for Rust files
    pub fn new_for_rust(workspace_root: Option<PathBuf>) -> Result<Self, std::io::Error> {
        let (request_tx, request_rx) = mpsc::channel::<LspRequest>();
        let (diagnostics_tx, diagnostics_rx) = mpsc::channel::<DiagnosticUpdate>();
        let (hover_tx, hover_rx) = mpsc::channel::<HoverUpdate>();
        let (symbols_tx, symbols_rx) = mpsc::channel::<SymbolsUpdate>();
        let (goto_definition_tx, goto_definition_rx) = mpsc::channel::<GotoDefinitionUpdate>();
        let diagnostics_rx = Arc::new(Mutex::new(diagnostics_rx));
        let hover_rx = Arc::new(Mutex::new(hover_rx));
        let symbols_rx = Arc::new(Mutex::new(symbols_rx));
        let goto_definition_rx = Arc::new(Mutex::new(goto_definition_rx));
        let current_version = Arc::new(AtomicU64::new(0));

        // Clone before moving into thread
        let workspace_root_clone = workspace_root.clone();

        // Spawn background thread for LSP communication
        let version_clone = current_version.clone();
        thread::spawn(move || {
            if let Err(e) = run_lsp_client(
                request_rx,
                diagnostics_tx,
                hover_tx,
                symbols_tx,
                goto_definition_tx,
                workspace_root_clone,
                version_clone,
            ) {
                eprintln!("LSP client error: {}", e);
            }
        });

        Ok(Self {
            tx: request_tx,
            diagnostics_rx,
            hover_rx,
            symbols_rx,
            goto_definition_rx,
            current_version,
            workspace_root,
        })
    }

    /// Initialize LSP for a file
    pub fn initialize(&self, file_path: PathBuf, text: String) {
        eprintln!("DEBUG: LspManager.initialize() called for {:?}", file_path);
        let _ = self.tx.send(LspRequest::Initialize { file_path, text });
    }

    /// Notify LSP of document changes with incremental updates
    pub fn document_changed(&self, changes: Vec<TextChange>) {
        let version = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self
            .tx
            .send(LspRequest::DocumentChanged { changes, version });
    }

    /// Notify LSP of document changes with full text (legacy)
    pub fn document_changed_full(&self, text: String) {
        let version = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
        // Convert to a single change representing the entire document
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

    /// Check for diagnostic updates (non-blocking)
    pub fn poll_diagnostics(&self) -> Option<DiagnosticUpdate> {
        if let Ok(rx) = self.diagnostics_rx.lock() {
            rx.try_recv().ok()
        } else {
            None
        }
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
        eprintln!("DEBUG: LspManager sending GotoDefinition request for line {}, char {}", line, character);
        let _ = self.tx.send(LspRequest::GotoDefinition { line, character });
    }

    /// Check for hover updates (non-blocking)
    pub fn poll_hover(&self) -> Option<HoverUpdate> {
        if let Ok(rx) = self.hover_rx.lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Check for go-to-definition updates (non-blocking)
    pub fn poll_goto_definition(&self) -> Option<GotoDefinitionUpdate> {
        if let Ok(rx) = self.goto_definition_rx.lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Check for symbols updates (non-blocking)
    pub fn poll_symbols(&self) -> Option<SymbolsUpdate> {
        if let Ok(rx) = self.symbols_rx.lock() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Shutdown the LSP server
    pub fn shutdown(&self) {
        let _ = self.tx.send(LspRequest::Shutdown);
    }

    /// Get or create the global LSP manager instance
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

/// Run the LSP client in a background thread
fn run_lsp_client(
    request_rx: mpsc::Receiver<LspRequest>,
    diagnostics_tx: mpsc::Sender<DiagnosticUpdate>,
    hover_tx: mpsc::Sender<HoverUpdate>,
    symbols_tx: mpsc::Sender<SymbolsUpdate>,
    goto_definition_tx: mpsc::Sender<GotoDefinitionUpdate>,
    workspace_root: Option<PathBuf>,
    version_counter: Arc<AtomicU64>,
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
    let mut initialized = false;
    let current_file = Arc::new(RwLock::new(None::<Uri>));

    // Spawn thread to handle LSP responses
    let diagnostics_tx_clone = diagnostics_tx.clone();
    let hover_tx_clone = hover_tx.clone();
    let symbols_tx_clone = symbols_tx.clone();
    let goto_definition_tx_clone = goto_definition_tx.clone();
    let current_file_clone = current_file.clone();
    let version_counter_clone = version_counter.clone();
    let response_handle = thread::spawn(move || {
        handle_lsp_responses(
            reader,
            diagnostics_tx_clone,
            hover_tx_clone,
            symbols_tx_clone,
            goto_definition_tx_clone,
            current_file_clone,
            version_counter_clone,
        );
    });
    let mut last_text = String::new();
    let mut pending_changes = Vec::new();
    let mut last_change_time = std::time::Instant::now();

    // Main request processing loop
    loop {
        // Check for debounced changes
        if !pending_changes.is_empty() && last_change_time.elapsed() > Duration::from_millis(200) {
            if let Some(final_request) = pending_changes.last() {
                if let LspRequest::DocumentChanged { changes, version } = final_request {
                    if initialized {
                        if let Ok(current_uri_guard) = current_file.read() {
                            if let Some(ref current_uri) = *current_uri_guard {
                                send_did_change_incremental(
                                    &mut stdin,
                                    current_uri,
                                    changes,
                                    *version,
                                )?;
                                // Update last_text if we have a full document change
                                if changes.len() == 1
                                    && changes[0].range.start.line == 0
                                    && changes[0].range.start.character == 0
                                {
                                    last_text = changes[0].text.clone();
                                }
                            }
                        }
                    }
                }
            }
            pending_changes.clear();
        }

        // Process requests with timeout
        match request_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(request) => {
                match request {
                    LspRequest::Initialize { file_path, text } => {
                        eprintln!("DEBUG: Processing Initialize request, initialized={}", initialized);
                        if !initialized {
                            // Send initialize request
                            let root_uri = workspace_root
                                .as_ref()
                                .and_then(|p| Url::from_file_path(p).ok())
                                .or_else(|| {
                                    file_path.parent().and_then(|p| Url::from_file_path(p).ok())
                                })
                                .map(|url| Uri::from_str(url.as_str()).unwrap());

                            eprintln!("DEBUG: Sending LSP initialize with root_uri: {:?}", root_uri);
                            eprintln!("DEBUG: workspace_root (from closure): {:?}", workspace_root);
                            send_initialize(&mut stdin, &mut next_id, root_uri)?;

                            // Wait a bit for initialize response
                            // TODO - revisit this
                            std::thread::sleep(Duration::from_millis(0));

                            initialized = true;
                            eprintln!("DEBUG: LSP initialized successfully");

                            // Send initialized notification
                            send_initialized(&mut stdin)?;
                        }

                        // Open the document
                        let file_url =
                            Url::from_file_path(&file_path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        *current_file.write().unwrap() = Some(file_uri.clone());
                        last_text = text.clone();
                        // Reset version counter to 1 for new document
                        version_counter.store(1, Ordering::SeqCst);
                        eprintln!("DEBUG: Sending didOpen for {:?}", file_uri);
                        send_did_open(&mut stdin, &file_uri, &text)?;
                    }
                    LspRequest::DocumentChanged { changes, version } => {
                        // Debounce changes
                        pending_changes.push(LspRequest::DocumentChanged { changes, version });
                        last_change_time = std::time::Instant::now();
                    }
                    LspRequest::DocumentSaved { path, text } => {
                        let file_url =
                            Url::from_file_path(&path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        send_did_save(&mut stdin, &file_uri, Some(&text))?;
                    }
                    LspRequest::Hover { line, character } => {
                        if initialized {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    send_hover(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
                                        line,
                                        character,
                                    )?;
                                }
                            }
                        }
                    }
                    LspRequest::DocumentSymbols => {
                        if initialized {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    send_document_symbols(&mut stdin, &mut next_id, current_uri)?;
                                }
                            }
                        }
                    }
                    LspRequest::GotoDefinition { line, character } => {
                        eprintln!("DEBUG: Processing GotoDefinition request, initialized={}", initialized);
                        if initialized {
                            if let Ok(current_uri_guard) = current_file.read() {
                                if let Some(ref current_uri) = *current_uri_guard {
                                    eprintln!("DEBUG: Sending goto_definition LSP request for {:?} at line {}, char {}", current_uri, line, character);
                                    let request_id = send_goto_definition(
                                        &mut stdin,
                                        &mut next_id,
                                        current_uri,
                                        line,
                                        character,
                                    )?;
                                    eprintln!("DEBUG: goto_definition request sent with ID: {}", request_id);
                                } else {
                                    eprintln!("DEBUG: No current URI set!");
                                }
                            }
                        } else {
                            eprintln!("DEBUG: LSP not initialized yet!");
                        }
                    }
                    LspRequest::Shutdown => {
                        send_shutdown(&mut stdin, &mut next_id)?;
                        break;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
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
    diagnostics_tx: mpsc::Sender<DiagnosticUpdate>,
    hover_tx: mpsc::Sender<HoverUpdate>,
    symbols_tx: mpsc::Sender<SymbolsUpdate>,
    goto_definition_tx: mpsc::Sender<GotoDefinitionUpdate>,
    current_file: Arc<RwLock<Option<Uri>>>,
    version_counter: Arc<AtomicU64>,
) {
    use std::io::{BufRead, Read};

    loop {
        let mut header = String::new();

        // Read headers
        let mut content_length: Option<usize> = None;
        loop {
            header.clear();
            if reader.read_line(&mut header).is_err() {
                return;
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
            if reader.read_exact(&mut buffer).is_err() {
                return;
            }

            // Parse and handle the message
            if let Ok(content) = String::from_utf8(buffer) {
                match serde_json::from_str::<JsonRpcMessage>(&content) {
                    Ok(msg) => {
                        handle_lsp_message(
                            msg,
                            &diagnostics_tx,
                            &hover_tx,
                            &symbols_tx,
                            &goto_definition_tx,
                            &current_file,
                            &version_counter,
                        );
                    }
                    Err(e) => {
                        eprintln!("LSP: Failed to parse message: {}", e);
                    }
                }
            }
        }
    }
}

/// Handle a single LSP message
fn handle_lsp_message(
    msg: JsonRpcMessage,
    diagnostics_tx: &mpsc::Sender<DiagnosticUpdate>,
    hover_tx: &mpsc::Sender<HoverUpdate>,
    symbols_tx: &mpsc::Sender<SymbolsUpdate>,
    goto_definition_tx: &mpsc::Sender<GotoDefinitionUpdate>,
    current_file: &Arc<RwLock<Option<Uri>>>,
    version_counter: &Arc<AtomicU64>,
) {
    // Debug: log all incoming messages
    if let Some(ref method) = msg.method {
        // Only log progress/indexing messages
        if method.contains("progress") || method.contains("Progress") {
            eprintln!("LSP Progress: {}", method);
            if let Some(ref params) = msg.params {
                eprintln!("  {:?}", params);
            }
        }
    }
    if let Some(id) = msg.id {
        if id <= 3 {  // Only log first few responses
            eprintln!("DEBUG: LSP message id: {}, has_result: {}, has_error: {}",
                id, msg.result.is_some(), msg.error.is_some());
        }
    }

    if let Some(method) = msg.method {
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = msg.params {
                if let Ok(publish_params) =
                    serde_json::from_value::<PublishDiagnosticsParams>(params)
                {
                    // Only process diagnostics for the currently open file
                    if let Ok(current_uri_guard) = current_file.read() {
                        if let Some(ref current_uri) = *current_uri_guard {
                            if publish_params.uri == *current_uri {
                                let diagnostics = parse_diagnostics(publish_params.diagnostics);
                                let current_version = version_counter.load(Ordering::SeqCst);
                                let update = DiagnosticUpdate {
                                    diagnostics,
                                    version: current_version,
                                };
                                let _ = diagnostics_tx.send(update);
                            }
                            // Silently ignore diagnostics for other files
                        }
                    }
                }
            }
        }
    } else if let Some(id) = msg.id {
        // Handle responses to our requests
        if let Some(ref result) = msg.result {
            // This could be a hover response
            if let Ok(hover_result) =
                serde_json::from_value::<Option<lsp_types::Hover>>(result.clone())
            {
                if let Some(hover) = hover_result {
                    let content = match hover.contents {
                        lsp_types::HoverContents::Scalar(marked_string) => match marked_string {
                            lsp_types::MarkedString::String(s) => s,
                            lsp_types::MarkedString::LanguageString(ls) => ls.value,
                        },
                        lsp_types::HoverContents::Array(arr) => arr
                            .iter()
                            .map(|ms| match ms {
                                lsp_types::MarkedString::String(s) => s.clone(),
                                lsp_types::MarkedString::LanguageString(ls) => ls.value.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        lsp_types::HoverContents::Markup(markup) => markup.value,
                    };

                    let hover_update = HoverUpdate {
                        content,
                        line: 0, // TODO: Track which request this was for
                        character: 0,
                    };
                    let _ = hover_tx.send(hover_update);
                }
            } else if msg.id.is_some() && msg.result.is_some() {
                // Check if this is a documentSymbol response
                // Since we track request IDs, we'd ideally match them
                // For now, try parsing as document symbols
                if let Some(ref result) = msg.result {
                    if let Ok(symbols_response) =
                        serde_json::from_value::<DocumentSymbolResponse>(result.clone())
                    {
                        let symbols = match symbols_response {
                            DocumentSymbolResponse::Flat(symbols) => {
                                // Convert SymbolInformation to DocumentSymbol for consistency
                                // This is a simplified conversion
                                symbols
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
                                    .collect()
                            }
                            DocumentSymbolResponse::Nested(symbols) => symbols,
                        };

                        let symbols_update = SymbolsUpdate { symbols };
                        let _ = symbols_tx.send(symbols_update);
                    }
                }
            }

            // Check if this is a goto definition response (only if it's not a hover/symbols response)
            if let Some(result) = msg.result.as_ref() {
                // Skip if it looks like document symbols (array of objects with "name" field)
                let is_symbols = if let Some(arr) = result.as_array() {
                    arr.first().and_then(|v| v.get("name")).is_some()
                } else {
                    false
                };

                // Skip if it looks like capabilities (has "capabilities" field)
                let is_capabilities = result.get("capabilities").is_some();

                if !is_symbols && !is_capabilities {
                    eprintln!("DEBUG: Trying to parse goto_definition from result (trimmed): {:?}",
                        serde_json::to_string(result).unwrap_or_default().chars().take(200).collect::<String>());
                    if let Ok(goto_response) =
                        serde_json::from_value::<lsp_types::GotoDefinitionResponse>(result.clone())
                    {
                        eprintln!("DEBUG: Successfully parsed goto_definition response: {:?}", goto_response);
                        let locations = match goto_response {
                            lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
                            lsp_types::GotoDefinitionResponse::Array(locs) => locs,
                            lsp_types::GotoDefinitionResponse::Link(links) => {
                                // Convert LocationLink to Location
                                links
                                    .into_iter()
                                    .map(|link| Location {
                                        uri: link.target_uri,
                                        range: link.target_selection_range,
                                    })
                                    .collect()
                            }
                        };

                        eprintln!("DEBUG: Got {} location(s) from goto_definition", locations.len());
                        if !locations.is_empty() {
                            for loc in &locations {
                                eprintln!("DEBUG: Location: {:?} at {:?}", loc.uri, loc.range);
                            }
                            let update = GotoDefinitionUpdate { locations };
                            let _ = goto_definition_tx.send(update);
                            eprintln!("DEBUG: Sent goto_definition update");
                        }
                    }
                }
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

#[allow(deprecated)]
fn send_initialize(
    writer: &mut dyn Write,
    next_id: &mut u64,
    root_uri: Option<Uri>,
) -> Result<(), Box<dyn std::error::Error>> {
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
            "enable": false  // We don't need hover yet
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

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(*next_id),
        method: Some("initialize".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };

    *next_id += 1;
    send_message(writer, &msg)
}

fn send_initialized(writer: &mut dyn Write) -> Result<(), Box<dyn std::error::Error>> {
    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: Some("initialized".to_string()),
        params: Some(serde_json::to_value(InitializedParams {})?),
        result: None,
        error: None,
    };
    send_message(writer, &msg)
}

fn send_did_open(
    writer: &mut dyn Write,
    uri: &Uri,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("DEBUG: send_did_open for {:?}, text length: {} bytes, first 200 chars: {:?}",
        uri, text.len(), text.chars().take(200).collect::<String>());

    let params = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "rust".to_string(),
            version: 1,
            text: text.to_string(),
        },
    };

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: Some("textDocument/didOpen".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    send_message(writer, &msg)
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
            if change.range.start.line == 0
                && change.range.start.character == 0
                && change.range.end.line == u32::MAX
                && change.range.end.character == u32::MAX
            {
                // Full document replacement
                TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: change.text.clone(),
                }
            } else {
                // Incremental change
                TextDocumentContentChangeEvent {
                    range: Some(change.range.clone()),
                    range_length: None, // Let LSP calculate this
                    text: change.text.clone(),
                }
            }
        })
        .collect();

    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: version as i32,
        },
        content_changes,
    };

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: Some("textDocument/didChange".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    send_message(writer, &msg)
}

fn send_did_save(
    writer: &mut dyn Write,
    uri: &Uri,
    text: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = DidSaveTextDocumentParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        text: text.map(|s| s.to_string()),
    };

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: Some("textDocument/didSave".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    send_message(writer, &msg)
}

fn send_hover(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = lsp_types::HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: lsp_types::Position { line, character },
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
    };

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(*next_id),
        method: Some("textDocument/hover".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    *next_id += 1;
    send_message(writer, &msg)
}

fn send_document_symbols(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = DocumentSymbolParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: WorkDoneProgressParams::default(),
        partial_result_params: lsp_types::PartialResultParams::default(),
    };

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(*next_id),
        method: Some("textDocument/documentSymbol".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    *next_id += 1;
    send_message(writer, &msg)
}

fn send_goto_definition(
    writer: &mut dyn Write,
    next_id: &mut u64,
    uri: &Uri,
    line: u32,
    character: u32,
) -> Result<u64, Box<dyn std::error::Error>> {
    // LSP uses UTF-16 code units for character positions
    // For now, we assume ASCII/simple text where UTF-8 char == UTF-16 code unit
    // TODO: Proper UTF-16 conversion for multi-byte characters

    eprintln!("DEBUG: send_goto_definition called with line={}, char={}", line, character);

    let params = lsp_types::GotoDefinitionParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: lsp_types::Position { line, character },
        },
        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
        partial_result_params: lsp_types::PartialResultParams::default(),
    };

    let request_id = *next_id;

    eprintln!("DEBUG: Sending LSP message: {}", serde_json::to_string_pretty(&params).unwrap_or_default());

    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(request_id),
        method: Some("textDocument/definition".to_string()),
        params: Some(serde_json::to_value(params)?),
        result: None,
        error: None,
    };
    *next_id += 1;
    send_message(writer, &msg)?;
    Ok(request_id)
}

fn send_shutdown(
    writer: &mut dyn Write,
    next_id: &mut u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = JsonRpcMessage {
        jsonrpc: "2.0".to_string(),
        id: Some(*next_id),
        method: Some("shutdown".to_string()),
        params: None,
        result: None,
        error: None,
    };
    *next_id += 1;
    send_message(writer, &msg)
}
