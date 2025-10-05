//! Grep plugin - full codebase search with fuzzy filtering

use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;
use tiny_font::SharedFontSystem;
use tiny_sdk::{
    Capability, Initializable, PaintContext, Paintable, Plugin,
    PluginError, SetupContext,
};
use tiny_core::tree::{Point, Rect};
use crate::scroll::Scrollable;
use crate::coordinates::Viewport;
use crate::filterable_dropdown::{FilterableDropdown, DropdownAction};
use crate::input_types::{Key, Modifiers};

/// A single grep result
#[derive(Clone, Debug)]
pub struct GrepResult {
    pub file_path: PathBuf,
    pub line_number: usize,
    pub column: usize,
    pub line_content: String,
}

/// Grep plugin for full codebase search
pub struct GrepPlugin {
    /// Filterable dropdown for search + results
    dropdown: FilterableDropdown<GrepResult>,

    /// All grep results (thread-safe)
    all_results: Arc<RwLock<Vec<GrepResult>>>,

    /// Working directory
    working_dir: PathBuf,

    /// Whether search is in progress
    searching: bool,

    /// Callback when result is selected
    on_select: Option<Box<dyn Fn(&GrepResult) + Send + Sync>>,

    /// Public visibility field for backwards compatibility
    pub visible: bool,
}

