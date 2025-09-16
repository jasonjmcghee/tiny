//! History management for undo/redo
//!
//! Leverages immutable tree snapshots for nearly-free undo

use crate::tree::Tree;
use std::sync::Arc;

/// History tracker
pub struct History {
    /// Undo stack
    undo: Vec<Arc<Tree>>,
    /// Redo stack
    redo: Vec<Arc<Tree>>,
    /// Maximum history size
    max_size: usize,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_size: 100,
        }
    }

    /// Save checkpoint for undo
    pub fn checkpoint(&mut self, tree: Arc<Tree>) {
        self.undo.push(tree);
        self.redo.clear(); // Clear redo on new edit

        // Limit history size
        if self.undo.len() > self.max_size {
            self.undo.remove(0);
        }
    }

    /// Undo last operation
    pub fn undo(&mut self, current: Arc<Tree>) -> Option<Arc<Tree>> {
        if let Some(tree) = self.undo.pop() {
            self.redo.push(current);
            Some(tree)
        } else {
            None
        }
    }

    /// Redo last undone operation
    pub fn redo(&mut self, current: Arc<Tree>) -> Option<Arc<Tree>> {
        if let Some(tree) = self.redo.pop() {
            self.undo.push(current);
            Some(tree)
        } else {
            None
        }
    }

    /// Clear history
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Check if undo available
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Check if redo available
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
