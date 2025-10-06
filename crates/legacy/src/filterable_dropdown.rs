//! FilterableDropdown - Reusable component for search with results
//!
//! Dual-focus pattern:
//! - Input buffer: Always active for typing/backspace
//! - Results buffer: Shows items, arrows navigate, enter selects
//!
//! Used by: grep, file picker, command palette, etc.

use crate::coordinates::Viewport;
use crate::editable_text_view::EditableTextView;
use crate::input_types::{Key, Modifiers, NamedKey};
use crate::text_view::TextView;
use tiny_core::tree::Rect;
use tiny_sdk::{types::RoundedRectInstance, LogicalPixels};

/// Action returned by FilterableDropdown after handling input
#[derive(Debug, Clone)]
pub enum DropdownAction<T> {
    /// Continue showing dropdown
    Continue,
    /// User selected an item
    Selected(T),
    /// User cancelled (Escape)
    Cancelled,
    /// Filter changed, need to update results
    FilterChanged(String),
}

/// Filterable dropdown with input + results
pub struct FilterableDropdown<T: Clone> {
    /// Input field (single line, always active)
    pub input: EditableTextView,

    /// Results display (read-only, shows formatted items)
    pub results: TextView,

    /// Title display (read-only)
    pub title_view: TextView,

    /// Actual data items
    items: Vec<T>,

    /// Currently selected index in items
    selected_index: usize,

    /// Function to format items for display
    format_fn: Box<dyn Fn(&T) -> String + Send + Sync>,

    /// Whether dropdown is visible
    pub visible: bool,

    /// Bounds for layout (includes title + border)
    bounds: Rect,

    /// Highlighted line in results
    highlighted_line: Option<usize>,
}

impl<T: Clone> FilterableDropdown<T> {
    /// Create a new filterable dropdown
    pub fn new<F>(format_fn: F) -> Self
    where
        F: Fn(&T) -> String + Send + Sync + 'static,
    {
        // Create placeholder viewports (will be updated by calculate_bounds)
        let placeholder_viewport = Viewport::new(800.0, 600.0, 1.0);

        Self {
            input: EditableTextView::single_line(placeholder_viewport.clone()),
            results: TextView::empty(placeholder_viewport.clone()),
            title_view: TextView::empty(placeholder_viewport),
            items: Vec::new(),
            selected_index: 0,
            format_fn: Box::new(format_fn),
            visible: false,
            bounds: Rect::default(),
            highlighted_line: None,
        }
    }

    /// Show dropdown with initial items and title
    pub fn show(&mut self, items: Vec<T>) {
        self.show_with_title(items, "")
    }

    /// Show dropdown with title
    pub fn show_with_title(&mut self, items: Vec<T>, title: &str) {
        self.visible = true;
        self.items = items;
        self.selected_index = 0;
        self.title_view.set_text(title);
        self.input.clear();
        self.input.set_focused(true); // Focus input for typing
        self.update_results_display();
    }

    /// Hide dropdown
    pub fn hide(&mut self) {
        self.visible = false;
        self.input.clear();
        self.results.clear();
        self.items.clear();
        self.selected_index = 0;
    }

