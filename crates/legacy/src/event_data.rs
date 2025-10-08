//! Typed event data structures
//!
//! All event payloads should be defined here with proper Serialize/Deserialize

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MousePressData {
    pub x: f64,
    pub y: f64,
    pub screen_x: f64,
    pub screen_y: f64,
    pub physical_x: f64,
    pub physical_y: f64,
    pub button: String,
    pub modifiers: ModifiersData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifiersData {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseDragData {
    pub from_x: f64,
    pub from_y: f64,
    pub to_x: f64,
    pub to_y: f64,
    pub alt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsertCharData {
    pub char: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOpenData {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileGotoData {
    pub file: String,
    pub line: u64,
    pub column: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DragScrollData {
    pub delta_x: f64,
    pub delta_y: f64,
}

/// Helper to convert event data from JSON Value
pub fn from_value<T: for<'de> Deserialize<'de>>(value: &serde_json::Value) -> anyhow::Result<T> {
    serde_json::from_value(value.clone())
        .map_err(|e| anyhow::anyhow!("Failed to deserialize event data: {}", e))
}
