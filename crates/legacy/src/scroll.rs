//! Scroll management for widgets

use tiny_core::tree::{Point, Rect};
use crate::coordinates::Viewport;

/// Widget that can be scrolled
pub trait Scrollable {
    /// Get current scroll position
    fn get_scroll(&self) -> Point;

    /// Set scroll position (will be clamped by implementation)
    fn set_scroll(&mut self, scroll: Point);

    /// Handle scroll delta with viewport metrics and widget bounds
    /// widget_bounds: The visible bounds of the widget in screen coordinates
    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool;

    /// Get content bounds for scroll clamping, using viewport metrics
    fn get_content_bounds(&self, viewport: &Viewport) -> Rect;
}

/// Identifies a scrollable widget
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidgetId {
    Editor,
    FilePicker,
    Grep,
    TabBar,
    Diagnostics,
    // Add more as needed
}

/// Manages scroll focus and routing
pub struct ScrollFocusManager {
    focused_widget: Option<WidgetId>,
}

impl ScrollFocusManager {
    pub fn new() -> Self {
        Self {
            focused_widget: None,
        }
    }

    /// Update which widget has scroll focus based on mouse position
    pub fn update_focus(&mut self, mouse_pos: Point, widget_bounds: &[(WidgetId, Rect, i32)]) {
        // Sort by z-index (highest first)
        let mut sorted = widget_bounds.to_vec();
        sorted.sort_by(|a, b| b.2.cmp(&a.2));

        // Find first widget that contains mouse position
        self.focused_widget = sorted.iter()
            .find(|(_, bounds, _)| Self::point_in_rect(mouse_pos, *bounds))
            .map(|(id, _, _)| *id);
    }

    /// Get currently focused widget
    pub fn focused_widget(&self) -> Option<WidgetId> {
        self.focused_widget
    }

    /// Manually set focus to a specific widget (e.g., when file picker opens)
    pub fn set_focus(&mut self, widget: WidgetId) {
        self.focused_widget = Some(widget);
    }

    /// Clear focus (e.g., when mouse leaves window)
    pub fn clear_focus(&mut self) {
        self.focused_widget = None;
    }

    fn point_in_rect(point: Point, rect: Rect) -> bool {
        point.x.0 >= rect.x.0
            && point.x.0 <= rect.x.0 + rect.width.0
            && point.y.0 >= rect.y.0
            && point.y.0 <= rect.y.0 + rect.height.0
    }
}
