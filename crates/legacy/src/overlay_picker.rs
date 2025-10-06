//! Generic overlay picker - unified implementation for file picker, grep, command palette, etc.

use crate::coordinates::Viewport;
use crate::filterable_dropdown::FilterableDropdown;
use crate::{scroll::Scrollable, Widget};
use std::sync::Arc;
use tiny_core::tree::{Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{Capability, Initializable, PaintContext, Paintable, Plugin, PluginError, SetupContext};

/// Generic overlay picker for searchable lists
pub struct OverlayPicker<T: Clone + Send + Sync + 'static> {
    /// Filterable dropdown for search + results
    pub dropdown: FilterableDropdown<T>,
    /// Cached items (thread-safe)
    pub cached_items: Arc<parking_lot::RwLock<Vec<T>>>,
    /// Search function: query -> filtered results
    search_fn: Arc<dyn Fn(&str, &[T]) -> Vec<T> + Send + Sync>,
    /// Whether filtering is in progress
    filtering: bool,
    /// Visibility state
    pub visible: bool,
}

impl<T: Clone + Send + Sync + 'static> OverlayPicker<T> {
    /// Create a new overlay picker
    pub fn new(
        format_fn: impl Fn(&T) -> String + Send + Sync + 'static,
        search_fn: impl Fn(&str, &[T]) -> Vec<T> + Send + Sync + 'static,
    ) -> Self {
        Self {
            dropdown: FilterableDropdown::new(format_fn),
            cached_items: Arc::new(parking_lot::RwLock::new(Vec::new())),
            search_fn: Arc::new(search_fn),
            filtering: false,
            visible: false,
        }
    }

    /// Set the available items (non-blocking)
    pub fn set_items(&mut self, items: Vec<T>) {
        // Use try_write to avoid blocking main thread if background thread is writing
        if let Some(mut cached) = self.cached_items.try_write() {
            *cached = items;
        }
    }

    /// Show with title
    pub fn show_with_title(&mut self, items: Vec<T>, title: &str) {
        self.visible = true;
        self.filtering = false;
        // Use try_write to avoid blocking main thread
        if let Some(mut cached) = self.cached_items.try_write() {
            *cached = items.clone();
        }
        self.dropdown.show_with_title(items, title);
    }

    /// Show with default title
    pub fn show(&mut self) {
        let items = self.cached_items.read().clone();
        self.show_with_title(items, "");
    }

    /// Hide the picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.dropdown.hide();
        self.filtering = false;
    }

    /// Trigger search/filter
    pub fn trigger_filter(&mut self, query: String) {
        if query.is_empty() {
            self.filtering = false;
            let items = self.cached_items.read().clone();
            self.dropdown.set_items(items);
            return;
        }

        self.filtering = true;
        let items = self.cached_items.read().clone();
        let filtered = (self.search_fn)(&query, &items);
        self.dropdown.set_items(filtered);
        self.filtering = false;
    }

    /// Calculate bounds based on viewport
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        self.dropdown.calculate_bounds(viewport);
    }

    /// Get current bounds
    pub fn get_bounds(&self) -> Rect {
        self.dropdown.bounds()
    }

    /// Collect glyphs for rendering
    /// TextViews now cache glyphs internally - this is cheap to call every frame
    pub fn collect_glyphs(&mut self, font_system: &Arc<SharedFontSystem>) -> Vec<(Vec<tiny_sdk::GlyphInstance>, (u32, u32, u32, u32))> {
        if !self.visible { return Vec::new(); }

        // Update layout (cheap - TextView caches and returns early if unchanged)
        self.dropdown.title_view.update_layout(font_system);
        self.dropdown.input.view.update_layout(font_system);
        self.dropdown.results.update_layout(font_system);

        let mut result = Vec::new();
        if !self.dropdown.title_view.text().is_empty() {
            let glyphs = self.dropdown.title_view.collect_glyphs(font_system);
            if !glyphs.is_empty() {
                result.push((glyphs, self.dropdown.title_view.get_scissor_rect()));
            }
        }

        let input_glyphs = self.dropdown.input.view.collect_glyphs(font_system);
        if !input_glyphs.is_empty() {
            result.push((input_glyphs, self.dropdown.input.view.get_scissor_rect()));
        }

        let results_glyphs = self.dropdown.results.collect_glyphs(font_system);
        if !results_glyphs.is_empty() {
            result.push((results_glyphs, self.dropdown.results.get_scissor_rect()));
        }

        result
    }

    /// Collect background rects
    pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        if !self.visible { return Vec::new(); }
        let mut rects = self.dropdown.get_chrome_rects();

        // Add selection highlight rect (must be before input rects to render behind text)
        if let Some(highlight_rect) = self.dropdown.get_selection_highlight_rect() {
            rects.push(highlight_rect);
        }

        rects.extend(self.dropdown.input.collect_background_rects());
        rects
    }

    /// Get frame rounded rect
    pub fn get_frame_rounded_rect(&self) -> Option<tiny_sdk::types::RoundedRectInstance> {
        self.dropdown.get_frame_rounded_rect()
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        self.dropdown.move_selection_up();
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        self.dropdown.move_selection_down();
    }

    pub fn selected_item(&self) -> Option<&T> {
        self.dropdown.items().get(self.dropdown.selected_index())
    }

    pub fn items(&self) -> &[T] {
        self.dropdown.items()
    }

    /// Handle mouse hover - updates selection
    pub fn handle_hover(&mut self, x: f32, y: f32) -> bool {
        self.dropdown.handle_hover(x, y)
    }

    /// Handle mouse click
    pub fn handle_click(&mut self, x: f32, y: f32, shift: bool) -> crate::filterable_dropdown::DropdownAction<T> {
        self.dropdown.handle_click(x, y, shift)
    }
}

