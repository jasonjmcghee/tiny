//! Tab bar plugin - renders tabs at the top of the screen

use crate::coordinates::Viewport;
use crate::scroll::Scrollable;
use crate::tab_manager::TabManager;
use tiny_core::tree::{Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::types::{LayoutRect, RectInstance};
use tiny_sdk::LayoutPos;
use tiny_ui::text_view::TextView;

/// Default tab bar height in logical pixels (will be calculated dynamically)
pub const TAB_BAR_HEIGHT: f32 = 30.0;
/// Vertical padding for tab text (top + bottom)
const TAB_VERTICAL_PADDING: f32 = 10.0;
/// Minimum tab width in logical pixels
const MIN_TAB_WIDTH: f32 = 120.0;
/// Dropdown menu width
const DROPDOWN_WIDTH: f32 = 200.0;
/// Maximum dropdown height
const MAX_DROPDOWN_HEIGHT: f32 = 300.0;

/// Tab bar plugin that renders tabs
pub struct TabBarPlugin {
    /// Height of the tab bar
    pub height: f32,
    /// Horizontal scroll offset for tabs
    scroll_offset: f32,
    /// Whether the dropdown menu is open
    dropdown_open: bool,
    /// Dropdown scroll offset
    dropdown_scroll_offset: f32,
}

impl TabBarPlugin {
    pub fn new() -> Self {
        Self {
            height: TAB_BAR_HEIGHT,
            scroll_offset: 0.0,
            dropdown_open: false,
            dropdown_scroll_offset: 0.0,
        }
    }

    /// Calculate tab width based on viewport width and number of tabs
    fn calculate_tab_width(&self, viewport_width: f32, num_tabs: usize) -> f32 {
        if num_tabs == 0 {
            return MIN_TAB_WIDTH;
        }

        // Reserve space for dropdown arrow (40px)
        let available_width = viewport_width - 40.0;

        // Calculate ideal width: 100%, 50%, 33%, 25% etc
        let ideal_width = available_width / num_tabs as f32;

        // Use minimum width if calculated width is too small
        ideal_width.max(MIN_TAB_WIDTH)
    }

    /// Ensure the active tab is visible by adjusting scroll offset
    pub fn scroll_to_tab(&mut self, tab_index: usize, viewport_width: f32, num_tabs: usize) {
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        let tab_start = 10.0 + (tab_index as f32 * tab_width);
        let tab_end = tab_start + tab_width;
        let visible_width = viewport_width - 40.0; // Account for dropdown arrow

        // If tab starts before visible area, scroll left to show it
        if tab_start < self.scroll_offset {
            self.scroll_offset = tab_start;
        }
        // If tab ends after visible area, scroll right to show it
        else if tab_end > self.scroll_offset + visible_width {
            self.scroll_offset = tab_end - visible_width;
        }

        // Clamp scroll offset
        self.scroll_offset = self.scroll_offset.max(0.0);
    }

    /// Toggle dropdown menu
    pub fn toggle_dropdown(&mut self) {
        self.dropdown_open = !self.dropdown_open;
        if self.dropdown_open {
            self.dropdown_scroll_offset = 0.0;
        }
    }

    /// Close dropdown menu
    pub fn close_dropdown(&mut self) {
        self.dropdown_open = false;
    }

    /// Calculate height based on line height (hug contents)
    pub fn calculate_height(&mut self, line_height: f32) {
        self.height = line_height + TAB_VERTICAL_PADDING;
    }

    /// Collect background rectangles for tabs
    pub fn collect_rects(
        &self,
        tab_manager: &TabManager,
        viewport_width: f32,
    ) -> Vec<RectInstance> {
        let mut rects = Vec::new();
        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        let mut x_offset = 10.0 - self.scroll_offset;

        // Colors (RGBA as u32) - match main background: rgb(0.11, 0.12, 0.13)
        // Active tab matches background (invisible/flush), inactive tabs are slightly darker
        const ACTIVE_TAB_BG: u32 = 0x1C1F21FF; // Same as main background (28, 31, 33)
        const INACTIVE_TAB_BG: u32 = 0x16191BFF; // Slightly darker than background (22, 25, 27)

        for (idx, _tab) in tab_manager.tabs().iter().enumerate() {
            let is_active = idx == tab_manager.active_index();

            let tab_rect = RectInstance {
                rect: LayoutRect::new(x_offset, 0.0, tab_width, self.height),
                color: if is_active {
                    ACTIVE_TAB_BG
                } else {
                    INACTIVE_TAB_BG
                },
            };

            rects.push(tab_rect);
            x_offset += tab_width;
        }

        rects
    }

    /// Scroll tabs left
    pub fn scroll_left(&mut self) {
        self.scroll_offset = (self.scroll_offset - 100.0).max(0.0);
    }

    /// Scroll tabs right
    pub fn scroll_right(&mut self, max_offset: f32) {
        self.scroll_offset = (self.scroll_offset + 100.0).min(max_offset);
    }

    /// Collect glyphs for rendering tabs using TextView
    pub fn collect_glyphs(
        &mut self,
        collector: &mut crate::render::GlyphCollector,
        tab_manager: &TabManager,
    ) {
        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let line_height = collector.viewport.line_height;

        // Extract widget bounds before loop
        let bounds_x = collector
            .widget_viewport
            .as_ref()
            .map(|w| w.bounds.x.0)
            .unwrap_or(0.0);
        let bounds_y = collector
            .widget_viewport
            .as_ref()
            .map(|w| w.bounds.y.0)
            .unwrap_or(0.0);
        let viewport_width = collector.viewport.logical_size.width.0;

        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);

        let mut x_offset = 10.0 - self.scroll_offset;

        const TAB_PADDING: f32 = 10.0;
        const CLOSE_BUTTON_WIDTH: f32 = 20.0;

        // Render each tab using TextView
        for (idx, tab) in tab_manager.tabs().iter().enumerate() {
            let is_active = idx == tab_manager.active_index();

            // Tab text
            let mut display_name = tab.display_name.clone();
            if tab.is_modified() {
                display_name.push_str(" •");
            }

            // Create and configure viewport for this tab's text
            let mut tab_viewport = Viewport::new(tab_width - CLOSE_BUTTON_WIDTH, self.height, collector.viewport.scale_factor);
            collector.configure_viewport(&mut tab_viewport);
            tab_viewport.bounds = Rect {
                x: tiny_sdk::LogicalPixels(bounds_x + x_offset),
                y: tiny_sdk::LogicalPixels(bounds_y),
                width: tiny_sdk::LogicalPixels(tab_width - CLOSE_BUTTON_WIDTH),
                height: tiny_sdk::LogicalPixels(self.height),
            };

            // Create TextView for tab text
            let mut tab_text_view = TextView::from_text(&display_name, tab_viewport)
                .with_padding_x(TAB_PADDING)
                .with_padding_y((self.height - line_height) / 2.0) // Vertically center
                .with_align(tiny_ui::text_view::TextAlign::Center); // Horizontally center

            tab_text_view.update_layout(&font_service);
            let mut tab_glyphs = tab_text_view.collect_glyphs(&font_service);

            // Set token based on active state (0 = normal white text, 255 = dimmed)
            for glyph in &mut tab_glyphs {
                glyph.token_id = if is_active { 0 } else { 255 };
            }

            collector.add_glyphs(tab_glyphs);

            // Close button "x"
            let close_x = x_offset + tab_width - CLOSE_BUTTON_WIDTH;

            let mut close_viewport = Viewport::new(CLOSE_BUTTON_WIDTH, self.height, collector.viewport.scale_factor);
            collector.configure_viewport(&mut close_viewport);
            close_viewport.bounds = Rect {
                x: tiny_sdk::LogicalPixels(bounds_x + close_x),
                y: tiny_sdk::LogicalPixels(bounds_y),
                width: tiny_sdk::LogicalPixels(CLOSE_BUTTON_WIDTH),
                height: tiny_sdk::LogicalPixels(self.height),
            };

            let mut close_view = TextView::from_text("×", close_viewport)
                .with_align(tiny_ui::text_view::TextAlign::Center)
                .with_padding_y((self.height - line_height) / 2.0);
            close_view.update_layout(&font_service);

            let mut close_glyphs = close_view.collect_glyphs(&font_service);
            for glyph in &mut close_glyphs {
                glyph.token_id = 3; // Close button token
            }
            collector.add_glyphs(close_glyphs);

            x_offset += tab_width; // Move to next tab position
        }

        // Dropdown arrow on the far right (always visible)
        let dropdown_x = viewport_width - 30.0;
        let dropdown_y = (self.height - line_height) / 2.0;

        let mut dropdown_viewport = Viewport::new(30.0, self.height, collector.viewport.scale_factor);
        collector.configure_viewport(&mut dropdown_viewport);
        dropdown_viewport.bounds = Rect {
            x: tiny_sdk::LogicalPixels(bounds_x + dropdown_x),
            y: tiny_sdk::LogicalPixels(bounds_y + dropdown_y),
            width: tiny_sdk::LogicalPixels(30.0),
            height: tiny_sdk::LogicalPixels(line_height),
        };

        let mut dropdown_view = TextView::from_text("▼", dropdown_viewport);
        dropdown_view.update_layout(&font_service);

        let mut dropdown_glyphs = dropdown_view.collect_glyphs(&font_service);
        for glyph in &mut dropdown_glyphs {
            glyph.token_id = 4; // Dropdown arrow token
        }
        collector.add_glyphs(dropdown_glyphs);

        // Render dropdown menu if open
        if self.dropdown_open {
            let dropdown_start_y = self.height;

            for (idx, tab) in tab_manager.tabs().iter().enumerate() {
                let item_y =
                    dropdown_start_y + (idx as f32 * line_height) - self.dropdown_scroll_offset;

                // Skip rendering if item is outside visible dropdown area
                if item_y < dropdown_start_y || item_y > dropdown_start_y + MAX_DROPDOWN_HEIGHT {
                    continue;
                }

                let is_active = idx == tab_manager.active_index();

                let mut display_name = tab.display_name.clone();
                if tab.is_modified() {
                    display_name.push_str(" •");
                }

                let marker = if is_active { "▸ " } else { "  " };
                let dropdown_text = format!("{}{}", marker, display_name);

                // Create TextView for dropdown item
                let dropdown_item_x = dropdown_x - DROPDOWN_WIDTH + 10.0;
                let mut dropdown_item_viewport = Viewport::new(DROPDOWN_WIDTH - 30.0, line_height, collector.viewport.scale_factor);
                collector.configure_viewport(&mut dropdown_item_viewport);
                dropdown_item_viewport.bounds = Rect {
                    x: tiny_sdk::LogicalPixels(bounds_x + dropdown_item_x),
                    y: tiny_sdk::LogicalPixels(bounds_y + item_y),
                    width: tiny_sdk::LogicalPixels(DROPDOWN_WIDTH - 30.0),
                    height: tiny_sdk::LogicalPixels(line_height),
                };

                let mut dropdown_item_view = TextView::from_text(&dropdown_text, dropdown_item_viewport);
                dropdown_item_view.update_layout(&font_service);

                let mut dropdown_item_glyphs = dropdown_item_view.collect_glyphs(&font_service);
                for glyph in &mut dropdown_item_glyphs {
                    glyph.token_id = if is_active { 0 } else { 255 };
                }
                collector.add_glyphs(dropdown_item_glyphs);

                // Close button for dropdown items
                let close_dropdown_x = dropdown_x - 20.0;
                let mut close_dropdown_viewport = Viewport::new(20.0, line_height, collector.viewport.scale_factor);
                collector.configure_viewport(&mut close_dropdown_viewport);
                close_dropdown_viewport.bounds = Rect {
                    x: tiny_sdk::LogicalPixels(bounds_x + close_dropdown_x),
                    y: tiny_sdk::LogicalPixels(bounds_y + item_y),
                    width: tiny_sdk::LogicalPixels(20.0),
                    height: tiny_sdk::LogicalPixels(line_height),
                };

                let mut close_dropdown_view = TextView::from_text("×", close_dropdown_viewport);
                close_dropdown_view.update_layout(&font_service);

                let mut close_dropdown_glyphs = close_dropdown_view.collect_glyphs(&font_service);
                for glyph in &mut close_dropdown_glyphs {
                    glyph.token_id = 3;
                }
                collector.add_glyphs(close_dropdown_glyphs);
            }
        }
    }

    /// Check if a click at the given position hits a tab
    pub fn hit_test_tab(
        &self,
        x: f32,
        y: f32,
        tab_manager: &TabManager,
        viewport_width: f32,
    ) -> Option<usize> {
        if y > self.height {
            return None;
        }

        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        let mut tab_x = 10.0 - self.scroll_offset;

        for idx in 0..num_tabs {
            if x >= tab_x && x < tab_x + tab_width {
                return Some(idx);
            }
            tab_x += tab_width;
        }

        None
    }

    /// Check if a click at the given position hits a close button
    pub fn hit_test_close_button(
        &self,
        x: f32,
        y: f32,
        tab_manager: &TabManager,
        viewport_width: f32,
    ) -> Option<usize> {
        if y > self.height {
            return None;
        }

        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        const CLOSE_BUTTON_WIDTH: f32 = 20.0;
        let mut tab_x = 10.0 - self.scroll_offset;

        for idx in 0..num_tabs {
            let close_x = tab_x + tab_width - CLOSE_BUTTON_WIDTH;
            if x >= close_x && x < close_x + CLOSE_BUTTON_WIDTH {
                return Some(idx);
            }
            tab_x += tab_width;
        }

        None
    }

    /// Check if a click hits the dropdown arrow
    pub fn hit_test_dropdown(&self, x: f32, y: f32, viewport_width: f32) -> bool {
        if y > self.height {
            return false;
        }

        let dropdown_x = viewport_width - 30.0;
        x >= dropdown_x && x < viewport_width
    }
}

// === Plugin Trait Implementation ===

tiny_sdk::plugin! {
    TabBarPlugin {
        name: "tab_bar",
        version: "1.0.0",
        z_index: 500,
        traits: [Init, Paint],
        defaults: [Init, Paint],
    }
}

impl Scrollable for TabBarPlugin {
    fn get_scroll(&self) -> Point {
        LayoutPos::new(self.scroll_offset, 0.0)
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.scroll_offset = scroll.x.0.max(0.0);
    }

    fn handle_scroll(&mut self, delta: Point, _viewport: &Viewport, _widget_bounds: Rect) -> bool {
        // For horizontal scrolling in tab bar
        let new_offset = self.scroll_offset - delta.x.0;

        // Clamp to positive values (max scroll will be handled by scroll_to_tab)
        self.scroll_offset = new_offset.max(0.0);

        true // Consumed the scroll event
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        let viewport_width = viewport.logical_size.width.0;
        // Return bounds representing scrollable content
        // This is approximate since we don't have tab count here
        LayoutRect::new(0.0, 0.0, viewport_width * 2.0, self.height)
    }
}
