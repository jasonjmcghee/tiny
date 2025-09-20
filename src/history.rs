//! Generic history management for undo/redo and navigation
//!
//! Provides a reusable history stack for any cloneable type

use crate::coordinates::DocPos;
use crate::input::Selection;
use std::sync::Arc;

/// Generic history tracker for undo/redo operations
pub struct History<T> {
    /// Undo stack
    undo: Vec<T>,
    /// Redo stack
    redo: Vec<T>,
    /// Maximum history size
    max_size: usize,
}

impl<T> History<T> {
    pub fn new() -> Self {
        Self::with_max_size(100)
    }

    pub fn with_max_size(max_size: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_size,
        }
    }

    /// Save checkpoint for undo
    pub fn checkpoint(&mut self, item: T) {
        self.undo.push(item);
        self.redo.clear(); // Clear redo on new edit

        // Limit history size
        if self.undo.len() > self.max_size {
            self.undo.remove(0);
        }
    }

    /// Undo last operation
    pub fn undo(&mut self, current: T) -> Option<T> {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(current);
            Some(previous)
        } else {
            None
        }
    }

    /// Redo last undone operation
    pub fn redo(&mut self, current: T) -> Option<T> {
        if let Some(next) = self.redo.pop() {
            self.undo.push(current);
            Some(next)
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

    /// Get current undo stack depth
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Get current redo stack depth
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
}

/// Document snapshot for undo/redo - captures both content and cursor state
#[derive(Clone)]
pub struct DocumentSnapshot {
    /// Document tree at this point in time
    pub tree: Arc<crate::tree::Tree>,
    /// All selections/cursors at this point in time
    pub selections: Vec<Selection>,
}

/// Type alias for document history with full state (undo/redo)
pub type DocumentHistory = History<DocumentSnapshot>;

/// Type alias for tree history (document content only)
pub type TreeHistory = History<Arc<crate::tree::Tree>>;

/// Type alias for cursor navigation history (Cmd+[/])
pub type SelectionHistory = History<DocPos>;

impl<T: Clone> History<T> {
    /// Peek at the next undo item without removing it
    pub fn peek_undo(&self) -> Option<&T> {
        self.undo.last()
    }

    /// Peek at the next redo item without removing it
    pub fn peek_redo(&self) -> Option<&T> {
        self.redo.last()
    }
}

impl<T: PartialEq> History<T> {
    /// Save checkpoint only if different from the last item
    pub fn checkpoint_if_changed(&mut self, item: T) {
        if self.undo.last() != Some(&item) {
            self.checkpoint(item);
        }
    }
}
