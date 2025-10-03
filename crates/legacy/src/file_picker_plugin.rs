//! File picker plugin - shows a searchable list of files

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

/// Simple file picker with regex-based search
pub struct FilePickerPlugin {
    /// Whether the picker is visible
    pub visible: bool,
    /// Current search query
    pub query: String,
    /// All files in the working directory (thread-safe)
    all_files: Arc<RwLock<Vec<PathBuf>>>,
    /// Filtered files based on query
    filtered_files: Vec<PathBuf>,
    /// Selected index in filtered list
    selected_index: usize,
    /// Working directory
    working_dir: PathBuf,
    /// Scroll position for long file lists
    scroll_position: Point,
    /// Bounds for overlay rendering (calculated based on viewport)
    bounds: Rect,
    /// Cached width to keep picker size consistent while filtering
    cached_width: Option<f32>,
}

impl FilePickerPlugin {
    pub fn new() -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let all_files = Arc::new(RwLock::new(Vec::new()));

        // Spawn background thread to scan directory
        let all_files_clone = all_files.clone();
        let working_dir_clone = working_dir.clone();
        std::thread::spawn(move || {
            let scanned = Self::scan_directory(&working_dir_clone);
            *all_files_clone.write() = scanned;
        });

        Self {
            visible: false,
            query: String::new(),
            all_files,
            filtered_files: Vec::new(),
            selected_index: 0,
            working_dir,
            scroll_position: Point::default(),
            bounds: Rect::default(),
            cached_width: None,
        }
    }

    /// Scan directory for files using ignore crate (respects .gitignore)
    fn scan_directory(dir: &Path) -> Vec<PathBuf> {
        use ignore::WalkBuilder;

        let mut files: Vec<PathBuf> = WalkBuilder::new(dir)
            .hidden(true) // Skip hidden files/directories
            .git_ignore(true) // Respect .gitignore
            .git_exclude(true) // Respect .git/info/exclude
            .build()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|ft| ft.is_file())
                    .unwrap_or(false)
            })
            .map(|entry| entry.into_path())
            .collect();

        // Sort files for consistent ordering
        files.sort();
        files
    }

    /// Show the file picker (bounds must be calculated before rendering)
    pub fn show(&mut self) {
        self.visible = true;
        self.query.clear();
        self.scroll_position = Point::default();
        self.bounds = Rect::default();
        self.cached_width = None; // Reset cached width so it recalculates
        self.update_filtered_files();
    }

    /// Hide the file picker
    pub fn hide(&mut self) {
        self.visible = false;
        self.query.clear();
    }

    /// Update search query
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.update_filtered_files();
    }

    /// Add character to query
    pub fn add_char(&mut self, ch: char) {
        self.query.push(ch);
        self.update_filtered_files();
    }

    /// Remove last character from query
    pub fn backspace(&mut self) {
        self.query.pop();
        self.update_filtered_files();
    }

    /// Update filtered files based on current query
    fn update_filtered_files(&mut self) {
        let all_files = self.all_files.read();

        if self.query.is_empty() {
            self.filtered_files = all_files.clone();
        } else {
            // Simple regex-based filtering with .* appended
            let pattern = format!("{}.*", regex::escape(&self.query));
            if let Ok(re) = regex::Regex::new(&pattern) {
                self.filtered_files = all_files
                    .iter()
                    .filter(|path| {
                        path.to_str()
                            .map(|s| re.is_match(s))
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect();
            } else {
                // If regex is invalid, show all files
                self.filtered_files = all_files.clone();
            }
        }

        // Reset selection to first item
        self.selected_index = 0;
    }

    /// Move selection up and ensure visible
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            self.ensure_selected_visible();
        }
    }

    /// Move selection down and ensure visible
    pub fn move_down(&mut self) {
        if self.selected_index < self.filtered_files.len().saturating_sub(1) {
            self.selected_index += 1;
            self.ensure_selected_visible();
        }
    }

    /// Ensure selected item is visible (scroll if needed)
    fn ensure_selected_visible(&mut self) {
        // Approximate line height - will be properly calculated when we have viewport
        const APPROX_LINE_HEIGHT: f32 = 20.0;
        const PADDING: f32 = 10.0;

        let content_start_y = PADDING + APPROX_LINE_HEIGHT * 1.5;
        let visible_height = self.bounds.height.0 - content_start_y - PADDING;

        let item_y = self.selected_index as f32 * APPROX_LINE_HEIGHT;
        let item_bottom = item_y + APPROX_LINE_HEIGHT;

        // Scroll up if item is above visible area
        if item_y < self.scroll_position.y.0 {
            self.scroll_position.y.0 = item_y;
        }
        // Scroll down if item is below visible area
        else if item_bottom > self.scroll_position.y.0 + visible_height {
            self.scroll_position.y.0 = item_bottom - visible_height;
        }
    }

    /// Get the selected file path
    pub fn selected_file(&self) -> Option<&Path> {
        self.filtered_files.get(self.selected_index).map(|p| p.as_path())
    }

    /// Get display name for a path (relative to working dir)
    fn display_name(&self, path: &Path) -> String {
        path.strip_prefix(&self.working_dir)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or_else(|| path.to_str().unwrap_or("???"))
            .to_string()
    }

    /// Calculate bounds based on content and viewport (positioned at top)
    pub fn calculate_bounds(&mut self, viewport: &Viewport) {
        const MAX_VISIBLE_FILES: usize = 12;
        const PADDING: f32 = 20.0;
        const TOP_MARGIN: f32 = 40.0;

        let line_height = viewport.metrics.line_height;

        // Calculate dimensions based on actual content
        let visible_count = self.filtered_files.len().min(MAX_VISIBLE_FILES);
        let content_height = line_height * (visible_count as f32 + 2.0) + PADDING * 2.0; // +2 for search input

        // Calculate or use cached width to keep size consistent while filtering
        let picker_width = if let Some(cached) = self.cached_width {
            cached
        } else {
            // First time - calculate max width from ALL files (not just filtered)
            let all_files = self.all_files.read();
            let mut max_path_width = 300.0f32; // Minimum width

            for path in all_files.iter().take(100) { // Sample first 100 files for performance
                let display_name = self.display_name(path);
                let path_width = (display_name.len() as f32) * viewport.metrics.space_width + PADDING * 2.0;
                max_path_width = max_path_width.max(path_width);
            }

            // Clamp to reasonable size
            let width = max_path_width.min(viewport.logical_size.width.0 * 0.8f32);
            self.cached_width = Some(width);
            width
        };

        let picker_height = content_height.min(viewport.logical_size.height.0 * 0.8f32);

        // Horizontal center, positioned at top
        let x = (viewport.logical_size.width.0 - picker_width) / 2.0;
        let y = TOP_MARGIN;

        self.bounds = Rect {
            x: LogicalPixels(x),
            y: LogicalPixels(y),
            width: LogicalPixels(picker_width),
            height: LogicalPixels(picker_height),
        };
    }

    /// Get current bounds for hit testing
    pub fn get_bounds(&self) -> Rect {
        self.bounds
    }

    /// Collect glyphs for overlay rendering with scroll support
    pub fn collect_glyphs(&self, collector: &mut crate::render::GlyphCollector) {
        if !self.visible {
            return;
        }

        // Check if bounds are valid (not default/uninitialized)
        if self.bounds.width.0 <= 1.0 || self.bounds.height.0 <= 1.0 {
            return; // Bounds not calculated yet
        }

        // Check if we have too many files
        if self.filtered_files.len() > 10000 {
            return;
        }

        // Get font service from service registry
        let font_service = match collector.services().get::<SharedFontSystem>() {
            Some(fs) => fs,
            None => return,
        };

        let scale_factor = collector.viewport.scale_factor;
        let font_size = collector.viewport.font_size;
        let line_height = collector.viewport.line_height;

        // Guard against invalid metrics
        if line_height <= 0.0 || scale_factor <= 0.0 || font_size <= 0.0 {
            return;
        }

        let overlay_x = self.bounds.x.0;
        let overlay_y = self.bounds.y.0;
        const PADDING: f32 = 10.0;

        let mut glyphs = Vec::new();

        // Render search input on first line (with horizontal scroll)
        let input_text = format!("> {}", self.query);
        let input_pos = LayoutPos::new(PADDING - self.scroll_position.x.0, PADDING);
        let input_glyphs = create_glyph_instances(
            &font_service,
            &input_text,
            input_pos,
            font_size,
            scale_factor,
            line_height,
            None,
            0,
        );
        glyphs.extend(input_glyphs);

        // Calculate visible range based on scroll and bounds
        let content_start_y = PADDING + line_height * 1.5; // After input line
        let visible_height = self.bounds.height.0 - content_start_y - PADDING;

        // Guard against invalid bounds
        if visible_height <= 0.0 || line_height <= 0.0 {
            return;
        }

        let first_visible_line = (self.scroll_position.y.0 / line_height).floor() as usize;
        let visible_line_count = ((visible_height / line_height).ceil() as usize + 1).min(20); // Cap at 20 lines max

        // Render only visible filtered files based on scroll
        let visible_files = self.filtered_files.iter()
            .skip(first_visible_line)
            .take(visible_line_count);

        const MAX_GLYPHS: usize = 3000; // Hard cap to prevent buffer overflow

        for (idx, path) in visible_files.enumerate() {
            // Safety check: stop if we've generated too many glyphs
            if glyphs.len() >= MAX_GLYPHS {
                break;
            }

            let actual_idx = first_visible_line + idx;
            let mut display_name = self.display_name(path);

            // Truncate very long paths to prevent buffer overflow
            if display_name.len() > 150 {
                display_name.truncate(147);
                display_name.push_str("...");
            }

            let is_selected = actual_idx == self.selected_index;

            // Add selection indicator
            let line_text = if is_selected {
                format!("→ {}", display_name)
            } else {
                format!("  {}", display_name)
            };

            // Position relative to scroll (both X and Y)
            let x_offset = PADDING - self.scroll_position.x.0;
            let y_offset = content_start_y + (actual_idx as f32 * line_height) - self.scroll_position.y.0;
            let file_pos = LayoutPos::new(x_offset, y_offset);

            let file_glyphs = create_glyph_instances(
                &font_service,
                &line_text,
                file_pos,
                font_size,
                scale_factor,
                line_height,
                None,
                if is_selected { 1 } else { 0 },
            );

            glyphs.extend(file_glyphs);
        }

        // Safety check before sending to GPU
        if glyphs.len() > 5000 {
            glyphs.truncate(5000);
        }

        // Transform to screen coordinates and clip to bounds
        for mut g in glyphs {
            let screen_x = g.pos.x.0 + overlay_x;
            let screen_y = g.pos.y.0 + overlay_y;

            // Clip glyphs outside bounds (simple bounds check)
            if screen_x < self.bounds.x.0 || screen_x > self.bounds.x.0 + self.bounds.width.0
                || screen_y < self.bounds.y.0 || screen_y > self.bounds.y.0 + self.bounds.height.0
            {
                continue; // Skip glyphs outside bounds
            }

            // Convert to physical coordinates
            g.pos = LayoutPos::new(screen_x * scale_factor, screen_y * scale_factor);
            collector.add_glyphs(vec![g]);
        }
    }
}

