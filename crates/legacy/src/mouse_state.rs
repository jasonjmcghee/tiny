//! Mouse state tracking
//!
//! Centralizes mouse position, button state, drag tracking, and modifiers

use crate::accelerator::Modifiers;
use winit::dpi::PhysicalPosition;

/// Tracks all mouse-related state for the application
#[derive(Debug, Clone)]
pub struct MouseState {
    /// Current cursor position in physical pixels
    pub position: Option<PhysicalPosition<f64>>,
    /// Whether any mouse button is currently pressed
    pub pressed: bool,
    /// Position where the current drag started (if dragging)
    pub drag_start: Option<PhysicalPosition<f64>>,
    /// Current modifier keys (cmd, ctrl, alt, shift)
    pub modifiers: Modifiers,
}

impl MouseState {
    pub fn new() -> Self {
        Self {
            position: None,
            pressed: false,
            drag_start: None,
            modifiers: Modifiers::default(),
        }
    }

    /// Update cursor position
    pub fn set_position(&mut self, pos: PhysicalPosition<f64>) {
        self.position = Some(pos);
    }

    /// Start a drag operation
    pub fn start_drag(&mut self, pos: PhysicalPosition<f64>) {
        self.pressed = true;
        self.drag_start = Some(pos);
    }

    /// End drag operation
    pub fn end_drag(&mut self) {
        self.pressed = false;
        self.drag_start = None;
    }

    /// Check if currently dragging
    pub fn is_dragging(&self) -> bool {
        self.pressed && self.drag_start.is_some()
    }

    /// Get drag delta (from start to current position)
    pub fn drag_delta(&self) -> Option<(f64, f64)> {
        match (self.drag_start, self.position) {
            (Some(start), Some(current)) => Some((
                current.x - start.x,
                current.y - start.y,
            )),
            _ => None,
        }
    }

    /// Update modifiers
    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }
}

impl Default for MouseState {
    fn default() -> Self {
        Self::new()
    }
}