impl GrepPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let all_results = Arc::new(RwLock::new(Vec::new()));

        // Format function for displaying grep results
        let format_fn = |result: &GrepResult| {
            let relative_path = result.file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("???");
            format!("{}:{}  {}", relative_path, result.line_number, result.line_content)
        };

        Self {
            dropdown: FilterableDropdown::new(format_fn),
            all_results,
            working_dir,
            searching: false,
            on_select: None,
            visible: false,
        }
    }

    /// Set callback for when result is selected
    pub fn set_on_select<F>(&mut self, callback: F)
    where
        F: Fn(&GrepResult) + Send + Sync + 'static,
    {
        self.on_select = Some(Box::new(callback));
    }

    /// Show the grep picker and start searching
    pub fn show(&mut self, search_term: String) {
        self.visible = true;
        self.searching = true;

        // Clear results and start searching if query is not empty
        if !search_term.is_empty() {
            let all_results = self.all_results.clone();
            let working_dir = self.working_dir.clone();
            let query = search_term.clone();
            std::thread::spawn(move || {
                let results = Self::search_codebase(&working_dir, &query);
                *all_results.write() = results;
            });
        } else {
            *self.all_results.write() = Vec::new();
        }

        // Show dropdown with initial results (empty or from cache)
        let results = self.all_results.read().clone();
        self.dropdown.show_with_title(results, "Search in Files");

        // Set initial filter text
        if !search_term.is_empty() {
            self.dropdown.input.set_text(&search_term);
        }
    }

    /// Hide the grep picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.dropdown.hide();
        self.searching = false;
    }

    /// Check if picker is visible
    pub fn is_visible(&self) -> bool {
        self.dropdown.visible
    }

    /// Trigger a new search with the current query
    fn trigger_search(&mut self, query: String) {
        if query.is_empty() {
            self.searching = false;
            *self.all_results.write() = Vec::new();
            self.dropdown.set_items(Vec::new());
            return;
        }

        self.searching = true;
        let all_results = self.all_results.clone();
        let working_dir = self.working_dir.clone();
        std::thread::spawn(move || {
            let results = Self::search_codebase(&working_dir, &query);
            *all_results.write() = results;
        });

        // Update with current results (will be updated by thread)
        let results = self.all_results.read().clone();
        self.dropdown.set_items(results);
    }

    /// Handle keyboard input
    pub fn handle_key(&mut self, key: &Key, modifiers: &Modifiers, viewport: &Viewport) -> bool {
        let action = self.dropdown.handle_key(key, modifiers, viewport);

        match action {
            DropdownAction::Continue => true,
            DropdownAction::Selected(result) => {
                if let Some(callback) = &self.on_select {
                    callback(&result);
                }
                self.hide();
                true
            }
            DropdownAction::Cancelled => {
                self.hide();
                true
            }
            DropdownAction::FilterChanged(new_filter) => {
                self.trigger_search(new_filter);
                true
            }
        }
    }

    /// Search codebase using ripgrep via ignore crate
    fn search_codebase(dir: &Path, search_term: &str) -> Vec<GrepResult> {
        use ignore::WalkBuilder;
        use std::io::BufRead;

        let mut results = Vec::new();
        let search_lower = search_term.to_lowercase();

        for entry in WalkBuilder::new(dir)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        {
            let path = entry.path();

            // Skip binary files (heuristic: check extension)
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_str().unwrap_or("");
                if matches!(ext_str, "png" | "jpg" | "jpeg" | "gif" | "ico" | "pdf" | "zip" | "tar" | "gz") {
                    continue;
                }
            }

            // Read file and search line by line
            if let Ok(file) = std::fs::File::open(path) {
                let reader = std::io::BufReader::new(file);
                for (line_idx, line) in reader.lines().enumerate() {
                    if let Ok(line_content) = line {
                        // Case-insensitive search
                        if let Some(col) = line_content.to_lowercase().find(&search_lower) {
                            results.push(GrepResult {
                                file_path: path.to_path_buf(),
                                line_number: line_idx + 1, // 1-indexed
                                column: col,
                                line_content: line_content.trim().to_string(),
                            });
                        }
                    }
                }
            }

            // Limit results to prevent memory issues
            if results.len() > 10000 {
                break;
            }
        }

        results
    }

    /// Calculate bounds based on viewport
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        self.dropdown.calculate_bounds(viewport);
    }

    /// Get current bounds
    pub fn get_bounds(&self) -> Rect {
        self.dropdown.bounds()
    }

    /// Collect glyphs for rendering (with scissor rects for each view)
    pub fn collect_glyphs(
        &mut self,
        font_system: &Arc<SharedFontSystem>,
    ) -> Vec<(Vec<tiny_sdk::GlyphInstance>, (u32, u32, u32, u32))> {
        let mut result = Vec::new();

        if !self.dropdown.visible {
            return result;
        }

        // Update layout for all views (bounds already set by calculate_bounds)
        self.dropdown.title_view.update_layout(font_system);
        self.dropdown.input.view.update_layout(font_system);
        self.dropdown.results.update_layout(font_system);

        // Collect glyphs from title with its scissor rect
        if !self.dropdown.title_view.text().is_empty() {
            let title_glyphs = self.dropdown.title_view.collect_glyphs(font_system);
            let scissor = self.dropdown.title_view.get_scissor_rect();
            if !title_glyphs.is_empty() {
                result.push((title_glyphs, scissor));
            }
        }

        // Collect glyphs from input with its scissor rect
        let input_glyphs = self.dropdown.input.view.collect_glyphs(font_system);
        let input_scissor = self.dropdown.input.view.get_scissor_rect();
        if !input_glyphs.is_empty() {
            result.push((input_glyphs, input_scissor));
        }

        // Collect glyphs from results with its scissor rect
        let results_glyphs = self.dropdown.results.collect_glyphs(font_system);
        let results_scissor = self.dropdown.results.get_scissor_rect();
        if !results_glyphs.is_empty() {
            result.push((results_glyphs, results_scissor));
        }

        result
    }

    /// Collect background rects for rendering (includes line highlight)
    pub fn collect_background_rects(&self) -> Vec<tiny_sdk::types::RectInstance> {
        let mut rects = Vec::new();

        if !self.dropdown.visible {
            return rects;
        }

        // Collect background chrome (input and results backgrounds)
        let chrome_rects = self.dropdown.get_chrome_rects();
        rects.extend(chrome_rects);

        // Collect input background rects (cursor)
        let input_rects = self.dropdown.input.collect_background_rects();
        rects.extend(input_rects);

        // Collect results background rects (selection highlight)
        let results_rects = self.dropdown.results.collect_background_rects();
        rects.extend(results_rects);

        rects
    }

    /// Get rounded rect for frame with border (SDF rendering)
    pub fn get_frame_rounded_rect(&self) -> Option<tiny_sdk::types::RoundedRectInstance> {
        self.dropdown.get_frame_rounded_rect()
    }

    /// Poll for updated search results (call from update loop)
    pub fn poll_results(&mut self) {
        if self.searching {
            let results = self.all_results.read().clone();
            if !results.is_empty() {
                self.dropdown.set_items(results);
                self.searching = false;
            }
        }
    }

    // === Legacy API compatibility ===

    /// Add character to search input
    pub fn add_char(&mut self, ch: char) {
        self.dropdown.input.handle_char(ch);
        self.trigger_search(self.dropdown.filter_text());
    }

    /// Handle backspace
    pub fn backspace(&mut self) {
        self.dropdown.input.handle_backspace();
        self.trigger_search(self.dropdown.filter_text());
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.dropdown.selected_index() > 0 {
            let viewport = Viewport::new(1920.0, 1080.0, 1.0);
            let modifiers = Modifiers::new();
            self.dropdown.handle_key(&Key::Named(crate::input_types::NamedKey::ArrowUp), &modifiers, &viewport);
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        let viewport = Viewport::new(1920.0, 1080.0, 1.0);
        let modifiers = Modifiers::new();
        self.dropdown.handle_key(&Key::Named(crate::input_types::NamedKey::ArrowDown), &modifiers, &viewport);
    }

    /// Get selected result
    pub fn selected_result(&self) -> Option<&GrepResult> {
        let idx = self.dropdown.selected_index();
        self.dropdown.items().get(idx)
    }
}

// Plugin trait implementations
impl Plugin for GrepPlugin {
    fn name(&self) -> &str {
        "grep"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("grep".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for GrepPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Paintable for GrepPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {
    }

    fn z_index(&self) -> i32 {
        1000
    }
}

impl Scrollable for GrepPlugin {
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