// === Plugin Trait Implementation ===

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
    fn paint(&self, _ctx: &PaintContext, _pass: &mut wgpu::RenderPass) {
    }

    fn z_index(&self) -> i32 {
        1000 // Render above everything else
    }
}

// === Scrollable Implementation ===

impl Scrollable for FilePickerPlugin {
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

        // Apply scroll delta (inverted for natural scrolling)
        self.scroll_position.y.0 -= delta.y.0;
        self.scroll_position.x.0 -= delta.x.0;

        // Clamp to content bounds using actual widget bounds
        let content_bounds = self.get_content_bounds(viewport);
        let visible_height = widget_bounds.height.0;
        let max_scroll_y = (content_bounds.height.0 - visible_height).max(0.0);

        self.scroll_position.y.0 = self.scroll_position.y.0.max(0.0).min(max_scroll_y);
        self.scroll_position.x.0 = self.scroll_position.x.0.max(0.0);

        true // Handled
    }

    fn get_content_bounds(&self, viewport: &Viewport) -> Rect {
        // Calculate total content size (not just visible)
        const PADDING: f32 = 20.0;
        let line_height = viewport.metrics.line_height;

        // All filtered files (not just visible ones)
        let total_height = line_height * (self.filtered_files.len() as f32 + 2.0) + PADDING * 2.0;

        // Calculate max path width from all files
        let mut max_width = 300.0f32;
        for path in &self.filtered_files {
            let display_name = self.display_name(path);
            let path_width = (display_name.len() as f32) * viewport.metrics.space_width + PADDING * 2.0;
            max_width = max_width.max(path_width);
        }

        Rect {
            x: LogicalPixels(0.0),
            y: LogicalPixels(0.0),
            width: LogicalPixels(max_width),
            height: LogicalPixels(total_height),
        }
    }
}