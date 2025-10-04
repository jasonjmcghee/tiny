//! Grep plugin - full codebase search with fuzzy filtering

use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;
use tiny_font::{create_glyph_instances, SharedFontSystem};
use tiny_sdk::{
    Capability, GlyphInstance, Initializable, LayoutPos, PaintContext, Paintable, Plugin,
    PluginError, SetupContext,
};
use tiny_core::tree::{Point, Rect};
use crate::scroll::Scrollable;
use crate::coordinates::Viewport;
use tiny_sdk::LogicalPixels;
use nucleo::{Config, Nucleo, Utf32String};

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
    /// Whether the picker is visible
    pub visible: bool,
    /// Current search query
    pub query: String,
    /// All grep results (thread-safe)
    all_results: Arc<RwLock<Vec<GrepResult>>>,
    /// Filtered results based on query (with scores)
    filtered_results: Vec<(GrepResult, u32)>,
    /// Selected index in filtered list
    selected_index: usize,
    /// Working directory
    working_dir: PathBuf,
    /// Scroll position for long result lists
    scroll_position: Point,
    /// Bounds for overlay rendering
    bounds: Rect,
    /// Cached width to keep picker size consistent while filtering
    cached_width: Option<f32>,
    /// Nucleo matcher for fuzzy matching results
    matcher: Nucleo<GrepResult>,
    /// Whether search is in progress
    searching: bool,
}