impl<T: Clone + Send + Sync + 'static> Scrollable for OverlayPicker<T> {
    fn get_scroll(&self) -> Point {
        self.dropdown.results.get_scroll()
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.dropdown.results.set_scroll(scroll);
    }

    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        self.dropdown.results.handle_scroll(delta, viewport, widget_bounds)
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        self.dropdown.results.get_content_bounds(viewport)
    }
}

impl<T: Clone + Send + Sync + 'static> Widget for OverlayPicker<T> {
    fn calculate_bounds(&mut self, viewport: &Viewport) {
        self.dropdown.calculate_bounds(viewport);
    }

    fn get_bounds(&self) -> Rect {
        self.dropdown.bounds()
    }

    fn collect_glyphs(&mut self, font_system: &Arc<SharedFontSystem>) -> Vec<(Vec<tiny_sdk::GlyphInstance>, (u32, u32, u32, u32))> {
        if !self.visible { return Vec::new(); }

        self.dropdown.title_view.update_layout(font_system);
        self.dropdown.input.view.update_layout(font_system);
        self.dropdown.results.update_layout(font_system);

        let mut result = Vec::new();
        if !self.dropdown.title_view.text().is_empty() {
            let glyphs = self.dropdown.title_view.collect_glyphs(font_system);
            if !glyphs.is_empty() {
                result.push((glyphs, self.dropdown.title_view.get_scissor_rect()));
            }
        }

        let input_glyphs = self.dropdown.input.view.collect_glyphs(font_system);
        if !input_glyphs.is_empty() {
            result.push((input_glyphs, self.dropdown.input.view.get_scissor_rect()));
        }

        let results_glyphs = self.dropdown.results.collect_glyphs(font_system);
        if !results_glyphs.is_empty() {
            result.push((results_glyphs, self.dropdown.results.get_scissor_rect()));
        }

        result
    }

    fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        if !self.visible { return Vec::new(); }
        let mut rects = self.dropdown.get_chrome_rects();

        // Add selection highlight rect (must be before input rects to render behind text)
        if let Some(highlight_rect) = self.dropdown.get_selection_highlight_rect() {
            rects.push(highlight_rect);
        }

        rects.extend(self.dropdown.input.collect_background_rects());
        rects
    }

    fn get_frame_rounded_rect(&self) -> Option<tiny_sdk::types::RoundedRectInstance> {
        self.dropdown.get_frame_rounded_rect()
    }

    fn is_visible(&self) -> bool {
        self.visible
    }
}

impl<T: Clone + Send + Sync + 'static> Plugin for OverlayPicker<T> {
    fn name(&self) -> &str {
        "overlay_picker"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Initializable, Capability::Paintable("overlay_picker".to_string())]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl<T: Clone + Send + Sync + 'static> Initializable for OverlayPicker<T> {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl<T: Clone + Send + Sync + 'static> Paintable for OverlayPicker<T> {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {}

    fn z_index(&self) -> i32 {
        1000
    }
}
