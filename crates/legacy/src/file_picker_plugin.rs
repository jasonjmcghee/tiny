//! File picker plugin - searchable file list with fuzzy filtering

use crate::coordinates::Viewport;
use crate::filterable_dropdown::{DropdownAction, FilterableDropdown};
use crate::input_types::{Key, Modifiers};
use crate::scroll::Scrollable;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_core::tree::{Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{
    Capability, Initializable, PaintContext, Paintable, Plugin, PluginError, SetupContext,
};

/// File picker plugin for finding and opening files
pub struct FilePickerPlugin {
    /// Filterable dropdown for search + results
    dropdown: FilterableDropdown<PathBuf>,

    /// All files in working directory (thread-safe)
    all_files: Arc<RwLock<Vec<PathBuf>>>,

    /// Whether filtering is in progress
    filtering: bool,

    /// Callback when file is selected
    on_select: Option<Box<dyn Fn(&Path) + Send + Sync>>,

    /// Public visibility field for backwards compatibility
    pub visible: bool,
}

impl FilePickerPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let all_files = Arc::new(RwLock::new(Vec::new()));

        // Format function for displaying file paths
        let working_dir_for_format = working_dir.clone();
        let format_fn = move |path: &PathBuf| {
            path.strip_prefix(&working_dir_for_format)
                .ok()
                .and_then(|p| p.to_str())
                .unwrap_or_else(|| path.to_str().unwrap_or("???"))
                .to_string()
        };

        // Spawn background thread to scan directory
        let all_files_clone = all_files.clone();
        let working_dir_clone = working_dir.clone();
        std::thread::spawn(move || {
            let scanned = Self::scan_directory(&working_dir_clone);
            *all_files_clone.write() = scanned;
        });

        Self {
            dropdown: FilterableDropdown::new(format_fn),
            all_files,
            filtering: false,
            on_select: None,
            visible: false,
        }
    }

    /// Scan directory for files using ignore crate (respects .gitignore)
    fn scan_directory(dir: &Path) -> Vec<PathBuf> {
        use ignore::WalkBuilder;

        let mut files: Vec<PathBuf> = WalkBuilder::new(dir)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .build()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|ft| ft.is_file()).unwrap_or(false))
            .map(|entry| entry.into_path())
            .collect();

        files.sort();
        files
    }

    /// Set callback for when file is selected
    pub fn set_on_select<F>(&mut self, callback: F)
    where
        F: Fn(&Path) + Send + Sync + 'static,
    {
        self.on_select = Some(Box::new(callback));
    }

    /// Show the file picker
    pub fn show(&mut self) {
        self.visible = true;
        self.filtering = false;

        // Show dropdown with all files (unfiltered initially)
        let files = self.all_files.read().clone();
        self.dropdown.show_with_title(files, "Open File");
    }

    /// Hide the file picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.dropdown.hide();
        self.filtering = false;
    }

    /// Check if picker is visible
    pub fn is_visible(&self) -> bool {
        self.dropdown.visible
    }

    /// Trigger filtering based on query
    fn trigger_filter(&mut self, query: String) {
        if query.is_empty() {
            // Show all files when no query
            self.filtering = false;
            let files = self.all_files.read().clone();
            self.dropdown.set_items(files);
            return;
        }

        self.filtering = true;

        // Simple substring filtering with scoring
        let all_files = self.all_files.read();
        let query_lower = query.to_lowercase();

        let mut results: Vec<(PathBuf, u32)> = all_files
            .iter()
            .filter_map(|path| {
                path.to_str().and_then(|s| {
                    let s_lower = s.to_lowercase();
                    if s_lower.contains(&query_lower) {
                        // Score: earlier match is better
                        let score = (1000 - s_lower.find(&query_lower).unwrap_or(999)) as u32;
                        Some((path.clone(), score))
                    } else {
                        None
                    }
                })
            })
            .collect();

        // Sort by score (higher is better)
        results.sort_by(|a, b| b.1.cmp(&a.1));

        // Extract just the paths
        let filtered: Vec<PathBuf> = results.into_iter().map(|(path, _)| path).collect();

        self.dropdown.set_items(filtered);
        self.filtering = false;
    }

    /// Handle keyboard input
    pub fn handle_key(&mut self, key: &Key, modifiers: &Modifiers, viewport: &Viewport) -> bool {
        let action = self.dropdown.handle_key(key, modifiers, viewport);

        match action {
            DropdownAction::Continue => true,
            DropdownAction::Selected(path) => {
                if let Some(callback) = &self.on_select {
                    callback(path.as_path());
                }
                self.hide();
                true
            }
            DropdownAction::Cancelled => {
                self.hide();
                true
            }
            DropdownAction::FilterChanged(new_filter) => {
                self.trigger_filter(new_filter);
                true
            }
        }
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

    // === Legacy API compatibility ===

    /// Add character to query
    pub fn add_char(&mut self, ch: char) {
        self.dropdown.input.handle_char(ch);
        self.trigger_filter(self.dropdown.filter_text());
    }

    /// Handle backspace
    pub fn backspace(&mut self) {
        self.dropdown.input.handle_backspace();
        self.trigger_filter(self.dropdown.filter_text());
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.dropdown.selected_index() > 0 {
            let viewport = Viewport::new(1920.0, 1080.0, 1.0);
            let modifiers = Modifiers::new();
            self.dropdown.handle_key(
                &Key::Named(crate::input_types::NamedKey::ArrowUp),
                &modifiers,
                &viewport,
            );
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        let viewport = Viewport::new(1920.0, 1080.0, 1.0);
        let modifiers = Modifiers::new();
        self.dropdown.handle_key(
            &Key::Named(crate::input_types::NamedKey::ArrowDown),
            &modifiers,
            &viewport,
        );
    }

    /// Get selected file
    pub fn selected_file(&self) -> Option<&Path> {
        let idx = self.dropdown.selected_index();
        self.dropdown.items().get(idx).map(|p| p.as_path())
    }

    /// Set query (for testing/compatibility)
    pub fn set_query(&mut self, query: String) {
        self.dropdown.input.set_text(&query);
        self.trigger_filter(query);
    }
}

// Plugin trait implementations
impl Plugin for FilePickerPlugin {
    fn name(&self) -> &str {
        "file_picker"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Initializable,
            Capability::Paintable("file_picker".to_string()),
        ]
    }

    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> {
        Some(self)
    }

    fn as_paintable(&self) -> Option<&dyn Paintable> {
        Some(self)
    }
}

impl Initializable for FilePickerPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> {
        Ok(())
    }
}

impl Paintable for FilePickerPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {}

    fn z_index(&self) -> i32 {
        1000
    }
}

impl Scrollable for FilePickerPlugin {
    fn get_scroll(&self) -> Point {
        self.dropdown.results.get_scroll()
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.dropdown.results.set_scroll(scroll);
    }

    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        self.dropdown
            .results
            .handle_scroll(delta, viewport, widget_bounds)
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        self.dropdown.results.get_content_bounds(viewport)
    }
}
