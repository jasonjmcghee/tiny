//! Bounded widget system for self-contained rendering regions
//!
//! This provides a pattern for creating widgets that:
//! - Render within specific bounds
//! - Have their own scroll state
//! - Use widget-local coordinates internally
//! - Automatically clip to their bounds

use crate::coordinates::Viewport;
use tiny_core::tree::Point;
use tiny_sdk::{LayoutPos, LayoutRect, types::WidgetViewport};

/// A bounded text editor widget
pub struct BoundedTextEditor {
    /// Widget ID for plugin association
    pub widget_id: u64,

    /// Bounds in window space (position and size)
    pub bounds: LayoutRect,

    /// Internal scroll offset (widget-local)
    pub scroll: LayoutPos,

    /// Content margin within the widget (for line numbers, etc.)
    pub content_margin: LayoutPos,

    /// Whether this editor has focus
    pub has_focus: bool,
}

impl BoundedTextEditor {
    /// Create a new bounded text editor
    pub fn new(widget_id: u64, bounds: LayoutRect) -> Self {
        Self {
            widget_id,
            bounds,
            scroll: LayoutPos::new(0.0, 0.0),
            content_margin: LayoutPos::new(0.0, 0.0),
            has_focus: false,
        }
    }

    /// Get the widget viewport for plugins
    pub fn widget_viewport(&self) -> WidgetViewport {
        WidgetViewport {
            bounds: self.bounds,
            scroll: self.scroll,
            content_margin: self.content_margin,
            widget_id: self.widget_id,
        }
    }

    /// Transform widget-local position to window position
    pub fn local_to_window(&self, pos: LayoutPos) -> LayoutPos {
        LayoutPos::new(
            self.bounds.x.0 + self.content_margin.x.0 + pos.x.0 - self.scroll.x.0,
            self.bounds.y.0 + self.content_margin.y.0 + pos.y.0 - self.scroll.y.0,
        )
    }

    /// Transform window position to widget-local position
    pub fn window_to_local(&self, pos: LayoutPos) -> LayoutPos {
        LayoutPos::new(
            pos.x.0 - self.bounds.x.0 - self.content_margin.x.0 + self.scroll.x.0,
            pos.y.0 - self.bounds.y.0 - self.content_margin.y.0 + self.scroll.y.0,
        )
    }

    /// Transform Point from window to local
    pub fn point_to_local(&self, pos: Point) -> Point {
        LayoutPos::new(
            pos.x.0 - self.bounds.x.0 - self.content_margin.x.0 + self.scroll.x.0,
            pos.y.0 - self.bounds.y.0 - self.content_margin.y.0 + self.scroll.y.0,
        )
    }

    /// Check if a window position is within this widget's bounds
    pub fn contains_window_pos(&self, pos: LayoutPos) -> bool {
        self.bounds.contains(pos)
    }

    /// Check if a Point is within bounds
    pub fn contains_point(&self, pos: Point) -> bool {
        pos.x.0 >= self.bounds.x.0 &&
        pos.x.0 <= self.bounds.x.0 + self.bounds.width.0 &&
        pos.y.0 >= self.bounds.y.0 &&
        pos.y.0 <= self.bounds.y.0 + self.bounds.height.0
    }

    /// Check if a widget-local position is visible (within bounds after scroll)
    pub fn is_visible(&self, local_pos: LayoutPos) -> bool {
        let window_pos = self.local_to_window(local_pos);
        self.bounds.contains(window_pos)
    }

    /// Handle mouse click - returns local position if click was within bounds
    pub fn handle_click(&mut self, window_pos: Point) -> Option<Point> {
        if self.contains_point(window_pos) {
            self.has_focus = true;
            Some(self.point_to_local(window_pos))
        } else {
            self.has_focus = false;
            None
        }
    }

    /// Handle scroll event - returns true if scroll was handled
    pub fn handle_scroll(&mut self, window_pos: Point, delta_x: f32, delta_y: f32) -> bool {
        if self.contains_point(window_pos) && self.has_focus {
            // Update scroll position
            self.scroll.x.0 += delta_x;
            self.scroll.y.0 += delta_y;

            // Clamp scroll to valid range
            self.scroll.x.0 = self.scroll.x.0.max(0.0);
            self.scroll.y.0 = self.scroll.y.0.max(0.0);

            true
        } else {
            false
        }
    }

    /// Update the widget bounds
    pub fn set_bounds(&mut self, bounds: LayoutRect) {
        self.bounds = bounds;
    }

    /// Set content margins (for line numbers, gutters, etc.)
    pub fn set_content_margin(&mut self, x: f32, y: f32) {
        self.content_margin = LayoutPos::new(x, y);
    }

    /// Get the visible content area in widget-local coordinates
    pub fn visible_content_rect(&self) -> LayoutRect {
        LayoutRect::new(
            self.scroll.x.0,
            self.scroll.y.0,
            self.bounds.width.0 - self.content_margin.x.0,
            self.bounds.height.0 - self.content_margin.y.0,
        )
    }

    /// Set scroll position directly
    pub fn set_scroll(&mut self, x: f32, y: f32) {
        self.scroll = LayoutPos::new(x, y);
        self.scroll.x.0 = self.scroll.x.0.max(0.0);
        self.scroll.y.0 = self.scroll.y.0.max(0.0);
    }

    /// Get current scroll position
    pub fn get_scroll(&self) -> (f32, f32) {
        (self.scroll.x.0, self.scroll.y.0)
    }
}