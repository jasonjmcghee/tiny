//! LSP (Language Server Protocol) manager for real-time diagnostics
//!
//! Currently supports rust-analyzer, designed to be extensible to other language servers

use lsp_types::{
    notification::{Notification, PublishDiagnostics},
    request::{Initialize, Request, Shutdown},
    ClientCapabilities, Diagnostic as LspDiagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    InitializeParams, InitializeResult, InitializedParams, MessageType, NumberOrString, Position,
    PublishDiagnosticsParams, Range, ServerCapabilities, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, TextDocumentSyncCapability, TextDocumentSyncKind,
    Uri, VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
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

/// LSP request types for the background thread
#[derive(Debug, Clone)]
pub enum LspRequest {
    Initialize { file_path: PathBuf, text: String },
    DocumentChanged { text: String, version: u64 },
    DocumentSaved { path: PathBuf, text: String },
    Shutdown,
}

/// Diagnostic update from LSP server
#[derive(Debug, Clone)]
pub struct DiagnosticUpdate {
    pub diagnostics: Vec<ParsedDiagnostic>,
    pub version: u64,
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

/// Main LSP manager
pub struct LspManager {
    tx: mpsc::Sender<LspRequest>,
    diagnostics_rx: Arc<Mutex<mpsc::Receiver<DiagnosticUpdate>>>,
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
        let diagnostics_rx = Arc::new(Mutex::new(diagnostics_rx));
        let current_version = Arc::new(AtomicU64::new(0));

        // Clone before moving into thread
        let workspace_root_clone = workspace_root.clone();

        // Spawn background thread for LSP communication
        let version_clone = current_version.clone();
        thread::spawn(move || {
            if let Err(e) = run_lsp_client(
                request_rx,
                diagnostics_tx,
                workspace_root_clone,
                version_clone,
            ) {
                eprintln!("LSP client error: {}", e);
            }
        });

        Ok(Self {
            tx: request_tx,
            diagnostics_rx,
            current_version,
            workspace_root,
        })
    }

    /// Initialize LSP for a file
    pub fn initialize(&self, file_path: PathBuf, text: String) {
        let _ = self.tx.send(LspRequest::Initialize { file_path, text });
    }

    /// Notify LSP of document changes
    pub fn document_changed(&self, text: String) {
        let version = self.current_version.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.tx.send(LspRequest::DocumentChanged { text, version });
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
        let mut hasher = DefaultHasher::new();
        file_path.hash(&mut hasher);
        content.hash(&mut hasher);
        let hash = hasher.finish();
        Some(format!("{:x}", hash))
    }

    /// Hash content for cache validation
    fn hash_content(content: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
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
    let current_file_clone = current_file.clone();
    let response_handle = thread::spawn(move || {
        handle_lsp_responses(reader, diagnostics_tx_clone, current_file_clone);
    });
    let mut last_text = String::new();
    let mut pending_changes = Vec::new();
    let mut last_change_time = std::time::Instant::now();

    // Main request processing loop
    loop {
        // Check for debounced changes
        if !pending_changes.is_empty() && last_change_time.elapsed() > Duration::from_millis(200) {
            if let Some(final_request) = pending_changes.last() {
                if let LspRequest::DocumentChanged { text, version } = final_request {
                    if initialized {
                        if let Ok(current_uri_guard) = current_file.read() {
                            if let Some(ref current_uri) = *current_uri_guard {
                                send_did_change(&mut stdin, current_uri, text, *version)?;
                                last_text = text.clone();
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
                        if !initialized {
                            // Send initialize request
                            let root_uri = workspace_root
                                .as_ref()
                                .and_then(|p| Url::from_file_path(p).ok())
                                .or_else(|| {
                                    file_path.parent().and_then(|p| Url::from_file_path(p).ok())
                                })
                                .map(|url| Uri::from_str(url.as_str()).unwrap());

                            send_initialize(&mut stdin, &mut next_id, root_uri)?;

                            // Wait a bit for initialize response
                            // TODO - revisit this
                            std::thread::sleep(Duration::from_millis(0));

                            initialized = true;

                            // Send initialized notification
                            send_initialized(&mut stdin)?;
                        }

                        // Open the document
                        let file_url =
                            Url::from_file_path(&file_path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        *current_file.write().unwrap() = Some(file_uri.clone());
                        last_text = text.clone();
                        send_did_open(&mut stdin, &file_uri, &text)?;
                    }
                    LspRequest::DocumentChanged { text, version } => {
                        // Debounce changes
                        pending_changes.push(LspRequest::DocumentChanged { text, version });
                        last_change_time = std::time::Instant::now();
                    }
                    LspRequest::DocumentSaved { path, text } => {
                        let file_url =
                            Url::from_file_path(&path).map_err(|_| "Invalid file path")?;
                        let file_uri = Uri::from_str(file_url.as_str()).unwrap();
                        send_did_save(&mut stdin, &file_uri, Some(&text))?;
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
    current_file: Arc<RwLock<Option<Uri>>>,
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
                        handle_lsp_message(msg, &diagnostics_tx, &current_file);
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
    current_file: &Arc<RwLock<Option<Uri>>>,
) {
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
                                let update = DiagnosticUpdate {
                                    diagnostics,
                                    version: 0, // TODO: Track document version
                                };
                                let _ = diagnostics_tx.send(update);
                            }
                            // Silently ignore diagnostics for other files
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
        root_uri,
        initialization_options: Some(init_options),
        capabilities,
        trace: Some(lsp_types::TraceValue::Off),
        workspace_folders: None,
        client_info: Some(lsp_types::ClientInfo {
            name: "tiny-editor".to_string(),
            version: Some("0.1.0".to_string()),
        }),
        locale: None,
        root_path: None,
        work_done_progress_params: WorkDoneProgressParams::default(),
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

fn send_did_change(
    writer: &mut dyn Write,
    uri: &Uri,
    text: &str,
    version: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: version as i32,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_string(),
        }],
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
