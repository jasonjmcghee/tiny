//! LSP (Language Server Protocol) manager for real-time diagnostics
//!
//! Currently supports rust-analyzer, designed to be extensible to other language servers

use lsp_types::{
    notification::{Notification, PublishDiagnostics},
    request::{Initialize, Request, Shutdown},
    ClientCapabilities, Diagnostic as LspDiagnostic, DiagnosticSeverity, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, MessageType, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentSyncCapability, TextDocumentSyncKind, VersionedTextDocumentIdentifier,
    WorkDoneProgressParams, Uri,
};
use serde::{Deserialize, Serialize};
use url::Url;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

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
#[derive(Debug, Clone)]
pub struct ParsedDiagnostic {
    pub line: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub message: String,
    pub severity: diagnostics_plugin::DiagnosticSeverity,
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
}

impl LspManager {
    /// Create a new LSP manager for Rust files
    pub fn new_for_rust(workspace_root: Option<PathBuf>) -> Result<Self, std::io::Error> {
        let (request_tx, request_rx) = mpsc::channel::<LspRequest>();
        let (diagnostics_tx, diagnostics_rx) = mpsc::channel::<DiagnosticUpdate>();
        let diagnostics_rx = Arc::new(Mutex::new(diagnostics_rx));
        let current_version = Arc::new(AtomicU64::new(0));

        // Spawn background thread for LSP communication
        let version_clone = current_version.clone();
        thread::spawn(move || {
            if let Err(e) = run_lsp_client(request_rx, diagnostics_tx, workspace_root, version_clone) {
                eprintln!("LSP client error: {}", e);
            }
        });

        Ok(Self {
            tx: request_tx,
            diagnostics_rx,
            current_version,
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
    eprintln!("LSP: rust-analyzer process started with PID: {:?}", child.id());

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
                            let root_uri = workspace_root.as_ref()
                                .and_then(|p| Url::from_file_path(p).ok())
                                .or_else(|| file_path.parent()
                                    .and_then(|p| Url::from_file_path(p).ok()))
                                .map(|url| Uri::from_str(url.as_str()).unwrap());

                            send_initialize(&mut stdin, &mut next_id, root_uri)?;

                            // Wait a bit for initialize response
                            std::thread::sleep(Duration::from_millis(500));

                            initialized = true;

                            // Send initialized notification
                            send_initialized(&mut stdin)?;
                        }

                        // Open the document
                        let file_url = Url::from_file_path(&file_path)
                            .map_err(|_| "Invalid file path")?;
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
                        let file_url = Url::from_file_path(&path)
                            .map_err(|_| "Invalid file path")?;
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
                if let Ok(msg) = serde_json::from_str::<JsonRpcMessage>(&content) {
                    handle_lsp_message(msg, &diagnostics_tx, &current_file);
                }
            }
        }
    }
}

/// Handle a single LSP message
fn handle_lsp_message(
    msg: JsonRpcMessage,
    diagnostics_tx: &mpsc::Sender<DiagnosticUpdate>,
    current_file: &Arc<RwLock<Option<Uri>>>
) {
    if let Some(method) = msg.method {
        if method == "textDocument/publishDiagnostics" {
            if let Some(params) = msg.params {
                if let Ok(publish_params) = serde_json::from_value::<PublishDiagnosticsParams>(params) {
                    // Only process diagnostics for the currently open file
                    if let Ok(current_uri_guard) = current_file.read() {
                        if let Some(ref current_uri) = *current_uri_guard {
                            if publish_params.uri == *current_uri {
                                // Only log if we actually have diagnostics
                            if !publish_params.diagnostics.is_empty() {
                                eprintln!("LSP: Got {} diagnostics for current file", publish_params.diagnostics.len());
                            }
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
        .map(|d| {
            let severity = match d.severity {
                Some(DiagnosticSeverity::ERROR) => diagnostics_plugin::DiagnosticSeverity::Error,
                Some(DiagnosticSeverity::WARNING) => diagnostics_plugin::DiagnosticSeverity::Warning,
                _ => diagnostics_plugin::DiagnosticSeverity::Info,
            };

            ParsedDiagnostic {
                line: d.range.start.line as usize,
                column_start: d.range.start.character as usize,
                column_end: d.range.end.character as usize,
                message: d.message,
                severity,
            }
        })
        .collect()
}

// === JSON-RPC Message Senders ===

fn send_message(writer: &mut dyn Write, msg: &JsonRpcMessage) -> Result<(), Box<dyn std::error::Error>> {
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
    let params = InitializeParams {
        process_id: Some(std::process::id()),
        root_uri,
        initialization_options: None,
        capabilities: ClientCapabilities::default(),
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