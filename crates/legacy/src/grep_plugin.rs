//! Grep plugin - full codebase search

use crate::{overlay_picker::OverlayPicker, scroll::Scrollable, Widget};
use crate::coordinates::Viewport;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_core::tree::{Point, Rect};
use tiny_font::SharedFontSystem;
use tiny_sdk::{Capability, Initializable, PaintContext, Paintable, Plugin, PluginError, SetupContext};

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
    picker: OverlayPicker<GrepResult>,
    working_dir: PathBuf,
    searching: bool,
    pub visible: bool,
}

impl GrepPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Format function
        let format_fn = |result: &GrepResult| {
            let name = result.file_path.file_name().and_then(|n| n.to_str()).unwrap_or("???");
            format!("{}:{}  {}", name, result.line_number, result.line_content)
        };

        // Search function (empty - results come from background thread)
        let search_fn = |_query: &str, items: &[GrepResult]| items.to_vec();

        Self {
            picker: OverlayPicker::new(format_fn, search_fn),
            working_dir,
            searching: false,
            visible: false,
        }
    }

    pub fn show(&mut self, search_term: String) {
        self.visible = true;
        self.searching = !search_term.is_empty();

        if !search_term.is_empty() {
            let cached = self.picker.cached_items.clone();
            let wd = self.working_dir.clone();
            let q = search_term.clone();
            std::thread::spawn(move || *cached.write() = Self::search_codebase(&wd, &q));
        }

        let results = self.picker.cached_items.read().clone();
        self.picker.show_with_title(results, "Search in Files");
        if !search_term.is_empty() {
            self.picker.dropdown.input.set_text(&search_term);
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.searching = false;
        self.picker.hide();
    }

    fn trigger_search(&mut self, query: String) {
        if query.is_empty() {
            self.searching = false;
            *self.picker.cached_items.write() = Vec::new();
            self.picker.dropdown.set_items(Vec::new());
            return;
        }

        self.searching = true;
        let cached = self.picker.cached_items.clone();
        let wd = self.working_dir.clone();
        std::thread::spawn(move || *cached.write() = Self::search_codebase(&wd, &query));

        let results = self.picker.cached_items.read().clone();
        self.picker.dropdown.set_items(results);
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
                if matches!(
                    ext_str,
                    "png" | "jpg" | "jpeg" | "gif" | "ico" | "pdf" | "zip" | "tar" | "gz"
                ) {
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

    pub fn poll_results(&mut self) {
        if self.searching {
            let results = self.picker.cached_items.read().clone();
            if !results.is_empty() {
                self.picker.dropdown.set_items(results);
                self.searching = false;
            }
        }
    }

    pub fn add_char(&mut self, ch: char) {
        self.picker.dropdown.input.handle_char(ch);
        self.trigger_search(self.picker.dropdown.filter_text());
    }
    pub fn backspace(&mut self) {
        self.picker.dropdown.input.handle_backspace();
        self.trigger_search(self.picker.dropdown.filter_text());
    }
    pub fn move_up(&mut self) { self.picker.move_up(); }
    pub fn move_down(&mut self) { self.picker.move_down(); }
    pub fn selected_result(&self) -> Option<&GrepResult> { self.picker.selected_item() }
}

impl Plugin for GrepPlugin {
    fn name(&self) -> &str { "grep" }
    fn version(&self) -> &str { "1.0.0" }
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::Initializable, Capability::Paintable("grep".to_string())]
    }
    fn as_initializable(&mut self) -> Option<&mut dyn Initializable> { Some(self) }
    fn as_paintable(&self) -> Option<&dyn Paintable> { Some(self) }
}

impl Initializable for GrepPlugin {
    fn setup(&mut self, _ctx: &mut SetupContext) -> Result<(), PluginError> { Ok(()) }
}

impl Paintable for GrepPlugin {
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {}
    fn z_index(&self) -> i32 { 1000 }
}

impl Scrollable for GrepPlugin {
    fn get_scroll(&self) -> Point { self.picker.get_scroll() }
    fn set_scroll(&mut self, scroll: Point) { self.picker.set_scroll(scroll); }
    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        self.picker.handle_scroll(delta, viewport, widget_bounds)
    }
    fn get_content_bounds(&self, viewport: &Viewport) -> Rect { self.picker.get_content_bounds(viewport) }
}

tiny_ui::impl_widget_delegate!(GrepPlugin, picker);