    /// Update items (after filtering)
    pub fn set_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.selected_index = 0;
        self.update_results_display();
    }

    /// Get current filter text
    pub fn filter_text(&self) -> String {
        self.input.text().as_ref().clone()
    }

    /// Update results display based on current items
    fn update_results_display(&mut self) {
        if self.items.is_empty() {
            self.results.set_text("No results");
            self.highlighted_line = None;
        } else {
            let text = self
                .items
                .iter()
                .enumerate()
                .map(|(idx, item)| {
                    let formatted = (self.format_fn)(item);
                    // Add selection indicator
                    if idx == self.selected_index {
                        format!("→ {}", formatted)
                    } else {
                        format!("  {}", formatted)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.results.set_text(&text);

            // Track highlighted line for rendering
            self.highlighted_line = Some(self.selected_index);
        }
    }

    /// Handle keyboard input with dual-focus logic
    pub fn handle_key(
        &mut self,
        key: &Key,
        modifiers: &Modifiers,
        viewport: &Viewport,
    ) -> DropdownAction<T> {
        match key {
            // Escape → cancel
            Key::Named(NamedKey::Escape) => DropdownAction::Cancelled,

            // Enter → select current item
            Key::Named(NamedKey::Enter) => {
                if !self.items.is_empty() && self.selected_index < self.items.len() {
                    DropdownAction::Selected(self.items[self.selected_index].clone())
                } else {
                    DropdownAction::Continue
                }
            }

            // Arrow Up/Down → navigate results
            Key::Named(NamedKey::ArrowUp) => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                    self.update_results_display();
                    self.scroll_to_selected(viewport);
                }
                DropdownAction::Continue
            }

            Key::Named(NamedKey::ArrowDown) => {
                if self.selected_index < self.items.len().saturating_sub(1) {
                    self.selected_index += 1;
                    self.update_results_display();
                    self.scroll_to_selected(viewport);
                }
                DropdownAction::Continue
            }

            // Backspace → update filter
            Key::Named(NamedKey::Backspace) => {
                self.input.handle_backspace();
                DropdownAction::FilterChanged(self.filter_text())
            }

            // Delete → update filter
            Key::Named(NamedKey::Delete) => {
                self.input.handle_delete();
                DropdownAction::FilterChanged(self.filter_text())
            }

            // Character input → update filter
            Key::Character(ch) => {
                for c in ch.chars() {
                    self.input.handle_char(c);
                }
                DropdownAction::FilterChanged(self.filter_text())
            }

            _ => DropdownAction::Continue,
        }
    }

    /// Calculate bounds for dropdown (centered, adaptive height)
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        const PADDING: f32 = 20.0;
        const TITLE_BAR_HEIGHT: f32 = 28.0; // Should match config title_bar_height
        const TAB_BAR_HEIGHT: f32 = 30.0;
        const TOP_MARGIN_BELOW_TABS: f32 = 10.0;
        const BORDER_WIDTH: f32 = 2.0;

        // Use line_height for sizing to ensure text fits properly
        // Add extra space for ascenders/descenders and padding
        let line_height = viewport.metrics.line_height;
        let title_vertical_padding = 8.0; // Padding above/below title text
        let input_vertical_padding = 6.0; // Padding above/below input text
        let dropdown_title_height = line_height + title_vertical_padding * 2.0;
        let input_height = line_height + input_vertical_padding * 2.0;

        let max_visible_results = 15;
        let visible_count = self.items.len().min(max_visible_results).max(1); // At least 1 line for "No results"
        let results_height = (visible_count as f32) * line_height;

        // Total height: title + input + results + padding + border
        let has_title = !self.title_view.text().is_empty();
        let title_space = if has_title {
            dropdown_title_height
        } else {
            0.0
        };
        let total_height =
            title_space + input_height + results_height + PADDING * 2.0 + BORDER_WIDTH * 2.0;
        let width = (viewport.logical_size.width.0 * 0.9).min(1200.0);

        let x = (viewport.logical_size.width.0 - width) / 2.0;
        // Start below title bar and tabs to avoid covering them
        let y = TITLE_BAR_HEIGHT + TAB_BAR_HEIGHT + TOP_MARGIN_BELOW_TABS;

        self.bounds = Rect {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(width),
            height: LogicalPixels(total_height),
        };

        // Update title viewport bounds, scale, logical_size, and metrics (if present)
        if has_title {
            const TITLE_PADDING_X: f32 = 8.0;
            let title_bounds_width = width - BORDER_WIDTH * 2.0 - TITLE_PADDING_X * 2.0;
            let title_bounds_height = line_height; // Full line height for text

            self.title_view.viewport.bounds = tiny_sdk::types::LayoutRect::new(
                x + BORDER_WIDTH + TITLE_PADDING_X,
                y + BORDER_WIDTH + title_vertical_padding,
                title_bounds_width,
                title_bounds_height,
            );
            // Update logical_size to match bounds for correct visible range calculation
            self.title_view.viewport.logical_size =
                tiny_sdk::LogicalSize::new(title_bounds_width, title_bounds_height);
            self.title_view.viewport.scale_factor = viewport.scale_factor;
            self.title_view.viewport.metrics = viewport.metrics.clone();
            // Ensure scroll is at origin
            self.title_view.viewport.scroll = tiny_sdk::types::LayoutPos::new(0.0, 0.0);
        }

        // Update input viewport bounds, scale, logical_size, and metrics (below title + border)
        let content_start_y = y + BORDER_WIDTH + title_space;
        let input_bounds_width = width - PADDING * 2.0 - BORDER_WIDTH * 2.0;

        self.input.view.viewport.bounds = tiny_sdk::types::LayoutRect::new(
            x + PADDING + BORDER_WIDTH,
            content_start_y + PADDING,
            input_bounds_width,
            input_height,
        );
        // Update logical_size to match bounds
        self.input.view.viewport.logical_size =
            tiny_sdk::LogicalSize::new(input_bounds_width, input_height);
        self.input.view.viewport.scale_factor = viewport.scale_factor;
        self.input.view.viewport.metrics = viewport.metrics.clone();
        self.input.view.viewport.scroll = tiny_sdk::types::LayoutPos::new(0.0, 0.0);

        // Set padding on the input view to inset text from edges
        self.input.view.padding_x = 0.0;
        self.input.view.padding_y = input_vertical_padding;

        // Update results viewport bounds, scale, logical_size, and metrics (below input)
        let results_bounds_width = width - PADDING * 2.0 - BORDER_WIDTH * 2.0;

        self.results.viewport.bounds = tiny_sdk::types::LayoutRect::new(
            x + PADDING + BORDER_WIDTH,
            content_start_y + PADDING + input_height + 10.0,
            results_bounds_width,
            results_height,
        );
        // Update logical_size to match bounds
        self.results.viewport.logical_size =
            tiny_sdk::LogicalSize::new(results_bounds_width, results_height);
        self.results.viewport.scale_factor = viewport.scale_factor;
        self.results.viewport.metrics = viewport.metrics.clone();
        // Results can scroll, so don't reset scroll here
    }

    /// Get bounds for rendering
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Get selected index
    pub fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Get current items
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Scroll results to keep selected item visible
    fn scroll_to_selected(&mut self, viewport: &Viewport) {
        if self.items.is_empty() {
            return;
        }

        let line_height = viewport.metrics.line_height;
        let selected_y = self.selected_index as f32 * line_height;
        let visible_height = self.results.viewport.bounds.height.0;

        // If selected item is below visible area, scroll down
        if selected_y + line_height > self.results.viewport.scroll.y.0 + visible_height {
            self.results.viewport.scroll.y.0 = selected_y + line_height - visible_height;
        }
        // If selected item is above visible area, scroll up
        else if selected_y < self.results.viewport.scroll.y.0 {
            self.results.viewport.scroll.y.0 = selected_y;
        }
    }

    /// Get border and background rects for rendering
    pub fn get_chrome_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        let mut rects = Vec::new();

        if !self.visible {
            return rects;
        }

        // Subtle color scheme - slightly differentiated from core bg
        const INPUT_BG: u32 = 0x232629FF; // Input field background - lighter
        const RESULTS_BG: u32 = 0x1C1F21FF; // Results area background (matches core bg)

        // Use viewport bounds directly (already calculated in calculate_bounds)
        // Input field background - use full input viewport bounds including padding
        let input_full_bounds = tiny_core::tree::Rect {
            x: tiny_sdk::LogicalPixels(self.input.view.viewport.bounds.x.0),
            y: tiny_sdk::LogicalPixels(self.input.view.viewport.bounds.y.0),
            width: tiny_sdk::LogicalPixels(self.input.view.viewport.bounds.width.0),
            height: tiny_sdk::LogicalPixels(self.input.view.viewport.bounds.height.0),
        };

        rects.push(tiny_sdk::types::RectInstance {
            rect: input_full_bounds,
            color: INPUT_BG,
        });

        // Results area background - use full results viewport bounds
        let results_full_bounds = tiny_core::tree::Rect {
            x: tiny_sdk::LogicalPixels(self.results.viewport.bounds.x.0),
            y: tiny_sdk::LogicalPixels(self.results.viewport.bounds.y.0),
            width: tiny_sdk::LogicalPixels(self.results.viewport.bounds.width.0),
            height: tiny_sdk::LogicalPixels(self.results.viewport.bounds.height.0),
        };

        rects.push(tiny_sdk::types::RectInstance {
            rect: results_full_bounds,
            color: RESULTS_BG,
        });

        rects
    }

    /// Get rounded rect for frame with border (SDF rendering)
    pub fn get_frame_rounded_rect(&self) -> Option<RoundedRectInstance> {
        if !self.visible {
            return None;
        }

        // Subtle color scheme
        const FRAME_BG: u32 = 0x1A1D1FFF; // Frame background (RGBA) - slightly darker
        const BORDER_COLOR: u32 = 0x30343AFF; // Border color - subtle contrast
        const CORNER_RADIUS: f32 = 4.0; // Rounded corners
        const BORDER_WIDTH: f32 = 1.0; // Border width

        Some(RoundedRectInstance {
            rect: tiny_sdk::types::LayoutRect {
                x: self.bounds.x,
                y: self.bounds.y,
                width: self.bounds.width,
                height: self.bounds.height,
            },
            color: FRAME_BG,
            border_color: BORDER_COLOR,
            corner_radius: CORNER_RADIUS,
            border_width: BORDER_WIDTH,
        })
    }

    /// Handle mouse wheel scroll
    pub fn handle_scroll(&mut self, delta_y: f32) {
        use crate::scroll::Scrollable;

        let delta = tiny_core::tree::Point {
            x: tiny_sdk::LogicalPixels(0.0),
            y: tiny_sdk::LogicalPixels(delta_y),
        };

        // Use TextView's Scrollable implementation for proper scroll clamping
        self.results.handle_scroll(delta, &self.results.viewport.clone(), self.results.viewport.bounds);
    }

    /// Handle mouse click - returns Selected if item clicked, or updates selection
    pub fn handle_click(&mut self, x: f32, y: f32, shift: bool) -> DropdownAction<T> {
        // Check if click is in input area
        let input_bounds = &self.input.view.viewport.bounds;
        if x >= input_bounds.x.0
            && x < input_bounds.x.0 + input_bounds.width.0
            && y >= input_bounds.y.0
            && y < input_bounds.y.0 + input_bounds.height.0
        {
            // Let input handle the click for cursor positioning
            let screen_pos = tiny_core::tree::Point {
                x: tiny_sdk::LogicalPixels(x),
                y: tiny_sdk::LogicalPixels(y),
            };
            self.input.handle_click(screen_pos, shift, false);
            return DropdownAction::Continue;
        }

        // Check if click is in results area
        let results_bounds = &self.results.viewport.bounds;
        if x >= results_bounds.x.0
            && x < results_bounds.x.0 + results_bounds.width.0
            && y >= results_bounds.y.0
            && y < results_bounds.y.0 + results_bounds.height.0
        {
            // Convert y to line index
            let relative_y = y - results_bounds.y.0 + self.results.viewport.scroll.y.0;
            let line_height = self.results.viewport.metrics.line_height;
            let line_idx = (relative_y / line_height) as usize;

            if line_idx < self.items.len() {
                return DropdownAction::Selected(self.items[line_idx].clone());
            }
        }

        DropdownAction::Continue
    }
}
