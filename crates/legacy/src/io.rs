//! File I/O operations
//!
//! Load and save documents

use std::fs;
use std::io;
use std::path::Path;
use tiny_core::tree::Doc;

/// Load document from file
/// Handles both text and binary files by replacing invalid UTF-8 with �
/// Uses simdutf8 for fast validation
pub fn load(path: &Path) -> io::Result<Doc> {
    let bytes = fs::read(path)?;
    let content = match simdutf8::basic::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
    };
    Ok(Doc::from_str(&content))
}

/// Save document to file
pub fn save(doc: &Doc, path: &Path) -> io::Result<()> {
    let content = doc.read().flatten_to_string();
    fs::write(path, content.as_ref())
}

/// Auto-save to temporary file
pub fn autosave(doc: &Doc, path: &Path) -> io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    save(doc, &tmp_path)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}
