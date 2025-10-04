//! Tab bar plugin - renders tabs at the top of the screen

use crate::scroll::Scrollable;
use crate::tab_manager::TabManager;
use crate::coordinates::Viewport;
use tiny_core::tree::{Point, Rect};
use tiny_font::{create_glyph_instances, SharedFontSystem};
use tiny_sdk::{
    Capability, Initializable, LayoutPos, PaintContext, Paintable, Plugin, PluginError,
    SetupContext,
};
use tiny_sdk::types::{LayoutRect, RectInstance};

/// Tab bar height in logical pixels
pub const TAB_BAR_HEIGHT: f32 = 30.0;
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

    /// Calculate total content width for all tabs
    fn calculate_total_width(&self, viewport_width: f32, num_tabs: usize) -> f32 {
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        tab_width * num_tabs as f32 + 40.0 // Include dropdown arrow space
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

    /// Collect background rectangles for tabs
    pub fn collect_rects(&self, tab_manager: &TabManager, viewport_width: f32) -> Vec<RectInstance> {
        let mut rects = Vec::new();
        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);
        let mut x_offset = 10.0 - self.scroll_offset;

        const TAB_HEIGHT: f32 = 30.0;

        // Colors (RGBA as u32) - match main background: rgb(0.11, 0.12, 0.13)
        // Active tab matches background (invisible/flush), inactive tabs are slightly darker
        const ACTIVE_TAB_BG: u32 = 0x1C1F21FF;   // Same as main background (28, 31, 33)
        const INACTIVE_TAB_BG: u32 = 0x16191BFF; // Slightly darker than background (22, 25, 27)

        for (idx, _tab) in tab_manager.tabs().iter().enumerate() {
            let is_active = idx == tab_manager.active_index();

            let tab_rect = RectInstance {
                rect: LayoutRect::new(x_offset, 0.0, tab_width, TAB_HEIGHT),
                color: if is_active { ACTIVE_TAB_BG } else { INACTIVE_TAB_BG },
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

    /// Collect glyphs for rendering tabs
    pub fn collect_glyphs(
        &self,
        collector: &mut crate::render::GlyphCollector,
        tab_manager: &TabManager,
    ) {
        // Get font service from service registry
        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let scale_factor = collector.viewport.scale_factor;
        let font_size = collector.viewport.font_size;
        let line_height = collector.viewport.line_height;

        // Extract widget bounds before loop to avoid borrow issues
        let bounds_x = collector.widget_viewport.as_ref().map(|w| w.bounds.x.0).unwrap_or(0.0);
        let bounds_y = collector.widget_viewport.as_ref().map(|w| w.bounds.y.0).unwrap_or(0.0);
        let viewport_width = collector.viewport.logical_size.width.0;

        let num_tabs = tab_manager.tabs().len();
        let tab_width = self.calculate_tab_width(viewport_width, num_tabs);

        let mut glyphs = Vec::new();
        let mut x_offset = 10.0 - self.scroll_offset;

        const TAB_PADDING: f32 = 10.0;
        const CLOSE_BUTTON_WIDTH: f32 = 20.0;

        // Render each tab in widget-local coordinates
        for (idx, tab) in tab_manager.tabs().iter().enumerate() {
            let is_active = idx == tab_manager.active_index();

            // Calculate available space for text (tab width - padding - close button)
            let text_width = tab_width - TAB_PADDING * 2.0 - CLOSE_BUTTON_WIDTH;
            let max_chars = (text_width / (font_size * 0.6)) as usize; // Rough estimate

            // Tab text (truncate if too long)
            let mut display_name = tab.display_name.clone();

            // Add modified indicator
            if tab.is_modified() {
                display_name.push_str(" •");
            }

            // Truncate if needed
            if display_name.len() > max_chars {
                let truncate_len = max_chars.saturating_sub(3);
                display_name.truncate(truncate_len);
                display_name.push_str("...");
            }

            // Calculate text width for centering (rough estimate: font_size * 0.6 per char)
            let text_width = display_name.len() as f32 * font_size * 0.6;
            let available_space = tab_width - CLOSE_BUTTON_WIDTH;
            let text_x = x_offset + (available_space - text_width) / 2.0;

            let tab_pos = LayoutPos::new(text_x, 5.0); // Widget-local Y

            let tab_glyphs = create_glyph_instances(
                &font_service,
                &display_name,
                tab_pos,
                font_size,
                scale_factor,
                line_height,
                None,
                if is_active { 2 } else { 255 }, // Active: token 2, Inactive: token 255 (dimmed like line numbers)
            );

            glyphs.extend(tab_glyphs);

            // Close button "×" at the right side of each tab
            let close_x = x_offset + tab_width - CLOSE_BUTTON_WIDTH;
            let close_pos = LayoutPos::new(close_x, 5.0); // Widget-local Y
            let close_glyphs = create_glyph_instances(
                &font_service,
                "×",
                close_pos,
                font_size,
                scale_factor,
                line_height,
                None,
                3, // Different token for close button
            );

            glyphs.extend(close_glyphs);

            x_offset += tab_width; // Move to next tab position
        }

        // Dropdown arrow on the far right (always visible)
        let dropdown_x = viewport_width - 30.0;
        let dropdown_pos = LayoutPos::new(dropdown_x, 5.0); // Widget-local Y
        let dropdown_glyphs = create_glyph_instances(
            &font_service,
            "▼",
            dropdown_pos,
            font_size,
            scale_factor,
            line_height,
            None,
            4, // Token for dropdown arrow
        );

        glyphs.extend(dropdown_glyphs);

        // Render dropdown menu if open
        if self.dropdown_open {
            let dropdown_start_y = self.height;

            for (idx, tab) in tab_manager.tabs().iter().enumerate() {
                let item_y = dropdown_start_y + (idx as f32 * line_height) - self.dropdown_scroll_offset;

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

                let dropdown_item_pos = LayoutPos::new(dropdown_x - DROPDOWN_WIDTH + 10.0, item_y);

                let dropdown_item_glyphs = create_glyph_instances(
                    &font_service,
                    &dropdown_text,
                    dropdown_item_pos,
                    font_size,
                    scale_factor,
                    line_height,
                    None,
                    if is_active { 2 } else { 255 }, // Match tab bar styling
                );

                glyphs.extend(dropdown_item_glyphs);

                // Close button for dropdown items
                let close_dropdown_x = dropdown_x - 20.0;
                let close_dropdown_pos = LayoutPos::new(close_dropdown_x, item_y);
                let close_dropdown_glyphs = create_glyph_instances(
                    &font_service,
                    "×",
                    close_dropdown_pos,
                    font_size,
                    scale_factor,
                    line_height,
                    None,
                    3,
                );

                glyphs.extend(close_dropdown_glyphs);
            }
        }

        // Convert to screen coordinates (like line numbers plugin does)
        for mut g in glyphs {
            // Transform from widget-local space to screen space
            let screen_x = g.pos.x.0 + bounds_x;
            let screen_y = g.pos.y.0 + bounds_y;
            // Convert to physical coordinates
            g.pos = LayoutPos::new(screen_x * scale_factor, screen_y * scale_factor);
            collector.add_glyphs(vec![g]);
        }
    }

    /// Check if a click at the given position hits a tab
    pub fn hit_test_tab(&self, x: f32, y: f32, tab_manager: &TabManager, viewport_width: f32) -> Option<usize> {
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
    pub fn hit_test_close_button(&self, x: f32, y: f32, tab_manager: &TabManager, viewport_width: f32) -> Option<usize> {
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

impl Plugin for TabBarPlugin {
    fn name(&self) -> &str {
        "tab_bar"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("tab_bar".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for TabBarPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Paintable for TabBarPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {
        // Tab bar uses collect_glyphs for batched rendering
        // This method is kept for plugin trait compatibility
    }

    fn z_index(&self) -> i32 {
        500 // Render above editor content but below file picker
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