impl GrepPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let all_results = Arc::new(RwLock::new(Vec::new()));

        // Create nucleo matcher
        let matcher = Nucleo::new(
            Config::DEFAULT,
            Arc::new(|| {}),
            None,
            1, // one column (display string)
        );

        Self {
            visible: false,
            query: String::new(),
            all_results,
            filtered_results: Vec::new(),
            selected_index: 0,
            working_dir,
            scroll_position: Point::default(),
            bounds: Rect::default(),
            cached_width: None,
            matcher,
            searching: false,
        }
    }

    /// Show the grep picker and start searching
    pub fn show(&mut self, search_term: String) {
        self.visible = true;
        self.query = search_term;
        self.scroll_position = Point::default();
        self.bounds = Rect::default();
        self.cached_width = None;

        // Clear results and start searching if query is not empty
        if !self.query.is_empty() {
            self.searching = true;
            let all_results = self.all_results.clone();
            let working_dir = self.working_dir.clone();
            let query = self.query.clone();
            std::thread::spawn(move || {
                let results = Self::search_codebase(&working_dir, &query);
                *all_results.write() = results;
            });
        } else {
            self.searching = false;
            *self.all_results.write() = Vec::new();
        }

        self.update_results();
    }

    /// Trigger a new search with the current query
    fn trigger_search(&mut self) {
        if self.query.is_empty() {
            self.searching = false;
            *self.all_results.write() = Vec::new();
            self.update_results();
            return;
        }

        self.searching = true;
        let all_results = self.all_results.clone();
        let working_dir = self.working_dir.clone();
        let query = self.query.clone();
        std::thread::spawn(move || {
            let results = Self::search_codebase(&working_dir, &query);
            *all_results.write() = results;
        });
        self.update_results();
    }

    /// Hide the grep picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.query.clear();
        self.searching = false;
    }

    /// Update search query
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.trigger_search();
    }

    /// Add character to search query
    pub fn add_char(&mut self, ch: char) {
        self.query.push(ch);
        self.trigger_search();
    }

    /// Remove last character from search query
    pub fn backspace(&mut self) {
        self.query.pop();
        self.trigger_search();
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

    /// Update display results (no filtering, just show all search results)
    fn update_results(&mut self) {
        let all_results = self.all_results.read();
        self.filtered_results = all_results.iter().map(|r| (r.clone(), 0)).collect();
        self.selected_index = 0;
    }

    /// Format a result for display
    fn format_result(&self, result: &GrepResult) -> String {
        let relative_path = result.file_path
            .strip_prefix(&self.working_dir)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or_else(|| result.file_path.to_str().unwrap_or("???"));

        format!("{}:{}  {}", relative_path, result.line_number, result.line_content)
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            self.ensure_selected_visible();
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        if self.selected_index < self.filtered_results.len().saturating_sub(1) {
            self.selected_index += 1;
            self.ensure_selected_visible();
        }
    }

    /// Ensure selected item is visible
    fn ensure_selected_visible(&mut self) {
        const APPROX_LINE_HEIGHT: f32 = 20.0;
        const PADDING: f32 = 10.0;

        let content_start_y = PADDING + APPROX_LINE_HEIGHT * 1.5;
        let visible_height = self.bounds.height.0 - content_start_y - PADDING;

        let item_y = self.selected_index as f32 * APPROX_LINE_HEIGHT;
        let item_bottom = item_y + APPROX_LINE_HEIGHT;

        if item_y < self.scroll_position.y.0 {
            self.scroll_position.y.0 = item_y;
        } else if item_bottom > self.scroll_position.y.0 + visible_height {
            self.scroll_position.y.0 = item_bottom - visible_height;
        }
    }

    /// Get the selected result
    pub fn selected_result(&self) -> Option<&GrepResult> {
        self.filtered_results.get(self.selected_index).map(|(r, _)| r)
    }

    /// Calculate bounds based on viewport
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        const MAX_VISIBLE_RESULTS: usize = 15;
        const PADDING: f32 = 20.0;
        const TOP_MARGIN: f32 = 40.0;

        let line_height = viewport.metrics.line_height;

        let visible_count = self.filtered_results.len().min(MAX_VISIBLE_RESULTS);
        let content_height = line_height * (visible_count as f32 + 2.0) + PADDING * 2.0;

        let picker_width = if let Some(cached) = self.cached_width {
            cached
        } else {
            let width = (viewport.logical_size.width.0 * 0.9f32).min(1200.0);
            self.cached_width = Some(width);
            width
        };

        let picker_height = content_height.min(viewport.logical_size.height.0 * 0.8f32);

        let x = (viewport.logical_size.width.0 - picker_width) / 2.0;
        let y = TOP_MARGIN;

        self.bounds = Rect {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(picker_width),
            height: LogicalPixels(picker_height),
        };
    }

    /// Get current bounds
    pub fn get_bounds(&self) -> Rect {
        self.bounds
    }

    /// Collect glyphs for rendering
    pub fn collect_glyphs(&self, collector: &mut crate::render::GlyphCollector) {
        if !self.visible {
            return;
        }

        if self.bounds.width.0 <= 1.0 || self.bounds.height.0 <= 1.0 {
            return;
        }

        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let scale_factor = collector.viewport.scale_factor;
        let font_size = collector.viewport.font_size;
        let line_height = collector.viewport.line_height;

        if line_height <= 0.0 || scale_factor <= 0.0 || font_size <= 0.0 {
            return;
        }

        let overlay_x = self.bounds.x.0;
        let overlay_y = self.bounds.y.0;
        const PADDING: f32 = 10.0;

        let mut glyphs = Vec::new();

        // Render search input on first line
        let status = if self.searching {
            format!("Search: {} (searching...)", self.query)
        } else if self.query.is_empty() {
            "Search: (type to search)".to_string()
        } else {
            format!("Search: {} ({} results)", self.query, self.filtered_results.len())
        };

        let input_pos = LayoutPos::new(PADDING - self.scroll_position.x.0, PADDING);
        let input_glyphs = create_glyph_instances(
            &font_service,
            &status,
            input_pos,
            font_size,
            scale_factor,
            line_height,
            None,
            0,
        );
        glyphs.extend(input_glyphs);

        // Calculate visible range
        let content_start_y = PADDING + line_height * 1.5;
        let visible_height = self.bounds.height.0 - content_start_y - PADDING;

        if visible_height <= 0.0 || line_height <= 0.0 {
            return;
        }

        let first_visible_line = (self.scroll_position.y.0 / line_height).floor() as usize;
        let visible_line_count = ((visible_height / line_height).ceil() as usize + 1).min(20);

        let visible_results = self.filtered_results.iter()
            .skip(first_visible_line)
            .take(visible_line_count);

        const MAX_GLYPHS: usize = 3000;

        for (idx, (result, _score)) in visible_results.enumerate() {
            if glyphs.len() >= MAX_GLYPHS {
                break;
            }

            let actual_idx = first_visible_line + idx;
            let mut display_text = self.format_result(result);

            if display_text.len() > 150 {
                display_text.truncate(147);
                display_text.push_str("...");
            }

            let is_selected = actual_idx == self.selected_index;

            let line_text = if is_selected {
                format!("→ {}", display_text)
            } else {
                format!("  {}", display_text)
            };

            let x_offset = PADDING - self.scroll_position.x.0;
            let y_offset = content_start_y + (actual_idx as f32 * line_height) - self.scroll_position.y.0;
            let result_pos = LayoutPos::new(x_offset, y_offset);

            let result_glyphs = create_glyph_instances(
                &font_service,
                &line_text,
                result_pos,
                font_size,
                scale_factor,
                line_height,
                None,
                if is_selected { 1 } else { 0 },
            );

            glyphs.extend(result_glyphs);
        }

        if glyphs.len() > 5000 {
            glyphs.truncate(5000);
        }

        // Transform to screen coordinates
        for mut g in glyphs {
            let screen_x = g.pos.x.0 + overlay_x;
            let screen_y = g.pos.y.0 + overlay_y;

            if screen_x < self.bounds.x.0 || screen_x > self.bounds.x.0 + self.bounds.width.0
                || screen_y < self.bounds.y.0 || screen_y > self.bounds.y.0 + self.bounds.height.0
            {
                continue;
            }

            g.pos = LayoutPos::new(screen_x * scale_factor, screen_y * scale_factor);
            collector.add_glyphs(vec![g]);
        }
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
        self.scroll_position
    }

    fn set_scroll(&mut self, scroll: Point) {
        self.scroll_position = scroll;
    }

    fn handle_scroll(&mut self, delta: Point, viewport: &Viewport, widget_bounds: Rect) -> bool {
        if !self.visible {
            return false;
        }

        self.scroll_position.y.0 -= delta.y.0;
        self.scroll_position.x.0 -= delta.x.0;

        let content_bounds = self.get_content_bounds(viewport);
        let visible_height = widget_bounds.height.0;
        let max_scroll_y = (content_bounds.height.0 - visible_height).max(0.0);

        self.scroll_position.y.0 = self.scroll_position.y.0.max(0.0).min(max_scroll_y);
        self.scroll_position.x.0 = self.scroll_position.x.0.max(0.0);

        true
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        const PADDING: f32 = 20.0;
        let line_height = viewport.metrics.line_height;

        let total_height = line_height * (self.filtered_results.len() as f32 + 2.0) + PADDING * 2.0;

        Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(800.0),
            height: LogicalPixels(total_height),
        }
    }
}
